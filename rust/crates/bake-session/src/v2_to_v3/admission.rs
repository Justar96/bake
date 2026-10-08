//! `assertEvent(event, 2)` from
//! `packages/session/session-format-v2-to-v3/src/payload.ts`: admission of
//! one strictly decoded source event before the V2-to-V3 transformation.
//!
//! The event is the codec's logical event: a JSON object whose members keep
//! the physical row's order, with `sourceEventSeqs` already expanded, a
//! string `type`, and a dense `seq`. Checks run in TypeScript's order and the
//! first failure wins. [`StageError::Unsupported`] marks what TypeScript
//! throws as `SessionFormatUnsupportedMigrationError`; a
//! [`StageError::NativeLimit`] fires where TypeScript's outcome depends on
//! JavaScript behavior this crate does not reproduce, never before an earlier
//! TypeScript rejection.

mod dispositions;

use serde_json::{Map, Value};

use super::StageError;
use super::payload_semantics::{
    Checked, assert_released_payload_semantics, content_block_fields, count, invalid, js_keys,
    quote, released_keys, released_record, safe_integer, stringify,
};
use crate::MAX_SAFE_INTEGER;
use dispositions::Lookup;
pub(crate) use dispositions::OBJECT_PROTOTYPE_NAMES;

type Record = Map<String, Value>;

const SURFACE_TYPES: [&str; 4] = [
    "system/message",
    "user/message",
    "assistant/message",
    "tool/result",
];
const SOURCE_KINDS: [&str; 15] = [
    "user",
    "plugin",
    "model",
    "tool",
    "agent-instructions",
    "session-reference",
    "team-message",
    "goal",
    "skill-invocation",
    "skill-catalog",
    "coordinator",
    "subagent-report",
    "subagent-settled",
    "webhook",
    "agent-message",
];
/// The caller passed something other than a decoded v2 event.
const INVARIANT: &str = "admission-invariant";
const CONTENT_KINDS: [&str; 6] = [
    "text",
    "reasoning",
    "image",
    "file",
    "tool-call",
    "tool-result",
];

fn unsupported<T>(message: String) -> Checked<T> {
    Err(StageError::Unsupported(message))
}

/// `record` in `payload.ts`.
fn record<'a>(value: Option<&'a Value>, label: &str) -> Checked<&'a Record> {
    match value {
        Some(Value::Object(fields)) => Ok(fields),
        _ => invalid(format!("{label} must be an object")),
    }
}

/// `keys` in `payload.ts`: the first missing member, then the first
/// unexpected member in JavaScript order.
fn keys(value: &Record, required: &[&str], optional: &[&str], label: &str) -> Checked {
    if let Some(key) = required.iter().find(|key| !value.contains_key(**key)) {
        return invalid(format!("{label} lacks required field {key}"));
    }
    if let Some(key) = js_keys(value)
        .into_iter()
        .find(|key| !required.contains(key) && !optional.contains(key))
    {
        return invalid(format!("{label} has unexpected field {key}"));
    }
    Ok(())
}

/// Admit one decoded released-v2 event for transformation.
pub(super) fn assert_source_event(event: &Value) -> Checked {
    let Some(event) = event.as_object() else {
        return Err(StageError::NativeLimit(INVARIANT.to_owned()));
    };
    let Some(event_type) = event.get("type").and_then(Value::as_str) else {
        return Err(StageError::NativeLimit(INVARIANT.to_owned()));
    };
    let disposition = dispositions::lookup(event_type);
    let feedback = matches!(
        event_type,
        "feedback/message-put" | "feedback/message-delete"
    );
    if disposition == Lookup::Absent && !feedback {
        return unsupported(format!(
            "format v2 to v3 cannot safely transform unclassified event {event_type}"
        ));
    }
    let surface = SURFACE_TYPES.contains(&event_type);
    let envelope_optional: &[&str] = if surface {
        &["ignorable", "sourceEventSeqs", "surfaceOp"]
    } else {
        &["ignorable"]
    };
    keys(
        event,
        &["type", "seq", "time", "data"],
        envelope_optional,
        event_type,
    )?;
    let seq = count(event.get("seq"), "event seq")?;
    safe_integer(event.get("time"), "event time")?;
    if event
        .get("ignorable")
        .is_some_and(|flag| flag != &Value::Bool(true))
    {
        return invalid("ignorable must be true".to_owned());
    }
    if surface {
        assert_surface_metadata(event, seq, event_type)?;
        if !event.contains_key("surfaceOp") {
            return invalid(format!("{event_type} requires surfaceOp"));
        }
    }
    let data = record(event.get("data"), &format!("{event_type} data"))?;
    if feedback {
        return assert_feedback(event_type, data);
    }
    let admitted = match disposition {
        Lookup::Own(admitted) => admitted,
        // TypeScript reads `.required` of the inherited member and throws an
        // engine `TypeError`, whose message is not format behavior.
        Lookup::Inherited | Lookup::Absent => {
            return Err(StageError::NativeLimit("object-prototype-type".to_owned()));
        }
    };
    keys(
        data,
        admitted.required,
        admitted.optional,
        &format!("{event_type} data"),
    )?;
    assert_owned_content(event_type, seq, data)?;
    // Assistant attempts are introduced by V2; the frozen helper has no case for them.
    if event_type != "assistant/attempt" {
        assert_released_payload_semantics(event_type, seq, event.get("data"), 2)?;
    }
    if matches!(event_type, "assistant/message" | "assistant/attempt") {
        for coordinate in ["turn", "step"] {
            if count(data.get(coordinate), coordinate)? == 0 {
                return invalid(format!("{coordinate} must be positive"));
            }
        }
    }
    if event_type == "session/end-seed"
        && data
            .get("inherited")
            .is_some_and(|flag| flag != &Value::Bool(true))
    {
        return invalid("session/end-seed inherited must be true".to_owned());
    }
    // Source classification applies only to Harness messages, not team delivery envelopes.
    match event_type {
        "user/message" => assert_source(data)?,
        "assistant/message" | "tool/result" => {
            assert_source(record(data.get("message"), "message")?)?
        }
        _ => {}
    }
    if event_type == "tool/result" && is_not_started(data) {
        let message = record(data.get("message"), "tool result message")?;
        let source = record(message.get("source"), "tool result source")?;
        if !is_repair_identity(message.get("id"), source.get("callId")) {
            return invalid(
                "TOOL_NOT_STARTED repair requires its canonical historical message id".to_owned(),
            );
        }
    }
    let messages = match event_type {
        "agent/inbox/spliced" => data.get("inserted"),
        "session/title-llm-request" => data.get("messages"),
        _ => None,
    };
    if let Some(messages) = messages {
        // The frozen semantics proved an array of message objects.
        for message in messages.as_array().into_iter().flatten() {
            assert_source(record(Some(message), "message")?)?;
        }
    }
    Ok(())
}

fn is_not_started(data: &Record) -> bool {
    data.get("error")
        .and_then(Value::as_object)
        .is_some_and(|error| error.get("code") == Some(&Value::from("TOOL_NOT_STARTED")))
}

/// `assertReleasedSurfaceMetadata(event, seq, type, 'forbid-assistant')` from
/// `session-format-v0-to-v1/src/validation.ts`.
fn assert_surface_metadata(event: &Record, seq: u64, event_type: &str) -> Checked {
    let sources = event.get("sourceEventSeqs");
    if event_type == "assistant/message" && sources.is_some() {
        return invalid(format!(
            "assistant/message {seq} retains obsolete chunk references"
        ));
    }
    if let Some(sources) = sources {
        let Value::Array(sources) = sources else {
            return invalid(format!(
                "{event_type} {seq} sourceEventSeqs must be an array"
            ));
        };
        let mut seen = std::collections::HashSet::new();
        for source in sources {
            let label = format!("{event_type} {seq} sourceEventSeqs member");
            let current = count(Some(source), &label)?;
            if current >= seq || !seen.insert(current) {
                return invalid(format!(
                    "{event_type} {seq} sourceEventSeqs must be unique earlier seqs"
                ));
            }
        }
        if sources.is_empty() {
            return invalid(format!(
                "{event_type} {seq} sourceEventSeqs must be non-empty"
            ));
        }
    }
    let operation = match event.get("surfaceOp") {
        None => return Ok(()),
        Some(operation) if operation == "append" => return Ok(()),
        Some(operation) => operation,
    };
    let label = format!("{event_type} {seq} surfaceOp");
    let replacement = released_record(Some(operation), &label)?;
    released_keys(replacement, &["op", "start", "end"], &[], &label)?;
    if replacement.get("op") != Some(&Value::from("replace")) {
        return invalid(format!("{label} must replace"));
    }
    let start = count(
        replacement.get("start"),
        &format!("{event_type} {seq} surface start"),
    )?;
    let end = count(
        replacement.get("end"),
        &format!("{event_type} {seq} surface end"),
    )?;
    if start >= seq || end >= seq {
        return invalid(format!(
            "{event_type} {seq} has an invalid surface replacement"
        ));
    }
    Ok(())
}

/// `isRepairIdentity`: `interrupted-tool-result-<callId>-<n>` where `n` is a
/// canonical decimal that `Number` reads as a safe integer.
fn is_repair_identity(id: Option<&Value>, call_id: Option<&Value>) -> bool {
    let (Some(Value::String(id)), Some(Value::String(call_id))) = (id, call_id) else {
        return false;
    };
    let prefix = format!("interrupted-tool-result-{call_id}-");
    let Some(suffix) = id.strip_prefix(&prefix) else {
        return false;
    };
    let canonical = suffix == "0"
        || (!suffix.is_empty()
            && !suffix.starts_with('0')
            && suffix.bytes().all(|b| b.is_ascii_digit()));
    // Every decimal above 2^53 - 1 reads as at least 2^53.
    canonical && suffix.len() <= 16 && suffix.parse::<u64>().is_ok_and(|n| n <= MAX_SAFE_INTEGER)
}

fn assert_source(message: &Record) -> Checked {
    let source = record(message.get("source"), "message source")?;
    let kind = source.get("kind").and_then(Value::as_str);
    if !kind.is_some_and(|kind| SOURCE_KINDS.contains(&kind)) {
        return unsupported("cannot safely transform unclassified message source".to_owned());
    }
    if kind == Some("agent-message") {
        keys(
            source,
            &["kind", "form", "senderSessionId"],
            &[],
            "agent-message source",
        )?;
        let relay = source.get("form") == Some(&Value::from("relay"));
        let sender = source
            .get("senderSessionId")
            .and_then(Value::as_str)
            .is_some_and(|sender| !sender.is_empty());
        if !relay || !sender {
            return invalid(
                "agent-message source requires relay form and senderSessionId".to_owned(),
            );
        }
    }
    Ok(())
}

fn content_array<'a>(value: Option<&'a Value>, label: &str) -> Checked<&'a [Value]> {
    match value {
        Some(Value::Array(items)) => Ok(items),
        _ => invalid(format!("{label}: content must be an array")),
    }
}

/// `assertOwnedContent`: refuse content kinds V3 cannot classify. Model and
/// tool JSON outside content blocks, and stream entries other than raw
/// `block-start` and `block-end` chunks, stay opaque.
fn assert_owned_content(event_type: &str, seq: u64, data: &Record) -> Checked {
    let label = format!("format v2 {event_type} at seq {seq} data");
    match event_type {
        "user/message" | "tool/code-dispatch" => {
            assert_content_kinds(data.get("content"), &format!("{label}.content"))?;
        }
        "assistant/message" | "tool/result" | "team/message/queued" => {
            let message = record(data.get("message"), &format!("{label}.message"))?;
            assert_content_kinds(message.get("content"), &format!("{label}.message.content"))?;
        }
        "agent/inbox/spliced" | "session/title-llm-request" => {
            let field = if event_type == "agent/inbox/spliced" {
                "inserted"
            } else {
                "messages"
            };
            let items = content_array(data.get(field), &format!("{label}.{field}"))?;
            for (index, value) in items.iter().enumerate() {
                let path = format!("{label}.{field}[{index}]");
                let message = record(Some(value), &path)?;
                assert_content_kinds(message.get("content"), &format!("{path}.content"))?;
            }
        }
        "compaction/summary" => {
            assert_content_kinds(data.get("summary"), &format!("{label}.summary"))?;
            if let Some(raw) = data.get("rawOutput") {
                assert_content_kinds(Some(raw), &format!("{label}.rawOutput"))?;
            }
        }
        _ => {}
    }
    if matches!(event_type, "assistant/message" | "assistant/attempt") {
        let stream = content_array(data.get("stream"), &format!("{label}.stream"))?;
        for (index, value) in stream.iter().enumerate() {
            let path = format!("{label}.stream[{index}]");
            let entry = record(Some(value), &path)?;
            if entry.get("type") != Some(&Value::from("chunk")) {
                continue;
            }
            let chunk = record(entry.get("chunk"), &format!("{path}.chunk"))?;
            match chunk.get("type").and_then(Value::as_str) {
                Some("block-end") => {
                    assert_content_block(chunk.get("block"), &format!("{path}.chunk.block"))?;
                }
                Some("block-start") => {
                    assert_content_kind(
                        chunk.get("blockType"),
                        &format!("{path}.chunk.blockType"),
                    )?;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn assert_content_kind(kind: Option<&Value>, label: &str) -> Checked {
    if kind
        .and_then(Value::as_str)
        .is_some_and(|kind| CONTENT_KINDS.contains(&kind))
    {
        return Ok(());
    }
    unsupported(format!(
        "{label}: cannot safely transform unclassified message content kind {}",
        stringify(kind)?
    ))
}

fn assert_content_kinds(content: Option<&Value>, label: &str) -> Checked {
    for (index, value) in content_array(content, label)?.iter().enumerate() {
        assert_content_block(Some(value), &format!("{label}[{index}]"))?;
    }
    Ok(())
}

fn assert_content_block(value: Option<&Value>, label: &str) -> Checked {
    let block = record(value, label)?;
    assert_content_kind(block.get("type"), label)?;
    let kind = block
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if kind == "tool-result" {
        if !block.get("content").is_some_and(Value::is_array) {
            return invalid(format!(
                "{label}.content: invalid message content kind \"tool-result\": content must be an array"
            ));
        }
        assert_content_kinds(block.get("content"), &format!("{label}.content"))?;
    }
    if kind == "file" {
        let file = format!("{label} kind \"file\"");
        keys(block, &["type", "attachment"], &[], &file)?;
        let attachment_label = format!("{file} attachment");
        let attachment = record(block.get("attachment"), &attachment_label)?;
        keys(
            attachment,
            &["attachmentId", "name", "bytes"],
            &[],
            &attachment_label,
        )?;
        let id = attachment.get("attachmentId").and_then(Value::as_str);
        if !id.is_some_and(|id| !id.is_empty())
            || !attachment.get("name").is_some_and(Value::is_string)
        {
            return invalid(format!(
                "{file}: file attachment requires attachmentId and name"
            ));
        }
        count(
            attachment.get("bytes"),
            &format!("{attachment_label} bytes"),
        )?;
        return Ok(());
    }
    // The frozen field rules run without revisiting tool-result children; the
    // synthetic `user/message` probe places the block at `content[0]`.
    let emptied: Option<Record> = (kind == "tool-result").then(|| {
        let fields = block.iter().map(|(key, value)| {
            let value = if key == "content" {
                Value::Array(Vec::new())
            } else {
                value.clone()
            };
            (key.clone(), value)
        });
        fields.collect()
    });
    match content_block_fields(
        emptied.as_ref().unwrap_or(block),
        "user/message 0 content[0]",
    ) {
        Err(StageError::Invalid(detail)) => invalid(format!(
            "{label}: invalid message content kind {}: SessionFormatError: {detail}",
            quote(kind)
        )),
        outcome => outcome,
    }
}

/// `assertFeedback`.
fn assert_feedback(event_type: &str, data: &Record) -> Checked {
    let put = event_type == "feedback/message-put";
    let required: &[&str] = if put {
        &["sessionId", "item"]
    } else {
        &["sessionId", "messageId"]
    };
    keys(data, required, &[], event_type)?;
    if !data.get("sessionId").is_some_and(Value::is_string) {
        return invalid("feedback sessionId must be a string".to_owned());
    }
    if !put {
        if !data.get("messageId").is_some_and(Value::is_string) {
            return invalid("feedback messageId must be a string".to_owned());
        }
        return Ok(());
    }
    let item = record(data.get("item"), "feedback item")?;
    keys(
        item,
        &["messageId", "rating", "version", "createdAt", "updatedAt"],
        &["note"],
        "feedback item",
    )?;
    for key in ["messageId", "version"] {
        if !item.get(key).is_some_and(Value::is_string) {
            return invalid(format!("feedback {key} must be a string"));
        }
    }
    let rating = item.get("rating").and_then(Value::as_str);
    if !matches!(rating, Some("positive" | "negative")) {
        return invalid("invalid feedback rating".to_owned());
    }
    if item.get("note").is_some_and(|note| !note.is_string()) {
        return invalid("feedback note must be a string".to_owned());
    }
    count(item.get("createdAt"), "feedback createdAt")?;
    count(item.get("updatedAt"), "feedback updatedAt")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Expect {
        Ok,
        Invalid,
        Unsupported,
        Limit,
    }
    use Expect::{Invalid, Limit, Ok, Unsupported};

    /// Outcomes recorded from the real `assertEvent(JSON.parse(row), 2)`;
    /// `Limit` marks the engine `TypeError` an inherited type name raises.
    const CASES: &[(&str, Expect, &str)] = &[
        (
            r#"{"type":"user/message","seq":5,"time":0,"data":{"id":"u","role":"user","content":[{"type":"text","text":"x"}],"source":{"kind":"user"}},"surfaceOp":"append"}"#,
            Ok,
            r#""#,
        ),
        (
            r#"{"type":"user/message","seq":5,"time":0,"data":{"id":"u","role":"user","content":[{"type":"text","text":"x"}],"source":{"kind":"user"}},"surfaceOp":"append","sourceEventSeqs":[3,3]}"#,
            Invalid,
            r#"user/message 5 sourceEventSeqs must be unique earlier seqs"#,
        ),
        (
            r#"{"type":"user/message","seq":5,"time":0,"data":{"id":"u","role":"user","content":[{"type":"text","text":"x"}],"source":{"kind":"user"}},"surfaceOp":"append","sourceEventSeqs":[5]}"#,
            Invalid,
            r#"user/message 5 sourceEventSeqs must be unique earlier seqs"#,
        ),
        (
            r#"{"type":"user/message","seq":5,"time":0,"data":{"id":"u","role":"user","content":[{"type":"text","text":"x"}],"source":{"kind":"user"}},"surfaceOp":"append","sourceEventSeqs":[]}"#,
            Invalid,
            r#"user/message 5 sourceEventSeqs must be non-empty"#,
        ),
        (
            r#"{"type":"user/message","seq":5,"time":0,"data":{"id":"u","role":"user","content":[{"type":"text","text":"x"}],"source":{"kind":"user"}},"surfaceOp":{"op":"replace","start":1,"end":5}}"#,
            Invalid,
            r#"user/message 5 has an invalid surface replacement"#,
        ),
        (
            r#"{"type":"user/message","seq":5,"time":0,"data":{"id":"u","role":"user","content":[{"type":"text","text":"x"}],"source":{"kind":"user"}},"surfaceOp":{"end":1,"start":1,"op":"swap"}}"#,
            Invalid,
            r#"user/message 5 surfaceOp must replace"#,
        ),
        (
            r#"{"type":"user/message","seq":5,"time":0,"data":{"id":"u","role":"user","content":[{"type":"text","text":"x"}],"source":{"kind":"user"}},"surfaceOp":{"7":0,"op":"replace","start":1,"end":2}}"#,
            Invalid,
            r#"user/message 5 surfaceOp has unexpected member "7""#,
        ),
        (
            r#"{"type":"user/message","seq":5,"time":0,"data":{"id":"u","role":"user","content":[{"type":"text","text":"x"}],"source":{"kind":"user"}}}"#,
            Invalid,
            r#"user/message requires surfaceOp"#,
        ),
        (
            r#"{"type":"user/message","seq":5,"time":0,"data":{"id":"u","role":"user","content":[{"type":"text","text":"x"}],"source":{"kind":"user"}},"surfaceOp":"append","ignorable":false}"#,
            Invalid,
            r#"ignorable must be true"#,
        ),
        (
            r#"{"type":"assistant/message","seq":5,"time":0,"data":{},"surfaceOp":"append","sourceEventSeqs":[]}"#,
            Invalid,
            r#"assistant/message 5 retains obsolete chunk references"#,
        ),
        (
            r#"{"type":"tool/call","seq":5,"time":0,"data":{},"surfaceOp":"append"}"#,
            Invalid,
            r#"tool/call has unexpected field surfaceOp"#,
        ),
        (
            r#"{"type":"user/message","seq":5,"time":0,"data":{"id":"u","role":"user","content":[{"type":"text","text":"x"}],"source":{"kind":"session-reference","form":"recall","version":1,"references":[{"sessionId":"s","label":"l","capturedThroughSeq":1,"compacted":false,"originalMessages":1,"retainedMessages":1,"omittedMessages":0,"omittedBytes":0,"truncated":false,"inputIndex":0},{"sessionId":"s","label":"l","capturedThroughSeq":1,"compacted":false,"originalMessages":1,"retainedMessages":1,"omittedMessages":0,"omittedBytes":0,"truncated":false,"inputIndex":1}]}},"surfaceOp":"append"}"#,
            Invalid,
            r#"user/message 5 source repeats sessionId s"#,
        ),
        (
            r#"{"type":"user/message","seq":5,"time":0,"data":{"id":"u","role":"user","content":[{"type":"text","text":"x"}],"source":{"kind":"session-reference","form":"recall","version":1,"references":[{"sessionId":"s","label":"l","capturedThroughSeq":1,"compacted":false,"originalMessages":1,"retainedMessages":1,"omittedMessages":0,"omittedBytes":0,"truncated":true,"inputIndex":0}]}},"surfaceOp":"append"}"#,
            Invalid,
            r#"user/message 5 source references[0] truncated disagrees with omitted content"#,
        ),
        (
            r#"{"type":"user/message","seq":5,"time":0,"data":{"id":"u","role":"user","content":[{"type":"text","text":"x"}],"source":{"kind":"session-reference","form":"recall","version":1,"references":[{"sessionId":"s","label":"l","capturedThroughSeq":1,"compacted":false,"originalMessages":1,"retainedMessages":1,"omittedMessages":0,"omittedBytes":0,"truncated":false,"inputIndex":0,"capturedFormatVersion":3}]}},"surfaceOp":"append"}"#,
            Invalid,
            r#"user/message 5 source references[0] capturedFormatVersion must be between 1 and 2"#,
        ),
        (
            r#"{"type":"user/message","seq":5,"time":0,"data":{"id":"u","role":"user","content":[{"type":"text","text":"x"}],"source":{"kind":"session-reference","form":"recall","version":1,"references":[]}},"surfaceOp":"append"}"#,
            Invalid,
            r#"user/message 5 source references must be non-empty"#,
        ),
        (
            r#"{"type":"user/message","seq":5,"time":0,"data":{"id":"u","role":"user","content":[{"type":"text","text":"x"}],"source":{"kind":"agent-message","form":"relay","senderSessionId":"s","extra":1}},"surfaceOp":"append"}"#,
            Invalid,
            r#"agent-message source has unexpected field extra"#,
        ),
        (
            r#"{"type":"user/message","seq":5,"time":0,"data":{"id":"u","role":"user","content":[{"type":"text","text":"x"}],"source":{"kind":"plugin","plugin":"p","form":"relay","summary":"s"}},"surfaceOp":"append"}"#,
            Invalid,
            r#"user/message 5 source summary requires notice form"#,
        ),
        (
            r#"{"type":"tool/result","seq":5,"time":0,"data":{"turn":1,"step":1,"message":{"id":"interrupted-tool-result-c-12","role":"user","content":[{"type":"tool-result","toolCallId":"c","content":[],"isError":true}],"source":{"kind":"tool","callId":"c"}},"error":{"name":"n","code":"TOOL_NOT_STARTED"}},"surfaceOp":"append"}"#,
            Ok,
            r#""#,
        ),
        (
            r#"{"type":"tool/result","seq":5,"time":0,"data":{"turn":1,"step":1,"message":{"id":"interrupted-tool-result-c-012","role":"user","content":[{"type":"tool-result","toolCallId":"c","content":[],"isError":true}],"source":{"kind":"tool","callId":"c"}},"error":{"name":"n","code":"TOOL_NOT_STARTED"}},"surfaceOp":"append"}"#,
            Invalid,
            r#"TOOL_NOT_STARTED repair requires its canonical historical message id"#,
        ),
        (
            r#"{"type":"tool/result","seq":5,"time":0,"data":{"turn":1,"step":1,"message":{"id":"interrupted-tool-result-c-9007199254740992","role":"user","content":[{"type":"tool-result","toolCallId":"c","content":[],"isError":true}],"source":{"kind":"tool","callId":"c"}},"error":{"name":"n","code":"TOOL_NOT_STARTED"}},"surfaceOp":"append"}"#,
            Invalid,
            r#"TOOL_NOT_STARTED repair requires its canonical historical message id"#,
        ),
        (
            r#"{"type":"tool/result","seq":5,"time":0,"data":{"turn":1,"step":1,"message":{"id":"interrupted-tool-result-c-9007199254740991","role":"user","content":[{"type":"tool-result","toolCallId":"c","content":[],"isError":true}],"source":{"kind":"tool","callId":"c"}},"error":{"name":"n","code":"TOOL_NOT_STARTED"}},"surfaceOp":"append"}"#,
            Ok,
            r#""#,
        ),
        (
            r#"{"type":"tool/result","seq":5,"time":0,"data":{"turn":1,"step":1,"message":{"id":"interrupted-tool-result-d-1","role":"user","content":[{"type":"tool-result","toolCallId":"c","content":[],"isError":true}],"source":{"kind":"tool","callId":"c"}},"error":{"name":"n","code":"TOOL_NOT_STARTED"}},"surfaceOp":"append"}"#,
            Invalid,
            r#"TOOL_NOT_STARTED repair requires its canonical historical message id"#,
        ),
        (
            r#"{"type":"user/message","seq":5,"time":0,"data":{"id":"u","role":"user","content":[{"type":"image","attachment":{"attachmentId":"a","mediaType":"image/bmp","bytes":1,"width":1,"height":1}}],"source":{"kind":"user"}},"surfaceOp":"append"}"#,
            Invalid,
            r#"format v2 user/message at seq 5 data.content[0]: invalid message content kind "image": SessionFormatError: user/message 0 content[0] attachment mediaType must be one of image/png, image/jpeg, image/webp, image/gif"#,
        ),
        (
            r#"{"type":"user/message","seq":5,"time":0,"data":{"id":"u","role":"user","content":[{"type":"tool-result","toolCallId":"c","content":[{"type":"mystery"}]}],"source":{"kind":"user"}},"surfaceOp":"append"}"#,
            Unsupported,
            r#"format v2 user/message at seq 5 data.content[0].content[0]: cannot safely transform unclassified message content kind "mystery""#,
        ),
        (
            r#"{"type":"user/message","seq":5,"time":0,"data":{"id":"u","role":"user","content":[{"1":{"0":-0,"b":[true,null,"q\""]},"type":"mystery"}],"source":{"kind":"user"}},"surfaceOp":"append"}"#,
            Unsupported,
            r#"format v2 user/message at seq 5 data.content[0]: cannot safely transform unclassified message content kind "mystery""#,
        ),
        (
            r#"{"type":"schedule/change","seq":5,"time":0,"data":{"version":1,"operation":"create","schedule":{"id":"s ","kind":"at","prompt":"p","scheduledAt":"2024-01-01T00:00:00.000Z"}}}"#,
            Invalid,
            r#"schedule/change 5 schedule id must not have surrounding whitespace"#,
        ),
        (
            r#"{"type":"schedule/change","seq":5,"time":0,"data":{"version":1,"operation":"create","schedule":{"id":"s","kind":"at","prompt":"p","scheduledAt":"1900-02-29T00:00:00.000Z"}}}"#,
            Invalid,
            r#"schedule/change 5 schedule scheduledAt must be a canonical UTC instant"#,
        ),
        (
            r#"{"type":"schedule/change","seq":5,"time":0,"data":{"version":1,"operation":"create","schedule":{"id":"s","kind":"at","prompt":"p","scheduledAt":"2000-02-29T00:00:00.000Z"}}}"#,
            Ok,
            r#""#,
        ),
        (
            r#"{"type":"hook/result","seq":5,"time":0,"data":{"turn":1,"point":"p","handlerId":"h","decision":"d","durationMs":-0}}"#,
            Invalid,
            r#"hook/result 5 durationMs must be a finite number"#,
        ),
        (
            r#"{"type":"step/start","seq":5,"time":-0,"data":{"turn":1,"step":1}}"#,
            Invalid,
            r#"event time must be a safe integer"#,
        ),
        (
            r#"{"type":"goal/change","seq":5,"time":0,"data":{"kind":"goal/change","version":1,"operation":"create","goal":{"id":"g","revision":1,"objective":"o","phase":"blocked","maxGoalRounds":1},"roundsStarted":0,"createdAt":0,"updatedAt":0}}"#,
            Invalid,
            r#"goal/change 5 goal blockedReason must be a JSON object"#,
        ),
        (
            r#"{"type":"feedback/message-delete","seq":5,"time":0,"data":{"sessionId":"s","messageId":1}}"#,
            Invalid,
            r#"feedback messageId must be a string"#,
        ),
        (
            r#"{"type":"valueOf","seq":5,"time":0,"data":{}}"#,
            Limit,
            r#"Cannot read properties of undefined (reading 'find')"#,
        ),
        (
            r#"{"type":"valueOf","seq":5,"time":0,"data":[]}"#,
            Invalid,
            r#"valueOf data must be an object"#,
        ),
        (
            r#"{"type":"tool/code-dispatch","seq":5,"time":0,"data":{"rootCallId":"r","parentCallId":"p","subCallId":"s","name":"n","arguments":0,"isError":false,"content":[{"type":{"b":[true,null,"q\"\u001f"],"1":2,"0":-0}}]}}"#,
            Unsupported,
            r#"format v2 tool/code-dispatch at seq 5 data.content[0]: cannot safely transform unclassified message content kind {"0":0,"1":2,"b":[true,null,"q\"\u001f"]}"#,
        ),
        (
            r#"{"type":"agent/inbox/spliced","seq":10,"time":5,"data":{"target":"next-turn","start":0,"removedCount":1,"inserted":[{"0":1,"id":"u1","role":"user","content":[{"type":"text","text":"hi"}],"source":{"kind":"user","rpcId":"r","clientTimeZone":"z"}}],"outcome":"canceled"}}"#,
            Invalid,
            r#"agent/inbox/spliced 10 inserted message has unexpected member "0""#,
        ),
        (
            r#"{"type":"assistant/attempt","seq":10,"time":5,"data":{"turn":1,"step":1,"stream":[{"type":"chunk","chunk":{"type":"block-start","index":0,"blockType":"text"}},{"type":"chunk","chunk":{"type":"block-end","index":0,"block":{"type":"tool-result","content":[{"type":"text","text":"x"}]}}},{"type":"chunk","chunk":{"type":"text-delta","index":0,"text":"x"}},{"type":"packed","chunk":{"anything":[1,2]}}]}}"#,
            Invalid,
            r#"format v2 assistant/attempt at seq 10 data.stream[1].chunk.block: invalid message content kind "tool-result": SessionFormatError: user/message 0 content[0] lacks required member "toolCallId""#,
        ),
        (
            r#"{"type":"TOOL_NOT_STARTED","seq":10,"time":5,"data":{"agentPreset":"code"}}"#,
            Unsupported,
            r#"format v2 to v3 cannot safely transform unclassified event TOOL_NOT_STARTED"#,
        ),
        (
            r#"{"type":"user/message","seq":10,"time":5,"data":{"id":"u","role":"user","content":[{"type":"file","attachment":{"attachmentId":null,"name":"n","bytes":0}},{"type":"text","text":"t"}],"source":{"kind":"user"}},"surfaceOp":"append","ignorable":true}"#,
            Invalid,
            r#"format v2 user/message at seq 10 data.content[0] kind "file": file attachment requires attachmentId and name"#,
        ),
        (
            r#"{"type":"user/message","seq":10,"time":5,"data":{"id":"u","role":"user","content":[{"type":"text","text":"t"}],"source":{"kind":"agent-message","form":null,"senderSessionId":"s"}},"surfaceOp":"append"}"#,
            Invalid,
            r#"agent-message source requires relay form and senderSessionId"#,
        ),
        (
            r#"{"type":"agent/inbox/spliced","seq":10,"time":5,"data":{"target":"next-turn","start":0,"removedCount":1,"inserted":[{"id":"u1","role":"user","content":[{"type":"text","text":"hi"}],"source":{"kind":"x","rpcId":"r","clientTimeZone":"z"}}],"outcome":"canceled"}}"#,
            Unsupported,
            r#"cannot safely transform unclassified message source"#,
        ),
        (
            r#"{"type":"assistant/attempt","seq":10,"time":5,"data":{"turn":0,"step":1,"stream":[{"type":"chunk","chunk":{"type":"block-start","index":0,"blockType":"text"}},{"type":"chunk","chunk":{"type":"block-end","index":0,"block":{"type":"tool-result","toolCallId":"c","content":[{"type":"text","text":"x"}]}}},{"type":"chunk","chunk":{"type":"text-delta","index":0,"text":"x"}},{"type":"packed","chunk":{"anything":[1,2]}}]}}"#,
            Invalid,
            r#"turn must be positive"#,
        ),
        (
            r#"{"type":"session/end-seed","seq":10,"time":5,"data":{"inherited":null}}"#,
            Invalid,
            r#"session/end-seed inherited must be true"#,
        ),
        (
            r#"{"type":"agent/inbox/spliced","seq":10,"time":5,"data":{"target":"next-turn","start":0,"removedCount":1,"inserted":[{"id":"u1","role":"user","content":[{}],"source":{"kind":"user","rpcId":"r","clientTimeZone":"z"}}],"outcome":"canceled"}}"#,
            Unsupported,
            r#"format v2 agent/inbox/spliced at seq 10 data.inserted[0].content[0]: cannot safely transform unclassified message content kind undefined"#,
        ),
        (
            r#"{"type":"request/header","seq":10,"time":5,"data":{"header":{"config":{"provider":"p","model":"m","temperature":0.5,"maxTokens":10,"stop":["x"]},"adapterDefaults":{"reasoningEffort":true,"maxTokens":true},"system":"s","tools":[{"name":"t","description":"d","parameters":{"type":"object"}}]},"reason":"initial","startsSeries":true}}"#,
            Invalid,
            r#"request/header 10 header adapter default reasoningEffort lacks config value"#,
        ),
        (
            r#"{"type":"llm/retry","seq":10,"time":5,"data":{"retryId":"r","turn":1,"step":1,"provider":"p","mode":"normal","policyKey":"k","retry":2,"maxRetries":3,"delayMs":9007199254740993,"failure":{"message":"m","code":"c","status":500,"providerRetryAfterMs":1.5,"requestId":"r"}}}"#,
            Invalid,
            r#"llm/retry 10 delayMs exceeds the timer range"#,
        ),
        (
            r#"{"type":"feedback/message-put","seq":10,"time":5,"data":{"sessionId":"s","item":{"messageId":"m","rating":"positive","version":"v","createdAt":1,"updatedAt":2,"note":1}}}"#,
            Invalid,
            r#"feedback note must be a string"#,
        ),
        (
            r#"{"0":1,"type":"agent-preset/selected","seq":10,"time":5,"data":{"agentPreset":"code"}}"#,
            Invalid,
            r#"agent-preset/selected has unexpected field 0"#,
        ),
        (
            r#"{"type":"compaction/summary","seq":10,"time":5,"data":{"compactionId":"c","summary":[{"type":"text","text":"s"}],"shadowedRange":{"start":2,"end":4},"shadowedSeqs":[2,3,4],"shadowedTokenCount":5,"provider":"p","model":"m","sourceCommandId":"s","maxTokens":10,"usage":{"inputTokens":1,"outputTokens":2,"totalTokens":3,"cacheReadTokens":0,"cacheWriteTokens":0,"reasoningTokens":1},"llmStreamCall":true}}"#,
            Invalid,
            r#"compaction/summary 10 llmStreamCall requires rawOutput"#,
        ),
        (
            r#"{"type":"agent/inbox/spliced","seq":10,"time":5,"data":{"target":"next-turn","start":0,"removedCount":1,"inserted":null,"outcome":"canceled"}}"#,
            Invalid,
            r#"format v2 agent/inbox/spliced at seq 10 data.inserted: content must be an array"#,
        ),
    ];

    #[test]
    fn source_events_match_recorded_typescript_outcomes() {
        for (row, expect, message) in CASES {
            let event: Value = serde_json::from_str(row).expect("row json");
            let outcome = assert_source_event(&event);
            let matches = match (&outcome, expect) {
                (Result::Ok(()), Ok) => true,
                (Err(StageError::Invalid(got)), Invalid)
                | (Err(StageError::Unsupported(got)), Unsupported) => got == message,
                (Err(StageError::NativeLimit(_)), Limit) => true,
                _ => false,
            };
            assert!(
                matches,
                "{row}\nexpected {expect:?} {message}\ngot {outcome:?}"
            );
        }
    }

    #[test]
    fn integral_float_lexemes_are_native_limits() {
        // JavaScript reads `1.0` as the count 1; this crate does not claim that reading.
        let row = r#"{"type":"turn/start","seq":5,"time":0,"data":{"turn":1.0}}"#;
        let event: Value = serde_json::from_str(row).expect("row json");
        assert!(matches!(
            assert_source_event(&event),
            Err(StageError::NativeLimit(_))
        ));
        // A negative float is refused exactly, before any later check.
        let row = r#"{"type":"turn/start","seq":5,"time":0,"data":{"turn":-1.5}}"#;
        let event: Value = serde_json::from_str(row).expect("row json");
        assert_eq!(
            assert_source_event(&event),
            Err(StageError::Invalid(
                "turn/start 5 turn must be a non-negative safe integer".to_owned()
            ))
        );
    }
}
