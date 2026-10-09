//! The released v2 codec's strict decoder feeding the adjacent v2→v3
//! migration, as `createSessionFormatChain` runs it, over parsed rows.
//!
//! [`migrate_v2_rows`] reproduces, for a v2 Session, TypeScript's
//! `releasedV2SessionFormatCodec.createDecoder(header, 'strict')` with each
//! `decodeRow` emitting into the chain stream that
//! `createSessionFormatChain(...).createStream(decoder.header, undefined, collector)`
//! builds, then the decoder's `finish` and the stream's `finish`. Its source
//! is `packages/session/session-format-v1-to-v2/src/codec.ts`,
//! `packages/session/session-format/src/chain.ts`, and
//! `packages/session/session-format-v2-to-v3/src/{migration,references,payload}.ts`.
//!
//! Its output is the stage output, not an opened Session: the catalog's
//! final `restoreReleasedV3Artifact` over the transformed artifact, which
//! checks relationships, the protected head, and vocabulary, is not run.

mod admission;
mod canonical;
mod header;
mod js;
mod payload_semantics;
mod references;
mod stage;

use serde_json::{Map, Value};

use crate::PathPlatform;
use crate::envelope::{
    EnvelopeLimit, EnvelopeRefusal, EnvelopeRejection, NumberField, decode_row_envelope,
};
use crate::json_parse::{Deep, clone_fields, clone_value, replace_member};
use crate::source_event_seqs::SourceEventSeqsLimit;
pub(crate) use admission::{Lookup, OBJECT_PROTOTYPE_NAMES, is_repair_identity, lookup};
pub(crate) use canonical::{assert_v3_event, safe_integer as v3_safe_integer};
use header::SourceHeader;
pub(crate) use header::contains_negative_zero;
pub(crate) use js::{exact_keys, integer_string, js_order, record as js_record};
pub(crate) use payload_semantics::{
    Checked, assert_released_payload_semantics, count, invalid, quote, released_keys,
    released_record,
};
use stage::Stage;

/// `SURFACE_TYPES` in `payload.ts`; every other admitted event is log-only.
const SURFACE_TYPES: [&str; 4] = [
    "system/message",
    "user/message",
    "assistant/message",
    "tool/result",
];
const MIGRATION: &str = "bake-session-format-v2-to-v3";
const ENVELOPE_KEYS: [&str; 7] = [
    "type",
    "seq",
    "time",
    "data",
    "ignorable",
    "sourceEventSeqs",
    "surfaceOp",
];

/// A migrated v2 Session's logical v3 header, events, and inherited cut.
pub struct MigratedV2 {
    /// The logical v3 header, without `type`: `version` 3 and the released v2
    /// fields in codec order, with an `agentPreset` of `code` renamed `ptc`.
    pub header: Value,
    /// Target events in order, with expanded `sourceEventSeqs` lists.
    pub events: Vec<Value>,
    /// The number of target events inherited from the parent Session.
    pub inherited_event_count: u64,
}

crate::json_parse::deep_session_parts!(MigratedV2);

/// Where a refusal was raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V2ToV3Location {
    /// The physical header, before any row.
    Header,
    /// The row at this index of `rows`.
    Row(usize),
    /// The decoder's or the migration's `finish`, after every row.
    Finish,
}

/// Which TypeScript layer refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V2ToV3Layer {
    /// The released v2 codec throws `SessionFormatError`.
    Codec,
    /// The migration chain throws `SessionFormatUnsupportedMigrationError`:
    /// either the stage's own unsupported error or an ordinary stage error
    /// wrapped as `bake-session-format-v2-to-v3 refuses this format v2 Session: <detail>`.
    Migration,
}

/// Why rows were not migrated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V2ToV3Refusal {
    /// TypeScript refuses at `location` in `layer` with exactly `message`.
    Rejected {
        location: V2ToV3Location,
        layer: V2ToV3Layer,
        message: String,
    },
    /// This crate cannot reproduce the TypeScript outcome at `location`, the
    /// check that reads the value; nothing is claimed. A limit can hide a
    /// later TypeScript refusal, never an earlier one. `limit` names it:
    /// `header-float-lexeme`, `time-float-lexeme`, `seq-float-lexeme`,
    /// `source-float-lexeme`, and `reference-float-lexeme` for a
    /// non-negative `f64` (a fraction or exponent spelling, or an integer
    /// above `u64::MAX`) where TypeScript reads a count or safe integer;
    /// `seq-diagnostic` for an array or object seq that a gap message would
    /// convert with `String`; `source-output-budget` when an expanded
    /// `sourceEventSeqs` list would exceed `source_budget`; and the
    /// `payload-float-lexeme` for a non-writer number spelling in the
    /// frozen payload checks,
    /// `object-prototype-type` for inherited JavaScript property names, and
    /// `unsafe-json-integer` for retained integer values outside ±(2^53 − 1)
    /// that are not spelled as `JSON.stringify` writes them.
    /// `canonical-float-lexeme`, `admission-invariant`, and `row-count` guard
    /// invariants that admitted, addressable input cannot reach.
    NativeSubset {
        location: V2ToV3Location,
        limit: String,
    },
}

/// A stage failure before the chain classifies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StageError {
    /// TypeScript throws `SessionFormatError` with this message.
    Invalid(String),
    /// TypeScript throws `SessionFormatUnsupportedMigrationError` with this message.
    Unsupported(String),
    /// This crate does not decide the TypeScript outcome; the string names the limit.
    NativeLimit(String),
}

/// Strictly decode a released v2 Session's physical `header` and `rows` and
/// migrate them to format v3.
///
/// `rows` are the parsed physical records after the header, in order, with
/// range-encoded `sourceEventSeqs`. The caller owns JSON parsing: a `Value`
/// stands for the value `JSON.parse` returns, so invalid UTF-8, parser
/// limits, and floating-point parsing are the caller's to check. Object members are
/// read in JavaScript's own-key order, array indices first, which decides
/// which unexpected key a refusal names and the member order of every output
/// object. Opaque payload numbers retain their parsed values. After each row
/// passes codec and migration checks, a retained integer outside the
/// JavaScript safe range is kept when spelled as `JSON.stringify` writes it
/// and refused when its digits do not round-trip.
///
/// `platform` decides whether the header's `cwd` is absolute.
/// `source_budget` caps each row's expanded `sourceEventSeqs` list, which
/// TypeScript does not; the input values are never modified.
pub fn migrate_v2_rows(
    header: &Value,
    rows: &[Value],
    platform: PathPlatform,
    source_budget: usize,
) -> Result<MigratedV2, V2ToV3Refusal> {
    let (source_header, target_header) =
        header::decode(header, platform).map_err(|error| codec(V2ToV3Location::Header, error))?;
    let is_seeded = source_header.is_seeded;
    let mut stage = Stage::new(source_header);
    let mut inherited_marker = None;
    for (index, row) in rows.iter().enumerate() {
        let location = V2ToV3Location::Row(index);
        let event =
            decode_row(row, index, source_budget).map_err(|error| codec(location, error))?;
        if event["type"] == "session/end-seed" && event["data"]["inherited"] == true {
            inherited_marker = Some(index as u64);
        }
        stage
            .transform(event)
            .map_err(|error| migration(location, error))?;
        if contains_unsafe_integer(row) {
            return Err(V2ToV3Refusal::NativeSubset {
                location,
                limit: "unsafe-json-integer".to_owned(),
            });
        }
    }
    if is_seeded && inherited_marker.is_none() {
        return Err(codec_message(
            "released v2 seeded Session lacks an inherited end-seed marker",
        ));
    }
    if !is_seeded && inherited_marker.is_some() {
        return Err(codec_message(
            "released v2 unseeded Session contains an inherited end-seed marker",
        ));
    }
    let (events, inherited_event_count) = stage
        .finish()
        .map_err(|error| migration(V2ToV3Location::Finish, error))?;
    Ok(MigratedV2 {
        header: target_header,
        events,
        inherited_event_count,
    })
}

/// `sessionFormatV2ToV3` as a chain runs it after the v1→v2 transformed
/// stage: `migrateHeader` over the logical v2 `header`, then the stage's
/// `transformEvent` for each logical v2 event, then its `finish`. No codec
/// runs, so a refusal's [`V2ToV3Location::Row`] is an index into `events`,
/// and every refusal is in the migration layer.
///
/// The header must be what the earlier stages emit after a released v0 or
/// v1 decode, which `assertReleasedV2Header` and `assertReleasedV3Header`
/// always admit; any other header reports the `decode-invariant` limit.
pub(crate) fn migrate_logical_v2(
    header: &Value,
    events: &[Value],
) -> Result<MigratedV2, V2ToV3Refusal> {
    let (source, target_header) = logical_header(header)?;
    let stage = transform_logical_events(source, events)?;
    let (events, inherited_event_count) = stage
        .finish()
        .map_err(|error| migration(V2ToV3Location::Finish, error))?;
    Ok(MigratedV2 {
        header: target_header,
        events,
        inherited_event_count,
    })
}

/// [`migrate_logical_v2`] without the stage's `finish`: whether the edge
/// refuses any of `events`, as a chain stream does before an earlier stage
/// refuses the next source event. Its output is then discarded.
pub(crate) fn check_logical_v2_prefix(
    header: &Value,
    events: &[Value],
) -> Result<(), V2ToV3Refusal> {
    let (source, _) = logical_header(header)?;
    transform_logical_events(source, events).map(drop)
}

fn transform_logical_events(
    source: SourceHeader,
    events: &[Value],
) -> Result<Stage, V2ToV3Refusal> {
    let mut stage = Stage::new(source);
    for (index, event) in events.iter().enumerate() {
        stage
            .transform(clone_value(event))
            .map_err(|error| migration(V2ToV3Location::Row(index), error))?;
    }
    Ok(stage)
}

/// The chain's v2→v3 `migrateHeader` over a logical v2 header: `version` 3
/// in place, and an `agentPreset` of `code` renamed `ptc` in place.
fn logical_header(header: &Value) -> Result<(SourceHeader, Value), V2ToV3Refusal> {
    const REQUIRED: [&str; 5] = ["version", "id", "createdAt", "isSeeded", "delegationDepth"];
    const OPTIONAL: [&str; 4] = ["cwd", "parentSession", "origin", "agentPreset"];
    let invariant = || V2ToV3Refusal::NativeSubset {
        location: V2ToV3Location::Header,
        limit: "decode-invariant".to_owned(),
    };
    let Value::Object(fields) = header else {
        return Err(invariant());
    };
    let exact = fields
        .keys()
        .all(|key| REQUIRED.contains(&key.as_str()) || OPTIONAL.contains(&key.as_str()))
        && REQUIRED.iter().all(|key| fields.contains_key(*key));
    let counted = |key: &str| {
        fields
            .get(key)
            .and_then(Value::as_u64)
            .is_some_and(|value| value <= crate::MAX_SAFE_INTEGER)
    };
    let strings = ["cwd", "parentSession", "agentPreset"]
        .iter()
        .all(|key| fields.get(*key).is_none_or(Value::is_string));
    let origin = fields
        .get("origin")
        .is_none_or(|origin| origin == "subagent");
    let (Some(Value::String(id)), Some(Value::Bool(is_seeded))) =
        (fields.get("id"), fields.get("isSeeded"))
    else {
        return Err(invariant());
    };
    // The decoders already proved `cwd` absolute on the caller's platform.
    if !exact
        || fields.get("version").and_then(Value::as_u64) != Some(2)
        || !counted("createdAt")
        || !counted("delegationDepth")
        || !strings
        || !origin
    {
        return Err(invariant());
    }
    let mut target = clone_fields(fields);
    target.insert("version".to_owned(), Value::from(3_u64));
    if target
        .get("agentPreset")
        .is_some_and(|preset| preset == "code")
    {
        target.insert("agentPreset".to_owned(), Value::from("ptc"));
    }
    let source = SourceHeader {
        id: id.clone(),
        is_seeded: *is_seeded,
        has_parent: fields.contains_key("parentSession"),
    };
    Ok((source, Value::Object(target)))
}

/// Whether `value` holds an integer outside ±(2^53 − 1) that is not a writer
/// spelling. A writer-spelled one is kept as is: printing it with
/// `json_number_text` and comparing it with another writer spelling both
/// agree with the double `JSON.parse` reads.
pub(crate) fn contains_unsafe_integer(value: &Value) -> bool {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Number(number) => {
                let magnitude = number
                    .as_i64()
                    .map(i64::unsigned_abs)
                    .or_else(|| number.as_u64());
                if magnitude.is_some_and(|value| value > crate::MAX_SAFE_INTEGER)
                    && !crate::json_text::is_writer_spelling(number)
                {
                    return true;
                }
            }
            Value::Array(items) => pending.extend(items),
            Value::Object(fields) => pending.extend(fields.values()),
            _ => {}
        }
    }
    false
}

/// The codec's `decodeRow` for row `index`, after `index` admitted rows:
/// the logical event, with `sourceEventSeqs` expanded in place.
fn decode_row(row: &Value, index: usize, budget: usize) -> Result<Value, StageError> {
    let row = Deep::new(js_order(clone_value(row)));
    let expected = index as u64;
    let sources = match decode_row_envelope(&row, expected, budget) {
        Ok(envelope) => envelope.source_event_seqs,
        Err(EnvelopeRefusal::NativeSubset(EnvelopeLimit::NegativeZeroSeq)) => {
            // `-0 !== 0` is false, so -0 passes the gap check at row 0 and
            // reaches the stage; elsewhere `String(-0)` names it "0".
            if expected != 0 {
                return Err(StageError::Invalid(format!(
                    "released v2 row {expected} has seq gap (expected {expected}, got 0)"
                )));
            }
            if row["type"] == "session/end-seed" && !row["data"].is_object() {
                return Err(StageError::Invalid(
                    "session/end-seed 0 data must be an object".to_owned(),
                ));
            }
            // The field is absent: with it, the seq is counted first and -0 refused.
            None
        }
        Err(EnvelopeRefusal::Rejected(rejection)) => {
            return Err(StageError::Invalid(row_message(&row, &rejection, expected)));
        }
        Err(EnvelopeRefusal::NativeSubset(limit)) => {
            return Err(StageError::NativeLimit(limit_name(limit).to_owned()));
        }
        Err(EnvelopeRefusal::ExpectedSeqOutOfRange) => {
            return Err(StageError::NativeLimit("row-count".to_owned()));
        }
    };
    let Value::Object(mut event) = row.into_inner() else {
        unreachable!("an admitted envelope is an object")
    };
    if let Some(sources) = sources {
        // Replacing a member keeps its position, as the codec's spread does.
        replace_member(
            &mut event,
            "sourceEventSeqs",
            Value::Array(sources.into_iter().map(Value::from).collect()),
        );
    }
    Ok(Value::Object(event))
}

/// How the released v2 codec's recoverable `decodeRow` treats the first row
/// that [`migrate_v2_rows`] refuses in its codec layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoverableRefusal {
    /// `decodeEvent` refused the row, which recovery catches as the issue.
    Caught,
    /// The row decoded with a seq gap, which recovery keeps as the issue,
    /// rethrown at once for a `turn/end` row.
    Gap { turn_end: bool },
    /// A `session/end-seed` row's data is not an object, which `decodeRow`
    /// throws outside the recovery catch.
    Thrown,
}

/// [`RecoverableRefusal`] for row `index`, the first row the strict codec
/// refused. `None` when that codec admits the row.
pub(crate) fn recoverable_refusal(
    row: &Value,
    index: usize,
    budget: usize,
) -> Option<RecoverableRefusal> {
    let row = Deep::new(js_order(clone_value(row)));
    let turn_end = row.get("type").is_some_and(|kind| kind == "turn/end");
    match decode_row_envelope(&row, index as u64, budget) {
        Err(EnvelopeRefusal::Rejected(EnvelopeRejection::SeqGap { .. })) => {
            Some(RecoverableRefusal::Gap { turn_end })
        }
        Err(EnvelopeRefusal::Rejected(EnvelopeRejection::EndSeedDataNotObject)) => {
            Some(RecoverableRefusal::Thrown)
        }
        Err(EnvelopeRefusal::Rejected(_)) => Some(RecoverableRefusal::Caught),
        // `-0` passes `decodeEvent` and reaches the gap check, as `decode_row` reads it.
        Err(EnvelopeRefusal::NativeSubset(EnvelopeLimit::NegativeZeroSeq)) if index != 0 => {
            Some(RecoverableRefusal::Gap { turn_end })
        }
        Err(EnvelopeRefusal::NativeSubset(EnvelopeLimit::NegativeZeroSeq))
            if row["type"] == "session/end-seed" && !row["data"].is_object() =>
        {
            Some(RecoverableRefusal::Thrown)
        }
        _ => None,
    }
}

/// Whether the released v2 codec's recoverable `decodeRow`, holding an
/// issue, rethrows it at `row`: `decodeEvent` admits the row and its type is
/// `turn/end`. The gap check does not run once an issue is held. `Err`
/// names a limit of [`migrate_v2_rows`] where `decodeEvent`'s outcome
/// depends on a number spelling.
pub(crate) fn rethrows_recovery_issue(row: &Value, budget: usize) -> Result<bool, String> {
    if row.get("type").is_none_or(|kind| kind != "turn/end") {
        return Ok(false);
    }
    let row = Deep::new(js_order(clone_value(row)));
    match decode_row_envelope(&row, 0, budget) {
        Ok(_)
        | Err(
            EnvelopeRefusal::Rejected(
                EnvelopeRejection::SeqGap { .. } | EnvelopeRejection::EndSeedDataNotObject,
            )
            // Only the gap check reads these, and `decodeEvent` passed first.
            | EnvelopeRefusal::NativeSubset(
                EnvelopeLimit::NegativeZeroSeq | EnvelopeLimit::SeqDiagnostic,
            ),
        ) => Ok(true),
        // Without `sourceEventSeqs`, only the gap check reads the seq.
        Err(EnvelopeRefusal::NativeSubset(EnvelopeLimit::FloatLexeme(NumberField::Seq)))
            if !row
                .as_object()
                .is_some_and(|fields| fields.contains_key("sourceEventSeqs")) =>
        {
            Ok(true)
        }
        Err(EnvelopeRefusal::Rejected(_)) => Ok(false),
        Err(EnvelopeRefusal::NativeSubset(limit)) => Err(limit_name(limit).to_owned()),
        Err(EnvelopeRefusal::ExpectedSeqOutOfRange) => Err("row-count".to_owned()),
    }
}

/// The codec's exact message. The row is in JavaScript order, so its first
/// unexpected member is the one TypeScript names, and an unsafe integer seq
/// renders as JavaScript's nearest double.
fn row_message(row: &Value, rejection: &EnvelopeRejection, expected: u64) -> String {
    match rejection {
        EnvelopeRejection::UnexpectedFields { .. } => {
            let key = row
                .as_object()
                .and_then(|fields: &Map<String, Value>| {
                    fields
                        .keys()
                        .find(|key| !ENVELOPE_KEYS.contains(&key.as_str()))
                })
                .cloned()
                .unwrap_or_default();
            format!("released v2 row {expected} has unexpected field {key}")
        }
        EnvelopeRejection::SeqGap { got: None } => {
            let got = match &row["seq"] {
                Value::Number(number) => integer_string(number).unwrap_or_default(),
                _ => String::new(),
            };
            format!("released v2 row {expected} has seq gap (expected {expected}, got {got})")
        }
        rejection => rejection
            .message(expected)
            .unwrap_or_else(|| unreachable!("every other rejection has an exact message")),
    }
}

const fn limit_name(limit: EnvelopeLimit) -> &'static str {
    match limit {
        EnvelopeLimit::FloatLexeme(NumberField::Time) => "time-float-lexeme",
        EnvelopeLimit::FloatLexeme(NumberField::Seq) => "seq-float-lexeme",
        EnvelopeLimit::NegativeZeroSeq => "negative-zero-seq",
        EnvelopeLimit::SeqDiagnostic => "seq-diagnostic",
        EnvelopeLimit::Source(SourceEventSeqsLimit::FloatLexeme { .. }) => "source-float-lexeme",
        EnvelopeLimit::Source(SourceEventSeqsLimit::OutputBudget) => "source-output-budget",
    }
}

fn codec(location: V2ToV3Location, error: StageError) -> V2ToV3Refusal {
    match error {
        StageError::Invalid(message) | StageError::Unsupported(message) => {
            V2ToV3Refusal::Rejected {
                location,
                layer: V2ToV3Layer::Codec,
                message,
            }
        }
        StageError::NativeLimit(limit) => V2ToV3Refusal::NativeSubset { location, limit },
    }
}

fn codec_message(message: &str) -> V2ToV3Refusal {
    V2ToV3Refusal::Rejected {
        location: V2ToV3Location::Finish,
        layer: V2ToV3Layer::Codec,
        message: message.to_owned(),
    }
}

/// The chain's `throwUnsupportedRefusal`: unsupported errors pass through,
/// ordinary ones are wrapped with the migration's name.
fn migration(location: V2ToV3Location, error: StageError) -> V2ToV3Refusal {
    let message = match error {
        StageError::Invalid(detail) => {
            format!("{MIGRATION} refuses this format v2 Session: {detail}")
        }
        StageError::Unsupported(message) => message,
        StageError::NativeLimit(limit) => {
            return V2ToV3Refusal::NativeSubset { location, limit };
        }
    };
    V2ToV3Refusal::Rejected {
        location,
        layer: V2ToV3Layer::Migration,
        message,
    }
}
