//! The I/O seam of the development-only Session writer: every filesystem
//! mutation [`crate::PlainLogFile`] and its write lock make, every sync that
//! makes one durable, and every read a write `open` and its recovery depend
//! on, go through one [`StorageIo`].
//!
//! Production uses [`RealIo`], the real filesystem, and the public
//! constructors of [`crate::PlainLogFile`] use it alone. The trait exists so
//! storage tests can inject a deterministic fault at a chosen operation (D30):
//! a disk-full, permission, or I/O error, a torn write that stores only a
//! prefix of its bytes, a crash after which every operation fails, so no
//! cleanup reaches the disk, or a power cut, after which only what a sync
//! made durable remains. It is hidden from the documented API and is not a
//! stable interface.
//!
//! The operations, one [`StorageOp`] each:
//!
//! - Namespace mutations: [`StorageIo::create_dir_all`] (mode `0o700` on
//!   Unix), [`StorageIo::create_new`] (a new file for writing, mode `0o600`
//!   on Unix), [`StorageIo::hard_link`], [`StorageIo::rename_new`], and
//!   [`StorageIo::remove_file`].
//! - Content mutations of an open file: [`StorageIo::open_write`],
//!   [`StorageIo::write_at`], which a fault may tear after any byte, and
//!   [`StorageIo::set_len`].
//! - Syncs: [`StorageIo::sync_file`], Node's `FileHandle.sync()`, which
//!   makes a file's bytes durable, and [`StorageIo::sync_dir`], which makes a
//!   directory's entries durable on POSIX.
//! - The write lock: [`StorageIo::open_lock`], which opens and, on Unix,
//!   truncates the lock file, [`StorageIo::try_lock`], and
//!   [`StorageIo::lock_is_current`], which reads whether the locked file is
//!   still the one at its path.
//! - Reads recovery depends on: [`StorageIo::read`] of a whole log,
//!   [`StorageIo::read_dir`] of a root, project, or Session directory,
//!   [`StorageIo::canonicalize`] for the stored-identity check,
//!   [`StorageIo::probe`] for the flat-layout probe, and
//!   [`StorageIo::stat_dir`] for the Win32 directory walk.
//!
//! Closing a file and releasing a lock are not operations: they are what the
//! kernel does when a process dies, so a crashed handle's files may still be
//! dropped.
//!
//! [`StorageIo::write_platform`] names the platform whose write sequence the
//! writer issues, the host's for [`RealIo`]: POSIX syncs the parent of each
//! directory it creates and the directory it links a log into, and
//! publishes with a hard link, as TypeScript's `materializePosix` and
//! `publishCurrentExclusive` do, while Win32 syncs no directory and
//! publishes with a write-through move, as `materializeWin32` and
//! `publishNewFileWin32` do. A faulting implementation may name the other
//! platform, so one host sweeps both sequences; [`RealIo::rename_new`] off
//! Windows stands in for the move only for such a test.

use std::ffi::OsString;
use std::fmt::Debug;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, ErrorKind, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::PathPlatform;

/// The kind of one [`StorageIo`] operation, in the module's taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StorageOp {
    CreateDir,
    CreateNew,
    OpenWrite,
    WriteAt,
    SetLen,
    HardLink,
    RenameNew,
    RemoveFile,
    SyncFile,
    SyncDir,
    OpenLock,
    TryLock,
    LockIsCurrent,
    Read,
    ReadDir,
    Canonicalize,
    Probe,
    StatDir,
}

/// One listed directory entry, symbolic links not followed.
#[derive(Debug)]
pub struct ListedEntry {
    pub name: OsString,
    pub is_dir: bool,
    pub is_file: bool,
}

/// Why [`StorageIo::try_lock`] took no lock.
#[derive(Debug)]
pub enum LockFailure {
    /// Another handle holds the lock.
    WouldBlock,
    Io(io::Error),
}

/// The filesystem operations of the Session writer; see the module comment.
/// Each method performs exactly one operation of its [`StorageOp`].
pub trait StorageIo: Debug + Send + Sync {
    /// Create `dir` and its missing parents.
    fn create_dir_all(&self, dir: &Path) -> io::Result<()>;
    /// Create a new file at `path` for writing; fail if one exists.
    fn create_new(&self, path: &Path) -> io::Result<File>;
    /// Open the existing file at `path` for writing.
    fn open_write(&self, path: &Path) -> io::Result<File>;
    /// Write all of `bytes` at `offset`.
    fn write_at(&self, file: &mut File, offset: u64, bytes: &[u8]) -> io::Result<()>;
    /// Truncate or extend `file` to `len` bytes.
    fn set_len(&self, file: &File, len: u64) -> io::Result<()>;
    /// Link `original` at `link`; fail if `link` exists.
    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()>;
    /// Move the file at `from` to `to` durably, as `MoveFileExW` with
    /// `MOVEFILE_WRITE_THROUGH` and without `MOVEFILE_REPLACE_EXISTING`
    /// does; fail with [`ErrorKind::AlreadyExists`] if `to` exists.
    fn rename_new(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Remove the file at `path`.
    fn remove_file(&self, path: &Path) -> io::Result<()>;
    /// Make `file`'s bytes durable, as Node's `FileHandle.sync()` (`fsync`,
    /// or `FlushFileBuffers` on Windows) does. `path` names the file, for an
    /// implementation that models durability by path.
    fn sync_file(&self, file: &File, path: &Path) -> io::Result<()>;
    /// Make the entries of directory `dir` durable, as TypeScript's
    /// `syncDirPosix` does by opening it read-only and syncing it. Only the
    /// POSIX write sequence syncs a directory.
    fn sync_dir(&self, dir: &Path) -> io::Result<()>;
    /// Open the lock file at `path`, created when absent.
    fn open_lock(&self, path: &Path) -> io::Result<File>;
    /// Take `file`'s exclusive lock without blocking.
    fn try_lock(&self, file: &File) -> Result<(), LockFailure>;
    /// Whether the file at `path` is still `held`.
    fn lock_is_current(&self, held: &File, path: &Path) -> io::Result<bool>;
    /// Read the whole file at `path`.
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// List `dir`, in the filesystem's order; `None` when `dir` is absent.
    fn read_dir(&self, dir: &Path) -> io::Result<Option<Vec<ListedEntry>>>;
    /// `path` resolved through every symbolic link; `None` when absent.
    fn canonicalize(&self, path: &Path) -> io::Result<Option<PathBuf>>;
    /// `exists(path)`: whether `open(path, 'r')` succeeds, following a
    /// symbolic link; only an absent path is false. libuv opens with
    /// `O_RDONLY` on POSIX and with `FILE_FLAG_BACKUP_SEMANTICS` on Windows
    /// (`fs__open` in `src/win/fs.c`), so a directory exists on both.
    fn probe(&self, path: &Path) -> io::Result<bool>;
    /// `stat(path).isDirectory()`, following a symbolic link; `None` when
    /// `path` is absent.
    fn stat_dir(&self, path: &Path) -> io::Result<Option<bool>>;
    /// The platform whose write sequence the writer issues; see the module
    /// comment.
    fn write_platform(&self) -> PathPlatform {
        PathPlatform::host()
    }
}

/// The real filesystem.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealIo;

impl StorageIo for RealIo {
    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(dir)
    }

    fn create_new(&self, path: &Path) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(path)
    }

    fn open_write(&self, path: &Path) -> io::Result<File> {
        OpenOptions::new().write(true).open(path)
    }

    fn write_at(&self, file: &mut File, offset: u64, bytes: &[u8]) -> io::Result<()> {
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(bytes)
    }

    fn set_len(&self, file: &File, len: u64) -> io::Result<()> {
        file.set_len(len)
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        fs::hard_link(original, link)
    }

    fn rename_new(&self, from: &Path, to: &Path) -> io::Result<()> {
        #[cfg(windows)]
        {
            move_file_write_through(from, to)
        }
        #[cfg(not(windows))]
        {
            // Only the Win32 sequence a faulting test names on another host
            // reaches here, so a check before the rename stands in for the
            // move's refusal to replace.
            match fs::symlink_metadata(to) {
                Ok(_) => Err(io::Error::from(ErrorKind::AlreadyExists)),
                Err(error) if error.kind() == ErrorKind::NotFound => fs::rename(from, to),
                Err(error) => Err(error),
            }
        }
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn sync_file(&self, file: &File, _path: &Path) -> io::Result<()> {
        file.sync_all()
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        #[cfg(windows)]
        {
            // TypeScript syncs no directory on Windows, whose write sequence
            // never calls this.
            let _ = dir;
            Ok(())
        }
        #[cfg(not(windows))]
        {
            File::open(dir)?.sync_all()
        }
    }

    fn open_lock(&self, path: &Path) -> io::Result<File> {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_SHARE_READ: u32 = 1;
            const FILE_SHARE_WRITE: u32 = 2;
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .open(path)
        }
        #[cfg(not(windows))]
        {
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
        }
    }

    fn try_lock(&self, file: &File) -> Result<(), LockFailure> {
        match file.try_lock() {
            Ok(()) => Ok(()),
            Err(TryLockError::WouldBlock) => Err(LockFailure::WouldBlock),
            Err(TryLockError::Error(error)) => Err(LockFailure::Io(error)),
        }
    }

    fn lock_is_current(&self, held: &File, path: &Path) -> io::Result<bool> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let held = held.metadata()?;
            match fs::metadata(path) {
                Ok(current) => Ok(current.dev() == held.dev() && current.ino() == held.ino()),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error),
            }
        }
        #[cfg(not(unix))]
        {
            // Windows holds the file without share-delete, so it cannot be
            // replaced while held.
            let _ = (held, path);
            Ok(true)
        }
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }

    fn read_dir(&self, dir: &Path) -> io::Result<Option<Vec<ListedEntry>>> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut listed = Vec::new();
        for entry in entries {
            let entry = entry?;
            let kind = entry.file_type()?;
            listed.push(ListedEntry {
                name: entry.file_name(),
                is_dir: kind.is_dir(),
                is_file: kind.is_file(),
            });
        }
        Ok(Some(listed))
    }

    fn canonicalize(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        match fs::canonicalize(path) {
            Ok(resolved) => Ok(Some(resolved)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn probe(&self, path: &Path) -> io::Result<bool> {
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
            Err(error) => Err(error),
        }
    }

    fn stat_dir(&self, path: &Path) -> io::Result<Option<bool>> {
        match fs::metadata(path) {
            Ok(metadata) => Ok(Some(metadata.is_dir())),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

/// `MoveFileExW(from, to, MOVEFILE_WRITE_THROUGH)`, which neither replaces
/// an existing `to` nor copies across volumes, as `publishNewFileWin32` calls
/// it. The paths are passed as given, where TypeScript passes their
/// `toNamespacedPath` spelling.
#[cfg(windows)]
fn move_file_write_through(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

    let wide = |path: &Path| -> io::Result<Vec<u16>> {
        let mut units: Vec<u16> = path.as_os_str().encode_wide().collect();
        if units.contains(&0) {
            return Err(io::Error::from(ErrorKind::InvalidInput));
        }
        units.push(0);
        Ok(units)
    };
    let (from, to) = (wide(from)?, wide(to)?);
    // SAFETY: both buffers are NUL-terminated UTF-16 strings without an
    // interior NUL, alive for the call, which only reads them.
    let moved = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) };
    if moved == 0 {
        // ERROR_ALREADY_EXISTS and ERROR_FILE_EXISTS map to AlreadyExists.
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
