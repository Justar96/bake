//! Development-only reproduction of `interruptedTurnClosers` in
//! `packages/core/session/src/repair.ts`: the synthetic events that close a
//! log whose writer stopped mid-turn.
//!
//! The scan is pure. Pending tool calls form an insertion-ordered map keyed by
//! call id: an Assistant message's tool-call blocks set them, setting an
//! existing id keeps its position and forgets its `tool/call` seq, a
//! `tool/call` records its seq for a pending id, a `tool/result` deletes its
//! source's id, and `turn/start`, `step/end`, and `turn/end` clear them all.
//! With a turn open, the closers are an error `tool/result` per pending call,
//! in map order, then `step/end` when a step is open, then
//! `turn/end {interrupted}`; their seqs continue the log and each reuses the
//! last event's time. Each result cites its `tool/call` when one was recorded.
//!
//! The scan reads payloads no check has validated, so JavaScript can coerce
//! or throw where JSON gives no answer. Such input is a
//! [`RestoreLimit`] instead of a guess: `null` data where a field is read, a
//! `null` content block, a non-string pending call id, and an open turn or
//! step whose value is not a safe count. Adoption already proved each
//! Assistant message's `message.content` an array and each tool result's
//! `source.callId` a non-empty string. An Assistant step needs no check here:
//! Session construction refuses a step that is not a safe count, or limits a
//! spelled one, at the message row. That check runs after this scan, so a
//! result closer copies the step as logged, at any depth, and the closers are
//! held so that dropping them never recurses.

use serde_json::{Map, Value, json};

use crate::json_parse::{Deep, clone_value};
use crate::{MAX_SAFE_INTEGER, RestoreLimit, UnadmittedEnvelope};

/// `TOOL_NOT_STARTED`'s result text in `repair.ts`.
const NOT_STARTED_TEXT: &str = "The tool call was interrupted before the Harness recorded it as started. Retry it if it is still needed.";
/// `TOOL_OUTCOME_UNKNOWN`'s result text in `repair.ts`.
const OUTCOME_UNKNOWN_TEXT: &str = "The tool call was interrupted after it was recorded, but no result was durably recorded. Its outcome is unknown. Decide whether to retry from the tool semantics: retry only if the operation is read-only or idempotent; if it may have side effects, first verify external state or ask the user. Do not retry blindly.";

/// A boundary value read from `data.turn` or `data.step`, and its row's seq.
/// `None` is JavaScript's `undefined`.
type Open<'a> = Option<(u64, Option<&'a Value>)>;

/// One pending call: its id, the Assistant row and step that set it, and the
/// seq of its recorded `tool/call`.
struct Pending<'a> {
    id: &'a Value,
    assistant_seq: u64,
    step: Option<&'a Value>,
    call_seq: Option<u64>,
}

/// The closers for `events`, a decoded log in seq order, or the limit whose
/// row the scan cannot follow without guessing JavaScript's coercion.
pub(crate) fn interrupted_turn_closers(
    events: &[UnadmittedEnvelope<'_>],
) -> Result<Deep<Vec<Value>>, (u64, RestoreLimit)> {
    let mut open_turn: Open<'_> = None;
    let mut open_step: Open<'_> = None;
    let mut pending: Vec<Pending<'_>> = Vec::new();
    for event in events {
        let seq = event.seq;
        let data = event.data;
        match event.event_type {
            "turn/start" => {
                open_turn = boundary(seq, data, "turn")?;
                open_step = None;
                pending.clear();
            }
            "turn/end" => {
                open_turn = None;
                open_step = None;
                pending.clear();
            }
            "step/start" => open_step = boundary(seq, data, "step")?,
            "step/end" => {
                pending.clear();
                open_step = None;
            }
            "assistant/message" => {
                let blocks = data["message"]["content"]
                    .as_array()
                    .expect("adoption proved message content an array");
                for block in blocks {
                    if block.is_null() {
                        return Err((seq, RestoreLimit::Repair));
                    }
                    if block["type"] != "tool-call" {
                        continue;
                    }
                    let id = block.get("id").unwrap_or(&Value::Null);
                    let call = Pending {
                        id,
                        assistant_seq: seq,
                        step: data.get("step"),
                        call_seq: None,
                    };
                    // A non-string id never matches a `tool/call` or
                    // `tool/result` this scan resolves; it is limited if it
                    // is still pending at the end.
                    match pending
                        .iter_mut()
                        .find(|entry| entry.id.is_string() && entry.id == id)
                    {
                        Some(entry) => *entry = call,
                        None => pending.push(call),
                    }
                }
            }
            "tool/call" => {
                if data.is_null() {
                    return Err((seq, RestoreLimit::Repair));
                }
                if let Some(Value::String(id)) = data.get("callId")
                    && let Some(entry) = pending.iter_mut().find(|entry| entry.id == id)
                {
                    entry.call_seq = Some(seq);
                }
            }
            "tool/result" => {
                let id = &data["message"]["source"]["callId"];
                pending.retain(|entry| entry.id != id);
            }
            _ => {}
        }
    }
    let (Some((turn_seq, turn)), Some(last)) = (open_turn, events.last()) else {
        return Ok(Deep::default());
    };
    let turn = safe_count(turn).ok_or((turn_seq, RestoreLimit::Coordinate))?;
    let mut seq = last.seq + 1;
    let time = last.time;
    let mut closers = Deep::<Vec<Value>>::default();
    for call in pending {
        let Value::String(id) = call.id else {
            return Err((call.assistant_seq, RestoreLimit::Repair));
        };
        closers.push(result_closer(seq, time, turn, call.step, id, call.call_seq));
        seq += 1;
    }
    if let Some((step_seq, step)) = open_step {
        let step = safe_count(step).ok_or((step_seq, RestoreLimit::Coordinate))?;
        closers.push(json!({
            "type": "step/end", "seq": seq, "time": time, "data": {"turn": turn, "step": step},
        }));
        seq += 1;
    }
    closers.push(json!({
        "type": "turn/end", "seq": seq, "time": time,
        "data": {"turn": turn, "reason": {"kind": "interrupted"}},
    }));
    Ok(closers)
}

/// `data[key]` for a boundary: `null` closes it, and `null` data throws in
/// JavaScript.
fn boundary<'a>(seq: u64, data: &'a Value, key: &str) -> Result<Open<'a>, (u64, RestoreLimit)> {
    if data.is_null() {
        return Err((seq, RestoreLimit::Repair));
    }
    Ok(match data.get(key) {
        Some(Value::Null) => None,
        value => Some((seq, value)),
    })
}

fn safe_count(value: Option<&Value>) -> Option<u64> {
    value
        .and_then(Value::as_u64)
        .filter(|count| *count <= MAX_SAFE_INTEGER)
}

/// The error result for one pending call. Its step is the Assistant row's
/// value, copied before Session construction refuses one that is not a safe
/// count, so it may nest as deep as its row.
fn result_closer(
    seq: u64,
    time: i64,
    turn: u64,
    step: Option<&Value>,
    id: &str,
    call_seq: Option<u64>,
) -> Value {
    let (text, name, code) = match call_seq {
        Some(_) => (
            OUTCOME_UNKNOWN_TEXT,
            "ToolOutcomeUnknownError",
            "TOOL_OUTCOME_UNKNOWN",
        ),
        None => (NOT_STARTED_TEXT, "ToolNotStartedError", "TOOL_NOT_STARTED"),
    };
    let mut data = Map::new();
    data.insert("turn".to_owned(), turn.into());
    if let Some(step) = step {
        data.insert("step".to_owned(), clone_value(step));
    }
    data.insert(
        "message".to_owned(),
        json!({
            "id": format!("interrupted-tool-result-{id}-{seq}"),
            "role": "user",
            "source": {"kind": "tool", "callId": id},
            "content": [{
                "type": "tool-result",
                "toolCallId": id,
                "isError": true,
                "content": [{"type": "text", "text": text}],
            }],
        }),
    );
    data.insert("error".to_owned(), json!({"name": name, "code": code}));
    // `json!` would serialize `data`, and with it the step, recursively.
    let mut closer = Map::new();
    closer.insert("type".to_owned(), "tool/result".into());
    closer.insert("seq".to_owned(), seq.into());
    closer.insert("time".to_owned(), time.into());
    closer.insert("data".to_owned(), Value::Object(data));
    closer.insert("surfaceOp".to_owned(), "append".into());
    if let Some(call_seq) = call_seq {
        closer.insert("sourceEventSeqs".to_owned(), json!([call_seq]));
    }
    Value::Object(closer)
}
