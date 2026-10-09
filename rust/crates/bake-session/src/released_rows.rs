//! The header and rows of a plain older-generation Session log, framed and
//! parsed as TypeScript's `decodeStreamingMigration` and `MigratingJsonlRows`
//! in `packages/session/session-persistence-jsonl/src/generation.ts` frame and
//! parse them before any format codec runs.
//!
//! The header is the bytes up to the first LF. It must parse as a JSON object
//! whose `version` is a non-negative safe integer equal to the version the
//! file name selected. The rows are the LF-terminated records after it; the
//! bytes after the last LF are a torn tail that is never parsed. The first
//! row `JSON.parse` rejects is the issue: no later row reaches a codec, and a
//! later row parsing as an object whose `type` is `turn/end` throws the
//! issue. Rows and the header parse at any nesting depth, as `JSON.parse`
//! reads them. Where this crate's parser and `JSON.parse` may disagree, at a
//! lone-surrogate escape or a number beyond the double range, the parse ends
//! at a named limit instead.

use serde_json::Value;

use crate::json_parse::{Deep, parse_json};
use crate::scan::long_integer_part;
use crate::{Count, count};

/// Where parsing ended early.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParseStop {
    /// TypeScript throws a plain `Error` with exactly this message.
    Corrupt(String),
    /// This crate does not decide the TypeScript outcome; the string names
    /// the limit.
    Limit(&'static str),
}

/// The parsed rows before the first issue or stop, and the stop, if any.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReleasedRows {
    /// Rows that reach the format codec, in order, numbered from 0.
    pub(crate) rows: Deep<Vec<Value>>,
    /// What ends the read after `rows` have been decoded and migrated: a
    /// `turn/end` row after the issue, or a limit. Every refusal raised
    /// while decoding or migrating `rows` comes first.
    pub(crate) stop: Option<ParseStop>,
}

/// Parse the header `record`, the first record of a log, LF included, of
/// the generation the file name `source_version` selected.
pub(crate) fn parse_released_header(
    record: &[u8],
    source_version: u64,
) -> Result<Value, ParseStop> {
    let body = record.strip_suffix(b"\n").unwrap_or(record);
    let text = std::str::from_utf8(body).map_err(|_| ParseStop::Limit("header/invalid-utf8"))?;
    let value = match parse_json(text) {
        Ok(value) => Deep::new(value),
        Err(error) if error.is_syntax() => {
            return Err(ParseStop::Corrupt(
                "corrupt session log: header line is not valid JSON".to_owned(),
            ));
        }
        Err(_) => return Err(ParseStop::Limit("header/json-parser")),
    };
    if long_integer_part(text) {
        return Err(ParseStop::Limit("header/number-lexeme"));
    }
    let Value::Object(fields) = &*value else {
        return Err(ParseStop::Corrupt(
            "corrupt session log: first line is not a JSON object".to_owned(),
        ));
    };
    match fields.get("version").and_then(count) {
        None => Err(ParseStop::Corrupt(
            "corrupt session log: header version is not a non-negative safe integer".to_owned(),
        )),
        Some(Count::Undecided) => Err(ParseStop::Limit("header/float-lexeme")),
        Some(Count::Safe(version)) if version != source_version => {
            Err(ParseStop::Corrupt(format!(
                "resolved JSONL source filename identifies v{source_version}, \
                 but its header identifies v{version}"
            )))
        }
        Some(Count::Safe(_)) => Ok(value.into_inner()),
    }
}

/// Parse the rows of `body`, every byte of the log after the header record.
/// The bytes after its last LF are dropped unparsed.
pub(crate) fn parse_released_rows(body: &[u8]) -> ReleasedRows {
    let mut rows = Deep::default();
    let Some(last) = body.iter().rposition(|byte| *byte == b'\n') else {
        return ReleasedRows { rows, stop: None };
    };
    let records = &body[..last];
    let mut issue: Option<u64> = None;
    for (number, record) in (1_u64..).zip(records.split(|byte| *byte == b'\n')) {
        let Ok(text) = std::str::from_utf8(record) else {
            // Node decodes the record with replacement characters.
            return limit(rows, "row/invalid-utf8");
        };
        let row = match parse_json(text) {
            Ok(row) => Deep::new(row),
            Err(error) if error.is_syntax() => {
                issue.get_or_insert(number);
                continue;
            }
            Err(_) => return limit(rows, "row/json-parser"),
        };
        if let Some(first) = issue {
            if row.get("type").is_some_and(|kind| kind == "turn/end") {
                let message = format!("corrupt session log: row {first} is not valid JSON");
                return ReleasedRows {
                    rows,
                    stop: Some(ParseStop::Corrupt(message)),
                };
            }
            continue;
        }
        if long_integer_part(text) {
            return limit(rows, "row/number-lexeme");
        }
        rows.push(row.into_inner());
    }
    ReleasedRows { rows, stop: None }
}

fn limit(rows: Deep<Vec<Value>>, name: &'static str) -> ReleasedRows {
    ReleasedRows {
        rows,
        stop: Some(ParseStop::Limit(name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_end_at_the_last_lf_and_the_first_issue() {
        let parsed = parse_released_rows(b"{\"a\":1}\nnot json\n{\"type\":\"x\"}\n");
        assert_eq!(*parsed.rows, [serde_json::json!({"a": 1})]);
        assert_eq!(parsed.stop, None);
        let parsed = parse_released_rows(b"\n{\"type\":\"turn/end\"}\n");
        assert!(parsed.rows.is_empty());
        assert_eq!(
            parsed.stop,
            Some(ParseStop::Corrupt(
                "corrupt session log: row 1 is not valid JSON".to_owned()
            ))
        );
        assert!(parse_released_rows(b"").rows.is_empty());
        let torn = parse_released_rows(b"{\"a\":1}\n{\"type\":\"turn/end\"}");
        assert_eq!(*torn.rows, [serde_json::json!({"a": 1})]);
        assert_eq!(torn.stop, None);
    }
}
