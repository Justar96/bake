//! `normalizeReleasedV0Event` from
//! `packages/session/session-format-v0-to-v1/src/migration.ts`: the legacy
//! rewrites of one decoded v0 event, in TypeScript's order, then
//! `assertReleasedEventPayload(event, 0)` from `validation.ts` for every
//! event other than `assistant/chunk`. The v1→v2 decoded stage runs the same
//! payload check at version 1.
//!
//! Object rewrites follow JavaScript's spread and delete order: a replaced
//! member keeps its position, an added member is appended, and a removed
//! member closes its gap.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::json_parse::{Deep, clone_fields, clone_value, remove_member, replace_member};

use super::dispositions::{self, Lookup};
use crate::v2_to_v3::{
    Checked, StageError, assert_released_payload_semantics, contains_negative_zero, count, invalid,
    quote, released_keys, released_record, stringify,
};

type Record = Map<String, Value>;
/// An event being rewritten, dropped without recursing when a stage refuses it.
type Event = Deep<Record>;

/// The legacy Assistant message member, `provenance`.
const LEGACY_ASSISTANT_SOURCE_KEY: &str = "provenance";
/// A non-string event `type`, which TypeScript reads through property-key
/// and template-string coercion.
pub(super) const TYPE_COERCION: &str = "type-coercion";
/// A replacement `start` spelled as a float, which TypeScript looks up as a
/// `Map` key by its double value.
const REFERENCE_FLOAT_LEXEME: &str = "reference-float-lexeme";
const OBJECT_PROTOTYPE_TYPE: &str = "object-prototype-type";

/// `LegacyNormalizationState`.
#[derive(Debug, Default)]
pub(super) struct LegacyState {
    /// Message ids of admitted events by seq.
    message_ids: HashMap<u64, String>,
    /// Retry ids by the `JSON.stringify` chain of turn, step, provider, and policy key.
    retry_ids: HashMap<String, String>,
    compaction_id: Option<String>,
}

fn unsupported<T>(message: String) -> Checked<T> {
    Err(StageError::Unsupported(message))
}

fn limit<T>(name: &str) -> Checked<T> {
    Err(StageError::NativeLimit(name.to_owned()))
}

fn data_record<'a>(event: &'a Record, label: &str) -> Checked<&'a Record> {
    released_record(event.get("data"), label)
}

fn event_type(event: &Record) -> &str {
    event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// `event` with `data` in place of its `data` member, which is dismantled.
fn with_data(mut event: Event, data: Record) -> Event {
    replace_member(&mut event, "data", Value::Object(data));
    event
}

fn legacy_message_id(session_id: &str, seq: u64) -> String {
    format!("legacy-message:{session_id}:{seq}")
}

fn malformed_legacy(session_id: &str, event_type: &str, seq: u64) -> StageError {
    StageError::Invalid(format!(
        "session {} contains malformed pre-react-loop {event_type} at seq {seq}",
        quote(session_id)
    ))
}

/// Normalize and admit the decoded event at `seq`, whose `type` is a string.
pub(super) fn normalize_event(
    event: Record,
    seq: u64,
    session_id: &str,
    state: &mut LegacyState,
) -> Checked<Record> {
    let named = rename_compaction_type(Deep::new(event));
    assert_supported_type(&named, seq, session_id)?;
    let start = normalize_turn_start(named, seq, session_id)?;
    let end = normalize_turn_end(start, seq, session_id)?;
    let header = normalize_request_header(end, seq, session_id)?;
    let steering = normalize_steering(header, seq, session_id)?;
    let retry = normalize_retry(steering, seq, session_id, &mut state.retry_ids)?;
    let compaction = normalize_compaction(retry, seq, session_id, state)?;
    let message = normalize_message(compaction, seq, session_id, &state.message_ids)?;
    let current_type = event_type(&message).to_owned();
    if current_type != "assistant/chunk" {
        assert_event_payload(&current_type, seq, message.get("data"), 0)?;
    }
    if let Some(id) = event_message_id(&message, &current_type, seq)? {
        state.message_ids.insert(seq, id);
    }
    Ok(message.into_inner())
}

fn rename_compaction_type(mut event: Event) -> Event {
    let renamed = match event_type(&event) {
        "compact/start" => "compaction/start",
        "compact/summary" => "compaction/summary",
        "compact/end" => "compaction/end",
        "compact/prune" => "compaction/prune",
        _ => return event,
    };
    event.insert("type".to_owned(), Value::from(renamed));
    event
}

fn assert_supported_type(event: &Record, seq: u64, session_id: &str) -> Checked {
    let current = event_type(event);
    if matches!(current, "request/header-delta" | "mode/set") {
        return unsupported(format!(
            "session {} contains unsupported legacy {current} event at seq {seq}",
            quote(session_id)
        ));
    }
    if current == "request/header" {
        let data = data_record(event, &format!("request/header {seq} data"))?;
        if data.get("reason") == Some(&Value::from("fallback")) {
            return unsupported(format!(
                "session {} contains unsupported request/header reason \"fallback\" at seq {seq}",
                quote(session_id)
            ));
        }
    }
    Ok(())
}

fn normalize_turn_start(event: Event, seq: u64, session_id: &str) -> Checked<Event> {
    if event_type(&event) != "turn/start" {
        return Ok(event);
    }
    let label = format!("turn/start {seq} data");
    let data = data_record(&event, &label)?;
    if !data.contains_key("trigger") {
        return Ok(event);
    }
    released_keys(data, &["turn", "trigger"], &[], &label)?;
    let turn = count(data.get("turn"), &format!("turn/start {seq} turn"))?;
    let trigger = released_record(data.get("trigger"), &format!("turn/start {seq} trigger"))?;
    let kind_named = trigger
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| !kind.is_empty());
    if turn < 1 || !kind_named {
        return Err(malformed_legacy(session_id, "turn/start", seq));
    }
    let mut current = Record::new();
    current.insert("turn".to_owned(), Value::from(turn));
    Ok(with_data(event, current))
}

fn normalize_turn_end(event: Event, seq: u64, session_id: &str) -> Checked<Event> {
    if event_type(&event) != "turn/end" {
        return Ok(event);
    }
    let label = format!("turn/end {seq} data");
    let data = data_record(&event, &label)?;
    released_keys(data, &["turn", "reason"], &[], &label)?;
    let turn = count(data.get("turn"), &format!("turn/end {seq} turn"))?;
    if turn < 1 {
        return Err(malformed_legacy(session_id, "turn/end", seq));
    }
    let reason_label = format!("turn/end {seq} reason");
    let reason = released_record(data.get("reason"), &reason_label)?;
    let Some(Value::String(kind)) = reason.get("kind") else {
        return Err(malformed_legacy(session_id, "turn/end", seq));
    };
    let kind_only = || released_keys(reason, &["kind"], &[], &reason_label);
    let current = match kind.as_str() {
        "completed" | "blocked" | "max-tokens" | "interrupted" => {
            kind_only()?;
            return Ok(event);
        }
        "aborted" => {
            if reason.contains_key("reason") {
                return Ok(event);
            }
            kind_only()?;
            abort_reason("legacy")
        }
        "disposed" => {
            kind_only()?;
            abort_reason("disposed")
        }
        "error" => {
            if reason.contains_key("error") {
                return Ok(event);
            }
            legacy_error_reason(reason, seq, session_id)?
        }
        _ => return Ok(event),
    };
    let mut data = Deep::new(clone_fields(data));
    replace_member(&mut data, "reason", Value::Object(current));
    Ok(with_data(event, data.into_inner()))
}

fn abort_reason(cause: &str) -> Record {
    let mut inner = Record::new();
    inner.insert("kind".to_owned(), Value::from(cause));
    let mut reason = Record::new();
    reason.insert("kind".to_owned(), Value::from("aborted"));
    reason.insert("reason".to_owned(), Value::Object(inner));
    reason
}

/// `normalizeLegacyErrorReason`.
fn legacy_error_reason(reason: &Record, seq: u64, session_id: &str) -> Checked<Record> {
    count(reason.get("step"), &format!("turn/end {seq} error step"))?;
    let reason_label = format!("turn/end {seq} reason");
    let mut current = Record::new();
    current.insert("kind".to_owned(), Value::from("error"));
    if let Some(failure) = reason.get("failure") {
        released_keys(reason, &["kind", "step", "failure"], &[], &reason_label)?;
        let failure_label = format!("turn/end {seq} failure");
        let record = released_record(Some(failure), &failure_label)?;
        released_keys(
            record,
            &["message", "code"],
            &["status", "providerRetryAfterMs", "requestId"],
            &failure_label,
        )?;
        if !record.get("message").is_some_and(Value::is_string)
            || !record.get("code").is_some_and(Value::is_string)
        {
            return Err(malformed_legacy(session_id, "turn/end", seq));
        }
        current.insert("error".to_owned(), clone_value(failure));
        return Ok(current);
    }
    released_keys(
        reason,
        &["kind", "step", "message"],
        &["code"],
        &reason_label,
    )?;
    let Some(Value::String(message)) = reason.get("message") else {
        return Err(malformed_legacy(session_id, "turn/end", seq));
    };
    let code = match reason.get("code") {
        None => "UNKNOWN",
        Some(Value::String(code)) => code,
        Some(_) => return Err(malformed_legacy(session_id, "turn/end", seq)),
    };
    let mut error = Record::new();
    error.insert("message".to_owned(), Value::from(message.as_str()));
    error.insert("code".to_owned(), Value::from(code));
    current.insert("error".to_owned(), Value::Object(error));
    Ok(current)
}

fn normalize_request_header(event: Event, seq: u64, session_id: &str) -> Checked<Event> {
    if event_type(&event) != "request/header" {
        return Ok(event);
    }
    let data = data_record(&event, &format!("request/header {seq} data"))?;
    let header = released_record(data.get("header"), &format!("request/header {seq} header"))?;
    let Some(prefix) = header.get("messagePrefix") else {
        return Ok(event);
    };
    if !prefix.is_array() {
        return Err(StageError::Invalid(format!(
            "session {} contains malformed request/header messagePrefix at seq {seq}",
            quote(session_id)
        )));
    }
    let mut current = clone_fields(header);
    remove_member(&mut current, "messagePrefix");
    let mut data = Deep::new(clone_fields(data));
    replace_member(&mut data, "header", Value::Object(current));
    Ok(with_data(event, data.into_inner()))
}

fn normalize_steering(mut event: Event, seq: u64, session_id: &str) -> Checked<Event> {
    if event_type(&event) != "steering/message" {
        return Ok(event);
    }
    let label = format!("steering/message {seq} data");
    let data = data_record(&event, &label)?;
    let turn_label = format!("steering/message {seq} turn");
    if let Some(wrapped) = data.get("message") {
        released_keys(data, &["turn", "message"], &[], &label)?;
        count(data.get("turn"), &turn_label)?;
        let wrapped = clone_value(wrapped);
        event.insert("type".to_owned(), Value::from("user/message"));
        replace_member(&mut event, "data", wrapped);
        return Ok(event);
    }
    released_keys(data, &["turn", "content", "source"], &[], &label)?;
    count(data.get("turn"), &turn_label)?;
    let mut message = clone_fields(data);
    remove_member(&mut message, "turn");
    replace_member(
        &mut message,
        "id",
        Value::from(legacy_message_id(session_id, seq)),
    );
    replace_member(&mut message, "role", Value::from("user"));
    event.insert("type".to_owned(), Value::from("user/message"));
    Ok(with_data(event, message))
}

fn normalize_retry(
    event: Event,
    seq: u64,
    session_id: &str,
    retry_ids: &mut HashMap<String, String>,
) -> Checked<Event> {
    if event_type(&event) != "llm/retry" {
        return Ok(event);
    }
    let data = data_record(&event, &format!("llm/retry {seq} data"))?;
    let chain = retry_chain(data);
    match data.get("retryId") {
        Some(Value::String(id)) if !id.is_empty() => {
            if let Some(chain) = chain {
                retry_ids.insert(chain, id.clone());
            }
            return Ok(event);
        }
        Some(_) => return Ok(event),
        None => {}
    }
    let reused = chain
        .as_ref()
        .and_then(|chain| retry_ids.get(chain))
        .cloned();
    let id = reused.unwrap_or_else(|| format!("legacy-retry:{session_id}:{seq}"));
    if let Some(chain) = chain {
        retry_ids.insert(chain, id.clone());
    }
    let mut data = Deep::new(clone_fields(data));
    replace_member(&mut data, "retryId", Value::from(id));
    Ok(with_data(event, data.into_inner()))
}

/// The `\0`-joined `JSON.stringify` of turn, step, provider, and policy key,
/// where `join` renders an absent member as the empty string. `None` when a
/// member holds a number JavaScript may print differently: turn and step
/// must then fail their count check and provider and policy key their string
/// check in this event's payload validation, so the key is never consulted.
fn retry_chain(data: &Record) -> Option<String> {
    let mut parts = Vec::new();
    for key in ["turn", "step", "provider", "policyKey"] {
        let part = match data.get(key) {
            None => String::new(),
            Some(value) => stringify(Some(value)).ok()?,
        };
        parts.push(part);
    }
    Some(parts.join("\0"))
}

fn normalize_compaction(
    event: Event,
    seq: u64,
    session_id: &str,
    state: &mut LegacyState,
) -> Checked<Event> {
    let current = event_type(&event).to_owned();
    if current == "session/end-seed" {
        state.compaction_id = None;
        return Ok(event);
    }
    if current == "compaction/start" {
        let data = data_record(&event, &format!("compaction/start {seq} data"))?;
        match data.get("compactionId") {
            Some(Value::String(id)) if !id.is_empty() => {
                state.compaction_id = Some(id.clone());
                return Ok(event);
            }
            Some(_) => return Ok(event),
            None => {}
        }
        let id = format!("legacy-compaction:{session_id}:{seq}");
        state.compaction_id = Some(id.clone());
        let mut data = Deep::new(clone_fields(data));
        replace_member(&mut data, "compactionId", Value::from(id));
        return Ok(with_data(event, data.into_inner()));
    }
    let Some(compaction_id) = state.compaction_id.clone() else {
        return Ok(event);
    };
    if current == "compaction/summary" || current == "compaction/end" {
        let data = data_record(&event, &format!("{current} {seq} data"))?;
        let normalized = if data.contains_key("compactionId") {
            event
        } else {
            let mut data = Deep::new(clone_fields(data));
            replace_member(&mut data, "compactionId", Value::from(compaction_id));
            with_data(event, data.into_inner())
        };
        if current == "compaction/end" {
            state.compaction_id = None;
        }
        return Ok(normalized);
    }
    if current != "user/message" {
        return Ok(event);
    }
    let data = data_record(&event, &format!("user/message {seq} data"))?;
    let Some(Value::Object(source)) = data.get("source") else {
        return Ok(event);
    };
    if source.get("kind") != Some(&Value::from("plugin"))
        || source.get("plugin") != Some(&Value::from("compact"))
        || source.contains_key("compactionId")
    {
        return Ok(event);
    }
    let mut source = clone_fields(source);
    replace_member(&mut source, "compactionId", Value::from(compaction_id));
    let mut data = Deep::new(clone_fields(data));
    replace_member(&mut data, "source", Value::Object(source));
    Ok(with_data(event, data.into_inner()))
}

fn normalize_message(
    event: Event,
    seq: u64,
    session_id: &str,
    message_ids: &HashMap<u64, String>,
) -> Checked<Event> {
    let current = event_type(&event).to_owned();
    let data = data_record(&event, &format!("{current} {seq} data"))?;
    match current.as_str() {
        "user/message" => {
            if ["id", "role", "message"]
                .iter()
                .any(|key| data.contains_key(*key))
                || !data.contains_key("content")
                || !data.contains_key("source")
            {
                return Ok(event);
            }
            let mut data = Deep::new(clone_fields(data));
            replace_member(
                &mut data,
                "id",
                Value::from(legacy_message_id(session_id, seq)),
            );
            replace_member(&mut data, "role", Value::from("user"));
            Ok(with_data(event, data.into_inner()))
        }
        "assistant/message" => {
            if data.contains_key("message")
                || !data.contains_key("content")
                || !data.contains_key(LEGACY_ASSISTANT_SOURCE_KEY)
            {
                return Ok(event);
            }
            let mut data = Deep::new(clone_fields(data));
            // `content` stays in `Deep` so a refused legacy source drops it
            // without recursing.
            let content = Deep::new(data.shift_remove("content").unwrap_or(Value::Null));
            let mut source = clone_fields(released_record(
                data.get(LEGACY_ASSISTANT_SOURCE_KEY),
                &format!("assistant/message {seq} legacy source"),
            )?);
            remove_member(&mut data, LEGACY_ASSISTANT_SOURCE_KEY);
            replace_member(&mut source, "kind", Value::from("model"));
            let mut message = Record::new();
            message.insert(
                "id".to_owned(),
                Value::from(legacy_message_id(session_id, seq)),
            );
            message.insert("role".to_owned(), Value::from("assistant"));
            message.insert("content".to_owned(), content.into_inner());
            message.insert("source".to_owned(), Value::Object(source));
            replace_member(&mut data, "message", Value::Object(message));
            Ok(with_data(event, data.into_inner()))
        }
        "tool/result" => {
            if data.contains_key("message")
                || !["callId", "content", "isError"]
                    .iter()
                    .all(|key| data.contains_key(*key))
            {
                return Ok(event);
            }
            let (Some(Value::String(call_id)), Some(Value::Bool(is_error)), Some(content)) =
                (data.get("callId"), data.get("isError"), data.get("content"))
            else {
                return Ok(event);
            };
            let message_id = match replacement_start(&event)? {
                None => legacy_message_id(session_id, seq),
                Some(start) => match start.and_then(|start| message_ids.get(&start)) {
                    Some(id) => id.clone(),
                    None => {
                        return invalid(format!(
                            "tool/result {seq} replacement cites a message without identity"
                        ));
                    }
                },
            };
            let mut block = Record::new();
            block.insert("type".to_owned(), Value::from("tool-result"));
            block.insert("toolCallId".to_owned(), Value::from(call_id.as_str()));
            block.insert("content".to_owned(), clone_value(content));
            block.insert("isError".to_owned(), Value::Bool(*is_error));
            let mut source = Record::new();
            source.insert("kind".to_owned(), Value::from("tool"));
            source.insert("callId".to_owned(), Value::from(call_id.as_str()));
            let mut message = Record::new();
            message.insert("id".to_owned(), Value::from(message_id));
            message.insert("role".to_owned(), Value::from("user"));
            message.insert(
                "content".to_owned(),
                Value::Array(vec![Value::Object(block)]),
            );
            message.insert("source".to_owned(), Value::Object(source));
            let mut rest = clone_fields(data);
            for key in ["callId", "content", "isError"] {
                remove_member(&mut rest, key);
            }
            replace_member(&mut rest, "message", Value::Object(message));
            Ok(with_data(event, rest))
        }
        _ => Ok(event),
    }
}

/// `replacementStart`: `None` without a replacing `surfaceOp` or without its
/// `start`, which TypeScript reads as `undefined`; otherwise the `Map` key
/// `start` names, `Some(None)` when no seq can equal it. Nothing has
/// validated the envelope on this edge.
fn replacement_start(event: &Record) -> Checked<Option<Option<u64>>> {
    let Some(Value::Object(operation)) = event.get("surfaceOp") else {
        return Ok(None);
    };
    if operation.get("op") != Some(&Value::from("replace")) {
        return Ok(None);
    }
    match operation.get("start") {
        None => Ok(None),
        Some(Value::Number(start)) if start.is_f64() => limit(REFERENCE_FLOAT_LEXEME),
        Some(start) => Ok(Some(start.as_u64())),
    }
}

/// Whether `RELEASED_V0_EVENT_DISPOSITIONS[event_type]` is defined: an own
/// released type or an inherited `Object.prototype` name.
pub(crate) fn has_released_v0_disposition(event_type: &str) -> bool {
    !matches!(dispositions::lookup(event_type), Lookup::Absent)
}

/// `assertReleasedEventPayload(event, version)` at payload generation
/// `version`, 0 or 1. Version 1 counts a descriptor `version` other than 3
/// and then admits the payload unchecked, and admits a delivery marker's
/// `sessionFormatVersion` member.
pub(crate) fn assert_event_payload(
    event_type: &str,
    seq: u64,
    data: Option<&Value>,
    version: u8,
) -> Checked {
    let label = format!("{event_type} {seq} data");
    let disposition = match dispositions::lookup(event_type) {
        Lookup::Own(disposition) => disposition,
        Lookup::Inherited => {
            released_record(data, &label)?;
            // TypeScript spreads the inherited member's absent `required`
            // list and throws an engine `TypeError`.
            return limit(OBJECT_PROTOTYPE_TYPE);
        }
        Lookup::Absent => {
            return unsupported(format!(
                "format v0 contains unknown historical event type {} at seq {seq}; migration refuses unknown historical events even when ignorable",
                quote(event_type)
            ));
        }
    };
    let record = released_record(data, &label)?;
    if event_type == "subagent/descriptor" && !is_three(record.get("version"))? {
        let descriptor_version = count(
            record.get("version"),
            &format!("{event_type} {seq} version"),
        )?;
        if version == 0 {
            return unsupported(format!(
                "{event_type} {seq} uses unsupported descriptor version {descriptor_version}"
            ));
        }
        return Ok(());
    }
    let mut optional = disposition.optional.to_vec();
    if version == 1 && event_type == "session-log-deepseek/delivery-accepted" {
        optional.push("sessionFormatVersion");
    }
    released_keys(record, disposition.required, &optional, &label)?;
    for key in disposition.opaque {
        if record.get(*key).is_some_and(contains_negative_zero) {
            return invalid(format!(
                "{event_type} {seq} opaque {key} is not lossless JSON"
            ));
        }
    }
    assert_released_payload_semantics(event_type, seq, data, version)
}

/// `value === 3`, false for an `f64` a writer spells, which is never
/// integral below 2^63, and undecided for another non-negative `f64`.
fn is_three(value: Option<&Value>) -> Checked<bool> {
    let Some(Value::Number(number)) = value else {
        return Ok(false);
    };
    if let Some(number) = number.as_u64() {
        return Ok(number == 3);
    }
    if number.is_i64()
        || number.as_f64().is_some_and(f64::is_sign_negative)
        || crate::json_text::is_writer_spelling(number)
    {
        return Ok(false);
    }
    limit("payload-float-lexeme")
}

/// `eventMessageId`: a user message's own id, or another event's
/// `message.id`, when it is a string.
fn event_message_id(event: &Record, event_type: &str, seq: u64) -> Checked<Option<String>> {
    let data = data_record(event, &format!("{event_type} {seq} data"))?;
    let message = if event_type == "user/message" {
        Some(data)
    } else {
        data.get("message").and_then(Value::as_object)
    };
    Ok(message
        .and_then(|message| message.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned))
}
