//! Context-overflow detection.
//!
//! Ported from Pi `packages/ai/src/utils/overflow.ts` (v1.1.0), patterns
//! included. See that file for the provider each pattern comes from.

use std::sync::LazyLock;

use regex::Regex;

use crate::types::{AssistantMessage, StopReason};

const OVERFLOW_PATTERN_SOURCES: &[&str] = &[
    r"prompt (?:is )?too long",
    r"prompt exceeds max length",
    r"request_too_large",
    r"input is too long for requested model",
    r"exceeds the context window",
    r"exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))",
    r"input token count.*exceeds the maximum",
    r"maximum prompt length is \d+",
    r"reduce the length of the messages",
    r"maximum context length is \d+ tokens",
    r"exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?",
    r"input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)",
    r"exceeds the limit of \d+",
    r"exceeds the available context size",
    r"greater than the context length",
    r"context window exceeds limit",
    r"exceeded model token limit",
    r"too large for model with \d+ maximum context length",
    r"prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?",
    r"model_context_window_exceeded",
    r"prompt too long; exceeded (?:max )?context length",
    r"range of input length should be",
    r"context[_ ]length[_ ]exceeded",
    r"too many tokens",
    r"token limit exceeded",
];

const NON_OVERFLOW_PATTERN_SOURCES: &[&str] = &[
    r"^(Throttling error|Service unavailable):",
    r"rate limit",
    r"too many requests",
];

const CEREBRAS_BODYLESS_OVERFLOW_SOURCE: &str = r"^4(?:00|13)\s*(?:status code)?\s*\(no body\)";

fn compile(sources: &[&str]) -> Vec<Regex> {
    sources
        .iter()
        .filter_map(|source| Regex::new(&format!("(?i){source}")).ok())
        .collect()
}

static OVERFLOW_PATTERNS: LazyLock<Vec<Regex>> =
    LazyLock::new(|| compile(OVERFLOW_PATTERN_SOURCES));
static NON_OVERFLOW_PATTERNS: LazyLock<Vec<Regex>> =
    LazyLock::new(|| compile(NON_OVERFLOW_PATTERN_SOURCES));
static CEREBRAS_BODYLESS_OVERFLOW: LazyLock<Vec<Regex>> =
    LazyLock::new(|| compile(&[CEREBRAS_BODYLESS_OVERFLOW_SOURCE]));

/// Whether `message` reports a context overflow: an error matching a known
/// overflow message, a successful response whose input exceeds
/// `context_window`, or a zero-output length stop that filled it.
pub fn is_context_overflow(message: &AssistantMessage, context_window: Option<u64>) -> bool {
    if message.stop_reason == StopReason::Error
        && let Some(error) = message
            .error_message
            .as_deref()
            .filter(|error| !error.is_empty())
        && !NON_OVERFLOW_PATTERNS
            .iter()
            .any(|pattern| pattern.is_match(error))
    {
        if OVERFLOW_PATTERNS
            .iter()
            .any(|pattern| pattern.is_match(error))
        {
            return true;
        }
        if message.provider == "cerebras"
            && CEREBRAS_BODYLESS_OVERFLOW
                .iter()
                .any(|pattern| pattern.is_match(error))
        {
            return true;
        }
    }
    let Some(window) = context_window.filter(|window| *window > 0) else {
        return false;
    };
    let input = message.usage.input.saturating_add(message.usage.cache_read);
    if message.stop_reason == StopReason::Stop && input > window {
        return true;
    }
    message.stop_reason == StopReason::Length
        && message.usage.output == 0
        && (input as f64) >= (window as f64) * 0.99
}

/// Whether a length stop ended below `desired_max_output`, the limit before
/// any context clamping, so one compact-and-retry may help.
pub fn is_recoverable_length(message: &AssistantMessage, desired_max_output: u64) -> bool {
    message.stop_reason == StopReason::Length
        && desired_max_output > 0
        && message.usage.output < desired_max_output
}

/// The overflow patterns, for tests.
pub fn overflow_patterns() -> Vec<Regex> {
    OVERFLOW_PATTERNS.clone()
}

#[cfg(test)]
mod tests {
    //! Ports of Pi `packages/ai/test/overflow.test.ts`.

    use super::*;
    use crate::types::Usage;

    #[test]
    fn every_pattern_compiles() {
        assert_eq!(OVERFLOW_PATTERNS.len(), OVERFLOW_PATTERN_SOURCES.len());
        assert_eq!(
            NON_OVERFLOW_PATTERNS.len(),
            NON_OVERFLOW_PATTERN_SOURCES.len()
        );
        assert_eq!(CEREBRAS_BODYLESS_OVERFLOW.len(), 1);
    }

    fn error_message(text: &str, provider: &str) -> AssistantMessage {
        let mut message = crate::providers::faux::faux_assistant_message("");
        message.content.clear();
        message.api = "openai-completions".into();
        message.provider = provider.into();
        message.stop_reason = StopReason::Error;
        message.error_message = Some(text.into());
        message
    }

    #[test]
    fn detects_provider_overflow_errors() {
        let cases: &[(&str, &str, u64)] = &[
            (
                "400 `prompt too long; exceeded max context length by 100918 tokens`",
                "ollama",
                32768,
            ),
            (
                "400 {\"code\":\"1261\",\"message\":\"Prompt too long\"}",
                "zai",
                1048576,
            ),
            (
                "400 {\"code\":\"1261\",\"message\":\"Prompt exceeds max length\"}",
                "zai",
                1048576,
            ),
            (
                "400 The input (516368 tokens) is longer than the model's context length (262144 tokens).",
                "ollama",
                262144,
            ),
            (
                "Error: 503 litellm.ServiceUnavailableError: litellm.MidStreamFallbackError: litellm.APIConnectionError: APIConnectionError: OpenAIException - Requested token count exceeds the model's maximum context length of 131072 tokens.",
                "ollama",
                131072,
            ),
            (
                "Error: 400 Input length (265330) exceeds model's maximum context length (262144).",
                "ollama",
                262144,
            ),
            (
                "Provider returned error: Input length 131393 exceeds the maximum allowed input length of 131040 tokens.",
                "ollama",
                131072,
            ),
            (
                "400 Prompt has 256468 tokens, but the configured context size is 256000 tokens",
                "ollama",
                256000,
            ),
            (
                "Prompt has 5,958,968 tokens, but the configured context size is 256,000 tokens",
                "ollama",
                256000,
            ),
        ];
        for (text, provider, window) in cases {
            assert!(
                is_context_overflow(&error_message(text, provider), Some(*window)),
                "{text}"
            );
        }
    }

    #[test]
    fn rejects_non_overflow_errors() {
        for text in [
            "500 `model runner crashed unexpectedly`",
            "Throttling error: Too many tokens, please wait before trying again.",
            "Service unavailable: The service is temporarily unavailable.",
            "Rate limit exceeded, please retry after 30 seconds.",
            "Too many requests. Please slow down.",
        ] {
            assert!(
                !is_context_overflow(&error_message(text, "ollama"), Some(200000)),
                "{text}"
            );
        }
    }

    // "only treats bodyless 400 and 413 errors as overflow for Cerebras"
    #[test]
    fn cerebras_bodyless_errors() {
        for text in ["400 status code (no body)", "413 status code (no body)"] {
            assert!(is_context_overflow(
                &error_message(text, "cerebras"),
                Some(131072)
            ));
            assert!(!is_context_overflow(
                &error_message(text, "opencode-go"),
                Some(1000000)
            ));
        }
    }

    fn length_stop(input: u64, cache_read: u64, output: u64, cache_write: u64) -> AssistantMessage {
        let mut message = error_message("", "test-provider");
        message.error_message = None;
        message.stop_reason = StopReason::Length;
        message.usage = Usage {
            input,
            output,
            cache_read,
            cache_write,
            total_tokens: input + cache_read + cache_write + output,
            ..Usage::default()
        };
        message
    }

    #[test]
    fn length_stops() {
        assert!(is_context_overflow(
            &length_stop(58, 1048512, 0, 0),
            Some(1048576)
        ));
        assert!(is_recoverable_length(
            &length_stop(3, 253584, 16, 25554),
            128000
        ));
        assert!(!is_recoverable_length(&length_stop(4062, 0, 1024, 0), 1024));
        assert!(is_recoverable_length(&length_stop(100, 0, 0, 0), 128000));
        assert!(!is_context_overflow(
            &length_stop(1000, 0, 4096, 0),
            Some(200000)
        ));
        assert!(!is_context_overflow(
            &length_stop(100, 0, 0, 0),
            Some(200000)
        ));
    }

    #[test]
    fn silent_overflow_needs_a_context_window() {
        let mut message = length_stop(300, 0, 5, 0);
        message.stop_reason = StopReason::Stop;
        assert!(is_context_overflow(&message, Some(200)));
        assert!(!is_context_overflow(&message, None));
        assert!(!is_context_overflow(&message, Some(0)));
    }
}
