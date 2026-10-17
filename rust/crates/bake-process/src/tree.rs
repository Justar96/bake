//! Process-tree ownership for spawned processes: Pi's `detached` spawn,
//! `killProcessTree`, and its detached-child tracking from
//! `src/utils/shell.ts`, with the identity and wait rules Pi's Node runtime
//! does not need.
//!
//! Unix: the command calls `setsid` before `exec`, as Node's `detached: true`
//! does, so it leads a new session and process group with no controlling
//! terminal, and `kill(-pgid, SIGKILL)` reaches every descendant that stayed
//! in the group. The group's id is the leader's pid, which the kernel cannot
//! reuse while the leader is unreaped (alive or a zombie); a tree is
//! therefore only ever signalled while it is armed, and its owner disarms it
//! ([`ProcessTree::disarm`]) before it reaps the leader. Disarming waits for
//! a signal already being sent, so no signal reaches a recycled group.
//!
//! Windows: Pi runs `taskkill /F /T /PID`. This port creates the command
//! inside a new job object (`PROC_THREAD_ATTRIBUTE_JOB_LIST` at
//! `CreateProcessW`, in [`crate::OwnedProcess::spawn`]), so the command and every
//! descendant it starts are members from their first instruction, with no
//! window in which a fast child could leave one outside. The job is
//! kill-on-close from before the command exists, so if Bake dies, or the
//! handle is closed any way but a normal detach, the tree ends with it; a
//! command whose job, limit, or completion port cannot be set up is not
//! started. It kills by terminating the job, and settles only on the
//! account in `job_ledger`: the job's active count reaches 0
//! before a terminated member's handle is signalled, so the count alone is
//! never read as the end. A command that exits normally has the limit
//! cleared ([`ProcessTree::detach`]) before the handle is closed. The job
//! is held by handle, so it has no id to reuse.
//!
//! A descendant that starts its own session (Unix) or breaks away from the
//! job (Windows) leaves the tree: it is neither killed nor awaited, as it
//! escapes Pi. The tree is cleanup, not containment; D24's sandbox is the
//! boundary.
//!
//! The only `unsafe` code is the FFI calls below, each with its contract.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

#[cfg(unix)]
use tokio::process::{Child, Command};

/// What became of a settled command's tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeState {
    /// The command exited by itself; like Pi, its descendants were left
    /// running, untracked and outside any owner's drain.
    LeftRunning,
    /// The tree was killed and no process is left in it: observed, never
    /// assumed.
    Gone,
}

/// Puts the command's process in a tree [`ProcessTree::kill`] can end:
/// its own session and process group. Unix only; Windows creates the
/// process in its job instead.
#[cfg(unix)]
pub(crate) fn configure(command: &mut Command) {
    // SAFETY: the closure runs in the forked child before `exec` and calls
    // only `setsid`, which is async-signal-safe, and reads errno.
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

/// A spawned command's whole tree.
#[derive(Debug)]
pub(crate) struct ProcessTree {
    /// The leader's pid: on Unix also the group's id; on Windows only
    /// kept for `Debug`, since the job names the tree.
    #[cfg_attr(windows, allow(dead_code, reason = "the job names a Windows tree"))]
    pid: u32,
    /// Whether the tree may be signalled: true until its owner disarms it,
    /// which it does before it reaps the leader. Held while signalling.
    armed: Mutex<bool>,
    #[cfg(windows)]
    job: std::os::windows::io::OwnedHandle,
    /// The completion port associated with the job before any process
    /// existed, and the account of its new-process messages; the account
    /// lives with the tree, so a cancelled settle loses nothing.
    #[cfg(windows)]
    account: windows::Account,
}

impl ProcessTree {
    /// Adopts a just-spawned, [`configure`]d child; `None` when it has no
    /// pid (it already exited and was reaped).
    #[cfg(unix)]
    pub(crate) fn adopt(child: &Child) -> Option<Self> {
        let pid = child.id()?;
        Some(Self {
            pid,
            armed: Mutex::new(true),
        })
    }

    /// The tree of a process created inside `job`, whose completion `port`
    /// was associated while the job was empty.
    #[cfg(windows)]
    pub(crate) fn with_job(
        pid: u32,
        job: std::os::windows::io::OwnedHandle,
        port: std::os::windows::io::OwnedHandle,
    ) -> Self {
        Self {
            pid,
            armed: Mutex::new(true),
            job,
            account: windows::Account::new(port),
        }
    }

    /// The leader's pid as adopted.
    #[cfg(unix)]
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    /// Kills every process in the tree if it is still armed. The call's
    /// result is not read: only [`Self::gone`] establishes that the tree
    /// has ended.
    pub(crate) fn kill(&self) {
        let armed = self.armed.lock().unwrap_or_else(PoisonError::into_inner);
        if !*armed {
            return;
        }
        #[cfg(unix)]
        self.signal_group(libc::SIGKILL);
        #[cfg(windows)]
        windows::terminate(&self.job);
        drop(armed);
    }

    /// Sends `SIGTERM` to the group if the tree is still armed (Unix); a
    /// request only. Windows has no such request, so nothing is sent.
    pub(crate) fn terminate(&self) {
        let armed = self.armed.lock().unwrap_or_else(PoisonError::into_inner);
        if !*armed {
            return;
        }
        #[cfg(unix)]
        self.signal_group(libc::SIGTERM);
        drop(armed);
    }

    /// Signals the group; the caller holds the armed lock.
    #[cfg(unix)]
    fn signal_group(&self, signal: libc::c_int) {
        // `kill(-1)` and `kill(0)` would signal this process's own group
        // or every process the user owns.
        let Ok(pgid) = libc::pid_t::try_from(self.pid) else {
            return;
        };
        if pgid > 1 {
            // SAFETY: plain syscall with integer arguments. The group id
            // is the unreaped leader's pid (the tree is armed).
            unsafe {
                libc::kill(-pgid, signal);
            }
        }
    }

    /// Releases a normally exited command's descendants from the tree's
    /// lifetime before the tree is dropped. Unix: nothing ends with the
    /// group, so nothing to do. Windows: clears the job's kill-on-close
    /// limit, which otherwise ends them when the handle closes; on failure
    /// the limit stands, and the caller still owns the tree.
    pub(crate) fn detach(&self) -> std::io::Result<()> {
        #[cfg(windows)]
        {
            set_kill_on_close(&self.job, false)?;
            // Nothing will drain the port again: stop the job queueing to
            // it. Failing leaves messages queued until the job ends.
            let _ = windows::dissociate_port(&self.job);
            Ok(())
        }
        #[cfg(not(windows))]
        {
            Ok(())
        }
    }

    /// Stops all later signalling, waiting for one being sent. Call before
    /// the leader is reaped, after which its id may name another group.
    pub(crate) fn disarm(&self) {
        *self.armed.lock().unwrap_or_else(PoisonError::into_inner) = false;
    }

    /// Waits until no process other than the leader is left in the killed
    /// tree, however long that takes; the caller must hold the leader
    /// unreaped (Unix), which pins the group's id, and must have waited
    /// for the leader's exit. Polls with a backoff from 1 ms
    /// to 20 ms.
    ///
    /// Linux reads each process's group from `/proc`. When this process is
    /// pid 1 (a container without an init), it reaps the group's zombies
    /// that were reparented to it, as an init would; otherwise their own
    /// parent or init reaps them. macOS asks the kernel for the group's
    /// members (`proc_listpids`). Windows settles on the job's account
    /// (`job_ledger`): every process the job reported as added
    /// is held by handle until signalled, and their number must equal the
    /// job's lifetime count; while it is not settled the job is terminated
    /// again on each poll, for a process created as it was terminated. A
    /// lost message, an unknown member, or a contradiction never settles,
    /// so the wait goes on. A query that fails, or a process whose membership
    /// cannot be read, counts as a member still there: the wait goes on
    /// and is retried, so a failure never reads as an empty tree.
    ///
    /// Linux limit: with `/proc` mounted `hidepid`, another user's process
    /// (a set-user-id descendant) is not listed at all, so it is not seen.
    pub(crate) async fn gone(&self) {
        let mut pause = Duration::from_millis(1);
        loop {
            if self.members() == Ok(0) {
                return;
            }
            #[cfg(windows)]
            self.kill();
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(Duration::from_millis(20));
        }
    }

    /// One bounded step of the live pump ([`crate::pump`]).
    #[cfg(windows)]
    pub(crate) fn pump_step(&self) -> Result<crate::pump::Step, ()> {
        self.account.pump(&self.job)
    }

    /// The job account's distinct members and those still held or unknown.
    #[cfg(all(test, windows))]
    pub(crate) fn account_snapshot(&self) -> (u64, usize) {
        self.account.snapshot()
    }

    /// How many processes other than the leader are in the tree; an error
    /// when that cannot be established.
    fn members(&self) -> Result<usize, &'static str> {
        #[cfg(target_os = "linux")]
        {
            let Ok(pgid) = i32::try_from(self.pid) else {
                return Err("the process group id is out of range");
            };
            linux::members(pgid).map_err(|_| "/proc could not be read")
        }
        #[cfg(target_os = "macos")]
        {
            let Ok(pgid) = i32::try_from(self.pid) else {
                return Err("the process group id is out of range");
            };
            macos::members(pgid).map_err(|_| "proc_listpids failed")
        }
        #[cfg(windows)]
        {
            // The leader is a member too: it is waited for before this is
            // asked, so it is never what remains.
            match self.account.settle(&self.job) {
                crate::job_ledger::Verdict::Settled => Ok(0),
                crate::job_ledger::Verdict::Pending(_) => Ok(1),
                crate::job_ledger::Verdict::Contradiction(why) => Err(why),
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            compile_error!("owned process trees need a way to list a tree's members");
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io;

    /// Members of group `pgid` other than its leader (`pgid` itself). As
    /// pid 1, reaps the group's zombies that are this process's children.
    pub(super) fn members(pgid: i32) -> io::Result<usize> {
        let me = libc::pid_t::try_from(std::process::id()).unwrap_or(0);
        members_in(std::path::Path::new("/proc"), pgid, me, me == 1)
    }

    /// The fields of a `/proc/<pid>/stat` line this module reads.
    #[derive(Debug, PartialEq, Eq)]
    pub(super) struct Stat {
        pub(super) state: u8,
        pub(super) ppid: i32,
        pub(super) pgrp: i32,
    }

    /// Parses `pid (comm) state ppid pgrp ...` as bytes. The command name
    /// is any bytes, `)` and invalid UTF-8 included, so only the ASCII
    /// fields after its last `)` are read. `None` when any of the three is
    /// missing or malformed: membership is then unknown.
    pub(super) fn parse_stat(stat: &[u8]) -> Option<Stat> {
        let close = stat.iter().rposition(|&byte| byte == b')')?;
        let mut fields = stat[close + 1..]
            .split(u8::is_ascii_whitespace)
            .filter(|field| !field.is_empty());
        let state = match fields.next()? {
            [state] if state.is_ascii_alphabetic() => *state,
            _ => return None,
        };
        let number = |field: &[u8]| -> Option<i32> {
            if field.is_empty() || !field.iter().all(u8::is_ascii_digit) {
                return None;
            }
            std::str::from_utf8(field).ok()?.parse().ok()
        };
        let ppid = number(fields.next()?)?;
        let pgrp = number(fields.next()?)?;
        Some(Stat { state, ppid, pgrp })
    }

    /// [`members`] over the process table at `root`.
    pub(super) fn members_in(
        root: &std::path::Path,
        pgid: i32,
        me: i32,
        reap_orphans: bool,
    ) -> io::Result<usize> {
        let mut count = 0;
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<i32>().ok())
            else {
                continue;
            };
            if pid == pgid {
                continue;
            }
            let stat = match std::fs::read(entry.path().join("stat")) {
                Ok(stat) => stat,
                // The process ended, and was reaped, since the listing.
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        || error.raw_os_error() == Some(libc::ESRCH) =>
                {
                    continue;
                }
                // Its membership is unknown: report that, not an absence.
                Err(error) => return Err(error),
            };
            let Some(Stat { state, ppid, pgrp }) = parse_stat(&stat) else {
                // Its membership is unknown: report that, not an absence.
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unreadable /proc stat",
                ));
            };
            if pgrp != pgid {
                continue;
            }
            if reap_orphans && state == b'Z' && ppid == me {
                // SAFETY: plain syscall on a zombie child of this process
                // in the pinned group, which is not the leader; nothing
                // else waits for it.
                let reaped = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
                if reaped == pid {
                    continue;
                }
            }
            count += 1;
        }
        Ok(count)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn table() -> crate::test_support::TempDir {
            let root = crate::test_support::TempDir::new();
            let write = |pid: &str, stat: &str| {
                std::fs::create_dir(root.join(pid)).expect("dir");
                std::fs::write(root.join(pid).join("stat"), stat).expect("stat");
            };
            // The leader (skipped), a member whose name holds `)`, and a
            // process of another group.
            write("100", "100 (sh) Z 1 100 100 0");
            write("101", "101 (a) b) S 100 100 100 0");
            write("102", "102 (other) S 1 7 7 0");
            // Not a process.
            std::fs::create_dir(root.join("self-ish")).expect("dir");
            root
        }

        #[test]
        fn counts_members_other_than_the_leader() {
            let root = table();
            assert_eq!(members_in(root.path(), 100, 1, false).ok(), Some(1));
            assert_eq!(members_in(root.path(), 7, 1, false).ok(), Some(1));
            assert_eq!(members_in(root.path(), 55, 1, false).ok(), Some(0));
        }

        // A process that vanished between the listing and its stat is not
        // a member.
        #[test]
        fn a_vanished_process_is_skipped() {
            let root = table();
            std::fs::create_dir(root.join("103")).expect("dir");
            assert_eq!(members_in(root.path(), 100, 1, false).ok(), Some(1));
        }

        // Review finding: a process whose membership cannot be read is an
        // error, not an absence, so the wait goes on rather than reporting
        // an empty tree.
        #[test]
        fn an_unreadable_process_is_an_error_not_an_absence() {
            use std::os::unix::fs::PermissionsExt;
            let root = table();
            std::fs::create_dir(root.join("104")).expect("dir");
            std::fs::write(root.join("104").join("stat"), "104 (x) S 1 100 100 0").expect("stat");
            std::fs::set_permissions(
                root.join("104").join("stat"),
                std::fs::Permissions::from_mode(0o000),
            )
            .expect("chmod");
            let readable = std::fs::read_to_string(root.join("104").join("stat")).is_ok();
            let result = members_in(root.path(), 55, 1, false);
            std::fs::set_permissions(
                root.join("104").join("stat"),
                std::fs::Permissions::from_mode(0o600),
            )
            .expect("chmod back");
            if readable {
                // Running as root: permissions are not enforced, so this
                // host cannot produce the case.
                eprintln!("skipped: an unreadable file is readable here (root)");
                return;
            }
            assert!(result.is_err(), "unknown membership read as {result:?}");
        }

        // A malformed stat is also unknown membership.
        #[test]
        fn a_malformed_stat_is_an_error() {
            let root = table();
            std::fs::create_dir(root.join("105")).expect("dir");
            std::fs::write(root.join("105").join("stat"), "105 no parenthesis").expect("stat");
            assert!(members_in(root.path(), 55, 1, false).is_err());
        }

        // A command name is bytes, not text: a process whose name is not
        // UTF-8 is still read, so an unrelated one does not hold the wait
        // and a member is counted.
        #[test]
        fn a_non_utf8_command_name_is_read() {
            let root = table();
            for (pid, pgrp) in [("106", "7"), ("107", "55")] {
                std::fs::create_dir(root.join(pid)).expect("dir");
                let mut stat = format!("{pid} (").into_bytes();
                stat.extend_from_slice(b"bad\xff\xfe)name");
                stat.extend_from_slice(format!(") S 1 {pgrp} {pgrp} 0").as_bytes());
                assert!(std::str::from_utf8(&stat).is_err());
                std::fs::write(root.join(pid).join("stat"), stat).expect("stat");
            }
            // Group 100 has its one member (101); the unrelated 106 and
            // 107 neither error nor count.
            assert_eq!(members_in(root.path(), 100, 1, false).ok(), Some(1));
            // Group 55's only member is the non-UTF-8 107.
            assert_eq!(members_in(root.path(), 55, 1, false).ok(), Some(1));
        }

        // The fields after the command name must be well formed, in an
        // unrelated process too: one that cannot be read is unknown, not
        // outside the group.
        #[test]
        fn malformed_group_fields_are_errors() {
            for fields in [
                ") S 1 x7 7 0",
                ") S 1",
                ") S x 7 7 0",
                ") SS 1 7 7 0",
                ") S 1 -7 7 0",
                ")",
            ] {
                let root = table();
                std::fs::create_dir(root.join("108")).expect("dir");
                std::fs::write(root.join("108").join("stat"), format!("108 (x{fields}"))
                    .expect("stat");
                assert!(
                    members_in(root.path(), 100, 1, false).is_err(),
                    "{fields:?} read as known"
                );
            }
        }

        #[test]
        fn parse_stat_reads_only_the_ascii_fields() {
            assert_eq!(
                parse_stat(b"9 (a) b\xff) Z 4 5 5 0"),
                Some(Stat {
                    state: b'Z',
                    ppid: 4,
                    pgrp: 5
                })
            );
            assert_eq!(parse_stat(b"9 a Z 4 5"), None);
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::io;

    /// `PROC_PGRP_ONLY` from `<sys/proc_info.h>`.
    const PROC_PGRP_ONLY: u32 = 2;
    const CAPACITY: usize = 1024;

    /// Members of group `pgid` other than its leader, zombies included.
    pub(super) fn members(pgid: i32) -> io::Result<usize> {
        let mut pids = vec![0 as libc::pid_t; CAPACITY];
        let bytes = libc::c_int::try_from(CAPACITY * size_of::<libc::pid_t>())
            .map_err(|_| io::Error::other("buffer size"))?;
        let group = u32::try_from(pgid).map_err(|_| io::Error::other("pgid"))?;
        // SAFETY: `pids` holds `bytes` bytes of writable space; the call
        // fills at most that and returns the bytes it wrote.
        let written =
            unsafe { libc::proc_listpids(PROC_PGRP_ONLY, group, pids.as_mut_ptr().cast(), bytes) };
        super::classify_group_listing(written, &pids, pgid)
            .ok_or_else(|| io::Error::other("proc_listpids failed or omitted the leader"))
    }
}

#[cfg(unix)]
/// Reads a group listing of `proc_listpids` (`written` bytes into `pids`)
/// as the number of members other than `leader`, or `None` when it cannot
/// be trusted. libproc returns 0 both for a failed call (with `errno` set)
/// and for an empty group, but the caller holds the leader unreaped, so a
/// true listing always names it: 0 bytes, or a listing without the leader,
/// is a failure, never an empty group. A full buffer counts at least the
/// members it holds, which is enough to know the group is not empty.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "macOS reads it; tested everywhere")
)]
fn classify_group_listing(
    written: libc::c_int,
    pids: &[libc::pid_t],
    leader: libc::pid_t,
) -> Option<usize> {
    let written = usize::try_from(written)
        .ok()
        .filter(|&written| written > 0)?;
    let listed = &pids[..(written / size_of::<libc::pid_t>()).min(pids.len())];
    if !listed.contains(&leader) {
        return None;
    }
    Some(
        listed
            .iter()
            .filter(|&&pid| pid != 0 && pid != leader)
            .count(),
    )
}

#[cfg(all(test, unix))]
mod listing_tests {
    use super::classify_group_listing;

    const PID: usize = size_of::<libc::pid_t>();

    #[test]
    fn a_zero_return_is_a_failure_not_an_empty_group() {
        assert_eq!(classify_group_listing(0, &[0; 4], 100), None);
        assert_eq!(classify_group_listing(-1, &[0; 4], 100), None);
    }

    #[test]
    fn a_listing_without_the_pinned_leader_is_a_failure() {
        let pids = [7, 8, 0, 0];
        assert_eq!(
            classify_group_listing((2 * PID) as libc::c_int, &pids, 100),
            None
        );
    }

    #[test]
    fn members_other_than_the_leader_are_counted() {
        let pids = [100, 0, 0, 0];
        assert_eq!(
            classify_group_listing(PID as libc::c_int, &pids, 100),
            Some(0)
        );
        let pids = [101, 100, 102, 0];
        assert_eq!(
            classify_group_listing((3 * PID) as libc::c_int, &pids, 100),
            Some(2)
        );
        // Bytes past the buffer are not read.
        let pids = [100, 101];
        assert_eq!(
            classify_group_listing((8 * PID) as libc::c_int, &pids, 100),
            Some(1)
        );
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
/// `untrackDetachedChildPid`. The owner disarms the tree before it reaps the
/// leader, so a tree still listed here is never signalled after that.
pub(crate) struct TrackedTree {
    id: u64,
    tree: Arc<ProcessTree>,
}

impl TrackedTree {
    pub(crate) fn new(tree: ProcessTree) -> Self {
        let tree = Arc::new(tree);
        let mut tracked = TRACKED.lock().unwrap_or_else(PoisonError::into_inner);
        let id = tracked.next;
        tracked.next = tracked.next.wrapping_add(1);
        tracked.trees.push((id, Arc::clone(&tree)));
        Self { id, tree }
    }

    pub(crate) fn kill(&self) {
        self.tree.kill();
    }

    /// A reference to the tree that does not keep it, or its job, alive:
    /// what the live pump holds between steps.
    #[cfg(windows)]
    pub(crate) fn downgrade(&self) -> std::sync::Weak<ProcessTree> {
        Arc::downgrade(&self.tree)
    }

    /// The tree itself, for the crate's own tests.
    #[cfg(all(test, windows))]
    pub(crate) fn tree(&self) -> &ProcessTree {
        &self.tree
    }

    /// [`ProcessTree::terminate`].
    pub(crate) fn terminate(&self) {
        self.tree.terminate();
    }

    /// [`ProcessTree::detach`].
    pub(crate) fn detach(&self) -> std::io::Result<()> {
        self.tree.detach()
    }

    /// [`ProcessTree::disarm`].
    pub(crate) fn disarm(&self) {
        self.tree.disarm();
    }

    /// [`ProcessTree::gone`].
    pub(crate) async fn gone(&self) {
        self.tree.gone().await;
    }
}

impl Drop for TrackedTree {
    fn drop(&mut self) {
        // Untracked only once disarmed: a guard dropped some other way
        // must not leave an armed tree its owner no longer pins.
        self.tree.disarm();
        TRACKED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .trees
            .retain(|(id, _)| *id != self.id);
    }
}

/// Pi's `killTrackedDetachedChildren`: kills the tree of every owned
/// process still running (tool commands and MCP servers alike), for a
/// shutdown that cannot await each owner (Pi runs it on `SIGHUP` and
/// `SIGTERM`). Returns at once and awaits nothing; each owner still waits
/// for its own tree. A tree whose owner has disarmed it,
/// because its command exited normally or it is about to reap the leader,
/// is not signalled; neither are the descendants a normally exited command
/// left running.
pub fn kill_tracked_detached_children() {
    let trees = std::mem::take(&mut TRACKED.lock().unwrap_or_else(PoisonError::into_inner).trees);
    for (_, tree) in trees {
        tree.kill();
    }
}

#[cfg(windows)]
pub(crate) use windows::{associate_port, completion_port, set_kill_on_close};

#[cfg(windows)]
mod windows {
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::sync::{Mutex, PoisonError};

    use windows_sys::Win32::Foundation::{
        ERROR_INVALID_PARAMETER, FILETIME, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::IO::{
        CreateIoCompletionPort, GetQueuedCompletionStatusEx, OVERLAPPED_ENTRY,
    };
    use windows_sys::Win32::System::JobObjects::{
        IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_ASSOCIATE_COMPLETION_PORT,
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectAssociateCompletionPortInformation, JobObjectBasicAccountingInformation,
        JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
        TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        WaitForSingleObject,
    };

    use crate::job_ledger::{Host, Ledger, Opened, Verdict};

    /// `JOB_OBJECT_MSG_NEW_PROCESS` from `winnt.h`.
    const JOB_OBJECT_MSG_NEW_PROCESS: u32 = 6;
    /// The key the job's messages carry; the port serves only this job.
    const KEY: usize = 1;
    /// Messages taken per call, and per settle poll: a backlog left from a
    /// long life is drained over several polls, never in one unbounded one.
    const BATCH: usize = 64;
    const PER_POLL: usize = 64 * BATCH;
    /// Messages one live pump step takes before it yields.
    const PER_STEP: usize = 4 * BATCH;

    /// A completion port for one job: not inheritable, never shared, so
    /// only the job posts to it.
    pub(crate) fn completion_port() -> io::Result<OwnedHandle> {
        // SAFETY: a new port, not tied to a file.
        let port =
            unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 1) };
        if port.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh handle owned here.
        Ok(unsafe { OwnedHandle::from_raw_handle(port) })
    }

    /// Associates `port` with `job`. The caller does so while the job has
    /// never had a process, so every association reports to the port.
    pub(crate) fn associate_port(job: &OwnedHandle, port: &OwnedHandle) -> io::Result<()> {
        set_port(job, port.as_raw_handle())
    }

    /// Removes the job's port association (Windows 8 and later).
    pub(super) fn dissociate_port(job: &OwnedHandle) -> io::Result<()> {
        set_port(job, std::ptr::null_mut())
    }

    fn set_port(job: &OwnedHandle, port: windows_sys::Win32::Foundation::HANDLE) -> io::Result<()> {
        let info = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
            CompletionKey: KEY as *mut core::ffi::c_void,
            CompletionPort: port,
        };
        let size = u32::try_from(size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>())
            .map_err(|_| io::Error::other("size"))?;
        // SAFETY: `info` has the size passed; both handles are live.
        let ok = unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectAssociateCompletionPortInformation,
                std::ptr::addr_of!(info).cast(),
                size,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The port and the account of what it reported.
    #[derive(Debug)]
    pub(super) struct Account {
        port: OwnedHandle,
        ledger: Mutex<Ledger<OwnedHandle>>,
    }

    impl Account {
        pub(super) fn new(port: OwnedHandle) -> Self {
            Self {
                port,
                ledger: Mutex::new(Ledger::default()),
            }
        }

        /// One settle poll: drains a bounded batch of messages, reads the
        /// lifetime count, checks every member, and reads it again.
        pub(super) fn settle(&self, job: &OwnedHandle) -> Verdict {
            let mut ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
            let mut host = System { job };
            match self.drain(&mut ledger, &mut host, PER_POLL) {
                Ok((_, true)) => {}
                Ok((_, false)) => return Verdict::Pending("messages are still queued"),
                Err(()) => return Verdict::Pending("the completion port could not be read"),
            }
            ledger.retry(&mut host);
            let Ok((before, _)) = accounting(job) else {
                return Verdict::Pending("the job could not be queried");
            };
            // Messages for associations up to `before` may still arrive;
            // a short account is pending, never settled.
            match self.drain(&mut ledger, &mut host, PER_POLL) {
                Ok((_, true)) => {}
                Ok((_, false)) => return Verdict::Pending("messages are still queued"),
                Err(()) => return Verdict::Pending("the completion port could not be read"),
            }
            let waiting = ledger.check(&mut host);
            let Ok((after, active)) = accounting(job) else {
                return Verdict::Pending("the job could not be queried");
            };
            ledger.verdict(before, after, active, waiting)
        }

        /// One step of the live pump ([`crate::pump`]): takes at most
        /// [`PER_STEP`] queued messages into the account, retries unknown
        /// members, and closes the handles of members now signalled. Reads
        /// no verdict. `Ok(Step)` says how much it took and whether the
        /// queue was seen empty; a port that cannot be read is `Err`, which
        /// changes nothing: settling still drains and fails closed.
        pub(super) fn pump(&self, job: &OwnedHandle) -> Result<crate::pump::Step, ()> {
            let mut ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
            let mut host = System { job };
            let (taken, empty) = self.drain(&mut ledger, &mut host, PER_STEP)?;
            ledger.retry(&mut host);
            // An unreadable handle stays held; settling reports it.
            let _ = ledger.check(&mut host);
            Ok(crate::pump::Step { taken, empty })
        }

        /// The account's distinct members and those still open.
        #[cfg(test)]
        pub(super) fn snapshot(&self) -> (u64, usize) {
            let ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
            (ledger.counted(), ledger.open_members())
        }

        /// Records the queued new-process messages, at most `budget`
        /// rounded up to a whole batch; returns how many messages were
        /// taken and whether the queue was seen empty.
        fn drain(
            &self,
            ledger: &mut Ledger<OwnedHandle>,
            host: &mut System<'_>,
            budget: usize,
        ) -> Result<(usize, bool), ()> {
            let mut taken = 0;
            while taken < budget {
                let mut entries = [OVERLAPPED_ENTRY::default(); BATCH];
                let mut removed = 0u32;
                // SAFETY: `entries` holds `BATCH` entries; a zero timeout
                // only takes what is queued.
                let ok = unsafe {
                    GetQueuedCompletionStatusEx(
                        self.port.as_raw_handle(),
                        entries.as_mut_ptr(),
                        BATCH as u32,
                        &mut removed,
                        0,
                        0,
                    )
                };
                if ok == 0 {
                    return if io::Error::last_os_error().raw_os_error() == Some(WAIT_TIMEOUT as i32)
                    {
                        Ok((taken, true))
                    } else {
                        Err(())
                    };
                }
                let removed = (removed as usize).min(BATCH);
                for entry in &entries[..removed] {
                    if entry.lpCompletionKey != KEY {
                        ledger.contradict("a message on the job's port carried another key");
                        continue;
                    }
                    if entry.dwNumberOfBytesTransferred != JOB_OBJECT_MSG_NEW_PROCESS {
                        continue;
                    }
                    match u32::try_from(entry.lpOverlapped as usize) {
                        Ok(pid) => ledger.reported(pid, host),
                        Err(_) => ledger.contradict("a new-process message's pid is out of range"),
                    }
                }
                taken += removed;
                if removed < BATCH {
                    return Ok((taken, true));
                }
            }
            Ok((taken, false))
        }
    }

    /// The system, for the ledger.
    struct System<'a> {
        job: &'a OwnedHandle,
    }

    impl Host for System<'_> {
        type Handle = OwnedHandle;

        fn open(&mut self, pid: u32) -> Opened<OwnedHandle> {
            // SAFETY: plain call; a null handle is an error.
            let raw = unsafe {
                OpenProcess(
                    PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                    0,
                    pid,
                )
            };
            if raw.is_null() {
                return if io::Error::last_os_error().raw_os_error()
                    == Some(ERROR_INVALID_PARAMETER as i32)
                {
                    Opened::NoSuchProcess
                } else {
                    Opened::Unknown
                };
            }
            // SAFETY: a fresh handle owned here.
            let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
            let mut member = 0;
            // SAFETY: both handles are live.
            if unsafe {
                IsProcessInJob(
                    handle.as_raw_handle(),
                    self.job.as_raw_handle(),
                    &mut member,
                )
            } == 0
            {
                return Opened::Unknown;
            }
            if member == 0 {
                return Opened::OtherProcess;
            }
            let mut times = [FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            }; 4];
            let [created, exited, kernel, user] = &mut times;
            // A failed query leaves a member held, without a creation time;
            // the failure never reads as an exit.
            // SAFETY: a live handle with query access and valid out pointers.
            let timed =
                unsafe { GetProcessTimes(handle.as_raw_handle(), created, exited, kernel, user) }
                    != 0;
            let created = timed.then(|| {
                (u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime)
            });
            Opened::Member { created, handle }
        }

        fn signalled(&mut self, handle: &OwnedHandle) -> Option<bool> {
            // SAFETY: a live handle opened with SYNCHRONIZE; a zero timeout
            // only checks.
            match unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } {
                WAIT_OBJECT_0 => Some(true),
                WAIT_TIMEOUT => Some(false),
                _ => None,
            }
        }
    }

    /// Sets or clears the job's kill-on-close limit, the only limit Bake
    /// sets on it.
    pub(crate) fn set_kill_on_close(job: &OwnedHandle, on: bool) -> io::Result<()> {
        // SAFETY: a zeroed struct is a valid starting value.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        if on {
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        }
        let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
            .map_err(|_| io::Error::other("size"))?;
        // SAFETY: `limits` has the size passed; the job handle is live.
        let ok = unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(limits).cast(),
                size,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Terminates every process in the job with exit code 1, as
    /// `taskkill /F` leaves. The result is not read; only the job's account
    /// (`job_ledger`) says when its members have ended.
    pub(super) fn terminate(job: &OwnedHandle) {
        // SAFETY: `job` is a valid job handle owned by the tree.
        unsafe {
            TerminateJobObject(job.as_raw_handle(), 1);
        }
    }

    /// The job's `(TotalProcesses, ActiveProcesses)`: every process ever
    /// associated, failed associations included, and those still active.
    fn accounting(job: &OwnedHandle) -> io::Result<(u32, u32)> {
        // SAFETY: a zeroed struct is valid output space.
        let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        let size = u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>())
            .map_err(|_| io::Error::other("size"))?;
        // SAFETY: `info` has the size passed; the job handle is live.
        let ok = unsafe {
            QueryInformationJobObject(
                job.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                std::ptr::addr_of_mut!(info).cast(),
                size,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((info.TotalProcesses, info.ActiveProcesses))
    }
}
