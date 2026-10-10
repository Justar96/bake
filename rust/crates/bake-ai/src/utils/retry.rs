//! Agent-level retry of transient provider failures.
//!
//! Ported from Pi `packages/ai/src/utils/retry.ts` (v1.1.0): the retryable
//! and non-retryable classifiers, the backoff, and `retryAssistantCall`.
//! Pi's callbacks may be asynchronous; here one synchronous observer receives
//! each [`RetryEvent`].

use std::future::Future;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;

use crate::types::{AssistantMessage, StopReason};
use crate::utils::abort::{AbortSignal, cancelled};

const NON_RETRYABLE_PROVIDER_LIMIT_ERRORS: &[&str] = &[
    "GoUsageLimitError",
    "FreeUsageLimitError",
    "Monthly usage limit reached",
    "available balance",
    "insufficient_quota",
    "out of budget",
    "quota exceeded",
    "billing",
    "subscription_sharing_usage_limit_exceeded",
];

const RETRYABLE_PROVIDER_ERRORS: &[&str] = &[
    "overloaded",
    "server_busy",
    "servers are currently busy",
    "currently experiencing high demand",
    "model is at capacity",
    "rate.?limit",
    "too many requests",
    "429",
    "500",
    "502",
    "503",
    "504",
    "520",
    "524",
    "service.?unavailable",
    "server.?error",
    "internal.?error",
    "provider.?returned.?error",
    "exceeded request buffer limit while retrying upstream",
    "network.?error",
    "connection.?error",
    "connection.?refused",
    "connection.?lost",
    "other side closed",
    "fetch failed",
    "getaddrinfo",
    "ENOTFOUND",
    "EAI_AGAIN",
    "upstream.?connect",
    "reset before headers",
    "socket hang up",
    "socket connection was closed",
    "timed? out",
    "timeout",
    "terminated",
    "websocket.?closed",
    "websocket.?error",
    "ended without",
    "stream ended before message_stop",
    "stream ended before a terminal response event",
    "http2 request did not get a response",
    "pending stream has been canceled",
    "retry delay",
    "you can retry your request",
    "try your request again",
    "please retry your request",
    "ResourceExhausted",
    "subscription_sharing_usage_unavailable",
    "subscription_sharing_user_unavailable",
];

fn build_pattern(patterns: &[&str]) -> Option<Regex> {
    Regex::new(&format!("(?i){}", patterns.join("|"))).ok()
}

static NON_RETRYABLE: LazyLock<Option<Regex>> =
    LazyLock::new(|| build_pattern(NON_RETRYABLE_PROVIDER_LIMIT_ERRORS));
static RETRYABLE: LazyLock<Option<Regex>> =
    LazyLock::new(|| build_pattern(RETRYABLE_PROVIDER_ERRORS));

/// The default cap of one agent-level retry delay.
pub const DEFAULT_MAX_AGENT_RETRY_DELAY_MS: u64 = 60_000;

/// Bounded attempts with exponential backoff (`base_delay_ms * 2^(attempt-1)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Whether to retry at all.
    pub enabled: bool,
    /// Retries after the first call.
    pub max_retries: u32,
    /// Base delay.
    pub base_delay_ms: u64,
    /// Cap of each delay; 60 seconds when `None`.
    pub max_agent_delay_ms: Option<u64>,
}

/// The delay before retry `attempt` (1-based).
pub fn retry_delay_ms(base_delay_ms: u64, max_agent_delay_ms: Option<u64>, attempt: u32) -> u64 {
    // JavaScript doubles exactly up to 2^53; beyond, Pi uses MAX_SAFE_INTEGER.
    const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
    let exponent = attempt.saturating_sub(1);
    let delay = 2u64
        .checked_pow(exponent)
        .and_then(|factor| base_delay_ms.checked_mul(factor))
        .filter(|delay| *delay <= MAX_SAFE_INTEGER)
        .unwrap_or(MAX_SAFE_INTEGER);
    delay.min(max_agent_delay_ms.unwrap_or(DEFAULT_MAX_AGENT_RETRY_DELAY_MS))
}

/// What [`retry_assistant_call`] reports to its observer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryEvent {
    /// Before the backoff sleep of retry `attempt` (1-based).
    Scheduled {
        /// The retry number.
        attempt: u32,
        /// The retry budget.
        max_attempts: u32,
        /// The backoff.
        delay_ms: u64,
        /// The error being retried.
        error_message: String,
    },
    /// After the backoff, right before the retried call.
    AttemptStart,
    /// Once, when a loop that scheduled a retry ends.
    Finished {
        /// Whether a later call completed normally.
        success: bool,
        /// The last retry number.
        attempt: u32,
        /// The final error, when the loop gave up on one.
        final_error: Option<String>,
    },
}

/// Whether `message` failed with a transient provider or transport error.
/// Context overflow should be handled first, separately.
pub fn is_retryable_assistant_error(message: &AssistantMessage) -> bool {
    if message.stop_reason != StopReason::Error {
        return false;
    }
    let Some(error) = message
        .error_message
        .as_deref()
        .filter(|error| !error.is_empty())
    else {
        return false;
    };
    if NON_RETRYABLE
        .as_ref()
        .is_some_and(|pattern| pattern.is_match(error))
    {
        return false;
    }
    RETRYABLE
        .as_ref()
        .is_some_and(|pattern| pattern.is_match(error))
}

/// Runs `produce` and retries transient errors under `policy`. Success and
/// non-retryable errors return at once; aborts are never retried; an abort
/// during the backoff returns the last error as an `aborted` message without
/// its error text.
pub async fn retry_assistant_call<F, Fut>(
    mut produce: F,
    policy: Option<RetryPolicy>,
    signal: Option<&AbortSignal>,
    mut on_event: impl FnMut(RetryEvent),
) -> AssistantMessage
where
    F: FnMut() -> Fut,
    Fut: Future<Output = AssistantMessage>,
{
    let max_attempts = policy
        .filter(|policy| policy.enabled)
        .map_or(0, |policy| policy.max_retries);
    let mut attempt = 0u32;
    let mut last_retry: Option<(u32, String)> = None;
    loop {
        let response = produce().await;
        if response.stop_reason == StopReason::Aborted {
            if let Some((attempt, _)) = &last_retry {
                on_event(RetryEvent::Finished {
                    success: false,
                    attempt: *attempt,
                    final_error: None,
                });
            }
            return response;
        }
        if response.stop_reason != StopReason::Error {
            if let Some((attempt, _)) = &last_retry {
                on_event(RetryEvent::Finished {
                    success: true,
                    attempt: *attempt,
                    final_error: None,
                });
            }
            return response;
        }
        let Some(policy) =
            policy.filter(|_| attempt < max_attempts && is_retryable_assistant_error(&response))
        else {
            if let Some((attempt, _)) = &last_retry {
                on_event(RetryEvent::Finished {
                    success: false,
                    attempt: *attempt,
                    final_error: response.error_message.clone(),
                });
            }
            return response;
        };
        attempt += 1;
        let error_message = response
            .error_message
            .clone()
            .filter(|error| !error.is_empty())
            .unwrap_or_else(|| "Unknown error".to_owned());
        last_retry = Some((attempt, error_message.clone()));
        let delay_ms = retry_delay_ms(policy.base_delay_ms, policy.max_agent_delay_ms, attempt);
        on_event(RetryEvent::Scheduled {
            attempt,
            max_attempts,
            delay_ms,
            error_message: error_message.clone(),
        });
        let aborted = signal.is_some_and(AbortSignal::aborted)
            || tokio::select! {
                () = tokio::time::sleep(Duration::from_millis(delay_ms)) => false,
                () = cancelled(signal) => true,
            };
        if aborted {
            on_event(RetryEvent::Finished {
                success: false,
                attempt,
                final_error: Some(error_message),
            });
            let mut aborted = response;
            aborted.error_message = None;
            aborted.stop_reason = StopReason::Aborted;
            return aborted;
        }
        on_event(RetryEvent::AttemptStart);
    }
}

#[cfg(test)]
mod tests {
    //! Ports of Pi `packages/ai/test/retry.test.ts`.

    use super::*;
    use crate::providers::faux::faux_assistant_message;
    use crate::utils::abort::AbortController;
    use std::cell::Cell;

    fn error(text: &str) -> AssistantMessage {
        let mut message = faux_assistant_message("");
        message.stop_reason = StopReason::Error;
        message.error_message = Some(text.to_owned());
        message
    }

    #[test]
    fn every_pattern_compiles() {
        assert!(NON_RETRYABLE.is_some());
        assert!(RETRYABLE.is_some());
    }

    #[test]
    fn classifies_retryable_wording() {
        for text in [
            "An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID req_******** in your message.",
            "{\"message\":\"The system encountered an unexpected error during processing. Try your request again.\"}",
            "ResourceExhausted: Worker local total request limit reached (288/48)",
            "The socket connection was closed unexpectedly. For more information, pass `verbose: true` in the second argument to fetch()",
            "Error: exceeded request buffer limit while retrying upstream",
            "The pending stream has been canceled (caused by: getaddrinfo ENOTFOUND bedrock-runtime.us-east-1.amazonaws.com)",
            "connect ENOTFOUND api.example.com",
            "EAI_AGAIN api.example.com",
            "getaddrinfo failed for api.example.com",
            "The pending stream has been canceled",
            "The pending stream has been canceled (caused by: socket closed)",
            "OpenAI Responses stream ended before a terminal response event",
            "The system is currently experiencing high demand and cannot process your request. Your request exceeds the maximum usage size allowed during peak load. For improved capacity reliability, consider switching to Provisioned Throughput.",
            "subscription_sharing_usage_unavailable: Usage cannot be checked.",
            "subscription_sharing_user_unavailable: User cannot be loaded.",
            "overloaded_error",
            "520 status code (no body)",
            "524 status code (no body)",
        ] {
            assert!(is_retryable_assistant_error(&error(text)), "{text}");
        }
        for text in [
            "429 quota exceeded",
            "OpenAI API error (429): {\"code\":\"subscription_sharing_usage_limit_exceeded\",\"message\":\"Usage limit reached.\"}",
        ] {
            assert!(!is_retryable_assistant_error(&error(text)), "{text}");
        }
        assert!(!is_retryable_assistant_error(&faux_assistant_message(
            "not an error"
        )));
    }

    // "caps agent retry delay"
    #[test]
    fn caps_agent_retry_delay() {
        assert_eq!(retry_delay_ms(2000, None, 6), 60000);
        assert_eq!(retry_delay_ms(2000, Some(5000), 5), 5000);
        assert_eq!(retry_delay_ms(2000, Some(0), 5), 0);
        assert_eq!(retry_delay_ms(u64::MAX, Some(u64::MAX), 200), (1 << 53) - 1);
    }

    const ENABLED: RetryPolicy = RetryPolicy {
        enabled: true,
        max_retries: 3,
        base_delay_ms: 0,
        max_agent_delay_ms: None,
    };

    fn text(message: &AssistantMessage) -> String {
        crate::utils::text::assistant_text(message)
    }

    #[tokio::test]
    async fn returns_success_immediately() {
        let calls = Cell::new(0);
        let result = retry_assistant_call(
            || {
                calls.set(calls.get() + 1);
                async { faux_assistant_message("ok") }
            },
            Some(ENABLED),
            None,
            |_| {},
        )
        .await;
        assert_eq!(text(&result), "ok");
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn does_not_retry_aborts_or_non_retryable_errors() {
        let calls = Cell::new(0);
        let mut events = Vec::new();
        let result = retry_assistant_call(
            || {
                calls.set(calls.get() + 1);
                async {
                    let mut message = faux_assistant_message("");
                    message.stop_reason = StopReason::Aborted;
                    message
                }
            },
            Some(ENABLED),
            None,
            |event| events.push(event),
        )
        .await;
        assert_eq!(result.stop_reason, StopReason::Aborted);
        assert_eq!(calls.get(), 1);
        assert!(events.is_empty());

        let result = retry_assistant_call(
            || async { error("insufficient_quota") },
            Some(ENABLED),
            None,
            |event| events.push(event),
        )
        .await;
        assert_eq!(result.stop_reason, StopReason::Error);
        assert!(events.is_empty());
    }

    // "retries a transient error up to maxRetries then returns the final error"
    #[tokio::test]
    async fn retries_up_to_the_budget() {
        let calls = Cell::new(0);
        let mut events = Vec::new();
        let result = retry_assistant_call(
            || {
                calls.set(calls.get() + 1);
                async { error("terminated") }
            },
            Some(ENABLED),
            None,
            |event| events.push(event),
        )
        .await;
        assert_eq!(result.stop_reason, StopReason::Error);
        assert_eq!(calls.get(), 4);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, RetryEvent::Scheduled { .. }))
                .count(),
            3
        );
        assert_eq!(
            events.last(),
            Some(&RetryEvent::Finished {
                success: false,
                attempt: 3,
                final_error: Some("terminated".into())
            })
        );
    }

    // "reports capped retry delays"
    #[tokio::test]
    async fn reports_capped_delays() {
        let calls = Cell::new(0);
        let mut delays = Vec::new();
        let policy = RetryPolicy {
            enabled: true,
            max_retries: 4,
            base_delay_ms: 10,
            max_agent_delay_ms: Some(15),
        };
        retry_assistant_call(
            || {
                calls.set(calls.get() + 1);
                let n = calls.get();
                async move {
                    if n < 5 {
                        error("terminated")
                    } else {
                        faux_assistant_message("recovered")
                    }
                }
            },
            Some(policy),
            None,
            |event| {
                if let RetryEvent::Scheduled { delay_ms, .. } = event {
                    delays.push(delay_ms);
                }
            },
        )
        .await;
        assert_eq!(delays, vec![10, 15, 15, 15]);
    }

    // "stops retrying once a call succeeds" and "emits onRetryAttemptStart after backoff before each retried call"
    #[tokio::test]
    async fn stops_after_success_and_orders_events() {
        let calls = Cell::new(0);
        let log = std::cell::RefCell::new(Vec::new());
        let result = retry_assistant_call(
            || {
                log.borrow_mut().push(format!("produce:{}", calls.get()));
                calls.set(calls.get() + 1);
                let n = calls.get();
                async move {
                    if n < 3 {
                        error("terminated")
                    } else {
                        faux_assistant_message("recovered")
                    }
                }
            },
            Some(ENABLED),
            None,
            |event| match event {
                RetryEvent::Scheduled { attempt, .. } => {
                    log.borrow_mut().push(format!("retry:{attempt}"))
                }
                RetryEvent::AttemptStart => log.borrow_mut().push("attempt-start".into()),
                RetryEvent::Finished {
                    success, attempt, ..
                } => log
                    .borrow_mut()
                    .push(format!("finished:{success}:{attempt}")),
            },
        )
        .await;
        assert_eq!(text(&result), "recovered");
        assert_eq!(
            log.into_inner(),
            vec![
                "produce:0",
                "retry:1",
                "attempt-start",
                "produce:1",
                "retry:2",
                "attempt-start",
                "produce:2",
                "finished:true:2",
            ]
        );
    }

    // "reports an aborted retried call as unsuccessful"
    #[tokio::test]
    async fn reports_an_aborted_retry_as_unsuccessful() {
        let calls = Cell::new(0);
        let mut events = Vec::new();
        let result = retry_assistant_call(
            || {
                calls.set(calls.get() + 1);
                let n = calls.get();
                async move {
                    if n == 1 {
                        error("terminated")
                    } else {
                        let mut message = faux_assistant_message("");
                        message.stop_reason = StopReason::Aborted;
                        message
                    }
                }
            },
            Some(ENABLED),
            None,
            |event| events.push(event),
        )
        .await;
        assert_eq!(result.stop_reason, StopReason::Aborted);
        assert_eq!(calls.get(), 2);
        assert_eq!(
            events.last(),
            Some(&RetryEvent::Finished {
                success: false,
                attempt: 1,
                final_error: None
            })
        );
    }

    // "does not retry when policy is disabled"
    #[tokio::test]
    async fn disabled_policy_returns_the_first_response() {
        let calls = Cell::new(0);
        let mut events = Vec::new();
        let disabled = RetryPolicy {
            enabled: false,
            ..ENABLED
        };
        let result = retry_assistant_call(
            || {
                calls.set(calls.get() + 1);
                async { error("terminated") }
            },
            Some(disabled),
            None,
            |event| events.push(event),
        )
        .await;
        assert_eq!(result.stop_reason, StopReason::Error);
        assert_eq!(calls.get(), 1);
        assert!(events.is_empty());
    }

    // "aborts backoff sleep via signal, returns an aborted message, and emits onRetryFinished(false)"
    #[tokio::test]
    async fn abort_during_backoff_returns_aborted() {
        let controller = AbortController::new();
        let signal = controller.signal();
        let calls = Cell::new(0);
        let mut events = Vec::new();
        let policy = RetryPolicy {
            enabled: true,
            max_retries: 5,
            base_delay_ms: 10_000,
            max_agent_delay_ms: None,
        };
        let result = retry_assistant_call(
            || {
                calls.set(calls.get() + 1);
                async { error("terminated") }
            },
            Some(policy),
            Some(&signal),
            |event| {
                if matches!(event, RetryEvent::Scheduled { .. }) {
                    controller.abort();
                }
                events.push(event);
            },
        )
        .await;
        assert_eq!(result.stop_reason, StopReason::Aborted);
        assert_eq!(result.error_message, None);
        assert_eq!(calls.get(), 1);
        assert_eq!(
            events.last(),
            Some(&RetryEvent::Finished {
                success: false,
                attempt: 1,
                final_error: Some("terminated".into())
            })
        );
    }
}
