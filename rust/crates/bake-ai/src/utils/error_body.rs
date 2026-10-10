//! Provider HTTP failures and their display text.
//!
//! Ported from Pi `packages/ai/src/utils/error-body.ts` (v1.1.0), together
//! with the message rules of the OpenAI and Anthropic SDKs' `APIError` that
//! Pi's adapters surface (`openai` 7.19.0 `core/error.js`,
//! `@anthropic-ai/sdk` `core/error.js`): a non-2xx response becomes
//! `"<status> <message>"`, where the message is the body's `error.message`,
//! else the JSON of its `error` member, else the raw body text.

use std::collections::BTreeMap;

use serde_json::Value;

/// The cap of a surfaced error body, in UTF-16 code units.
pub const MAX_PROVIDER_ERROR_BODY_CHARS: usize = 4000;

/// How a provider request failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// A non-2xx HTTP response.
    Http,
    /// No response: connection, DNS, or TLS failure.
    Connection,
    /// No response within the request timeout.
    Timeout,
    /// The request's signal aborted.
    Aborted,
    /// Anything else, such as an invalid request or stream.
    Other,
}

/// A failed provider request, shaped like the SDK error objects Pi
/// normalizes.
///
/// The details are boxed so `Result<_, ProviderError>` stays small.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderError(Box<ProviderErrorDetails>);

/// The fields of a [`ProviderError`].
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderErrorDetails {
    /// The failure class.
    pub kind: ProviderErrorKind,
    /// The HTTP status, for [`ProviderErrorKind::Http`].
    pub status: Option<u16>,
    /// Response headers, names lowercased.
    pub headers: BTreeMap<String, String>,
    /// The SDK-style message.
    pub message: String,
    /// The parsed body's `error` member (the SDKs' `error.error`).
    pub error: Option<Value>,
}

impl std::ops::Deref for ProviderError {
    type Target = ProviderErrorDetails;

    fn deref(&self) -> &ProviderErrorDetails {
        &self.0
    }
}

impl std::ops::DerefMut for ProviderError {
    fn deref_mut(&mut self) -> &mut ProviderErrorDetails {
        &mut self.0
    }
}

impl From<ProviderErrorDetails> for ProviderError {
    fn from(details: ProviderErrorDetails) -> Self {
        Self(Box::new(details))
    }
}

impl ProviderErrorDetails {
    fn other(message: String) -> Self {
        Self {
            kind: ProviderErrorKind::Other,
            status: None,
            headers: BTreeMap::new(),
            message,
            error: None,
        }
    }
}

impl ProviderError {
    /// A failure with only a message.
    pub fn other(message: impl Into<String>) -> Self {
        ProviderErrorDetails::other(message.into()).into()
    }

    /// The abort failure.
    pub fn aborted() -> Self {
        ProviderErrorDetails {
            kind: ProviderErrorKind::Aborted,
            ..ProviderErrorDetails::other("Request was aborted".to_owned())
        }
        .into()
    }

    /// A connection failure; `detail` follows the SDK's `Connection error.`.
    pub fn connection(detail: &str) -> Self {
        let message = if detail.is_empty() {
            "Connection error.".to_owned()
        } else {
            format!("Connection error. {detail}")
        };
        ProviderErrorDetails {
            kind: ProviderErrorKind::Connection,
            ..ProviderErrorDetails::other(message)
        }
        .into()
    }

    /// A request timeout.
    pub fn timeout() -> Self {
        ProviderErrorDetails {
            kind: ProviderErrorKind::Timeout,
            ..ProviderErrorDetails::other("Request timed out.".to_owned())
        }
        .into()
    }

    /// A non-2xx response with its body text, as the SDKs build it.
    pub fn http(status: u16, headers: BTreeMap<String, String>, body: &str) -> Self {
        let parsed: Option<Value> = serde_json::from_str(body).ok();
        let error = parsed
            .as_ref()
            .and_then(|value| value.get("error"))
            .cloned();
        let raw_message = if parsed.is_some() { None } else { Some(body) };
        let message = make_message(Some(status), error.as_ref(), raw_message);
        ProviderErrorDetails {
            kind: ProviderErrorKind::Http,
            status: Some(status),
            headers,
            message,
            error,
        }
        .into()
    }

    /// A non-2xx response as `@anthropic-ai/sdk` builds it: the error is the
    /// whole parsed body.
    pub fn anthropic_http(status: u16, headers: BTreeMap<String, String>, body: &str) -> Self {
        let parsed: Option<Value> = serde_json::from_str(body).ok();
        let raw_message = if parsed.is_some() { None } else { Some(body) };
        let message = make_message(Some(status), parsed.as_ref(), raw_message);
        ProviderErrorDetails {
            kind: ProviderErrorKind::Http,
            status: Some(status),
            headers,
            message,
            error: parsed,
        }
        .into()
    }

    /// An `error` payload received inside a stream, without a status.
    pub fn stream_payload(error: Value) -> Self {
        let message = make_message(None, Some(&error), None);
        ProviderErrorDetails {
            kind: ProviderErrorKind::Other,
            error: Some(error),
            ..ProviderErrorDetails::other(message)
        }
        .into()
    }
}

fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// The SDKs' `APIError.makeMessage`.
pub fn make_message(status: Option<u16>, error: Option<&Value>, message: Option<&str>) -> String {
    let error_message = error
        .and_then(|error| error.get("message"))
        .filter(|value| is_truthy(value));
    let msg: Option<String> = match error_message {
        Some(Value::String(text)) => Some(text.clone()),
        Some(other) => Some(other.to_string()),
        None => match error.filter(|error| is_truthy(error)) {
            Some(error) => Some(error.to_string()),
            None => message
                .filter(|message| !message.is_empty())
                .map(str::to_owned),
        },
    };
    match (status, msg) {
        (Some(status), Some(msg)) => format!("{status} {msg}"),
        (Some(status), None) => format!("{status} status code (no body)"),
        (None, Some(msg)) => msg,
        (None, None) => "(no status code or body)".to_owned(),
    }
}

/// Pi's `NormalizedProviderError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedProviderError {
    /// HTTP status.
    pub status: Option<u16>,
    /// The body, trimmed and truncated.
    pub body: Option<String>,
    /// The message.
    pub message: String,
    /// Whether `message` already contains `body`.
    pub message_carries_body: bool,
}

/// Pi's `normalizeProviderError`: the status, and the parsed `error` member
/// as body text when it is a non-empty object.
pub fn normalize_provider_error(error: &ProviderError) -> NormalizedProviderError {
    let body = error
        .error
        .as_ref()
        .filter(|value| value.as_object().is_some_and(|object| !object.is_empty()))
        .map(Value::to_string)
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .map(|text| truncate_error_text(&text, MAX_PROVIDER_ERROR_BODY_CHARS));
    let message_carries_body = body
        .as_ref()
        .is_none_or(|body| error.message.contains(body.as_str()));
    NormalizedProviderError {
        status: error.status,
        body,
        message: error.message.clone(),
        message_carries_body,
    }
}

/// Pi's `formatProviderError`.
pub fn format_provider_error(norm: &NormalizedProviderError, prefix: Option<&str>) -> String {
    match (&norm.body, norm.status) {
        (Some(body), Some(status)) if !norm.message_carries_body => match prefix {
            Some(prefix) => format!("{prefix} ({status}): {body}"),
            None => format!("{status}: {body}"),
        },
        _ => match (prefix, norm.status) {
            (Some(prefix), Some(status)) => format!("{prefix} ({status}): {}", norm.message),
            _ => norm.message.clone(),
        },
    }
}

/// Pi's `truncateErrorText`, counting UTF-16 code units as JavaScript does.
pub fn truncate_error_text(text: &str, max_chars: usize) -> String {
    let units = text.encode_utf16().count();
    if units <= max_chars {
        return text.to_owned();
    }
    let mut kept = String::new();
    let mut count = 0;
    for ch in text.chars() {
        let width = ch.len_utf16();
        if count + width > max_chars {
            break;
        }
        kept.push(ch);
        count += width;
    }
    format!("{kept}... [truncated {} chars]", units - max_chars)
}

#[cfg(test)]
mod tests {
    //! Cases from Pi `packages/ai/test/error-body.test.ts` and
    //! `provider-error-body-regression.test.ts`, applied to the SDK shapes
    //! these adapters produce.

    use super::*;
    use serde_json::json;

    #[test]
    fn builds_sdk_messages() {
        assert_eq!(
            ProviderError::http(400, BTreeMap::new(), "{\"error\":{\"message\":\"bad\"}}").message,
            "400 bad"
        );
        assert_eq!(
            ProviderError::http(403, BTreeMap::new(), "").message,
            "403 status code (no body)"
        );
        assert_eq!(
            ProviderError::http(502, BTreeMap::new(), "upstream down").message,
            "502 upstream down"
        );
        assert_eq!(
            ProviderError::http(400, BTreeMap::new(), "{\"error\":\"bad\"}").message,
            "400 \"bad\""
        );
        assert_eq!(
            ProviderError::http(400, BTreeMap::new(), "{\"detail\":1}").message,
            "400 status code (no body)"
        );
        assert_eq!(
            ProviderError::stream_payload(json!({ "message": "boom" })).message,
            "boom"
        );
    }

    #[test]
    fn surfaces_the_body_when_the_message_does_not_carry_it() {
        let error = ProviderError::http(
            400,
            BTreeMap::new(),
            "{\"error\":{\"message\":\"bad\",\"code\":\"invalid\"}}",
        );
        let norm = normalize_provider_error(&error);
        assert_eq!(
            norm.body.as_deref(),
            Some("{\"message\":\"bad\",\"code\":\"invalid\"}")
        );
        assert!(!norm.message_carries_body);
        assert_eq!(
            format_provider_error(&norm, None),
            "400: {\"message\":\"bad\",\"code\":\"invalid\"}"
        );
        assert_eq!(
            format_provider_error(&norm, Some("OpenAI API error")),
            "OpenAI API error (400): {\"message\":\"bad\",\"code\":\"invalid\"}"
        );
    }

    #[test]
    fn keeps_messages_without_a_body() {
        let error = ProviderError::http(403, BTreeMap::new(), "");
        let norm = normalize_provider_error(&error);
        assert_eq!(
            format_provider_error(&norm, None),
            "403 status code (no body)"
        );
        assert_eq!(
            format_provider_error(&norm, Some("x API error")),
            "x API error (403): 403 status code (no body)"
        );
        let connection = normalize_provider_error(&ProviderError::connection(""));
        assert_eq!(
            format_provider_error(&connection, Some("x API error")),
            "Connection error."
        );
    }

    #[test]
    fn truncates_long_bodies_in_utf16_units() {
        let text = "é".repeat(4005);
        let cut = truncate_error_text(&text, MAX_PROVIDER_ERROR_BODY_CHARS);
        assert!(cut.ends_with("... [truncated 5 chars]"));
        assert_eq!(truncate_error_text("short", 10), "short");
        let emoji = "🙈".repeat(3);
        assert_eq!(truncate_error_text(&emoji, 3), "🙈... [truncated 3 chars]");
    }
}
