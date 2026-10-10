//! Request-level retry with an interruptible backoff.
//!
//! Ported from Pi `packages/ai/src/utils/provider-retry.ts` (v1.1.0), which
//! mirrors the OpenAI and Anthropic SDK retry policy: `x-should-retry`, then
//! connection failures, 408, 409, 429, and 5xx; `retry-after-ms`, then
//! `retry-after` (seconds or an HTTP date), then jittered exponential backoff.

use std::future::Future;
use std::time::Duration;

use crate::utils::abort::{AbortSignal, cancelled};
use crate::utils::error_body::{ProviderError, ProviderErrorKind};

const DEFAULT_MAX_RETRY_DELAY_MS: u64 = 60_000;

/// Options of [`retry_provider_request`].
#[derive(Debug, Clone, Default)]
pub struct ProviderRetryOptions {
    /// Retries after the first request; 0 when `None`.
    pub max_retries: Option<u32>,
    /// Cap of a server-requested delay; 60 seconds when `None`, none when 0.
    pub max_retry_delay_ms: Option<u64>,
    /// Aborts the backoff.
    pub signal: Option<AbortSignal>,
    /// Statuses that fail at once although the policy would retry them.
    pub no_retry_statuses: Vec<u16>,
}

fn is_provider_error(error: &ProviderError) -> bool {
    matches!(
        error.kind,
        ProviderErrorKind::Http | ProviderErrorKind::Connection | ProviderErrorKind::Timeout
    )
}

fn is_retryable_provider_error(error: &ProviderError) -> bool {
    match error.headers.get("x-should-retry").map(String::as_str) {
        Some("true") => return true,
        Some("false") => return false,
        _ => {}
    }
    match error.status {
        None => true,
        Some(status) => status == 408 || status == 409 || status == 429 || status >= 500,
    }
}

fn validate_server_retry_delay(
    delay_ms: f64,
    max_retry_delay_ms: Option<u64>,
    message: &str,
) -> Result<f64, ProviderError> {
    let max = max_retry_delay_ms.unwrap_or(DEFAULT_MAX_RETRY_DELAY_MS);
    if max > 0 && delay_ms > max as f64 {
        return Err(ProviderError::other(format!(
            "Server requested {}s retry delay (max: {}s). {message}",
            (delay_ms / 1000.0).ceil(),
            (max as f64 / 1000.0).ceil()
        )));
    }
    Ok(delay_ms)
}

/// JavaScript `Number.parseFloat`: the longest numeric prefix after
/// whitespace.
fn parse_float_prefix(text: &str) -> Option<f64> {
    let text = text.trim_start();
    let mut end = 0;
    let bytes = text.as_bytes();
    let mut seen_digit = false;
    let mut seen_dot = false;
    if matches!(bytes.first(), Some(b'+' | b'-')) {
        end = 1;
    }
    while let Some(&byte) = bytes.get(end) {
        match byte {
            b'0'..=b'9' => seen_digit = true,
            b'.' if !seen_dot => seen_dot = true,
            _ => break,
        }
        end += 1;
    }
    if !seen_digit {
        return None;
    }
    let mut number_end = end;
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let mut exp_end = end + 1;
        if matches!(bytes.get(exp_end), Some(b'+' | b'-')) {
            exp_end += 1;
        }
        let digits_start = exp_end;
        while matches!(bytes.get(exp_end), Some(b'0'..=b'9')) {
            exp_end += 1;
        }
        if exp_end > digits_start {
            number_end = exp_end;
        }
    }
    text.get(..number_end)?.parse().ok()
}

/// Parses an IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`) to Unix ms.
fn parse_http_date(text: &str) -> Option<f64> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    let [_, day, month, year, time, zone] = parts.as_slice() else {
        return None;
    };
    if *zone != "GMT" && *zone != "UTC" {
        return None;
    }
    let day: i64 = day.parse().ok()?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|name| name == month)? as i64
        + 1;
    let year: i64 = year.parse().ok()?;
    let hms: Vec<i64> = time
        .split(':')
        .map(|part| part.parse().ok())
        .collect::<Option<_>>()?;
    let [hour, minute, second] = hms.as_slice() else {
        return None;
    };
    // Days from the civil date (Howard Hinnant's algorithm). Every field
    // comes from the header, so arithmetic that overflows means "no date".
    let y = if month <= 2 {
        year.checked_sub(1)?
    } else {
        year
    };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = ((153 * mp + 2) / 5).checked_add(day)?.checked_sub(1)?;
    let doe = (yoe * 365 + yoe / 4 - yoe / 100).checked_add(doy)?;
    let days = era
        .checked_mul(146_097)?
        .checked_add(doe)?
        .checked_sub(719_468)?;
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(hour.checked_mul(3600)?)?
        .checked_add(minute.checked_mul(60)?)?
        .checked_add(*second)?;
    Some(seconds.checked_mul(1000)? as f64)
}

fn retry_delay_ms(
    error: &ProviderError,
    retry_index: u32,
    max_retry_delay_ms: Option<u64>,
) -> Result<f64, ProviderError> {
    if let Some(value) = error
        .headers
        .get("retry-after-ms")
        .filter(|value| !value.is_empty())
        && let Some(value) = parse_float_prefix(value).filter(|value| value.is_finite())
    {
        return validate_server_retry_delay(value, max_retry_delay_ms, &error.message);
    }
    if let Some(value) = error
        .headers
        .get("retry-after")
        .filter(|value| !value.is_empty())
    {
        let delay = match parse_float_prefix(value) {
            Some(seconds) => Some(seconds * 1000.0),
            None => parse_http_date(value).map(|at| at - crate::now_ms() as f64),
        };
        if let Some(delay) = delay.filter(|delay| delay.is_finite()) {
            return validate_server_retry_delay(delay, max_retry_delay_ms, &error.message);
        }
    }
    let exponential = (0.5 * 2f64.powi(retry_index.min(64) as i32)).min(8.0) * 1000.0;
    Ok(exponential * (1.0 - crate::random_f64() * 0.25))
}

/// Runs `request`, retrying retryable provider failures up to
/// `options.max_retries` times. A server-requested delay above the cap fails
/// at once; an abort during the backoff fails with the abort error.
pub async fn retry_provider_request<T, F, Fut>(
    mut request: F,
    options: &ProviderRetryOptions,
) -> Result<T, ProviderError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ProviderError>>,
{
    let max_retries = options.max_retries.unwrap_or(0);
    let mut remaining = max_retries;
    loop {
        let error = match request().await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
            return Err(ProviderError::aborted());
        }
        if remaining == 0 || !is_provider_error(&error) || !is_retryable_provider_error(&error) {
            return Err(error);
        }
        if error
            .status
            .is_some_and(|status| options.no_retry_statuses.contains(&status))
        {
            return Err(error);
        }
        let retry_index = max_retries - remaining;
        remaining -= 1;
        let delay = retry_delay_ms(&error, retry_index, options.max_retry_delay_ms)?;
        let delay = Duration::from_millis(delay.max(0.0).min(u64::MAX as f64) as u64);
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            () = cancelled(options.signal.as_ref()) => return Err(ProviderError::aborted()),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Ports of Pi `packages/ai/test/provider-retry.test.ts`.

    use super::*;
    use crate::utils::abort::AbortController;
    use std::cell::Cell;
    use std::collections::BTreeMap;

    fn provider_error(status: u16, headers: &[(&str, &str)]) -> ProviderError {
        let headers: BTreeMap<String, String> = headers
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let mut error = ProviderError::http(status, headers, "");
        error.message = format!("Provider error: {status}");
        error
    }

    // "retries retryable provider errors"
    #[tokio::test(start_paused = true)]
    async fn retries_retryable_errors() {
        let calls = Cell::new(0);
        let started = tokio::time::Instant::now();
        let result = retry_provider_request(
            || {
                calls.set(calls.get() + 1);
                let n = calls.get();
                async move {
                    if n == 1 {
                        Err(provider_error(429, &[("retry-after-ms", "1000")]))
                    } else {
                        Ok("ok")
                    }
                }
            },
            &ProviderRetryOptions {
                max_retries: Some(1),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(result, Ok("ok"));
        assert_eq!(calls.get(), 2);
        assert_eq!(started.elapsed(), Duration::from_millis(1000));
    }

    // "does not retry errors the provider marks as non-retryable" and
    // "does not retry statuses listed in noRetryStatuses"
    #[tokio::test]
    async fn honours_should_retry_and_no_retry_statuses() {
        let calls = Cell::new(0);
        let result: Result<(), _> = retry_provider_request(
            || {
                calls.set(calls.get() + 1);
                async { Err(provider_error(429, &[("x-should-retry", "false")])) }
            },
            &ProviderRetryOptions {
                max_retries: Some(2),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(result.unwrap_err().status, Some(429));
        assert_eq!(calls.get(), 1);

        calls.set(0);
        let result: Result<(), _> = retry_provider_request(
            || {
                calls.set(calls.get() + 1);
                async { Err(provider_error(504, &[("retry-after-ms", "0")])) }
            },
            &ProviderRetryOptions {
                max_retries: Some(2),
                no_retry_statuses: vec![504],
                ..Default::default()
            },
        )
        .await;
        assert_eq!(result.unwrap_err().status, Some(504));
        assert_eq!(calls.get(), 1);
    }

    // "rejects a provider-requested retry delay above the limit"
    #[tokio::test]
    async fn rejects_delays_above_the_limit() {
        let calls = Cell::new(0);
        let result: Result<(), _> = retry_provider_request(
            || {
                calls.set(calls.get() + 1);
                async { Err(provider_error(429, &[("retry-after", "277403")])) }
            },
            &ProviderRetryOptions {
                max_retries: Some(1),
                max_retry_delay_ms: Some(1000),
                ..Default::default()
            },
        )
        .await;
        assert!(
            result
                .unwrap_err()
                .message
                .starts_with("Server requested 277403s retry delay (max: 1s)")
        );
        assert_eq!(calls.get(), 1);
    }

    // "allows disabling the provider-requested retry delay cap"
    #[tokio::test(start_paused = true)]
    async fn zero_disables_the_cap() {
        let calls = Cell::new(0);
        let started = tokio::time::Instant::now();
        let result = retry_provider_request(
            || {
                calls.set(calls.get() + 1);
                let n = calls.get();
                async move {
                    if n == 1 {
                        Err(provider_error(429, &[("retry-after", "2")]))
                    } else {
                        Ok("ok")
                    }
                }
            },
            &ProviderRetryOptions {
                max_retries: Some(1),
                max_retry_delay_ms: Some(0),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(result, Ok("ok"));
        assert_eq!(started.elapsed(), Duration::from_millis(2000));
    }

    // "aborts a provider-requested retry delay"
    #[tokio::test]
    async fn aborts_the_backoff() {
        let controller = AbortController::new();
        let calls = Cell::new(0);
        let options = ProviderRetryOptions {
            max_retries: Some(2),
            max_retry_delay_ms: Some(0),
            signal: Some(controller.signal()),
            ..Default::default()
        };
        let request = retry_provider_request(
            || {
                calls.set(calls.get() + 1);
                async { Err::<(), _>(provider_error(429, &[("retry-after", "277403")])) }
            },
            &options,
        );
        let abort = async {
            tokio::task::yield_now().await;
            controller.abort();
        };
        let (result, ()) = tokio::join!(request, abort);
        assert_eq!(result.unwrap_err().kind, ProviderErrorKind::Aborted);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn parses_retry_after_values() {
        assert_eq!(parse_float_prefix("1.5s"), Some(1.5));
        assert_eq!(parse_float_prefix(" 2e3"), Some(2000.0));
        assert_eq!(parse_float_prefix("abc"), None);
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784_111_777_000.0)
        );
        assert_eq!(parse_http_date("not a date"), None);
        // Header fields that overflow the date arithmetic are no date, not a
        // panic.
        for text in [
            "Sun, 06 Nov 99999999999999999 08:49:37 GMT",
            "Sun, 06 Nov -9223372036854775808 08:49:37 GMT",
            "Sun, 9223372036854775807 Nov 1994 08:49:37 GMT",
            "Sun, 06 Nov 1994 9223372036854775807:00:00 GMT",
            "Sun, 06 Nov 1994 00:00:9223372036854775807 GMT",
            "Sun, 06 Nov 9223372036854775807 00:00:00 GMT",
        ] {
            assert_eq!(parse_http_date(text), None, "{text}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_overflowing_retry_after_date_falls_back_to_backoff() {
        let calls = Cell::new(0);
        let result: Result<(), ProviderError> = retry_provider_request(
            || {
                calls.set(calls.get() + 1);
                let attempt = calls.get();
                async move {
                    if attempt == 1 {
                        Err(provider_error(
                            503,
                            &[("retry-after", "Sun, 06 Nov 99999999999999999 08:49:37 GMT")],
                        ))
                    } else {
                        Ok(())
                    }
                }
            },
            &ProviderRetryOptions {
                max_retries: Some(1),
                ..Default::default()
            },
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn connection_failures_are_retryable() {
        assert!(is_retryable_provider_error(&ProviderError::connection("")));
        assert!(!is_provider_error(&ProviderError::other("x")));
        assert!(!is_retryable_provider_error(&provider_error(400, &[])));
        assert!(is_retryable_provider_error(&provider_error(
            400,
            &[("x-should-retry", "true")]
        )));
    }
}
