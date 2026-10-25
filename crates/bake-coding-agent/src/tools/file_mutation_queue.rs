//! Serialize local file mutations as Pi's `src/core/tools/file-mutation-queue.ts`
//! does (v1.1.0; see the crate's `NOTICE`). All callers share one process-wide
//! registry. Registration resolves a path before taking its place in the queue,
//! in registration order; unrelated files can execute concurrently.
//!
//! The callback is synchronous. Async callers must run the whole call on an
//! owned blocking worker and await it during teardown. Dropping an async wait
//! must not release a queue while a filesystem operation is still in flight.
//! This module starts no worker and provides no cross-process exclusion.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::session::paths::lexical_normalize;

// The Loom test compiles this same core with instrumented synchronization.
mod sync {
    pub(super) use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
}
mod queue;

/// Run a synchronous mutation after every earlier registration for this file
/// has completed. The callback's return or unwind releases the next waiter.
/// Callback errors propagate unchanged; path-resolution errors run no callback.
///
/// Existing symlink aliases share a queue. A missing path, including `ENOTDIR`,
/// uses its lexical absolute path, as Pi does. Hard links and aliases of missing
/// files need not share a queue. This is ordering, not an authorization check;
/// filesystem changes can invalidate the resolved key after registration.
///
/// Cancellation is cooperative: check it inside the callback before each
/// effect and after completed I/O, as Pi's write and edit tools do. Never
/// launch work that outlives the callback. Recursively entering the same queue
/// from a callback deadlocks; multi-file callbacks must not nest queue calls.
pub fn with_file_mutation_queue<T>(
    file_path: &Path,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    static QUEUE: OnceLock<queue::Queue> = OnceLock::new();
    QUEUE
        .get_or_init(queue::Queue::new)
        .run(|| mutation_key(file_path), operation)
}

fn mutation_key(path: &Path) -> io::Result<PathBuf> {
    // path.resolve does not expand '~' or shell paths; the tool owns that step.
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::path::absolute(path)?
    };
    let resolved = lexical_normalize(&absolute);
    match std::fs::canonicalize(&resolved) {
        Ok(canonical) => Ok(normalize_key(canonical)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(normalize_key(resolved))
        }
        Err(error) => Err(error),
    }
}

#[cfg(not(windows))]
fn normalize_key(path: PathBuf) -> PathBuf {
    path
}

#[cfg(windows)]
fn normalize_key(path: PathBuf) -> PathBuf {
    use std::path::{Component, Prefix};
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return path;
    };
    // canonicalize adds a verbatim prefix, unlike Node realpath. Without this
    // normalization, creating a missing file changes its key mid-operation.
    let mut normalized = match prefix.kind() {
        Prefix::VerbatimDisk(drive) => PathBuf::from(format!("{}:", char::from(drive))),
        Prefix::VerbatimUNC(server, share) => {
            let mut prefix = std::ffi::OsString::from(r"\\");
            prefix.push(server);
            prefix.push(r"\");
            prefix.push(share);
            PathBuf::from(prefix)
        }
        _ => return path,
    };
    normalized.push(components.as_path());
    normalized
}

#[cfg(test)]
mod tests;
