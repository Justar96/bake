//! Development-only strict V3 codec decode of one already parsed event row.
//!
//! [`decode_v3_row`] reproduces one strict
//! `releasedV3SessionFormatCodec.createDecoder(header, 'strict').decodeRow(row)`
//! call from `packages/session/session-format-v2-to-v3/src/codec.ts`, made
//! after the decoder admitted rows 0 through `expected_seq - 1`. It runs the
//! codec's raw-row admission, then [`decode_row_envelope`] unchanged, then the
//! codec's per-event checks.
//!
//! A decoded row is codec output, not a restored event. The codec admits
//! unknown types with opaque payloads; requiring an installed vocabulary,
//! checking relationships between events, and validating most payloads belong
//! to restoration. Recovery modes, the seeded and end-seed checks of `finish`,
//! framing, and replay are outside this decoder.

use serde_json::{Map, Value};

use crate::envelope::{
    EnvelopeLimit, EnvelopeRefusal, EnvelopeRejection, UnadmittedEnvelope, decode_row_envelope,
};
use crate::{Count, MAX_SAFE_INTEGER, count};

/// `SURFACE_TYPES` in `session-format-v2-to-v3/src/payload.ts`.
const SURFACE_TYPES: [&str; 4] = [
    "system/message",
    "user/message",
    "assistant/message",
    "tool/result",
];
/// Dispatch types V3 never knows. A row that is not ignorable is unsupported;
/// an ignorable one decodes as opaque.
const OBSOLETE_TYPES: [&str; 2] = ["tool/code-dispatch-start", "tool/code-dispatch"];
/// `Object.keys(RELEASED_V2_EVENT_DISPOSITIONS)`, obsolete types included.
const DISPOSITION_TYPES: [&str; 51] = [
    "agent-preset/selected",
    "agent/inbox/spliced",
    "approval/asked",
    "approval/decided",
    "approval/policy",
    "command/done",
    "command/run",
    "compaction/end",
    "compaction/prune",
    "compaction/start",
    "compaction/summary",
    "feedback/record",
    "goal/change",
    "hook/invoked",
    "hook/result",
    "llm/retry",
    "llm/retry-started",
    "model/selection",
    "permission/preset",
    "plan/mode",
    "request/context",
    "request/header",
    "sandbox/mode",
    "schedule/change",
    "session/title",
    "session/title-llm-request",
    "step/end",
    "step/start",
    "subagent/descriptor",
    "subagent/model-selection-policy",
    "team/member",
    "team/message/delivered",
    "team/message/queued",
    "team/task",
    "todo/write",
    "tool-workflow/agent-end",
    "tool-workflow/agent-start",
    "tool-workflow/run-end",
    "tool-workflow/run-start",
    "tool/call",
    "tool/code-dispatch",
    "tool/code-dispatch-start",
    "tool/result",
    "turn/end",
    "turn/start",
    "user/message",
    "web/deepseek-search-llm-request",
    "assistant/attempt",
    "assistant/message",
    "session-log-deepseek/delivery-accepted",
    "session/end-seed",
];
/// Types `assertV3Event` knows outside the dispositions.
const NATIVE_TYPES: [&str; 4] = [
    "tool/ptc-dispatch-start",
    "tool/ptc-dispatch",
    "feedback/message-put",
    "feedback/message-delete",
];
/// `Object.getOwnPropertyNames(Object.prototype)`. `assertV3Event` looks a
/// type up in the frozen dispositions object literal, which inherits these
/// names, so they classify as known.
const OBJECT_PROTOTYPE_NAMES: [&str; 12] = [
    "constructor",
    "__defineGetter__",
    "__defineSetter__",
    "hasOwnProperty",
    "__lookupGetter__",
    "__lookupSetter__",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toString",
    "valueOf",
    "__proto__",
    "toLocaleString",
];
const SYSTEM_DATA_KEYS: [&str; 3] = ["turn", "step", "message"];
const SYSTEM_MESSAGE_KEYS: [&str; 4] = ["id", "role", "source", "content"];

/// How `assertV3Event` classifies a type before checking its envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Vocabulary {
    Surface,
    /// Known and not surface: only `ignorable` may join the required fields.
    Known,
    /// Unknown or obsolete: any optional field, opaque payload.
    Opaque,
}

fn vocabulary(event_type: &str) -> Vocabulary {
    if SURFACE_TYPES.contains(&event_type) {
        Vocabulary::Surface
    } else if OBSOLETE_TYPES.contains(&event_type) {
        Vocabulary::Opaque
    } else if DISPOSITION_TYPES.contains(&event_type)
        || NATIVE_TYPES.contains(&event_type)
        || OBJECT_PROTOTYPE_NAMES.contains(&event_type)
    {
        Vocabulary::Known
    } else {
        Vocabulary::Opaque
    }
}

/// A row the strict V3 codec emits. It is neither restored nor checked against
/// an installed vocabulary. Most payloads remain unvalidated: only the
/// `system/message` checks and the partial `request/header` and `tool/result`
/// rules of [`decode_v3_row`] apply, as `conformance/README.md#v3-row-cases`
/// lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3CodecEvent<'a> {
    envelope: UnadmittedEnvelope<'a>,
}

impl<'a> V3CodecEvent<'a> {
    /// The decoded envelope, borrowing the caller's row.
    pub const fn envelope(&self) -> &UnadmittedEnvelope<'a> {
        &self.envelope
    }
}

/// Why the strict V3 codec did not emit a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V3RowRefusal {
    /// The caller's expected seq exceeds 2^53 − 1. This is a caller error,
    /// not a format error.
    ExpectedSeqOutOfRange,
    /// TypeScript throws `SessionFormatError`; see [`V3Rejection::message`].
    Rejected(V3Rejection),
    /// TypeScript throws `SessionFormatUnsupportedMigrationError`.
    Unsupported(V3Unsupported),
    /// This crate cannot reproduce the TypeScript outcome; nothing is claimed.
    NativeSubset(V3Limit),
}

/// A `SessionFormatError`, by the codec step that raised it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V3Rejection {
    /// Raw-row admission, before the envelope is decoded.
    Structural(StructuralRejection),
    /// The released v2 envelope decoder.
    Envelope(EnvelopeRejection),
    /// A check on the decoded event, whose type names the diagnostic subject.
    Event {
        event_type: String,
        rejection: EventRejection,
    },
}

impl V3Rejection {
    /// TypeScript's exact message, or `None` where this crate claims only the
    /// class. Strict, contiguous decoding makes the row index, the expected
    /// seq, and an accepted event's seq the same number, `expected_seq`.
    pub fn message(&self, expected_seq: u64) -> Option<String> {
        match self {
            Self::Structural(rejection) => rejection.message(),
            Self::Envelope(rejection) => rejection.message(expected_seq),
            Self::Event {
                event_type,
                rejection,
            } => rejection.message(&format!("format v3 {event_type} at seq {expected_seq}")),
        }
    }
}

/// A record inside a `system/message` row's data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemRecord {
    Data,
    Message,
}

impl SystemRecord {
    const fn label(self) -> &'static str {
        match self {
            Self::Data => "system/message data",
            Self::Message => "system message",
        }
    }
}

/// A `system/message` coordinate that must be a positive count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coordinate {
    Turn,
    Step,
}

impl Coordinate {
    const fn key(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::Step => "step",
        }
    }
}

/// Raw-row admission errors of `request/header` and `system/message` rows, in
/// check order. None depends on the row's seq.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructuralRejection {
    /// A `request/header` row's data is missing or not an object.
    HeaderDataNotObject,
    HeaderNotObject,
    /// A `system/message` row's data is missing or not an object.
    SystemDataNotObject,
    /// The first required key the record omits.
    MissingField {
        record: SystemRecord,
        key: &'static str,
    },
    /// Keys outside the required ones, in byte order; the message is exact
    /// only for one key, as for [`EnvelopeRejection::UnexpectedFields`].
    UnexpectedFields {
        record: SystemRecord,
        keys: Vec<String>,
    },
    InvalidCoordinate(Coordinate),
    CoordinateNotPositive(Coordinate),
    SystemMessageNotObject,
    /// The id is not a non-empty string, or the role is not `system`.
    SystemIdentity,
    SystemSourceNotObject,
    /// The source kind is not `plugin`, or its plugin is not a non-empty string.
    SystemSource,
}

impl StructuralRejection {
    fn message(&self) -> Option<String> {
        Some(match self {
            Self::HeaderDataNotObject => "request/header data must be an object".to_owned(),
            Self::HeaderNotObject => "request header must be an object".to_owned(),
            Self::SystemDataNotObject => "system/message data must be an object".to_owned(),
            Self::MissingField { record, key } => {
                format!("{} lacks required field {key}", record.label())
            }
            Self::UnexpectedFields { record, keys } => match keys.as_slice() {
                [key] => format!("{} has unexpected field {key}", record.label()),
                _ => return None,
            },
            Self::InvalidCoordinate(coordinate) => {
                format!("{} must be a non-negative safe integer", coordinate.key())
            }
            Self::CoordinateNotPositive(coordinate) => {
                format!("{} must be positive", coordinate.key())
            }
            Self::SystemMessageNotObject => "system message must be an object".to_owned(),
            Self::SystemIdentity => "system message requires an id and system role".to_owned(),
            Self::SystemSourceNotObject => "system source must be an object".to_owned(),
            Self::SystemSource => "system message requires plugin source".to_owned(),
        })
    }
}

/// An endpoint of a surface replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    StartSeq,
    EndSeq,
}

impl Endpoint {
    const fn key(self) -> &'static str {
        match self {
            Self::StartSeq => "startSeq",
            Self::EndSeq => "endSeq",
        }
    }
}

/// Errors of the checks on the decoded event, in check order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventRejection {
    /// A known type other than a surface carries `sourceEventSeqs` and/or
    /// `surfaceOp`, in byte order; the message is exact only for one key.
    UnexpectedFields {
        keys: Vec<String>,
    },
    MissingSurfaceOp,
    /// `surfaceOp` is neither `"append"` nor an object, including JSON `null`.
    SurfaceOpNotObject,
    /// A replacement is not exactly `op: "replace"`, `startSeq`, and `endSeq`.
    InexactReplace,
    InvalidEndpoint(Endpoint),
    /// An endpoint is not earlier than the row. `startSeq > endSeq` is accepted.
    LaterEndpoint,
    AssistantSources,
    EmptySources,
    /// A `request/header` carries `tools: []` or `adapterDefaults: {}`.
    EmptyHeaderOptional,
    ToolResultDataNotObject,
    /// A `tool/result` has an `error` member, JSON `null` included, and its
    /// message is not an object.
    ToolResultMessageNotObject,
    /// A `tool/result` with `error` lacks exactly one `tool-result` block
    /// whose `isError` is `true`.
    NonErrorToolResult,
}

impl EventRejection {
    fn message(&self, subject: &str) -> Option<String> {
        Some(match self {
            Self::UnexpectedFields { keys } => match keys.as_slice() {
                [key] => format!("{subject} has unexpected field {key}"),
                _ => return None,
            },
            Self::MissingSurfaceOp => format!("{subject} requires a surfaceOp marker"),
            Self::SurfaceOpNotObject => format!("{subject} surfaceOp must be an object"),
            Self::InexactReplace => {
                format!("{subject} requires exact replace fields op/startSeq/endSeq")
            }
            Self::InvalidEndpoint(endpoint) => format!(
                "{subject} surfaceOp {} must be a non-negative safe integer",
                endpoint.key()
            ),
            Self::LaterEndpoint => {
                format!("{subject} replacement endpoints must reference earlier events")
            }
            Self::AssistantSources => {
                format!("{subject} embeds its stream and cannot carry sourceEventSeqs")
            }
            Self::EmptySources => format!("{subject} sourceEventSeqs must be a non-empty array"),
            Self::EmptyHeaderOptional => {
                format!("{subject} empty optional header fields must be omitted")
            }
            Self::ToolResultDataNotObject => format!("{subject} data must be an object"),
            Self::ToolResultMessageNotObject => format!("{subject} message must be an object"),
            Self::NonErrorToolResult => {
                format!("{subject} carries error metadata for a non-error tool result")
            }
        })
    }
}

/// A `SessionFormatUnsupportedMigrationError` from raw-row admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V3Unsupported {
    /// A `request/header` row's header has its own `system` member.
    RetiredHeaderSystem,
    /// An obsolete dispatch row that is not ignorable. `seq` is JavaScript's
    /// `String(seq)` of the raw, unvalidated field: `undefined` when missing,
    /// and verbatim safe integers, strings, `null`, and booleans.
    ObsoleteType {
        event_type: &'static str,
        seq: String,
    },
}

impl V3Unsupported {
    /// TypeScript's exact message.
    pub fn message(&self) -> String {
        match self {
            Self::RetiredHeaderSystem => {
                "format v3 request/header rejects retired header.system".to_owned()
            }
            Self::ObsoleteType { event_type, seq } => {
                format!("format v3 contains unknown event type \"{event_type}\" at seq {seq}")
            }
        }
    }
}

/// A count the codec reads whose spelling can decide the outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V3NumberField {
    SystemCoordinate(Coordinate),
    Endpoint(Endpoint),
}

/// Input whose TypeScript outcome this decoder does not reproduce. Each limit
/// fires at the TypeScript check that would read the value, so it may hide a
/// later TypeScript outcome but never an earlier one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V3Limit {
    /// The envelope decoder's own limit, passed through unchanged. Its
    /// `NegativeZeroSeq` stays a limit: V3 refuses a -0 seq, but by a check
    /// that runs after V2 checks this decoder does not complete.
    Envelope(EnvelopeLimit),
    /// An obsolete dispatch row's seq is an array, an object, or a number
    /// serde_json stores as an `f64`, -0 included, or an integer outside the
    /// safe range. JavaScript's `String(seq)` may render it in ways this crate
    /// does not reproduce, or throw a `TypeError`.
    ObsoleteSeqDiagnostic,
    /// serde_json stores the count as an `f64` without a negative sign, such
    /// as `1.0` or `1.5`. Negative spellings, -0 included, are rejected
    /// exactly: an underflow serde_json reads as -0 is a negative number in
    /// JavaScript, so either reading fails the count.
    FloatLexeme(V3NumberField),
    /// A `system/message` row passes the codec's own checks, but its content
    /// or source falls outside the closed shape this crate knows the frozen
    /// payload validator accepts. No rejection or acceptance is claimed.
    SystemPayload,
}

/// Decode one row as the strict V3 codec does after admitting rows 0 through
/// `expected_seq - 1`.
///
/// Checks run in TypeScript's order and the first failure wins: raw-row
/// admission of `request/header`, `system/message`, and obsolete dispatch
/// rows; then [`decode_row_envelope`]; then the decoded event's envelope by
/// vocabulary, surface operation and sources, and the canonical `request/header`
/// and `tool/result` payload rules. `source_budget` bounds the expanded source
/// list only and does not make every TypeScript resource failure a limit. The
/// parsed row is the caller's, so its size and nesting were bounded, if at
/// all, by whoever parsed it. Source decoding also scales with the expanded list.
pub fn decode_v3_row(
    row: &Value,
    expected_seq: u64,
    source_budget: usize,
) -> Result<V3CodecEvent<'_>, V3RowRefusal> {
    if expected_seq > MAX_SAFE_INTEGER {
        return Err(V3RowRefusal::ExpectedSeqOutOfRange);
    }
    if let Value::Object(fields) = row {
        admit_row(fields)?;
    }
    let envelope =
        decode_row_envelope(row, expected_seq, source_budget).map_err(|refusal| match refusal {
            EnvelopeRefusal::ExpectedSeqOutOfRange => V3RowRefusal::ExpectedSeqOutOfRange,
            EnvelopeRefusal::Rejected(rejection) => {
                V3RowRefusal::Rejected(V3Rejection::Envelope(rejection))
            }
            EnvelopeRefusal::NativeSubset(limit) => {
                V3RowRefusal::NativeSubset(V3Limit::Envelope(limit))
            }
        })?;
    check_event(&envelope)?;
    Ok(V3CodecEvent { envelope })
}

const fn structural<T>(rejection: StructuralRejection) -> Result<T, V3RowRefusal> {
    Err(V3RowRefusal::Rejected(V3Rejection::Structural(rejection)))
}

const fn subset<T>(limit: V3Limit) -> Result<T, V3RowRefusal> {
    Err(V3RowRefusal::NativeSubset(limit))
}

/// TypeScript's `assertV3RowAdmission` on an object row.
fn admit_row(fields: &Map<String, Value>) -> Result<(), V3RowRefusal> {
    match fields.get("type").and_then(Value::as_str) {
        Some("request/header") => {
            let Some(Value::Object(data)) = fields.get("data") else {
                return structural(StructuralRejection::HeaderDataNotObject);
            };
            let Some(Value::Object(header)) = data.get("header") else {
                return structural(StructuralRejection::HeaderNotObject);
            };
            if header.contains_key("system") {
                return Err(V3RowRefusal::Unsupported(
                    V3Unsupported::RetiredHeaderSystem,
                ));
            }
            Ok(())
        }
        Some("system/message") => {
            let Some(Value::Object(data)) = fields.get("data") else {
                return structural(StructuralRejection::SystemDataNotObject);
            };
            admit_system(data)
        }
        Some(event_type) if fields.get("ignorable") != Some(&Value::Bool(true)) => {
            let Some(&event_type) = OBSOLETE_TYPES.iter().find(|name| **name == event_type) else {
                return Ok(());
            };
            let Some(seq) = obsolete_seq(fields.get("seq")) else {
                return subset(V3Limit::ObsoleteSeqDiagnostic);
            };
            Err(V3RowRefusal::Unsupported(V3Unsupported::ObsoleteType {
                event_type,
                seq,
            }))
        }
        _ => Ok(()),
    }
}

/// JavaScript's `String(seq)` where this crate renders it. Every `f64` is
/// refused, -0 included: serde_json also reads an underflowing spelling such
/// as `-2.4703282292062328e-324` as -0, where `JSON.parse` yields `-5e-324`.
fn obsolete_seq(seq: Option<&Value>) -> Option<String> {
    let Some(seq) = seq else {
        return Some("undefined".to_owned());
    };
    match seq {
        Value::Null => Some("null".to_owned()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => {
            if let Some(number) = number.as_u64() {
                (number <= MAX_SAFE_INTEGER).then(|| number.to_string())
            } else if let Some(number) = number.as_i64() {
                (number.unsigned_abs() <= MAX_SAFE_INTEGER).then(|| number.to_string())
            } else {
                None
            }
        }
        Value::Array(_) | Value::Object(_) => None,
    }
}

/// The first missing required key, then any other key, as TypeScript's `keys`.
fn exact_keys(
    record: &Map<String, Value>,
    required: &[&'static str],
    which: SystemRecord,
) -> Result<(), V3RowRefusal> {
    if let Some(key) = required.iter().find(|key| !record.contains_key(**key)) {
        return structural(StructuralRejection::MissingField { record: which, key });
    }
    let keys: Vec<String> = record
        .keys()
        .filter(|key| !required.contains(&key.as_str()))
        .cloned()
        .collect();
    if !keys.is_empty() {
        return structural(StructuralRejection::UnexpectedFields {
            record: which,
            keys,
        });
    }
    Ok(())
}

/// TypeScript's `assertSystem`. Its own checks are exact; its final call to
/// the frozen `assertReleasedPayloadSemantics` is claimed only for the closed
/// shape in [`frozen_payload_accepts`].
fn admit_system(data: &Map<String, Value>) -> Result<(), V3RowRefusal> {
    exact_keys(data, &SYSTEM_DATA_KEYS, SystemRecord::Data)?;
    for coordinate in [Coordinate::Turn, Coordinate::Step] {
        match count(&data[coordinate.key()]) {
            None => return structural(StructuralRejection::InvalidCoordinate(coordinate)),
            Some(Count::Safe(0)) => {
                return structural(StructuralRejection::CoordinateNotPositive(coordinate));
            }
            Some(Count::Safe(_)) => {}
            Some(Count::Undecided) => {
                return subset(V3Limit::FloatLexeme(V3NumberField::SystemCoordinate(
                    coordinate,
                )));
            }
        }
    }
    let Value::Object(message) = &data["message"] else {
        return structural(StructuralRejection::SystemMessageNotObject);
    };
    exact_keys(message, &SYSTEM_MESSAGE_KEYS, SystemRecord::Message)?;
    if !matches!(&message["id"], Value::String(id) if !id.is_empty()) || message["role"] != "system"
    {
        return structural(StructuralRejection::SystemIdentity);
    }
    let Value::Object(source) = &message["source"] else {
        return structural(StructuralRejection::SystemSourceNotObject);
    };
    let plugin = match (source.get("kind"), source.get("plugin")) {
        (Some(kind), Some(Value::String(plugin))) if kind == "plugin" && !plugin.is_empty() => {
            plugin
        }
        _ => return structural(StructuralRejection::SystemSource),
    };
    if frozen_payload_accepts(source, plugin, &message["content"]) {
        Ok(())
    } else {
        subset(V3Limit::SystemPayload)
    }
}

/// Whether the frozen validator certainly accepts the message as `user/message`
/// content, given the checks above: a source of exactly `kind` and a plugin
/// other than `compact`, which would require a `compactionId`, and an array of
/// blocks that are each exactly `{type: "text" | "reasoning", text: <string>}`.
/// An empty array qualifies.
fn frozen_payload_accepts(source: &Map<String, Value>, plugin: &str, content: &Value) -> bool {
    let text_block = |block: &Value| {
        block.as_object().is_some_and(|block| {
            block.len() == 2
                && matches!(
                    block.get("type").and_then(Value::as_str),
                    Some("text" | "reasoning")
                )
                && block.get("text").is_some_and(Value::is_string)
        })
    };
    source.len() == 2
        && plugin != "compact"
        && content
            .as_array()
            .is_some_and(|blocks| blocks.iter().all(text_block))
}

/// TypeScript's `assertV3Event` on the decoded event, without an installed
/// vocabulary.
///
/// The envelope decoder already enforced what `assertV3Event` re-checks of
/// the type, seq, time, and `ignorable`, and the uniqueness and order of
/// decoded sources; a -0 seq never reaches here. The codec's second obsolete
/// type check and its rerun of the raw-row admission on the event are
/// elided: the envelope borrows the same immutable `type`, `ignorable`, and
/// `data` values that [`admit_row`] accepted, so both reruns pass.
fn check_event(envelope: &UnadmittedEnvelope<'_>) -> Result<(), V3RowRefusal> {
    let rejected = |rejection| {
        Err(V3RowRefusal::Rejected(V3Rejection::Event {
            event_type: envelope.event_type.to_owned(),
            rejection,
        }))
    };
    let class = vocabulary(envelope.event_type);
    if class == Vocabulary::Known {
        let keys: Vec<String> = [
            ("sourceEventSeqs", envelope.source_event_seqs.is_some()),
            ("surfaceOp", envelope.surface_op.is_some()),
        ]
        .into_iter()
        .filter(|(_, present)| *present)
        .map(|(key, _)| key.to_owned())
        .collect();
        if !keys.is_empty() {
            return rejected(EventRejection::UnexpectedFields { keys });
        }
    }
    if class == Vocabulary::Surface {
        match envelope.surface_op {
            None => return rejected(EventRejection::MissingSurfaceOp),
            Some(Value::String(operation)) if operation == "append" => {}
            Some(Value::Object(replace)) => {
                if replace.len() != 3
                    || replace.get("op").and_then(Value::as_str) != Some("replace")
                    || !replace.contains_key("startSeq")
                    || !replace.contains_key("endSeq")
                {
                    return rejected(EventRejection::InexactReplace);
                }
                for endpoint in [Endpoint::StartSeq, Endpoint::EndSeq] {
                    match count(&replace[endpoint.key()]) {
                        None => return rejected(EventRejection::InvalidEndpoint(endpoint)),
                        Some(Count::Safe(seq)) if seq >= envelope.seq => {
                            return rejected(EventRejection::LaterEndpoint);
                        }
                        Some(Count::Safe(_)) => {}
                        Some(Count::Undecided) => {
                            return subset(V3Limit::FloatLexeme(V3NumberField::Endpoint(endpoint)));
                        }
                    }
                }
            }
            Some(_) => return rejected(EventRejection::SurfaceOpNotObject),
        }
        match &envelope.source_event_seqs {
            Some(_) if envelope.event_type == "assistant/message" => {
                return rejected(EventRejection::AssistantSources);
            }
            Some(seqs) if seqs.is_empty() => return rejected(EventRejection::EmptySources),
            _ => {}
        }
    }
    match envelope.event_type {
        "request/header" => {
            // `admit_row` proved this borrowed data and its header are objects.
            let header = &envelope.data["header"];
            if header["tools"].as_array().is_some_and(Vec::is_empty)
                || header["adapterDefaults"]
                    .as_object()
                    .is_some_and(Map::is_empty)
            {
                return rejected(EventRejection::EmptyHeaderOptional);
            }
        }
        "tool/result" => {
            let Value::Object(data) = envelope.data else {
                return rejected(EventRejection::ToolResultDataNotObject);
            };
            if data.contains_key("error") {
                let Some(Value::Object(message)) = data.get("message") else {
                    return rejected(EventRejection::ToolResultMessageNotObject);
                };
                let error_block = match message.get("content") {
                    Some(Value::Array(content)) => match content.as_slice() {
                        [Value::Object(block)] => {
                            block.get("type").and_then(Value::as_str) == Some("tool-result")
                                && block.get("isError") == Some(&Value::Bool(true))
                        }
                        _ => false,
                    },
                    _ => false,
                };
                if !error_block {
                    return rejected(EventRejection::NonErrorToolResult);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn names(table: &Value, list: &str) -> BTreeSet<String> {
        table["vocabulary"][list]
            .as_array()
            .unwrap_or_else(|| panic!("vocabulary.{list} array"))
            .iter()
            .map(|name| name.as_str().expect("type name").to_owned())
            .collect()
    }

    fn set(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn vocabulary_lists_equal_the_shared_table() {
        // The TypeScript spec checks the same lists against the real exports.
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../conformance/session/v3-row-cases.json"
        );
        let text = std::fs::read_to_string(path).expect("read v3-row-cases.json");
        let table: Value = serde_json::from_str(&text).expect("parse v3-row-cases.json");
        assert_eq!(names(&table, "surfaceTypes"), set(&SURFACE_TYPES));
        assert_eq!(names(&table, "obsoleteTypes"), set(&OBSOLETE_TYPES));
        assert_eq!(names(&table, "dispositionTypes"), set(&DISPOSITION_TYPES));
        assert_eq!(names(&table, "nativeTypes"), set(&NATIVE_TYPES));
        assert_eq!(
            names(&table, "objectPrototypeNames"),
            set(&OBJECT_PROTOTYPE_NAMES)
        );
    }

    #[test]
    fn expected_seq_must_be_a_safe_integer() {
        let row = serde_json::json!({"type": "t", "seq": 0, "time": 0, "data": {}});
        assert_eq!(
            decode_v3_row(&row, MAX_SAFE_INTEGER + 1, 0),
            Err(V3RowRefusal::ExpectedSeqOutOfRange)
        );
        // The caller bound precedes raw-row admission.
        let header = serde_json::json!({"type": "request/header"});
        assert_eq!(
            decode_v3_row(&header, u64::MAX, 0),
            Err(V3RowRefusal::ExpectedSeqOutOfRange)
        );
    }
}
