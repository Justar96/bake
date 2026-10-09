//! The released v0 and v1 physical codecs' decoder, as
//! `releasedV0SessionFormatCodec` and `releasedV1SessionFormatCodec` in
//! `packages/session/session-format-v0-to-v1/src/codec.ts` run it over parsed
//! rows: `createDecoder(header, recovery)`, `decodeRow` for each row, then
//! `finish`.
//!
//! [`decode_v0_v1_items`] returns what the codec emits: the logical header,
//! the inherited cut, and each row's event or, for a packed Assistant chunk
//! row, its `ReleasedAssistantChunkRun`. [`decode_v0_v1_rows`] expands each
//! run to its `assistant/chunk` events as `run.expand()` yields them. No
//! migration runs, so the events are neither v1 nor v2 events and nothing has
//! checked their vocabulary, payloads, or relationships.

use serde_json::{Map, Value};

use crate::json_parse::{DebugJson, Deep, clone_fields, clone_value, dismantle, values_equal};
use crate::v2_to_v3::{contains_negative_zero, contains_unsafe_integer, integer_string, js_order};
use crate::{MAX_SAFE_INTEGER, PathPlatform, is_absolute};

const HEADER_REQUIRED: [&str; 5] = ["type", "version", "id", "createdAt", "delegationDepth"];
const HEADER_OPTIONAL: [&str; 5] = [
    "cwd",
    "parentSession",
    "seedLength",
    "origin",
    "agentPreset",
];
const PACKED_TAGS: [&str; 3] = ["text-chunks", "reasoning-chunks", "tool-call-chunks"];

/// Which released physical layout the header must carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V1CodecVersion {
    V0,
    V1,
}

impl V1CodecVersion {
    const fn number(self) -> u64 {
        match self {
            Self::V0 => 0,
            Self::V1 => 1,
        }
    }
}

/// `SessionFormatRecovery`: whether a malformed row refuses at once or ends
/// the decoded prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V1CodecRecovery {
    Strict,
    /// The first malformed row or seq gap is kept as the issue and every
    /// later row is dropped, except that a later row decoding as a
    /// `turn/end` event refuses with that issue.
    Recoverable,
}

/// A decoded released v0 or v1 Session.
pub struct DecodedV1Rows {
    /// The logical header, without `type`, with `isSeeded` recording whether
    /// `seedLength` was present, in the codec's member order.
    pub header: Value,
    /// The physical `seedLength`, or 0 without one.
    pub inherited_event_count: u64,
    /// Emitted events in order: ordinary rows unchanged, in JavaScript
    /// member order, with `sourceEventSeqs` expanded in place, and packed
    /// rows expanded to `assistant/chunk` events.
    pub events: Vec<Value>,
}

crate::json_parse::deep_session_parts!(DecodedV1Rows);

/// A decoded released v0 or v1 Session with its packed rows kept as runs.
pub struct DecodedV1Items {
    /// As [`DecodedV1Rows::header`].
    pub header: Value,
    /// As [`DecodedV1Rows::inherited_event_count`].
    pub inherited_event_count: u64,
    /// One item per row, in row order: `context.emitEvent` or
    /// `context.emitRun` as the decoder calls it.
    pub items: Vec<V1Item>,
}

impl Drop for DecodedV1Items {
    fn drop(&mut self) {
        dismantle(std::mem::take(&mut self.header));
    }
}

impl Clone for DecodedV1Items {
    fn clone(&self) -> Self {
        Self {
            header: clone_value(&self.header),
            inherited_event_count: self.inherited_event_count,
            items: self.items.clone(),
        }
    }
}

impl PartialEq for DecodedV1Items {
    fn eq(&self, other: &Self) -> bool {
        values_equal(&self.header, &other.header)
            && self.inherited_event_count == other.inherited_event_count
            && self.items == other.items
    }
}

impl std::fmt::Debug for DecodedV1Items {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedV1Items")
            .field("header", &DebugJson(&self.header))
            .field("inherited_event_count", &self.inherited_event_count)
            .field("items", &self.items)
            .finish()
    }
}

/// One emitted row.
pub enum V1Item {
    /// An ordinary row, as [`DecodedV1Rows::events`] holds it.
    Event(Value),
    /// A packed Assistant chunk row.
    AssistantChunkRun(ReleasedChunkRun),
}

impl Drop for V1Item {
    fn drop(&mut self) {
        if let Self::Event(event) = self {
            dismantle(std::mem::take(event));
        }
    }
}

impl Clone for V1Item {
    fn clone(&self) -> Self {
        match self {
            Self::Event(event) => Self::Event(clone_value(event)),
            Self::AssistantChunkRun(run) => Self::AssistantChunkRun(run.clone()),
        }
    }
}

impl PartialEq for V1Item {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Event(left), Self::Event(right)) => values_equal(left, right),
            (Self::AssistantChunkRun(left), Self::AssistantChunkRun(right)) => left == right,
            _ => false,
        }
    }
}

impl std::fmt::Debug for V1Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Event(event) => f.debug_tuple("Event").field(&DebugJson(event)).finish(),
            Self::AssistantChunkRun(run) => f.debug_tuple("AssistantChunkRun").field(run).finish(),
        }
    }
}

/// `ReleasedAssistantChunkRun`: a packed row's compact stream record and the
/// coordinates of the events it stands for. Only the codec builds one, so
/// its members are always the checked values.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleasedChunkRun {
    first_seq: u64,
    turn: u64,
    step: u64,
    last_time: i64,
    pub(crate) record: PackedStreamRecord,
}

impl ReleasedChunkRun {
    /// `firstSeq`, the row's `seq0`.
    pub fn first_seq(&self) -> u64 {
        self.first_seq
    }

    /// `eventCount`, the payload's length.
    pub fn event_count(&self) -> u64 {
        self.record.members.len() as u64
    }

    /// `turn`.
    pub fn turn(&self) -> u64 {
        self.turn
    }

    /// `step`.
    pub fn step(&self) -> u64 {
        self.step
    }

    /// `lastSeq`, the seq of the last event.
    pub fn last_seq(&self) -> u64 {
        // The codec checked that this is a safe integer.
        self.first_seq
            .saturating_add(self.event_count())
            .saturating_sub(1)
    }

    /// `lastTime`, the time of the last event.
    pub fn last_time(&self) -> i64 {
        self.last_time
    }

    /// `stream`: the row as one durable stream record, in TypeScript's
    /// member order.
    pub fn stream(&self) -> Value {
        self.record.to_value()
    }

    /// `run.expand()`: the run's `assistant/chunk` events.
    pub fn expand(&self) -> Vec<Value> {
        let record = &self.record;
        let mut time = record.time0;
        record
            .members
            .iter()
            .enumerate()
            .map(|(offset, member)| {
                if offset > 0 {
                    // The codec checked that every member time is a safe integer.
                    let gap = record.dt.get(offset - 1).copied().unwrap_or_default();
                    time = time.saturating_add(gap);
                }
                let mut chunk = Map::new();
                chunk.insert("type".to_owned(), Value::from(record.kind.delta_type()));
                chunk.insert("index".to_owned(), Value::from(record.index));
                if let Some(id) = &record.id {
                    chunk.insert("id".to_owned(), Value::from(id.as_str()));
                    if let Some(name) = &record.name {
                        chunk.insert("name".to_owned(), Value::from(name.as_str()));
                    }
                    chunk.insert("argumentsDelta".to_owned(), Value::from(member.as_str()));
                } else {
                    chunk.insert("text".to_owned(), Value::from(member.as_str()));
                }
                let mut data = Map::new();
                data.insert("turn".to_owned(), Value::from(self.turn));
                data.insert("step".to_owned(), Value::from(self.step));
                data.insert("chunk".to_owned(), Value::Object(chunk));
                let mut event = Map::new();
                event.insert("type".to_owned(), Value::from("assistant/chunk"));
                event.insert(
                    "seq".to_owned(),
                    Value::from(self.first_seq.saturating_add(offset as u64)),
                );
                event.insert("time".to_owned(), Value::from(time));
                event.insert("data".to_owned(), Value::Object(data));
                Value::Object(event)
            })
            .collect()
    }
}

/// Which delta a packed stream record holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PackedKind {
    Text,
    Reasoning,
    ToolCall,
}

impl PackedKind {
    /// The packed row's and stream record's `type`.
    pub(crate) const fn record_type(self) -> &'static str {
        match self {
            Self::Text => "text-chunks",
            Self::Reasoning => "reasoning-chunks",
            Self::ToolCall => "tool-call-chunks",
        }
    }

    const fn delta_type(self) -> &'static str {
        match self {
            Self::Text => "text-delta",
            Self::Reasoning => "reasoning-delta",
            Self::ToolCall => "tool-call-delta",
        }
    }
}

/// A `text-chunks`, `reasoning-chunks`, or `tool-call-chunks` stream record.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PackedStreamRecord {
    pub(crate) kind: PackedKind,
    pub(crate) time0: i64,
    pub(crate) index: u64,
    pub(crate) dt: Vec<i64>,
    /// `id`, present exactly for a tool call.
    pub(crate) id: Option<String>,
    /// A tool call's optional `name`.
    pub(crate) name: Option<String>,
    /// `texts`, or a tool call's `args`.
    pub(crate) members: Vec<String>,
}

impl PackedStreamRecord {
    /// The record in TypeScript's member order:
    /// `type, time0, index, dt, [id, name], texts | args`.
    pub(crate) fn to_value(&self) -> Value {
        let mut record = Map::new();
        record.insert("type".to_owned(), Value::from(self.kind.record_type()));
        record.insert("time0".to_owned(), Value::from(self.time0));
        record.insert("index".to_owned(), Value::from(self.index));
        record.insert("dt".to_owned(), Value::from(self.dt.clone()));
        let members = Value::from(self.members.clone());
        if let Some(id) = &self.id {
            record.insert("id".to_owned(), Value::from(id.as_str()));
            if let Some(name) = &self.name {
                record.insert("name".to_owned(), Value::from(name.as_str()));
            }
            record.insert("args".to_owned(), members);
        } else {
            record.insert("texts".to_owned(), members);
        }
        Value::Object(record)
    }
}

/// Where a refusal was raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V1CodecLocation {
    /// `createDecoder`, before any row.
    Header,
    /// `decodeRow` for the row at this index of `rows`.
    Row(usize),
    /// The decoder's `finish`, after every row.
    Finish,
}

/// Why rows were not decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V1CodecRefusal {
    /// TypeScript throws `SessionFormatError` at `location` with exactly `message`.
    Rejected {
        location: V1CodecLocation,
        message: String,
    },
    /// This crate does not decide the TypeScript outcome at `location`, the
    /// check that reads the value; nothing is claimed. A limit can hide a
    /// later TypeScript refusal, never an earlier one.
    NativeSubset {
        location: V1CodecLocation,
        limit: V1CodecLimit,
    },
}

/// Input whose TypeScript outcome depends on JavaScript number parsing,
/// coercion, or precision, or on this crate's own budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V1CodecLimit {
    /// A non-negative `f64` (a fraction or exponent spelling, or an integer
    /// above `u64::MAX`) where the header's `version` is compared or a count
    /// is read; JavaScript accepts the integral ones.
    HeaderFloatLexeme,
    /// A non-negative `f64` where a row's `seq` is read as a count, or a
    /// nonzero `f64` where it is compared with the expected seq.
    SeqFloatLexeme,
    /// An array or object `seq` that a gap message would convert with `String`.
    SeqDiagnostic,
    /// A non-negative `f64` where a `sourceEventSeqs` member or range bound
    /// is read as a count.
    SourceFloatLexeme,
    /// A non-negative `f64` where a packed row's `seq0`, `turn`, `step`, or
    /// `index` is read as a count, or an `f64` other than -0 where its
    /// `time0` or a `dt` member is read as a safe integer.
    PackedFloatLexeme,
    /// A row's expanded `sourceEventSeqs` list would exceed `source_budget`.
    SourceOutputBudget,
    /// An emitted row retains an integer outside ±(2^53 − 1) spelled other
    /// than `JSON.stringify` writes its double, such as `9007199254740993`,
    /// which `JSON.parse` would round. An unsafe integer a writer spells,
    /// such as `9223372036854776000`, is kept: serde_json stores it as it
    /// stores the text of the double `JSON.parse` reads.
    UnsafeJsonInteger,
}

impl V1CodecLimit {
    /// The limit's name in `conformance/session/v1-codec-cases.json`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::HeaderFloatLexeme => "header-float-lexeme",
            Self::SeqFloatLexeme => "seq-float-lexeme",
            Self::SeqDiagnostic => "seq-diagnostic",
            Self::SourceFloatLexeme => "source-float-lexeme",
            Self::PackedFloatLexeme => "packed-float-lexeme",
            Self::SourceOutputBudget => "source-output-budget",
            Self::UnsafeJsonInteger => "unsafe-json-integer",
        }
    }
}

enum Failure {
    Invalid(String),
    Limit(V1CodecLimit),
}

/// Decode a released v0 or v1 Session's parsed physical `header` and `rows`,
/// expanding each packed row: [`decode_v0_v1_items`], then
/// [`ReleasedChunkRun::expand`] for each run.
pub fn decode_v0_v1_rows(
    header: &Value,
    rows: &[Value],
    version: V1CodecVersion,
    recovery: V1CodecRecovery,
    platform: PathPlatform,
    source_budget: usize,
) -> Result<DecodedV1Rows, V1CodecRefusal> {
    let mut decoded = decode_v0_v1_items(header, rows, version, recovery, platform, source_budget)?;
    let mut events = Vec::new();
    for mut item in std::mem::take(&mut decoded.items) {
        match &mut item {
            V1Item::Event(event) => events.push(std::mem::take(event)),
            V1Item::AssistantChunkRun(run) => events.extend(run.expand()),
        }
    }
    Ok(DecodedV1Rows {
        header: std::mem::take(&mut decoded.header),
        inherited_event_count: decoded.inherited_event_count,
        events,
    })
}

/// Decode a released v0 or v1 Session's parsed physical `header` and `rows`.
///
/// The caller owns JSON parsing: a `Value` stands for the value `JSON.parse`
/// returns. Object members are read in JavaScript's own-key order, array
/// indices first, which decides which unexpected member a refusal names and
/// the member order of every emitted event. `platform` decides whether the
/// header's `cwd` is absolute, and `source_budget` caps each row's expanded
/// `sourceEventSeqs` list, which TypeScript does not.
pub fn decode_v0_v1_items(
    header: &Value,
    rows: &[Value],
    version: V1CodecVersion,
    recovery: V1CodecRecovery,
    platform: PathPlatform,
    source_budget: usize,
) -> Result<DecodedV1Items, V1CodecRefusal> {
    match decode_v0_v1_items_before_finish(
        header,
        rows,
        version,
        recovery,
        platform,
        source_budget,
    )? {
        (decoded, None) => Ok(decoded),
        (_, Some(finish)) => Err(finish),
    }
}

/// [`decode_v0_v1_items`] up to the decoder's `finish`: the items every row
/// emitted, with the refusal `finish` then raises, if any. TypeScript streams
/// those items through the later stages before `finish` runs.
pub(crate) fn decode_v0_v1_items_before_finish(
    header: &Value,
    rows: &[Value],
    version: V1CodecVersion,
    recovery: V1CodecRecovery,
    platform: PathPlatform,
    source_budget: usize,
) -> Result<(DecodedV1Items, Option<V1CodecRefusal>), V1CodecRefusal> {
    let (header, inherited_event_count) = decode_header(header, version.number(), platform)
        .map_err(|failure| refusal(V1CodecLocation::Header, failure))?;
    let recoverable = recovery == V1CodecRecovery::Recoverable;
    let mut items = Vec::new();
    let mut event_count: u64 = 0;
    let mut issue: Option<String> = None;
    for (index, row) in rows.iter().enumerate() {
        let location = V1CodecLocation::Row(index);
        let rejected = |message: String| V1CodecRefusal::Rejected { location, message };
        let native = |limit| V1CodecRefusal::NativeSubset { location, limit };
        let row = Deep::new(js_order(clone_value(row)));
        if let Some(issue) = &issue {
            // Every later row is still decoded, but only one decoding as a
            // `turn/end` event rethrows; any other outcome drops the row.
            if row.get("type").and_then(Value::as_str) == Some("turn/end") {
                match decode_event(row.into_inner(), index, source_budget) {
                    Ok(event) => {
                        dismantle(event);
                        return Err(rejected(issue.clone()));
                    }
                    Err(Failure::Invalid(_)) => {}
                    Err(Failure::Limit(limit)) => return Err(native(limit)),
                }
            }
            continue;
        }
        let item = match decode_item(row.into_inner(), index, source_budget) {
            Ok(item) => item,
            Err(Failure::Limit(limit)) => return Err(native(limit)),
            Err(Failure::Invalid(message)) if recoverable => {
                issue = Some(message);
                continue;
            }
            Err(Failure::Invalid(message)) => return Err(rejected(message)),
        };
        let (got, is_turn_end) = match &item {
            V1Item::AssistantChunkRun(run) => (
                (run.first_seq != event_count).then(|| run.first_seq.to_string()),
                false,
            ),
            V1Item::Event(event) => (
                seq_gap(event.get("seq"), event_count).map_err(native)?,
                event["type"] == "turn/end",
            ),
        };
        if let Some(got) = got {
            let gap = format!(
                "released Session row {index} has seq gap (expected {event_count}, got {got})"
            );
            if !recoverable || is_turn_end {
                return Err(rejected(gap));
            }
            issue = Some(gap);
            continue;
        }
        match &item {
            V1Item::AssistantChunkRun(run) => event_count += run.event_count(),
            V1Item::Event(event) => {
                if contains_unsafe_integer(event) {
                    return Err(native(V1CodecLimit::UnsafeJsonInteger));
                }
                event_count += 1;
            }
        }
        items.push(item);
    }
    let finish = (inherited_event_count > event_count).then(|| V1CodecRefusal::Rejected {
        location: V1CodecLocation::Finish,
        message: "Session inheritedEventCount exceeds its event count".to_owned(),
    });
    let decoded = DecodedV1Items {
        header,
        inherited_event_count,
        items,
    };
    Ok((decoded, finish))
}

fn refusal(location: V1CodecLocation, failure: Failure) -> V1CodecRefusal {
    match failure {
        Failure::Invalid(message) => V1CodecRefusal::Rejected { location, message },
        Failure::Limit(limit) => V1CodecRefusal::NativeSubset { location, limit },
    }
}

/// `decodePhysicalHeader`, then `assertReleasedSessionFormatHeader`, which
/// adds only the path check to what the physical checks proved.
fn decode_header(
    header: &Value,
    version: u64,
    platform: PathPlatform,
) -> Result<(Value, u64), Failure> {
    let physical = format!("released v{version} physical header");
    let label = format!("released v{version} header");
    // `snapshotSessionFormatJson`: JSON input can be lossy only through -0.
    if contains_negative_zero(header) {
        return Err(Failure::Invalid(format!("{physical} is not lossless JSON")));
    }
    let Value::Object(fields) = header else {
        return Err(Failure::Invalid(format!(
            "{physical} must be a JSON object"
        )));
    };
    let Value::Object(fields) = js_order(Value::Object(clone_fields(fields))) else {
        unreachable!("js_order keeps an object an object")
    };
    let fields = Deep::new(fields);
    exact_keys(&fields, &HEADER_REQUIRED, &HEADER_OPTIONAL, &physical)?;
    if fields["type"] != "session" || !is_version(&fields["version"], version)? {
        return Err(Failure::Invalid(format!(
            "expected released v{version} physical Session header"
        )));
    }
    let Value::String(id) = &fields["id"] else {
        return Err(Failure::Invalid(format!("{label} id must be a string")));
    };
    let header_count = |key: &str| {
        count(
            fields.get(key),
            &format!("{label} {key}"),
            V1CodecLimit::HeaderFloatLexeme,
        )
    };
    let created_at = header_count("createdAt")?;
    let delegation_depth = header_count("delegationDepth")?;
    let seed_length = match fields.get("seedLength") {
        None => 0,
        Some(_) => header_count("seedLength")?,
    };
    for key in ["cwd", "parentSession", "agentPreset"] {
        if fields.get(key).is_some_and(|value| !value.is_string()) {
            return Err(Failure::Invalid(format!("{label} {key} must be a string")));
        }
    }
    if fields
        .get("origin")
        .is_some_and(|origin| origin != "subagent")
    {
        return Err(Failure::Invalid(format!(
            "{label} origin must be \"subagent\""
        )));
    }
    if let Some(Value::String(cwd)) = fields.get("cwd")
        && !is_absolute(cwd, platform)
    {
        return Err(Failure::Invalid(format!(
            "format v{version} header cwd must be absolute"
        )));
    }
    let mut logical = Map::new();
    logical.insert("version".to_owned(), Value::from(version));
    logical.insert("id".to_owned(), Value::String(id.clone()));
    logical.insert("createdAt".to_owned(), Value::from(created_at));
    for key in ["cwd", "parentSession"] {
        if let Some(value) = fields.get(key) {
            logical.insert(key.to_owned(), value.clone());
        }
    }
    logical.insert(
        "isSeeded".to_owned(),
        Value::Bool(fields.contains_key("seedLength")),
    );
    if let Some(origin) = fields.get("origin") {
        logical.insert("origin".to_owned(), origin.clone());
    }
    logical.insert("delegationDepth".to_owned(), Value::from(delegation_depth));
    if let Some(preset) = fields.get("agentPreset") {
        logical.insert("agentPreset".to_owned(), preset.clone());
    }
    Ok((Value::Object(logical), seed_length))
}

/// `record['version'] !== version`, undecided for a non-negative `f64`.
fn is_version(value: &Value, version: u64) -> Result<bool, Failure> {
    let Value::Number(number) = value else {
        return Ok(false);
    };
    if let Some(number) = number.as_u64() {
        return Ok(number == version);
    }
    if number.is_i64() || number.as_f64().is_some_and(f64::is_sign_negative) {
        return Ok(false);
    }
    Err(Failure::Limit(V1CodecLimit::HeaderFloatLexeme))
}

/// `assertReleasedV0Keys`: the first unexpected member in key order, then
/// the first missing required member.
fn exact_keys(
    fields: &Map<String, Value>,
    required: &[&str],
    optional: &[&str],
    label: &str,
) -> Result<(), Failure> {
    if let Some(key) = fields
        .keys()
        .find(|key| !required.contains(&key.as_str()) && !optional.contains(&key.as_str()))
    {
        return Err(Failure::Invalid(format!(
            "{label} has unexpected member {}",
            quoted(key)
        )));
    }
    if let Some(key) = required.iter().find(|key| !fields.contains_key(**key)) {
        return Err(Failure::Invalid(format!(
            "{label} lacks required member {}",
            quoted(key)
        )));
    }
    Ok(())
}

/// `JSON.stringify` of a string.
fn quoted(key: &str) -> String {
    crate::v2_to_v3::quote(key)
}

/// `sessionFormatCount`. A non-negative `f64` reports `limit`.
fn count(value: Option<&Value>, label: &str, limit: V1CodecLimit) -> Result<u64, Failure> {
    let invalid = || Failure::Invalid(format!("{label} must be a non-negative safe integer"));
    let Some(Value::Number(number)) = value else {
        return Err(invalid());
    };
    if let Some(number) = number.as_u64() {
        return if number <= MAX_SAFE_INTEGER {
            Ok(number)
        } else {
            Err(invalid())
        };
    }
    if number.is_i64() || number.as_f64().is_some_and(f64::is_sign_negative) {
        return Err(invalid());
    }
    Err(Failure::Limit(limit))
}

/// `sessionFormatSafeInteger`. An `f64` other than -0 reports `limit`.
fn safe_integer(value: Option<&Value>, label: &str, limit: V1CodecLimit) -> Result<i64, Failure> {
    let invalid = || Failure::Invalid(format!("{label} must be a safe integer"));
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
    if number.is_u64() || number.as_f64().is_some_and(is_negative_zero) {
        return Err(invalid());
    }
    Err(Failure::Limit(limit))
}

fn is_negative_zero(number: f64) -> bool {
    number == 0.0 && number.is_sign_negative()
}

/// The got text of the gap check `seq !== eventCount`, `None` when equal.
fn seq_gap(seq: Option<&Value>, expected: u64) -> Result<Option<String>, V1CodecLimit> {
    let got = match seq {
        None => "undefined".to_owned(),
        Some(Value::Null) => "null".to_owned(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(_) | Value::Object(_)) => return Err(V1CodecLimit::SeqDiagnostic),
        Some(Value::Number(number)) => match integer_string(number) {
            Some(text) => {
                // JavaScript compares the parsed double; every expected seq
                // here is an exact double.
                let nearest = number.as_f64().expect("an integer has a double");
                if nearest == expected as f64 {
                    return Ok(None);
                }
                text
            }
            // A zero spelled as a float, -0 included, equals 0 and prints as "0".
            None if number.as_f64() == Some(0.0) => {
                if expected == 0 {
                    return Ok(None);
                }
                "0".to_owned()
            }
            None => return Err(V1CodecLimit::SeqFloatLexeme),
        },
    };
    Ok(Some(got))
}

fn decode_item(row: Value, index: usize, budget: usize) -> Result<V1Item, Failure> {
    let row = Deep::new(row);
    let Value::Object(fields) = &*row else {
        return Err(Failure::Invalid(format!(
            "released Session row {index} must be a JSON object"
        )));
    };
    match fields.get("type") {
        Some(Value::String(tag)) if PACKED_TAGS.contains(&tag.as_str()) => {
            decode_packed_run(fields, tag, index)
        }
        _ => decode_event(row.into_inner(), index, budget).map(V1Item::Event),
    }
}

/// `decodeEvent`: the row itself, with `sourceEventSeqs` expanded in place,
/// as the codec's spread replaces a member at its position.
fn decode_event(row: Value, index: usize, budget: usize) -> Result<Value, Failure> {
    let Value::Object(fields) = row else {
        unreachable!("only object rows reach decodeEvent")
    };
    let mut fields = Deep::new(fields);
    if let Some(sources) = fields.get("sourceEventSeqs") {
        let seq = count(
            fields.get("seq"),
            &format!("released Session row {index} seq"),
            V1CodecLimit::SeqFloatLexeme,
        )?;
        let expanded = decode_seq_ranges(sources, seq, budget)?;
        // The replaced member decoded as a list of counts, so it is shallow.
        fields.insert(
            "sourceEventSeqs".to_owned(),
            Value::Array(expanded.into_iter().map(Value::from).collect()),
        );
    }
    Ok(Value::Object(fields.into_inner()))
}

/// `decodeSeqRanges` with at most `max_entries` members.
fn decode_seq_ranges(value: &Value, max_entries: u64, budget: usize) -> Result<Vec<u64>, Failure> {
    let invalid = |message: &str| Failure::Invalid(message.to_owned());
    let Value::Array(entries) = value else {
        return Err(invalid("sourceEventSeqs must be an array"));
    };
    let limit = V1CodecLimit::SourceFloatLexeme;
    let mut output: Vec<u64> = Vec::new();
    let mut has_range = false;
    for entry in entries {
        if entry.is_number() {
            if output.len() as u64 >= max_entries {
                return Err(invalid("sourceEventSeqs exceeds its event seq"));
            }
            let member = count(Some(entry), "sourceEventSeqs member", limit)?;
            if output.len() >= budget {
                return Err(Failure::Limit(V1CodecLimit::SourceOutputBudget));
            }
            output.push(member);
            continue;
        }
        let pair = match entry {
            Value::Array(pair) if pair.len() == 2 => pair,
            _ => return Err(invalid("sourceEventSeqs range must be a [start, end] pair")),
        };
        let start = count(pair.first(), "sourceEventSeqs range start", limit)?;
        let end = count(pair.get(1), "sourceEventSeqs range end", limit)?;
        if end < start || end - start + 1 > max_entries - output.len() as u64 {
            return Err(invalid("sourceEventSeqs range exceeds its event seq"));
        }
        if end - start + 1 > (budget - output.len()) as u64 {
            return Err(Failure::Limit(V1CodecLimit::SourceOutputBudget));
        }
        output.extend(start..=end);
        has_range = true;
    }
    if has_range && output.windows(2).any(|pair| pair[1] <= pair[0]) {
        return Err(invalid(
            "sourceEventSeqs ranges must be strictly increasing",
        ));
    }
    Ok(output)
}

/// `decodePackedRun`, in the codec's check order.
fn decode_packed_run(
    fields: &Map<String, Value>,
    tag: &str,
    index: usize,
) -> Result<V1Item, Failure> {
    let label = format!("released {tag} row {index}");
    let limit = V1CodecLimit::PackedFloatLexeme;
    exact_keys(fields, &["type", "seq0", "time0", "data"], &[], &label)?;
    let seq0 = count(fields.get("seq0"), &format!("{label} seq0"), limit)?;
    let time0 = safe_integer(fields.get("time0"), &format!("{label} time0"), limit)?;
    let Some(Value::Object(data)) = fields.get("data") else {
        return Err(Failure::Invalid(format!(
            "{label} data must be a JSON object"
        )));
    };
    let is_tool = tag == "tool-call-chunks";
    let data_label = format!("{label} data");
    if is_tool {
        exact_keys(
            data,
            &["turn", "step", "index", "id", "dt", "args"],
            &["name"],
            &data_label,
        )?;
    } else {
        exact_keys(
            data,
            &["turn", "step", "index", "dt", "texts"],
            &[],
            &data_label,
        )?;
    }
    let members: Vec<&str> = match &data[if is_tool { "args" } else { "texts" }] {
        Value::Array(items) if !items.is_empty() => items
            .iter()
            .map(Value::as_str)
            .collect::<Option<_>>()
            .ok_or_else(|| {
                Failure::Invalid(format!("{label} payload must be a non-empty string array"))
            })?,
        _ => {
            return Err(Failure::Invalid(format!(
                "{label} payload must be a non-empty string array"
            )));
        }
    };
    let gaps = match &data["dt"] {
        Value::Array(gaps) if gaps.len() == members.len() - 1 => gaps,
        _ => {
            return Err(Failure::Invalid(format!(
                "{label} dt length must match its payload"
            )));
        }
    };
    let mut dt = Vec::with_capacity(gaps.len());
    let mut last_time = time0;
    for gap in gaps {
        let gap = safe_integer(Some(gap), &format!("{label} dt member"), limit)?;
        // Both are safe, so the double sum is unsafe exactly when the exact one is.
        let time = last_time + gap;
        if time.unsigned_abs() > MAX_SAFE_INTEGER {
            return Err(Failure::Invalid(format!(
                "{label} member time must be a safe integer"
            )));
        }
        dt.push(gap);
        last_time = time;
    }
    let turn = count(data.get("turn"), &format!("{label} turn"), limit)?;
    let step = count(data.get("step"), &format!("{label} step"), limit)?;
    let chunk_index = count(data.get("index"), &format!("{label} index"), limit)?;
    let name = data.get("name");
    if is_tool
        && (!data["id"].as_str().is_some_and(|id| !id.is_empty())
            || name.is_some_and(|name| !name.is_string()))
    {
        return Err(Failure::Invalid(format!(
            "{label} id and optional name must be strings"
        )));
    }
    // `seq0 + payload.length - 1` in doubles, which can round back into range.
    let last_seq = (seq0 as f64 + members.len() as f64) - 1.0;
    if last_seq > MAX_SAFE_INTEGER as f64 {
        return Err(Failure::Invalid(format!(
            "{label} final seq must be a non-negative safe integer"
        )));
    }
    let kind = match tag {
        "text-chunks" => PackedKind::Text,
        "reasoning-chunks" => PackedKind::Reasoning,
        _ => PackedKind::ToolCall,
    };
    let record = PackedStreamRecord {
        kind,
        time0,
        index: chunk_index,
        dt,
        id: data
            .get("id")
            .and_then(Value::as_str)
            .filter(|_| is_tool)
            .map(str::to_owned),
        name: name.and_then(Value::as_str).map(str::to_owned),
        members: members.iter().map(|member| (*member).to_owned()).collect(),
    };
    Ok(V1Item::AssistantChunkRun(ReleasedChunkRun {
        first_seq: seq0,
        turn,
        step,
        last_time,
        record,
    }))
}
