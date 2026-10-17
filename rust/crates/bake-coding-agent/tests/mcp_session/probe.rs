//! Stable identities of the processes a case starts, so a check that a
//! process is gone, or a cleanup kill, never reaches a reused pid. It
//! observes and cleans up; it is not a process owner, and it never reaps.
//!
//! Adapted from `bake-mcp`'s `tests/stdio/observe.rs` as qualified at
//! `be46b191` (the macOS `proc_pidinfo` zombie fix included). Identity:
//!
//! - The server is this process's child, held unreaped by its owner from
//!   its spawn (a process handle on Windows), so its pid is stable when it is
//!   observed.
//! - A grandchild is observed while the server still runs: the fixture
//!   server never waits for it and keeps its handle (`fixture.rs`), so until
//!   the server exits the grandchild's pid cannot name another process. The
//!   identity is opened first and the server checked as still running after
//!   it; otherwise the observation fails and nothing is ever signalled.
//!
//! Per platform the identity is a Linux pidfd (signals go through it), a
//! Windows process handle (which keeps the pid from being reused), or on
//! macOS the pid with its start time from `proc_pidinfo`. macOS has no
//! process handle and no atomic check-and-signal: each kill re-checks the
//! start time first, and a process that exits and whose pid is reused
//! between that check and the signal could be signalled. That window is
//! this fixture's macOS limitation. Other platforms are not supported: a
//! case that needs an observation fails there rather than passing vacuously.

use std::time::{Duration, Instant};

/// How long cleanup waits for a killed process to exit.
const CLEANUP_BOUND: Duration = Duration::from_secs(10);

/// A process a case started, observed by a stable identity.
pub struct Process {
    pub pid: u32,
    identity: sys::Identity,
}

impl Process {
    /// Observes `pid`, this process's child whose owner holds it unreaped.
    pub fn observe(pid: u32) -> Result<Self, String> {
        let identity = sys::Identity::open(pid)
            .map_err(|error| format!("could not observe {pid}: {error}"))?;
        Ok(Self { pid, identity })
    }

    /// Observes `pid`, a child of `parent`, which must still run once the
    /// identity is open.
    pub fn observe_descendant(parent: &Process, pid: u32) -> Result<Self, String> {
        let process = Self::observe(pid)?;
        if !parent.identity.running() {
            // Possibly another process by now: never signalled.
            return Err(format!(
                "{pid}'s parent {} could not be confirmed running: its identity is unverified",
                parent.pid
            ));
        }
        Ok(process)
    }

    /// Whether it has exited (a zombie counts), checked at once.
    pub fn exited(&self) -> bool {
        self.identity.exited()
    }

    /// Kills it if it still runs, then waits, bounded, until it has exited.
    /// `Ok(true)` when it had to be killed.
    pub fn clean_up(&self) -> Result<bool, String> {
        if self.exited() {
            return Ok(false);
        }
        self.identity.kill();
        let deadline = Instant::now() + CLEANUP_BOUND;
        while !self.exited() {
            if Instant::now() >= deadline {
                return Err(format!(
                    "process {} still runs {CLEANUP_BOUND:?} after a kill",
                    self.pid
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(true)
    }
}

#[cfg(target_os = "linux")]
mod sys {
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    pub struct Identity {
        pidfd: OwnedFd,
    }

    impl Identity {
        pub fn open(pid: u32) -> io::Result<Self> {
            let raw = libc::pid_t::try_from(pid).map_err(|_| io::Error::other("pid"))?;
            // SAFETY: plain syscall; it returns a new descriptor or -1.
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, raw, 0) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let fd = i32::try_from(fd).map_err(|_| io::Error::other("fd"))?;
            // SAFETY: a fresh descriptor owned only here.
            Ok(Self {
                pidfd: unsafe { OwnedFd::from_raw_fd(fd) },
            })
        }

        /// A pidfd is readable once its process exits. Only `POLLIN` means
        /// that: `POLLERR`, `POLLHUP`, or `POLLNVAL` alone is not an exit.
        pub fn exited(&self) -> bool {
            let mut poll = libc::pollfd {
                fd: self.pidfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd, no wait.
            let ready = unsafe { libc::poll(&mut poll, 1, 0) };
            ready == 1 && poll.revents & libc::POLLIN != 0
        }

        pub fn running(&self) -> bool {
            let mut poll = libc::pollfd {
                fd: self.pidfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd, no wait. An error is not liveness.
            unsafe { libc::poll(&mut poll, 1, 0) == 0 }
        }

        pub fn kill(&self) {
            // SAFETY: plain syscall on our pidfd; it never reaches another
            // process.
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.pidfd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use std::io;

    /// `SZOMB` from `<sys/proc.h>`.
    const SZOMB: u32 = 5;

    pub struct Identity {
        pid: u32,
        start: (u64, u64),
    }

    /// The process's start time and status, or `None` if no such process.
    fn info(pid: u32) -> io::Result<Option<((u64, u64), u32)>> {
        let raw =
            libc::c_int::try_from(pid).map_err(|_| io::Error::other("pid is out of range"))?;
        // SAFETY: a zeroed struct is valid output space.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>())
            .map_err(|_| io::Error::other("proc_bsdinfo is too large"))?;
        // SAFETY: `info` has the size passed.
        let written = unsafe {
            libc::proc_pidinfo(
                raw,
                libc::PROC_PIDTBSDINFO,
                // Nonzero includes zombies; zero hides them as ESRCH, which
                // would read an unreaped process as gone.
                1,
                std::ptr::addr_of_mut!(info).cast(),
                size,
            )
        };
        if written == size {
            return Ok(Some((
                (info.pbi_start_tvsec, info.pbi_start_tvusec),
                info.pbi_status,
            )));
        }
        if written <= 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(None)
            } else {
                Err(error)
            };
        }
        Err(io::Error::other(
            "proc_pidinfo returned an incomplete record",
        ))
    }

    impl Identity {
        pub fn open(pid: u32) -> io::Result<Self> {
            let (start, _) = info(pid)?.ok_or_else(|| io::Error::other("no such process"))?;
            Ok(Self { pid, start })
        }

        /// A zombie, a process gone, or another process with the pid.
        pub fn exited(&self) -> bool {
            match info(self.pid) {
                Ok(Some((start, status))) => start != self.start || status == SZOMB,
                Ok(None) => true,
                Err(_) => false,
            }
        }

        pub fn running(&self) -> bool {
            matches!(info(self.pid), Ok(Some((start, status))) if start == self.start && status != SZOMB)
        }

        /// Re-checks the start time, then signals; not atomic (see the
        /// module documentation).
        pub fn kill(&self) {
            if self.running()
                && let Ok(pid) = libc::pid_t::try_from(self.pid)
            {
                // SAFETY: plain syscall on the verified pid.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }
}

#[cfg(windows)]
mod sys {
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
        TerminateProcess, WaitForSingleObject,
    };

    /// A process handle of its own, which keeps the pid from naming another
    /// process and is signalled when the process exits.
    pub struct Identity {
        handle: OwnedHandle,
    }

    impl Identity {
        pub fn open(pid: u32) -> io::Result<Self> {
            // SAFETY: plain call; a null handle is an error.
            let raw = unsafe {
                OpenProcess(
                    PROCESS_SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                    0,
                    pid,
                )
            };
            if raw.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: a fresh handle owned only here.
            Ok(Self {
                handle: unsafe { OwnedHandle::from_raw_handle(raw) },
            })
        }

        /// Signalled means exited; `WAIT_TIMEOUT` (running) and
        /// `WAIT_FAILED` are not.
        pub fn exited(&self) -> bool {
            // SAFETY: a live handle; a zero timeout only checks.
            unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) == WAIT_OBJECT_0 }
        }

        pub fn running(&self) -> bool {
            // SAFETY: a live handle; a failed wait is not liveness.
            unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) == WAIT_TIMEOUT }
        }

        pub fn kill(&self) {
            // SAFETY: a live handle to the observed process.
            unsafe {
                TerminateProcess(self.handle.as_raw_handle(), 1);
            }
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod sys {
    use std::io;

    pub struct Identity;

    impl Identity {
        pub fn open(_pid: u32) -> io::Result<Self> {
            Err(io::Error::other("no process observer on this platform"))
        }

        pub fn exited(&self) -> bool {
            false
        }

        pub fn running(&self) -> bool {
            false
        }

        pub fn kill(&self) {}
    }
}
