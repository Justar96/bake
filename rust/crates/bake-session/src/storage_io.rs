//! The I/O seam of the development-only Session writer: every filesystem
//! mutation [`crate::PlainLogFile`] and its write lock make, and every read
//! a write `open` and its recovery depend on, go through one [`StorageIo`].
//!
//! Production uses [`RealIo`], the real filesystem, and the public
//! constructors of [`crate::PlainLogFile`] use it alone. The trait exists so
//! storage tests can inject a deterministic fault at a chosen operation (D30):
//! a disk-full, permission, or I/O error, a torn write that stores only a
//! prefix of its bytes, or a crash after which every operation fails, so no
//! cleanup reaches the disk. It is hidden from the documented API and is not
//! a stable interface.
//!
//! The operations, one [`StorageOp`] each:
//!
//! - Namespace mutations: [`StorageIo::create_dir_all`] (mode `0o700` on
//!   Unix), [`StorageIo::create_new`] (a new file for writing, mode `0o600`
//!   on Unix), [`StorageIo::hard_link`], and [`StorageIo::remove_file`].
//! - Content mutations of an open file: [`StorageIo::open_write`],
//!   [`StorageIo::write_at`], which a fault may tear after any byte, and
//!   [`StorageIo::set_len`].
//! - The write lock: [`StorageIo::open_lock`], which opens and, on Unix,
//!   truncates the lock file, [`StorageIo::try_lock`], and
//!   [`StorageIo::lock_is_current`], which reads whether the locked file is
//!   still the one at its path.
//! - Reads recovery depends on: [`StorageIo::read`] of a whole log,
//!   [`StorageIo::read_dir`] of a root, project, or Session directory,
//!   [`StorageIo::canonicalize`] for the stored-identity check, and
//!   [`StorageIo::probe`] for the flat-layout probe.
//!
//! Closing a file and releasing a lock are not operations: they are what the
//! kernel does when a process dies, so a crashed handle's files may still be
//! dropped. The writer does not fsync a file or a directory (see
//! [`crate::PlainLogFile`]), so no sync operation exists.

use std::ffi::OsString;
use std::fmt::Debug;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, ErrorKind, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The kind of one [`StorageIo`] operation, in the module's taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StorageOp {
    CreateDir,
    CreateNew,
    OpenWrite,
    WriteAt,
    SetLen,
    HardLink,
    RemoveFile,
    OpenLock,
    TryLock,
    LockIsCurrent,
    Read,
    ReadDir,
    Canonicalize,
    Probe,
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
    /// Remove the file at `path`.
    fn remove_file(&self, path: &Path) -> io::Result<()>;
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

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
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
}
