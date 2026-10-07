//! Development-only decoder for one already parsed event row's envelope.
//!
//! [`decode_row_envelope`] reproduces one strict
//! `releasedV2SessionFormatCodec.createDecoder(header, 'strict').decodeRow(row)`
//! call from `packages/session/session-format-v1-to-v2/src/codec.ts`, made
//! after the decoder admitted rows 0 through `expected_seq - 1`. It checks the
//! envelope fields, decodes `sourceEventSeqs`, checks the seq gap, and requires
//! a `session/end-seed` row's data to be an object. The result borrows `data`
//! and `surfaceOp` without validating them.
//!
//! This is not current-format row admission. The V3 codec checks some
//! structural payloads before this step and known-event envelope and payload
//! rules after it; a decoded envelope can still
//! be invalid V3. Recovery modes, the seeded and end-seed checks of `finish`,
//! framing, and replay are outside this decoder.

use serde_json::{Map, Value};

use crate::source_event_seqs::{
    SourceEventSeqsLimit, SourceEventSeqsRefusal, SourceEventSeqsRejection,
    decode_source_event_seqs,
};

const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
const OPTIONAL_KEYS: [&str; 3] = ["ignorable", "sourceEventSeqs", "surfaceOp"];

/// An envelope the released v2 decoder emits. It is metadata over borrowed
/// row values, not an admitted current-format event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnadmittedEnvelope<'a> {
    /// Any string, including types the current format does not know.
    pub event_type: &'a str,
    /// Always the caller's expected seq.
    pub seq: u64,
    /// A safe integer; never -0.
    pub time: i64,
    /// `true` exactly when the row carries `"ignorable": true`.
    pub ignorable: bool,
    /// The expanded field, or `None` when the row omits it. Present and empty
    /// lists are kept, though the current format refuses them.
    pub source_event_seqs: Option<Vec<u64>>,
    /// The row's value, unvalidated: `Some(Value::Null)` for a present JSON `null`.
    pub surface_op: Option<&'a Value>,
    /// The row's value, unvalidated and not projected.
    pub data: &'a Value,
}

/// Why a row's envelope was not decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeRefusal {
    /// The caller's expected seq exceeds this API's safe-integer bound of
    /// 2^53 − 1. This is a caller error, not a format error.
    ExpectedSeqOutOfRange,
    /// TypeScript throws `SessionFormatError`; see [`EnvelopeRejection::message`].
    Rejected(EnvelopeRejection),
    /// This crate cannot reproduce the TypeScript outcome; nothing is claimed.
    NativeSubset(EnvelopeLimit),
}

/// A required envelope field, in the order TypeScript checks presence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredField {
    Type,
    Seq,
    Time,
    Data,
}

impl RequiredField {
    const ALL: [Self; 4] = [Self::Type, Self::Seq, Self::Time, Self::Data];

    /// The field's JSON key.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Type => "type",
            Self::Seq => "seq",
            Self::Time => "time",
            Self::Data => "data",
        }
    }
}

/// The released v2 decoder's row errors, in check order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeRejection {
    /// The row is an array, `null`, a string, a number, or a boolean.
    NotObject,
    /// The first required field the row omits; JSON `null` is present.
    MissingField(RequiredField),
    /// Keys outside the seven envelope fields, in byte order. TypeScript
    /// names the first in JavaScript property order, which a parsed `Value`
    /// does not retain, so the message is exact only for one key.
    UnexpectedFields {
        keys: Vec<String>,
    },
    TypeNotString,
    /// `time` is not a safe integer, or is -0.
    InvalidTime,
    /// `ignorable` is present and is not `true`, including JSON `null`.
    IgnorableNotTrue,
    /// `sourceEventSeqs` is present and `seq` is not a non-negative safe
    /// integer. Without the field, a bad seq reaches the gap check instead.
    InvalidSeq,
    /// `sourceEventSeqs` fails against the row's own seq.
    Source(SourceEventSeqsRejection),
    /// `seq` differs from the expected seq. `got` is JavaScript's `String(seq)`
    /// when this crate renders it: a safe integer, string, `null`, or boolean.
    /// Other integers are certainly gaps, but their rounding is not rendered.
    SeqGap {
        got: Option<String>,
    },
    /// A `session/end-seed` row's data is not an object.
    EndSeedDataNotObject,
}

impl EnvelopeRejection {
    /// TypeScript's exact message, or `None` where this crate claims only the
    /// class. Strict, contiguous decoding makes the row index, the expected
    /// seq, and an accepted event's seq the same number, `expected_seq`.
    pub fn message(&self, expected_seq: u64) -> Option<String> {
        let row = format!("released v2 row {expected_seq}");
        Some(match self {
            Self::NotObject => format!("{row} must be an object"),
            Self::MissingField(field) => format!("{row} lacks required field {}", field.key()),
            Self::UnexpectedFields { keys } => match keys.as_slice() {
                [key] => format!("{row} has unexpected field {key}"),
                _ => return None,
            },
            Self::TypeNotString => format!("{row} type must be a string"),
            Self::InvalidTime => format!("{row} time must be a safe integer"),
            Self::IgnorableNotTrue => format!("{row} ignorable must be true when present"),
            Self::InvalidSeq => format!("{row} seq must be a non-negative safe integer"),
            Self::Source(rejection) => rejection.message().to_owned(),
            Self::SeqGap { got } => {
                format!(
                    "{row} has seq gap (expected {expected_seq}, got {})",
                    got.as_ref()?
                )
            }
            Self::EndSeedDataNotObject => {
                format!("session/end-seed {expected_seq} data must be an object")
            }
        })
    }
}

/// A numeric envelope field whose spelling can decide the outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberField {
    Time,
    Seq,
}

/// Input whose TypeScript outcome this decoder does not reproduce. Each limit
/// fires at the TypeScript check that would read the value, so it may hide a
/// later TypeScript rejection but never an earlier one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeLimit {
    /// serde_json stores the number as an `f64` other than -0: a fraction or
    /// exponent spelling, or an integer outside `i64` and `u64`. JavaScript
    /// accepts integral ones such as `1.0`; this crate does not claim its rounding.
    FloatLexeme(NumberField),
    /// A `-0` or `-0.0` seq without `sourceEventSeqs`. The released v2 decoder
    /// admits it as seq 0 at expected seq 0 and reports a gap elsewhere; the
    /// current format refuses it. This crate admits neither.
    NegativeZeroSeq,
    /// An array or object seq without `sourceEventSeqs` reaches the gap
    /// message, where JavaScript's `String(seq)` may throw a `TypeError`
    /// outside the decoder's error handling. This crate does not convert it.
    SeqDiagnostic,
    /// The `sourceEventSeqs` decoder's own limit.
    Source(SourceEventSeqsLimit),
}

/// Decode one row's envelope as the strict released v2 decoder does after
/// admitting rows 0 through `expected_seq - 1`.
///
/// Checks run in TypeScript's order and the first failure wins: object,
/// required fields, unexpected fields, type, time, ignorable, then, only when
/// `sourceEventSeqs` is present, the row's seq as a count and the field
/// against that seq, then the gap, then end-seed data. `source_budget` bounds
/// the expanded source list only, as [`decode_source_event_seqs`] documents;
/// it does not bound the parsed row. Duplicate keys were already resolved by
/// the parser, which keeps the last value as `JSON.parse` does.
pub fn decode_row_envelope(
    row: &Value,
    expected_seq: u64,
    source_budget: usize,
) -> Result<UnadmittedEnvelope<'_>, EnvelopeRefusal> {
    if expected_seq > MAX_SAFE_INTEGER {
        return Err(EnvelopeRefusal::ExpectedSeqOutOfRange);
    }
    let Value::Object(fields) = row else {
        return rejected(EnvelopeRejection::NotObject);
    };
    if let Some(field) = RequiredField::ALL
        .into_iter()
        .find(|field| !fields.contains_key(field.key()))
    {
        return rejected(EnvelopeRejection::MissingField(field));
    }
    let keys: Vec<String> = fields
        .keys()
        .filter(|key| {
            !RequiredField::ALL.iter().any(|field| field.key() == *key)
                && !OPTIONAL_KEYS.contains(&key.as_str())
        })
        .cloned()
        .collect();
    if !keys.is_empty() {
        return rejected(EnvelopeRejection::UnexpectedFields { keys });
    }
    let Value::String(event_type) = &fields["type"] else {
        return rejected(EnvelopeRejection::TypeNotString);
    };
    let time = safe_integer(&fields["time"])?;
    let ignorable = match fields.get("ignorable") {
        None => false,
        Some(Value::Bool(true)) => true,
        Some(_) => return rejected(EnvelopeRejection::IgnorableNotTrue),
    };
    let source_event_seqs = source_event_seqs(fields, source_budget)?;
    check_gap(&fields["seq"], expected_seq)?;
    let data = &fields["data"];
    if event_type == "session/end-seed" && !data.is_object() {
        return rejected(EnvelopeRejection::EndSeedDataNotObject);
    }
    Ok(UnadmittedEnvelope {
        event_type,
        seq: expected_seq,
        time,
        ignorable,
        source_event_seqs,
        surface_op: fields.get("surfaceOp"),
        data,
    })
}

const fn rejected<T>(rejection: EnvelopeRejection) -> Result<T, EnvelopeRefusal> {
    Err(EnvelopeRefusal::Rejected(rejection))
}

const fn subset<T>(limit: EnvelopeLimit) -> Result<T, EnvelopeRefusal> {
    Err(EnvelopeRefusal::NativeSubset(limit))
}

/// TypeScript's `sessionFormatSafeInteger`. Safe-integer spellings are exact
/// in both runtimes; `-0` and `-0.0` are refused because JavaScript reads -0.
fn safe_integer(value: &Value) -> Result<i64, EnvelopeRefusal> {
    let invalid = rejected(EnvelopeRejection::InvalidTime);
    let Value::Number(number) = value else {
        return invalid;
    };
    if let Some(number) = number.as_i64() {
        return if number.unsigned_abs() <= MAX_SAFE_INTEGER {
            Ok(number)
        } else {
            invalid
        };
    }
    if number.is_u64() || is_negative_zero(number.as_f64()) {
        return invalid;
    }
    subset(EnvelopeLimit::FloatLexeme(NumberField::Time))
}

/// The row's seq as a count, then its `sourceEventSeqs` decoded against that
/// seq. Nothing is read when the field is absent.
fn source_event_seqs(
    fields: &Map<String, Value>,
    budget: usize,
) -> Result<Option<Vec<u64>>, EnvelopeRefusal> {
    let Some(field) = fields.get("sourceEventSeqs") else {
        return Ok(None);
    };
    let row_seq = count(&fields["seq"])?;
    match decode_source_event_seqs(Some(field), row_seq, budget) {
        Ok(seqs) => Ok(seqs),
        Err(SourceEventSeqsRefusal::Rejected(rejection)) => {
            rejected(EnvelopeRejection::Source(rejection))
        }
        Err(SourceEventSeqsRefusal::NativeSubset(limit)) => subset(EnvelopeLimit::Source(limit)),
        // `count` admits only safe integers.
        Err(SourceEventSeqsRefusal::EventSeqOutOfRange) => {
            unreachable!("a counted seq is a safe integer")
        }
    }
}

/// TypeScript's `sessionFormatCount` on the row's seq. Every negative spelling,
/// including `-0` and `-0.0`, is refused exactly.
fn count(value: &Value) -> Result<u64, EnvelopeRefusal> {
    let invalid = rejected(EnvelopeRejection::InvalidSeq);
    let Value::Number(number) = value else {
        return invalid;
    };
    if let Some(number) = number.as_u64() {
        return if number <= MAX_SAFE_INTEGER {
            Ok(number)
        } else {
            invalid
        };
    }
    if number.is_i64() || number.as_f64().is_some_and(f64::is_sign_negative) {
        return invalid;
    }
    subset(EnvelopeLimit::FloatLexeme(NumberField::Seq))
}

/// TypeScript's `event.seq !== eventCount` and the gap message's `String(seq)`.
fn check_gap(seq: &Value, expected_seq: u64) -> Result<(), EnvelopeRefusal> {
    let got = match seq {
        Value::Number(number) => {
            if let Some(number) = number.as_u64() {
                if number == expected_seq {
                    return Ok(());
                }
                (number <= MAX_SAFE_INTEGER).then(|| number.to_string())
            } else if let Some(number) = number.as_i64() {
                (number.unsigned_abs() <= MAX_SAFE_INTEGER).then(|| number.to_string())
            } else if is_negative_zero(number.as_f64()) {
                return subset(EnvelopeLimit::NegativeZeroSeq);
            } else {
                return subset(EnvelopeLimit::FloatLexeme(NumberField::Seq));
            }
        }
        Value::String(text) => Some(text.clone()),
        Value::Null => Some("null".to_owned()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Array(_) | Value::Object(_) => return subset(EnvelopeLimit::SeqDiagnostic),
    };
    rejected(EnvelopeRejection::SeqGap { got })
}

fn is_negative_zero(number: Option<f64>) -> bool {
    number.is_some_and(|number| number == 0.0 && number.is_sign_negative())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn expected_seq_must_be_a_safe_integer() {
        let row = json!({"type": "t", "seq": 0, "time": 0, "data": {}});
        assert_eq!(
            decode_row_envelope(&row, MAX_SAFE_INTEGER + 1, 0),
            Err(EnvelopeRefusal::ExpectedSeqOutOfRange)
        );
        assert_eq!(
            decode_row_envelope(&json!(null), u64::MAX, 0),
            Err(EnvelopeRefusal::ExpectedSeqOutOfRange)
        );
    }

    #[test]
    fn a_row_at_the_largest_expected_seq_decodes() {
        let row = json!({"type": "t", "seq": MAX_SAFE_INTEGER, "time": 0, "data": {}});
        let envelope = decode_row_envelope(&row, MAX_SAFE_INTEGER, 0).expect("decodes");
        assert_eq!(envelope.seq, MAX_SAFE_INTEGER);
    }

    #[test]
    fn serde_reads_negative_zero_as_a_signed_float() {
        // The shared cases rely on this for time, seq, and source members.
        for text in ["-0", "-0.0", "-0e0"] {
            let value: Value = serde_json::from_str(text).expect("json");
            assert!(!value.is_i64() && !value.is_u64(), "{text}");
            assert!(is_negative_zero(value.as_f64()), "{text}");
        }
    }
}
