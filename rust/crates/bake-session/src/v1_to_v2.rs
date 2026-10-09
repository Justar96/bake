//! The released v1→v2 migration's transformed stage over a decoded v1
//! Session, as `sessionFormatV1ToV2` in
//! `packages/session/session-format-v1-to-v2/src/migration.ts` runs it:
//! `migrateHeader`, `assertReleasedV2Header`, then the stage that
//! `createStage({ sourceKind: 'transformed' })` builds, with `transformEvent`
//! for each decoded event and then `finish`.
//!
//! This is the stage a chain runs after v0→v1, not the one production runs
//! on a directly decoded v1 file: that stage first checks each payload with
//! `assertReleasedEventPayload`, which [`crate::migrate_v1_to_v2_decoded`]
//! adds. The input
//! is unvalidated codec output, so this port follows the stage's unchecked
//! casts and refuses with a [`V1ToV2Limit`] wherever TypeScript would throw a
//! `TypeError` or coerce a value.
//!
//! [`migrate_v1_to_v2_transformed`] feeds every decoded event, packed rows
//! expanded, to `transformEvent`, so a log with packed rows matches
//! TypeScript's `transformEvent` over its expanded events.
//! [`migrate_v1_to_v2_transformed_items`] feeds each packed row to
//! `transformRun` instead, as a chain reading the file does. That path skips
//! the event checks, forgets the previous event, takes the run's last time,
//! and checks the cut only across the run's own seqs, so its result can
//! differ from the expanded one. A chain feeds the stage's output to v2→v3
//! as it is emitted, so the crate-private `stream_v1_to_v2_items` also
//! keeps the output emitted before `finish` and whether an attempt was still
//! pending then.
//!
//! An attempt's stream is a list of records with their last times. Each
//! `assistant/chunk` event goes through the attempt's
//! `AssistantStreamAccumulator`, ported in [`crate::assistant_stream`], which
//! is flushed into the list before a run's record is appended and when the
//! attempt or its message is emitted. `appendStreamRecord` merges a record
//! into the list's last one on the same type, index, tool id, and tool name,
//! across a safe gap. A run's record never carries `-0`: the codec refuses it
//! in `time0` and `dt`, and every other number in it is a count.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::assistant_stream::{AssistantStreamAccumulator, StreamPushError, safe_gap};
use crate::json_parse::{
    Deep, clone_fields, clone_value, dismantle, remove_member, replace_member,
};
use crate::json_text::{is_writer_spelling, json_number_text};
use crate::v1_codec::{
    DecodedV1Items, DecodedV1Rows, PackedKind, PackedStreamRecord, ReleasedChunkRun, V1Item,
};
use crate::v2_to_v3::integer_string;

/// `CHUNK_EVENT_REQUIRED` and `CHUNK_EVENT_OPTIONAL`.
const CHUNK_EVENT_REQUIRED: [&str; 4] = ["type", "seq", "time", "data"];
const CHUNK_EVENT_OPTIONAL: [&str; 3] = ["ignorable", "sourceEventSeqs", "surfaceOp"];

/// The own keys of `RELEASED_V0_EVENT_DISPOSITIONS` in
/// `packages/session/session-format-v0-to-v1/src/dispositions.ts`. Only
/// membership is read here.
const RELEASED_V0_EVENT_TYPES: [&str; 51] = [
    "agent-preset/selected",
    "agent/inbox/spliced",
    "approval/asked",
    "approval/decided",
    "approval/policy",
    "assistant/chunk",
    "assistant/message",
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
    "session-log-deepseek/delivery-accepted",
    "session/end-seed",
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
];

/// `Object.getOwnPropertyNames(Object.prototype)`: the frozen literal
/// inherits these, so a lookup of one is defined.
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

/// A v1 Session migrated to v2 by the transformed stage.
pub struct MigratedV1ToV2 {
    /// The logical v2 header: the v1 header with `version` 2 in place.
    pub header: Value,
    /// Target events in order, with remapped references.
    pub events: Vec<Value>,
    /// The number of target events inherited from the parent Session.
    pub inherited_event_count: u64,
}

crate::json_parse::deep_session_parts!(MigratedV1ToV2);

/// Where a refusal was raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V1ToV2Location {
    /// `migrateHeader`, before any event.
    Header,
    /// `transformEvent` for the decoded event at this index, or, from
    /// [`migrate_v1_to_v2_transformed_items`], `transformEvent` or
    /// `transformRun` for the item at this index.
    Event(usize),
    /// The stage's `finish`, after every event.
    Finish,
}

/// Why a decoded v1 Session was not migrated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V1ToV2Refusal {
    /// TypeScript throws at `location` with exactly `message`: a
    /// `SessionFormatError` from the header check, otherwise a
    /// `SessionFormatUnsupportedMigrationError` from the stage.
    Rejected {
        location: V1ToV2Location,
        message: String,
    },
    /// This crate does not decide the TypeScript outcome at `location`, the
    /// step that reads the value; nothing is claimed. A limit can hide a
    /// later TypeScript refusal, never an earlier one.
    NativeSubset {
        location: V1ToV2Location,
        limit: V1ToV2Limit,
    },
}

/// Input whose TypeScript outcome this port does not reproduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V1ToV2Limit {
    /// An `assistant/chunk` whose `time` or `data.chunk` the stream
    /// accumulator refuses: TypeScript throws a `TypeError`, or an `Error`
    /// from `assertNever` for an unknown chunk type.
    ChunkShape,
    /// An event `type` that is not a string, which the vocabulary lookup
    /// converts to a property key.
    NonStringType,
    /// A value the stage casts without checking and then reads as an object
    /// member, spreads, maps as an array, or adds to or prints as a number,
    /// where it is not of that kind: JavaScript throws a `TypeError` or
    /// coerces it. This includes an attempt coordinate that is an object or
    /// array, which `!==` compares by reference.
    UncheckedShape,
    /// A number spelled with a fraction or exponent, or beyond `u64`, where
    /// TypeScript compares or adds to it, including a chunk's `time` or
    /// `index`, or a spelling no writer produces, such as `3.0`, that
    /// TypeScript looks up as a seq. Refusal messages print every number as
    /// `JSON.stringify` does, so printing never raises it.
    FloatLexeme,
    /// An emitted event would carry a member whose value is `undefined`,
    /// which JSON cannot express: a synthesized event's `time` from an event
    /// without one, a passed-through event without `data`, or an attempt
    /// whose first chunk had no `turn` or `step`.
    UndefinedMember,
}

impl V1ToV2Limit {
    /// The limit's name in `conformance/session/v1-to-v2-cases.json`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::ChunkShape => "chunk-shape",
            Self::NonStringType => "non-string-type",
            Self::UncheckedShape => "unchecked-shape",
            Self::FloatLexeme => "float-lexeme",
            Self::UndefinedMember => "undefined-member",
        }
    }
}

enum Failure {
    Unsupported(String),
    Limit(V1ToV2Limit),
}

type Checked<T> = Result<T, Failure>;

/// Migrate a decoded v1 Session to v2 as the released transformed stage does.
///
/// `decoded` must be [`decode_v0_v1_rows`](crate::decode_v0_v1_rows) output:
/// each event's `seq` is its index, its `sourceEventSeqs`, when present, is
/// an expanded list of safe counts, and no event retains an unsafe integer.
/// A decoded v0 header is refused as `migrateHeader` refuses it; every other
/// header member was already checked by the codec.
pub fn migrate_v1_to_v2_transformed(
    decoded: &DecodedV1Rows,
) -> Result<MigratedV1ToV2, V1ToV2Refusal> {
    let items = decoded.events.iter().map(Source::Event);
    migrate(&decoded.header, decoded.inherited_event_count, items)
}

/// Migrate a decoded v1 Session to v2 as the released transformed stage does
/// when a chain reads the file: `transformEvent` for each event item and
/// `transformRun` for each packed row.
///
/// `decoded` must be [`decode_v0_v1_items`](crate::decode_v0_v1_items)
/// output, with the same guarantees as for
/// [`migrate_v1_to_v2_transformed`].
pub fn migrate_v1_to_v2_transformed_items(
    decoded: &DecodedV1Items,
) -> Result<MigratedV1ToV2, V1ToV2Refusal> {
    let streamed = stream_v1_to_v2_items(decoded)?;
    let inherited_event_count = streamed.finished?;
    Ok(MigratedV1ToV2 {
        header: streamed.header,
        events: streamed.events.into_inner(),
        inherited_event_count,
    })
}

/// [`migrate_v1_to_v2_transformed_items`] with what it streamed before
/// `finish`, for a caller that feeds the output to a later stage as
/// TypeScript's chain does.
pub(crate) struct StreamedV1ToV2 {
    /// The logical v2 header.
    pub(crate) header: Value,
    /// Every event the stage emitted: the first `streamed_len` before
    /// `finish`, then what `finish` emitted, up to a refusal there.
    pub(crate) events: Deep<Vec<Value>>,
    /// The number of events emitted before `finish`.
    pub(crate) streamed_len: usize,
    /// Whether an Assistant attempt, with any events buffered after its last
    /// chunk, was still pending after the last item: the next item or
    /// `finish` may emit it before refusing.
    pub(crate) pending: bool,
    /// `finish`: the inherited cut, or its refusal at
    /// [`V1ToV2Location::Finish`].
    pub(crate) finished: Result<u64, V1ToV2Refusal>,
}

/// The transformed stage over `decoded`'s items, as
/// [`migrate_v1_to_v2_transformed_items`] runs it, keeping what was streamed
/// before `finish` and whether `finish` refused. A header or item refusal is
/// returned as is.
pub(crate) fn stream_v1_to_v2_items(
    decoded: &DecodedV1Items,
) -> Result<StreamedV1ToV2, V1ToV2Refusal> {
    let items = decoded.items.iter().map(|item| match item {
        V1Item::Event(event) => Source::Event(event),
        V1Item::AssistantChunkRun(run) => Source::Run(run),
    });
    stream(&decoded.header, decoded.inherited_event_count, items)
}

/// One input to the stage.
enum Source<'a> {
    Event(&'a Value),
    Run(&'a ReleasedChunkRun),
}

fn migrate<'a>(
    header: &'a Value,
    inherited_event_count: u64,
    items: impl Iterator<Item = Source<'a>>,
) -> Result<MigratedV1ToV2, V1ToV2Refusal> {
    let streamed = stream(header, inherited_event_count, items)?;
    let inherited_event_count = streamed.finished?;
    Ok(MigratedV1ToV2 {
        header: streamed.header,
        events: streamed.events.into_inner(),
        inherited_event_count,
    })
}

fn stream<'a>(
    header: &'a Value,
    inherited_event_count: u64,
    items: impl Iterator<Item = Source<'a>>,
) -> Result<StreamedV1ToV2, V1ToV2Refusal> {
    let header = match header {
        Value::Object(fields) if fields.get("version").and_then(Value::as_u64) == Some(1) => fields,
        _ => {
            return Err(V1ToV2Refusal::Rejected {
                location: V1ToV2Location::Header,
                message: "expected format v1 header".to_owned(),
            });
        }
    };
    let mut target_header = clone_fields(header);
    target_header.insert("version".to_owned(), Value::from(2));
    let mut stage = Stage::new(header, inherited_event_count);
    // The decoded seq of the next event: the codec checked that each event's
    // `seq` and each run's `firstSeq` is the count of events before it.
    let mut seq: u64 = 0;
    for (index, item) in items.enumerate() {
        match item {
            Source::Event(event) => {
                let result = stage.transform(seq, event);
                seq = seq.saturating_add(1);
                result
            }
            Source::Run(run) => {
                seq = seq.saturating_add(run.event_count());
                stage.transform_run(run)
            }
        }
        .map_err(|failure| refusal(V1ToV2Location::Event(index), failure))?;
    }
    let streamed_len = stage.output.len();
    let pending = stage.pending.is_some();
    let finished = stage
        .finish()
        .map_err(|failure| refusal(V1ToV2Location::Finish, failure));
    Ok(StreamedV1ToV2 {
        header: Value::Object(target_header),
        events: stage.output,
        streamed_len,
        pending,
        finished,
    })
}

fn refusal(location: V1ToV2Location, failure: Failure) -> V1ToV2Refusal {
    match failure {
        Failure::Unsupported(message) => V1ToV2Refusal::Rejected { location, message },
        Failure::Limit(limit) => V1ToV2Refusal::NativeSubset { location, limit },
    }
}

/// The open turn as `LegacyTurnState.openTurn` holds it.
enum OpenTurn {
    /// `null`.
    Closed,
    /// The `turn` member of the opening `turn/start`, `None` when absent.
    Open(Deep<Option<Value>>),
}

/// `StreamingAttempt` and its `AttemptGroup`: the chunks of one Assistant
/// attempt so far, and the events buffered after its last chunk.
struct PendingAttempt {
    /// `turn` and `step` of the attempt's first chunk, `None` when absent.
    turn: Deep<Option<Value>>,
    step: Deep<Option<Value>>,
    /// `spans` as `(firstSeq, eventCount)`.
    spans: Vec<(u64, u64)>,
    /// `stream`: the records flushed so far.
    stream: Vec<StreamEntry>,
    /// The accumulator for chunk events since the last flush.
    accumulator: Option<AssistantStreamAccumulator>,
    chunk_count: u64,
    last_chunk_seq: u64,
    /// The last chunk's `time`, a safe integer the accumulator admitted or
    /// a run's `lastTime`.
    last_chunk_time: Deep<Value>,
    terminal: bool,
    /// `afterLastChunk`: each buffered event's index and members.
    after_last_chunk: Deep<Vec<(u64, Map<String, Value>)>>,
}

impl PendingAttempt {
    /// `attemptGroup(turn, step)` with no chunk yet.
    fn new(turn: Option<Value>, step: Option<Value>, seq: u64) -> Self {
        Self {
            turn: Deep::new(turn),
            step: Deep::new(step),
            spans: Vec::new(),
            stream: Vec::new(),
            accumulator: None,
            chunk_count: 0,
            last_chunk_seq: seq,
            last_chunk_time: Deep::new(Value::Null),
            terminal: false,
            after_last_chunk: Deep::default(),
        }
    }

    /// `assertAttemptCut`: an attempt's chunks and its message must all
    /// precede the source cut or all follow it.
    fn assert_cut(&self, source_cut: u64, member: u64) -> Checked<()> {
        let first = self.spans.first().map_or(member, |(first, _)| *first);
        if (first < source_cut) != (member < source_cut) {
            return Err(Failure::Unsupported(format!(
                "inherited Session cut {source_cut} splits one Assistant attempt"
            )));
        }
        Ok(())
    }

    /// `recordChunkSpan`: `event_count` chunks from `first_seq`, the last at
    /// `time`.
    fn record_span(&mut self, first_seq: u64, event_count: u64, time: Value) {
        match self.spans.last_mut() {
            Some((first, count)) if first.checked_add(*count) == Some(first_seq) => {
                *count = count.saturating_add(event_count);
            }
            _ => self.spans.push((first_seq, event_count)),
        }
        self.chunk_count = self.chunk_count.saturating_add(event_count);
        self.last_chunk_seq = first_seq.saturating_add(event_count).saturating_sub(1);
        self.last_chunk_time = Deep::new(time);
    }

    /// `flushAccumulator`: append the accumulator's records to the stream.
    fn flush_accumulator(&mut self) {
        let Some(accumulator) = self.accumulator.take() else {
            return;
        };
        for record in accumulator.snapshot() {
            append_stream_record(&mut self.stream, StreamEntry::from_snapshot(record));
        }
    }

    /// `streamOf`: flush, then the stream's records.
    fn take_stream(&mut self) -> Vec<Value> {
        self.flush_accumulator();
        std::mem::take(&mut self.stream)
            .into_iter()
            .map(StreamEntry::into_value)
            .collect()
    }

    /// `matchesChunkSources`: the message cites exactly this attempt's
    /// chunks, in order.
    fn matches_sources(&self, sources: &[Value]) -> bool {
        if sources.len() as u64 != self.chunk_count {
            return false;
        }
        let mut cited = sources.iter();
        self.spans.iter().all(|(first, count)| {
            (0..*count)
                .all(|offset| cited.next().and_then(Value::as_u64) == first.checked_add(offset))
        })
    }

    /// `attemptEvent`: the `assistant/attempt` at the last chunk's seq and time.
    fn attempt_event(&mut self) -> Checked<Map<String, Value>> {
        // Decide before copying: a dropped copy of a deep coordinate recurses.
        let (Some(turn), Some(step)) = (self.turn.as_ref(), self.step.as_ref()) else {
            return Err(Failure::Limit(V1ToV2Limit::UndefinedMember));
        };
        let (turn, step) = (clone_value(turn), clone_value(step));
        let mut data = Map::new();
        data.insert("turn".to_owned(), turn);
        data.insert("step".to_owned(), step);
        data.insert("stream".to_owned(), Value::Array(self.take_stream()));
        generated(
            "assistant/attempt",
            self.last_chunk_seq,
            Some(&self.last_chunk_time),
            Value::Object(data),
        )
    }
}

/// One stream record and its last time, as `AttemptGroup.stream` holds it.
enum StreamEntry {
    /// A text, reasoning, or tool-call record, which a later one can merge into.
    Packed {
        record: PackedStreamRecord,
        last_time: i64,
    },
    /// A raw `chunk` record, which never merges; its chunk is unchecked log
    /// JSON of any depth.
    Chunk(Deep<Value>),
}

impl StreamEntry {
    /// An accumulator snapshot record with `recordLastTime`. Anything that
    /// is not a packed record is the accumulator's raw `chunk` record.
    fn from_snapshot(record: Value) -> Self {
        let Value::Object(fields) = &record else {
            return Self::Chunk(Deep::new(record));
        };
        let kind = match fields.get("type").and_then(Value::as_str) {
            Some("text-chunks") => PackedKind::Text,
            Some("reasoning-chunks") => PackedKind::Reasoning,
            Some("tool-call-chunks") => PackedKind::ToolCall,
            _ => return Self::Chunk(Deep::new(record)),
        };
        let strings = |key: &str| -> Option<Vec<String>> {
            fields
                .get(key)?
                .as_array()?
                .iter()
                .map(|item| item.as_str().map(str::to_owned))
                .collect()
        };
        let integers = |key: &str| -> Option<Vec<i64>> {
            fields
                .get(key)?
                .as_array()?
                .iter()
                .map(Value::as_i64)
                .collect()
        };
        let packed = (|| {
            let time0 = fields.get("time0")?.as_i64()?;
            let dt = integers("dt")?;
            let last_time = dt
                .iter()
                .try_fold(time0, |time, gap| time.checked_add(*gap))?;
            let (id, members) = if kind == PackedKind::ToolCall {
                (
                    Some(fields.get("id")?.as_str()?.to_owned()),
                    strings("args")?,
                )
            } else {
                (None, strings("texts")?)
            };
            let name = match fields.get("name") {
                None => None,
                Some(name) => Some(name.as_str()?.to_owned()),
            };
            let record = PackedStreamRecord {
                kind,
                time0,
                index: fields.get("index")?.as_u64()?,
                dt,
                id,
                name,
                members,
            };
            Some(Self::Packed { record, last_time })
        })();
        packed.unwrap_or_else(|| Self::Chunk(Deep::new(record)))
    }

    fn into_value(self) -> Value {
        match self {
            Self::Packed { record, .. } => record.to_value(),
            Self::Chunk(record) => record.into_inner(),
        }
    }
}

/// `appendStreamRecord`: merge `source` into the stream's last record when
/// both are packed records of one type, index, tool id, and tool name, and
/// the gap between them is a safe integer; otherwise push it.
fn append_stream_record(stream: &mut Vec<StreamEntry>, source: StreamEntry) {
    let StreamEntry::Packed {
        record: source,
        last_time,
    } = source
    else {
        stream.push(source);
        return;
    };
    if let Some(StreamEntry::Packed {
        record: target,
        last_time: target_last_time,
    }) = stream.last_mut()
        && target.kind == source.kind
        && target.index == source.index
        && let Some(gap) = safe_gap(*target_last_time, source.time0)
        && target.id == source.id
        && target.name == source.name
    {
        target.dt.push(gap);
        target.dt.extend(source.dt);
        target.members.extend(source.members);
        *target_last_time = last_time;
        return;
    }
    stream.push(StreamEntry::Packed {
        record: source,
        last_time,
    });
}

/// `ReleasedV1ToV2State`.
struct Stage<'a> {
    header: &'a Map<String, Value>,
    is_seeded: bool,
    source_cut: u64,
    mapping: HashMap<u64, u64>,
    open_turn: OpenTurn,
    /// Whether `openStep` is not `null`.
    open_step: bool,
    /// `legacyTurns.previous`: the last observed event, forgotten by a run.
    previous: Option<&'a Value>,
    target_seq: u64,
    target_cut: Option<u64>,
    /// `lastTime`, `None` when an event had no `time`.
    last_time: Deep<Option<Value>>,
    pending: Option<PendingAttempt>,
    output: Deep<Vec<Value>>,
}

impl<'a> Stage<'a> {
    fn new(header: &'a Map<String, Value>, source_cut: u64) -> Self {
        let is_seeded = header.get("isSeeded") == Some(&Value::Bool(true));
        Self {
            header,
            is_seeded,
            source_cut,
            mapping: HashMap::new(),
            open_turn: OpenTurn::Closed,
            open_step: false,
            previous: None,
            target_seq: 0,
            target_cut: if is_seeded { None } else { Some(0) },
            last_time: Deep::new(header.get("createdAt").map(clone_value)),
            pending: None,
            output: Deep::default(),
        }
    }

    /// `transformReleasedEvent`.
    fn transform(&mut self, seq: u64, event: &'a Value) -> Checked<()> {
        let fields = record(Some(event))?;
        let raw_type = fields.get("type");
        if raw_type == Some(&Value::from("assistant/chunk")) {
            assert_chunk_envelope(fields, seq)?;
        }
        let Some(Value::String(event_type)) = raw_type else {
            return Err(Failure::Limit(V1ToV2Limit::NonStringType));
        };
        let event_type = event_type.as_str();
        if !RELEASED_V0_EVENT_TYPES.contains(&event_type)
            && !OBJECT_PROTOTYPE_NAMES.contains(&event_type)
        {
            return Err(Failure::Unsupported(format!(
                "format v1 contains unknown event type {} at seq {seq}",
                Value::from(event_type)
            )));
        }
        let interrupted = self
            .legacy_interrupted_turn(event_type, fields, seq)?
            .map(Deep::new);
        if event_type == "turn/start"
            && matches!(self.open_turn, OpenTurn::Open(_))
            && interrupted.is_none()
        {
            let turn = record(fields.get("data"))?.get("turn");
            return Err(Failure::Unsupported(format!(
                "turn/start {} does not close the prior turn",
                stringify(turn)
            )));
        }
        self.assert_source_delivery_marker(event_type, fields, seq)?;
        self.observe_legacy_turn(event_type, fields, event)?;
        self.last_time = Deep::new(fields.get("time").map(clone_value));
        if let Some(interrupted) = interrupted {
            self.finish_attempt()?;
            self.emit_generated(seq, interrupted.into_inner())?;
        }
        if let Some(LegacyGoalSplit { change, message }) =
            split_legacy_goal_change(event_type, fields, seq)?
        {
            self.emit_generated(seq, change.into_inner())?;
            return self.emit_source(seq, message.into_inner(), fields.get("time"));
        }
        match event_type {
            "assistant/chunk" => self.transform_chunk(fields, seq),
            "assistant/message" => self.transform_message(fields, seq),
            // `closesAttempt`.
            "turn/end" | "step/end" | "llm/retry" | "llm/retry-started" => {
                self.finish_attempt()?;
                self.emit_source(seq, clone_fields(fields), fields.get("time"))
            }
            _ => match &mut self.pending {
                Some(pending) => {
                    pending.after_last_chunk.push((seq, clone_fields(fields)));
                    Ok(())
                }
                None => self.emit_source(seq, clone_fields(fields), fields.get("time")),
            },
        }
    }

    /// `transformChunk`.
    fn transform_chunk(&mut self, fields: &Map<String, Value>, seq: u64) -> Checked<()> {
        let data = record(fields.get("data"))?;
        let turn = data.get("turn");
        let step = data.get("step");
        if let Some(pending) = &self.pending {
            let continues = !pending.terminal
                && same_coordinate(pending.turn.as_ref(), turn)?
                && same_coordinate(pending.step.as_ref(), step)?;
            if continues {
                self.flush_buffered()?;
            } else {
                self.finish_attempt()?;
            }
        }
        let source_cut = self.source_cut;
        let pending = self.pending.get_or_insert_with(|| {
            PendingAttempt::new(turn.map(clone_value), step.map(clone_value), seq)
        });
        pending.assert_cut(source_cut, seq)?;
        let chunk = data.get("chunk");
        pending
            .accumulator
            .get_or_insert_default()
            .push(fields.get("time"), chunk)
            .map_err(|error| {
                Failure::Limit(match error {
                    StreamPushError::ChunkShape => V1ToV2Limit::ChunkShape,
                    StreamPushError::FloatLexeme => V1ToV2Limit::FloatLexeme,
                })
            })?;
        // `push` admitted the time, so it is present.
        pending.record_span(seq, 1, fields.get("time").map_or(Value::Null, clone_value));
        if chunk.and_then(|chunk| chunk.get("type")) == Some(&Value::from("finish")) {
            pending.terminal = true;
        }
        Ok(())
    }

    /// `transformReleasedRun` for a packed Assistant chunk row.
    fn transform_run(&mut self, run: &ReleasedChunkRun) -> Checked<()> {
        self.previous = None;
        self.last_time = Deep::new(Some(Value::from(run.last_time())));
        let turn = Value::from(run.turn());
        let step = Value::from(run.step());
        if let Some(pending) = &self.pending {
            let continues = !pending.terminal
                && same_coordinate(pending.turn.as_ref(), Some(&turn))?
                && same_coordinate(pending.step.as_ref(), Some(&step))?;
            if continues {
                self.flush_buffered()?;
            } else {
                self.finish_attempt()?;
            }
        }
        let first_seq = run.first_seq();
        let pending = self
            .pending
            .get_or_insert_with(|| PendingAttempt::new(Some(turn), Some(step), first_seq));
        // `assertAttemptRange`: only the run's own seqs are checked.
        if (first_seq < self.source_cut) != (run.last_seq() < self.source_cut) {
            return Err(Failure::Unsupported(format!(
                "inherited Session cut {} splits one Assistant attempt",
                self.source_cut
            )));
        }
        pending.flush_accumulator();
        append_stream_record(
            &mut pending.stream,
            StreamEntry::Packed {
                record: run.record.clone(),
                last_time: run.last_time(),
            },
        );
        pending.record_span(first_seq, run.event_count(), Value::from(run.last_time()));
        Ok(())
    }

    /// `finishAttempt`: emit the pending attempt, then the events buffered
    /// after its last chunk.
    fn finish_attempt(&mut self) -> Checked<()> {
        let Some(mut pending) = self.pending.take() else {
            return Ok(());
        };
        let attempt = pending.attempt_event()?;
        self.emit_generated(pending.last_chunk_seq, attempt)?;
        self.emit_buffered(pending.after_last_chunk)
    }

    /// `flushBuffered` for the attempt that stays pending.
    fn flush_buffered(&mut self) -> Checked<()> {
        let buffered = match &mut self.pending {
            Some(pending) => std::mem::take(&mut pending.after_last_chunk),
            None => return Ok(()),
        };
        self.emit_buffered(buffered)
    }

    fn emit_buffered(&mut self, mut buffered: Deep<Vec<(u64, Map<String, Value>)>>) -> Checked<()> {
        buffered.reverse();
        while let Some((seq, fields)) = buffered.pop() {
            let time = Deep::new(fields.get("time").map(clone_value));
            self.emit_source(seq, fields, time.as_ref())?;
        }
        Ok(())
    }

    /// `legacyInterruptedTurn`: the `turn/end` a released next-turn splice
    /// left implicit.
    fn legacy_interrupted_turn(
        &self,
        event_type: &str,
        fields: &Map<String, Value>,
        seq: u64,
    ) -> Checked<Option<Map<String, Value>>> {
        let OpenTurn::Open(open_turn) = &self.open_turn else {
            return Ok(None);
        };
        if event_type != "turn/start" || self.open_step {
            return Ok(None);
        }
        let turn = record(fields.get("data"))?.get("turn");
        // `openTurn + 1` adds a number only when the open turn is one.
        let next = match &**open_turn {
            Some(Value::Number(number)) => match (number.as_u64(), number.as_i64()) {
                (Some(value), _) => i128::from(value) + 1,
                (None, Some(value)) => i128::from(value) + 1,
                (None, None) => return Err(Failure::Limit(V1ToV2Limit::FloatLexeme)),
            },
            _ => return Err(Failure::Limit(V1ToV2Limit::UncheckedShape)),
        };
        let matches = match turn {
            Some(Value::Number(number)) => match (number.as_u64(), number.as_i64()) {
                (Some(value), _) => i128::from(value) == next,
                (None, Some(value)) => i128::from(value) == next,
                (None, None) => return Err(Failure::Limit(V1ToV2Limit::FloatLexeme)),
            },
            // `!==` with a number is true for every other value.
            _ => false,
        };
        let Some(previous) = self.previous.and_then(Value::as_object) else {
            return Ok(None);
        };
        if !matches || previous.get("type") != Some(&Value::from("agent/inbox/spliced")) {
            return Ok(None);
        }
        let splice = record(previous.get("data"))?;
        if splice.get("target") != Some(&Value::from("next-turn")) {
            return Ok(None);
        }
        match splice.get("inserted") {
            Some(Value::Array(inserted)) if !inserted.is_empty() => {}
            _ => return Ok(None),
        }
        let mut reason = Map::new();
        reason.insert("kind".to_owned(), Value::from("interrupted"));
        let mut data = Map::new();
        data.insert(
            "turn".to_owned(),
            open_turn.as_ref().map_or(Value::Null, clone_value),
        );
        data.insert("reason".to_owned(), Value::Object(reason));
        Ok(Some(generated(
            "turn/end",
            seq,
            fields.get("time"),
            Value::Object(data),
        )?))
    }

    /// `assertSourceDeliveryMarker`.
    fn assert_source_delivery_marker(
        &self,
        event_type: &str,
        fields: &Map<String, Value>,
        seq: u64,
    ) -> Checked<()> {
        if event_type != "session-log-deepseek/delivery-accepted" {
            return Ok(());
        }
        let data = record(fields.get("data"))?;
        let inherited = self.header.contains_key("parentSession") && seq < self.source_cut;
        let current = match data.get("sessionFormatVersion") {
            Some(Value::Number(number)) if number.is_u64() || number.is_i64() => {
                number.as_u64() == Some(1)
            }
            Some(Value::Number(_)) => return Err(Failure::Limit(V1ToV2Limit::FloatLexeme)),
            _ => false,
        };
        if current && !inherited && data.get("sessionId") != self.header.get("id") {
            return Err(Failure::Unsupported(
                "current-generation delivery marker names the wrong Session".to_owned(),
            ));
        }
        Ok(())
    }

    /// `observeLegacyTurn`.
    fn observe_legacy_turn(
        &mut self,
        event_type: &str,
        fields: &Map<String, Value>,
        event: &'a Value,
    ) -> Checked<()> {
        match event_type {
            "turn/start" => {
                self.open_turn = match record(fields.get("data"))?.get("turn") {
                    Some(Value::Null) => OpenTurn::Closed,
                    turn => OpenTurn::Open(Deep::new(turn.map(clone_value))),
                };
                self.open_step = false;
            }
            "turn/end" => {
                self.open_turn = OpenTurn::Closed;
                self.open_step = false;
            }
            "step/start" => {
                self.open_step = record(fields.get("data"))?.get("step") != Some(&Value::Null);
            }
            "step/end" => self.open_step = false,
            _ => {}
        }
        self.previous = Some(event);
        Ok(())
    }

    /// `transformMessage`.
    fn transform_message(&mut self, fields: &Map<String, Value>, seq: u64) -> Checked<()> {
        let data = record(fields.get("data"))?;
        let time = fields.get("time");
        if let Some(pending) = &self.pending
            && !(same_coordinate(pending.turn.as_ref(), data.get("turn"))?
                && same_coordinate(pending.step.as_ref(), data.get("step"))?)
        {
            self.finish_attempt()?;
            return self.emit_source(seq, message_event(fields, data, Vec::new()), time);
        }
        let sources = match fields.get("sourceEventSeqs") {
            Some(Value::Array(sources)) => sources,
            _ => {
                if self.pending.is_some() {
                    return Err(Failure::Unsupported(format!(
                        "assistant/message {seq} does not cite its complete v1 chunk attempt"
                    )));
                }
                return self.emit_source(seq, message_event(fields, data, Vec::new()), time);
            }
        };
        if sources.is_empty() {
            self.finish_attempt()?;
            return self.emit_source(seq, message_event(fields, data, Vec::new()), time);
        }
        let Some(mut pending) = self
            .pending
            .take_if(|pending| pending.matches_sources(sources))
        else {
            return Err(Failure::Unsupported(format!(
                "assistant/message {seq} chunk references are not one complete ordered attempt"
            )));
        };
        pending.assert_cut(self.source_cut, seq)?;
        let stream = Deep::new(pending.take_stream());
        self.emit_buffered(pending.after_last_chunk)?;
        self.emit_source(seq, message_event(fields, data, stream.into_inner()), time)
    }

    /// `emitSource`. `time` is the source event's own.
    fn emit_source(
        &mut self,
        seq: u64,
        source: Map<String, Value>,
        time: Option<&Value>,
    ) -> Checked<()> {
        let mut source = Deep::new(source);
        let event_type = source
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if self.is_seeded && seq == self.source_cut && event_type == "session/end-seed" {
            replace_member(&mut source, "data", inherited_marker());
        }
        self.ensure_target_cut(seq, time, &event_type)?;
        self.mapping.insert(seq, self.target_seq);
        let event = self.remap_references(source.into_inner(), &event_type, seq)?;
        self.output.push(event);
        self.target_seq += 1;
        Ok(())
    }

    /// `emitGenerated`.
    fn emit_generated(&mut self, origin: u64, event: Map<String, Value>) -> Checked<()> {
        let event = Deep::new(event);
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        self.ensure_target_cut(origin, event.get("time"), &event_type)?;
        let event = self.remap_references(event.into_inner(), &event_type, origin)?;
        self.output.push(event);
        self.target_seq += 1;
        Ok(())
    }

    /// `ensureTargetCut`: the first event at or after the source cut gets
    /// an inherited end-seed before it, unless it is the source's own.
    fn ensure_target_cut(
        &mut self,
        origin: u64,
        time: Option<&Value>,
        event_type: &str,
    ) -> Checked<()> {
        if !self.is_seeded || self.target_cut.is_some() || origin < self.source_cut {
            return Ok(());
        }
        self.target_cut = Some(self.target_seq);
        if origin == self.source_cut && event_type == "session/end-seed" {
            return Ok(());
        }
        let marker = generated(
            "session/end-seed",
            self.target_seq,
            time,
            inherited_marker(),
        )?;
        self.output.push(Value::Object(marker));
        self.target_seq += 1;
        Ok(())
    }

    /// `finishMigration`.
    fn finish(&mut self) -> Checked<u64> {
        self.finish_attempt()?;
        if let Some(cut) = self.target_cut {
            return Ok(cut);
        }
        let cut = self.target_seq;
        self.target_cut = Some(cut);
        let marker = generated(
            "session/end-seed",
            cut,
            self.last_time.as_ref(),
            inherited_marker(),
        )?;
        self.output.push(Value::Object(marker));
        self.target_seq += 1;
        Ok(cut)
    }

    /// `remapReferences`: the event at `target_seq`, with `sourceEventSeqs`
    /// and `surfaceOp` mapped and moved last. `seq` labels refusals.
    fn remap_references(
        &self,
        event: Map<String, Value>,
        event_type: &str,
        seq: u64,
    ) -> Checked<Value> {
        let mut event = Deep::new(event);
        let sources = Deep::new(event.shift_remove("sourceEventSeqs"));
        let surface = Deep::new(event.shift_remove("surfaceOp"));
        let sources = match &*sources {
            None => None,
            Some(sources) => {
                Some(self.map_list(Some(sources), &format!("{event_type} {seq} sources"))?)
            }
        };
        let operation = match surface.into_inner() {
            None => None,
            Some(Value::String(text)) if text == "append" => Some(Value::String(text)),
            Some(Value::Object(replacement)) => {
                let replacement = Deep::new(replacement);
                let start = self.map_one(
                    replacement.get("start"),
                    &format!("{event_type} {seq} surface start"),
                )?;
                let end = self.map_one(
                    replacement.get("end"),
                    &format!("{event_type} {seq} surface end"),
                )?;
                let mut operation = Map::new();
                operation.insert("op".to_owned(), Value::from("replace"));
                operation.insert("start".to_owned(), Value::from(start));
                operation.insert("end".to_owned(), Value::from(end));
                Some(Value::Object(operation))
            }
            Some(other) => {
                dismantle(other);
                return Err(Failure::Limit(V1ToV2Limit::UncheckedShape));
            }
        };
        let data = self.remap_payload_references(event_type, seq, event.get("data"))?;
        event.insert("seq".to_owned(), Value::from(self.target_seq));
        let Some(data) = data else {
            return Err(Failure::Limit(V1ToV2Limit::UndefinedMember));
        };
        replace_member(&mut event, "data", data);
        if let Some(sources) = sources {
            event.insert("sourceEventSeqs".to_owned(), sources);
        }
        if let Some(operation) = operation {
            event.insert("surfaceOp".to_owned(), operation);
        }
        Ok(Value::Object(event.into_inner()))
    }

    /// `remapPayloadReferences`. `None` stands for an absent `data`.
    fn remap_payload_references(
        &self,
        event_type: &str,
        seq: u64,
        data: Option<&Value>,
    ) -> Checked<Option<Value>> {
        let mut remapped = match event_type {
            "command/done"
            | "compaction/prune"
            | "compaction/summary"
            | "session/title"
            | "session/title-llm-request" => Deep::new(clone_fields(record(data)?)),
            _ => return Ok(data.map(clone_value)),
        };
        match event_type {
            "command/done" => {
                if let Some(source) = remapped.get("sourceEventSeq") {
                    let mapped =
                        self.map_one(Some(source), &format!("command/done {seq} sourceEventSeq"))?;
                    replace_member(&mut remapped, "sourceEventSeq", Value::from(mapped));
                }
            }
            "compaction/prune" | "compaction/summary" => {
                let range = record(remapped.get("shadowedRange"))?;
                let start = self.map_one(
                    range.get("start"),
                    &format!("{event_type} {seq} shadowedRange start"),
                )?;
                let end = self.map_one(
                    range.get("end"),
                    &format!("{event_type} {seq} shadowedRange end"),
                )?;
                let seqs = self.map_list(
                    remapped.get("shadowedSeqs"),
                    &format!("{event_type} {seq} shadowedSeqs"),
                )?;
                let mut range = Map::new();
                range.insert("start".to_owned(), Value::from(start));
                range.insert("end".to_owned(), Value::from(end));
                replace_member(&mut remapped, "shadowedRange", Value::Object(range));
                replace_member(&mut remapped, "shadowedSeqs", seqs);
            }
            _ => {
                let seqs = self.map_list(
                    remapped.get("messageSeqs"),
                    &format!("{event_type} {seq} messageSeqs"),
                )?;
                replace_member(&mut remapped, "messageSeqs", seqs);
            }
        }
        Ok(Some(Value::Object(remapped.into_inner())))
    }

    /// `mapList` over `numberArray(value)`.
    fn map_list(&self, value: Option<&Value>, label: &str) -> Checked<Value> {
        let Some(Value::Array(values)) = value else {
            return Err(Failure::Limit(V1ToV2Limit::UncheckedShape));
        };
        values
            .iter()
            .map(|value| self.map_one(Some(value), label).map(Value::from))
            .collect::<Checked<Vec<Value>>>()
            .map(Value::Array)
    }

    /// `mapOne` over `coordinate(value)`: the target seq of an emitted
    /// source event.
    fn map_one(&self, value: Option<&Value>, label: &str) -> Checked<u64> {
        let Some(Value::Number(number)) = value else {
            return Err(Failure::Limit(V1ToV2Limit::UncheckedShape));
        };
        // `Map.get` compares by value: a writer-spelled `f64` is a fraction
        // or lies beyond every seq, so it misses and prints as JavaScript
        // prints it; another spelling such as `3.0` might hit, a limit.
        let text = match integer_string(number) {
            Some(text) => text,
            None if is_writer_spelling(number) => {
                json_number_text(number.as_f64().unwrap_or(f64::NAN))
            }
            None => return Err(Failure::Limit(V1ToV2Limit::FloatLexeme)),
        };
        number
            .as_u64()
            .and_then(|source| self.mapping.get(&source).copied())
            .ok_or_else(|| {
                Failure::Unsupported(format!("{label} targets consumed assistant/chunk {text}"))
            })
    }
}

/// `assertChunkEnvelope`.
fn assert_chunk_envelope(fields: &Map<String, Value>, seq: u64) -> Checked<()> {
    if let Some(key) = fields.keys().find(|key| {
        !CHUNK_EVENT_REQUIRED.contains(&key.as_str())
            && !CHUNK_EVENT_OPTIONAL.contains(&key.as_str())
    }) {
        return Err(Failure::Unsupported(format!(
            "assistant/chunk {seq} has unexpected member {key}"
        )));
    }
    if let Some(key) = CHUNK_EVENT_REQUIRED
        .iter()
        .find(|key| !fields.contains_key(**key))
    {
        return Err(Failure::Unsupported(format!(
            "assistant/chunk {seq} lacks required member {key}"
        )));
    }
    if fields
        .get("ignorable")
        .is_some_and(|ignorable| *ignorable != Value::Bool(true))
    {
        return Err(Failure::Unsupported(format!(
            "assistant/chunk {seq} ignorable must be true when present"
        )));
    }
    Ok(())
}

/// `messageEvent`: the message without `sourceEventSeqs`, with `stream` set
/// in `data`.
fn message_event(
    fields: &Map<String, Value>,
    data: &Map<String, Value>,
    stream: Vec<Value>,
) -> Map<String, Value> {
    let mut message = clone_fields(fields);
    remove_member(&mut message, "sourceEventSeqs");
    let mut message_data = clone_fields(data);
    replace_member(&mut message_data, "stream", Value::Array(stream));
    replace_member(&mut message, "data", Value::Object(message_data));
    message
}

/// `!==` between attempt coordinates, negated: numbers compare by value,
/// other primitives and `undefined` by identity. An object or array, which
/// compares by reference, and a fraction or exponent spelling are limits.
fn same_coordinate(left: Option<&Value>, right: Option<&Value>) -> Checked<bool> {
    let integer = |number: &serde_json::Number| {
        number
            .as_u64()
            .map(i128::from)
            .or_else(|| number.as_i64().map(i128::from))
    };
    match (left, right) {
        (Some(Value::Object(_) | Value::Array(_)), _)
        | (_, Some(Value::Object(_) | Value::Array(_))) => {
            Err(Failure::Limit(V1ToV2Limit::UncheckedShape))
        }
        (Some(Value::Number(left)), Some(Value::Number(right))) => {
            match (integer(left), integer(right)) {
                (Some(left), Some(right)) => Ok(left == right),
                _ => Err(Failure::Limit(V1ToV2Limit::FloatLexeme)),
            }
        }
        _ => Ok(left == right),
    }
}

/// The two events a legacy goal message becomes.
struct LegacyGoalSplit {
    change: Deep<Map<String, Value>>,
    message: Deep<Map<String, Value>>,
}

/// `splitLegacyGoalChange`: a goal-sourced `user/message` carrying its
/// `change` becomes a `goal/change` and a plugin-sourced message.
fn split_legacy_goal_change(
    event_type: &str,
    fields: &Map<String, Value>,
    seq: u64,
) -> Checked<Option<LegacyGoalSplit>> {
    if event_type != "user/message" {
        return Ok(None);
    }
    let data = record(fields.get("data"))?;
    let source = record(data.get("source"))?;
    let Some(change) = source.get("change") else {
        return Ok(None);
    };
    if source.get("kind") != Some(&Value::from("goal")) {
        return Ok(None);
    }
    let change = Deep::new(generated(
        "goal/change",
        seq,
        fields.get("time"),
        clone_value(change),
    )?);
    let mut plugin = Map::new();
    plugin.insert("kind".to_owned(), Value::from("plugin"));
    plugin.insert("plugin".to_owned(), Value::from("goal"));
    let mut message_data = clone_fields(data);
    replace_member(&mut message_data, "source", Value::Object(plugin));
    let mut message = clone_fields(fields);
    replace_member(&mut message, "data", Value::Object(message_data));
    Ok(Some(LegacyGoalSplit {
        change,
        message: Deep::new(message),
    }))
}

/// An event the stage builds as `{ type, seq, time, data }`.
fn generated(
    event_type: &str,
    seq: u64,
    time: Option<&Value>,
    data: Value,
) -> Checked<Map<String, Value>> {
    let Some(time) = time else {
        dismantle(data);
        return Err(Failure::Limit(V1ToV2Limit::UndefinedMember));
    };
    let mut event = Map::new();
    event.insert("type".to_owned(), Value::from(event_type));
    event.insert("seq".to_owned(), Value::from(seq));
    event.insert("time".to_owned(), clone_value(time));
    event.insert("data".to_owned(), data);
    Ok(event)
}

fn inherited_marker() -> Value {
    let mut data = Map::new();
    data.insert("inherited".to_owned(), Value::Bool(true));
    Value::Object(data)
}

/// `record(value)` where a member is then read: only an object is decided.
fn record(value: Option<&Value>) -> Checked<&Map<String, Value>> {
    match value {
        Some(Value::Object(fields)) => Ok(fields),
        _ => Err(Failure::Limit(V1ToV2Limit::UncheckedShape)),
    }
}

/// `JSON.stringify(value)` in a message, `undefined` when absent; every
/// number prints as JavaScript prints its value.
fn stringify(value: Option<&Value>) -> String {
    value.map_or_else(
        || "undefined".to_owned(),
        |value| crate::js_string::from_rust(&crate::json_text(value)).into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn shared_list(table: &Value, list: &str) -> BTreeSet<String> {
        table["vocabulary"][list]
            .as_array()
            .unwrap_or_else(|| panic!("vocabulary.{list} array"))
            .iter()
            .map(|name| name.as_str().expect("type name").to_owned())
            .collect()
    }

    #[test]
    fn type_lists_equal_the_shared_table() {
        // The v1-to-v2 conformance spec checks these lists against the real exports.
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../conformance/session/v1-to-v2-cases.json"
        );
        let text = std::fs::read_to_string(path).expect("read v1-to-v2-cases.json");
        let table: Value = serde_json::from_str(&text).expect("parse v1-to-v2-cases.json");
        let own: BTreeSet<String> = RELEASED_V0_EVENT_TYPES
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        assert_eq!(own.len(), RELEASED_V0_EVENT_TYPES.len());
        assert_eq!(shared_list(&table, "releasedV0EventTypes"), own);
        let inherited: BTreeSet<String> = OBJECT_PROTOTYPE_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        assert_eq!(shared_list(&table, "objectPrototypeNames"), inherited);
    }

    #[test]
    fn stringify_prints_every_number_as_javascript_does() {
        assert_eq!(
            stringify(Some(&serde_json::json!({"b": [1, -2, null], "a": "x"}))),
            r#"{"b":[1,-2,null],"a":"x"}"#
        );
        assert_eq!(stringify(None), "undefined");
        let value: Value = serde_json::from_str("[1.5,1e21,5e-7,2.0,-0]").expect("JSON");
        assert_eq!(stringify(Some(&value)), "[1.5,1e+21,5e-7,2,0]");
    }
}
