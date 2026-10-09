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
//! TypeScript locks its first byte; the ranges overlap.
//!
//! The lock file's operations go through the `storage_io` seam. A holder
//! that dies leaves the file and its lock to the kernel, which releases the
//! lock when it closes the file; the fault-injection tests drop a crashed
//! value for that.
//!
//! Exclusion between a Rust and a TypeScript holder is tested across real
//! processes by `bun run test:rust:lease`, which drives the development-only
//! `bake-session-lease-probe` against the TypeScript JSONL backend: a live
//! holder of either runtime refuses the other's write `open`, and a released
//! or killed holder lets the other take over and append. A stopped (not
//! killed) holder's exclusion is tested on Unix only.

use std::fs::File;
use std::io;
use std::path::Path;

use crate::storage_io::{LockFailure, StorageIo};

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
    /// Lock `dir/session.lock` through `io`, creating `dir` and the file when
    /// absent.
    pub(crate) fn acquire(io: &dyn StorageIo, dir: &Path) -> Result<Self, LeaseRefusal> {
        io.create_dir_all(dir)?;
        let path = dir.join(LEASE_FILENAME);
        #[cfg(unix)]
        {
            for _ in 0..ATTEMPTS {
                let file = io.open_lock(&path)?;
                lock(io, &file)?;
                if io.lock_is_current(&file, &path)? {
                    return Ok(Self { file });
                }
                // The locked inode is no longer at the path; dropping the
                // file releases its lock before the next attempt.
            }
            Err(LeaseRefusal::AlreadyOwned)
        }
        #[cfg(windows)]
        {
            let file = io.open_lock(&path)?;
            lock(io, &file)?;
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
fn lock(io: &dyn StorageIo, file: &File) -> Result<(), LeaseRefusal> {
    match io.try_lock(file) {
        Ok(()) => Ok(()),
        Err(LockFailure::WouldBlock) => Err(LeaseRefusal::AlreadyOwned),
        Err(LockFailure::Io(error)) => Err(LeaseRefusal::Io(error)),
    }
}

impl Drop for WriteLease {
    fn drop(&mut self) {
        // Closing the file releases the lock as well; unlocking first frees
        // it at once on Windows, where a closed handle's lock may linger.
        let _ = self.file.unlock();
    }
}
