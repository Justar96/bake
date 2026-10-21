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
//! On drop the writer explicitly unlocks before closing its descriptor.
//! On Unix a child being started inherits the descriptor until `exec`
//! closes it; explicit unlocking releases the shared open file description
//! even while that child still has its copy. Taking a held lock waits for
//! up to [`LOCK_WAIT`] before it reports the file as held.
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
//! - A group of entries ([`append_lines`]) is written one line at a time,
//!   each written and synced before the next, from one open of the file. A
//!   failure stops the group there and reports the lines synced and what
//!   happened to the next one ([`FailedLine`]); a process killed meanwhile
//!   leaves the synced lines and possibly a torn prefix of the next.
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
//!   behind. A crash during that rewrite can leave the file truncated. The
//!   rewritten file keeps its permissions but, when renamed, takes the
//!   writer as its owner.
//! - After a file is created or renamed, its directory is synced on Unix,
//!   best effort as SQLite does: the data is already synced, and a
//!   filesystem that cannot sync a directory does not fail the write.
//!   Windows has no directory sync through `std`.
//! - On macOS, `std` syncs with `F_FULLFSYNC`; where a filesystem refuses it
//!   (`ENOTSUP` or `EINVAL`), a plain `fsync` is used.
//!
//! The crate's tests kill a writer process at each step of a rewrite through
//! a temporary file (`Faults::hold_rewrite`, a test-only seam) and, on
//! Linux, stop one inside the `write` of an append, of each line of a group,
//! and of a rewrite's temporary file with a file size limit, which either
//! kills it there (`SIGXFSZ`) or fails the write with `EFBIG`; `/dev/full`
//! fails appends with `ENOSPC`. They kill processes, not the machine, so
//! they do not test what a power loss leaves unsynced. The in-place rewrite
//! is not tested.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::session::SessionError;
use crate::session::id::uuid_v4;

#[cfg(test)]
pub(crate) mod disk_log;
#[cfg(test)]
use disk_log::DiskOp;

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
    lock: File,
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
            let held = Self {
                session_file: session_file.to_path_buf(),
                in_place,
                lock,
            };
            if still_at(&held.lock, &target)? {
                return Ok(Some(held));
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

impl Drop for WriterLock {
    fn drop(&mut self) {
        // A forked child's descriptor must not extend this owner's lock.
        let _ = self.lock.unlock();
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

/// [`sync`] `file`, which is at `path`; the crate's tests log it.
fn sync_file(file: &File, path: &Path, all: bool) -> io::Result<()> {
    sync(file, all)?;
    #[cfg(test)]
    disk_log::record(DiskOp::Fsync(path.to_path_buf()));
    #[cfg(not(test))]
    let _ = path;
    Ok(())
}

/// Write all of `bytes` to `file`, which is at `path`; the crate's tests
/// log the write.
fn write_file(file: &mut File, path: &Path, bytes: &[u8]) -> io::Result<()> {
    file.write_all(bytes)?;
    #[cfg(test)]
    disk_log::record(DiskOp::Write(path.to_path_buf(), bytes.to_vec()));
    #[cfg(not(test))]
    let _ = path;
    Ok(())
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
        if let Ok(file) = File::open(dir)
            && sync(&file, true).is_ok()
        {
            #[cfg(test)]
            disk_log::record(DiskOp::DirSync(dir.to_path_buf()));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Open `path` to append to it, creating it when missing; the crate's
/// tests log a creation.
fn open_append(path: &Path) -> io::Result<File> {
    #[cfg(test)]
    let created = !path.exists();
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)?;
    #[cfg(test)]
    if created {
        disk_log::record(DiskOp::Create(path.to_path_buf()));
    }
    Ok(file)
}

/// Append `bytes`, which end in a newline, on a line of their own, and sync
/// them. The file is created when missing, as Pi's `appendFileSync` does.
pub(crate) fn append(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = open_append(path)?;
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
    write_file(&mut file, path, &out)?;
    sync_file(&file, path, false)?;
    if len == 0 {
        sync_parent_dir(path);
    }
    Ok(())
}

/// What happened to the line a group append stopped at
/// ([`crate::session::SessionManager::append_group`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailedLine {
    /// No byte of it reached the file. A file that ended in a torn line may
    /// have gained the newline that sets that line apart.
    NotWritten,
    /// Some of its bytes may be in the file, without its newline: a torn
    /// last line. Loading skips it as Pi's loader does, unless all of the
    /// entry's JSON reached the file, when it reads as the entry.
    Torn,
    /// All of its bytes were written but syncing them failed: a reader may
    /// see the line now, and a crash may still lose it.
    Unsynced,
}

/// Why [`append_lines`] stopped: the lines before `synced` are written and
/// synced, line `synced` is in the state `line` says, and no later line was
/// written.
#[derive(Debug)]
pub(crate) struct LinesFailure {
    pub(crate) synced: usize,
    pub(crate) line: FailedLine,
    pub(crate) error: io::Error,
}

/// Faults the crate's unit tests inject into [`append_lines`] and
/// [`replace`]; empty, and never consulted, outside tests.
#[derive(Debug, Default)]
pub(crate) struct Faults {
    #[cfg(test)]
    next: Option<Fault>,
    #[cfg(test)]
    rewrite: Option<RewriteHold>,
    #[cfg(test)]
    in_place: Option<InPlaceHold>,
}

/// A step of a rewrite through a temporary file ([`replace`]) at which a
/// test can stop the process ([`Faults::hold_rewrite`]).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RewriteStep {
    /// The temporary file is created and empty.
    Created,
    /// The first half of the new contents is written to it, unsynced.
    PartlyWritten,
    /// All of the new contents are written to it, unsynced.
    Written,
    /// It is synced, with the file's permissions.
    Synced,
    /// It is renamed over the file; the directory is not synced.
    Renamed,
    /// The directory is synced: the rewrite is done.
    DirSynced,
}

#[cfg(test)]
impl RewriteStep {
    /// Every step, in the order a rewrite reaches them.
    pub(crate) const ALL: [Self; 6] = [
        Self::Created,
        Self::PartlyWritten,
        Self::Written,
        Self::Synced,
        Self::Renamed,
        Self::DirSynced,
    ];
}

/// A step of an in-place rewrite ([`replace`] of a file locked in place,
/// hard-linked, or behind a dangling link) at which a test can stop the
/// process ([`Faults::hold_in_place`]).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InPlaceStep {
    /// The file is opened, truncated to empty.
    Truncated,
    /// The first half of the new contents is written to it, unsynced.
    PartlyWritten,
    /// All of the new contents are written to it, unsynced.
    Written,
    /// It is synced: the rewrite is done.
    Synced,
}

#[cfg(test)]
impl InPlaceStep {
    /// Every step, in the order an in-place rewrite reaches them.
    pub(crate) const ALL: [Self; 4] = [
        Self::Truncated,
        Self::PartlyWritten,
        Self::Written,
        Self::Synced,
    ];
}

/// A callback the next in-place rewrite runs at its step.
#[cfg(test)]
struct InPlaceHold {
    step: InPlaceStep,
    hold: Box<dyn FnOnce() + Send>,
}

#[cfg(test)]
impl std::fmt::Debug for InPlaceHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InPlaceHold")
            .field("step", &self.step)
            .finish()
    }
}

/// A callback the next rewrite runs at its step.
#[cfg(test)]
struct RewriteHold {
    step: RewriteStep,
    hold: Box<dyn FnOnce() + Send>,
}

#[cfg(test)]
impl std::fmt::Debug for RewriteHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RewriteHold")
            .field("step", &self.step)
            .finish()
    }
}

/// A fault at one line of the next [`append_lines`] call.
#[cfg(test)]
pub(crate) struct Fault {
    /// The index of the line, within the call, it strikes.
    pub line: usize,
    pub kind: FaultKind,
}

#[cfg(test)]
impl std::fmt::Debug for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fault").field("line", &self.line).finish()
    }
}

/// What a [`Fault`] does at its line.
#[cfg(test)]
pub(crate) enum FaultKind {
    /// Fail before writing any of its bytes.
    Refuse,
    /// Write its first `n` bytes, then fail.
    Tear(usize),
    /// Write all of it, then fail the sync.
    FailSync,
    /// Write its first `n` bytes, then run the callback, which a test uses
    /// to stop the process there; the write fails if the callback returns.
    Hold(usize, Box<dyn FnOnce() + Send>),
}

impl Faults {
    /// Arm `fault` for the next [`append_lines`] call.
    #[cfg(test)]
    pub(crate) fn arm(&mut self, fault: Fault) {
        self.next = Some(fault);
    }

    /// The fault armed for line `line`, taken.
    #[cfg(test)]
    fn take(&mut self, line: usize) -> Option<FaultKind> {
        if self.next.as_ref().is_some_and(|fault| fault.line == line) {
            return self.next.take().map(|fault| fault.kind);
        }
        None
    }

    /// Run `hold` when the next rewrite through a temporary file reaches
    /// `step`; a test uses it to stop the process there. The rewrite fails
    /// if `hold` returns.
    #[cfg(test)]
    pub(crate) fn hold_rewrite(&mut self, step: RewriteStep, hold: Box<dyn FnOnce() + Send>) {
        self.rewrite = Some(RewriteHold { step, hold });
    }

    /// Whether the held rewrite stops at `step`.
    #[cfg(test)]
    fn holds_at(&self, step: RewriteStep) -> bool {
        self.rewrite.as_ref().is_some_and(|held| held.step == step)
    }

    /// The rewrite has reached `step`: run the callback held for it, and
    /// fail if that returns.
    #[cfg(test)]
    fn reach(&mut self, step: RewriteStep) -> io::Result<()> {
        if !self.holds_at(step) {
            return Ok(());
        }
        if let Some(held) = self.rewrite.take() {
            (held.hold)();
        }
        Err(io::Error::other("injected: held rewrite"))
    }

    /// Run `hold` when the next in-place rewrite reaches `step`; a test uses
    /// it to stop the process there. The rewrite fails if `hold` returns.
    #[cfg(test)]
    pub(crate) fn hold_in_place(&mut self, step: InPlaceStep, hold: Box<dyn FnOnce() + Send>) {
        self.in_place = Some(InPlaceHold { step, hold });
    }

    /// The in-place rewrite has reached `step`: run the callback held for
    /// it, and fail if that returns. With a hold at
    /// [`InPlaceStep::PartlyWritten`], `half` is written first.
    #[cfg(test)]
    fn reach_in_place(
        &mut self,
        step: InPlaceStep,
        file: &mut File,
        half: Option<&[u8]>,
    ) -> io::Result<()> {
        if self.in_place.as_ref().is_none_or(|held| held.step != step) {
            return Ok(());
        }
        if let Some(half) = half {
            file.write_all(half)?;
        }
        if let Some(held) = self.in_place.take() {
            (held.hold)();
        }
        Err(io::Error::other("injected: held in-place rewrite"))
    }

    /// With a hold at [`RewriteStep::PartlyWritten`], write the first half
    /// of `bytes` to the temporary file and hold there.
    #[cfg(test)]
    fn partly_write(&mut self, file: &mut File, bytes: &[u8]) -> io::Result<()> {
        if !self.holds_at(RewriteStep::PartlyWritten) {
            return Ok(());
        }
        file.write_all(bytes.get(..bytes.len() / 2).unwrap_or_default())?;
        self.reach(RewriteStep::PartlyWritten)
    }
}

/// Writes `bytes` to `file`, which is at `path`, as far as it can,
/// returning how many it wrote before an error; the crate's tests log what
/// it wrote.
fn write_counted(file: &mut File, path: &Path, bytes: &[u8]) -> Result<(), (usize, io::Error)> {
    let mut written = 0;
    let result = loop {
        if written == bytes.len() {
            break Ok(());
        }
        match file.write(&bytes[written..]) {
            Ok(0) => break Err((written, io::ErrorKind::WriteZero.into())),
            Ok(n) => written += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => break Err((written, error)),
        }
    };
    #[cfg(test)]
    if written > 0 {
        disk_log::record(DiskOp::Write(path.to_path_buf(), bytes[..written].to_vec()));
    }
    #[cfg(not(test))]
    let _ = path;
    result
}

/// Append `lines`, each ending in a newline, in order, the first on a line
/// of its own as [`append`] does, writing and syncing each line before the
/// next. On failure no later line is written, and [`LinesFailure`] says
/// which lines are durable. A process killed meanwhile leaves the lines it
/// synced and possibly a torn prefix of the next one.
pub(crate) fn append_lines(
    path: &Path,
    lines: &[Vec<u8>],
    faults: &mut Faults,
) -> Result<(), LinesFailure> {
    let failure = |synced, line, error| LinesFailure {
        synced,
        line,
        error,
    };
    let not_written = |error| failure(0, FailedLine::NotWritten, error);
    let mut file = open_append(path).map_err(not_written)?;
    let len = file.metadata().map_err(not_written)?.len();
    let mut separator = false;
    if len > 0 {
        file.seek(SeekFrom::End(-1)).map_err(not_written)?;
        let mut last = [0u8; 1];
        file.read_exact(&mut last).map_err(not_written)?;
        separator = last[0] != b'\n';
    }
    if separator {
        write_counted(&mut file, path, b"\n").map_err(|(_, error)| not_written(error))?;
    }
    #[cfg(not(test))]
    let _ = faults;
    for (index, line) in lines.iter().enumerate() {
        // The seam's own writes and syncs must succeed: a real failure of
        // one panics the test rather than pass for the injected stage.
        #[cfg(test)]
        let real_write = |file: &mut File, bytes: &[u8]| {
            if let Err((written, error)) = write_counted(file, path, bytes) {
                panic!("test seam: real write failed after {written} bytes: {error}");
            }
        };
        // How many bytes a tear leaves: never the newline, so a torn line
        // stays the last line, and none reports the line not written.
        #[cfg(test)]
        let torn = |n: usize| {
            let n = n.min(line.len().saturating_sub(1));
            let state = if n == 0 {
                FailedLine::NotWritten
            } else {
                FailedLine::Torn
            };
            (n, state)
        };
        #[cfg(test)]
        match faults.take(index) {
            None => {}
            Some(FaultKind::Refuse) => {
                return Err(failure(
                    index,
                    FailedLine::NotWritten,
                    io::Error::other("injected: line refused"),
                ));
            }
            Some(FaultKind::Tear(n)) => {
                let (n, state) = torn(n);
                real_write(&mut file, &line[..n]);
                return Err(failure(
                    index,
                    state,
                    io::Error::other("injected: line torn"),
                ));
            }
            Some(FaultKind::FailSync) => {
                // The whole line, newline included, and no sync.
                real_write(&mut file, line);
                return Err(failure(
                    index,
                    FailedLine::Unsynced,
                    io::Error::other("injected: sync failed"),
                ));
            }
            Some(FaultKind::Hold(n, hold)) => {
                let (n, state) = torn(n);
                real_write(&mut file, &line[..n]);
                if let Err(error) = sync(&file, false) {
                    panic!("test seam: real sync of the held prefix failed: {error}");
                }
                hold();
                return Err(failure(
                    index,
                    state,
                    io::Error::other("injected: held line"),
                ));
            }
        }
        write_counted(&mut file, path, line).map_err(|(written, error)| {
            let state = if written == 0 {
                FailedLine::NotWritten
            } else {
                FailedLine::Torn
            };
            failure(index, state, error)
        })?;
        sync_file(&file, path, false)
            .map_err(|error| failure(index, FailedLine::Unsynced, error))?;
        if len == 0 && index == 0 {
            sync_parent_dir(path);
        }
    }
    Ok(())
}

/// Create `path`, which must not exist, holding `bytes`, and sync it and
/// its directory entry.
pub(crate) fn create_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    #[cfg(test)]
    disk_log::record(DiskOp::Create(path.to_path_buf()));
    write_file(&mut file, path, bytes)?;
    sync_file(&file, path, false)?;
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
    /// `permissions` when given, and sync it. `faults` is consulted only by
    /// the crate's tests.
    pub(crate) fn write(
        target: &Path,
        bytes: &[u8],
        permissions: Option<fs::Permissions>,
        faults: &mut Faults,
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
        #[cfg(test)]
        {
            disk_log::record(DiskOp::Create(staged.temp.clone()));
            faults.reach(RewriteStep::Created)?;
            faults.partly_write(&mut file, bytes)?;
        }
        #[cfg(not(test))]
        let _ = faults;
        write_file(&mut file, &staged.temp, bytes)?;
        #[cfg(test)]
        faults.reach(RewriteStep::Written)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        sync_file(&file, &staged.temp, true)?;
        #[cfg(test)]
        faults.reach(RewriteStep::Synced)?;
        Ok(staged)
    }

    /// The temporary file.
    #[cfg(test)]
    pub(crate) fn temp(&self) -> &Path {
        &self.temp
    }

    /// Rename the temporary file over the target and sync the directory.
    /// `faults` is consulted only by the crate's tests.
    pub(crate) fn commit(mut self, faults: &mut Faults) -> io::Result<()> {
        fs::rename(&self.temp, &self.target)?;
        self.committed = true;
        #[cfg(test)]
        disk_log::record(DiskOp::Rename(self.temp.clone(), self.target.clone()));
        #[cfg(test)]
        faults.reach(RewriteStep::Renamed)?;
        #[cfg(not(test))]
        let _ = faults;
        sync_parent_dir(&self.target);
        #[cfg(test)]
        faults.reach(RewriteStep::DirSynced)?;
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.committed && fs::remove_file(&self.temp).is_ok() {
            #[cfg(test)]
            disk_log::record(DiskOp::Remove(self.temp.clone()));
        }
    }
}

/// Truncate `path` and write `bytes` in place, then sync it. `faults` is
/// consulted only by the crate's tests.
fn overwrite(path: &Path, bytes: &[u8], faults: &mut Faults) -> io::Result<()> {
    #[cfg(test)]
    let created = !path.exists();
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    #[cfg(test)]
    disk_log::record(if created {
        DiskOp::Create(path.to_path_buf())
    } else {
        DiskOp::Truncate(path.to_path_buf())
    });
    #[cfg(test)]
    {
        faults.reach_in_place(InPlaceStep::Truncated, &mut file, None)?;
        let half = bytes.get(..bytes.len() / 2).unwrap_or_default();
        faults.reach_in_place(InPlaceStep::PartlyWritten, &mut file, Some(half))?;
    }
    #[cfg(not(test))]
    let _ = faults;
    write_file(&mut file, path, bytes)?;
    #[cfg(test)]
    faults.reach_in_place(InPlaceStep::Written, &mut file, None)?;
    sync_file(&file, path, false)?;
    #[cfg(test)]
    faults.reach_in_place(InPlaceStep::Synced, &mut file, None)?;
    Ok(())
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
/// (module docs). `faults` is consulted only by the crate's tests.
pub(crate) fn replace(
    path: &Path,
    bytes: &[u8],
    in_place: bool,
    faults: &mut Faults,
) -> io::Result<()> {
    if in_place {
        return overwrite(path, bytes, faults);
    }
    let is_link = fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_symlink());
    let target = if is_link {
        match fs::canonicalize(path) {
            Ok(target) => target,
            Err(_) => return overwrite(path, bytes, faults),
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
        return overwrite(&target, bytes, faults);
    }
    let permissions = existing.map(|metadata| metadata.permissions());
    Staged::write(&target, bytes, permissions, faults)?.commit(faults)
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

    #[cfg(unix)]
    #[test]
    fn dropping_a_lock_releases_inherited_descriptors() {
        let dir = Dir::new("inherited-lock");
        let file = dir.0.join("s.jsonl");
        let first = WriterLock::acquire(&file).expect("first");
        // A duplicate shares the open file description, as a child does
        // between fork and exec, without depending on process timing.
        let inherited = first.lock.try_clone().expect("copy descriptor");
        assert!(WriterLock::try_acquire(&file).expect("try held").is_none());

        drop(first);
        let second = WriterLock::try_acquire(&file)
            .expect("try released")
            .expect("the inherited descriptor must not retain the writer lock");
        assert!(inherited.metadata().is_ok());
        drop(inherited);
        assert!(
            WriterLock::try_acquire(&file)
                .expect("second held")
                .is_none()
        );
        drop(second);
        assert!(
            WriterLock::try_acquire(&file)
                .expect("second released")
                .is_some()
        );
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
        assert!(!still_at(&first.lock, &lock_path(&file)).expect("stat"));
        assert!(still_at(&second.lock, &lock_path(&file)).expect("stat"));
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
        replace(
            &without,
            b"rewritten\n",
            in_place.in_place(),
            &mut Faults::default(),
        )
        .expect("in place");
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
        let staged = Staged::write(&file, b"new\n", None, &mut Faults::default()).expect("stage");
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
        replace(&file, b"new\n", false, &mut Faults::default()).expect("replace");
        assert_eq!(fs::read(&file).expect("read"), b"new\n");
        assert_eq!(names(&dir.0), ["s.jsonl"]);
    }

    /// Each step of the rewrite seam is where its name says: the target and
    /// the temporary file hold there what a crash at that point leaves. A
    /// hold that returns fails the rewrite, which removes a temporary file
    /// not yet renamed, and the next rewrite holds nowhere.
    #[test]
    fn a_held_rewrite_stops_at_each_step() {
        use std::sync::{Arc, Mutex};
        type Seen = Arc<Mutex<Option<(Vec<u8>, Vec<Vec<u8>>)>>>;
        let new: &[u8] = b"{\"new\":\"contents\"}\n";
        for step in RewriteStep::ALL {
            let dir = Dir::new("held-rewrite");
            let file = dir.0.join("s.jsonl");
            fs::write(&file, b"old\n").expect("write");
            let seen: Seen = Arc::default();
            let mut faults = Faults::default();
            let (record, probe, target) = (Arc::clone(&seen), dir.0.clone(), file.clone());
            faults.hold_rewrite(
                step,
                Box::new(move || {
                    let temps = names(&probe)
                        .into_iter()
                        .filter(|name| name.ends_with(".tmp"))
                        .map(|name| fs::read(probe.join(name)).expect("temp"))
                        .collect();
                    let now = (fs::read(&target).expect("target"), temps);
                    *record.lock().expect("record") = Some(now);
                }),
            );
            let error = replace(&file, new, false, &mut faults).expect_err("held");
            assert_eq!(error.to_string(), "injected: held rewrite", "{step:?}");
            let (target, temps) = seen.lock().expect("seen").take().expect("the hold ran");
            let renamed = matches!(step, RewriteStep::Renamed | RewriteStep::DirSynced);
            let staged = match step {
                RewriteStep::Created => &new[..0],
                RewriteStep::PartlyWritten => &new[..new.len() / 2],
                _ => new,
            };
            if renamed {
                assert_eq!((target.as_slice(), temps.len()), (new, 0), "{step:?}");
            } else {
                assert_eq!(target, b"old\n", "{step:?}");
                assert_eq!(temps, [staged], "{step:?}");
            }
            assert_eq!(names(&dir.0), ["s.jsonl"], "{step:?}");
            let kept: &[u8] = if renamed { new } else { b"old\n" };
            assert_eq!(fs::read(&file).expect("read"), kept, "{step:?}");
            replace(&file, b"again\n", false, &mut faults).expect("next rewrite");
            assert_eq!(fs::read(&file).expect("read"), b"again\n");
        }
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

        replace(&real, b"one\n", false, &mut Faults::default()).expect("replace");
        assert_eq!(mode(&real), 0o600);

        let link = dir.0.join("link.jsonl");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        replace(&link, b"two\n", false, &mut Faults::default()).expect("through link");
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
        replace(&real, b"three\n", false, &mut Faults::default()).expect("hard-linked");
        assert_eq!(fs::metadata(&real).expect("stat").ino(), inode);
        assert_eq!(fs::read(&hard).expect("read"), b"three\n");
        assert_eq!(names(&dir.0), ["hard.jsonl", "link.jsonl", "real.jsonl"]);
    }
}
