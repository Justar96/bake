//! Development-only reproduction of the TypeScript test helper
//! `replayRequests` in `packages/core/agent-loop/tests/runtime-fixture.ts`,
//! over a closed subset of current-format logs.
//!
//! The helper does not restore its log. For a log already in format 3,
//! `scanLog` runs only the strict V3 codec on each row and `finish`: the
//! catalog's transformed validation is the identity for current input. The
//! helper then constructs a Session from each request's prefix. Session
//! construction validates each prefix event's envelope, message, settlement,
//! and request-header fields, and its surface transition, including the rule
//! that only a `system/message` may replace the system head. The helper skips
//! full restoration (`restoreReleasedV3Artifact`) and with it the restored
//! vocabulary, step and turn relationships, tool lifecycles, and the
//! protected-first-head rule, so it accepts an unknown required event type.
//! Requests derived here therefore carry no restoration claim. This subset
//! instead admits only its own 12 event types and refuses every replacement.
//!
//! [`replay_requests`] runs the same stages in the same order:
//!
//! 1. The header record through [`read_header_record`].
//! 2. Every row through [`decode_v3_row`]; the first refusal wins.
//! 3. Subset qualification of every row, and the step and settlement
//!    coordinates.
//! 4. Each prefix that ends before a step's settlement: number qualification,
//!    the Session construction checks the codec does not already cover, and
//!    the [`RequestFold`].
//!
//! Rows at or after the last cut are never checked by Session construction,
//! as in the helper. For rows of the 12 subset types, the codec already
//! proves the envelope fields, sequence contiguity, the `surfaceOp` marker and its
//! eligibility, source references, and the `request/header` and `tool/result`
//! rules of `validateSessionEventData`. Its `system/message` checks imply
//! every Session check of that type. Those obligations are delegated, not
//! repeated.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::request::{Fact, FoldRefusal, Request, RequestFold, SurfaceKind};
use crate::{
    HeaderRefusal, MAX_SAFE_INTEGER, PathPlatform, V3CodecEvent, V3RowRefusal, decode_v3_row,
    read_header_record,
};

/// The event types this subset admits: those of the committed
/// request-reconstruction fixture.
const SUBSET_TYPES: [&str; 12] = [
    "agent/inbox/spliced",
    "assistant/message",
    "request/context",
    "request/header",
    "step/end",
    "step/start",
    "system/message",
    "tool/call",
    "tool/result",
    "turn/end",
    "turn/start",
    "user/message",
];
/// The `LlmCallConfig` data members in `packages/llm/llm/src/call-config.ts`.
/// Request assembly spreads `config` before its own members, so this closed
/// set also keeps `config` from supplying them.
const CONFIG_KEYS: [&str; 6] = [
    "provider",
    "model",
    "reasoningEffort",
    "temperature",
    "maxTokens",
    "stop",
];
const HEADER_REASONS: [&str; 4] = ["initial", "resume", "change", "series"];
/// Deepest array and object nesting this subset copies; a scalar may sit one
/// level below it. Copies and comparisons of `serde_json::Value` recurse, so
/// this bounds them.
const MAX_PAYLOAD_DEPTH: usize = 64;

/// Why [`replay_requests`] returned no requests.
///
/// Every variant except [`ReplayRefusal::NativeSubset`] and the native-subset
/// outcomes it wraps claims only that `replayRequests` throws for the same
/// log. None claims the TypeScript error's class or message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayRefusal {
    /// The header reader refused the record. Its own
    /// [`HeaderRefusal::NativeSubset`] claims nothing.
    Header(HeaderRefusal),
    /// The V3 codec refused row `seq`. Its own [`V3RowRefusal::NativeSubset`]
    /// claims nothing.
    Row { seq: u64, refusal: V3RowRefusal },
    /// Session construction rejects the first prefix that contains row `seq`.
    Seed { seq: u64, rejection: SeedRejection },
    /// The step has no Assistant settlement after its `step/start`.
    NoLaterSettlement { turn: u64, step: u64 },
    /// No `request/header` precedes the step's settlement.
    NoRequestHeader { turn: u64, step: u64 },
    /// This subset cannot reproduce the outcome; nothing is claimed.
    /// `seq` names the row, or `None` for the header.
    NativeSubset {
        seq: Option<u64>,
        limit: ReplayLimit,
    },
}

/// A Session construction check that rejected a prefix event, in the order
/// `assertCurrentLlmShape` in `packages/core/session/src/index.ts` runs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedRejection {
    /// The message is not an object with a non-empty string `id`.
    MessageIdentity,
    /// The message `role` does not match its event type.
    MessageRole,
    /// The message `source` lacks a non-empty string `kind`.
    MessageSource,
    /// The message `content` is not an array.
    MessageContent,
    /// An Assistant source is not `model` with a non-empty provider and model.
    ModelSource,
    /// A tool result source is not `tool` with a non-empty `callId`.
    ToolSource,
    /// A tool result's content is not exactly one `tool-result` block with
    /// array `content`.
    ToolResultBlock,
    /// The block's `toolCallId` differs from the source's `callId`.
    ToolCallId,
    /// An Assistant message's `stream` is not an array. Session construction
    /// also checks `turn` and `step` here, but whole-log qualification refuses
    /// a non-safe-integer coordinate as [`ReplayLimit::Coordinate`] first.
    Settlement,
    /// `config` lacks a non-empty string `provider` or `model`.
    HeaderProviderModel,
    /// A present `config.reasoningEffort` is not a non-empty string.
    HeaderReasoningEffort,
    /// `adapterDefaults` is not an object of `true` markers for configured
    /// `reasoningEffort` or `maxTokens` values.
    HeaderAdapterDefaults,
    /// `reason` is not `initial`, `resume`, `change`, or `series`.
    HeaderReason,
    /// A present `startsSeries` is not `true`.
    HeaderStartsSeries,
}

/// Input this subset does not derive, whatever TypeScript does with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayLimit {
    /// The header is seeded. Seeded logs need an inherited cut and the codec's
    /// `finish` checks, which this subset does not implement.
    SeededHeader,
    /// A row type outside the fixture's 12 types, including ignorable
    /// unknown types, `assistant/attempt`, tool updates, message projections,
    /// and `session/end-seed`.
    EventType,
    /// A row carries `ignorable`.
    Ignorable,
    /// A surface row replaces a range instead of appending.
    Replacement,
    /// A prefix payload holds a number other than a safe integer. JavaScript's
    /// rounding, -0, and underflow cannot be decided from a parsed `f64`.
    Number,
    /// A prefix payload nests arrays and objects more than 64 containers deep.
    Depth,
    /// A `step/start` or `assistant/message` coordinate is not an object
    /// member of safe-integer `turn` and `step`.
    Coordinate,
    /// Two `step/start` rows, or two Assistant settlements, share a
    /// coordinate. Retries are outside this subset.
    RepeatedCoordinate,
    /// A second `request/header` in a prefix.
    HeaderChange,
    /// `config` holds a member outside `LlmCallConfig`.
    ConfigMember,
    /// `tools` is present but not an array of objects.
    ToolSchema,
}

/// One `(turn, step)` pair.
type Coordinate = (u64, u64);

/// Rebuild the request each step dispatched, as `replayRequests` does.
///
/// `header_record` is the log's first record, LF included. `rows` are the
/// parsed event rows in log order, numbered from 0, and `source_budget`
/// bounds each row's expanded `sourceEventSeqs` as in [`decode_v3_row`]. Each
/// `step/start` yields one request, in log order: the messages and header of
/// the prefix that ends before the first Assistant message with the same
/// coordinate. Framing, torn tails, and JSON parsing belong to the caller.
pub fn replay_requests(
    header_record: &[u8],
    platform: PathPlatform,
    rows: &[Value],
    source_budget: usize,
) -> Result<Vec<Request>, ReplayRefusal> {
    let header = read_header_record(header_record, platform).map_err(ReplayRefusal::Header)?;
    if header.is_seeded {
        return Err(limit(None, ReplayLimit::SeededHeader));
    }
    let mut events = Vec::with_capacity(rows.len());
    for (seq, row) in (0u64..).zip(rows) {
        events.push(
            decode_v3_row(row, seq, source_budget)
                .map_err(|refusal| ReplayRefusal::Row { seq, refusal })?,
        );
    }
    let mut starts: Vec<(Coordinate, u64)> = Vec::new();
    let mut started = BTreeSet::new();
    let mut settlements: BTreeMap<Coordinate, u64> = BTreeMap::new();
    for event in &events {
        let envelope = event.envelope();
        let seq = envelope.seq;
        if !SUBSET_TYPES.contains(&envelope.event_type) {
            return Err(limit(Some(seq), ReplayLimit::EventType));
        }
        if envelope.ignorable {
            return Err(limit(Some(seq), ReplayLimit::Ignorable));
        }
        if envelope.surface_op.is_some_and(|op| *op != "append") {
            return Err(limit(Some(seq), ReplayLimit::Replacement));
        }
        if !matches!(envelope.event_type, "step/start" | "assistant/message") {
            continue;
        }
        let at = coordinate(envelope.data).ok_or(limit(Some(seq), ReplayLimit::Coordinate))?;
        let repeated = if envelope.event_type == "step/start" {
            starts.push((at, seq));
            !started.insert(at)
        } else {
            settlements.insert(at, seq).is_some()
        };
        if repeated {
            return Err(limit(Some(seq), ReplayLimit::RepeatedCoordinate));
        }
    }
    // With unique coordinates, the first settlement `find` returns is the
    // only one. A missing or earlier settlement ends the helper at that step,
    // so later steps contribute no prefix.
    let cuts: Vec<Option<u64>> = starts
        .iter()
        .map(|(at, start)| settlements.get(at).copied().filter(|cut| cut > start))
        .collect();
    let end = cuts.iter().map_while(|cut| *cut).max().unwrap_or(0);
    let cut_set: BTreeSet<u64> = cuts.iter().flatten().copied().collect();
    let mut fold = RequestFold::new(header.id);
    let mut snapshots: BTreeMap<u64, Option<Request>> = BTreeMap::new();
    let mut failure = None;
    for event in &events[..usize::try_from(end).unwrap_or(events.len())] {
        let seq = event.envelope().seq;
        if cut_set.contains(&seq) {
            snapshots.insert(seq, fold.request());
        }
        if let Err(refusal) = admit(event).and_then(|fact| {
            fold.append(fact)
                .map_err(|FoldRefusal::HeaderChange| limit(Some(seq), ReplayLimit::HeaderChange))
        }) {
            failure = Some((seq, refusal));
            break;
        }
    }
    if failure.is_none() {
        snapshots.insert(end, fold.request());
    }
    let mut requests = Vec::with_capacity(cuts.len());
    for (((turn, step), _), cut) in starts.into_iter().zip(cuts) {
        let Some(cut) = cut else {
            return Err(ReplayRefusal::NoLaterSettlement { turn, step });
        };
        if let Some((seq, refusal)) = &failure
            && *seq < cut
        {
            return Err(refusal.clone());
        }
        let snapshot = snapshots.get(&cut).cloned().flatten();
        requests.push(snapshot.ok_or(ReplayRefusal::NoRequestHeader { turn, step })?);
    }
    Ok(requests)
}

const fn limit(seq: Option<u64>, limit: ReplayLimit) -> ReplayRefusal {
    ReplayRefusal::NativeSubset { seq, limit }
}

const fn seed(seq: u64, rejection: SeedRejection) -> ReplayRefusal {
    ReplayRefusal::Seed { seq, rejection }
}

fn safe_count(value: Option<&Value>) -> Option<u64> {
    value
        .and_then(Value::as_u64)
        .filter(|count| *count <= MAX_SAFE_INTEGER)
}

fn coordinate(data: &Value) -> Option<Coordinate> {
    Some((safe_count(data.get("turn"))?, safe_count(data.get("step"))?))
}

fn non_empty_string(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|text| !text.is_empty())
}

/// Admit one prefix row: qualify its payload, run the Session construction
/// checks the codec leaves, and convert it to a fact.
fn admit(event: &V3CodecEvent<'_>) -> Result<Fact, ReplayRefusal> {
    let envelope = event.envelope();
    let seq = envelope.seq;
    let data = envelope.data;
    qualify_payload(data).map_err(|refusal| limit(Some(seq), refusal))?;
    let surface = |kind, message: &Value, role| {
        let message = message_shape(message, role).map_err(|rejection| seed(seq, rejection))?;
        Ok(Fact::Surface {
            kind,
            message: message.clone(),
        })
    };
    match envelope.event_type {
        "system/message" => surface(SurfaceKind::System, &data["message"], "system"),
        "user/message" => surface(SurfaceKind::User, data, "user"),
        "assistant/message" => {
            let fact = surface(SurfaceKind::Assistant, &data["message"], "assistant")?;
            let source = &data["message"]["source"];
            if source["kind"] != "model"
                || !non_empty_string(source.get("provider"))
                || !non_empty_string(source.get("model"))
            {
                return Err(seed(seq, SeedRejection::ModelSource));
            }
            if !data["stream"].is_array() {
                return Err(seed(seq, SeedRejection::Settlement));
            }
            Ok(fact)
        }
        "tool/result" => {
            let fact = surface(SurfaceKind::ToolResult, &data["message"], "user")?;
            tool_result(&data["message"]).map_err(|rejection| seed(seq, rejection))?;
            Ok(fact)
        }
        "request/header" => request_header(seq, data),
        _ => Ok(Fact::LogOnly),
    }
}

/// `snapshotJsonValue` refuses -0 and non-finite numbers in every prefix
/// event, and JavaScript rounds integers beyond 2^53. Admitting only safe
/// integers covers both conservatively. The walk is iterative, so it is safe
/// at any depth the caller's parser produced.
fn qualify_payload(data: &Value) -> Result<(), ReplayLimit> {
    let mut pending = vec![(data, 1usize)];
    while let Some((value, depth)) = pending.pop() {
        match value {
            Value::Number(number) => {
                let safe = number.as_u64().is_some_and(|n| n <= MAX_SAFE_INTEGER)
                    || number
                        .as_i64()
                        .is_some_and(|n| n.unsigned_abs() <= MAX_SAFE_INTEGER);
                if !safe {
                    return Err(ReplayLimit::Number);
                }
            }
            Value::Array(items) => {
                if depth > MAX_PAYLOAD_DEPTH {
                    return Err(ReplayLimit::Depth);
                }
                pending.extend(items.iter().map(|item| (item, depth + 1)));
            }
            Value::Object(fields) => {
                if depth > MAX_PAYLOAD_DEPTH {
                    return Err(ReplayLimit::Depth);
                }
                pending.extend(fields.values().map(|item| (item, depth + 1)));
            }
            Value::Null | Value::Bool(_) | Value::String(_) => {}
        }
    }
    Ok(())
}

/// `assertMessageEventShape`'s checks common to every surface type.
fn message_shape<'a>(
    message: &'a Value,
    role: &str,
) -> Result<&'a Map<String, Value>, SeedRejection> {
    let Some(fields) = message
        .as_object()
        .filter(|fields| non_empty_string(fields.get("id")))
    else {
        return Err(SeedRejection::MessageIdentity);
    };
    if fields.get("role").and_then(Value::as_str) != Some(role) {
        return Err(SeedRejection::MessageRole);
    }
    if !non_empty_string(fields.get("source").and_then(|source| source.get("kind"))) {
        return Err(SeedRejection::MessageSource);
    }
    if !fields.get("content").is_some_and(Value::is_array) {
        return Err(SeedRejection::MessageContent);
    }
    Ok(fields)
}

/// `assertMessageEventShape`'s `tool/result` checks, after the common ones.
fn tool_result(message: &Value) -> Result<(), SeedRejection> {
    let source = &message["source"];
    let call_id = source.get("callId").filter(|id| non_empty_string(Some(id)));
    let Some(call_id) = call_id.filter(|_| source["kind"] == "tool") else {
        return Err(SeedRejection::ToolSource);
    };
    let block = match message["content"].as_array().map(Vec::as_slice) {
        Some([Value::Object(block)]) => block,
        _ => return Err(SeedRejection::ToolResultBlock),
    };
    if block.get("type").and_then(Value::as_str) != Some("tool-result")
        || !block.get("content").is_some_and(Value::is_array)
    {
        return Err(SeedRejection::ToolResultBlock);
    }
    if block.get("toolCallId") != Some(call_id) {
        return Err(SeedRejection::ToolCallId);
    }
    Ok(())
}

/// `assertCurrentLlmShape`'s `request/header` checks, then the subset's
/// `config` and `tools` qualification.
fn request_header(seq: u64, data: &Value) -> Result<Fact, ReplayRefusal> {
    // The codec proved that `data` and `data.header` are objects.
    let header = &data["header"];
    let Some(config) = header["config"].as_object().filter(|config| {
        non_empty_string(config.get("provider")) && non_empty_string(config.get("model"))
    }) else {
        return Err(seed(seq, SeedRejection::HeaderProviderModel));
    };
    if config
        .get("reasoningEffort")
        .is_some_and(|effort| !non_empty_string(Some(effort)))
    {
        return Err(seed(seq, SeedRejection::HeaderReasoningEffort));
    }
    if let Some(defaults) = header.get("adapterDefaults") {
        let valid = defaults.as_object().is_some_and(|defaults| {
            defaults.iter().all(|(key, marker)| {
                (key == "reasoningEffort" || key == "maxTokens")
                    && *marker == Value::Bool(true)
                    && config.contains_key(key)
            })
        });
        if !valid {
            return Err(seed(seq, SeedRejection::HeaderAdapterDefaults));
        }
    }
    if !data
        .get("reason")
        .and_then(Value::as_str)
        .is_some_and(|reason| HEADER_REASONS.contains(&reason))
    {
        return Err(seed(seq, SeedRejection::HeaderReason));
    }
    if data
        .get("startsSeries")
        .is_some_and(|marker| *marker != Value::Bool(true))
    {
        return Err(seed(seq, SeedRejection::HeaderStartsSeries));
    }
    if config
        .keys()
        .any(|key| !CONFIG_KEYS.contains(&key.as_str()))
    {
        return Err(limit(Some(seq), ReplayLimit::ConfigMember));
    }
    let tools = match header.get("tools") {
        None => None,
        Some(Value::Array(tools)) if tools.iter().all(Value::is_object) => Some(tools.clone()),
        Some(_) => return Err(limit(Some(seq), ReplayLimit::ToolSchema)),
    };
    Ok(Fact::Header {
        config: config.clone(),
        tools,
    })
}
