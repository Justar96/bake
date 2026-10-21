//! A test-only log of the filesystem operations the durable writes make,
//! in order, for the simulated power-loss tests
//! (`session::manager::power_loss_tests`). Each thread records into its
//! own log between [`start`] and [`finish`]; nothing is recorded outside
//! those calls or outside the crate's tests.

use std::cell::RefCell;
use std::path::PathBuf;

/// One operation a durable write made, and succeeded at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiskOp {
    /// A name was created for a new, empty file.
    Create(PathBuf),
    /// A directory was created; its parent is not yet synced.
    CreateDir(PathBuf),
    /// The file at the name was truncated to empty.
    Truncate(PathBuf),
    /// Bytes were written at the file's end. Every durable write appends:
    /// to a session file opened for appending, or to a file just created
    /// or truncated.
    Write(PathBuf, Vec<u8>),
    /// The file's data was synced (`fsync` or `fdatasync`).
    Fsync(PathBuf),
    /// The first name was renamed over the second.
    Rename(PathBuf, PathBuf),
    /// The name was removed.
    Remove(PathBuf),
    /// The directory was synced, making its names durable.
    DirSync(PathBuf),
}

thread_local! {
    static LOG: RefCell<Option<Vec<DiskOp>>> = const { RefCell::new(None) };
}

/// Start recording this thread's operations, discarding any earlier log.
pub(crate) fn start() {
    LOG.with(|log| *log.borrow_mut() = Some(Vec::new()));
}

/// Stop recording and return this thread's operations since [`start`].
pub(crate) fn finish() -> Vec<DiskOp> {
    LOG.with(|log| log.borrow_mut().take().unwrap_or_default())
}

/// Record `op` when this thread is recording.
pub(super) fn record(op: DiskOp) {
    LOG.with(|log| {
        if let Some(log) = log.borrow_mut().as_mut() {
            log.push(op);
        }
    });
}
