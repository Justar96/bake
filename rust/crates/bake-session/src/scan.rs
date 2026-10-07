//! Development-only scan of one plain, uncompressed current-format Session
//! log held in memory, as TypeScript's `scanLog` in
//! `packages/session/session-persistence-jsonl/src/format.ts` scans it.
//!
//! `scanLog` reads the bytes up to the first LF as the header record, then
//! feeds the rest to a `SessionLogScanner` in its default `recoverable` mode.
//! Each LF ends one event record; the bytes after the last LF are a torn tail
//! that is never parsed. The scanner keeps the contiguous prefix of rows the
//! strict V3 codec decodes and the byte offset after the last of them. Its
//! first unparsable or codec-invalid record is an issue: later records are
//! still parsed and raw-admitted, a row whose `type` is `turn/end` rethrows
//! the issue, and the codec decodes nothing more. The codec's `finish` then
//! checks the inherited end-seed marker against the header.
//!
//! The released v2 decoder inside the codec records an inherited marker
//! before the V3 event checks run. A `session/end-seed` row that passes the
//! v2 checks and fails a V3 one therefore still sets the cut `finish` reads,
//! although it ends the prefix. Only that issue row can do so, because no
//! later row is decoded.
//!
//! [`scan_log`] neither reads a file nor decompresses one. A default Session
//! log is Zstd-compressed, so this scan does not read it.

use serde_json::Value;

use crate::v3_row::{Admission, admit_v3_row};
use crate::{
    HeaderRefusal, PathPlatform, Rejection, SessionHeader, StructuralRejection, V3CodecEvent,
    V3Limit, V3Rejection, V3RowRefusal, V3Unsupported, decode_v3_row, first_record,
    is_syntax_error, read_header_record,
};

/// The header, decoded event prefix, inherited cut, and committed byte
/// offset of a scanned log.
///
/// The scan keeps each decoded row as parsed JSON; [`ScannedLog::events`]
/// decodes them again on demand rather than storing a second copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedLog {
    header: SessionHeader,
    rows: Vec<Value>,
    inherited_event_count: u64,
    committed_bytes: usize,
    source_budget: usize,
}

impl ScannedLog {
    /// The header's logical metadata.
    pub const fn header(&self) -> &SessionHeader {
        &self.header
    }

    /// The parsed rows the codec decoded, in log order and numbered from 0,
    /// before any range in their `sourceEventSeqs` is expanded.
    pub fn rows(&self) -> &[Value] {
        &self.rows
    }

    /// The codec's output for each row of [`ScannedLog::rows`].
    pub fn events(&self) -> impl Iterator<Item = V3CodecEvent<'_>> {
        (0u64..).zip(&self.rows).map(|(seq, row)| {
            decode_v3_row(row, seq, self.source_budget)
                .expect("the scan decoded this immutable row with the same budget")
        })
    }

    /// The seq of the last inherited `session/end-seed` marker that passed
    /// the v2 checks, which may be the row that ended the prefix, or 0 for an
    /// unseeded log.
    pub const fn inherited_event_count(&self) -> u64 {
        self.inherited_event_count
    }

    /// The length of the header record and every decoded row's record, LF
    /// included: the offset after which the log can be truncated.
    pub const fn committed_bytes(&self) -> usize {
        self.committed_bytes
    }
}

/// Why [`scan_log`] returned no scan.
///
/// `line` numbers event records from 1, after the header record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanRefusal {
    /// The header record was refused, as by [`read_header_record`]. A log
    /// without an LF is [`Rejection::Framing`].
    Header(HeaderRefusal),
    /// Raw-row admission rejected the record. TypeScript throws
    /// `SessionFormatError`, even after an issue.
    Structural {
        line: u64,
        rejection: StructuralRejection,
    },
    /// Raw-row admission refused the record as unsupported. TypeScript throws
    /// `SessionFormatUnsupportedError`, even after an issue.
    Unsupported {
        line: u64,
        unsupported: V3Unsupported,
    },
    /// The `turn/end` record at `line` rethrew the scan's first issue, a
    /// plain `Error`. `line` is the issue's own line when that record is
    /// itself an invalid `turn/end`.
    Corrupt { line: u64, issue: ScanIssue },
    /// The codec's `finish` threw `SessionFormatError`.
    Finish(FinishRejection),
    /// This crate cannot reproduce the TypeScript outcome of the record at
    /// `line`; nothing is claimed about it or any later record.
    NativeSubset { line: u64, limit: ScanLimit },
}

impl ScanRefusal {
    /// TypeScript's exact message, or `None` for a header refusal, a native
    /// limit, or a rejection whose message this crate does not render.
    pub fn message(&self) -> Option<String> {
        match self {
            Self::Header(_) | Self::NativeSubset { .. } => None,
            Self::Structural { rejection, .. } => {
                V3Rejection::Structural(rejection.clone()).message(0)
            }
            Self::Unsupported { unsupported, .. } => Some(unsupported.message()),
            Self::Corrupt { issue, .. } => issue.message(),
            Self::Finish(rejection) => Some(rejection.message().to_owned()),
        }
    }
}

/// The first record that ended the decoded prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanIssue {
    /// `JSON.parse` rejects the record.
    Unparsable { line: u64 },
    /// The strict V3 codec rejects the row. Every earlier record decoded, so
    /// the row's expected seq is `line - 1`.
    Invalid { line: u64, rejection: V3Rejection },
}

impl ScanIssue {
    /// TypeScript's exact message, or `None` where [`V3Rejection::message`]
    /// claims only the class.
    pub fn message(&self) -> Option<String> {
        match self {
            Self::Unparsable { line } => Some(format!(
                "corrupt session log: unparsable committed event at line {line}"
            )),
            Self::Invalid { line, rejection } => {
                let detail = rejection.message(line - 1)?;
                Some(format!(
                    "corrupt session log: invalid committed event at line {line}: {detail}"
                ))
            }
        }
    }
}

/// The codec's `finish` checks of the inherited end-seed marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishRejection {
    SeededWithoutMarker,
    UnseededWithMarker,
}

impl FinishRejection {
    /// TypeScript's exact message.
    pub const fn message(self) -> &'static str {
        match self {
            Self::SeededWithoutMarker => {
                "released v2 seeded Session lacks an inherited end-seed marker"
            }
            Self::UnseededWithMarker => {
                "released v2 unseeded Session contains an inherited end-seed marker"
            }
        }
    }
}

/// A record whose TypeScript outcome this crate does not reproduce.
///
/// A limit refuses the scan even after an issue, where TypeScript may ignore
/// the record or rethrow the issue; it never hides an earlier outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanLimit {
    /// The record is not UTF-8. Node decodes it with replacement characters.
    InvalidUtf8,
    /// serde_json refused input not proven invalid for `JSON.parse`, such as
    /// a lone surrogate escape, nesting deeper than 128, or a number outside
    /// the `f64` range.
    JsonParser,
    /// A number's integer part has more than 768 digits. serde_json 1.0.151
    /// keeps 768 significant digits and treats any further digit as nonzero
    /// without trimming an integer part's trailing zeros, so it can round an
    /// exact halfway value up where `JSON.parse` rounds to even. Every such
    /// lexeme is refused, whatever its value.
    NumberLexeme,
    /// A raw-admission or codec limit of [`decode_v3_row`].
    Codec(V3Limit),
    /// The row would be event 2^53 or later, beyond the codec's safe counts.
    EventCount,
}

/// Scan a plain current-format log as TypeScript's `scanLog` does.
///
/// Header, then each LF-terminated record in order: `JSON.parse`, raw-row
/// admission, then, until the first issue, the strict V3 codec. A codec
/// rejection marks an inherited end-seed marker before it fails, as
/// TypeScript's released v2 decoder records the marker before the V3 event
/// checks run. Then `finish`. `source_budget` bounds each row's expanded
/// `sourceEventSeqs` as in [`decode_v3_row`]. serde_json parses each record
/// with its default 128-level recursion limit, and a parsed record is checked
/// for [`ScanLimit::NumberLexeme`] before admission; the final unterminated
/// bytes are neither parsed nor bounded.
pub fn scan_log(
    log: &[u8],
    platform: PathPlatform,
    source_budget: usize,
) -> Result<ScannedLog, ScanRefusal> {
    let record = first_record(log).ok_or(ScanRefusal::Header(HeaderRefusal::Rejected(
        Rejection::Framing,
    )))?;
    let header = read_header_record(record, platform).map_err(ScanRefusal::Header)?;
    let mut rows = Vec::new();
    let mut inherited = None;
    let mut issue: Option<ScanIssue> = None;
    let mut committed_bytes = record.len();
    let mut start = record.len();
    for (line, end) in (1u64..).zip(
        log.iter()
            .enumerate()
            .skip(start)
            .filter_map(|(index, byte)| (*byte == b'\n').then_some(index)),
    ) {
        let text = &log[start..end];
        start = end + 1;
        let limit = |limit| ScanRefusal::NativeSubset { line, limit };
        let text = std::str::from_utf8(text).map_err(|_| limit(ScanLimit::InvalidUtf8))?;
        let row: Value = match serde_json::from_str(text) {
            Ok(row) => row,
            Err(error) if is_syntax_error(&error) => {
                issue.get_or_insert(ScanIssue::Unparsable { line });
                continue;
            }
            Err(_) => return Err(limit(ScanLimit::JsonParser)),
        };
        if long_integer_part(text) {
            return Err(limit(ScanLimit::NumberLexeme));
        }
        admit_v3_row(&row).map_err(|refusal| match refusal {
            Admission::Structural(rejection) => ScanRefusal::Structural { line, rejection },
            Admission::Unsupported(unsupported) => ScanRefusal::Unsupported { line, unsupported },
            Admission::Limit(codec) => limit(ScanLimit::Codec(codec)),
        })?;
        let turn_end = row.get("type").is_some_and(|value| value == "turn/end");
        if let Some(issue) = &issue {
            if turn_end {
                return Err(ScanRefusal::Corrupt {
                    line,
                    issue: issue.clone(),
                });
            }
            continue;
        }
        let seq = rows.len() as u64;
        let marker = row
            .get("type")
            .is_some_and(|value| value == "session/end-seed")
            && row["data"].get("inherited") == Some(&Value::Bool(true));
        match decode_v3_row(&row, seq, source_budget) {
            Ok(_) => {}
            // The codec reruns the admission that passed above.
            Err(V3RowRefusal::Rejected(V3Rejection::Structural(rejection))) => {
                return Err(ScanRefusal::Structural { line, rejection });
            }
            Err(V3RowRefusal::Rejected(rejection)) => {
                // Only an event rejection follows the v2 marker.
                if marker && matches!(rejection, V3Rejection::Event { .. }) {
                    inherited = Some(seq);
                }
                let invalid = ScanIssue::Invalid { line, rejection };
                if turn_end {
                    return Err(ScanRefusal::Corrupt {
                        line,
                        issue: invalid,
                    });
                }
                issue = Some(invalid);
                continue;
            }
            Err(V3RowRefusal::NativeSubset(codec)) => return Err(limit(ScanLimit::Codec(codec))),
            Err(V3RowRefusal::ExpectedSeqOutOfRange) => return Err(limit(ScanLimit::EventCount)),
            Err(V3RowRefusal::Unsupported(unsupported)) => {
                return Err(ScanRefusal::Unsupported { line, unsupported });
            }
        }
        if marker {
            inherited = Some(seq);
        }
        rows.push(row);
        committed_bytes = start;
    }
    let inherited_event_count = match (header.is_seeded, inherited) {
        (true, None) => return Err(ScanRefusal::Finish(FinishRejection::SeededWithoutMarker)),
        (false, Some(_)) => return Err(ScanRefusal::Finish(FinishRejection::UnseededWithMarker)),
        (_, cut) => cut.unwrap_or(0),
    };
    Ok(ScannedLog {
        header,
        rows,
        inherited_event_count,
        committed_bytes,
        source_budget,
    })
}

/// The most integer-part digits serde_json 1.0.151 rounds as `JSON.parse`
/// does, by [`ScanLimit::NumberLexeme`].
const MAX_INTEGER_DIGITS: usize = 768;

/// Whether `text`, which serde_json parsed, holds a number whose integer part
/// has more than [`MAX_INTEGER_DIGITS`] digits. Digits inside strings, and
/// fraction and exponent digits, do not count.
fn long_integer_part(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() <= MAX_INTEGER_DIGITS {
        return false;
    }
    let (mut in_string, mut escaped) = (false, false);
    let (mut run, mut integer) = (0, false);
    for (index, &byte) in bytes.iter().enumerate() {
        if in_string {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
        } else if byte.is_ascii_digit() {
            if run == 0 {
                integer = starts_integer_part(&bytes[..index]);
            }
            run += 1;
            if integer && run > MAX_INTEGER_DIGITS {
                return true;
            }
        } else {
            run = 0;
            in_string = byte == b'"';
        }
    }
    false
}

/// Whether a digit run outside strings, after the valid JSON `before`, is a
/// number's integer part rather than its fraction or exponent.
fn starts_integer_part(before: &[u8]) -> bool {
    !matches!(
        before,
        [.., b'.' | b'e' | b'E' | b'+'] | [.., b'e' | b'E', b'-']
    )
}
