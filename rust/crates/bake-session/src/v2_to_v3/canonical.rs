//! `canonicalizeTransformedEvent` and the `assertV3Event` it ends with, from
//! `session-format-v2-to-v3/src/payload.ts`, over target-coordinate events.

use serde_json::{Map, Value};

use super::admission::{Lookup, lookup};
use super::js::{MAX_SAFE_INTEGER, count, exact_keys, record};
use super::{SURFACE_TYPES, StageError};

const LIMIT: &str = "canonical-float-lexeme";
const REQUIRED: [&str; 4] = ["type", "seq", "time", "data"];

/// Convert replace endpoints to `startSeq`/`endSeq`, omit empty request
/// header `tools` and `adapterDefaults`, then validate the V3 event.
pub(super) fn canonicalize(mut event: Map<String, Value>) -> Result<Value, StageError> {
    let event_type = event["type"].as_str().unwrap_or_default().to_owned();
    let seq = event["seq"].as_u64().unwrap_or_default();
    let subject = format!("format v2 {event_type} at seq {seq}");
    if let Some(operation) = event.get("surfaceOp")
        && operation != "append"
    {
        let replace = record(Some(operation), &format!("{subject} surfaceOp"))?;
        if replace.len() != 3
            || replace.get("op").is_none_or(|op| op != "replace")
            || !replace.contains_key("start")
            || !replace.contains_key("end")
        {
            return Err(StageError::Invalid(format!(
                "{subject} requires exact replace fields op/start/end"
            )));
        }
        let start = count(
            replace.get("start"),
            &format!("{subject} replace start"),
            LIMIT,
        )?;
        let end = count(replace.get("end"), &format!("{subject} replace end"), LIMIT)?;
        event.insert("surfaceOp".to_owned(), replace_op(start, end));
    }
    if event_type == "request/header" {
        let data = record(
            event.get("data"),
            &format!("format v2 request/header at seq {seq} data"),
        )?;
        let header = record(
            data.get("header"),
            &format!("format v2 request/header at seq {seq} header"),
        )?;
        let empty = |key: &str, value: &Value| match (key, value) {
            ("tools", Value::Array(items)) => items.is_empty(),
            ("adapterDefaults", Value::Object(fields)) => fields.is_empty(),
            _ => false,
        };
        if header.iter().any(|(key, value)| empty(key, value)) {
            let canonical: Map<String, Value> = header
                .iter()
                .filter(|(key, value)| !empty(key, value))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            let mut data = data.clone();
            data.insert("header".to_owned(), Value::Object(canonical));
            event.insert("data".to_owned(), Value::Object(data));
        }
    }
    assert_v3_event(&event, &[])?;
    Ok(Value::Object(event))
}

/// The canonical V3 replace marker, in TypeScript's member order.
fn replace_op(start: u64, end: u64) -> Value {
    let mut operation = Map::new();
    operation.insert("op".to_owned(), Value::from("replace"));
    operation.insert("startSeq".to_owned(), Value::from(start));
    operation.insert("endSeq".to_owned(), Value::from(end));
    Value::Object(operation)
}

/// `assertV3Event(event, installed)`, where `installed` lists the
/// `knownEventTypes` beyond the audited ones; canonicalization passes none.
/// A type outside both is opaque, so it may carry surface metadata.
///
/// The event must be a JavaScript-ordered object with a string `type` and a
/// `seq` that is a count, so the subject spells it as TypeScript does. Every
/// transformed event passed source admission and the PTC renames, so its
/// type is audited: in the released v2 inventory, a feedback event, a PTC
/// dispatch, or a generated `system/message`.
pub(crate) fn assert_v3_event(
    event: &Map<String, Value>,
    installed: &[&str],
) -> Result<(), StageError> {
    let Some(Value::String(event_type)) = event.get("type") else {
        return Err(StageError::Invalid(
            "format v3 event type must be a string".to_owned(),
        ));
    };
    let seq = event.get("seq").and_then(Value::as_u64).unwrap_or_default();
    let subject = format!("format v3 {event_type} at seq {seq}");
    let surface = SURFACE_TYPES.contains(&event_type.as_str());
    let obsolete = matches!(
        event_type.as_str(),
        "tool/code-dispatch-start" | "tool/code-dispatch"
    );
    let known = !obsolete
        && (surface
            || lookup(event_type) != Lookup::Absent
            || matches!(
                event_type.as_str(),
                "tool/ptc-dispatch-start"
                    | "tool/ptc-dispatch"
                    | "feedback/message-put"
                    | "feedback/message-delete"
            )
            || installed.contains(&event_type.as_str()));
    let optional: &[&str] = if surface || !known {
        &["ignorable", "surfaceOp", "sourceEventSeqs"]
    } else {
        &["ignorable"]
    };
    exact_keys(event, &REQUIRED, optional, &subject)?;
    let seq = count(event.get("seq"), &format!("{subject} seq"), LIMIT)?;
    safe_integer(event.get("time"), &format!("{subject} time"))?;
    if event.get("ignorable").is_some_and(|value| value != true) {
        return Err(StageError::Invalid(format!(
            "{subject} ignorable must be true when present"
        )));
    }
    if surface {
        surface_metadata(event, event_type, seq, &subject)?;
    }
    structural(event, event_type)?;
    canonical_payload(event, event_type, &subject)
}

fn surface_metadata(
    event: &Map<String, Value>,
    event_type: &str,
    seq: u64,
    subject: &str,
) -> Result<(), StageError> {
    let Some(operation) = event.get("surfaceOp") else {
        return Err(StageError::Invalid(format!(
            "{subject} requires a surfaceOp marker"
        )));
    };
    if operation != "append" {
        let replace = record(Some(operation), &format!("{subject} surfaceOp"))?;
        if replace.len() != 3
            || replace.get("op").is_none_or(|op| op != "replace")
            || !replace.contains_key("startSeq")
            || !replace.contains_key("endSeq")
        {
            return Err(StageError::Invalid(format!(
                "{subject} requires exact replace fields op/startSeq/endSeq"
            )));
        }
        for key in ["startSeq", "endSeq"] {
            let endpoint = count(
                replace.get(key),
                &format!("{subject} surfaceOp {key}"),
                LIMIT,
            )?;
            if endpoint >= seq {
                return Err(StageError::Invalid(format!(
                    "{subject} replacement endpoints must reference earlier events"
                )));
            }
        }
    }
    let Some(sources) = event.get("sourceEventSeqs") else {
        return Ok(());
    };
    if event_type == "assistant/message" {
        return Err(StageError::Invalid(format!(
            "{subject} embeds its stream and cannot carry sourceEventSeqs"
        )));
    }
    let sources = match sources {
        Value::Array(sources) if !sources.is_empty() => sources,
        _ => {
            return Err(StageError::Invalid(format!(
                "{subject} sourceEventSeqs must be a non-empty array"
            )));
        }
    };
    let mut seen = std::collections::HashSet::new();
    for source in sources {
        let source = count(
            Some(source),
            &format!("{subject} sourceEventSeqs member"),
            LIMIT,
        )?;
        if source >= seq || !seen.insert(source) {
            return Err(StageError::Invalid(format!(
                "{subject} sourceEventSeqs must be unique earlier seqs"
            )));
        }
    }
    Ok(())
}

/// `assertV3StructuralRow` on a transformed event.
fn structural(event: &Map<String, Value>, event_type: &str) -> Result<(), StageError> {
    match event_type {
        "request/header" => {
            let data = record(event.get("data"), "request/header data")?;
            if record(data.get("header"), "request header")?.contains_key("system") {
                return Err(StageError::Unsupported(
                    "format v3 request/header rejects retired header.system".to_owned(),
                ));
            }
            Ok(())
        }
        "system/message" => system(record(event.get("data"), "system/message data")?),
        _ => Ok(()),
    }
}

/// `assertSystem`. Only the migration emits `system/message`, since source
/// admission refuses it, so the message is the generated one: a non-empty
/// hex identity, the system-prompt plugin source, and no content or one text
/// block holding a string. The frozen user-message semantics it ends with
/// accept that shape, so only the copied step coordinates can fail.
fn system(data: &Map<String, Value>) -> Result<(), StageError> {
    exact_keys(
        data,
        &["turn", "step", "message"],
        &[],
        "system/message data",
    )?;
    for coordinate in ["turn", "step"] {
        if count(data.get(coordinate), coordinate, LIMIT)? == 0 {
            return Err(StageError::Invalid(format!(
                "{coordinate} must be positive"
            )));
        }
    }
    let message = record(data.get("message"), "system message")?;
    exact_keys(
        message,
        &["id", "role", "source", "content"],
        &[],
        "system message",
    )?;
    let identified = message
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty());
    if !identified || message.get("role").is_none_or(|role| role != "system") {
        return Err(StageError::Invalid(
            "system message requires an id and system role".to_owned(),
        ));
    }
    let source = record(message.get("source"), "system source")?;
    let plugin = source
        .get("plugin")
        .and_then(Value::as_str)
        .is_some_and(|plugin| !plugin.is_empty());
    if source.get("kind").is_none_or(|kind| kind != "plugin") || !plugin {
        return Err(StageError::Invalid(
            "system message requires plugin source".to_owned(),
        ));
    }
    Ok(())
}

/// `assertCanonicalPayload`.
fn canonical_payload(
    event: &Map<String, Value>,
    event_type: &str,
    subject: &str,
) -> Result<(), StageError> {
    if event_type == "request/header" {
        let data = record(event.get("data"), &format!("{subject} data"))?;
        let header = record(data.get("header"), &format!("{subject} header"))?;
        let empty_tools =
            matches!(header.get("tools"), Some(Value::Array(items)) if items.is_empty());
        let empty_defaults = matches!(header.get("adapterDefaults"), Some(Value::Object(fields)) if fields.is_empty());
        if empty_tools || empty_defaults {
            return Err(StageError::Invalid(format!(
                "{subject} empty optional header fields must be omitted"
            )));
        }
    }
    if event_type != "tool/result" {
        return Ok(());
    }
    let data = record(event.get("data"), &format!("{subject} data"))?;
    if !data.contains_key("error") {
        return Ok(());
    }
    let message = record(data.get("message"), &format!("{subject} message"))?;
    let error_result = match message.get("content") {
        Some(Value::Array(content)) => match content.as_slice() {
            [Value::Object(block)] => {
                block.get("type").is_some_and(|kind| kind == "tool-result")
                    && block.get("isError").is_some_and(|flag| flag == true)
            }
            _ => false,
        },
        _ => false,
    };
    if !error_result {
        return Err(StageError::Invalid(format!(
            "{subject} carries error metadata for a non-error tool result"
        )));
    }
    Ok(())
}

/// `sessionFormatSafeInteger`, with -0 refused and other `f64` values deferred.
pub(crate) fn safe_integer(value: Option<&Value>, label: &str) -> Result<i64, StageError> {
    let invalid = || StageError::Invalid(format!("{label} must be a safe integer"));
    let Some(Value::Number(number)) = value else {
        return Err(invalid());
    };
    if let Some(number) = number.as_i64() {
        return if number.unsigned_abs() <= MAX_SAFE_INTEGER {
            Ok(number)
        } else {
            Err(invalid())
        };
    }
    if number.is_u64()
        || number
            .as_f64()
            .is_some_and(|number| number == 0.0 && number.is_sign_negative())
    {
        return Err(invalid());
    }
    Err(StageError::NativeLimit(LIMIT.to_owned()))
}
