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
//!   generation, which must be the current one; its bytes are opened as
//!   [`PlainAppendLog::open`] opens them, and the selected path must be the
//!   one the header's id and `cwd` name.
//! - [`PlainLogFile::append`] and [`PlainLogFile::flush`] run the model's
//!   operation, then bring the file to the model's bytes, refused operations
//!   included: a first write creates the Session directory and a new file
//!   holding every byte, and a later one truncates the file to the bytes it
//!   shares with the model and writes the rest.
//!
//! Dropping the value is the handle's `close`. The refusals TypeScript's
//! `SessionAlreadyExistsError`, `SessionPersistenceNotFoundError`, and
//! duplicate-id `Error` carry are returned with their exact messages.
//!
//! The write lease and its `session.lock` file, fsync and directory sync,
//! the temporary file and `link` publication, rollback after a failed write,
//! file modes, Zstd compression, migration of an older generation, and the
//! `validateStoredEvents` check of an opened log are not modelled. Opening a
//! log TypeScript's validation refuses is outside this model's domain, as is
//! a path the filesystem refuses, such as one longer than a file or path
//! name may be, a symlink, a root another process changes, or a second handle
//! of the same Session open at once, which TypeScript refuses in-process.
//! Every listing is visited in byte order of its UTF-8 names. After an I/O
//! error the file may hold a partial write, and every later operation on the
//! value fails.

use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::log_layout::{canonical_generation, encode_segment, log_path};
use crate::{
    AppendRefusal, CURRENT_SESSION_FORMAT_VERSION, CreateRefusal, PathPlatform, PlainAppendLog,
    ScanRefusal, first_record, read_header_record,
};

/// One write handle of a plain current-format Session log under a root.
#[derive(Debug)]
pub struct PlainLogFile {
    path: PathBuf,
    log: PlainAppendLog,
    failed: bool,
}

/// Why an operation of [`PlainLogFile`] was refused.
#[derive(Debug)]
pub enum LogFileRefusal {
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
            Self::AlreadyExists { message }
            | Self::NotFound { message }
            | Self::Duplicate { message } => Some(message),
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
    /// The id's highest canonical generation is older than the current one;
    /// TypeScript migrates it.
    OlderGeneration,
    /// The id's highest canonical generation is newer than the current one;
    /// TypeScript reads its header and refuses it.
    NewerGeneration,
    /// The selected log's header id differs from the requested one, or its
    /// id and `cwd` name another path than the selected one. TypeScript
    /// refuses unless `realpath` resolves both paths to one file.
    Identity,
    /// [`PlainAppendLog::open`] refused the stored bytes, which TypeScript
    /// reports with the path of the log in its message.
    Scan(ScanRefusal),
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
        if selected.version < CURRENT_SESSION_FORMAT_VERSION {
            return Err(LogFileRefusal::NativeSubset(LogFileLimit::OlderGeneration));
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
    /// after a torn tail was truncated.
    pub fn append(&mut self, events: &[Value]) -> Result<(), LogFileRefusal> {
        self.check_usable()?;
        let before = self.log.bytes().map(<[u8]>::to_vec);
        let outcome = self.log.append(events);
        self.sync(before.as_deref())?;
        outcome.map_err(LogFileRefusal::Append)
    }

    /// The handle's `flush`: an unwritten log's file is created holding the
    /// header line alone; a written one is left as it is.
    pub fn flush(&mut self) -> Result<(), LogFileRefusal> {
        self.check_usable()?;
        let before = self.log.bytes().map(<[u8]>::to_vec);
        self.log.flush();
        self.sync(before.as_deref())
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
