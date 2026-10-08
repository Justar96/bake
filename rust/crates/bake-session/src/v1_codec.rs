//! The released v0 and v1 physical codecs' decoder, as
//! `releasedV0SessionFormatCodec` and `releasedV1SessionFormatCodec` in
//! `packages/session/session-format-v0-to-v1/src/codec.ts` run it over parsed
//! rows: `createDecoder(header, recovery)`, `decodeRow` for each row, then
//! `finish`.
//!
//! The output is the codec's: the logical header, the inherited cut, and the
//! emitted events, with each packed Assistant chunk row expanded to its
//! `assistant/chunk` events as `run.expand()` yields them. No migration runs,
//! so the events are neither v1 nor v2 events and nothing has checked their
//! vocabulary, payloads, or relationships.

use serde_json::{Map, Value};

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
#[derive(Debug, Clone, PartialEq)]
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
    /// An emitted row retains an integer outside ±(2^53 − 1), which
    /// `JSON.parse` would round.
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

enum Item {
    Event(Value),
    Run { first_seq: u64, events: Vec<Value> },
}

/// Decode a released v0 or v1 Session's parsed physical `header` and `rows`.
///
/// The caller owns JSON parsing: a `Value` stands for the value `JSON.parse`
/// returns. Object members are read in JavaScript's own-key order, array
/// indices first, which decides which unexpected member a refusal names and
/// the member order of every emitted event. `platform` decides whether the
/// header's `cwd` is absolute, and `source_budget` caps each row's expanded
/// `sourceEventSeqs` list, which TypeScript does not.
pub fn decode_v0_v1_rows(
    header: &Value,
    rows: &[Value],
    version: V1CodecVersion,
    recovery: V1CodecRecovery,
    platform: PathPlatform,
    source_budget: usize,
) -> Result<DecodedV1Rows, V1CodecRefusal> {
    let (header, inherited_event_count) = decode_header(header, version.number(), platform)
        .map_err(|failure| refusal(V1CodecLocation::Header, failure))?;
    let recoverable = recovery == V1CodecRecovery::Recoverable;
    let mut events = Vec::new();
    let mut event_count: u64 = 0;
    let mut issue: Option<String> = None;
    for (index, row) in rows.iter().enumerate() {
        let location = V1CodecLocation::Row(index);
        let rejected = |message: String| V1CodecRefusal::Rejected { location, message };
        let native = |limit| V1CodecRefusal::NativeSubset { location, limit };
        let row = js_order(row.clone());
        if let Some(issue) = &issue {
            // Every later row is still decoded, but only one decoding as a
            // `turn/end` event rethrows; any other outcome drops the row.
            if row.get("type").and_then(Value::as_str) == Some("turn/end") {
                match decode_event(row, index, source_budget) {
                    Ok(_) => return Err(rejected(issue.clone())),
                    Err(Failure::Invalid(_)) => {}
                    Err(Failure::Limit(limit)) => return Err(native(limit)),
                }
            }
            continue;
        }
        let item = match decode_item(row, index, source_budget) {
            Ok(item) => item,
            Err(Failure::Limit(limit)) => return Err(native(limit)),
            Err(Failure::Invalid(message)) if recoverable => {
                issue = Some(message);
                continue;
            }
            Err(Failure::Invalid(message)) => return Err(rejected(message)),
        };
        let (got, is_turn_end) = match &item {
            Item::Run { first_seq, .. } => (
                (*first_seq != event_count).then(|| first_seq.to_string()),
                false,
            ),
            Item::Event(event) => (
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
        match item {
            Item::Run {
                events: expanded, ..
            } => {
                event_count += expanded.len() as u64;
                events.extend(expanded);
            }
            Item::Event(event) => {
                if contains_unsafe_integer(&event) {
                    return Err(native(V1CodecLimit::UnsafeJsonInteger));
                }
                event_count += 1;
                events.push(event);
            }
        }
    }
    if inherited_event_count > event_count {
        return Err(V1CodecRefusal::Rejected {
            location: V1CodecLocation::Finish,
            message: "Session inheritedEventCount exceeds its event count".to_owned(),
        });
    }
    Ok(DecodedV1Rows {
        header,
        inherited_event_count,
        events,
    })
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
    let Value::Object(fields) = js_order(header.clone()) else {
        return Err(Failure::Invalid(format!(
            "{physical} must be a JSON object"
        )));
    };
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

/// `JSON.stringify` of a string. A Rust string holds no lone surrogate, and
/// for every other string both escape the same characters the same way.
fn quoted(key: &str) -> String {
    serde_json::to_string(key).expect("a string serializes")
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

fn decode_item(row: Value, index: usize, budget: usize) -> Result<Item, Failure> {
    let Value::Object(fields) = &row else {
        return Err(Failure::Invalid(format!(
            "released Session row {index} must be a JSON object"
        )));
    };
    match fields.get("type") {
        Some(Value::String(tag)) if PACKED_TAGS.contains(&tag.as_str()) => {
            decode_packed_run(fields, tag, index)
        }
        _ => decode_event(row, index, budget).map(Item::Event),
    }
}

/// `decodeEvent`: the row itself, with `sourceEventSeqs` expanded in place,
/// as the codec's spread replaces a member at its position.
fn decode_event(row: Value, index: usize, budget: usize) -> Result<Value, Failure> {
    let Value::Object(mut fields) = row else {
        unreachable!("only object rows reach decodeEvent")
    };
    if let Some(sources) = fields.get("sourceEventSeqs") {
        let seq = count(
            fields.get("seq"),
            &format!("released Session row {index} seq"),
            V1CodecLimit::SeqFloatLexeme,
        )?;
        let expanded = decode_seq_ranges(sources, seq, budget)?;
        fields.insert(
            "sourceEventSeqs".to_owned(),
            Value::Array(expanded.into_iter().map(Value::from).collect()),
        );
    }
    Ok(Value::Object(fields))
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

/// `decodePackedRun` and the run's `expand`, in the codec's check order.
fn decode_packed_run(
    fields: &Map<String, Value>,
    tag: &str,
    index: usize,
) -> Result<Item, Failure> {
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
    let mut times = vec![time0];
    for gap in gaps {
        let gap = safe_integer(Some(gap), &format!("{label} dt member"), limit)?;
        let last = times[times.len() - 1];
        // Both are safe, so the double sum is unsafe exactly when the exact one is.
        let time = last + gap;
        if time.unsigned_abs() > MAX_SAFE_INTEGER {
            return Err(Failure::Invalid(format!(
                "{label} member time must be a safe integer"
            )));
        }
        times.push(time);
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
    let events = members
        .iter()
        .zip(times)
        .enumerate()
        .map(|(offset, (member, time))| {
            let mut chunk = Map::new();
            let delta = match tag {
                "text-chunks" => "text-delta",
                "reasoning-chunks" => "reasoning-delta",
                _ => "tool-call-delta",
            };
            chunk.insert("type".to_owned(), Value::from(delta));
            chunk.insert("index".to_owned(), Value::from(chunk_index));
            if is_tool {
                chunk.insert("id".to_owned(), data["id"].clone());
                if let Some(name) = name {
                    chunk.insert("name".to_owned(), name.clone());
                }
                chunk.insert("argumentsDelta".to_owned(), Value::from(*member));
            } else {
                chunk.insert("text".to_owned(), Value::from(*member));
            }
            let mut event_data = Map::new();
            event_data.insert("turn".to_owned(), Value::from(turn));
            event_data.insert("step".to_owned(), Value::from(step));
            event_data.insert("chunk".to_owned(), Value::Object(chunk));
            let mut event = Map::new();
            event.insert("type".to_owned(), Value::from("assistant/chunk"));
            event.insert("seq".to_owned(), Value::from(seq0 + offset as u64));
            event.insert("time".to_owned(), Value::from(time));
            event.insert("data".to_owned(), Value::Object(event_data));
            Value::Object(event)
        })
        .collect();
    Ok(Item::Run {
        first_seq: seq0,
        events,
    })
}
