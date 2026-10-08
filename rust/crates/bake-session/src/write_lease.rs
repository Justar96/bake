//! The cross-process write lock of one Session directory, taken as
//! TypeScript's `SessionWriteLease.acquire` takes it: a non-blocking
//! exclusive lock on the `session.lock` file beside the log, held until the
//! value is dropped. The lock file is never removed.
//!
//! On Unix the file is opened for writing, created and truncated as Node's
//! `open(path, 'w')` opens it, then locked with `flock`. A lock names an
//! inode, so the locked file must still be the one at the path; otherwise
//! the attempt starts over, and three failed attempts report the lock as
//! owned, as TypeScript does. On Windows the file is opened for reading and
//! writing, created if absent, without share-delete so it cannot be replaced
//! while held, then locked with `LockFileEx`. std locks the whole file where
//! TypeScript locks its first byte; the ranges overlap, but exclusion between
//! a Rust and a TypeScript holder is not tested.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;

/// Base name of the lock file in a Session directory.
const LEASE_FILENAME: &str = "session.lock";

/// How many times Unix relocks after the lock path was replaced.
#[cfg(unix)]
const ATTEMPTS: usize = 3;

/// A held write lock; dropping it releases the lock and keeps the file.
#[derive(Debug)]
pub(crate) struct WriteLease {
    file: File,
}

/// Why [`WriteLease::acquire`] holds no lock.
#[derive(Debug)]
pub(crate) enum LeaseRefusal {
    /// Another handle holds the lock, which TypeScript reports as
    /// `SessionAlreadyOwnedError`.
    AlreadyOwned,
    /// Creating the directory or opening, locking, or checking the file
    /// failed.
    Io(io::Error),
}

impl From<io::Error> for LeaseRefusal {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl WriteLease {
    /// Lock `dir/session.lock`, creating `dir` and the file when absent.
    pub(crate) fn acquire(dir: &Path) -> Result<Self, LeaseRefusal> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(dir)?;
        let path = dir.join(LEASE_FILENAME);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            for _ in 0..ATTEMPTS {
                let file = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&path)?;
                lock(&file)?;
                let held = file.metadata()?;
                match fs::metadata(&path) {
                    Ok(current) if current.dev() == held.dev() && current.ino() == held.ino() => {
                        return Ok(Self { file });
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                // The locked inode is no longer at the path; dropping the
                // file releases its lock before the next attempt.
            }
            Err(LeaseRefusal::AlreadyOwned)
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_SHARE_READ: u32 = 1;
            const FILE_SHARE_WRITE: u32 = 2;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .open(&path)?;
            lock(&file)?;
            Ok(Self { file })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            Err(LeaseRefusal::Io(io::Error::from(
                io::ErrorKind::Unsupported,
            )))
        }
    }
}

/// Take `file`'s exclusive lock without blocking.
#[cfg(any(unix, windows))]
fn lock(file: &File) -> Result<(), LeaseRefusal> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(TryLockError::WouldBlock) => Err(LeaseRefusal::AlreadyOwned),
        Err(TryLockError::Error(error)) => Err(LeaseRefusal::Io(error)),
    }
}

impl Drop for WriteLease {
    fn drop(&mut self) {
        // Closing the file releases the lock as well; unlocking first frees
        // it at once on Windows, where a closed handle's lock may linger.
        let _ = self.file.unlock();
    }
}
