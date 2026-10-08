//! The released v1→v2 migration's transformed stage over a decoded v1
//! Session, as `sessionFormatV1ToV2` in
//! `packages/session/session-format-v1-to-v2/src/migration.ts` runs it:
//! `migrateHeader`, `assertReleasedV2Header`, then the stage that
//! `createStage({ sourceKind: 'transformed' })` builds, with `transformEvent`
//! for each decoded event and then `finish`.
//!
//! This is the stage a chain runs after v0→v1, not the one production runs
//! on a directly decoded v1 file: that stage first checks each payload with
//! `assertReleasedEventPayload`, which this port does not include. The input
//! is unvalidated codec output, so this port follows the stage's unchecked
//! casts and refuses with a [`V1ToV2Limit`] wherever TypeScript would throw a
//! `TypeError` or coerce a value.
//!
//! The subset is chunk-free. Every `assistant/chunk` event, including those
//! expanded from packed rows, refuses with [`V1ToV2Limit::AssistantChunk`]
//! where TypeScript would start compacting it into an attempt. Without
//! chunks no attempt is ever pending, so the attempt grouping, stream
//! compaction, and buffering paths are never reached.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::MAX_SAFE_INTEGER;
use crate::v1_codec::DecodedV1Rows;
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
#[derive(Debug, Clone, PartialEq)]
pub struct MigratedV1ToV2 {
    /// The logical v2 header: the v1 header with `version` 2 in place.
    pub header: Value,
    /// Target events in order, with remapped references.
    pub events: Vec<Value>,
    /// The number of target events inherited from the parent Session.
    pub inherited_event_count: u64,
}

/// Where a refusal was raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V1ToV2Location {
    /// `migrateHeader`, before any event.
    Header,
    /// `transformEvent` for the decoded event at this index.
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
    /// An `assistant/chunk` event that passed its envelope check, where
    /// TypeScript would add it to an Assistant attempt.
    AssistantChunk,
    /// An event `type` that is not a string, which the vocabulary lookup
    /// converts to a property key.
    NonStringType,
    /// A value the stage casts without checking and then reads as an object
    /// member, spreads, maps as an array, or adds to or prints as a number,
    /// where it is not of that kind: JavaScript throws a `TypeError` or
    /// coerces it.
    UncheckedShape,
    /// A number spelled with a fraction or exponent, or beyond `u64`, where
    /// TypeScript compares, looks up, adds to, or prints it.
    FloatLexeme,
    /// An emitted event would carry a member whose value is `undefined`,
    /// which JSON cannot express: a synthesized event's `time` from an event
    /// without one, or a passed-through event without `data`.
    UndefinedMember,
}

impl V1ToV2Limit {
    /// The limit's name in `conformance/session/v1-to-v2-cases.json`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::AssistantChunk => "assistant-chunk",
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
    let header = match &decoded.header {
        Value::Object(fields) if fields.get("version").and_then(Value::as_u64) == Some(1) => fields,
        _ => {
            return Err(V1ToV2Refusal::Rejected {
                location: V1ToV2Location::Header,
                message: "expected format v1 header".to_owned(),
            });
        }
    };
    let mut target_header = header.clone();
    target_header.insert("version".to_owned(), Value::from(2));
    let mut stage = Stage::new(header, decoded.inherited_event_count, &decoded.events);
    for (index, event) in decoded.events.iter().enumerate() {
        stage
            .transform(index, event)
            .map_err(|failure| refusal(V1ToV2Location::Event(index), failure))?;
    }
    let inherited_event_count = stage
        .finish()
        .map_err(|failure| refusal(V1ToV2Location::Finish, failure))?;
    Ok(MigratedV1ToV2 {
        header: Value::Object(target_header),
        events: stage.output,
        inherited_event_count,
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
    Open(Option<Value>),
}

/// `ReleasedV1ToV2State` with no pending attempt.
struct Stage<'a> {
    header: &'a Map<String, Value>,
    events: &'a [Value],
    is_seeded: bool,
    source_cut: u64,
    mapping: HashMap<u64, u64>,
    open_turn: OpenTurn,
    /// Whether `openStep` is not `null`.
    open_step: bool,
    /// The index of the last observed event.
    previous: Option<usize>,
    target_seq: u64,
    target_cut: Option<u64>,
    /// `lastTime`, `None` when an event had no `time`.
    last_time: Option<Value>,
    output: Vec<Value>,
}

impl<'a> Stage<'a> {
    fn new(header: &'a Map<String, Value>, source_cut: u64, events: &'a [Value]) -> Self {
        let is_seeded = header.get("isSeeded") == Some(&Value::Bool(true));
        Self {
            header,
            events,
            is_seeded,
            source_cut,
            mapping: HashMap::new(),
            open_turn: OpenTurn::Closed,
            open_step: false,
            previous: None,
            target_seq: 0,
            target_cut: if is_seeded { None } else { Some(0) },
            last_time: header.get("createdAt").cloned(),
            output: Vec::new(),
        }
    }

    /// `transformReleasedEvent`.
    fn transform(&mut self, index: usize, event: &Value) -> Checked<()> {
        let fields = record(Some(event))?;
        let seq = index as u64;
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
        let interrupted = self.legacy_interrupted_turn(event_type, fields, seq)?;
        if event_type == "turn/start"
            && matches!(self.open_turn, OpenTurn::Open(_))
            && interrupted.is_none()
        {
            let turn = record(fields.get("data"))?.get("turn");
            return Err(Failure::Unsupported(format!(
                "turn/start {} does not close the prior turn",
                stringify(turn)?
            )));
        }
        self.assert_source_delivery_marker(event_type, fields, seq)?;
        self.observe_legacy_turn(event_type, fields, index)?;
        self.last_time = fields.get("time").cloned();
        if let Some(interrupted) = interrupted {
            self.emit_generated(seq, interrupted)?;
        }
        if let Some(LegacyGoalSplit { change, message }) =
            split_legacy_goal_change(event_type, fields, seq)?
        {
            self.emit_generated(seq, change)?;
            return self.emit_source(seq, message, fields.get("time"));
        }
        match event_type {
            "assistant/chunk" => Err(Failure::Limit(V1ToV2Limit::AssistantChunk)),
            "assistant/message" => self.transform_message(fields, seq),
            // `closesAttempt` finishes an attempt first, and none is pending.
            _ => self.emit_source(seq, fields.clone(), fields.get("time")),
        }
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
        let next = match open_turn {
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
        let previous = self.previous.and_then(|index| self.events.get(index));
        let Some(previous) = previous.and_then(Value::as_object) else {
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
        data.insert("turn".to_owned(), open_turn.clone().unwrap_or(Value::Null));
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
        index: usize,
    ) -> Checked<()> {
        match event_type {
            "turn/start" => {
                self.open_turn = match record(fields.get("data"))?.get("turn") {
                    Some(Value::Null) => OpenTurn::Closed,
                    turn => OpenTurn::Open(turn.cloned()),
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
        self.previous = Some(index);
        Ok(())
    }

    /// `transformMessage` with no pending attempt.
    fn transform_message(&mut self, fields: &Map<String, Value>, seq: u64) -> Checked<()> {
        // `data['turn']` and `data['step']` are read, but only for a pending attempt.
        let data = record(fields.get("data"))?;
        if let Some(Value::Array(sources)) = fields.get("sourceEventSeqs")
            && !sources.is_empty()
        {
            return Err(Failure::Unsupported(format!(
                "assistant/message {seq} chunk references are not one complete ordered attempt"
            )));
        }
        // `messageEvent` with an empty stream.
        let mut message = fields.clone();
        message.shift_remove("sourceEventSeqs");
        let mut message_data = data.clone();
        message_data.insert("stream".to_owned(), Value::Array(Vec::new()));
        message.insert("data".to_owned(), Value::Object(message_data));
        self.emit_source(seq, message, fields.get("time"))
    }

    /// `emitSource`. `time` is the source event's own.
    fn emit_source(
        &mut self,
        seq: u64,
        mut source: Map<String, Value>,
        time: Option<&Value>,
    ) -> Checked<()> {
        let event_type = source
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if self.is_seeded && seq == self.source_cut && event_type == "session/end-seed" {
            source.insert("data".to_owned(), inherited_marker());
        }
        self.ensure_target_cut(seq, time, &event_type)?;
        self.mapping.insert(seq, self.target_seq);
        let event = self.remap_references(source, &event_type, seq)?;
        self.output.push(event);
        self.target_seq += 1;
        Ok(())
    }

    /// `emitGenerated`.
    fn emit_generated(&mut self, origin: u64, event: Map<String, Value>) -> Checked<()> {
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        self.ensure_target_cut(origin, event.get("time"), &event_type)?;
        let event = self.remap_references(event, &event_type, origin)?;
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

    /// `finishMigration` with no pending attempt.
    fn finish(&mut self) -> Checked<u64> {
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
        mut event: Map<String, Value>,
        event_type: &str,
        seq: u64,
    ) -> Checked<Value> {
        let sources = event.shift_remove("sourceEventSeqs");
        let surface = event.shift_remove("surfaceOp");
        let sources = match sources {
            None => None,
            Some(sources) => {
                Some(self.map_list(Some(&sources), &format!("{event_type} {seq} sources"))?)
            }
        };
        let operation = match surface {
            None => None,
            Some(Value::String(text)) if text == "append" => Some(Value::String(text)),
            Some(Value::Object(replacement)) => {
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
            Some(_) => return Err(Failure::Limit(V1ToV2Limit::UncheckedShape)),
        };
        let data = self.remap_payload_references(event_type, seq, event.get("data"))?;
        event.insert("seq".to_owned(), Value::from(self.target_seq));
        let Some(data) = data else {
            return Err(Failure::Limit(V1ToV2Limit::UndefinedMember));
        };
        event.insert("data".to_owned(), data);
        if let Some(sources) = sources {
            event.insert("sourceEventSeqs".to_owned(), sources);
        }
        if let Some(operation) = operation {
            event.insert("surfaceOp".to_owned(), operation);
        }
        Ok(Value::Object(event))
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
            | "session/title-llm-request" => record(data)?.clone(),
            _ => return Ok(data.cloned()),
        };
        match event_type {
            "command/done" => {
                if let Some(source) = remapped.get("sourceEventSeq") {
                    let mapped =
                        self.map_one(Some(source), &format!("command/done {seq} sourceEventSeq"))?;
                    remapped.insert("sourceEventSeq".to_owned(), Value::from(mapped));
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
                remapped.insert("shadowedRange".to_owned(), Value::Object(range));
                remapped.insert("shadowedSeqs".to_owned(), seqs);
            }
            _ => {
                let seqs = self.map_list(
                    remapped.get("messageSeqs"),
                    &format!("{event_type} {seq} messageSeqs"),
                )?;
                remapped.insert("messageSeqs".to_owned(), seqs);
            }
        }
        Ok(Some(Value::Object(remapped)))
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
        let Some(text) = integer_string(number) else {
            return Err(Failure::Limit(V1ToV2Limit::FloatLexeme));
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

/// The two events a legacy goal message becomes.
struct LegacyGoalSplit {
    change: Map<String, Value>,
    message: Map<String, Value>,
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
    let change = generated("goal/change", seq, fields.get("time"), change.clone())?;
    let mut plugin = Map::new();
    plugin.insert("kind".to_owned(), Value::from("plugin"));
    plugin.insert("plugin".to_owned(), Value::from("goal"));
    let mut message_data = data.clone();
    message_data.insert("source".to_owned(), Value::Object(plugin));
    let mut message = fields.clone();
    message.insert("data".to_owned(), Value::Object(message_data));
    Ok(Some(LegacyGoalSplit { change, message }))
}

/// An event the stage builds as `{ type, seq, time, data }`.
fn generated(
    event_type: &str,
    seq: u64,
    time: Option<&Value>,
    data: Value,
) -> Checked<Map<String, Value>> {
    let Some(time) = time else {
        return Err(Failure::Limit(V1ToV2Limit::UndefinedMember));
    };
    let mut event = Map::new();
    event.insert("type".to_owned(), Value::from(event_type));
    event.insert("seq".to_owned(), Value::from(seq));
    event.insert("time".to_owned(), time.clone());
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

/// `JSON.stringify(value)` in a message, `undefined` when absent. Members
/// are already in JavaScript order; a number that is not a safe integer
/// spelled without a fraction or exponent reports a limit.
fn stringify(value: Option<&Value>) -> Checked<String> {
    let Some(value) = value else {
        return Ok("undefined".to_owned());
    };
    let mut pending = vec![value];
    while let Some(item) = pending.pop() {
        match item {
            Value::Number(number) => {
                let safe = number
                    .as_u64()
                    .map(|value| value <= MAX_SAFE_INTEGER)
                    .or_else(|| {
                        number
                            .as_i64()
                            .map(|value| value.unsigned_abs() <= MAX_SAFE_INTEGER)
                    })
                    .unwrap_or(false);
                if !safe {
                    return Err(Failure::Limit(V1ToV2Limit::FloatLexeme));
                }
            }
            Value::Array(items) => pending.extend(items),
            Value::Object(fields) => pending.extend(fields.values()),
            _ => {}
        }
    }
    Ok(value.to_string())
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
    fn stringify_prints_safe_integers_and_defers_other_numbers() {
        assert_eq!(
            stringify(Some(&serde_json::json!({"b": [1, -2, null], "a": "x"}))).ok(),
            Some(r#"{"b":[1,-2,null],"a":"x"}"#.to_owned())
        );
        assert_eq!(stringify(None).ok(), Some("undefined".to_owned()));
        assert!(matches!(
            stringify(Some(&serde_json::json!([1.5]))),
            Err(Failure::Limit(V1ToV2Limit::FloatLexeme))
        ));
    }
}
