//! Process-tree ownership for shell commands: Pi's `detached` spawn,
//! `killProcessTree`, and its detached-child tracking from
//! `src/utils/shell.ts`.
//!
//! Unix: the shell calls `setsid` before `exec`, as Node's `detached: true`
//! does, so it leads a new session and process group with no controlling
//! terminal, and `kill(-pgid, SIGKILL)` reaches every descendant that stayed
//! in the group. Windows: Pi runs `taskkill /F /T /PID`; this port puts the
//! shell in a new job object, which every descendant joins, and terminates
//! the job. Should the job not be created or assigned, it falls back to
//! Pi's `taskkill`. A descendant that starts its own session (Unix) or
//! breaks away from the job (Windows) escapes, as it escapes Pi.
//!
//! The only `unsafe` code is the FFI calls below, each with its contract.

use std::sync::{Arc, Mutex, PoisonError};

use tokio::process::{Child, Command};

/// Puts the command's process in a tree [`ProcessTree::kill`] can end.
pub(super) fn configure(command: &mut Command) {
    #[cfg(unix)]
    {
        // SAFETY: the closure runs in the forked child before `exec` and
        // calls only `setsid`, which is async-signal-safe, and reads errno.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    #[cfg(windows)]
    {
        // Pi's `windowsHide: true`.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
}

/// A spawned shell's whole tree.
#[derive(Debug)]
pub(super) struct ProcessTree {
    pid: u32,
    #[cfg(windows)]
    job: Option<std::os::windows::io::OwnedHandle>,
}

impl ProcessTree {
    /// Adopts a just-spawned child; `None` when it has no pid (it already
    /// exited and was reaped).
    pub(super) fn adopt(child: &Child) -> Option<Self> {
        let pid = child.id()?;
        Some(Self {
            pid,
            #[cfg(windows)]
            job: windows::job_for(child),
        })
    }

    /// Kills every process in the tree; errors mean it is already gone.
    pub(super) fn kill(&self) {
        #[cfg(unix)]
        {
            // `kill(-1)` and `kill(0)` would signal this process's own
            // group or every process the user owns.
            let Ok(pgid) = libc::pid_t::try_from(self.pid) else {
                return;
            };
            if pgid > 1 {
                // SAFETY: plain syscall with integer arguments.
                unsafe {
                    libc::kill(-pgid, libc::SIGKILL);
                }
            }
        }
        #[cfg(windows)]
        {
            match &self.job {
                Some(job) => windows::terminate(job),
                None => windows::taskkill(self.pid),
            }
        }
    }
}

/// Pi's `trackedDetachedChildPids`: trees still running a command.
static TRACKED: Mutex<Tracked> = Mutex::new(Tracked {
    next: 0,
    trees: Vec::new(),
});

struct Tracked {
    next: u64,
    trees: Vec<(u64, Arc<ProcessTree>)>,
}

/// Pi's `trackDetachedChildPid`; dropping the guard is
/// `untrackDetachedChildPid`.
pub(super) struct TrackedTree {
    id: u64,
    tree: Arc<ProcessTree>,
}

impl TrackedTree {
    pub(super) fn new(tree: ProcessTree) -> Self {
        let tree = Arc::new(tree);
        let mut tracked = TRACKED.lock().unwrap_or_else(PoisonError::into_inner);
        let id = tracked.next;
        tracked.next = tracked.next.wrapping_add(1);
        tracked.trees.push((id, Arc::clone(&tree)));
        Self { id, tree }
    }

    pub(super) fn kill(&self) {
        self.tree.kill();
    }
}

impl Drop for TrackedTree {
    fn drop(&mut self) {
        TRACKED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .trees
            .retain(|(id, _)| *id != self.id);
    }
}

/// Pi's `killTrackedDetachedChildren`: kills the tree of every command still
/// running, for a shutdown that cannot await each call (Pi runs it on
/// `SIGHUP` and `SIGTERM`). Returns at once; each call still awaits its own
/// shell when it is next polled or dropped.
pub fn kill_tracked_detached_children() {
    let trees = std::mem::take(&mut TRACKED.lock().unwrap_or_else(PoisonError::into_inner).trees);
    for (_, tree) in trees {
        tree.kill();
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::PathBuf;

    use tokio::process::Child;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
    };

    /// A new job holding `child`, or `None` if either step fails.
    pub(super) fn job_for(child: &Child) -> Option<OwnedHandle> {
        let process = child.raw_handle()?;
        // SAFETY: null attributes and name request an unnamed job with
        // default security; the call has no other preconditions.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return None;
        }
        // SAFETY: `job` is a fresh handle this function exclusively owns.
        let job = unsafe { OwnedHandle::from_raw_handle(job) };
        // SAFETY: both handles are valid for the call: `job` is owned here
        // and `process` is borrowed from the live `Child`.
        let assigned = unsafe { AssignProcessToJobObject(job.as_raw_handle(), process) };
        (assigned != 0).then_some(job)
    }

    /// Terminates every process in the job with exit code 1, as
    /// `taskkill /F` leaves.
    pub(super) fn terminate(job: &OwnedHandle) {
        // SAFETY: `job` is a valid job handle owned by the tree.
        unsafe {
            TerminateJobObject(job.as_raw_handle(), 1);
        }
    }

    /// Pi's `killProcessTree` on Windows: System32's `taskkill`, so cleanup
    /// does not depend on `PATH`. Like Pi, it is not awaited.
    pub(super) fn taskkill(pid: u32) {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new(taskkill_path(
            std::env::var_os("SystemRoot").map(PathBuf::from),
        ))
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    }

    pub(super) fn taskkill_path(system_root: Option<PathBuf>) -> PathBuf {
        system_root
            .unwrap_or_else(|| PathBuf::from("C:\\Windows"))
            .join("System32")
            .join("taskkill.exe")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Pi `test/suite/regressions/6596-taskkill-enoent.test.ts`: the
        /// trusted System32 executable under `SystemRoot`.
        #[test]
        fn taskkill_uses_system32_under_system_root() {
            assert_eq!(
                taskkill_path(Some(PathBuf::from("C:\\CustomWindows"))),
                PathBuf::from("C:\\CustomWindows\\System32\\taskkill.exe")
            );
            assert_eq!(
                taskkill_path(None),
                PathBuf::from("C:\\Windows\\System32\\taskkill.exe")
            );
        }
    }
}
