//! Locked, atomic updates of a JSON object file in the Bake home.
//!
//! Adapted from the file backends of Pi `packages/coding-agent/src/core/`
//! `auth-storage.ts` (`FileAuthStorageBackend.withLock`) and
//! `settings-manager.ts` (`FileSettingsStorage.withLock`), release v1.2.0,
//! revision `06a466935536f8c9bcacb2d5efc0fc021be1bdff` (MIT; see the crate's
//! `NOTICE`). Pi takes a lock, reads the file's current text, lets the caller
//! compute the next text from it, and writes that. Both of Bake's writable
//! files, `settings.json` ([`crate::settings::SettingsStore`]) and
//! `auth.json` ([`crate::auth_storage::AuthStorage`]), share this backend, and
//! every change is applied to the contents read under the lock. A process
//! holding an old copy therefore changes only the keys it sets, and an
//! update never discards another process's write.
//!
//! # Guarantees
//!
//! - **Serialized.** Each update holds an exclusive OS file lock
//!   ([`File::try_lock`]: `flock` on Unix, `LockFileEx` on Windows) on a
//!   sibling `.<name>.lock` file, waiting with backoff up to
//!   [`DEFAULT_LOCK_TIMEOUT`]. The lock belongs to the open handle, so
//!   updates exclude each other within one process as well as across
//!   processes.
//! - **No stale locks.** The OS releases the lock when its holder closes the
//!   handle or exits, including by a crash, so a dead process never blocks a
//!   later update. The lock file itself is empty and is never deleted:
//!   deleting a lock file that another process is waiting on would let two
//!   writers in at once.
//! - **Atomic.** The next contents go to a new temporary file in the same
//!   directory, are flushed with `sync_all`, and replace the file by
//!   `rename`. Readers,
//!   which take no lock, see the old or the new file and never a partial
//!   one. A failed update removes its temporary file, and the next update
//!   removes any left by a writer that died mid-update.
//! - **Durability.** On Unix the directory is flushed after the rename. An
//!   update in an existing directory that returns `Ok` survives power loss
//!   on file systems that honor `fsync`. Newly created ancestor directories
//!   are not flushed; their creation is outside this durability guarantee.
//!   When the final directory flush fails, the update returns
//!   [`StoreError::Unsynced`]: the new contents are in place and later reads
//!   see them, but a power loss may still revert them
//!   ([`StoreError::is_committed`] tells this error from the others, which
//!   leave the file as it was). Windows has no directory flush: after a power
//!   loss right after an update, the file may hold either version.
//! - **Private, and access kept.** A missing directory is created owner-only
//!   (`0700` on Unix), and a new file owner-only (`0600`); on Windows both
//!   take the inherited ACL of their parent, as Pi's files do. An existing
//!   file must be writable by this process, as Pi's in-place write requires,
//!   so a read-only file is refused even where the directory would let a
//!   rename replace it. Pi's in-place write keeps the file's access, which a
//!   replacement must carry over by hand, and it does so on the empty
//!   temporary file, before any contents are written:
//!   - Unix: its owner and group (a change of either needs privilege or
//!     group membership; when the change fails the update is refused) and
//!     its permission bits.
//!   - Linux: its POSIX access ACL (`system.posix_acl_access`), or no access
//!     ACL when it has none, so an ACL a default ACL of the directory gave
//!     the temporary file is removed.
//!   - macOS: its ACL, copied by `fcopyfile(3)` with `COPYFILE_ACL`.
//!   - Windows: its DACL is copied onto the temporary file. This adapts
//!     Bake's `writeFileAtomic` in `packages/fs/fs-local/src/fsio.ts` and
//!     `src/win32.ts`. The DACL is read by path when the replacement is
//!     prepared, not from the handle the update read, so it is whatever
//!     object holds the name then.
//!
//!   A replacement does not keep other extended attributes, an SELinux
//!   label, BSD file flags, or hard links (a hard-linked file is split from
//!   its other names), or the Windows owner and alternate data streams. On
//!   other Unix systems no ACL is carried over.
//! - **Refusals.** An update reads the file first and writes nothing when it
//!   is not a regular file (a symbolic link or a directory), cannot be opened
//!   for reading and writing, is larger than the store's limit, is not UTF-8,
//!   is not JSON, or is not a JSON object. The update itself is refused when
//!   its result would exceed the limit.
//! - **Checked contents.** A crate-internal checked update lets the caller
//!   refuse the file through the handle its bytes were read from, under the
//!   lock and after the read, so the contents a change starts from are the
//!   ones that check passed on, not those of an earlier read by path.
//!
//! # Limitations
//!
//! - The lock is advisory. A program that rewrites the file without it,
//!   such as an editor, can still lose an update or have its edit replaced.
//! - Lock semantics on network file systems depend on the file system.
//! - An update needs write access to the directory, for the lock file and
//!   the temporary file; Pi's in-place write needs only the file's.
//! - Writes are JSON with two-space indentation and no trailing newline, as
//!   Pi's `JSON.stringify(value, null, 2)`. Members keep their order, but a
//!   rewritten file does not keep its original whitespace, a leading
//!   byte-order mark, or a number's spelling (`1e3` becomes `1000.0`).
//! - Updates block the calling thread; async callers run them on a blocking
//!   thread.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::auth_storage::strip_bom;

#[cfg(test)]
mod tests;

/// How long an update waits for another holder of the lock.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// The longest pause between attempts to take the lock.
const MAX_LOCK_BACKOFF: Duration = Duration::from_millis(50);

/// Why an update failed. Every variant but [`StoreError::Unsynced`] means
/// the file was left as it was. Messages name the file, never its contents.
#[derive(Debug)]
#[non_exhaustive]
pub enum StoreError {
    /// A file operation failed.
    Io {
        /// What was being done, as in "Could not *read* `path`".
        operation: &'static str,
        /// The file or directory.
        path: PathBuf,
        /// The OS error.
        source: io::Error,
    },
    /// Another holder kept the lock for longer than the wait allows.
    Locked {
        /// The data file the lock guards.
        path: PathBuf,
        /// How long the update waited.
        waited: Duration,
    },
    /// The file, or the update's result, cannot be written safely.
    Refused {
        /// The data file.
        path: PathBuf,
        /// Why, without the file's contents.
        reason: String,
    },
    /// The new contents replaced the file, but flushing its directory
    /// failed: the update is visible and later reads see it, but a power
    /// loss may revert it. Unix only.
    Unsynced {
        /// The data file.
        path: PathBuf,
        /// The OS error of opening or flushing the directory.
        source: io::Error,
    },
}

impl StoreError {
    /// Whether the update replaced the file despite the error, which is
    /// true only of [`StoreError::Unsynced`]. A caller that keeps a copy of
    /// the file's contents takes the new contents as stored.
    pub fn is_committed(&self) -> bool {
        matches!(self, Self::Unsynced { .. })
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(f, "Could not {operation} {}: {source}", path.display()),
            Self::Locked { path, waited } => write!(
                f,
                "Timed out after {} ms waiting for the lock on {}",
                waited.as_millis(),
                path.display()
            ),
            Self::Refused { path, reason } => {
                write!(f, "Refusing to update {}: {reason}", path.display())
            }
            Self::Unsynced { path, source } => write!(
                f,
                "Updated {}, but could not flush its directory, so the update may not survive a power loss: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } | Self::Unsynced { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// A JSON object file updated under a lock (see the module documentation).
#[derive(Debug, Clone)]
pub(crate) struct JsonFileStore {
    path: PathBuf,
    limit: u64,
    lock_timeout: Duration,
    #[cfg(test)]
    fault: Option<Fault>,
    #[cfg(test)]
    hook: Option<Hook>,
}

/// Where in an update a test runs [`Hook::run`].
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    /// Under the lock, before the file is inspected or opened.
    BeforeOpen,
    /// Under the lock, after the bytes are read and before the check.
    AfterRead,
}

/// Code a test runs at one [`Stage`] of an update, under its lock: a
/// deterministic point for a concurrent change.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct Hook {
    stage: Stage,
    run: std::sync::Arc<dyn Fn() + Send + Sync>,
}

#[cfg(test)]
impl fmt::Debug for Hook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hook").field("stage", &self.stage).finish()
    }
}

/// A failure a test injects into an update.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fault {
    /// The replacement fails after the temporary file is written.
    BeforeRename,
    /// Flushing the directory fails after the rename.
    DirectorySync,
}

/// The file as read under the lock: its object, and the open handle and
/// metadata whose access the replacement keeps.
struct Current {
    object: Map<String, Value>,
    original: Original,
}

/// An existing file, opened for reading and writing. Unix keeps the handle
/// and metadata to copy their access; Windows copies the DACL by path.
struct Original {
    #[cfg(unix)]
    file: File,
    #[cfg(unix)]
    metadata: fs::Metadata,
}

/// The result of an update that changed the file or found nothing to change.
#[derive(Debug)]
pub(crate) struct Updated {
    /// The object as now stored.
    pub(crate) object: Map<String, Value>,
    /// [`StoreError::Unsynced`], when the file was replaced but its
    /// directory could not be flushed.
    pub(crate) unsynced: Option<StoreError>,
}

/// The held lock; dropping it closes the handle, which releases the lock.
pub(crate) struct StoreLock {
    _file: File,
}

/// Parses JSON object text, after a byte-order mark. The error is the
/// parser's message, which carries a position but none of the text.
pub(crate) fn parse_object(text: &str) -> Result<Map<String, Value>, String> {
    match serde_json::from_str::<Value>(strip_bom(text)) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err("expected a JSON object".to_owned()),
        Err(error) => Err(error.to_string()),
    }
}

/// Sets `object[key]` to `value`; whether that changed it.
pub(crate) fn set_member(object: &mut Map<String, Value>, key: &str, value: Value) -> bool {
    if object.get(key) == Some(&value) {
        return false;
    }
    object.insert(key.to_owned(), value);
    true
}

/// Removes `object[key]`, keeping the order of the other members; whether it
/// was there.
pub(crate) fn remove_member(object: &mut Map<String, Value>, key: &str) -> bool {
    object.shift_remove(key).is_some()
}

fn io_error(operation: &'static str, path: &Path) -> impl FnOnce(io::Error) -> StoreError + use<> {
    let path = path.to_path_buf();
    move |source| StoreError::Io {
        operation,
        path,
        source,
    }
}

fn private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Options that create owner-only files and, on Unix, do not follow a
/// symbolic link at the final component.
fn private_options() -> OpenOptions {
    let options = OpenOptions::new();
    #[cfg(unix)]
    let options = {
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = options;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        options
    };
    options
}

/// A random lowercase hex suffix for a temporary name.
fn random_suffix() -> io::Result<String> {
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
    Ok(random.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// A temporary file removed on drop unless it was renamed into place. The
/// handle is closed first, so Windows can remove or rename the file.
struct TempFile {
    path: PathBuf,
    file: Option<File>,
    armed: bool,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        drop(self.file.take());
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl JsonFileStore {
    /// The store at `path`, refusing files larger than `limit` bytes.
    pub(crate) fn new(path: PathBuf, limit: u64) -> Self {
        Self {
            path,
            limit,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
            #[cfg(test)]
            fault: None,
            #[cfg(test)]
            hook: None,
        }
    }

    /// The data file.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub(crate) fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_fault(mut self, fault: Fault) -> Self {
        self.fault = Some(fault);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_hook(
        mut self,
        stage: Stage,
        run: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        self.hook = Some(Hook {
            stage,
            run: std::sync::Arc::new(run),
        });
        self
    }

    #[cfg(test)]
    fn run_hook(&self, stage: Stage) {
        if let Some(hook) = self.hook.as_ref().filter(|hook| hook.stage == stage) {
            (hook.run)();
        }
    }

    #[cfg(test)]
    fn injected(&self, fault: Fault) -> Option<io::Error> {
        (self.fault == Some(fault)).then(|| io::Error::other("injected failure"))
    }

    fn refused(&self, reason: impl Into<String>) -> StoreError {
        StoreError::Refused {
            path: self.path.clone(),
            reason: reason.into(),
        }
    }

    fn dir(&self) -> &Path {
        match self.path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        }
    }

    fn file_name(&self) -> Result<&OsStr, StoreError> {
        self.path
            .file_name()
            .ok_or_else(|| self.refused("the path names no file"))
    }

    /// A sibling name, `.<name><suffix>`.
    fn sibling(&self, suffix: &str) -> Result<PathBuf, StoreError> {
        let mut name = OsString::from(".");
        name.push(self.file_name()?);
        name.push(suffix);
        Ok(self.dir().join(name))
    }

    /// Takes the lock, creating the directory and the lock file as needed.
    pub(crate) fn lock(&self) -> Result<StoreLock, StoreError> {
        let dir = self.dir();
        private_dir(dir).map_err(io_error("create the directory", dir))?;
        let lock_path = self.sibling(".lock")?;
        let file = private_options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(io_error("open the lock file", &lock_path))?;
        let start = Instant::now();
        let mut backoff = Duration::from_millis(1);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(StoreLock { _file: file }),
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(error)) => {
                    return Err(io_error("lock", &lock_path)(error));
                }
            }
            let waited = start.elapsed();
            let Some(remaining) = self
                .lock_timeout
                .checked_sub(waited)
                .filter(|r| !r.is_zero())
            else {
                return Err(StoreError::Locked {
                    path: self.path.clone(),
                    waited,
                });
            };
            std::thread::sleep(backoff.min(remaining));
            backoff = (backoff * 2).min(MAX_LOCK_BACKOFF);
        }
    }

    /// Reads the file for an update: `None` when it does not exist. `check`
    /// runs on the handle after its bytes are read, before they are parsed.
    fn read_current(
        &self,
        check: impl FnOnce(&File) -> Result<(), String>,
    ) -> Result<Option<Current>, StoreError> {
        #[cfg(test)]
        self.run_hook(Stage::BeforeOpen);
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error("inspect", &self.path)(error)),
        };
        if !metadata.file_type().is_file() {
            return Err(self.refused("it is not a regular file"));
        }
        let too_large = || self.refused(format!("it is larger than {} bytes", self.limit));
        if metadata.len() > self.limit {
            return Err(too_large());
        }
        // Opened for writing too, which checks what Pi's in-place write
        // checks: a read-only file is refused although a rename could
        // replace it.
        let file = private_options()
            .read(true)
            .write(true)
            .open(&self.path)
            .map_err(io_error("open for reading and writing", &self.path))?;
        let metadata = file.metadata().map_err(io_error("inspect", &self.path))?;
        if !metadata.file_type().is_file() {
            return Err(self.refused("it is not a regular file"));
        }
        let mut bytes = Vec::new();
        (&file)
            .take(self.limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(io_error("read", &self.path))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > self.limit {
            return Err(too_large());
        }
        #[cfg(test)]
        self.run_hook(Stage::AfterRead);
        check(&file).map_err(|reason| self.refused(reason))?;
        let text = String::from_utf8(bytes).map_err(|_| self.refused("it is not UTF-8"))?;
        let object = parse_object(&text).map_err(|reason| self.refused(reason))?;
        Ok(Some(Current {
            object,
            original: Original {
                #[cfg(unix)]
                file,
                #[cfg(unix)]
                metadata,
            },
        }))
    }

    /// Removes temporary files left by a writer that died mid-update. Only
    /// called under the lock, when no live writer has one open.
    fn remove_orphans(&self) {
        let Ok(name) = self.file_name() else { return };
        let mut prefix = OsString::from(".");
        prefix.push(name);
        prefix.push(".");
        let Some(prefix) = prefix.to_str() else {
            return;
        };
        let Ok(entries) = fs::read_dir(self.dir()) else {
            return;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(middle) = file_name
                .to_str()
                .and_then(|file_name| file_name.strip_prefix(prefix))
                .and_then(|rest| rest.strip_suffix(".tmp"))
            else {
                continue;
            };
            if middle.len() == 16 && middle.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    fn create_temp(&self) -> Result<TempFile, StoreError> {
        let path = self.sibling(&format!(
            ".{}.tmp",
            random_suffix().map_err(io_error("name a temporary file for", &self.path))?
        ))?;
        let file = private_options()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(io_error("create", &path))?;
        Ok(TempFile {
            path,
            file: Some(file),
            armed: true,
        })
    }

    /// Gives the empty temporary file the access of the file it replaces
    /// (see the module documentation).
    fn keep_access(
        &self,
        temp: &File,
        temp_path: &Path,
        original: &Original,
    ) -> Result<(), StoreError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, fchown};
            let created = temp.metadata().map_err(io_error("inspect", temp_path))?;
            let owner = (created.uid() != original.metadata.uid()).then(|| original.metadata.uid());
            let group = (created.gid() != original.metadata.gid()).then(|| original.metadata.gid());
            if owner.is_some() || group.is_some() {
                fchown(temp, owner, group).map_err(|error| {
                    self.refused(format!(
                        "its owner and group cannot be kept by a replacement: {error}"
                    ))
                })?;
            }
            platform::copy_acl(&original.file, temp).map_err(|error| {
                self.refused(format!(
                    "its access control list cannot be kept by a replacement: {error}"
                ))
            })?;
            temp.set_permissions(original.metadata.permissions())
                .map_err(io_error("set the permissions of", temp_path))?;
        }
        #[cfg(windows)]
        {
            let _ = (temp, original);
            platform::copy_dacl(&self.path, temp_path).map_err(|error| {
                self.refused(format!(
                    "its access control list cannot be kept by a replacement: {error}"
                ))
            })?;
        }
        #[cfg(not(any(unix, windows)))]
        let _ = (temp, temp_path, original);
        Ok(())
    }

    /// Replaces the file with `text`, keeping `original`'s access. Returns
    /// [`StoreError::Unsynced`] as a value once the file is replaced.
    fn replace(
        &self,
        text: &str,
        original: Option<Original>,
    ) -> Result<Option<StoreError>, StoreError> {
        let mut temp = self.create_temp()?;
        let temp_path = temp.path.clone();
        let Some(mut file) = temp.file.take() else {
            return Err(self.refused("the temporary file is closed"));
        };
        if let Some(original) = original {
            self.keep_access(&file, &temp_path, &original)?;
        }
        file.write_all(text.as_bytes())
            .map_err(io_error("write", &temp_path))?;
        file.sync_all().map_err(io_error("flush", &temp_path))?;
        drop(file);
        #[cfg(test)]
        if let Some(error) = self.injected(Fault::BeforeRename) {
            return Err(io_error("replace", &self.path)(error));
        }
        fs::rename(&temp_path, &self.path).map_err(io_error("replace", &self.path))?;
        temp.armed = false;
        Ok(self.sync_dir().err())
    }

    /// Flushes the directory after a rename, on Unix.
    fn sync_dir(&self) -> Result<(), StoreError> {
        #[cfg(test)]
        if let Some(source) = self.injected(Fault::DirectorySync) {
            return Err(StoreError::Unsynced {
                path: self.path.clone(),
                source,
            });
        }
        #[cfg(unix)]
        File::open(self.dir())
            .and_then(|dir| dir.sync_all())
            .map_err(|source| StoreError::Unsynced {
                path: self.path.clone(),
                source,
            })?;
        Ok(())
    }

    /// Applies `change` to the file's object under the lock, and writes the
    /// result when `change` reports that it changed something. A missing
    /// file reads as `{}`.
    pub(crate) fn update(
        &self,
        change: impl FnOnce(&mut Map<String, Value>) -> bool,
    ) -> Result<Updated, StoreError> {
        self.update_checked(|_| Ok(()), change)
    }

    /// [`Self::update`], first refusing an existing file when `check`
    /// returns an error for the handle its bytes were read from. `check`
    /// runs under the lock, after the read and before `change`, so the
    /// object `change` receives was read from the object `check` passed;
    /// a refusal is [`StoreError::Refused`] with its reason, and nothing is
    /// written. It is not called when the file does not exist. What `check`
    /// observes about that object holds when it runs; a writer with access
    /// to the object can still change it afterwards.
    pub(crate) fn update_checked(
        &self,
        check: impl FnOnce(&File) -> Result<(), String>,
        change: impl FnOnce(&mut Map<String, Value>) -> bool,
    ) -> Result<Updated, StoreError> {
        let _lock = self.lock()?;
        self.remove_orphans();
        let (mut object, original) = match self.read_current(check)? {
            Some(Current { object, original }) => (object, Some(original)),
            None => (Map::new(), None),
        };
        if !change(&mut object) {
            return Ok(Updated {
                object,
                unsynced: None,
            });
        }
        let text = serde_json::to_string_pretty(&object)
            .map_err(|error| self.refused(format!("it cannot be serialized: {error}")))?;
        if u64::try_from(text.len()).unwrap_or(u64::MAX) > self.limit {
            return Err(self.refused(format!(
                "the result would be larger than {} bytes",
                self.limit
            )));
        }
        let unsynced = self.replace(&text, original)?;
        Ok(Updated { object, unsynced })
    }
}

/// The platform's access-copying and replacement primitives.
#[cfg(unix)]
mod platform {
    use std::fs::File;
    use std::io;

    /// Gives `temp` the POSIX access ACL of `original`, or none.
    #[cfg(target_os = "linux")]
    pub(super) fn copy_acl(original: &File, temp: &File) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        const NAME: &std::ffi::CStr = c"system.posix_acl_access";
        let missing = |error: &io::Error| {
            matches!(error.raw_os_error(), Some(libc::ENODATA | libc::EOPNOTSUPP))
        };
        // SAFETY: a size query with no buffer, on an open descriptor.
        let size = unsafe {
            libc::fgetxattr(original.as_raw_fd(), NAME.as_ptr(), std::ptr::null_mut(), 0)
        };
        let Ok(size) = usize::try_from(size) else {
            let error = io::Error::last_os_error();
            if !missing(&error) {
                return Err(error);
            }
            // SAFETY: an open descriptor and a NUL-terminated name.
            if unsafe { libc::fremovexattr(temp.as_raw_fd(), NAME.as_ptr()) } != 0 {
                let error = io::Error::last_os_error();
                if !missing(&error) {
                    return Err(error);
                }
            }
            return Ok(());
        };
        let mut value = vec![0u8; size];
        // SAFETY: `value` holds `value.len()` bytes.
        let read = unsafe {
            libc::fgetxattr(
                original.as_raw_fd(),
                NAME.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
            )
        };
        // A negative count is an error, including an ACL that grew since the
        // size query (`ERANGE`).
        let read = usize::try_from(read).map_err(|_| io::Error::last_os_error())?;
        value.truncate(read);
        // SAFETY: `value` holds `value.len()` initialized bytes.
        let set = unsafe {
            libc::fsetxattr(
                temp.as_raw_fd(),
                NAME.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        };
        if set == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Gives `temp` the ACL of `original`.
    #[cfg(target_os = "macos")]
    pub(super) fn copy_acl(original: &File, temp: &File) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        // SAFETY: two open descriptors and no copy state.
        let copied = unsafe {
            libc::fcopyfile(
                original.as_raw_fd(),
                temp.as_raw_fd(),
                std::ptr::null_mut(),
                libc::COPYFILE_ACL,
            )
        };
        if copied == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// No ACL is carried over on this system.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(super) fn copy_acl(_original: &File, _temp: &File) -> io::Result<()> {
        Ok(())
    }
}

/// Windows DACL copying, adapted from Bake's
/// `packages/fs/fs-local/src/win32.ts` (`copyFileDaclWin32`) and its use in
/// `fsio.ts` `writeFileAtomic`. Publication uses the same rename as Unix.
#[cfg(windows)]
mod platform {
    use std::ffi::OsStr;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetFileSecurityW, SetFileSecurityW,
    };

    fn wide(path: &Path) -> Vec<u16> {
        OsStr::new(path).encode_wide().chain(Some(0)).collect()
    }

    /// A file's self-relative security descriptor holding its DACL, in a
    /// `u64` buffer, which is aligned for the descriptor's header.
    pub(super) fn read_dacl(path: &Path) -> io::Result<Vec<u64>> {
        let source = wide(path);
        let mut needed = 0u32;
        // SAFETY: a size query with no buffer; `source` is NUL-terminated.
        unsafe {
            GetFileSecurityW(
                source.as_ptr(),
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                0,
                &mut needed,
            )
        };
        if needed == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut descriptor = vec![0u64; usize::try_from(needed).unwrap_or(0).div_ceil(8)];
        let length = u32::try_from(descriptor.len() * 8).unwrap_or(u32::MAX);
        // SAFETY: `descriptor` holds `length` bytes.
        let read = unsafe {
            GetFileSecurityW(
                source.as_ptr(),
                DACL_SECURITY_INFORMATION,
                descriptor.as_mut_ptr().cast(),
                length,
                &mut needed,
            )
        };
        if read == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(descriptor)
    }

    /// Copies `source`'s DACL onto `destination`, which must still be
    /// empty. `SetFileSecurityW` sets the descriptor as read, inherited
    /// entries and control bits included.
    pub(super) fn copy_dacl(source: &Path, destination: &Path) -> io::Result<()> {
        let mut descriptor = read_dacl(source)?;
        let destination = wide(destination);
        // SAFETY: `descriptor` is the self-relative descriptor read above.
        let set = unsafe {
            SetFileSecurityW(
                destination.as_ptr(),
                DACL_SECURITY_INFORMATION,
                descriptor.as_mut_ptr().cast(),
            )
        };
        if set == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
