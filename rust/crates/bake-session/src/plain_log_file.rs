//! Development-only plain current-format Session log on disk, laid out and
//! written as TypeScript's JSONL backend with `compression: 'none'` lays out
//! and writes it.
//!
//! A [`PlainLogFile`] wraps a [`PlainAppendLog`] model of one write handle and
//! keeps the log file beneath a Session root equal to the model's bytes:
//!
//! - [`PlainLogFile::create`] is the backend's `create`: the header must
//!   encode, then the root is checked as `ensureRootEncoding` checks it, then
//!   the id must have no canonical generation in any project directory, as
//!   `findLog` resolves it. Nothing is written.
//! - [`PlainLogFile::open`] is a write `open`: the root check, then `findLog`,
//!   which must find exactly one project directory holding a canonical
//!   generation, then the Session directory's write lock, taken before the
//!   generation is read; every later refusal releases the lock and keeps its
//!   file. The generation must not be newer than the current one. A current
//!   generation's bytes are opened as [`PlainAppendLog::open`] opens them,
//!   and the selected path must be the one the header's id and `cwd` name.
//!   An older generation, format v0, v1, or v2, is migrated as the backend
//!   prepares and publishes it: its header and rows are parsed, its header
//!   identity is checked against the requested id and the selected path, its
//!   rows are decoded by the released codec in recoverable mode and read
//!   through every format edge to v3, the result passes the catalog's final
//!   check ([`check_transformed_artifact`]), and it is encoded, written to
//!   a temporary file beside the source, and published by hard link as a
//!   new `session.v3.jsonl`, as TypeScript publishes it on POSIX, so a
//!   failed write, or a process killed before the link, never leaves that
//!   file. The source is never changed.
//!   The handle then holds those bytes, opened as a current log.
//! - [`PlainLogFile::append`] and [`PlainLogFile::flush`] run the model's
//!   operation, then bring the file to the model's bytes, refused operations
//!   included: a first write creates the Session directory and a new file
//!   holding every byte, and a later one truncates the file to the bytes it
//!   shares with the model and writes the rest. A created handle first takes
//!   the write lock, creating the Session directory, at its first flush or
//!   its first batch that is neither empty nor refused for -0, before the
//!   batch's contiguity check, as TypeScript's `ensureLease` does. The
//!   opposite-encoding check TypeScript repeats before that lock is not
//!   repeated: a Zstd log written after `create`, by a process outside this
//!   model, is outside the domain.
//!
//! The write lock is an exclusive kernel lock on the `session.lock` file in
//! the Session directory, which is created and never removed, as
//! TypeScript's `SessionWriteLease` takes it; a lock another handle holds is
//! refused with `SessionAlreadyOwnedError`'s exact message, and a refused
//! lock leaves a handle usable. Each value stands for a handle of its own
//! backend instance, so two values of one Session are arbitrated by that
//! lock alone. Dropping the value is the handle's `close`, which releases
//! the lock. The refusals TypeScript's `SessionAlreadyExistsError`,
//! `SessionPersistenceNotFoundError`, and duplicate-id `Error` carry are
//! returned with their exact messages, as are the
//! `SessionPersistenceCorruptionError` and `SessionFormatUnsupportedError` a
//! migration reports, which name the source path.
//!
//! A migration's streaming order is recovered from batch stages: a refusal
//! the codec raises at a row is reported only when reading the rows before
//! it through every edge raises no header or event refusal, which TypeScript
//! would have reported first, and a parse that ends at a later row, on a
//! `turn/end` after an unparsable row or at a limit, is reported only when
//! no codec or edge refusal precedes it.
//!
//! Paths are spelled from `root` as given, and TypeScript spells them from
//! `path.resolve(root)`, so the messages are exact only for an absolute root
//! that `path.resolve` leaves unchanged, with no `.` or `..` component and
//! no trailing or repeated separator.
//!
//! A TypeScript backend instance's in-process write claims and pending
//! creates, which refuse a second handle of one Session within that
//! instance, are not modelled; exclusion between a Rust value and a
//! TypeScript handle is tested between processes only, with uncompressed
//! logs, as the `write_lease` module describes. Fsync and directory sync, the
//! publication's verifier, rollback after a failed write, file modes, Zstd
//! compression, and the `validateStoredEvents` check of an opened log are
//! not modelled. Opening a log TypeScript's validation refuses is
//! outside this model's domain. So is migrating a log whose v3
//! result the publication's verifier or `validateStoredEvents` refuses: Rust
//! writes the migrated file where TypeScript refuses and writes nothing. So
//! is a path the filesystem refuses, such as one longer than a file or path name may
//! be, a symlink, or a root another process changes, a lock file removed or
//! replaced included. Every listing is visited in byte order of its UTF-8
//! names. A migration's
//! temporary file is removed after a failed write or link, and a failed
//! removal is reported with that failure; one a killed process leaves is
//! not a generation. After an I/O error in `append` or `flush` the file may
//! hold a partial write, and every later operation on the value fails.

use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use crate::fork::holds_negative_zero;
use crate::log_layout::{CURRENT_LOG_FILENAME, canonical_generation, encode_segment, log_path};
use crate::released_rows::{ParseStop, parse_released_header, parse_released_rows};
use crate::write_lease::{LeaseRefusal, WriteLease};
use crate::{
    AppendRefusal, CURRENT_SESSION_FORMAT_VERSION, CreateRefusal, FinalCheckRefusal,
    GenerationHeaderRefusal, HistoryLocation, HistoryRefusal, MigratedV2, PathPlatform,
    PlainAppendLog, ScanRefusal, SubsetLimit, V1CodecLocation, V1CodecRecovery, V1CodecRefusal,
    V1CodecVersion, V2ToV3Layer, V2ToV3Location, V2ToV3Refusal, check_transformed_artifact,
    decode_v0_v1_items, encode_event_line, encode_header_line, first_record,
    migrate_released_history, migrate_v2_rows, read_generation_header_record, read_header_record,
};

/// One write handle of a plain current-format Session log under a root.
#[derive(Debug)]
pub struct PlainLogFile {
    path: PathBuf,
    log: PlainAppendLog,
    /// The Session directory's write lock, held from a write `open` or from
    /// a created handle's first write on.
    lease: Option<WriteLease>,
    failed: bool,
}

/// Why an operation of [`PlainLogFile`] was refused.
#[derive(Debug)]
pub enum LogFileRefusal {
    /// Another handle holds the Session directory's write lock; TypeScript
    /// throws `SessionAlreadyOwnedError` with this exact message. The log
    /// was neither read nor written; only the lock file and its directory
    /// may have been created.
    AlreadyOwned { message: String },
    /// `create` found a canonical generation of the id in exactly one project
    /// directory; TypeScript throws `SessionAlreadyExistsError` with this
    /// exact message.
    AlreadyExists { message: String },
    /// A write `open` found no canonical generation of the id; TypeScript
    /// throws `SessionPersistenceNotFoundError` with this exact message.
    NotFound { message: String },
    /// Two or more project directories hold a canonical generation of the id;
    /// TypeScript's `findLog` throws a plain `Error` with this exact message.
    Duplicate { message: String },
    /// Migrating an older generation found it corrupt; TypeScript throws
    /// `SessionPersistenceCorruptionError` with this exact message, which
    /// names the source path. No file was written.
    Corrupt { message: String },
    /// A format edge, or the catalog's final check of the migrated log,
    /// refused to migrate an older generation; TypeScript throws
    /// `SessionFormatUnsupportedError` with this exact message, which names
    /// the source path. No file was written.
    Unsupported { message: String },
    /// [`PlainAppendLog::create`] refused the header. Nothing was read.
    Create(CreateRefusal),
    /// [`PlainAppendLog::append`] refused the batch; the file holds what the
    /// model holds after the refusal.
    Append(AppendRefusal),
    /// This crate does not reproduce the TypeScript outcome; nothing is
    /// claimed about it, and the file is as the refused operation left it.
    NativeSubset(LogFileLimit),
    /// Reading or writing the root failed here. No TypeScript outcome is
    /// claimed.
    Io(io::Error),
}

impl LogFileRefusal {
    /// TypeScript's exact message, or `None` where none is claimed.
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::AlreadyOwned { message }
            | Self::AlreadyExists { message }
            | Self::NotFound { message }
            | Self::Duplicate { message }
            | Self::Corrupt { message }
            | Self::Unsupported { message } => Some(message),
            Self::Append(refusal) => refusal.message(),
            Self::Create(_) | Self::NativeSubset(_) | Self::Io(_) => None,
        }
    }
}

impl From<io::Error> for LogFileRefusal {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Roots and logs whose TypeScript outcome this model does not reproduce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogFileLimit {
    /// `open` was asked for the empty id. TypeScript reports it not found in
    /// a root without a project directory and otherwise throws while
    /// encoding it; `create` reports [`crate::CreateLimit::EmptyId`].
    EmptyId,
    /// The encoded id ends in `.` or names a Windows device, such as `CON`,
    /// `nul`, or `COM1`, which Windows path normalization would not keep as
    /// a directory name. It is refused on every host.
    WindowsName,
    /// A project directory holds an entry whose name ends in `.jsonl` or
    /// `.jsonl.zstd`. TypeScript refuses a file named so as the flat legacy
    /// layout, and probes the id's flat names by opening them.
    LegacyLayout,
    /// A Session directory holds a canonical generation with the `.zstd`
    /// suffix, which TypeScript refuses as an encoding mismatch.
    OppositeEncoding,
    /// The root or a project directory lists a name that is not UTF-8, which
    /// Node lists with replacement characters and then opens by that spelling.
    NonUtf8Name,
    /// The id's highest canonical generation is newer than the current one;
    /// TypeScript reads its header and refuses it.
    NewerGeneration,
    /// The selected current log's header id differs from the requested one,
    /// or a selected log's header id and `cwd` name another path than the
    /// selected one. TypeScript refuses unless `realpath` resolves both paths
    /// to one file.
    Identity,
    /// [`PlainAppendLog::open`] refused the stored bytes, which TypeScript
    /// reports with the path of the log in its message.
    Scan(ScanRefusal),
    /// Migrating an older generation reached input whose TypeScript outcome
    /// this crate does not decide. Nothing was written. The name is one of:
    ///
    /// - `header/<name>` or `row/<name>`: the header or a row is not UTF-8
    ///   (`invalid-utf8`), serde_json refuses what `JSON.parse` may admit
    ///   (`json-parser`), or a number has more integer digits than
    ///   serde_json rounds as `JSON.parse` does (`number-lexeme`), as
    ///   [`crate::ScanLimit`] describes them; or the header's version or a
    ///   count is held as a float (`float-lexeme`) or its identity check
    ///   reports `version-diagnostic`.
    /// - `codec/<name>`, `history/<name>`, `v2-to-v3/<name>`, or
    ///   `final-check/<name>`: a limit of [`decode_v0_v1_items`],
    ///   [`migrate_released_history`], [`migrate_v2_rows`], or
    ///   [`check_transformed_artifact`], under its name there.
    /// - `finish-order`: the v0 or v1 codec's `finish` refuses rows that the
    ///   later stages, or an earlier stop, must see first.
    /// - `v2-codec-recovery`: the v2 codec refuses a `turn/end` or
    ///   `session/end-seed` row, or a row before a later `turn/end`, whose
    ///   recoverable outcome depends on which check refused it.
    /// - `decode-invariant`: a rerun over a prefix disagrees with the first
    ///   run.
    /// - `encode` or `scan`: the migrated log reaches a native limit of this
    ///   crate's encoder, such as a number with a fraction that TypeScript
    ///   writes, or its encoded bytes do not open as [`PlainAppendLog::open`]
    ///   opens a current log. TypeScript may migrate such a log.
    Migration(String),
}

/// A canonical plain generation found for an id.
struct Generation {
    path: PathBuf,
    version: u64,
}

impl PlainLogFile {
    /// The backend's `create(header, { inheritedEventCount })` in `root`.
    /// The header is checked as [`PlainAppendLog::create`] checks it, then
    /// the root, then every project directory for a canonical generation of
    /// the id. No file or directory is written.
    pub fn create(
        root: &Path,
        header: &Value,
        inherited_event_count: Option<u64>,
    ) -> Result<Self, LogFileRefusal> {
        let log = PlainAppendLog::create(header, inherited_event_count)
            .map_err(LogFileRefusal::Create)?;
        let encoded = encoded_id(log.id())?;
        let found = find_generations(root, &encoded)?;
        if !found.is_empty() {
            return Err(duplicate(log.id(), &found).unwrap_or_else(|| {
                LogFileRefusal::AlreadyExists {
                    message: format!("session \"{}\" already exists", log.id()),
                }
            }));
        }
        // The encoder admits only an absent `cwd` or an absolute string.
        let cwd = header.get("cwd").and_then(Value::as_str);
        Ok(Self {
            path: log_path(root, cwd, &encoded),
            log,
            lease: None,
            failed: false,
        })
    }

    /// The backend's write `open(id)` in `root`, scanning the current
    /// generation's bytes with this host's path platform and `source_budget`,
    /// as [`PlainAppendLog::open`] does.
    pub fn open(root: &Path, id: &str, source_budget: usize) -> Result<Self, LogFileRefusal> {
        if id.is_empty() {
            return Err(LogFileRefusal::NativeSubset(LogFileLimit::EmptyId));
        }
        let encoded = encoded_id(id)?;
        let mut found = find_generations(root, &encoded)?;
        if let Some(refusal) = duplicate(id, &found) {
            return Err(refusal);
        }
        let Some(selected) = found.pop() else {
            return Err(LogFileRefusal::NotFound {
                message: format!("session \"{id}\" not found"),
            });
        };
        // The lock is taken before the selected log is read, and a refusal
        // from here on releases it and keeps its file.
        let dir = selected
            .path
            .parent()
            .ok_or_else(|| io::Error::other("a Session log path has a directory"))?;
        let lease = acquire_lease(dir, id)?;
        if selected.version < CURRENT_SESSION_FORMAT_VERSION {
            return Self::migrate(root, id, &encoded, &selected, lease, source_budget);
        }
        if selected.version > CURRENT_SESSION_FORMAT_VERSION {
            return Err(LogFileRefusal::NativeSubset(LogFileLimit::NewerGeneration));
        }
        let bytes = fs::read(&selected.path)?;
        let platform = PathPlatform::host();
        let log = PlainAppendLog::open(&bytes, platform, source_budget)
            .map_err(|refusal| LogFileRefusal::NativeSubset(LogFileLimit::Scan(refusal)))?;
        let identity = LogFileRefusal::NativeSubset(LogFileLimit::Identity);
        // The scan admitted this header record, so it decodes again.
        let Some(Ok(header)) =
            first_record(&bytes).map(|record| read_header_record(record, platform))
        else {
            return Err(identity);
        };
        if header.id != id || log_path(root, header.cwd.as_deref(), &encoded) != selected.path {
            return Err(identity);
        }
        Ok(Self {
            path: selected.path,
            log,
            lease: Some(lease),
            failed: false,
        })
    }

    /// The write `open` of an older generation: `prepareStoredMigration`,
    /// then `publishStoredMigration`, which writes the encoded v3 log beside
    /// the unchanged source before the handle holds it.
    fn migrate(
        root: &Path,
        id: &str,
        encoded: &str,
        selected: &Generation,
        lease: WriteLease,
        source_budget: usize,
    ) -> Result<Self, LogFileRefusal> {
        let bytes = fs::read(&selected.path)?;
        let platform = PathPlatform::host();
        let refusal = |refused: Refused| refused.into_refusal(id, selected);
        let Some(record) = first_record(&bytes) else {
            return Err(refusal(Refused::Corrupt(
                "Error: empty or header-less session log".to_owned(),
            )));
        };
        let header = parse_released_header(record, selected.version)
            .map_err(|stop| refusal(Refused::from(stop)))?;
        // `validateSourceIdentity` checks a header its codec reads, before any row.
        match read_generation_header_record(record, selected.version, platform) {
            Ok(Some(stored)) => {
                if stored.id != id {
                    let path = selected.path.display();
                    return Err(refusal(Refused::Corrupt(format!(
                        "Error: corrupt session log \"{path}\": requested id \"{id}\" \
                         does not match header id \"{}\"",
                        stored.id
                    ))));
                }
                let current = selected.path.with_file_name(CURRENT_LOG_FILENAME);
                if log_path(root, stored.cwd.as_deref(), encoded) != current {
                    return Err(LogFileRefusal::NativeSubset(LogFileLimit::Identity));
                }
            }
            // The codec refuses a header with retired fields.
            Ok(None) | Err(GenerationHeaderRefusal::Rejected(_)) => {}
            Err(GenerationHeaderRefusal::Unsupported(_)) => {
                return Err(refusal(Refused::Limit("decode-invariant".to_owned())));
            }
            Err(GenerationHeaderRefusal::NativeSubset(limit)) => {
                return Err(refusal(Refused::Limit(header_limit(limit).to_owned())));
            }
        }
        let parsed = parse_released_rows(&bytes[record.len()..]);
        let stop = parsed.stop.map(Refused::from);
        let migrated = if selected.version == 2 {
            migrate_v2(&header, &parsed.rows, stop, source_budget)
        } else {
            let version = if selected.version == 0 {
                V1CodecVersion::V0
            } else {
                V1CodecVersion::V1
            };
            migrate_v0_v1(&header, &parsed.rows, stop, version, source_budget)
        }
        .map_err(refusal)?;
        // The catalog checks the transformed artifact when the chain finishes.
        if let Err(checked) = check_transformed_artifact(&migrated, platform) {
            return Err(refusal(match checked {
                FinalCheckRefusal::NativeSubset(name) => {
                    Refused::Limit(format!("final-check/{name}"))
                }
                checked => Refused::Unsupported(
                    checked
                        .catalog_message(selected.version)
                        .unwrap_or_default(),
                ),
            }));
        }
        let encode = || Refused::Limit("encode".to_owned());
        let mut text = encode_header_line(&migrated.header, Some(migrated.inherited_event_count))
            .map_err(|_| refusal(encode()))?;
        text.push('\n');
        for event in &migrated.events {
            text.push_str(&encode_event_line(event).map_err(|_| refusal(encode()))?);
            text.push('\n');
        }
        let bytes = text.into_bytes();
        let log = PlainAppendLog::open(&bytes, platform, source_budget)
            .map_err(|_| refusal(Refused::Limit("scan".to_owned())))?;
        let path = selected.path.with_file_name(CURRENT_LOG_FILENAME);
        publish_new_file(&path, &bytes)?;
        Ok(Self {
            path,
            log,
            lease: Some(lease),
            failed: false,
        })
    }

    /// The log file's path: the one the header's id and `cwd` name.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The model of the handle and the bytes the file holds.
    pub const fn log(&self) -> &PlainAppendLog {
        &self.log
    }

    /// The handle's `append`, as [`PlainAppendLog::append`] runs it, with the
    /// file then brought to the model's bytes, even when the batch is refused
    /// after a torn tail was truncated. A created handle takes the write lock
    /// at its first batch that is neither empty nor refused for -0, before
    /// the contiguity check, as TypeScript's `ensureLease` does.
    pub fn append(&mut self, events: &[Value]) -> Result<(), LogFileRefusal> {
        self.check_usable()?;
        if !events.is_empty() && !events.iter().any(holds_negative_zero) {
            self.ensure_lease()?;
        }
        let before = self.log.bytes().map(<[u8]>::to_vec);
        let outcome = self.log.append(events);
        self.sync(before.as_deref())?;
        outcome.map_err(LogFileRefusal::Append)
    }

    /// The handle's `flush`: an unwritten log takes the write lock, then its
    /// file is created holding the header line alone; a written one is left
    /// as it is.
    pub fn flush(&mut self) -> Result<(), LogFileRefusal> {
        self.check_usable()?;
        if self.log.bytes().is_none() {
            self.ensure_lease()?;
        }
        let before = self.log.bytes().map(<[u8]>::to_vec);
        self.log.flush();
        self.sync(before.as_deref())
    }

    /// Take the Session directory's write lock unless it is held. A refusal
    /// leaves the handle usable, so a later operation tries again.
    fn ensure_lease(&mut self) -> Result<(), LogFileRefusal> {
        if self.lease.is_none() {
            let dir = self
                .path
                .parent()
                .ok_or_else(|| io::Error::other("a Session log path has a directory"))?;
            self.lease = Some(acquire_lease(dir, self.log.id())?);
        }
        Ok(())
    }

    fn check_usable(&self) -> Result<(), LogFileRefusal> {
        if self.failed {
            return Err(LogFileRefusal::Io(io::Error::other(
                "an earlier write to this Session log failed",
            )));
        }
        Ok(())
    }

    /// Write the difference between `before`, the bytes the file holds, and
    /// the model's bytes now. A failure marks the value failed.
    fn sync(&mut self, before: Option<&[u8]>) -> Result<(), LogFileRefusal> {
        let Some(after) = self.log.bytes() else {
            return Ok(());
        };
        let written = match before {
            None => create_log_file(&self.path, after),
            Some(before) => rewrite_tail(&self.path, before, after),
        };
        written.map_err(|error| {
            self.failed = true;
            LogFileRefusal::Io(error)
        })
    }
}

/// A migration's refusal before the source path and the requested id are
/// put in its message.
enum Refused {
    /// `SessionPersistenceCorruptionError`, with `String(error)` of the cause.
    Corrupt(String),
    /// `SessionFormatUnsupportedError`, with the format edge's or the
    /// catalog's message.
    Unsupported(String),
    /// [`LogFileLimit::Migration`], with its name.
    Limit(String),
}

impl Refused {
    /// The backend's `generationFailure` translation.
    fn into_refusal(self, id: &str, selected: &Generation) -> LogFileRefusal {
        let source = selected.path.display();
        match self {
            Self::Corrupt(cause) => LogFileRefusal::Corrupt {
                message: format!(
                    "session \"{id}\": stored log is corrupt: {cause} (raw log: {source})"
                ),
            },
            Self::Unsupported(reason) => LogFileRefusal::Unsupported {
                message: format!(
                    "{reason}; source v{} artifact remains unchanged (raw log: {source})",
                    selected.version
                ),
            },
            Self::Limit(name) => LogFileRefusal::NativeSubset(LogFileLimit::Migration(name)),
        }
    }

    /// A refusal of the released v0 or v1 codec, a `SessionFormatError`.
    fn codec(refusal: V1CodecRefusal) -> Self {
        match refusal {
            V1CodecRefusal::Rejected { message, .. } => {
                Self::Corrupt(format!("SessionFormatError: {message}"))
            }
            V1CodecRefusal::NativeSubset { limit, .. } => {
                Self::Limit(format!("codec/{}", limit.name()))
            }
        }
    }

    /// A refusal of the format chain, always unsupported.
    fn history(refusal: HistoryRefusal) -> Self {
        match refusal {
            HistoryRefusal::Rejected { message, .. } => Self::Unsupported(message),
            HistoryRefusal::NativeSubset { limit, .. } => {
                Self::Limit(format!("history/{}", limit.name()))
            }
        }
    }

    /// A refusal of the released v2 codec or the v2→v3 edge.
    fn v2(refusal: V2ToV3Refusal) -> Self {
        match refusal {
            V2ToV3Refusal::Rejected {
                layer: V2ToV3Layer::Codec,
                message,
                ..
            } => Self::Corrupt(format!("SessionFormatError: {message}")),
            V2ToV3Refusal::Rejected { message, .. } => Self::Unsupported(message),
            V2ToV3Refusal::NativeSubset { limit, .. } => Self::Limit(format!("v2-to-v3/{limit}")),
        }
    }
}

impl From<ParseStop> for Refused {
    fn from(stop: ParseStop) -> Self {
        match stop {
            ParseStop::Corrupt(message) => Self::Corrupt(format!("Error: {message}")),
            ParseStop::Limit(name) => Self::Limit(name.to_owned()),
        }
    }
}

const fn header_limit(limit: SubsetLimit) -> &'static str {
    match limit {
        SubsetLimit::InvalidUtf8 => "header/invalid-utf8",
        SubsetLimit::JsonParser => "header/json-parser",
        SubsetLimit::FloatLexeme => "header/float-lexeme",
        SubsetLimit::VersionDiagnostic => "header/version-diagnostic",
    }
}

/// A v0 or v1 log's parsed `rows` through the recoverable codec and every
/// format edge, then `stop`, in TypeScript's streaming order.
fn migrate_v0_v1(
    header: &Value,
    rows: &[Value],
    stop: Option<Refused>,
    version: V1CodecVersion,
    source_budget: usize,
) -> Result<MigratedV2, Refused> {
    let decode = |rows: &[Value]| {
        decode_v0_v1_items(
            header,
            rows,
            version,
            V1CodecRecovery::Recoverable,
            PathPlatform::host(),
            source_budget,
        )
    };
    let refusal = match decode(rows) {
        Ok(items) => {
            return match (migrate_released_history(&items), stop) {
                (Err(refused), _) if !at_finish(&refused) => Err(Refused::history(refused)),
                // Every row streamed before the stop, and `finish` runs after it.
                (_, Some(stop)) => Err(stop),
                (outcome, None) => outcome.map_err(Refused::history),
            };
        }
        Err(refusal) => refusal,
    };
    let location = match &refusal {
        V1CodecRefusal::Rejected { location, .. }
        | V1CodecRefusal::NativeSubset { location, .. } => *location,
    };
    let row = match location {
        V1CodecLocation::Header => return Err(Refused::codec(refusal)),
        // The rows the decoder emitted before `finish` are not returned.
        V1CodecLocation::Finish => return Err(Refused::Limit("finish-order".to_owned())),
        V1CodecLocation::Row(row) => row,
    };
    // The items before the refused row streamed through every edge first.
    let prefix = match decode(rows.get(..row).unwrap_or_default()) {
        Ok(prefix) => prefix,
        Err(V1CodecRefusal::Rejected {
            location: V1CodecLocation::Finish,
            ..
        }) => return Err(Refused::Limit("finish-order".to_owned())),
        Err(_) => return Err(invariant()),
    };
    match migrate_released_history(&prefix) {
        Err(refused) if !at_finish(&refused) => Err(Refused::history(refused)),
        _ => Err(Refused::codec(refusal)),
    }
}

fn at_finish(refusal: &HistoryRefusal) -> bool {
    matches!(
        refusal,
        HistoryRefusal::Rejected {
            location: HistoryLocation::Finish,
            ..
        } | HistoryRefusal::NativeSubset {
            location: HistoryLocation::Finish,
            ..
        }
    )
}

fn invariant() -> Refused {
    Refused::Limit("decode-invariant".to_owned())
}

/// A v2 log's parsed `rows` through the released v2 codec in recoverable
/// mode and the v2→v3 edge, then `stop`, in TypeScript's streaming order.
/// [`migrate_v2_rows`] decodes strictly; where its codec refuses a row, the
/// recoverable codec drops that row and every later one unless one of them
/// throws, which only a `turn/end` row or a `session/end-seed` row can do.
fn migrate_v2(
    header: &Value,
    rows: &[Value],
    stop: Option<Refused>,
    source_budget: usize,
) -> Result<MigratedV2, Refused> {
    let migrate =
        |rows: &[Value]| migrate_v2_rows(header, rows, PathPlatform::host(), source_budget);
    let streamed = match migrate(rows) {
        Err(V2ToV3Refusal::Rejected {
            location: V2ToV3Location::Row(row),
            layer: V2ToV3Layer::Codec,
            ..
        }) => {
            let has_type =
                |row: &Value, kind: &str| row.get("type").is_some_and(|value| value == kind);
            let throws = rows.get(row).is_none_or(|refused| {
                has_type(refused, "turn/end") || has_type(refused, "session/end-seed")
            }) || rows
                .iter()
                .skip(row.saturating_add(1))
                .any(|later| has_type(later, "turn/end"));
            if throws {
                return Err(Refused::Limit("v2-codec-recovery".to_owned()));
            }
            match migrate(rows.get(..row).unwrap_or_default()) {
                Err(
                    V2ToV3Refusal::Rejected {
                        location: V2ToV3Location::Row(_),
                        ..
                    }
                    | V2ToV3Refusal::NativeSubset {
                        location: V2ToV3Location::Row(_),
                        ..
                    },
                ) => return Err(invariant()),
                outcome => outcome,
            }
        }
        outcome => outcome,
    };
    let before_stop = matches!(
        streamed,
        Err(V2ToV3Refusal::Rejected {
            location: V2ToV3Location::Header | V2ToV3Location::Row(_),
            ..
        } | V2ToV3Refusal::NativeSubset {
            location: V2ToV3Location::Header | V2ToV3Location::Row(_),
            ..
        })
    );
    match stop {
        // Every row streamed before the stop, and `finish` runs after it.
        Some(stop) if !before_stop => Err(stop),
        _ => streamed.map_err(Refused::v2),
    }
}

/// Publish `bytes` as a new file at `path`, whose directory exists: write
/// them to a new `session.migration.<token>.tmp` beside it, which is never a
/// canonical generation, then hard-link that file to `path`, which fails if
/// `path` exists, and remove it. A write or link failure removes the
/// temporary file and never leaves a file at `path`; a failed removal is
/// reported with the failure that made the file disposable.
fn publish_new_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static NEXT_TOKEN: AtomicU64 = AtomicU64::new(0);
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("a Session log path has a directory"))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let (staged, mut file) = loop {
        let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
        let staged = dir.join(format!(
            "session.migration.{}-{token}.tmp",
            std::process::id()
        ));
        match options.open(&staged) {
            Ok(file) => break (staged, file),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    };
    let written = file.write_all(bytes);
    // Windows removes and links only a file no handle holds open.
    drop(file);
    if let Err(error) = written.and_then(|()| fs::hard_link(&staged, path)) {
        return Err(match fs::remove_file(&staged) {
            Ok(()) => error,
            Err(cleanup) => io::Error::new(
                error.kind(),
                format!(
                    "{error}; failed to remove migration temporary \"{}\": {cleanup}",
                    staged.display()
                ),
            ),
        });
    }
    // `path` holds the bytes; a leftover temporary is never a generation.
    let _ = fs::remove_file(&staged);
    Ok(())
}

/// The write lock of the Session directory `dir`, or TypeScript's
/// `SessionAlreadyOwnedError` refusal for `id`.
fn acquire_lease(dir: &Path, id: &str) -> Result<WriteLease, LogFileRefusal> {
    WriteLease::acquire(dir).map_err(|refusal| match refusal {
        LeaseRefusal::AlreadyOwned => LogFileRefusal::AlreadyOwned {
            message: format!("session \"{id}\" is already owned by an active write handle"),
        },
        LeaseRefusal::Io(error) => LogFileRefusal::Io(error),
    })
}

/// The id's path segment, or the [`LogFileLimit::WindowsName`] limit.
fn encoded_id(id: &str) -> Result<String, LogFileRefusal> {
    let encoded = encode_segment(id);
    if windows_device_or_dot(&encoded) {
        return Err(LogFileRefusal::NativeSubset(LogFileLimit::WindowsName));
    }
    Ok(encoded)
}

/// Whether Windows would not keep an encoded segment as a directory name. An
/// encoded segment holds only ASCII letters, digits, `.`, `_`, `-`, and `~`,
/// so spaces and the superscript device digits never appear.
fn windows_device_or_dot(encoded: &str) -> bool {
    if encoded.ends_with('.') {
        return true;
    }
    let stem = encoded
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    match stem.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        _ => stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|number| {
                matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            }),
    }
}

/// The duplicate-id refusal when more than one generation was found.
fn duplicate(id: &str, found: &[Generation]) -> Option<LogFileRefusal> {
    (found.len() > 1).then(|| LogFileRefusal::Duplicate {
        message: format!(
            "duplicate JSONL session id \"{id}\" appears in multiple project directories"
        ),
    })
}

/// One listed entry: its name when it is UTF-8, and whether it is a
/// directory, symlinks not followed.
struct Entry {
    name: Option<String>,
    is_dir: bool,
}

/// The listing of `dir`, UTF-8 names in byte order before the others, or
/// `None` when `dir` is absent.
fn list(dir: &Path) -> Result<Option<Vec<Entry>>, LogFileRefusal> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut listed = Vec::new();
    for entry in entries {
        let entry = entry?;
        listed.push(Entry {
            is_dir: entry.file_type()?.is_dir(),
            name: entry.file_name().into_string().ok(),
        });
    }
    listed.sort_by(|left, right| match (&left.name, &right.name) {
        (Some(left), Some(right)) => left.cmp(right),
        (left, right) => right.is_some().cmp(&left.is_some()),
    });
    Ok(Some(listed))
}

/// The UTF-8 names `dir` lists; a name that is not UTF-8 is never a
/// canonical generation, even in Node's replacement spelling.
fn utf8_names(dir: &Path) -> Result<Vec<String>, LogFileRefusal> {
    Ok(list(dir)?
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| entry.name)
        .collect())
}

const NON_UTF8_NAME: LogFileRefusal = LogFileRefusal::NativeSubset(LogFileLimit::NonUtf8Name);

/// The root check of `ensureRootEncoding`, then `findLog` for the encoded id:
/// the highest canonical generation of each project directory holding one.
fn find_generations(root: &Path, encoded: &str) -> Result<Vec<Generation>, LogFileRefusal> {
    let mut projects = Vec::new();
    for entry in list(root)?.unwrap_or_default() {
        // Only directories are traversed.
        match entry {
            Entry { is_dir: false, .. } => {}
            Entry {
                name: Some(name), ..
            } => projects.push(name),
            Entry { name: None, .. } => return Err(NON_UTF8_NAME),
        }
    }
    for project in &projects {
        let project_dir = root.join(project);
        for entry in list(&project_dir)?.unwrap_or_default() {
            let Some(name) = entry.name else {
                return Err(NON_UTF8_NAME);
            };
            if name.ends_with(".jsonl") || name.ends_with(".jsonl.zstd") {
                return Err(LogFileRefusal::NativeSubset(LogFileLimit::LegacyLayout));
            }
            if entry.is_dir {
                refuse_opposite(&utf8_names(&project_dir.join(name))?)?;
            }
        }
    }
    let mut found = Vec::new();
    for project in &projects {
        let dir = root.join(project).join(encoded);
        let names = utf8_names(&dir)?;
        refuse_opposite(&names)?;
        let newest = names
            .into_iter()
            .filter_map(|name| canonical_generation(&name).map(|version| (version, name)))
            .max();
        if let Some((version, name)) = newest {
            found.push(Generation {
                path: dir.join(name),
                version,
            });
        }
    }
    Ok(found)
}

/// The [`LogFileLimit::OppositeEncoding`] limit when a Session directory's
/// `names` hold a canonical generation with the `.zstd` suffix.
fn refuse_opposite(names: &[String]) -> Result<(), LogFileRefusal> {
    let opposite = names.iter().any(|name| {
        name.strip_suffix(".zstd")
            .and_then(canonical_generation)
            .is_some()
    });
    if opposite {
        return Err(LogFileRefusal::NativeSubset(LogFileLimit::OppositeEncoding));
    }
    Ok(())
}

/// Create the Session directory and a new log file holding `bytes`.
fn create_log_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("a Session log path has a directory"))?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(bytes)
}

/// Bring a file holding `before` to `after`: truncate it to their common
/// prefix, then write the rest of `after` there.
fn rewrite_tail(path: &Path, before: &[u8], after: &[u8]) -> io::Result<()> {
    let shared = before
        .iter()
        .zip(after)
        .take_while(|(old, new)| old == new)
        .count();
    if shared == before.len() && shared == after.len() {
        return Ok(());
    }
    let mut file: File = OpenOptions::new().write(true).open(path)?;
    if shared < before.len() {
        // A byte count of an in-memory buffer fits in u64.
        file.set_len(shared as u64)?;
    }
    file.seek(SeekFrom::Start(shared as u64))?;
    file.write_all(&after[shared..])
}
