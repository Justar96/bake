//! Development-only current-format row encoder: the exact text TypeScript
//! writes for one Session header record and for one event row.
//!
//! [`encode_header_line`] reproduces `JSON.stringify(toHeaderLine(header,
//! inheritedEventCount))` and [`encode_event_line`] reproduces
//! `eventLine(event)`, both from
//! `packages/session/session-persistence-jsonl/src/format.ts`. Neither adds
//! the record's LF. The header encoder ports the checks of `toHeaderLine`, the
//! catalog's `encodeCurrentHeader`, and the released v3 and v2 codecs'
//! `encodeHeader`. The event encoder ports `encodeSeqRanges` and admits the
//! row it builds with [`decode_v3_row`]: the strict V3 decoder runs every
//! check the encoder's `assertV3EventAdmission` and `assertV3Event` run, on
//! the same values once the source list is encoded, plus the source-range and
//! end-seed data checks, which this module settles before or after decoding.
//!
//! [`EncodeRefusal::Unadmitted`] claims only that TypeScript throws; no error
//! class or message is claimed. Where JavaScript's own semantics decide the
//! outcome, or TypeScript writes a row its own strict decoder refuses, the
//! encoder returns an [`EncodeLimit`] and claims nothing. A `Value` stands for
//! the value `JSON.parse` returns, its strings in the [`crate::js_string`]
//! spelling, so a lone surrogate is written as `JSON.stringify` writes it, a
//! lowercase `\udxxx` escape; the returned line is the text itself. Nothing
//! is read from or written to a file.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::json_parse::{Deep, clone_fields, dismantle};
use crate::json_text::{is_writer_spelling, json_text};
use crate::v3_row::{Vocabulary, vocabulary};
use crate::{
    CURRENT_SESSION_FORMAT_VERSION, Count, EnvelopeRejection, MAX_SAFE_INTEGER, PathPlatform,
    V3Rejection, V3RowRefusal, count, decode_v3_row, is_absolute,
};

/// The logical header's required members, as `assertReleasedV2Header` lists them.
const HEADER_REQUIRED: [&str; 5] = ["version", "id", "createdAt", "isSeeded", "delegationDepth"];
const HEADER_OPTIONAL: [&str; 4] = ["cwd", "parentSession", "origin", "agentPreset"];

/// Why no row was encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeRefusal {
    /// TypeScript throws. Neither its error class nor its message is claimed.
    Unadmitted,
    /// This crate does not reproduce the TypeScript outcome; nothing is claimed.
    NativeSubset(EncodeLimit),
}

/// Input whose TypeScript outcome this encoder does not reproduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeLimit {
    /// A number read as a count (a seq, a header count, or a source list
    /// member) that serde_json holds as an `f64` spelled other than
    /// `JSON.stringify` writes its value, such as `1.0` or `1e3`, where
    /// JavaScript admits the integral ones. A count spelled as a writer
    /// spells it is decided by value, and every number outside a count is
    /// written as `JSON.stringify` writes it.
    FloatNumber,
    /// The event or header is `null`; TypeScript reads a property of it and
    /// throws a `TypeError`.
    TypeError,
    /// An unknown or ignorable obsolete type's `sourceEventSeqs` is not an
    /// array of numbers. `assertV3Event` checks no such list, and
    /// `encodeSeqRanges` coerces its members or calls a missing `some`.
    SourceCoercion,
    /// TypeScript may write a row its own strict decoder refuses: an unknown
    /// type's source list holding a negative, repeated, or not earlier seq, or
    /// a `session/end-seed` whose data is not an object. This encoder writes
    /// no such row.
    UnreadableRow,
    /// [`decode_v3_row`] reported a native limit while admitting the row.
    Codec(crate::V3Limit),
}

const fn unadmitted<T>() -> Result<T, EncodeRefusal> {
    Err(EncodeRefusal::Unadmitted)
}

const fn limit<T>(limit: EncodeLimit) -> Result<T, EncodeRefusal> {
    Err(EncodeRefusal::NativeSubset(limit))
}

/// `JSON.stringify(toHeaderLine(header, inheritedEventCount))`.
///
/// `header` is the logical `SessionHeader`: `version` 3, `id`, `createdAt`,
/// `isSeeded`, and optionally `delegationDepth`, `cwd`, `parentSession`,
/// `origin`, and `agentPreset`. An absent or `null` `delegationDepth` is
/// written as 0. A seeded header requires a count; an unseeded one admits only
/// 0 or none. The count is checked, not written: the exact cut lives on the
/// inherited `session/end-seed` event. `cwd` must be absolute for this host's
/// Node `path.isAbsolute`. The members are written in the codec's fixed
/// order, with `type` first and `version` second, whatever the input order.
///
/// Every TypeScript check throws, so a value that certainly fails any of them
/// is [`EncodeRefusal::Unadmitted`] even when another one is undecided.
pub fn encode_header_line(
    header: &Value,
    inherited_event_count: Option<u64>,
) -> Result<String, EncodeRefusal> {
    let fields = match header {
        Value::Object(fields) => fields,
        Value::Null => return limit(EncodeLimit::TypeError),
        // A spread of any other value has no `version`, which is then refused.
        _ => return unadmitted(),
    };
    let Some(Value::Bool(is_seeded)) = fields.get("isSeeded") else {
        // `assertReleasedV2Header` refuses any other value, if nothing earlier throws.
        return unadmitted();
    };
    if *is_seeded && inherited_event_count.is_none() {
        return unadmitted();
    }
    let cut = inherited_event_count.unwrap_or(0);
    if cut > MAX_SAFE_INTEGER || (!is_seeded && cut != 0) {
        return unadmitted();
    }
    // `Some(None)` is a count this crate does not decide.
    let counted = |value: Option<&Value>| match value.and_then(count) {
        Some(Count::Safe(number)) => Some(Some(number)),
        Some(Count::Undecided) if !is_writer_float(value) => Some(None),
        _ => None,
    };
    let version = counted(fields.get("version"));
    // `toHeaderLine` spreads `delegationDepth ?? 0`, so the member is always present.
    let delegation_depth = match fields.get("delegationDepth") {
        None | Some(Value::Null) => Some(Some(0)),
        Some(value) => counted(Some(value)),
    };
    let created_at = counted(fields.get("createdAt"));
    let (Some(version), Some(created_at), Some(delegation_depth)) =
        (version, created_at, delegation_depth)
    else {
        return unadmitted();
    };
    if version.is_some_and(|version| version != CURRENT_SESSION_FORMAT_VERSION) {
        return unadmitted();
    }
    let keys_valid = HEADER_REQUIRED
        .iter()
        .filter(|key| **key != "delegationDepth")
        .all(|key| fields.contains_key(*key))
        && fields.keys().all(|key| {
            HEADER_REQUIRED.contains(&key.as_str()) || HEADER_OPTIONAL.contains(&key.as_str())
        });
    let Some(Value::String(id)) = fields.get("id") else {
        return unadmitted();
    };
    let cwd = optional_string(fields, "cwd")?;
    if cwd.is_some_and(|cwd| !is_absolute(cwd, PathPlatform::host())) {
        return unadmitted();
    }
    let parent_session = optional_string(fields, "parentSession")?;
    let agent_preset = optional_string(fields, "agentPreset")?;
    let origin = match fields.get("origin") {
        None => None,
        Some(Value::String(origin)) if origin == "subagent" => Some(origin),
        Some(_) => return unadmitted(),
    };
    if !keys_valid {
        return unadmitted();
    }
    let (Some(_), Some(created_at), Some(delegation_depth)) =
        (version, created_at, delegation_depth)
    else {
        return limit(EncodeLimit::FloatNumber);
    };
    let mut line = format!(
        "{{\"type\":\"session\",\"version\":{CURRENT_SESSION_FORMAT_VERSION},\"id\":{},\"createdAt\":{created_at}",
        json_string(id)
    );
    if let Some(cwd) = cwd {
        line.push_str(&format!(",\"cwd\":{}", json_string(cwd)));
    }
    if let Some(parent_session) = parent_session {
        line.push_str(&format!(
            ",\"parentSession\":{}",
            json_string(parent_session)
        ));
    }
    line.push_str(&format!(",\"isSeeded\":{is_seeded}"));
    if let Some(origin) = origin {
        line.push_str(&format!(",\"origin\":{}", json_string(origin)));
    }
    line.push_str(&format!(",\"delegationDepth\":{delegation_depth}"));
    if let Some(agent_preset) = agent_preset {
        line.push_str(&format!(",\"agentPreset\":{}", json_string(agent_preset)));
    }
    line.push('}');
    Ok(line)
}

/// `JSON.stringify` of a string as line bytes: a literal U+FDD0 is written
/// once, as `JSON.stringify` writes it.
fn json_string(text: &str) -> String {
    let mut quoted = String::new();
    crate::js_string::push_quoted(&mut quoted, text);
    quoted
}

/// An absent member, or a present string; any other value, `null` included,
/// is refused.
fn optional_string<'a>(
    fields: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, EncodeRefusal> {
    match fields.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => unadmitted(),
    }
}

/// `eventLine(event)`: the current codec's `encodeEvent`, then
/// `JSON.stringify`.
///
/// A logical `sourceEventSeqs` list is range-encoded as `encodeSeqRanges`
/// does: a list that does not strictly increase is copied, and otherwise each
/// run of three or more consecutive seqs becomes `[start, end]` while shorter
/// runs stay single seqs. The row is then admitted with [`decode_v3_row`] at
/// its own seq, and written with object members in JavaScript's own-key
/// order: array-index keys ascending, then the rest in insertion order.
pub fn encode_event_line(event: &Value) -> Result<String, EncodeRefusal> {
    let fields = match event {
        Value::Object(fields) => fields,
        Value::Null => return limit(EncodeLimit::TypeError),
        // `assertV3Event` requires an object.
        _ => return unadmitted(),
    };
    // `assertV3Event` counts every seq, so an invalid one always throws.
    let seq = match fields.get("seq").and_then(count) {
        Some(Count::Safe(seq)) => seq,
        Some(Count::Undecided) if !is_writer_float(fields.get("seq")) => {
            return limit(EncodeLimit::FloatNumber);
        }
        _ => return unadmitted(),
    };
    let Some(Value::String(event_type)) = fields.get("type") else {
        // `assertV3Event` requires a string type.
        return unadmitted();
    };
    // The copy may nest as deep as the event, so it is held to drop without
    // recursing on every exit.
    let mut row = Deep::new(Value::Object(clone_fields(fields)));
    let mut source_budget = 0;
    if let Some(sources) = fields.get("sourceEventSeqs") {
        let seqs = match vocabulary(event_type) {
            // Only `ignorable` may join a known type's required members.
            Vocabulary::Known => return unadmitted(),
            Vocabulary::Surface => surface_sources(sources)?,
            Vocabulary::Opaque => opaque_sources(sources, seq)?,
        };
        source_budget = seqs.len();
        if let Value::Object(members) = &mut *row
            && let Some(logical) =
                members.insert("sourceEventSeqs".to_owned(), encode_seq_ranges(&seqs))
        {
            dismantle(logical);
        }
    }
    match decode_v3_row(&row, seq, source_budget) {
        Ok(_) => {}
        // The released v2 decoder checks end-seed data; the encoder does not.
        Err(V3RowRefusal::Rejected(V3Rejection::Envelope(
            EnvelopeRejection::EndSeedDataNotObject,
        ))) => return limit(EncodeLimit::UnreadableRow),
        Err(V3RowRefusal::NativeSubset(codec)) => return limit(EncodeLimit::Codec(codec)),
        // A refused seq beyond 2^53 − 1 is one TypeScript's count refuses too.
        Err(
            V3RowRefusal::Rejected(_)
            | V3RowRefusal::Unsupported(_)
            | V3RowRefusal::ExpectedSeqOutOfRange,
        ) => return unadmitted(),
    }
    Ok(json_text(&row))
}

/// Whether `value` is an `f64` spelled as `JSON.stringify` writes its value:
/// a fraction or an integer of at least 2^63 in magnitude, which is never a
/// safe count.
fn is_writer_float(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Number(number)) if is_writer_spelling(number))
}

/// A surface's logical list, which `assertV3Event` requires to be a
/// non-empty array of counts. Order, uniqueness, and the earlier-seq rule are
/// left to the decoder, which checks them on the encoded list.
fn surface_sources(sources: &Value) -> Result<Vec<u64>, EncodeRefusal> {
    let Value::Array(members) = sources else {
        return unadmitted();
    };
    if members.is_empty() {
        return unadmitted();
    }
    let counts: Vec<Option<Count>> = members.iter().map(count).collect();
    if counts.iter().any(Option::is_none)
        || members.iter().any(|member| {
            matches!(count(member), Some(Count::Undecided)) && is_writer_float(Some(member))
        })
    {
        return unadmitted();
    }
    counts
        .into_iter()
        .map(|member| match member {
            Some(Count::Safe(seq)) => Ok(seq),
            _ => limit(EncodeLimit::FloatNumber),
        })
        .collect()
}

/// An unknown type's list, which `assertV3Event` does not check. Only a list
/// the strict decoder reads back is encoded: unique safe counts earlier than
/// the row's seq.
fn opaque_sources(sources: &Value, seq: u64) -> Result<Vec<u64>, EncodeRefusal> {
    let Value::Array(members) = sources else {
        return limit(EncodeLimit::SourceCoercion);
    };
    if !members.iter().all(Value::is_number) {
        return limit(EncodeLimit::SourceCoercion);
    }
    let mut seqs = Vec::with_capacity(members.len());
    let mut readable = true;
    for member in members {
        // A writer-spelled fraction or unsafe integer is copied into a list
        // the strict decoder refuses.
        let writer_unsafe = || {
            if is_writer_float(Some(member)) {
                limit(EncodeLimit::UnreadableRow)
            } else {
                limit(EncodeLimit::FloatNumber)
            }
        };
        let Some(number) = member.as_i64() else {
            return writer_unsafe();
        };
        if number.unsigned_abs() > MAX_SAFE_INTEGER {
            return writer_unsafe();
        }
        match u64::try_from(number) {
            Ok(source) if source < seq => seqs.push(source),
            _ => readable = false,
        }
    }
    let mut seen = HashSet::with_capacity(seqs.len());
    if !readable || !seqs.iter().all(|source| seen.insert(*source)) {
        return limit(EncodeLimit::UnreadableRow);
    }
    Ok(seqs)
}

/// `encodeSeqRanges` over safe counts.
fn encode_seq_ranges(values: &[u64]) -> Value {
    if values.windows(2).any(|pair| pair[1] <= pair[0]) {
        return values.iter().copied().map(Value::from).collect();
    }
    let mut output = Vec::new();
    let mut index = 0;
    while let Some(&start) = values.get(index) {
        let mut end = start;
        // Every value is a safe integer, so `end + 1` cannot overflow.
        while values.get(index + 1) == Some(&(end + 1)) {
            index += 1;
            end += 1;
        }
        if end - start >= 2 {
            output.push(Value::from(vec![start, end]));
        } else {
            output.push(Value::from(start));
            if end - start == 1 {
                output.push(Value::from(end));
            }
        }
        index += 1;
    }
    Value::Array(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn seq_ranges_encode_as_typescript_writes_them() {
        assert_eq!(encode_seq_ranges(&[]), json!([]));
        assert_eq!(encode_seq_ranges(&[4]), json!([4]));
        assert_eq!(encode_seq_ranges(&[4, 5]), json!([4, 5]));
        assert_eq!(encode_seq_ranges(&[4, 5, 6]), json!([[4, 6]]));
        assert_eq!(
            encode_seq_ranges(&[0, 1, 3, 4, 5, 6, 8]),
            json!([0, 1, [3, 6], 8])
        );
        assert_eq!(encode_seq_ranges(&[2, 1, 0]), json!([2, 1, 0]));
        assert_eq!(
            encode_seq_ranges(&[MAX_SAFE_INTEGER - 2, MAX_SAFE_INTEGER - 1, MAX_SAFE_INTEGER]),
            json!([[MAX_SAFE_INTEGER - 2, MAX_SAFE_INTEGER]])
        );
    }
}
