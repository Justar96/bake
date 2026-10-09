//! Development-only plain current-format Session log on disk, laid out and
//! written as TypeScript's JSONL backend with `compression: 'none'` lays out
//! and writes it.
//!
//! A [`PlainLogFile`] wraps a [`PlainAppendLog`] model of one write handle and
//! keeps the log file beneath a Session root equal to the model's bytes:
//!
//! - [`PlainLogFile::create`] is the backend's `create`: the header must
//!   encode, then the root is checked as `ensureRootEncoding` checks it,
//!   which refuses a regular file named `*.jsonl` or `*.jsonl.zstd` in a
//!   project directory by TypeScript's flat-layout message, and a Session
//!   directory holding a canonical generation with the `.zstd` suffix by
//!   TypeScript's encoding-mismatch message, naming the highest such
//!   generation, then the id must have no canonical generation in any
//!   project directory, as `findLog` resolves it after probing the id's flat
//!   names. Nothing is written.
//! - [`PlainLogFile::open`] is a write `open`: the root check, then `findLog`,
//!   which must find exactly one project directory holding a canonical
//!   generation, then the Session directory's write lock, taken before the
//!   generation is read; every later refusal releases the lock and keeps its
//!   file. The generation must not be newer than the current one. A current
//!   generation's bytes are opened as [`PlainAppendLog::open`] opens them,
//!   and, as `assertStoredIdentity` checks it, the header must carry the
//!   requested id and its id and `cwd` must name the selected path, by
//!   spelling or else by `realpath`, which resolves a case alias on a
//!   case-insensitive volume, or a symbolic link, to one file.
//!   An older generation, format v0, v1, or v2, is migrated as the backend
//!   prepares and publishes it: its header and rows are parsed, its header
//!   identity is checked against the requested id and the selected path as a
//!   current header's is, its
//!   rows are decoded by the released codec in recoverable mode and read
//!   through every format edge to v3, the result passes the catalog's final
//!   check ([`check_transformed_artifact`]), and it is encoded, written to
//!   a temporary file beside the source, and published by hard link as a
//!   new `session.v3.jsonl`, as TypeScript publishes it on POSIX, so a
//!   failed write, or a process killed before the link, never leaves that
//!   file. The source is never changed.
//!   The handle then holds those bytes, opened as a current log. The
//!   in-memory half, the read `open`'s preparation without its publication,
//!   is [`released_generation_header`] followed by
//!   [`migrate_released_generation`].
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
//! `SessionPersistenceNotFoundError`, and duplicate-id, flat-layout,
//! encoding-mismatch, and stored-identity `Error` carry are returned with
//! their exact messages,
//! as are the `SessionPersistenceCorruptionError` and
//! `SessionFormatUnsupportedError` a migration reports, which name the
//! source path.
//!
//! An id's directory is named by `encodeSegment` alone, as Node names it on
//! POSIX, so an id such as `con`, `nightly.`, or `aux.txt` is laid out as
//! any other. On a Win32 path platform, where Windows would treat such a
//! name as a device or drop its trailing dot, it is refused as
//! [`LogFileLimit::WindowsName`].
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
//! no trailing or repeated separator. Node lists a directory in the
//! filesystem's order, which this model takes as byte order, so where a
//! root holds more than one entry TypeScript refuses, the one a flat-layout
//! or encoding-mismatch message names may differ.
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
//! is a path the filesystem refuses, such as one longer than a file or path
//! name may be, or a root another process changes, a lock file removed or
//! replaced included. A symbolic link is followed where a path is joined,
//! never where a directory is listed, as Node's `Dirent.isDirectory` does
//! not follow one. Every listing is visited in byte order of its UTF-8
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
use crate::json_parse::{Deep, dismantle};
use crate::log_layout::{CURRENT_LOG_FILENAME, canonical_generation, encode_segment, log_path};
use crate::released_rows::{ParseStop, parse_released_header, parse_released_rows};
use crate::v1_codec::decode_v0_v1_items_before_finish;
use crate::v2_to_v3::{RecoverableRefusal, recoverable_refusal, rethrows_recovery_issue};
use crate::write_lease::{LeaseRefusal, WriteLease};
use crate::{
    AppendRefusal, CURRENT_SESSION_FORMAT_VERSION, CreateRefusal, FinalCheckRefusal,
    GenerationHeaderRefusal, HistoryLocation, HistoryRefusal, MigratedV2, PathPlatform,
    PlainAppendLog, ScanRefusal, SessionHeader, SubsetLimit, V1CodecLocation, V1CodecRecovery,
    V1CodecRefusal, V1CodecVersion, V2ToV3Layer, V2ToV3Location, V2ToV3Refusal,
    check_transformed_artifact, encode_event_line, encode_header_line, first_record,
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
    /// A Session directory holds a canonical generation with the `.zstd`
    /// suffix, which a backend configured for compression `none` does not
    /// read; TypeScript's `encodingMismatch` throws a plain `Error` with this
    /// exact message, which names that generation's path. Nothing was
    /// written.
    EncodingMismatch { message: String },
    /// The flat legacy layout: a project directory holds a regular file
    /// named `*.jsonl` or `*.jsonl.zstd`, which `listSessionDirs` refuses, or
    /// `findLog`'s `rejectLegacyFlatArtifact` opened
    /// `<project>/<encoded id>.jsonl.zstd` or `.jsonl`. That probe opens a
    /// directory too, so a Session directory `nightly.jsonl` refuses the id
    /// `nightly`. TypeScript's `legacyLayout` throws a plain `Error` with
    /// this exact message, which names the path. Nothing was written.
    LegacyLayout { message: String },
    /// A selected current log's header id differs from the requested one,
    /// or its id and `cwd` name another path than the selected one, which
    /// `realpath` does not resolve to the same file; TypeScript's
    /// `assertStoredIdentity` throws a plain `Error` with this exact message,
    /// which names the selected path, and the other path when the id
    /// matches. The lock file is kept.
    StoredIdentity { message: String },
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
            | Self::EncodingMismatch { message }
            | Self::LegacyLayout { message }
            | Self::StoredIdentity { message }
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
    /// On a Win32 path platform, the encoded id ends in `.` or names a
    /// Windows device, such as `CON`, `nul`, `aux.txt`, or `COM1`, which
    /// Windows would not keep as a directory name. A POSIX path platform
    /// lays such an id out as TypeScript does.
    WindowsName,
    /// The root or a project directory lists a name that is not UTF-8, which
    /// Node lists with replacement characters and then opens by that spelling.
    NonUtf8Name,
    /// The id's highest canonical generation is newer than the current one;
    /// TypeScript reads its header and refuses it.
    NewerGeneration,
    /// [`PlainAppendLog::open`] refused the stored bytes, which TypeScript
    /// reports with the path of the log in its message.
    Scan(ScanRefusal),
    /// Migrating an older generation reached input whose TypeScript outcome
    /// this crate does not decide. Nothing was written. The name is one of:
    ///
    /// - `header/<name>` or `row/<name>`: the header or a row is not UTF-8
    ///   (`invalid-utf8`), holds a number beyond the double range, which
    ///   `JSON.parse` admits and the parser, which reads any nesting depth
    ///   and keeps lone surrogates, refuses (`json-parser`), or a number has
    ///   more integer digits than
    ///   serde_json rounds as `JSON.parse` does (`number-lexeme`), as
    ///   [`crate::ScanLimit`] describes them; or the header's version or a
    ///   count is held as a float (`float-lexeme`) or its identity check
    ///   reports `version-diagnostic`.
    /// - `codec/<name>`, `history/<name>`, `v2-to-v3/<name>`, or
    ///   `final-check/<name>`: a limit of [`decode_v0_v1_items`](crate::decode_v0_v1_items),
    ///   [`migrate_released_history`], [`migrate_v2_rows`], or
    ///   [`check_transformed_artifact`], under its name there.
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
    /// the id. No file or directory is written. The header's strings use
    /// [`crate::js_string`]'s spelling, as [`crate::parse_json`] returns them.
    pub fn create(
        root: &Path,
        header: &Value,
        inherited_event_count: Option<u64>,
    ) -> Result<Self, LogFileRefusal> {
        let log = PlainAppendLog::create(header, inherited_event_count)
            .map_err(LogFileRefusal::Create)?;
        let encoded = encoded_id(log.id(), PathPlatform::host())?;
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
    /// as [`PlainAppendLog::open`] does. `id` uses [`crate::js_string`]'s
    /// spelling: [`crate::js_string::from_rust`] spells an argument.
    pub fn open(root: &Path, id: &str, source_budget: usize) -> Result<Self, LogFileRefusal> {
        if id.is_empty() {
            return Err(LogFileRefusal::NativeSubset(LogFileLimit::EmptyId));
        }
        let encoded = encoded_id(id, PathPlatform::host())?;
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
        // The scan admitted this header record, so it decodes again.
        let header = first_record(&bytes)
            .map(|record| read_header_record(record, platform))
            .and_then(Result::ok)
            .ok_or_else(|| io::Error::other("an opened log's header decodes again"))?;
        if let Some(cause) = stored_identity_mismatch(root, id, &encoded, &header, &selected)? {
            return Err(LogFileRefusal::StoredIdentity { message: cause });
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
        let refusal = |refused: ReleasedGenerationRefusal| refused.into_refusal(id, selected);
        // `validateSourceIdentity` checks a header its codec reads, before any row.
        if let Some(stored) =
            released_generation_header(&bytes, selected.version, platform).map_err(refusal)?
            && let Some(cause) = stored_identity_mismatch(root, id, encoded, &stored, selected)?
        {
            return Err(refusal(ReleasedGenerationRefusal::Corrupt(format!(
                "Error: {cause}"
            ))));
        }
        let migrated = migrate_released_generation(&bytes, selected.version, source_budget)
            .map_err(refusal)?;
        // The catalog checks the transformed artifact when the chain finishes.
        if let Err(checked) = check_transformed_artifact(&migrated, platform) {
            return Err(refusal(match checked {
                FinalCheckRefusal::NativeSubset(name) => {
                    ReleasedGenerationRefusal::Limit(format!("final-check/{name}"))
                }
                checked => ReleasedGenerationRefusal::Unsupported(
                    checked
                        .catalog_message(selected.version)
                        .unwrap_or_default(),
                ),
            }));
        }
        let encode = || ReleasedGenerationRefusal::Limit("encode".to_owned());
        let mut text = encode_header_line(&migrated.header, Some(migrated.inherited_event_count))
            .map_err(|_| refusal(encode()))?;
        text.push('\n');
        for event in &migrated.events {
            text.push_str(&encode_event_line(event).map_err(|_| refusal(encode()))?);
            text.push('\n');
        }
        let bytes = text.into_bytes();
        let log = PlainAppendLog::open(&bytes, platform, source_budget)
            .map_err(|_| refusal(ReleasedGenerationRefusal::Limit("scan".to_owned())))?;
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

/// Why [`released_generation_header`] or [`migrate_released_generation`]
/// read no migrated Session, before the backend's `generationFailure` puts
/// the requested id and the source path in its message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleasedGenerationRefusal {
    /// TypeScript throws `SessionPersistenceCorruptionError` with the message
    /// `session "<id>": stored log is corrupt: <cause> (raw log: <source>)`;
    /// this is the cause, `String(error)` of the underlying error.
    Corrupt(String),
    /// TypeScript throws `SessionFormatUnsupportedError` with the message
    /// `<reason>; source v<N> artifact remains unchanged (raw log:
    /// <source>)`; this is the format edge's or the catalog's reason.
    Unsupported(String),
    /// This crate does not decide the TypeScript outcome; the string names
    /// the limit as [`LogFileLimit::Migration`] names it.
    Limit(String),
}

impl ReleasedGenerationRefusal {
    /// The backend's `generationFailure` translation.
    fn into_refusal(self, id: &str, selected: &Generation) -> LogFileRefusal {
        let source = selected.path.display().to_string();
        let source = crate::js_string::from_rust(&source);
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

impl From<ParseStop> for ReleasedGenerationRefusal {
    fn from(stop: ParseStop) -> Self {
        match stop {
            ParseStop::Corrupt(message) => Self::Corrupt(format!("Error: {message}")),
            ParseStop::Limit(name) => Self::Limit(name.to_owned()),
        }
    }
}

/// The header of a plain v0, v1, or v2 generation, read as the backend's
/// `prepareStoredMigration` reads it before any row: framed and parsed as
/// `decodeStreamingMigration` frames and parses it, then read by the
/// catalog for `validateSourceIdentity`. `source_version` is the version the
/// file name selected, and `log` is the whole file.
///
/// `Ok(Some(header))` is the migrated header whose stored identity the
/// caller checks against the requested id and the selected path before
/// calling [`migrate_released_generation`]; `Ok(None)` is a header the
/// identity check skips, which the codec refuses. Nothing is read from or
/// written to a file.
pub fn released_generation_header(
    log: &[u8],
    source_version: u64,
    platform: PathPlatform,
) -> Result<Option<SessionHeader>, ReleasedGenerationRefusal> {
    let Some(record) = first_record(log) else {
        return Err(ReleasedGenerationRefusal::Corrupt(
            "Error: empty or header-less session log".to_owned(),
        ));
    };
    dismantle(parse_released_header(record, source_version)?);
    match read_generation_header_record(record, source_version, platform) {
        Ok(stored) => Ok(stored),
        // The codec refuses a header with retired fields.
        Err(GenerationHeaderRefusal::Rejected(_)) => Ok(None),
        Err(GenerationHeaderRefusal::Unsupported(_)) => Err(invariant()),
        Err(GenerationHeaderRefusal::NativeSubset(limit)) => Err(ReleasedGenerationRefusal::Limit(
            header_limit(limit).to_owned(),
        )),
    }
}

/// Migrate a plain v0, v1, or v2 generation in memory, as the backend's
/// `prepareStoredMigration` decodes it after
/// [`released_generation_header`] admitted its header: the rows are parsed,
/// decoded by the released codec in recoverable mode, and read through every
/// format edge to v3, in TypeScript's streaming order, with this host's path
/// platform and `source_budget` for each expanded `sourceEventSeqs` list.
///
/// The catalog's final check is not run; [`crate::restore_migrated`] runs it.
/// Nothing is read from or written to a file, so the source stays as it was,
/// as a TypeScript read leaves it. A `source_version` above 2 is the
/// `decode-invariant` limit.
pub fn migrate_released_generation(
    log: &[u8],
    source_version: u64,
    source_budget: usize,
) -> Result<MigratedV2, ReleasedGenerationRefusal> {
    let Some(record) = first_record(log) else {
        return Err(ReleasedGenerationRefusal::Corrupt(
            "Error: empty or header-less session log".to_owned(),
        ));
    };
    let header = Deep::new(parse_released_header(record, source_version)?);
    let parsed = parse_released_rows(&log[record.len()..]);
    let stop = parsed.stop.map(ReleasedGenerationRefusal::from);
    match source_version {
        2 => migrate_v2(&header, &parsed.rows, stop, source_budget),
        0 => migrate_v0_v1(
            &header,
            &parsed.rows,
            stop,
            V1CodecVersion::V0,
            source_budget,
        ),
        1 => migrate_v0_v1(
            &header,
            &parsed.rows,
            stop,
            V1CodecVersion::V1,
            source_budget,
        ),
        _ => Err(invariant()),
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
    stop: Option<ReleasedGenerationRefusal>,
    version: V1CodecVersion,
    source_budget: usize,
) -> Result<MigratedV2, ReleasedGenerationRefusal> {
    let decode = |rows: &[Value]| {
        decode_v0_v1_items_before_finish(
            header,
            rows,
            version,
            V1CodecRecovery::Recoverable,
            PathPlatform::host(),
            source_budget,
        )
    };
    let refusal = match decode(rows) {
        Ok((items, finish)) => {
            return match (migrate_released_history(&items), stop, finish) {
                (Err(refused), _, _) if !at_finish(&refused) => {
                    Err(ReleasedGenerationRefusal::history(refused))
                }
                // Every row streamed before the stop, and `finish` runs after it.
                (_, Some(stop), _) => Err(stop),
                // The decoder's `finish` runs before the chain's.
                (_, None, Some(finish)) => Err(ReleasedGenerationRefusal::codec(finish)),
                (outcome, None, None) => outcome.map_err(ReleasedGenerationRefusal::history),
            };
        }
        Err(refusal) => refusal,
    };
    let location = match &refusal {
        V1CodecRefusal::Rejected { location, .. }
        | V1CodecRefusal::NativeSubset { location, .. } => *location,
    };
    let row = match location {
        V1CodecLocation::Header => return Err(ReleasedGenerationRefusal::codec(refusal)),
        // `finish` refusals come back with the items.
        V1CodecLocation::Finish => return Err(invariant()),
        V1CodecLocation::Row(row) => row,
    };
    // The items before the refused row streamed through every edge first;
    // the decoder's `finish` never runs after a row refusal.
    let prefix = match decode(rows.get(..row).unwrap_or_default()) {
        Ok((prefix, _)) => prefix,
        Err(_) => return Err(invariant()),
    };
    match migrate_released_history(&prefix) {
        Err(refused) if !at_finish(&refused) => Err(ReleasedGenerationRefusal::history(refused)),
        _ => Err(ReleasedGenerationRefusal::codec(refusal)),
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

fn invariant() -> ReleasedGenerationRefusal {
    ReleasedGenerationRefusal::Limit("decode-invariant".to_owned())
}

/// A v2 log's parsed `rows` through the released v2 codec in recoverable
/// mode and the v2→v3 edge, then `stop`, in TypeScript's streaming order.
/// [`migrate_v2_rows`] decodes strictly; where its codec refuses a row, the
/// recoverable codec drops that row and every later one unless one of them
/// throws: the refused row when it is a `session/end-seed` row whose data
/// is not an object or a `turn/end` row with a seq gap, otherwise the first
/// later `turn/end` row `decodeEvent` admits, which rethrows the refusal.
fn migrate_v2(
    header: &Value,
    rows: &[Value],
    stop: Option<ReleasedGenerationRefusal>,
    source_budget: usize,
) -> Result<MigratedV2, ReleasedGenerationRefusal> {
    let migrate =
        |rows: &[Value]| migrate_v2_rows(header, rows, PathPlatform::host(), source_budget);
    let streamed = match migrate(rows) {
        Err(
            refusal @ V2ToV3Refusal::Rejected {
                location: V2ToV3Location::Row(row),
                layer: V2ToV3Layer::Codec,
                ..
            },
        ) => {
            let thrown = match rows
                .get(row)
                .and_then(|refused| recoverable_refusal(refused, row, source_budget))
            {
                Some(RecoverableRefusal::Thrown | RecoverableRefusal::Gap { turn_end: true }) => {
                    true
                }
                Some(RecoverableRefusal::Caught | RecoverableRefusal::Gap { turn_end: false }) => {
                    let mut rethrown = false;
                    for (later, value) in rows.iter().enumerate().skip(row.saturating_add(1)) {
                        match rethrows_recovery_issue(value, source_budget) {
                            Ok(true) => {
                                rethrown = true;
                                break;
                            }
                            Ok(false) => {}
                            Err(limit) => {
                                return Err(ReleasedGenerationRefusal::v2(
                                    V2ToV3Refusal::NativeSubset {
                                        location: V2ToV3Location::Row(later),
                                        limit,
                                    },
                                ));
                            }
                        }
                    }
                    rethrown
                }
                None => return Err(invariant()),
            };
            // Every row before the refused one passed the codec and the
            // stage, so the refusal itself is what a throw reports.
            if thrown {
                return Err(ReleasedGenerationRefusal::v2(refusal));
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
        _ => streamed.map_err(ReleasedGenerationRefusal::v2),
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

/// The id's path segment, or, on a Win32 path platform, the
/// [`LogFileLimit::WindowsName`] limit.
fn encoded_id(id: &str, platform: PathPlatform) -> Result<String, LogFileRefusal> {
    let encoded = encode_segment(id);
    if platform == PathPlatform::Win32 && windows_device_or_dot(&encoded) {
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

/// `assertStoredIdentity` of the `selected` generation, whose `header` was
/// read for the requested `id`, encoded as `encoded`: the message of the
/// `Error` TypeScript throws, or `None` when the header names the selected
/// path, by spelling or else by `realpath`. A requested id is never empty,
/// so `generationLogPath` names a path.
fn stored_identity_mismatch(
    root: &Path,
    id: &str,
    encoded: &str,
    header: &SessionHeader,
    selected: &Generation,
) -> Result<Option<String>, LogFileRefusal> {
    let path = crate::js_string::from_rust(&selected.path.display().to_string()).into_owned();
    if header.id != id {
        return Ok(Some(format!(
            "corrupt session log \"{path}\": requested id \"{id}\" does not match header id \"{}\"",
            header.id
        )));
    }
    let current = log_path(root, header.cwd.as_deref(), encoded);
    let expected = match selected.path.file_name() {
        Some(name) => current.with_file_name(name),
        None => current,
    };
    if expected == selected.path || same_file(&selected.path, &expected)? {
        return Ok(None);
    }
    let expected = crate::js_string::from_rust(&expected.display().to_string()).into_owned();
    Ok(Some(format!(
        "corrupt session log \"{path}\": header id \"{id}\" and cwd identify \"{expected}\""
    )))
}

/// `sameFile`: whether both paths resolve to one file, as `realpath`
/// resolves them; an absent path resolves to none.
fn same_file(path: &Path, expected: &Path) -> Result<bool, LogFileRefusal> {
    let resolve = |path: &Path| match fs::canonicalize(path) {
        Ok(resolved) => Ok(Some(resolved)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(LogFileRefusal::Io(error)),
    };
    let (actual, expected) = (resolve(path)?, resolve(expected)?);
    Ok(actual.is_some() && actual == expected)
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
/// directory or a regular file, symlinks not followed.
struct Entry {
    name: Option<String>,
    is_dir: bool,
    is_file: bool,
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
        let kind = entry.file_type()?;
        listed.push(Entry {
            is_dir: kind.is_dir(),
            is_file: kind.is_file(),
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
        // `listSessionDirs` refuses a flat file before any Session directory
        // of the project is checked; a directory with that suffix is a
        // Session directory, since it checks `isFile()`.
        let mut sessions = Vec::new();
        for entry in list(&project_dir)?.unwrap_or_default() {
            let Some(name) = entry.name else {
                return Err(NON_UTF8_NAME);
            };
            if entry.is_file && (name.ends_with(".jsonl") || name.ends_with(".jsonl.zstd")) {
                return Err(legacy_layout(&project_dir.join(name)));
            }
            if entry.is_dir {
                sessions.push(name);
            }
        }
        for session in sessions {
            let dir = project_dir.join(session);
            let highest = utf8_names(&dir)?
                .into_iter()
                .filter_map(|name| opposite_generation(&name).map(|version| (version, name)))
                .max();
            if let Some((_, name)) = highest {
                return Err(encoding_mismatch(&dir.join(name)));
            }
        }
    }
    let mut found = Vec::new();
    for project in &projects {
        let project_dir = root.join(project);
        // `rejectLegacyFlatArtifact` probes the id's flat names first.
        for suffix in [".jsonl.zstd", ".jsonl"] {
            let path = project_dir.join(format!("{encoded}{suffix}"));
            if probe_exists(&path)? {
                return Err(legacy_layout(&path));
            }
        }
        let dir = project_dir.join(encoded);
        let names = utf8_names(&dir)?;
        // `resolveGenerationInDirectory` names the first one it lists.
        if let Some(name) = names
            .iter()
            .find(|name| opposite_generation(name).is_some())
        {
            return Err(encoding_mismatch(&dir.join(name)));
        }
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

/// `exists(path)`: whether `open(path, 'r')` succeeds, following a symlink;
/// only an absent path is false. libuv opens with `O_RDONLY` on POSIX and
/// with `FILE_FLAG_BACKUP_SEMANTICS` on Windows (`fs__open` in
/// `src/win/fs.c`), so a directory exists on both.
fn probe_exists(path: &Path) -> Result<bool, LogFileRefusal> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0200_0000); // FILE_FLAG_BACKUP_SEMANTICS
    }
    match options.open(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// TypeScript's `legacyLayout(path)`, which spells `path` with
/// `JSON.stringify`.
fn legacy_layout(path: &Path) -> LogFileRefusal {
    let path = path.display().to_string();
    let spelled = crate::js_string::quote(&crate::js_string::from_rust(&path));
    LogFileRefusal::LegacyLayout {
        message: format!(
            "session artifact {spelled} uses the unsupported flat-file layout; use a separate \
             root or move it into a project/session directory before loading"
        ),
    }
}

/// The generation a canonical name with the `.zstd` suffix carries,
/// `parseGenerationLogFilename(name, 'zstd')`.
fn opposite_generation(name: &str) -> Option<u64> {
    name.strip_suffix(".zstd").and_then(canonical_generation)
}

/// TypeScript's `encodingMismatch(path)` of a backend configured for
/// compression `none`, which spells `path` with `JSON.stringify`.
fn encoding_mismatch(path: &Path) -> LogFileRefusal {
    let path = path.display().to_string();
    let spelled = crate::js_string::quote(&crate::js_string::from_rust(&path));
    LogFileRefusal::EncodingMismatch {
        message: format!(
            "session artifact {spelled} uses .jsonl.zstd, but this backend is configured for \
             compression \"none\"; use a separate root or select the matching compression mode"
        ),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_names_are_refused_on_the_win32_path_platform_only() {
        for (id, encoded) in [
            ("con", "con"),
            ("CON", "CON"),
            ("nightly.", "nightly."),
            ("aux.txt", "aux.txt"),
            ("Nul", "Nul"),
            ("COM1", "COM1"),
            ("lpt9.log", "lpt9.log"),
        ] {
            assert_eq!(
                encoded_id(id, PathPlatform::Posix).ok().as_deref(),
                Some(encoded),
                "{id}"
            );
            assert!(
                matches!(
                    encoded_id(id, PathPlatform::Win32),
                    Err(LogFileRefusal::NativeSubset(LogFileLimit::WindowsName))
                ),
                "{id}"
            );
        }
        // A space and `$` are escaped, so `CONIN$` and `con ` are not devices.
        for (id, encoded) in [
            ("console", "console"),
            ("COM0", "COM0"),
            ("COM10", "COM10"),
            ("a.con", "a.con"),
            ("CONIN$", "CONIN~0024"),
            ("con ", "con~0020"),
            (".", "~002E"),
            ("..", "~002E~002E"),
        ] {
            for platform in [PathPlatform::Posix, PathPlatform::Win32] {
                assert_eq!(
                    encoded_id(id, platform).ok().as_deref(),
                    Some(encoded),
                    "{id} {platform:?}"
                );
            }
        }
    }

    #[test]
    fn encoding_mismatch_spells_the_path_as_json_stringify_does() {
        let refusal = encoding_mismatch(Path::new("/r/\"q\"/session.v3.jsonl.zstd"));
        assert_eq!(
            refusal.message(),
            Some(
                "session artifact \"/r/\\\"q\\\"/session.v3.jsonl.zstd\" uses .jsonl.zstd, but \
                 this backend is configured for compression \"none\"; use a separate root or \
                 select the matching compression mode"
            )
        );
    }
}
