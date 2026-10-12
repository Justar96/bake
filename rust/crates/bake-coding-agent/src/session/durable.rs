//! Writer exclusion and durable writes for session files. Pi has neither;
//! both are Bake design rules (scope 03 of the Rust 0.4 roadmap).
//!
//! # Writer exclusion
//!
//! A writer holds an exclusive [`File::try_lock`] on a sidecar lock file,
//! the session file's name with `.lock` appended (see [`lock_path`]), never
//! on the session file itself: Windows locks are mandatory, so a lock on the
//! `.jsonl` would block the readers that list and display sessions, and a
//! rewrite replaces the session file by renaming over it, which would leave a
//! lock on the old file behind. The lock is `flock` on Linux and macOS and
//! `LockFileEx` on Windows; both belong to the open file, so a second writer
//! in the same process is refused as one in another process is, and the
//! operating system releases the lock when its holder exits or is killed.
//!
//! A lock on an open file is also held by every copy of its descriptor. On
//! Unix, a process starting a child copies all of its descriptors into the
//! child until the child's `exec` closes them (every `std` descriptor is
//! close-on-exec), so a lock this process has just released can stay held
//! for that moment. Taking the lock therefore retries for up to
//! [`LOCK_WAIT`] before it reports the file as held.
//!
//! The sidecar is left in place when the lock is released. Removing it would
//! race: a second writer that opened the sidecar before the unlink would
//! lock the removed file while a third writer locked a new one, and both
//! would hold the session. A sidecar's name does not end in `.jsonl`, so
//! session listing and discovery never read it.
//!
//! # Durable writes
//!
//! - An append writes the whole entry with one `write_all` and then
//!   `sync_data`. When the file does not end in a newline, as after a writer
//!   was killed mid-append, the entry is preceded by one, so a torn line
//!   stays an unparseable line of its own and the new entry is read.
//! - A new file is created exclusively, written, synced, and on Unix its
//!   directory is synced so the name survives a crash.
//! - A rewrite (migration, an empty file's first header, a branched
//!   session) writes a temporary file in the same directory, syncs it, and
//!   renames it over the session file, then syncs the directory on Unix. A
//!   crash leaves either the old file or the new one, never a truncated
//!   file. The temporary file's name starts with `.` and ends in `.tmp`, so
//!   one a crash leaves behind is never listed as a session.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::session::SessionError;
use crate::session::id::uuid_v4;

/// The sidecar lock file of `session_file`: its name with `.lock` appended.
pub(crate) fn lock_path(session_file: &Path) -> PathBuf {
    let mut name = OsString::from(session_file.as_os_str());
    name.push(".lock");
    PathBuf::from(name)
}

/// How long [`WriterLock::acquire`] waits for a held lock before it fails.
pub(crate) const LOCK_WAIT: Duration = Duration::from_secs(1);

/// An exclusive hold on one session file, released on drop.
#[derive(Debug)]
pub(crate) struct WriterLock {
    session_file: PathBuf,
    _lock: File,
}

impl WriterLock {
    /// Take the writer lock of `session_file`, or fail with
    /// [`SessionError::Locked`] when another writer still holds it after
    /// [`LOCK_WAIT`].
    pub(crate) fn acquire(session_file: &Path) -> Result<Self, SessionError> {
        let deadline = Instant::now() + LOCK_WAIT;
        let mut pause = Duration::from_millis(1);
        loop {
            match Self::try_acquire(session_file) {
                Ok(Some(lock)) => return Ok(lock),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(pause);
                    pause = (pause * 2).min(Duration::from_millis(50));
                }
                Ok(None) => return Err(SessionError::Locked(session_file.to_path_buf())),
                Err(error) => return Err(SessionError::io(lock_path(session_file), error)),
            }
        }
    }

    /// Take the writer lock of `session_file`; `None` while another writer
    /// holds it.
    pub(crate) fn try_acquire(session_file: &Path) -> io::Result<Option<Self>> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path(session_file))?;
        match lock.try_lock() {
            Ok(()) => Ok(Some(Self {
                session_file: session_file.to_path_buf(),
                _lock: lock,
            })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }

    /// The session file this lock holds.
    pub(crate) fn session_file(&self) -> &Path {
        &self.session_file
    }
}

/// Sync `path`'s directory so a name created or renamed there survives a
/// crash. Windows has no directory sync through `std`; its metadata
/// journaling covers renames.
fn sync_parent_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let dir = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };
        File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Append `bytes`, which end in a newline, on a line of their own, and sync
/// them. The file is created when missing, as Pi's `appendFileSync` does.
pub(crate) fn append(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)?;
    let len = file.metadata()?.len();
    let mut out = Vec::with_capacity(bytes.len() + 1);
    if len > 0 {
        file.seek(SeekFrom::End(-1))?;
        let mut last = [0u8; 1];
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            out.push(b'\n');
        }
    }
    out.extend_from_slice(bytes);
    file.write_all(&out)?;
    file.sync_data()?;
    if len == 0 {
        sync_parent_dir(path)?;
    }
    Ok(())
}

/// Create `path`, which must not exist, holding `bytes`, and sync it and
/// its directory entry.
pub(crate) fn create_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_data()?;
    drop(file);
    sync_parent_dir(path)
}

/// New contents for a file, written and synced beside it under a temporary
/// name; [`Staged::commit`] renames it over the file. Dropping it without a
/// commit removes the temporary file and leaves the file as it was.
#[derive(Debug)]
pub(crate) struct Staged {
    temp: PathBuf,
    target: PathBuf,
    committed: bool,
}

impl Staged {
    /// Write `bytes` to a new temporary file beside `target` and sync it.
    pub(crate) fn write(target: &Path, bytes: &[u8]) -> io::Result<Self> {
        let dir = target.parent().unwrap_or_else(|| Path::new(""));
        let name = target
            .file_name()
            .map_or_else(|| "session".into(), |name| name.to_string_lossy());
        let temp = dir.join(format!(".{name}.{}.tmp", uuid_v4()));
        let staged = Self {
            temp,
            target: target.to_path_buf(),
            committed: false,
        };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged.temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(staged)
    }

    /// The temporary file.
    #[cfg(test)]
    pub(crate) fn temp(&self) -> &Path {
        &self.temp
    }

    /// Rename the temporary file over the target and sync the directory.
    pub(crate) fn commit(mut self) -> io::Result<()> {
        fs::rename(&self.temp, &self.target)?;
        self.committed = true;
        sync_parent_dir(&self.target)
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.temp);
        }
    }
}

/// Replace `path`'s contents with `bytes` atomically: the file holds either
/// its old contents or all of `bytes`, whenever the process stops.
pub(crate) fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    Staged::write(path, bytes)?.commit()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "bake-durable-{name}-{}-{}",
                std::process::id(),
                uuid_v4()
            ));
            fs::create_dir_all(&dir).expect("temp dir");
            Self(dir)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| entry.expect("entry").file_name().to_string_lossy().into())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn lock_files_sit_beside_the_session() {
        assert_eq!(
            lock_path(Path::new("/s/a_b.jsonl")),
            PathBuf::from("/s/a_b.jsonl.lock")
        );
    }

    #[test]
    fn a_second_lock_is_refused_until_the_first_drops() {
        let dir = Dir::new("lock");
        let file = dir.0.join("s.jsonl");
        let first = WriterLock::acquire(&file).expect("first");
        assert!(matches!(
            WriterLock::acquire(&file),
            Err(SessionError::Locked(path)) if path == file
        ));
        drop(first);
        let second = WriterLock::acquire(&file).expect("taken over");
        assert_eq!(second.session_file(), file);
        // The sidecar stays; the session file was never created.
        assert_eq!(names(&dir.0), ["s.jsonl.lock"]);
    }

    #[test]
    fn appends_start_on_a_line_of_their_own() {
        let dir = Dir::new("append");
        let file = dir.0.join("s.jsonl");
        append(&file, b"{\"a\":1}\n").expect("create");
        fs::write(&file, b"{\"a\":1}\n{\"torn\":").expect("tear");
        append(&file, b"{\"b\":2}\n").expect("append");
        assert_eq!(
            fs::read(&file).expect("read"),
            b"{\"a\":1}\n{\"torn\":\n{\"b\":2}\n"
        );
        append(&file, b"{\"c\":3}\n").expect("append");
        assert!(
            fs::read_to_string(&file)
                .expect("read")
                .ends_with("{\"b\":2}\n{\"c\":3}\n")
        );
    }

    #[test]
    fn an_uncommitted_rewrite_leaves_the_file_as_it_was() {
        let dir = Dir::new("stage");
        let file = dir.0.join("s.jsonl");
        fs::write(&file, b"old\n").expect("write");
        let staged = Staged::write(&file, b"new\n").expect("stage");
        let temp = staged.temp().to_path_buf();
        let name = temp
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with(".s.jsonl.") && name.ends_with(".tmp"),
            "{name}"
        );
        assert_eq!(fs::read(&file).expect("read"), b"old\n");
        drop(staged);
        assert!(!temp.exists());
        assert_eq!(names(&dir.0), ["s.jsonl"]);
        replace(&file, b"new\n").expect("replace");
        assert_eq!(fs::read(&file).expect("read"), b"new\n");
        assert_eq!(names(&dir.0), ["s.jsonl"]);
    }
}
