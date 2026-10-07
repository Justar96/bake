//! Development-only decoder for one event's physical `sourceEventSeqs` field.
//!
//! [`decode_source_event_seqs`] expands the field's compressed list into the
//! source seqs TypeScript's private `decodeSeqRanges` in
//! `packages/session/session-format-v1-to-v2/src/codec.ts` returns, or refuses
//! it with that function's error. The current format's codec delegates this
//! field to the released v2 codec. The decoder admits a field value only: it
//! does not validate the containing row, its payload, the event type's own
//! source rules, or replay.

use std::collections::HashSet;

use serde_json::Value;

const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// Why a `sourceEventSeqs` field value was not decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceEventSeqsRefusal {
    /// The caller's event seq exceeds 2^53 − 1, which TypeScript refuses before
    /// it decodes the field. This is a caller error, not a format error.
    EventSeqOutOfRange,
    /// TypeScript throws `SessionFormatError` with [`SourceEventSeqsRejection::message`].
    Rejected(SourceEventSeqsRejection),
    /// This crate cannot reproduce the TypeScript outcome; no TypeScript error is claimed.
    NativeSubset(SourceEventSeqsLimit),
}

/// The format errors of TypeScript's `decodeSeqRanges`, in check order.
///
/// `entry` is the index in the physical list; `source` is the first expanded
/// seq that fails. TypeScript's messages carry neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceEventSeqsRejection {
    /// The field is not an array, including JSON `null`.
    NotArray,
    /// A scalar member is not a non-negative safe integer.
    InvalidMember { entry: usize },
    /// A nested array does not have exactly two elements.
    RangeNotPair { entry: usize },
    /// A range start is not a non-negative safe integer.
    InvalidRangeStart { entry: usize },
    /// A range end is not a non-negative safe integer.
    InvalidRangeEnd { entry: usize },
    /// A range is reversed, reaches the event seq, or would make the output
    /// longer than the event seq.
    RangeExceedsEventSeq { entry: usize },
    /// After expansion, a seq is not earlier than the event or repeats one before it.
    NotUniqueEarlier { source: u64 },
    /// The list contains a range and its expansion does not strictly increase.
    NotStrictlyIncreasing { source: u64 },
}

impl SourceEventSeqsRejection {
    /// The exact message TypeScript's `SessionFormatError` carries.
    pub const fn message(self) -> &'static str {
        match self {
            Self::NotArray => "sourceEventSeqs must be an array",
            Self::InvalidMember { .. } => {
                "sourceEventSeqs member must be a non-negative safe integer"
            }
            Self::RangeNotPair { .. } => "sourceEventSeqs range must be a [start, end] pair",
            Self::InvalidRangeStart { .. } => {
                "sourceEventSeqs range start must be a non-negative safe integer"
            }
            Self::InvalidRangeEnd { .. } => {
                "sourceEventSeqs range end must be a non-negative safe integer"
            }
            Self::RangeExceedsEventSeq { .. } => "sourceEventSeqs range exceeds its event seq",
            Self::NotUniqueEarlier { .. } => {
                "sourceEventSeqs ranges must contain unique earlier seqs"
            }
            Self::NotStrictlyIncreasing { .. } => {
                "sourceEventSeqs ranges must be strictly increasing"
            }
        }
    }
}

/// Input whose TypeScript outcome this decoder does not reproduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceEventSeqsLimit {
    /// serde_json stores this number as a non-negative `f64`: a fraction or
    /// exponent spelling, including `0.0`, or an integer above `u64::MAX`.
    /// JavaScript accepts the integral ones; this crate does not claim
    /// JavaScript's rounding, so it stops at the first such number.
    FloatLexeme { entry: usize },
    /// The expansion would exceed the caller's output budget. TypeScript has
    /// no such limit and may accept the value or reject it at a later check.
    OutputBudget,
}

/// Decode an event's `sourceEventSeqs` field: `None` for an absent field,
/// otherwise the expanded source seqs.
///
/// A scalar member is kept in place. A two-element array `[start, end]` is an
/// inclusive range; its end must be earlier than `event_seq`, and the total
/// output may not exceed `event_seq` entries once it is expanded. After
/// expansion every seq must be unique and earlier than `event_seq`, and if any
/// range appeared the whole list must strictly increase. Checks and their
/// order follow TypeScript, so the first failing check names the refusal. A
/// JSON `null` is a present non-array value, never an absent field.
///
/// `output_budget` caps the number of expanded seqs. It is checked before each
/// member is added or range expanded, so at most that many seqs enter the
/// output and the uniqueness set. Each entry's member, pair and range checks
/// precede its budget check. Uniqueness and global ordering are checked after
/// expansion, so an exhausted budget precedes them.
/// `event_seq` must be a safe integer, as TypeScript's row decoder requires.
pub fn decode_source_event_seqs(
    field: Option<&Value>,
    event_seq: u64,
    output_budget: usize,
) -> Result<Option<Vec<u64>>, SourceEventSeqsRefusal> {
    if event_seq > MAX_SAFE_INTEGER {
        return Err(SourceEventSeqsRefusal::EventSeqOutOfRange);
    }
    let Some(field) = field else {
        return Ok(None);
    };
    let reject = SourceEventSeqsRefusal::Rejected;
    let Value::Array(entries) = field else {
        return Err(reject(SourceEventSeqsRejection::NotArray));
    };
    let mut output: Vec<u64> = Vec::new();
    let mut has_range = false;
    for (entry, value) in entries.iter().enumerate() {
        let Value::Array(pair) = value else {
            let source = count(
                value,
                entry,
                SourceEventSeqsRejection::InvalidMember { entry },
            )?;
            within_budget(&output, 1, output_budget)?;
            output.push(source);
            continue;
        };
        let [start, end] = pair.as_slice() else {
            return Err(reject(SourceEventSeqsRejection::RangeNotPair { entry }));
        };
        let start = count(
            start,
            entry,
            SourceEventSeqsRejection::InvalidRangeStart { entry },
        )?;
        let end = count(
            end,
            entry,
            SourceEventSeqsRejection::InvalidRangeEnd { entry },
        )?;
        // TypeScript compares against `event_seq - output.length`, which is
        // negative once scalars outnumber the event seq; any range then fails.
        let remaining = event_seq.saturating_sub(output.len() as u64);
        if start > end || end >= event_seq || end - start + 1 > remaining {
            return Err(reject(SourceEventSeqsRejection::RangeExceedsEventSeq {
                entry,
            }));
        }
        // A length that does not fit `usize` exceeds any budget.
        let length = usize::try_from(end - start + 1).map_err(|_| budget_refusal())?;
        within_budget(&output, length, output_budget)?;
        output.extend(start..=end);
        has_range = true;
    }
    let mut seen = HashSet::with_capacity(output.len());
    if let Some(&source) = output
        .iter()
        .find(|&&source| source >= event_seq || !seen.insert(source))
    {
        return Err(reject(SourceEventSeqsRejection::NotUniqueEarlier {
            source,
        }));
    }
    if has_range && let Some(pair) = output.windows(2).find(|pair| pair[1] <= pair[0]) {
        return Err(reject(SourceEventSeqsRejection::NotStrictlyIncreasing {
            source: pair[1],
        }));
    }
    Ok(Some(output))
}

/// TypeScript's `sessionFormatCount`: a non-negative safe integer other than -0.
/// Any negative spelling, including `-0` and `-0.0`, is refused exactly,
/// because JavaScript parses it to a negative number or -0.
fn count(
    value: &Value,
    entry: usize,
    invalid: SourceEventSeqsRejection,
) -> Result<u64, SourceEventSeqsRefusal> {
    let refused = Err(SourceEventSeqsRefusal::Rejected(invalid));
    let Value::Number(number) = value else {
        return refused;
    };
    if let Some(number) = number.as_u64() {
        return if number <= MAX_SAFE_INTEGER {
            Ok(number)
        } else {
            refused
        };
    }
    if number.is_i64() || number.as_f64().is_some_and(f64::is_sign_negative) {
        return refused;
    }
    Err(SourceEventSeqsRefusal::NativeSubset(
        SourceEventSeqsLimit::FloatLexeme { entry },
    ))
}

/// Refuse before adding `additional` seqs would make `output` longer than `budget`.
fn within_budget(
    output: &[u64],
    additional: usize,
    budget: usize,
) -> Result<(), SourceEventSeqsRefusal> {
    if additional > budget.saturating_sub(output.len()) {
        return Err(budget_refusal());
    }
    Ok(())
}

const fn budget_refusal() -> SourceEventSeqsRefusal {
    SourceEventSeqsRefusal::NativeSubset(SourceEventSeqsLimit::OutputBudget)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decode(
        field: Value,
        event_seq: u64,
        budget: usize,
    ) -> Result<Vec<u64>, SourceEventSeqsRefusal> {
        decode_source_event_seqs(Some(&field), event_seq, budget).map(|seqs| seqs.expect("present"))
    }

    fn rejected(rejection: SourceEventSeqsRejection) -> Result<Vec<u64>, SourceEventSeqsRefusal> {
        Err(SourceEventSeqsRefusal::Rejected(rejection))
    }

    #[test]
    fn huge_range_with_tiny_budget_refuses_before_expanding() {
        let field = json!([[0, MAX_SAFE_INTEGER - 1]]);
        assert_eq!(
            decode(field, MAX_SAFE_INTEGER, 16),
            Err(SourceEventSeqsRefusal::NativeSubset(
                SourceEventSeqsLimit::OutputBudget
            ))
        );
    }

    #[test]
    fn budget_admits_exactly_its_length() {
        let scalars: Vec<u64> = (0..40).collect();
        assert_eq!(decode(json!(scalars), 64, 40), Ok(scalars.clone()));
        assert_eq!(decode(json!(scalars), 64, 39), Err(budget_refusal()));
        let mixed = json!([0, [1, 9], 10]);
        assert_eq!(decode(mixed.clone(), 64, 11), Ok((0..=10).collect()));
        assert_eq!(decode(mixed, 64, 10), Err(budget_refusal()));
    }

    #[test]
    fn event_seq_must_be_a_safe_integer_even_when_absent() {
        let out_of_range = Err(SourceEventSeqsRefusal::EventSeqOutOfRange);
        assert_eq!(
            decode_source_event_seqs(None, MAX_SAFE_INTEGER + 1, 0),
            out_of_range
        );
        assert_eq!(
            decode_source_event_seqs(Some(&json!([])), u64::MAX, 0),
            out_of_range
        );
        assert_eq!(
            decode_source_event_seqs(None, MAX_SAFE_INTEGER, 0),
            Ok(None)
        );
    }

    #[test]
    fn refusals_name_the_failing_entry_or_source() {
        assert_eq!(
            decode(json!([1, "x"]), 9, 9),
            rejected(SourceEventSeqsRejection::InvalidMember { entry: 1 })
        );
        assert_eq!(
            decode(json!([1, [2]]), 9, 9),
            rejected(SourceEventSeqsRejection::RangeNotPair { entry: 1 })
        );
        assert_eq!(
            decode(json!([[null, 2]]), 9, 9),
            rejected(SourceEventSeqsRejection::InvalidRangeStart { entry: 0 })
        );
        assert_eq!(
            decode(json!([0, [1, -2]]), 9, 9),
            rejected(SourceEventSeqsRejection::InvalidRangeEnd { entry: 1 })
        );
        assert_eq!(
            decode(json!([0, [3, 2]]), 9, 9),
            rejected(SourceEventSeqsRejection::RangeExceedsEventSeq { entry: 1 })
        );
        assert_eq!(
            decode(json!([4, 2, 4, 9]), 9, 9),
            rejected(SourceEventSeqsRejection::NotUniqueEarlier { source: 4 })
        );
        assert_eq!(
            decode(json!([1, 9, 1]), 9, 9),
            rejected(SourceEventSeqsRejection::NotUniqueEarlier { source: 9 })
        );
        assert_eq!(
            decode(json!([[0, 1], 5, 3]), 9, 9),
            rejected(SourceEventSeqsRejection::NotStrictlyIncreasing { source: 3 })
        );
        let float: Value = serde_json::from_str("[0, [1, 2.0]]").expect("json");
        assert_eq!(
            decode(float, 9, 9),
            Err(SourceEventSeqsRefusal::NativeSubset(
                SourceEventSeqsLimit::FloatLexeme { entry: 1 }
            ))
        );
    }

    #[test]
    fn scalars_alone_skip_the_order_check() {
        assert_eq!(decode(json!([5, 0, 3]), 9, 9), Ok(vec![5, 0, 3]));
    }
}
