//! The Linux orphan drain: Bake as its own descendants' child subreaper
//! (`PR_SET_CHILD_SUBREAPER`), and a drain that kills and reaps every
//! child left at exit. Bake's own; Pi has no counterpart.
//!
//! An owned tree is settled by its owner ([`crate::OwnedProcess`]), but a
//! command that exits normally leaves its background descendants running,
//! as Pi does, and a descendant that leaves its tree is outside every
//! owner. Once their parent has died the kernel reparents them to the
//! nearest subreaper. With [`enable`] that is this process, so
//! [`drain_adopted`] can kill and reap them when the host shuts down
//! instead of leaving them to the system's init. Opt-in: it ends processes
//! a person may have backgrounded on purpose, which only the host's caller
//! can decide.
//!
//! While enabled, a killed tree's wait reaps its group's zombies that were
//! reparented here, as it does when Bake is pid 1, and the wait for a
//! namespace init reaps that init: without it each would stay a zombie of
//! this process, and a killed tree's wait would never end.
//!
//! The drain's limits: it runs only when the host reaches it, so a host
//! killed with `SIGKILL` drains nothing, and its orphans go to the next
//! subreaper or init as before. A descendant that is not yet an orphan
//! when the drain ends (its parent still runs outside any owned tree, for
//! example under a host that calls the drain before its own owners are
//! settled) is not reached; call it last.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Makes this process the child subreaper of its descendants, so that an
/// orphaned descendant is reparented here instead of to init. Process
/// wide; it stays in effect until the process exits.
pub fn enable() -> io::Result<()> {
    // SAFETY: plain prctl with integer arguments; it only changes how this
    // process's orphaned descendants are reparented.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    ENABLED.store(true, Ordering::SeqCst);
    Ok(())
}

/// Whether [`enable`] has made this process a child subreaper.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// What [`drain_adopted`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Drained {
    /// Children killed and reaped.
    pub reaped: usize,
    /// Children still present when the bound passed.
    pub remaining: usize,
}

/// Kills (`SIGKILL`) and reaps every child of this process, again and
/// again as killed children's own children are reparented here, until
/// none is left or `bound` has passed. Blocking; call it once, after every
/// owner in this process has settled its processes and nothing else waits
/// for a child: a child still owned elsewhere is killed and reaped too.
///
/// Each child is pinned by a pidfd before it is signalled, so no signal
/// reaches a process that reused a reaped child's pid.
pub fn drain_adopted(bound: Duration) -> Drained {
    let deadline = Instant::now() + bound;
    let me = libc::pid_t::try_from(std::process::id()).unwrap_or(0);
    let mut drained = Drained::default();
    loop {
        let children = children_of(me);
        if children.is_empty() {
            drained.remaining = 0;
            return drained;
        }
        for child in &children {
            kill(*child);
            if reap(*child) {
                drained.reaped += 1;
            }
        }
        if Instant::now() >= deadline {
            drained.remaining = children_of(me).len();
            return drained;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The pids whose parent is `me`, read from `/proc`; entries that cannot
/// be read are skipped.
fn children_of(me: libc::pid_t) -> Vec<libc::pid_t> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<libc::pid_t>().ok()?;
            let stat = std::fs::read(entry.path().join("stat")).ok()?;
            (crate::tree::parent_in_stat(&stat)? == me).then_some(pid)
        })
        .collect()
}

/// Sends `SIGKILL` to the child `pid` through a pidfd, or by pid without
/// pidfds; a child is not reaped while it is this process's, so its pid
/// names it either way unless something else reaps it first, which the
/// pidfd covers.
fn kill(pid: libc::pid_t) {
    // SAFETY: plain syscall with integer arguments; it returns a new
    // descriptor this function then owns, or -1.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    match libc::c_int::try_from(fd).ok().filter(|&fd| fd >= 0) {
        Some(fd) => {
            // SAFETY: `fd` was just returned by `pidfd_open`.
            let pidfd = unsafe { OwnedFd::from_raw_fd(fd) };
            // SAFETY: plain syscall on an owned pidfd, with no `siginfo`.
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    pidfd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
        None => {
            // SAFETY: plain syscall; `pid` is an unreaped child.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
}

/// Reaps the child `pid` if it has exited; whether it did.
fn reap(pid: libc::pid_t) -> bool {
    // SAFETY: plain syscall on one child pid, with no status wanted.
    unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) == pid }
}
