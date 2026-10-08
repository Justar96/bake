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
use crate::source_event_seqs::SourceEventSeqsLimit;
pub(crate) use admission::OBJECT_PROTOTYPE_NAMES;
pub(crate) use header::contains_negative_zero;
pub(crate) use js::{integer_string, js_order};
pub(crate) use payload_semantics::{
    Checked, assert_released_payload_semantics, count, invalid, quote, released_keys,
    released_record, stringify,
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
#[derive(Debug, Clone, PartialEq)]
pub struct MigratedV2 {
    /// The logical v3 header, without `type`: `version` 3 and the released v2
    /// fields in codec order, with an `agentPreset` of `code` renamed `ptc`.
    pub header: Value,
    /// Target events in order, with expanded `sourceEventSeqs` lists.
    pub events: Vec<Value>,
    /// The number of target events inherited from the parent Session.
    pub inherited_event_count: u64,
}

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
    /// `payload-float-lexeme` for the frozen payload checks,
    /// `object-prototype-type` for inherited JavaScript property names,
    /// `content-kind-diagnostic` for unsupported JSON number rendering, and
    /// `unsafe-json-integer` for retained integer values outside ±(2^53 − 1).
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
/// passes codec and migration checks, retained integer values outside the
/// JavaScript safe range are refused rather than claiming matching precision.
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

pub(crate) fn contains_unsafe_integer(value: &Value) -> bool {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Number(number) => {
                let magnitude = number
                    .as_i64()
                    .map(i64::unsigned_abs)
                    .or_else(|| number.as_u64());
                if magnitude.is_some_and(|value| value > crate::MAX_SAFE_INTEGER) {
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
    let row = js_order(row.clone());
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
    let Value::Object(mut event) = row else {
        unreachable!("an admitted envelope is an object")
    };
    if let Some(sources) = sources {
        // Replacing a member keeps its position, as the codec's spread does.
        event.insert(
            "sourceEventSeqs".to_owned(),
            Value::Array(sources.into_iter().map(Value::from).collect()),
        );
    }
    Ok(Value::Object(event))
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
