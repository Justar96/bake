//! Development-only model of the bytes TypeScript's JSONL backend writes when
//! it creates or appends to a plain, uncompressed current-format Session log.
//!
//! A [`PlainAppendLog`] stands for one write handle of
//! `packages/session/session-persistence-jsonl/src/storage.ts` and the log
//! file it owns, as `compression: 'none'` writes it:
//!
//! - [`PlainAppendLog::create`] is `create(header, { inheritedEventCount })`:
//!   the header must encode as `toHeaderLine` does, and no file exists yet.
//!   An empty `id` is a native limit: TypeScript's `create` admits it in a
//!   root with no project directory, and the first non-empty `append` or
//!   `flush` then throws while resolving the session directory, before the
//!   contiguity check.
//! - [`PlainAppendLog::open`] is a write `open` of existing bytes: the cursor
//!   is the scanned event count, and a log whose committed bytes end before
//!   its last byte keeps that offset as its torn-tail truncation point.
//! - [`PlainAppendLog::append`] is the handle's `append`: the batch is
//!   snapshotted as lossless JSON (`materializeAppendBatch`), an empty batch
//!   returns, the seqs must continue the cursor (`assertContiguous`), a
//!   pending torn tail is truncated, and only then are the rows encoded. An
//!   unmaterialized log is written as the header line and the rows, each
//!   followed by LF; a materialized one gets the rows appended. A refused
//!   encode therefore still leaves the torn tail truncated, and no row of a
//!   refused batch is written.
//! - [`PlainAppendLog::flush`] writes the header line alone when nothing is
//!   materialized, and does nothing otherwise, even over a torn tail.
//!
//! Only the log's bytes and the refusals are modelled. Storage paths, the
//! write lease, the root-encoding file, fsync ordering, rollback after a
//! failed write, and Zstd compression are not. TypeScript resolves the id and
//! `cwd` to an artifact path, so an id or `cwd` whose path the filesystem
//! refuses, such as an id whose encoded segment is longer than a file name
//! may be, is outside this model's domain: it reports a write TypeScript
//! fails.
//! [`open`] runs [`scan_log`] only, without the stored identity and
//! `validateStoredEvents` checks of a TypeScript open, so a log TypeScript
//! refuses to open, including one whose `id` is empty, is outside its
//! domain. Nothing is read from or written to a file.
//!
//! [`open`]: PlainAppendLog::open

use serde_json::Value;

use crate::fork::holds_negative_zero;
use crate::{
    EncodeLimit, EncodeRefusal, MAX_SAFE_INTEGER, PathPlatform, ScanRefusal, encode_event_line,
    encode_header_line, scan_log,
};

/// The `TypeError` message of TypeScript's `materializeAppendBatch`.
const NOT_LOSSLESS_MESSAGE: &str = "session event batch is not losslessly JSON-serializable because it contains non-JSON-serializable data";

/// One write handle's view of a plain current-format log and its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlainAppendLog {
    id: String,
    cursor: u64,
    inherited_event_count: u64,
    storage: Storage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Storage {
    /// Created, with no file yet; the header line is written at the first
    /// non-empty append or flush.
    Pending { header_line: String },
    /// The file's bytes, and the torn-tail truncation point the first
    /// appended batch consumes.
    Materialized {
        bytes: Vec<u8>,
        torn_truncate_to: Option<usize>,
    },
}

/// Why [`PlainAppendLog::append`] wrote nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendRefusal {
    /// A batch event holds -0, so `materializeAppendBatch` throws a
    /// `TypeError` before any other check. The log is unchanged.
    NotLossless,
    /// `assertContiguous` throws a plain `Error` with this exact message. The
    /// log is unchanged, torn tail included.
    SeqMismatch { message: String },
    /// Encoding a row threw after the contiguity check passed. Neither the
    /// error class nor its message is claimed. A pending torn tail is
    /// truncated, and nothing else is written.
    Unadmitted,
    /// This crate does not reproduce the TypeScript outcome. The model is left
    /// as it was and claims nothing about TypeScript's log from here on.
    NativeSubset(AppendLimit),
}

impl AppendRefusal {
    /// TypeScript's exact message, or `None` where none is claimed.
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::NotLossless => Some(NOT_LOSSLESS_MESSAGE),
            Self::SeqMismatch { message } => Some(message),
            Self::Unadmitted | Self::NativeSubset(_) => None,
        }
    }
}

/// Why [`PlainAppendLog::create`] returned no log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateRefusal {
    /// [`encode_header_line`] refused the header, so TypeScript's `create`
    /// throws. Neither the error class nor its message is claimed.
    Unadmitted,
    /// This crate does not reproduce the TypeScript outcome of the create or
    /// of any operation on its handle; nothing is claimed.
    NativeSubset(CreateLimit),
}

/// Input whose TypeScript create outcome this model does not reproduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateLimit {
    /// [`encode_header_line`] reported a native limit for the header.
    Encode(EncodeLimit),
    /// The header's `id` is empty. TypeScript's `create` succeeds when no
    /// project directory exists, and the handle's first non-empty `append`
    /// or `flush` throws from the empty session directory segment before
    /// anything else is checked.
    EmptyId,
}

/// Input whose TypeScript append outcome this model does not reproduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendLimit {
    /// A batch event is not an object, or its `seq` is an array, an object,
    /// or a number serde_json holds as neither a safe integer nor -0: a
    /// fraction or exponent spelling, or an integer beyond 2^53 − 1.
    /// `assertContiguous` reads, compares, or renders it with JavaScript's
    /// property access, number, or `String` semantics.
    SeqValue,
    /// [`encode_event_line`] reported a native limit for a batch row.
    Encode(EncodeLimit),
}

impl PlainAppendLog {
    /// A created log with no file: `header` and `inherited_event_count` must
    /// encode as [`encode_header_line`] encodes them, which is the check
    /// TypeScript's `create` runs before it returns a handle. `create`'s
    /// lossless snapshot of the header refuses only -0, which the header
    /// encoder refuses wherever it can appear. An admitted header with an
    /// empty `id` is the [`CreateLimit::EmptyId`] limit.
    pub fn create(
        header: &Value,
        inherited_event_count: Option<u64>,
    ) -> Result<Self, CreateRefusal> {
        let header_line =
            encode_header_line(header, inherited_event_count).map_err(|refusal| match refusal {
                EncodeRefusal::Unadmitted => CreateRefusal::Unadmitted,
                EncodeRefusal::NativeSubset(limit) => {
                    CreateRefusal::NativeSubset(CreateLimit::Encode(limit))
                }
            })?;
        // The encoder admitted only a header whose `id` is a string.
        let Some(Value::String(id)) = header.get("id") else {
            return Err(CreateRefusal::Unadmitted);
        };
        if id.is_empty() {
            return Err(CreateRefusal::NativeSubset(CreateLimit::EmptyId));
        }
        Ok(Self {
            id: id.clone(),
            cursor: 0,
            inherited_event_count: inherited_event_count.unwrap_or(0),
            storage: Storage::Pending { header_line },
        })
    }

    /// A write open of an existing plain log's bytes, scanned as [`scan_log`]
    /// scans them with the same `platform` and `source_budget`. The cursor is
    /// the scanned event count. Committed bytes that end before `log` does
    /// leave a torn tail, which may hold complete records after the first
    /// one the scan refused; the first appended batch truncates it.
    pub fn open(
        log: &[u8],
        platform: PathPlatform,
        source_budget: usize,
    ) -> Result<Self, ScanRefusal> {
        let scanned = scan_log(log, platform, source_budget)?;
        let committed = scanned.committed_bytes();
        Ok(Self {
            id: scanned.header().id.clone(),
            // The scan decodes at most 2^53 rows, each held in memory.
            cursor: scanned.rows().len() as u64,
            inherited_event_count: scanned.inherited_event_count(),
            storage: Storage::Materialized {
                bytes: log.to_vec(),
                torn_truncate_to: (committed < log.len()).then_some(committed),
            },
        })
    }

    /// The Session id the contiguity message names.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The seq the next appended event must carry.
    pub const fn cursor(&self) -> u64 {
        self.cursor
    }

    /// The inherited cut the handle carries: the create count, or 0 when
    /// none was given, or the scanned cut.
    pub const fn inherited_event_count(&self) -> u64 {
        self.inherited_event_count
    }

    /// The log file's bytes, or `None` while no file exists.
    pub fn bytes(&self) -> Option<&[u8]> {
        match &self.storage {
            Storage::Pending { .. } => None,
            Storage::Materialized { bytes, .. } => Some(bytes),
        }
    }

    /// Append one batch as the handle's `append` does, in its order: the
    /// lossless snapshot, the empty-batch return, `assertContiguous`, the
    /// torn-tail truncation, then the encode and the write.
    pub fn append(&mut self, events: &[Value]) -> Result<(), AppendRefusal> {
        if events.iter().any(holds_negative_zero) {
            return Err(AppendRefusal::NotLossless);
        }
        if events.is_empty() {
            return Ok(());
        }
        for (index, event) in (0u64..).zip(events) {
            // The cursor counts events held in memory, so the sum stays far
            // below 2^53 and JavaScript renders it exactly.
            let expected = self.cursor.saturating_add(index);
            let got = seq_label(event, expected)?;
            if let Some(got) = got {
                let id = &self.id;
                return Err(AppendRefusal::SeqMismatch {
                    message: format!(
                        "append seq mismatch for \"{id}\": expected {expected} at index {index}, got {got}"
                    ),
                });
            }
        }
        let lines: Result<Vec<String>, EncodeRefusal> =
            events.iter().map(encode_event_line).collect();
        let lines = match lines {
            Ok(lines) => lines,
            Err(EncodeRefusal::NativeSubset(limit)) => {
                return Err(AppendRefusal::NativeSubset(AppendLimit::Encode(limit)));
            }
            Err(EncodeRefusal::Unadmitted) => {
                self.truncate_torn_tail();
                return Err(AppendRefusal::Unadmitted);
            }
        };
        self.truncate_torn_tail();
        let mut body = lines.join("\n");
        body.push('\n');
        match &mut self.storage {
            Storage::Pending { header_line } => {
                let mut bytes = Vec::with_capacity(header_line.len() + 1 + body.len());
                bytes.extend_from_slice(header_line.as_bytes());
                bytes.push(b'\n');
                bytes.extend_from_slice(body.as_bytes());
                self.storage = Storage::Materialized {
                    bytes,
                    torn_truncate_to: None,
                };
            }
            Storage::Materialized { bytes, .. } => bytes.extend_from_slice(body.as_bytes()),
        }
        self.cursor = self.cursor.saturating_add(events.len() as u64);
        Ok(())
    }

    /// The handle's `flush`: an unmaterialized log is written as its header
    /// line alone; a materialized one, torn or not, is left as it is.
    pub fn flush(&mut self) {
        if let Storage::Pending { header_line } = &self.storage {
            let mut bytes = Vec::with_capacity(header_line.len() + 1);
            bytes.extend_from_slice(header_line.as_bytes());
            bytes.push(b'\n');
            self.storage = Storage::Materialized {
                bytes,
                torn_truncate_to: None,
            };
        }
    }

    /// `truncateTornTail`: drop the bytes after the committed offset once.
    fn truncate_torn_tail(&mut self) {
        if let Storage::Materialized {
            bytes,
            torn_truncate_to,
        } = &mut self.storage
            && let Some(offset) = torn_truncate_to.take()
        {
            bytes.truncate(offset);
        }
    }
}

/// `String(event.seq)` when `event.seq !== expected`, `None` when they are
/// equal, or the [`AppendLimit::SeqValue`] limit.
fn seq_label(event: &Value, expected: u64) -> Result<Option<String>, AppendRefusal> {
    let limit = Err(AppendRefusal::NativeSubset(AppendLimit::SeqValue));
    let Value::Object(fields) = event else {
        return limit;
    };
    let label = match fields.get("seq") {
        None => "undefined".to_owned(),
        Some(Value::Null) => "null".to_owned(),
        Some(Value::Bool(flag)) => flag.to_string(),
        // `!==` never equates a string with a number.
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => {
            if let Some(number) = number.as_u64().filter(|number| *number <= MAX_SAFE_INTEGER) {
                if number == expected {
                    return Ok(None);
                }
                number.to_string()
            } else if let Some(number) = number
                .as_i64()
                .filter(|number| number.unsigned_abs() <= MAX_SAFE_INTEGER)
            {
                // Only a negative integer reaches here; it is never expected.
                number.to_string()
            } else {
                return limit;
            }
        }
        Some(Value::Array(_) | Value::Object(_)) => return limit,
    };
    Ok(Some(label))
}
