//! Development-only reproduction of the TypeScript test helper
//! `replayRequests` in `packages/core/agent-loop/tests/runtime-fixture.ts`,
//! over a closed subset of current-format logs.
//!
//! The helper does not restore its log. For a log already in format 3,
//! `scanLog` runs only the strict V3 codec on each row and `finish`: the
//! catalog's transformed validation is the identity for current input. The
//! helper then refuses a log with bytes after the decoded prefix or a seeded
//! one, and constructs a Session from each request's prefix. Session
//! construction snapshots each prefix event as lossless JSON, then validates
//! its envelope, message, settlement, and request-header fields, and its
//! surface transition, including the rule that a replacement starting at a
//! `system/message` in node 0 must be one `system/message` over exactly that
//! node. The helper skips full restoration (`restoreReleasedV3Artifact`) and
//! with it the restored vocabulary, step and turn relationships, tool
//! lifecycles, compaction records, and restoration's stricter
//! protected-first-head rules, so it accepts an unknown required event type
//! and some logs restoration refuses. Requests derived here therefore carry no
//! restoration claim. This subset admits the 60 known event types other than
//! `image/offload` and `session/end-seed`, and no `ignorable` row.
//!
//! [`replay_requests`] runs the same stages in the same order:
//!
//! 1. [`scan_log`], with its documented refusal and native-limit contracts.
//! 2. Uncommitted trailing bytes, including any record after the first issue
//!    and a torn tail, then a seeded header or a nonzero inherited cut.
//! 3. Subset qualification of every row, and the step and settlement
//!    coordinates. Each step yields one request per settlement with its
//!    coordinate. Both `assistant/message` and `assistant/attempt` supply
//!    cutoffs, including interrupted messages.
//! 4. Each prefix that ends before a settlement: per event, the -0 check that
//!    is exactly the lossless snapshot for scan-admitted rows, number and
//!    depth qualification of projected payloads, the Session construction
//!    checks the codec does not already cover, and the [`RequestFold`], which
//!    plans each surface replacement and checks each tool update against the
//!    current state before it changes them.
//!
//! Rows at or after the last cut are never checked by Session construction,
//! as in the helper, so a codec-admitted row there that Session construction
//! would refuse, such as an invalid replacement or a -0, does not refuse the
//! log. A codec-invalid row anywhere still does: it ends the scan's prefix, so
//! its bytes are uncommitted, unless it is a `turn/end` and the scan throws.
//! For the surface types and every known type the codec classifies, the codec
//! already proves the envelope fields, sequence contiguity, the `surfaceOp`
//! marker and its eligibility, an exact replacement shape with earlier
//! endpoints, the event-local source rules (non-empty, unique, earlier, none
//! on an Assistant message), and the `request/header` and `tool/result` rules
//! of `validateSessionEventData`. Its `system/message` checks imply every
//! Session check of that type. Session construction applies no
//! type-specific check to the other known non-surface types, such as the
//! permission and sandbox knobs, titles, hooks, compaction records, and retry
//! records, except the attempt's settlement fields, so derivation does not
//! either, and admitting one does not establish that it is valid. Those
//! obligations are delegated, not repeated. Locating endpoints and checking
//! source coverage and the tool-result and system-head rules need the current
//! nodes, so the fold runs them.
//!
//! Projected payloads, those of the surface messages, request headers, and
//! tool updates, admit only safe integers and at most 64 nested containers,
//! because their values reach request JSON and its copies. Other payloads
//! reach no request, so any number except -0 and any depth the scan parsed is
//! admitted, as the lossless snapshot admits them.
//!
//! The codec treats `request/tool-update` and five other known types
//! (`deliverables/presented`, `image/offload`, `subagent/catalog`,
//! `subagent/routing-decision`, and `workspace/changes`) as opaque: it checks
//! the envelope fields and decodes any `sourceEventSeqs` like every row's, but
//! proves nothing about the payload or marker eligibility, so a `surfaceOp` of
//! any value reaches Session construction. Derivation therefore refuses either
//! marker on those types, as `surfaceOpOf` does, without reading its value.
//! For the tool update it runs every Session check in Session construction's
//! order: `validateToolUpdateData`, that marker refusal, and
//! `validateToolUpdate`, whose header, change, and anchor checks the fold
//! runs. Its `ignorable` check stays behind the whole-log
//! [`ReplayLimit::Ignorable`]. Each request's tool history is the
//! `ToolHistoryProjection` snapshot of its prefix, and its config and tools
//! come from the latest header, as `foldRequestHeader` reads them.
//!
//! Whether a later header redeclares a tool compares `JSON.stringify` text,
//! which depends on member order. Each object's member order is read from the
//! scan's parsed rows, which the workspace's `preserve_order` feature keeps
//! in insertion order; JavaScript's array-index-first enumeration is applied
//! when comparing, as `JSON.parse` would see the log's members.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::request::{Fact, FoldRefusal, Request, RequestFold, SurfaceKind, SurfaceOp};
use crate::{MAX_SAFE_INTEGER, PathPlatform, ScanRefusal, V3CodecEvent, scan_log};

/// `KNOWN_SESSION_EVENT_TYPES` in
/// `packages/core/session/src/known-event-types.ts`, the vocabulary this build
/// understands. A unit test pins the list against that generated file.
const KNOWN_EVENT_TYPES: [&str; 60] = [
    "agent-preset/selected",
    "agent/inbox/spliced",
    "approval/asked",
    "approval/decided",
    "approval/policy",
    "assistant/attempt",
    "assistant/message",
    "command/done",
    "command/run",
    "compaction/end",
    "compaction/prune",
    "compaction/start",
    "compaction/summary",
    "deliverables/presented",
    "feedback/message-delete",
    "feedback/message-put",
    "feedback/record",
    "goal/change",
    "hook/invoked",
    "hook/result",
    "image/offload",
    "llm/retry",
    "llm/retry-started",
    "model/selection",
    "permission/preset",
    "plan/mode",
    "request/context",
    "request/header",
    "request/tool-update",
    "sandbox/mode",
    "schedule/change",
    "session-log-deepseek/delivery-accepted",
    "session/end-seed",
    "session/title",
    "session/title-llm-request",
    "step/end",
    "step/start",
    "subagent/catalog",
    "subagent/descriptor",
    "subagent/model-selection-policy",
    "subagent/routing-decision",
    "system/message",
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
    "tool/ptc-dispatch",
    "tool/ptc-dispatch-start",
    "tool/result",
    "turn/end",
    "turn/start",
    "user/message",
    "web/deepseek-search-llm-request",
    "workspace/changes",
];
/// Known types this subset still refuses: `image/offload` needs a message
/// projection, and `session/end-seed` belongs to seeded logs.
const EXCLUDED_TYPES: [&str; 2] = ["image/offload", "session/end-seed"];
/// The types whose payload a request can carry. Their payloads keep the
/// conservative number and depth qualification.
const PROJECTED_TYPES: [&str; 6] = [
    "system/message",
    "user/message",
    "assistant/message",
    "tool/result",
    "request/header",
    "request/tool-update",
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
/// [`ReplayRefusal::Scan`], [`ReplayRefusal::Uncommitted`], and
/// [`ReplayRefusal::Seeded`] claim the TypeScript error as their own
/// documentation states it, except where a scan native limit claims nothing.
/// Every other variant except [`ReplayRefusal::NativeSubset`] claims only that
/// `replayRequests` throws for the same log, not the error's class or message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayRefusal {
    /// `scanLog` throws; see [`ScanRefusal`].
    Scan(ScanRefusal),
    /// The log holds `bytes` bytes after the scan's committed prefix.
    /// TypeScript throws a plain `Error`, "log has `bytes` uncommitted
    /// trailing bytes".
    Uncommitted { bytes: usize },
    /// The header is seeded. TypeScript throws a plain `Error`, "replay
    /// expects an unseeded log". A scanned unseeded log always has a zero
    /// inherited cut.
    Seeded,
    /// Session construction rejects the first prefix that contains row `seq`.
    Seed { seq: u64, rejection: SeedRejection },
    /// The step has no Assistant settlement after its `step/start`.
    NoLaterSettlement { turn: u64, step: u64 },
    /// No `request/header` precedes the step's settlement.
    NoRequestHeader { turn: u64, step: u64 },
    /// This subset cannot reproduce the outcome; nothing is claimed.
    /// `seq` names the row.
    NativeSubset { seq: u64, limit: ReplayLimit },
}

impl ReplayRefusal {
    /// TypeScript's exact message for a scan, uncommitted, or seeded refusal,
    /// where [`ScanRefusal::message`] renders one; otherwise `None`.
    pub fn message(&self) -> Option<String> {
        match self {
            Self::Scan(refusal) => refusal.message(),
            Self::Uncommitted { bytes } => {
                Some(format!("log has {bytes} uncommitted trailing bytes"))
            }
            Self::Seeded => Some("replay expects an unseeded log".to_owned()),
            _ => None,
        }
    }
}

/// A Session construction check that rejected a prefix event, in the order
/// Session construction runs them: the lossless snapshot, then
/// `validateToolUpdateData`, then
/// `assertCurrentLlmShape` in `packages/core/session/src/index.ts`, then the
/// surface metadata and replacement checks of `planSurfaceEvent` in
/// `packages/core/session/src/surface.ts`, then `validateToolUpdate` in
/// `packages/core/session/src/tool-history.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedRejection {
    /// The event holds -0, which `snapshotJsonValue` refuses. TypeScript
    /// throws "seed event at index `seq` is not losslessly
    /// JSON-serializable" before any other check of the event.
    LosslessJson,
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
    /// An Assistant message's or attempt's `stream` is not an array. Session construction
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
    /// A `request/tool-update`'s data fails `validateToolUpdateData`.
    ToolUpdateData,
    /// A `request/tool-update` carries `surfaceOp` or `sourceEventSeqs`. The
    /// codec treats the type as opaque; Session construction does not.
    NonSurfaceMarker,
    /// A replacement's `startSeq` is not a current surface node.
    ReplaceStart,
    /// A replacement's `endSeq` is not a current surface node.
    ReplaceEnd,
    /// A replacement's start node sits after its end node.
    ReplaceOrder,
    /// A replacement's `sourceEventSeqs` omit a node it shadows.
    ReplaceSources,
    /// A `tool/result` replacement shadows more than one node.
    ToolResultSpan,
    /// A `tool/result` replacement shadows a node of another type.
    ToolResultTarget,
    /// A `tool/result` replacement changes a member other than its result
    /// block's `content`.
    ToolResultRest,
    /// A replacement covering a `system/message` at node 0 is not one
    /// `system/message` over exactly that node.
    SystemHead,
    /// A tool update's `headerSeq` is not an earlier `request/header`.
    ToolUpdateHeader,
    /// A header or tool update sits between a tool update and the header it
    /// references.
    ToolUpdateStale,
    /// No `request/header` precedes the one a tool update references.
    ToolUpdateBaseline,
    /// A tool update's additions or removals, in order, differ from the names
    /// the referenced header adds to and removes from the one before it.
    ToolUpdateChange,
    /// The last current non-system message is not the user or tool-result
    /// message a tool update names.
    ToolUpdateAnchor,
}

/// Input this subset does not derive, whatever TypeScript does with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayLimit {
    /// A row type outside the 60 known types, required or ignorable, or the
    /// known `image/offload`, which needs a message projection, or
    /// `session/end-seed`, which belongs to seeded logs.
    EventType,
    /// A row carries `ignorable`, including a tool update, which Session
    /// construction refuses only after its data and surface checks.
    Ignorable,
    /// A projected prefix payload, that of a surface message, request header,
    /// or tool update, holds a number other than a safe integer and not -0.
    /// JavaScript's rounding of what a request copies is not decided here.
    Number,
    /// A projected prefix payload nests arrays and objects more than 64
    /// containers deep.
    Depth,
    /// A `step/start`, `assistant/attempt`, or `assistant/message` coordinate
    /// is not an object member of safe-integer `turn` and `step`.
    Coordinate,
    /// Two `step/start` rows share a coordinate. Several settlements of one
    /// step are admitted.
    RepeatedCoordinate,
    /// `config` holds a member outside `LlmCallConfig`.
    ConfigMember,
    /// `tools` is present but not an array of objects.
    ToolSchema,
}

/// One `(turn, step)` pair.
type Coordinate = (u64, u64);

/// Rebuild the request each dispatch sent, as `replayRequests` does.
///
/// `log` is a plain, uncompressed log, and `source_budget` bounds each row's
/// expanded `sourceEventSeqs` as in [`scan_log`]. Each `step/start` in log
/// order yields one request per Assistant settlement, `assistant/attempt` or
/// `assistant/message`, with its coordinate, in log order: the messages and
/// header of the prefix that ends before that settlement. A step whose first
/// settlement is missing or earlier refuses the log. A settlement whose
/// coordinate has no `step/start` cuts nothing. The helper's rule also emits
/// a request for a settlement after a step's `assistant/message`, which the
/// loop never writes; so does this one.
pub fn replay_requests(
    log: &[u8],
    platform: PathPlatform,
    source_budget: usize,
) -> Result<Vec<Request>, ReplayRefusal> {
    let scan = scan_log(log, platform, source_budget).map_err(ReplayRefusal::Scan)?;
    if scan.committed_bytes() != log.len() {
        return Err(ReplayRefusal::Uncommitted {
            bytes: log.len() - scan.committed_bytes(),
        });
    }
    if scan.header().is_seeded || scan.inherited_event_count() != 0 {
        return Err(ReplayRefusal::Seeded);
    }
    let header = scan.header();
    let events: Vec<V3CodecEvent<'_>> = scan.events().collect();
    let mut starts: Vec<(Coordinate, u64)> = Vec::new();
    let mut started = BTreeSet::new();
    let mut settlements: BTreeMap<Coordinate, Vec<u64>> = BTreeMap::new();
    for event in &events {
        let envelope = event.envelope();
        let seq = envelope.seq;
        if !KNOWN_EVENT_TYPES.contains(&envelope.event_type)
            || EXCLUDED_TYPES.contains(&envelope.event_type)
        {
            return Err(limit(seq, ReplayLimit::EventType));
        }
        if envelope.ignorable {
            return Err(limit(seq, ReplayLimit::Ignorable));
        }
        if !matches!(
            envelope.event_type,
            "step/start" | "assistant/message" | "assistant/attempt"
        ) {
            continue;
        }
        let at = coordinate(envelope.data).ok_or(limit(seq, ReplayLimit::Coordinate))?;
        if envelope.event_type == "step/start" {
            starts.push((at, seq));
            if !started.insert(at) {
                return Err(limit(seq, ReplayLimit::RepeatedCoordinate));
            }
        } else {
            settlements.entry(at).or_default().push(seq);
        }
    }
    // A step whose first settlement is missing or earlier ends the helper
    // there, so later steps contribute no prefix. Otherwise every settlement
    // with its coordinate, all later than the step, cuts one request.
    let cuts: Vec<Option<&[u64]>> = starts
        .iter()
        .map(|(at, start)| {
            settlements
                .get(at)
                .map(Vec::as_slice)
                .filter(|cuts| cuts[0] > *start)
        })
        .collect();
    let end = cuts
        .iter()
        .map_while(|cuts| *cuts)
        .flatten()
        .copied()
        .max()
        .unwrap_or(0);
    let cut_set: BTreeSet<u64> = cuts.iter().flatten().copied().flatten().copied().collect();
    let rows = scan.rows();
    let mut fold = RequestFold::new(header.id.clone());
    let mut snapshots: BTreeMap<u64, Option<Request>> = BTreeMap::new();
    let mut failure = None;
    for (event, row) in events
        .iter()
        .zip(rows)
        .take(usize::try_from(end).unwrap_or(events.len()))
    {
        let seq = event.envelope().seq;
        if cut_set.contains(&seq) {
            snapshots.insert(seq, fold.request());
        }
        if let Err(refusal) = admit(event, row)
            .and_then(|fact| fold.append(fact).map_err(|refusal| folded(seq, refusal)))
        {
            failure = Some((seq, refusal));
            break;
        }
    }
    if failure.is_none() {
        snapshots.insert(end, fold.request());
    }
    let mut requests = Vec::new();
    for (((turn, step), _), cuts) in starts.into_iter().zip(cuts) {
        let Some(cuts) = cuts else {
            return Err(ReplayRefusal::NoLaterSettlement { turn, step });
        };
        for cut in cuts {
            if let Some((seq, refusal)) = &failure
                && seq < cut
            {
                return Err(refusal.clone());
            }
            let snapshot = snapshots.get(cut).cloned().flatten();
            requests.push(snapshot.ok_or(ReplayRefusal::NoRequestHeader { turn, step })?);
        }
    }
    Ok(requests)
}

const fn limit(seq: u64, limit: ReplayLimit) -> ReplayRefusal {
    ReplayRefusal::NativeSubset { seq, limit }
}

const fn seed(seq: u64, rejection: SeedRejection) -> ReplayRefusal {
    ReplayRefusal::Seed { seq, rejection }
}

const fn folded(seq: u64, refusal: FoldRefusal) -> ReplayRefusal {
    let rejection = match refusal {
        FoldRefusal::ReplaceStart => SeedRejection::ReplaceStart,
        FoldRefusal::ReplaceEnd => SeedRejection::ReplaceEnd,
        FoldRefusal::ReplaceOrder => SeedRejection::ReplaceOrder,
        FoldRefusal::ReplaceSources => SeedRejection::ReplaceSources,
        FoldRefusal::ToolResultSpan => SeedRejection::ToolResultSpan,
        FoldRefusal::ToolResultTarget => SeedRejection::ToolResultTarget,
        FoldRefusal::ToolResultRest => SeedRejection::ToolResultRest,
        FoldRefusal::SystemHead => SeedRejection::SystemHead,
        FoldRefusal::ToolUpdateHeader => SeedRejection::ToolUpdateHeader,
        FoldRefusal::ToolUpdateStale => SeedRejection::ToolUpdateStale,
        FoldRefusal::ToolUpdateBaseline => SeedRejection::ToolUpdateBaseline,
        FoldRefusal::ToolUpdateChange => SeedRejection::ToolUpdateChange,
        FoldRefusal::ToolUpdateAnchor => SeedRejection::ToolUpdateAnchor,
    };
    seed(seq, rejection)
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

/// Admit one prefix row: check that the whole parsed `row` is lossless JSON,
/// qualify a projected payload, run the Session construction checks the codec
/// leaves, and convert it to a fact.
fn admit(event: &V3CodecEvent<'_>, row: &Value) -> Result<Fact, ReplayRefusal> {
    let envelope = event.envelope();
    let seq = envelope.seq;
    let data = envelope.data;
    if holds_negative_zero(row) {
        return Err(seed(seq, SeedRejection::LosslessJson));
    }
    if PROJECTED_TYPES.contains(&envelope.event_type) {
        qualify_payload(data).map_err(|refusal| limit(seq, refusal))?;
    }
    // Only for the surface types the closure serves did the codec prove the
    // marker: `"append"`, or an exact replacement whose endpoints are safe
    // integers. Other types never read it; an opaque type may carry anything.
    let surface = |kind, message: &Value, role, payload: &Value| {
        message_shape(message, role).map_err(|rejection| seed(seq, rejection))?;
        let op = match envelope.surface_op {
            Some(Value::Object(replace)) => SurfaceOp::Replace {
                start: replace["startSeq"].as_u64().expect("codec-proved endpoint"),
                end: replace["endSeq"].as_u64().expect("codec-proved endpoint"),
                sources: envelope.source_event_seqs.clone().unwrap_or_default(),
            },
            _ => SurfaceOp::Append,
        };
        Ok(Fact::Surface {
            seq,
            kind,
            op,
            // `message_shape` proved the message an object, and the codec
            // proved a tool result's data one.
            payload: payload.as_object().expect("object payload").clone(),
        })
    };
    match envelope.event_type {
        "system/message" => {
            let message = &data["message"];
            surface(SurfaceKind::System, message, "system", message)
        }
        "user/message" => surface(SurfaceKind::User, data, "user", data),
        "assistant/message" => {
            let message = &data["message"];
            let fact = surface(SurfaceKind::Assistant, message, "assistant", message)?;
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
            let fact = surface(SurfaceKind::ToolResult, &data["message"], "user", data)?;
            tool_result(&data["message"]).map_err(|rejection| seed(seq, rejection))?;
            Ok(fact)
        }
        "request/header" => request_header(seq, data),
        "request/tool-update" => {
            let fact = tool_update(seq, data).ok_or(seed(seq, SeedRejection::ToolUpdateData))?;
            non_surface_marker(event)?;
            Ok(fact)
        }
        "assistant/attempt" => {
            // The coordinate scan proved `turn` and `step` safe counts.
            if !data["stream"].is_array() {
                return Err(seed(seq, SeedRejection::Settlement));
            }
            Ok(Fact::LogOnly)
        }
        _ => {
            non_surface_marker(event)?;
            Ok(Fact::LogOnly)
        }
    }
}

/// `surfaceOpOf`'s refusal of either marker on a known type that is not
/// surface-eligible. The codec already refuses both on every such type it
/// knows, so only its opaque known types can reach this check.
fn non_surface_marker(event: &V3CodecEvent<'_>) -> Result<(), ReplayRefusal> {
    let envelope = event.envelope();
    if envelope.surface_op.is_some() || envelope.source_event_seqs.is_some() {
        return Err(seed(envelope.seq, SeedRejection::NonSurfaceMarker));
    }
    Ok(())
}

/// Whether `value` holds -0 anywhere. `snapshotJsonValue` refuses -0 and
/// non-finite numbers in a whole prefix event. The scan refuses a number
/// outside the `f64` range as a native limit, and `float_roundtrip` parsing
/// keeps the sign of every zero, including an underflowing spelling such as
/// `-1e-400`, so for an admitted row this is exactly that refusal. The walk is
/// iterative, so it is safe at any depth the parser produced.
fn holds_negative_zero(value: &Value) -> bool {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Number(number) => {
                if number
                    .as_f64()
                    .is_some_and(|n| n == 0.0 && n.is_sign_negative())
                {
                    return true;
                }
            }
            Value::Array(items) => pending.extend(items),
            Value::Object(fields) => pending.extend(fields.values()),
            Value::Null | Value::Bool(_) | Value::String(_) => {}
        }
    }
    false
}

/// `validateToolUpdateData` over a qualified payload, as a fact.
fn tool_update(seq: u64, data: &Value) -> Option<Fact> {
    let names = |key| {
        let names: Vec<String> = data
            .get(key)?
            .as_array()?
            .iter()
            .map(|name| {
                name.as_str()
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
            })
            .collect::<Option<_>>()?;
        let unique: BTreeSet<&String> = names.iter().collect();
        (unique.len() == names.len()).then_some(names)
    };
    let header_seq = data.get("headerSeq")?.as_u64()?;
    let after_message_id = data.get("afterMessageId")?.as_str()?;
    let additions = names("additions")?;
    let removals = names("removals")?;
    if after_message_id.is_empty()
        || additions.len() + removals.len() == 0
        || additions.iter().any(|name| removals.contains(name))
    {
        return None;
    }
    Some(Fact::ToolUpdate {
        seq,
        header_seq,
        after_message_id: after_message_id.to_owned(),
        additions,
        removals,
    })
}

/// A projected payload's numbers reach request JSON, and JavaScript rounds
/// integers beyond 2^53, so admitting only safe integers keeps them exact;
/// copies and comparisons recurse, so depth is bounded. The walk is
/// iterative, so it is safe at any depth the caller's parser produced. When
/// one payload holds both an unqualified number and excess depth, which limit
/// is reported depends on member order and is not specified; neither claims
/// anything.
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
        return Err(limit(seq, ReplayLimit::ConfigMember));
    }
    let tools = match header.get("tools") {
        None => None,
        Some(Value::Array(tools)) if tools.iter().all(Value::is_object) => Some(tools.clone()),
        Some(_) => return Err(limit(seq, ReplayLimit::ToolSchema)),
    };
    Ok(Fact::Header {
        seq,
        config: config.clone(),
        tools,
        resets: data["reason"] == "series" || data.get("startsSeries").is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventRejection, V3Rejection, V3RowRefusal, decode_v3_row};

    const SURFACE_TYPES: [&str; 4] = [
        "system/message",
        "user/message",
        "assistant/message",
        "tool/result",
    ];
    /// The known non-surface types the codec treats as opaque, so their
    /// markers reach Session construction.
    const CODEC_OPAQUE: [&str; 6] = [
        "deliverables/presented",
        "image/offload",
        "request/tool-update",
        "subagent/catalog",
        "subagent/routing-decision",
        "workspace/changes",
    ];

    #[test]
    fn known_types_equal_the_generated_typescript_list() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/core/session/src/known-event-types.ts"
        );
        let source = std::fs::read_to_string(path).expect("read known-event-types.ts");
        let start = source
            .find("KNOWN_SESSION_EVENT_TYPES: ReadonlySet<string> = new Set([")
            .expect("known list");
        let list = &source[start..start + source[start..].find("])").expect("list end")];
        let names: Vec<&str> = list
            .lines()
            .skip(1)
            .map(|line| line.trim().trim_end_matches(',').trim_matches('\''))
            .collect();
        assert_eq!(names, KNOWN_EVENT_TYPES);
        assert_eq!(names.len(), 60);
    }

    #[test]
    fn the_codec_refuses_markers_on_every_known_type_it_classifies() {
        let mut opaque = Vec::new();
        for name in KNOWN_EVENT_TYPES {
            if SURFACE_TYPES.contains(&name) {
                continue;
            }
            let mut refused = 0;
            for (key, value) in [
                ("surfaceOp", serde_json::json!("append")),
                ("sourceEventSeqs", serde_json::json!([0])),
            ] {
                // Raw-row admission needs a header object before the envelope.
                let data = if name == "request/header" {
                    serde_json::json!({"header": {}})
                } else {
                    serde_json::json!({})
                };
                let mut row = serde_json::json!({"type": name, "seq": 1, "time": 0, "data": data});
                row[key] = value;
                match decode_v3_row(&row, 1, 8) {
                    Err(V3RowRefusal::Rejected(V3Rejection::Event {
                        rejection: EventRejection::UnexpectedFields { keys },
                        ..
                    })) => {
                        assert_eq!(keys, [key], "{name}");
                        refused += 1;
                    }
                    Ok(_) => {}
                    other => panic!("{name} with {key}: {other:?}"),
                }
            }
            match refused {
                2 => {}
                0 => opaque.push(name),
                _ => panic!("{name}: one marker refused"),
            }
        }
        assert_eq!(opaque, CODEC_OPAQUE);
    }
}
