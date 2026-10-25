//! `<root>/install.lock`: the exclusive lock every install, update,
//! rollback, and prune holds.
//!
//! The lock is a directory, taken by creating it: `mkdir` is atomic on
//! every filesystem the installers run on, and the POSIX shell and
//! PowerShell installers take the same lock the same way, which `flock`
//! would not allow (macOS has no `flock(1)`).
//!
//! The holder writes `<root>/install.lock/owner` immediately after
//! creating the directory: one line of `<pid> <unix-seconds> <kind>`. A
//! held lock is stale only when that line names a process that is no
//! longer alive; a lock whose owner file is missing or unreadable counts
//! as held for [`NEW_LOCK_GRACE`], because its holder may be about to
//! write it, and is stale after that. A stale lock is removed and the
//! acquisition retried once. Releasing removes the owner file and then
//! the directory, on every path, through [`InstallLock`]'s drop.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::UpdateError;

/// The lock directory's name under the install root.
pub const LOCK_DIR: &str = "install.lock";

/// The owner file inside it.
pub const OWNER_FILE: &str = "owner";

/// How long a lock with no readable owner file counts as held.
pub const NEW_LOCK_GRACE: Duration = Duration::from_secs(10);

/// What a holder is doing, as the owner file records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockKind {
    Install,
    Update,
    Rollback,
    Prune,
}

impl LockKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Update => "update",
            Self::Rollback => "rollback",
            Self::Prune => "prune",
        }
    }
}

/// A held lock. Dropping it releases the lock.
#[derive(Debug)]
pub struct InstallLock {
    dir: PathBuf,
}

impl InstallLock {
    pub fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.dir.join(OWNER_FILE));
        if fs::remove_dir(&self.dir).is_err() {
            // Something else wrote inside the lock; the directory must not
            // outlive its holder, so remove what is there.
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

/// Take the install lock for `kind`.
///
/// # Errors
///
/// [`UpdateError::Locked`] while another live run holds it, naming that
/// run; [`UpdateError::NotInstalled`] when the root does not exist; and
/// [`UpdateError::Install`] when the lock cannot be written.
pub fn acquire(root: &Path, kind: LockKind) -> Result<InstallLock, UpdateError> {
    if !root.is_dir() {
        return Err(UpdateError::NotInstalled(format!(
            "{} is not a Bake install directory",
            root.display()
        )));
    }
    let dir = root.join(LOCK_DIR);
    for attempt in 0..2 {
        match fs::create_dir(&dir) {
            Ok(()) => {
                let lock = InstallLock { dir: dir.clone() };
                let owner = format!("{} {} {}\n", std::process::id(), now(), kind.name());
                fs::write(dir.join(OWNER_FILE), owner).map_err(|error| {
                    UpdateError::Install(format!("could not write the install lock: {error}"))
                })?;
                return Ok(lock);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if attempt > 0 || !stale(&dir) {
                    return Err(UpdateError::Locked(held_message(&dir)));
                }
                let _ = fs::remove_file(dir.join(OWNER_FILE));
                if fs::remove_dir(&dir).is_err() {
                    let _ = fs::remove_dir_all(&dir);
                }
            }
            Err(error) => {
                return Err(UpdateError::Install(format!(
                    "could not take {}: {error}",
                    dir.display()
                )));
            }
        }
    }
    Err(UpdateError::Locked(held_message(&dir)))
}

/// What the owner file says: the holder's process id and what it is doing.
fn owner(dir: &Path) -> Option<(u32, String)> {
    let text = fs::read_to_string(dir.join(OWNER_FILE)).ok()?;
    let mut fields = text.split_whitespace();
    let pid = fields.next()?.parse().ok()?;
    let _started = fields.next();
    let kind = fields.next().unwrap_or("install").to_owned();
    Some((pid, kind))
}

/// Whether a held lock may be taken over: its owner names a process that
/// has exited, or it has had no readable owner file for longer than
/// [`NEW_LOCK_GRACE`].
fn stale(dir: &Path) -> bool {
    match owner(dir) {
        Some((pid, _)) => !process_alive(pid),
        None => age(dir).is_none_or(|age| age > NEW_LOCK_GRACE),
    }
}

fn held_message(dir: &Path) -> String {
    match owner(dir) {
        Some((pid, kind)) => {
            format!("another Bake {kind} is running (process {pid}); try again when it finishes")
        }
        None => "another Bake install is running; try again when it finishes".to_owned(),
    }
}

/// How long ago the lock directory was created, as its modification time
/// records it; `None` when that cannot be read or lies in the future.
fn age(dir: &Path) -> Option<Duration> {
    let modified = fs::metadata(dir).and_then(|meta| meta.modified()).ok()?;
    SystemTime::now().duration_since(modified).ok()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// Whether the process holding the lock still exists. A process this user
/// may not signal exists, so only a definite "no such process" is dead.
#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 checks for the process without delivering anything.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, GetLastError};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const STILL_ACTIVE: u32 = 259;
    // SAFETY: the handle is closed before returning, and the exit code is
    // read only through a handle `OpenProcess` returned.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            // No such process id; anything else (access denied) is a
            // process this user may not open, which exists.
            return GetLastError() != ERROR_INVALID_PARAMETER;
        }
        let mut code: u32 = 0;
        let read = GetExitCodeProcess(handle, &raw mut code);
        CloseHandle(handle);
        read == 0 || code == STILL_ACTIVE
    }
}

#[cfg(not(any(unix, windows)))]
fn process_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::tests::temp_dir;

    #[test]
    fn the_lock_is_a_directory_with_an_owner_line_and_is_released_on_drop() {
        let dir = temp_dir("lock-owner");
        let lock = acquire(dir.path(), LockKind::Update).expect("the lock");
        let owner = fs::read_to_string(dir.path().join(LOCK_DIR).join(OWNER_FILE)).expect("owner");
        let fields: Vec<&str> = owner.trim_end().split(' ').collect();
        assert_eq!(fields.len(), 3, "{owner:?}");
        assert_eq!(fields[0].parse::<u32>().ok(), Some(std::process::id()));
        assert!(fields[1].parse::<u64>().is_ok(), "{owner:?}");
        assert_eq!(fields[2], "update");
        assert!(owner.ends_with('\n'));
        drop(lock);
        assert!(!dir.path().join(LOCK_DIR).exists());
    }

    #[test]
    fn a_live_holder_keeps_the_lock_and_the_message_names_it() {
        let dir = temp_dir("lock-held");
        let held = acquire(dir.path(), LockKind::Install).expect("the lock");
        let error = acquire(dir.path(), LockKind::Update).expect_err("held");
        assert!(matches!(error, UpdateError::Locked(_)), "{error}");
        assert!(
            error.message().contains("another Bake install is running")
                && error.message().contains(&std::process::id().to_string()),
            "{error}"
        );
        drop(held);
        drop(acquire(dir.path(), LockKind::Update).expect("free again"));
    }

    #[test]
    fn a_lock_whose_owner_has_exited_is_taken_over() {
        let dir = temp_dir("lock-stale");
        let lock = dir.path().join(LOCK_DIR);
        fs::create_dir(&lock).expect("lock");
        // Process id 0 is never a live process this updater started.
        fs::write(lock.join(OWNER_FILE), format!("{} 1 update\n", dead_pid())).expect("owner");
        let taken = acquire(dir.path(), LockKind::Rollback).expect("stale lock taken over");
        let owner = fs::read_to_string(lock.join(OWNER_FILE)).expect("owner");
        assert!(owner.ends_with(" rollback\n"), "{owner:?}");
        drop(taken);
        assert!(!lock.exists());
    }

    #[test]
    fn a_lock_with_no_owner_file_is_held_until_the_grace_passes() {
        let dir = temp_dir("lock-no-owner");
        let lock = dir.path().join(LOCK_DIR);
        fs::create_dir(&lock).expect("lock");
        let error = acquire(dir.path(), LockKind::Update).expect_err("held");
        assert!(matches!(error, UpdateError::Locked(_)), "{error}");
        assert_eq!(
            error.message(),
            "another Bake install is running; try again when it finishes"
        );
        // An unreadable owner line counts the same way.
        fs::write(lock.join(OWNER_FILE), "garbage\n").expect("owner");
        assert!(acquire(dir.path(), LockKind::Update).is_err());
        // Older than the grace, with nothing naming a holder: stale.
        assert!(stale(Path::new("/this/lock/does/not/exist")));
    }

    #[test]
    fn a_missing_root_is_not_an_install() {
        let dir = temp_dir("lock-root");
        let error = acquire(&dir.path().join("absent"), LockKind::Update).expect_err("no root");
        assert!(matches!(error, UpdateError::NotInstalled(_)), "{error}");
    }

    /// A process id that no process has: the highest id, which this test
    /// does not create, and which `kill(pid, 0)` reports as absent.
    fn dead_pid() -> u32 {
        let mut candidate = 4_194_303;
        while process_alive(candidate) && candidate > 2 {
            candidate -= 1;
        }
        candidate
    }
}
