//! The fixture's observer of the processes a test starts: a stable identity
//! for each, taken while it cannot have been reused, and the cleanup the
//! runner drains after every test, passing or failing, before the next one
//! starts. It observes and cleans up; it is not a process owner, and it never
//! signals a bare pid it did not verify.
//!
//! Identity:
//!
//! - The server is this process's child, held unreaped by its owner from its
//!   spawn, so its pid is stable when it is observed.
//! - A grandchild is observed while the server is still running. The fixture
//!   server never waits for its children, so until the server exits the
//!   grandchild is the server's child, running or a zombie, and its pid
//!   cannot name another process. The identity is opened first and the
//!   server checked as still running after it; if the server has exited,
//!   the identity is unverified, the test fails, and cleanup never signals
//!   it.
//!
//! Per platform, the identity is a Linux pidfd (signals and the reap go
//! through it, so it can never reach another process), a Windows process
//! handle (which keeps the pid from being reused), or on macOS the pid with
//! its start time from `proc_pidinfo`. macOS has no process handle: each
//! signal re-checks the start time first, and a process that exits and whose
//! pid is reused between that check and the signal could be signalled. That
//! window is the macOS limitation of this fixture.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long cleanup waits for each process to exit, and to be reaped.
const CLEANUP_BOUND: Duration = Duration::from_secs(10);

/// What the observer may do with a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// This process's own child (the server): cleanup kills it and waits for
    /// its exit, but leaves its reap to the owner or to Tokio's orphan
    /// reaper, which hold its pid; reaping it here could later make Tokio
    /// reap a reused pid.
    Child,
    /// Someone else's child: cleanup kills it, waits for its exit, reaps it
    /// if it became this process's child (a subreaper), and waits until its
    /// new parent has reaped it.
    Descendant,
}

/// A process a test started, observed by a stable identity.
pub struct Observed {
    pid: u32,
    what: String,
    role: Role,
    verified: bool,
    identity: sys::Identity,
}

/// Everything the current test observed; drained by the runner after it.
static OWNED: Mutex<Vec<Arc<Observed>>> = Mutex::new(Vec::new());

impl Observed {
    /// Observes `pid`, this process's child, whose identity its owner holds.
    pub fn child(pid: u32, what: &str) -> Arc<Self> {
        let identity = sys::Identity::open(pid).unwrap_or_else(|error| {
            panic!("could not observe {what} ({pid}): {error}");
        });
        register(Self {
            pid,
            what: what.to_owned(),
            role: Role::Child,
            verified: true,
            identity,
        })
    }

    /// Observes `pid`, a child of `parent`, while `parent` still runs.
    pub fn descendant(parent: &Observed, pid: u32, what: &str) -> Arc<Self> {
        let identity = sys::Identity::open(pid);
        let verified = identity.is_ok() && parent.is_running();
        let Ok(identity) = identity else {
            panic!("could not observe {what} ({pid})");
        };
        let observed = register(Self {
            pid,
            what: what.to_owned(),
            role: Role::Descendant,
            verified,
            identity,
        });
        assert!(
            verified,
            "{what} ({pid}) was observed after its parent exited: its identity is unverified"
        );
        observed
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Running: neither exited nor a zombie.
    pub fn is_running(&self) -> bool {
        self.identity.state() == sys::State::Running
    }

    /// Exited and reaped by its parent (Windows: exited). A zombie is not.
    pub fn is_reaped(&self) -> bool {
        self.identity.state() == sys::State::Reaped
    }

    /// Exited but not yet reaped (Unix).
    #[cfg(unix)]
    pub fn is_zombie(&self) -> bool {
        self.identity.state() == sys::State::Zombie
    }

    /// Kills it through its identity. Nothing is sent to an unverified or
    /// already exited process.
    pub fn kill(&self) {
        if self.verified && self.is_running() {
            self.identity.kill();
        }
    }

    /// Reaps it if it is this process's exited child, without blocking.
    /// Returns whether it was reaped here.
    #[cfg(unix)]
    pub fn reap_if_own_exited_child(&self) -> bool {
        self.verified && self.is_zombie() && self.identity.reap_now()
    }

    /// Kills it and waits, bounded, until it has exited and, for a
    /// descendant, been reaped. Returns what is left, if anything.
    fn clean_up(&self) -> Result<String, String> {
        if !self.verified {
            return Err(format!(
                "{} ({}): unverified identity, not signalled",
                self.what, self.pid
            ));
        }
        let before = self.identity.state();
        self.kill();
        let deadline = Instant::now() + CLEANUP_BOUND;
        #[cfg_attr(windows, allow(unused_mut))]
        let mut reaped_here = false;
        loop {
            let done = match self.role {
                Role::Child => !self.is_running(),
                Role::Descendant => {
                    #[cfg(unix)]
                    {
                        reaped_here |= self.reap_if_own_exited_child();
                    }
                    self.is_reaped()
                }
            };
            if done {
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "{} ({}) still {:?} after {CLEANUP_BOUND:?}",
                    self.what,
                    self.pid,
                    self.identity.state()
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut done = vec![format!("found {before:?}")];
        if before == sys::State::Running {
            done.push("killed".into());
        }
        if reaped_here {
            done.push("reaped here".into());
        }
        done.push(format!("now {:?}", self.identity.state()));
        Ok(format!("{} ({}): {}", self.what, self.pid, done.join(", ")))
    }
}

fn register(observed: Observed) -> Arc<Observed> {
    let observed = Arc::new(observed);
    OWNED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(Arc::clone(&observed));
    observed
}

/// Cleans up everything the finished test observed, descendants first, and
/// returns one line per process and whether all of it is gone.
pub fn drain() -> (Vec<String>, bool) {
    let mut owned = std::mem::take(&mut *OWNED.lock().unwrap_or_else(PoisonError::into_inner));
    owned.sort_by_key(|observed| observed.role == Role::Child);
    let mut lines = Vec::new();
    let mut clean = true;
    for observed in owned {
        match observed.clean_up() {
            Ok(line) => lines.push(line),
            Err(line) => {
                clean = false;
                lines.push(format!("LEFT: {line}"));
            }
        }
    }
    (lines, clean)
}

#[cfg(target_os = "linux")]
mod sys {
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum State {
        Running,
        Zombie,
        Reaped,
    }

    pub struct Identity {
        pid: u32,
        pidfd: OwnedFd,
        /// `starttime` from `/proc/<pid>/stat`, read once the pidfd is open.
        start: Option<u64>,
    }

    fn start_time(pid: u32) -> Option<u64> {
        let stat = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
        let close = stat.iter().rposition(|&byte| byte == b')')?;
        let fields: Vec<&[u8]> = stat[close + 1..]
            .split(u8::is_ascii_whitespace)
            .filter(|field| !field.is_empty())
            .collect();
        // Field 22, counted from the pid; the state is field 3.
        std::str::from_utf8(fields.get(19)?).ok()?.parse().ok()
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
            let pidfd = unsafe { OwnedFd::from_raw_fd(fd) };
            Ok(Self {
                pid,
                pidfd,
                start: start_time(pid),
            })
        }

        fn exited(&self) -> bool {
            let mut poll = libc::pollfd {
                fd: self.pidfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd, no wait.
            unsafe { libc::poll(&mut poll, 1, 0) == 1 }
        }

        pub fn state(&self) -> State {
            if !self.exited() {
                return State::Running;
            }
            // Exited; a zombie keeps its `/proc` entry and start time.
            if self.start.is_some() && start_time(self.pid) == self.start {
                State::Zombie
            } else {
                State::Reaped
            }
        }

        pub fn kill(&self) {
            // SAFETY: plain syscall on our pidfd.
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

        /// Reaps it if it is this process's exited child, without waiting.
        pub fn reap_now(&self) -> bool {
            // SAFETY: a zeroed `siginfo_t` is valid output space.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: plain syscall through our pidfd; `WNOHANG` never blocks,
            // and a process that is not our child fails with `ECHILD`.
            let result = unsafe {
                libc::waitid(
                    libc::P_PIDFD,
                    self.pidfd.as_raw_fd() as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG,
                )
            };
            // SAFETY: `waitid` filled the record or left it zeroed.
            result == 0 && unsafe { info.si_pid() } != 0
        }
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use std::io;

    /// `SZOMB` from `<sys/proc.h>`.
    const SZOMB: u32 = 5;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum State {
        Running,
        Zombie,
        Reaped,
    }

    pub struct Identity {
        pid: u32,
        start: (u64, u64),
    }

    /// The process's start time and status, or `None` if no such process.
    fn info(pid: u32) -> Option<((u64, u64), u32)> {
        let raw = libc::c_int::try_from(pid).ok()?;
        // SAFETY: a zeroed struct is valid output space.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
        // SAFETY: `info` has the size passed.
        let written = unsafe {
            libc::proc_pidinfo(
                raw,
                libc::PROC_PIDTBSDINFO,
                0,
                std::ptr::addr_of_mut!(info).cast(),
                size,
            )
        };
        (written == size).then_some((
            (info.pbi_start_tvsec, info.pbi_start_tvusec),
            info.pbi_status,
        ))
    }

    impl Identity {
        pub fn open(pid: u32) -> io::Result<Self> {
            let (start, _) = info(pid).ok_or_else(|| io::Error::other("no such process"))?;
            Ok(Self { pid, start })
        }

        pub fn state(&self) -> State {
            match info(self.pid) {
                Some((start, status)) if start == self.start => {
                    if status == SZOMB {
                        State::Zombie
                    } else {
                        State::Running
                    }
                }
                _ => State::Reaped,
            }
        }

        /// Re-checks the start time, then signals; not atomic (see the module
        /// documentation).
        pub fn kill(&self) {
            if self.state() == State::Running
                && let Ok(pid) = libc::pid_t::try_from(self.pid)
            {
                // SAFETY: plain syscall on the verified pid.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }

        /// Reaps it if it is this process's exited child, without waiting.
        /// An unreaped zombie child's pid cannot be reused, so the start time
        /// checked just before names it.
        pub fn reap_now(&self) -> bool {
            let Ok(pid) = libc::pid_t::try_from(self.pid) else {
                return false;
            };
            if self.state() != State::Zombie {
                return false;
            }
            let mut status = 0;
            // SAFETY: plain syscall; `WNOHANG` never blocks, and a process
            // that is not our child fails with `ECHILD`.
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) == pid }
        }
    }
}

#[cfg(windows)]
mod sys {
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
        TerminateProcess, WaitForSingleObject,
    };

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum State {
        Running,
        /// Windows has no zombie: an exited process counts as reaped.
        Reaped,
    }

    /// A process handle, which keeps the pid from naming another process.
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

        pub fn state(&self) -> State {
            // SAFETY: a live handle; a zero timeout only checks.
            if unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) } == WAIT_TIMEOUT {
                State::Running
            } else {
                State::Reaped
            }
        }

        pub fn kill(&self) {
            // SAFETY: a live handle to the observed process.
            unsafe {
                TerminateProcess(self.handle.as_raw_handle(), 1);
            }
        }
    }
}
