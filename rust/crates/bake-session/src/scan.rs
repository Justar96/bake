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

use crate::json_parse::Deep;
use crate::v3_row::{Admission, admit_v3_row};
use crate::{
    HeaderRefusal, PathPlatform, Rejection, SessionHeader, StructuralRejection, V3CodecEvent,
    V3Limit, V3Rejection, V3RowRefusal, V3Unsupported, decode_v3_row, dismantle, first_record,
    parse_json, read_header_record,
};

/// The header, decoded event prefix, inherited cut, and committed byte
/// offset of a scanned log.
///
/// The scan keeps each decoded row as parsed JSON; [`ScannedLog::events`]
/// decodes them again on demand rather than storing a second copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedLog {
    header: SessionHeader,
    rows: Deep<Vec<Value>>,
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
        (0u64..).zip(self.rows.iter()).map(|(seq, row)| {
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
    /// included, in plaintext bytes. For compressed input this is not a file
    /// truncation offset; use [`crate::RestoredLog::torn`] for physical recovery.
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
    /// The parser refused input not proven invalid for `JSON.parse`: a lone
    /// surrogate escape or a number outside the `f64` range.
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
    let mut scanner = LogScanner::new(record, platform, source_budget)?;
    scanner.feed(&log[record.len()..])?;
    scanner.finish()
}

/// Incremental plaintext scan. Complete-frame checks intentionally precede
/// `finish`, whose inherited-marker check can otherwise hide a frame failure.
pub(crate) struct LogScanner {
    header: SessionHeader,
    rows: Deep<Vec<Value>>,
    inherited: Option<u64>,
    issue: Option<ScanIssue>,
    committed_bytes: usize,
    input_bytes: usize,
    line: u64,
    fragment: Vec<u8>,
    source_budget: usize,
}

impl LogScanner {
    pub(crate) fn new(
        record: &[u8],
        platform: PathPlatform,
        source_budget: usize,
    ) -> Result<Self, ScanRefusal> {
        let header = read_header_record(record, platform).map_err(ScanRefusal::Header)?;
        Ok(Self {
            header,
            rows: Deep::new(Vec::new()),
            inherited: None,
            issue: None,
            committed_bytes: record.len(),
            input_bytes: record.len(),
            line: 0,
            fragment: Vec::new(),
            source_budget,
        })
    }

    pub(crate) fn feed(&mut self, chunk: &[u8]) -> Result<(), ScanRefusal> {
        let chunk_start = self.input_bytes;
        self.input_bytes += chunk.len();
        let mut start = 0;
        for (end, _) in chunk.iter().enumerate().filter(|(_, byte)| **byte == b'\n') {
            if self.fragment.is_empty() {
                self.consume(&chunk[start..end], chunk_start + end + 1)?;
            } else {
                let mut fragment = std::mem::take(&mut self.fragment);
                fragment.extend_from_slice(&chunk[start..end]);
                self.consume(&fragment, chunk_start + end + 1)?;
                fragment.clear();
                self.fragment = fragment;
            }
            start = end + 1;
        }
        self.fragment.extend_from_slice(&chunk[start..]);
        Ok(())
    }

    /// Input bytes, committed plaintext bytes, and decoded row count.
    pub(crate) fn checkpoint(&self) -> (usize, usize, usize) {
        (self.input_bytes, self.committed_bytes, self.rows.len())
    }

    pub(crate) fn finish(self) -> Result<ScannedLog, ScanRefusal> {
        let inherited_event_count = match (self.header.is_seeded, self.inherited) {
            (true, None) => return Err(ScanRefusal::Finish(FinishRejection::SeededWithoutMarker)),
            (false, Some(_)) => {
                return Err(ScanRefusal::Finish(FinishRejection::UnseededWithMarker));
            }
            (_, cut) => cut.unwrap_or(0),
        };
        Ok(ScannedLog {
            header: self.header,
            rows: self.rows,
            inherited_event_count,
            committed_bytes: self.committed_bytes,
            source_budget: self.source_budget,
        })
    }

    fn consume(&mut self, text: &[u8], end_byte: usize) -> Result<(), ScanRefusal> {
        self.line += 1;
        let line = self.line;
        let limit = |limit| ScanRefusal::NativeSubset { line, limit };
        let text = std::str::from_utf8(text).map_err(|_| limit(ScanLimit::InvalidUtf8))?;
        let row: Value = match parse_json(text) {
            Ok(row) => row,
            Err(error) if error.is_syntax() => {
                self.issue.get_or_insert(ScanIssue::Unparsable { line });
                return Ok(());
            }
            Err(_) => return Err(limit(ScanLimit::JsonParser)),
        };
        // A row may nest arbitrarily deep, so every path drops it iteratively.
        match self.decode(line, text, &row) {
            Ok(true) => {
                self.rows.push(row);
                self.committed_bytes = end_byte;
                Ok(())
            }
            Ok(false) => {
                dismantle(row);
                Ok(())
            }
            Err(refusal) => {
                dismantle(row);
                Err(refusal)
            }
        }
    }

    /// Admit and decode one parsed row; `Ok(true)` when it extends the prefix.
    fn decode(&mut self, line: u64, text: &str, row: &Value) -> Result<bool, ScanRefusal> {
        let limit = |limit| ScanRefusal::NativeSubset { line, limit };
        if long_integer_part(text) {
            return Err(limit(ScanLimit::NumberLexeme));
        }
        admit_v3_row(row).map_err(|refusal| match refusal {
            Admission::Structural(rejection) => ScanRefusal::Structural { line, rejection },
            Admission::Unsupported(unsupported) => ScanRefusal::Unsupported { line, unsupported },
            Admission::Limit(codec) => limit(ScanLimit::Codec(codec)),
        })?;
        let turn_end = row.get("type").is_some_and(|value| value == "turn/end");
        if let Some(issue) = &self.issue {
            if turn_end {
                return Err(ScanRefusal::Corrupt {
                    line,
                    issue: issue.clone(),
                });
            }
            return Ok(false);
        }
        let seq = self.rows.len() as u64;
        let marker = row
            .get("type")
            .is_some_and(|value| value == "session/end-seed")
            && row["data"].get("inherited") == Some(&Value::Bool(true));
        match decode_v3_row(row, seq, self.source_budget) {
            Ok(_) => {}
            // The codec reruns the admission that passed above.
            Err(V3RowRefusal::Rejected(V3Rejection::Structural(rejection))) => {
                return Err(ScanRefusal::Structural { line, rejection });
            }
            Err(V3RowRefusal::Rejected(rejection)) => {
                // Only an event rejection follows the v2 marker.
                if marker && matches!(rejection, V3Rejection::Event { .. }) {
                    self.inherited = Some(seq);
                }
                let invalid = ScanIssue::Invalid { line, rejection };
                if turn_end {
                    return Err(ScanRefusal::Corrupt {
                        line,
                        issue: invalid,
                    });
                }
                self.issue = Some(invalid);
                return Ok(false);
            }
            Err(V3RowRefusal::NativeSubset(codec)) => return Err(limit(ScanLimit::Codec(codec))),
            Err(V3RowRefusal::ExpectedSeqOutOfRange) => return Err(limit(ScanLimit::EventCount)),
            Err(V3RowRefusal::Unsupported(unsupported)) => {
                return Err(ScanRefusal::Unsupported { line, unsupported });
            }
        }
        if marker {
            self.inherited = Some(seq);
        }
        Ok(true)
    }
}
/// The most integer-part digits serde_json 1.0.151 rounds as `JSON.parse`
/// does, by [`ScanLimit::NumberLexeme`].
const MAX_INTEGER_DIGITS: usize = 768;

/// Whether `text`, which serde_json parsed, holds a number whose integer part
/// has more than [`MAX_INTEGER_DIGITS`] digits. Digits inside strings, and
/// fraction and exponent digits, do not count.
pub(crate) fn long_integer_part(text: &str) -> bool {
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

#[cfg(test)]
mod chunk_tests {
    use super::*;

    #[test]
    fn every_split_preserves_rows_utf8_and_failure_order() {
        let header = b"{\"type\":\"session\",\"version\":3,\"id\":\"s\",\"createdAt\":1,\"isSeeded\":false,\"delegationDepth\":0}\n";
        let row = "{\"type\":\"x/opaque\",\"seq\":0,\"time\":0,\"data\":\"🦀世界\"}\n";
        let end = "{\"type\":\"turn/end\",\"seq\":1,\"time\":0,\"data\":{\"turn\":1}}\n";
        for tail in [
            row.to_owned(),
            format!("{row}bad\n{end}"),
            format!("{row}torn"),
            format!("{row}\u{fffd}\n"),
        ] {
            let bytes = [header.as_slice(), tail.as_bytes()].concat();
            let expected = scan_log(&bytes, PathPlatform::Posix, 64);
            for split in 0..=tail.len() {
                let mut scanner = LogScanner::new(header, PathPlatform::Posix, 64).unwrap();
                let actual = scanner
                    .feed(&tail.as_bytes()[..split])
                    .and_then(|()| scanner.feed(&tail.as_bytes()[split..]))
                    .and_then(|()| scanner.finish());
                assert_eq!(actual, expected, "split {split} in {tail:?}");
            }
        }
    }
}
