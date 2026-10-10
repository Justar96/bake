//! Request transforms and wire protocols ported from Pi `packages/ai/src/api/`.

pub mod anthropic_messages;
pub mod constrained_sampling;
pub mod openai_completions;
pub mod openai_responses;
pub mod openai_responses_shared;
pub mod simple_options;
pub mod transform_messages;

#[cfg(test)]
mod protocol_tests;
#[cfg(test)]
mod request_tests;

use std::future::Future;

use serde_json::Value;

use crate::types::{AssistantMessage, Model, StopReason};
use crate::utils::event_stream::{
    AssistantMessageEventSender, AssistantMessageEventStream, assistant_message_channel,
};

/// Runs `run` on the current Tokio runtime and returns its stream at once.
/// Outside a runtime the stream ends with an error message instead.
pub(crate) fn spawn_stream<F, Fut>(model: &Model, run: F) -> AssistantMessageEventStream
where
    F: FnOnce(AssistantMessageEventSender) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (sender, stream) = assistant_message_channel();
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(run(sender));
        }
        Err(_) => sender.finish(error_message(
            model,
            "bake-ai streams must run inside a Tokio runtime",
        )),
    }
    stream
}

/// A failed response for `model` that never started.
pub(crate) fn error_message(model: &Model, error: &str) -> AssistantMessage {
    let mut message = AssistantMessage::pending(model);
    message.stop_reason = StopReason::Error;
    message.error_message = Some(error.to_owned());
    message
}

/// A JSON count as a token number: integers as given, other finite
/// non-negative numbers truncated, anything else `None`.
pub(crate) fn count(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .map(|n| n as u64)
    })
}

/// JavaScript truthiness of a JSON value.
pub(crate) fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}

/// A non-empty string member.
pub(crate) fn non_empty_str(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

/// Replaces every UTF-16 unit outside `[A-Za-z0-9_-]` with `_`, as Pi's
/// `replace(/[^a-zA-Z0-9_-]/g, "_")` does.
pub(crate) fn sanitize_id_chars(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            for _ in 0..ch.len_utf16() {
                out.push('_');
            }
        }
    }
    out
}

/// The first `count` UTF-16 units of an id made of single-unit characters.
pub(crate) fn truncate_units(text: &str, count: usize) -> String {
    let mut out = String::new();
    let mut units = 0;
    for ch in text.chars() {
        units += ch.len_utf16();
        if units > count {
            break;
        }
        out.push(ch);
    }
    out
}
