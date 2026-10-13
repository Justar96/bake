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
//! Both take a lock through a read-only handle as well.
//!
//! When the sidecar cannot be created, because the directory is read-only or
//! another user owns it, an existing sidecar is opened read-only and locked.
//! When there is none, on Unix, the session file itself is locked instead.
//! That is sound because a rewrite cannot rename in such a directory either:
//! a writer that locks the session file rewrites it in place ([`replace`]).
//! Windows has no such fallback, since its mandatory lock would also refuse
//! the writer's own appends through another handle; the writer fails with
//! the sidecar's error there.
//!
//! After locking, the writer checks on Unix that the file it locked is still
//! the one at its path (device and inode), and starts over when it is not, so
//! a sidecar deleted and recreated meanwhile cannot leave two holders. On
//! Windows a file opened by `std` may be deleted while open; a sidecar
//! deleted while held is not detected there. Bake never deletes a sidecar.
//!
//! A lock on an open file is also held by every copy of its descriptor. On
//! Unix, a process starting a child copies all of its descriptors into the
//! child until the child's `exec` closes them (every `std` descriptor is
//! close-on-exec), so a lock this process has just released can stay held
//! for that moment. Taking the lock therefore retries for up to
//! [`LOCK_WAIT`] before it reports the file as held.
//!
//! On NFS and CIFS, Linux emulates `flock` with POSIX record locks, which
//! belong to the process rather than the open file: two writers in one
//! process are not excluded there, and closing any descriptor of the lock
//! file in the process releases its lock. Writers in different processes
//! are still excluded. Readers never open the lock file, so a reader in the
//! writer's process cannot release it.
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
//! - A new file is created exclusively, written, and synced.
//! - A rewrite (migration, an empty file's first header, a branched
//!   session) writes a temporary file beside the real file (a symbolic link
//!   is followed to it), gives it the file's permissions, syncs it, and
//!   renames it over the file. A crash leaves either the old file or the
//!   new one, never a truncated file. The temporary file's name starts with
//!   `.` and ends in `.tmp`, so one a crash leaves behind is never listed as
//!   a session. A file with other hard links, a dangling link, or a file
//!   locked in place is instead truncated and rewritten in place, as Pi
//!   does, and synced: renaming would split the links or leave the target
//!   behind. The rewritten file keeps its permissions but, when renamed,
//!   takes the writer as its owner.
//! - After a file is created or renamed, its directory is synced on Unix,
//!   best effort as SQLite does: the data is already synced, and a
//!   filesystem that cannot sync a directory does not fail the write.
//!   Windows has no directory sync through `std`.
//! - On macOS, `std` syncs with `F_FULLFSYNC`; where a filesystem refuses it
//!   (`ENOTSUP` or `EINVAL`), a plain `fsync` is used.

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

/// How often [`WriterLock::try_acquire`] starts over when the file it
/// locked was replaced meanwhile.
const LOCK_ATTEMPTS: usize = 16;

/// An exclusive hold on one session file, released on drop.
#[derive(Debug)]
pub(crate) struct WriterLock {
    session_file: PathBuf,
    /// Whether the session file itself is locked, for want of a sidecar.
    in_place: bool,
    _lock: File,
}

fn cannot_create(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
    )
}

/// Whether `file` is still the file at `path`. Only Unix can tell.
fn still_at(file: &File, path: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let held = file.metadata()?;
        match fs::metadata(path) {
            Ok(current) => Ok(held.dev() == current.dev() && held.ino() == current.ino()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (file, path);
        Ok(true)
    }
}

impl WriterLock {
    /// Take the writer lock of `session_file`, or fail with
    /// [`SessionError::Locked`] when another writer still holds it after
    /// [`LOCK_WAIT`]. Other failures name the session file.
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
                Err(error) => {
                    let source = io::Error::new(
                        error.kind(),
                        format!(
                            "cannot take the writer lock {}: {error}",
                            lock_path(session_file).display()
                        ),
                    );
                    return Err(SessionError::io(session_file, source));
                }
            }
        }
    }

    /// The file to lock: the sidecar, created when missing, else an
    /// existing sidecar read-only, else the session file.
    fn open_target(session_file: &Path) -> io::Result<(File, PathBuf, bool)> {
        let sidecar = lock_path(session_file);
        let error = match OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&sidecar)
        {
            Ok(file) => return Ok((file, sidecar, false)),
            Err(error) if cannot_create(&error) => error,
            Err(error) => return Err(error),
        };
        match File::open(&sidecar) {
            Ok(file) => Ok((file, sidecar, false)),
            #[cfg(unix)]
            Err(missing) if missing.kind() == io::ErrorKind::NotFound => {
                match File::open(session_file) {
                    Ok(file) => Ok((file, session_file.to_path_buf(), true)),
                    // Neither a sidecar nor a session file to lock: the
                    // sidecar's own failure says why.
                    Err(_) => Err(error),
                }
            }
            Err(_) => Err(error),
        }
    }

    /// Take the writer lock of `session_file`; `None` while another writer
    /// holds it.
    pub(crate) fn try_acquire(session_file: &Path) -> io::Result<Option<Self>> {
        for _ in 0..LOCK_ATTEMPTS {
            let (lock, target, in_place) = Self::open_target(session_file)?;
            match lock.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => return Ok(None),
                Err(TryLockError::Error(error)) => return Err(error),
            }
            if still_at(&lock, &target)? {
                return Ok(Some(Self {
                    session_file: session_file.to_path_buf(),
                    in_place,
                    _lock: lock,
                }));
            }
        }
        Err(io::Error::other(
            "the lock file kept being replaced while it was locked",
        ))
    }

    /// The session file this lock holds.
    pub(crate) fn session_file(&self) -> &Path {
        &self.session_file
    }

    /// Whether the session file itself is locked, so rewrites happen in
    /// place.
    pub(crate) fn in_place(&self) -> bool {
        self.in_place
    }
}

/// Sync a file's data, or with `all` its metadata too.
fn sync(file: &File, all: bool) -> io::Result<()> {
    let result = if all {
        file.sync_all()
    } else {
        file.sync_data()
    };
    #[cfg(target_vendor = "apple")]
    if let Err(error) = &result
        && matches!(error.raw_os_error(), Some(libc::ENOTSUP | libc::EINVAL))
    {
        use std::os::fd::AsRawFd;
        // SAFETY: `fsync` reads only the descriptor, which `file` owns and
        // keeps open for the call.
        return if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        };
    }
    result
}

/// Sync `path`'s directory so a name created or renamed there survives a
/// crash; best effort, and nothing on Windows.
fn sync_parent_dir(path: &Path) {
    #[cfg(unix)]
    {
        let dir = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };
        if let Ok(dir) = File::open(dir) {
            let _ = sync(&dir, true);
        }
    }
    #[cfg(not(unix))]
    let _ = path;
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
    sync(&file, false)?;
    if len == 0 {
        sync_parent_dir(path);
    }
    Ok(())
}

/// Create `path`, which must not exist, holding `bytes`, and sync it and
/// its directory entry.
pub(crate) fn create_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    sync(&file, false)?;
    drop(file);
    sync_parent_dir(path);
    Ok(())
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
    /// Write `bytes` to a new temporary file beside `target`, with
    /// `permissions` when given, and sync it.
    pub(crate) fn write(
        target: &Path,
        bytes: &[u8],
        permissions: Option<fs::Permissions>,
    ) -> io::Result<Self> {
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
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        sync(&file, true)?;
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
        sync_parent_dir(&self.target);
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.temp);
        }
    }
}

/// Truncate `path` and write `bytes` in place, then sync it.
fn overwrite(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    file.write_all(bytes)?;
    sync(&file, false)
}

/// Whether other names link to the file.
fn has_other_links(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.nlink() > 1
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

/// Replace `path`'s contents with `bytes`. Through a temporary file renamed
/// over the real file, the file holds either its old contents or all of
/// `bytes` whenever the process stops; with `in_place`, or for a file with
/// other hard links or a dangling link, it is rewritten in place
/// (module docs).
pub(crate) fn replace(path: &Path, bytes: &[u8], in_place: bool) -> io::Result<()> {
    if in_place {
        return overwrite(path, bytes);
    }
    let is_link = fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_symlink());
    let target = if is_link {
        match fs::canonicalize(path) {
            Ok(target) => target,
            Err(_) => return overwrite(path, bytes),
        }
    } else {
        path.to_path_buf()
    };
    let existing = match fs::metadata(&target) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if existing.as_ref().is_some_and(has_other_links) {
        return overwrite(&target, bytes);
    }
    let permissions = existing.map(|metadata| metadata.permissions());
    Staged::write(&target, bytes, permissions)?.commit()
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
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
            }
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
        assert!(!first.in_place());
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

    /// A sidecar replaced while it was being locked is locked again at its
    /// path, so a deleted sidecar cannot leave two holders.
    #[cfg(unix)]
    #[test]
    fn a_replaced_sidecar_is_not_held() {
        let dir = Dir::new("replaced");
        let file = dir.0.join("s.jsonl");
        let first = WriterLock::acquire(&file).expect("first");
        fs::remove_file(lock_path(&file)).expect("unlink");
        let second = WriterLock::acquire(&file).expect("new sidecar");
        assert!(!still_at(&first._lock, &lock_path(&file)).expect("stat"));
        assert!(still_at(&second._lock, &lock_path(&file)).expect("stat"));
    }

    /// In a read-only directory an existing sidecar is locked read-only, and
    /// without one the session file is locked and rewritten in place.
    #[cfg(unix)]
    #[test]
    fn a_read_only_directory_falls_back() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Dir::new("read-only");
        let with_sidecar = dir.0.join("a.jsonl");
        let without = dir.0.join("b.jsonl");
        fs::write(&with_sidecar, b"a\n").expect("write");
        fs::write(&without, b"b\n").expect("write");
        drop(WriterLock::acquire(&with_sidecar).expect("sidecar"));
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o555)).expect("chmod");
        if OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.0.join("probe"))
            .is_ok()
        {
            // Running as root: directory permissions do not apply.
            return;
        }
        let held = WriterLock::acquire(&with_sidecar).expect("read-only sidecar");
        assert!(!held.in_place());
        assert!(matches!(
            WriterLock::acquire(&with_sidecar),
            Err(SessionError::Locked(_))
        ));
        let in_place = WriterLock::acquire(&without).expect("session file");
        assert!(in_place.in_place());
        assert!(matches!(
            WriterLock::acquire(&without),
            Err(SessionError::Locked(_))
        ));
        replace(&without, b"rewritten\n", in_place.in_place()).expect("in place");
        assert_eq!(fs::read(&without).expect("read"), b"rewritten\n");
        // A missing session in such a directory fails, naming the session.
        let missing = dir.0.join("c.jsonl");
        match WriterLock::acquire(&missing) {
            Err(SessionError::Io { path, source }) => {
                assert_eq!(path, missing);
                assert!(source.to_string().contains("c.jsonl.lock"), "{source}");
            }
            other => panic!("expected an I/O error, got {other:?}"),
        }
        assert_eq!(names(&dir.0), ["a.jsonl", "a.jsonl.lock", "b.jsonl"]);
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
        let staged = Staged::write(&file, b"new\n", None).expect("stage");
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
        replace(&file, b"new\n", false).expect("replace");
        assert_eq!(fs::read(&file).expect("read"), b"new\n");
        assert_eq!(names(&dir.0), ["s.jsonl"]);
    }

    /// A rewrite keeps the file's mode, follows a symbolic link to the real
    /// file, and rewrites a hard-linked file in place.
    #[cfg(unix)]
    #[test]
    fn rewrites_keep_mode_links_and_targets() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = Dir::new("rewrite-links");
        let real = dir.0.join("real.jsonl");
        fs::write(&real, b"old\n").expect("write");
        fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).expect("chmod");
        let mode = |path: &Path| fs::metadata(path).expect("stat").permissions().mode() & 0o777;

        replace(&real, b"one\n", false).expect("replace");
        assert_eq!(mode(&real), 0o600);

        let link = dir.0.join("link.jsonl");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        replace(&link, b"two\n", false).expect("through link");
        assert!(
            fs::symlink_metadata(&link)
                .expect("lstat")
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&real).expect("read"), b"two\n");
        assert_eq!(mode(&real), 0o600);

        let hard = dir.0.join("hard.jsonl");
        fs::hard_link(&real, &hard).expect("hard link");
        let inode = fs::metadata(&real).expect("stat").ino();
        replace(&real, b"three\n", false).expect("hard-linked");
        assert_eq!(fs::metadata(&real).expect("stat").ino(), inode);
        assert_eq!(fs::read(&hard).expect("read"), b"three\n");
        assert_eq!(names(&dir.0), ["hard.jsonl", "link.jsonl", "real.jsonl"]);
    }
}
