//! Bake's owner of a spawned child and its whole process tree, shared by the
//! coding tools (`bake-coding-agent`: bash, direct exec, grep, find) and the
//! MCP stdio servers (`bake-mcp`). Adapted from the process handling of Pi's
//! `packages/coding-agent` (`src/utils/shell.ts`'s detached spawn,
//! `killProcessTree`, and detached-child tracking; v1.1.0, unchanged through
//! v1.3.0, `71023aec1546d6d677a65669c5665c0927ce0dd0`; MIT, see `NOTICE`),
//! with the identity and wait rules Pi's Node runtime does not need.
//!
//! [`OwnedProcess`] is the one owner. Its rules:
//!
//! - **Admission.** The caller takes its owner's admission (a [`Claim`])
//!   before [`OwnedProcess::spawn`], so a closing owner refuses before
//!   anything is created. The claim is held until the process is settled.
//!   The platform spawn suspends only before the process exists, and the
//!   owner is built in the same poll as the creation, so a dropped spawn
//!   has created nothing and a created process always has its owner.
//! - **Identity.** The leader is not reaped until its owner settles it: its
//!   exit is observed without reaping (`waitid` with `WNOWAIT`), so its pid,
//!   which is its process group's id, cannot be reused while the tree may
//!   still be signalled. The tree is disarmed before the leader is reaped. A
//!   leader reaped by anything else (`ECHILD`) loses its identity: the tree
//!   is disarmed for good and nothing is signalled or awaited again.
//! - **Two exit policies, chosen by the caller.** A process that exited by
//!   itself and was never [`kill`](OwnedProcess::kill)ed is *detached* when
//!   settled ([`TreeState::LeftRunning`]), as Pi leaves a command's
//!   background children: the Windows job's kill-on-close limit is cleared
//!   first (if that fails, settling fails and the still-owned tree is killed
//!   as on a drop), then the tree is released. A process that was killed,
//!   before or after its exit, settles only once the leader has exited and
//!   no process is observed left in its tree ([`TreeState::Gone`]), with no
//!   time limit of its own. A sent kill is never read as the tree's end, and
//!   a membership query that fails is retried, never read as empty. On
//!   Windows the tree's end is an account of the job's new-process messages
//!   against its lifetime count (`job_ledger`), since the job's active count
//!   reaches 0 before its terminated members' handles are signalled; an
//!   account that falls short never settles.
//! - **Drop.** Dropping an unsettled process kills its tree at once. With a
//!   claim, the process and its claim move to a task on the runtime it was
//!   spawned on, which settles it, so the claim's owner waits for that
//!   task; without one the drop only kills, the leader is reaped by Tokio
//!   (Unix) or its handle closed (Windows), and the tree is not awaited.
//!
//! Unix trees are a new session and process group (`setsid` before `exec`);
//! Windows trees are a kill-on-close job the process is born in
//! (`PROC_THREAD_ATTRIBUTE_JOB_LIST`), with only its standard handles
//! inherited and overlapped named pipes connected before creation. A
//! descendant that leaves the tree (`setsid`, a job breakaway) is outside
//! this ownership: the tree is cleanup, not containment, and D24's sandbox
//! is the boundary.
#![warn(missing_docs)]

use std::any::Any;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;

#[cfg(unix)]
use tokio::process::Command;
use tokio::runtime::Handle;

#[cfg(unix)]
use crate::tree::configure;
use crate::tree::{ProcessTree, TrackedTree};

pub use crate::tree::{TreeState, kill_tracked_detached_children};

mod command_line;
#[cfg(any(windows, test))]
mod job_ledger;
mod tree;

#[cfg(all(test, target_os = "linux"))]
mod test_support {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A private directory under the system temp directory, created
    /// exclusively and removed on drop.
    pub(crate) struct TempDir(PathBuf);

    impl TempDir {
        pub(crate) fn new() -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos());
            let dir = std::env::temp_dir().join(format!(
                "bake-process-{}-{}-{nanos}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst),
            ));
            std::fs::create_dir(&dir).expect("create temp dir");
            Self(dir)
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }

        pub(crate) fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// An owner's admission, held by the process until it is settled and moved
/// with it into the settling task a drop starts. Dropping the claim is how
/// the owner learns its process is gone.
pub type Claim = Box<dyn Any + Send>;

/// A settled process: the leader's status and what became of its tree.
#[derive(Debug)]
pub struct Settled {
    /// The leader's exit status.
    pub status: ExitStatus,
    /// What became of the rest of its tree.
    pub tree: TreeState,
}

/// What to spawn: the narrow set of options every consumer uses, so that
/// each platform can create the process inside its tree atomically (on
/// Windows, born in its job; Tokio's `Command` cannot ask for that).
/// Standard output and standard error are always piped. The program runs
/// directly, never through a shell.
#[derive(Debug, Clone)]
pub struct ProcessSpec {
    /// The program. On Windows a bare name is looked up on the child's
    /// `PATH` (with `.exe` added when it has no extension), never in the
    /// current directory, and batch files are refused.
    pub program: OsString,
    /// The arguments, passed literally.
    pub args: Vec<OsString>,
    /// The working directory; `None` keeps this process's.
    pub cwd: Option<PathBuf>,
    /// The exact environment, later entries winning; `None` inherits this
    /// process's.
    pub env: Option<Vec<(OsString, OsString)>>,
    /// Pipe standard input; otherwise it reads the null device.
    pub stdin: bool,
}

/// The parent's end of the child's standard input.
#[cfg(unix)]
pub type ProcessStdin = tokio::process::ChildStdin;
/// The parent's end of the child's standard output.
#[cfg(unix)]
pub type ProcessStdout = tokio::process::ChildStdout;
/// The parent's end of the child's standard error.
#[cfg(unix)]
pub type ProcessStderr = tokio::process::ChildStderr;
/// The parent's end of the child's standard input.
#[cfg(windows)]
pub type ProcessStdin = tokio::net::windows::named_pipe::NamedPipeServer;
/// The parent's end of the child's standard output.
#[cfg(windows)]
pub type ProcessStdout = tokio::net::windows::named_pipe::NamedPipeServer;
/// The parent's end of the child's standard error.
#[cfg(windows)]
pub type ProcessStderr = tokio::net::windows::named_pipe::NamedPipeServer;

#[cfg(unix)]
use self::unix::Leader;
#[cfg(windows)]
use self::windows::Leader;

/// A spawned child in a tree of its own, owned until it is settled.
pub struct OwnedProcess {
    /// The leader's pid as spawned.
    pid: u32,
    /// `None` once settled or dropped.
    leader: Option<Leader>,
    tree: Option<TrackedTree>,
    stdin: Option<ProcessStdin>,
    stdout: Option<ProcessStdout>,
    stderr: Option<ProcessStderr>,
    killed: bool,
    /// The leader's exit was observed; it is unreaped until settled.
    exited: bool,
    /// The owner's admission, released once the process is settled.
    claim: Option<Claim>,
    runtime: Handle,
    /// Set on the copy a drop moves to its settling task, whose own drop
    /// must not hand it off again.
    handed_off: bool,
}

impl std::fmt::Debug for OwnedProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnedProcess")
            .field("pid", &self.pid)
            .field("settled", &self.leader.is_none())
            .field("killed", &self.killed)
            .field("exited", &self.exited)
            .finish_non_exhaustive()
    }
}

impl OwnedProcess {
    /// Spawns `spec` as the leader of a new process tree, holding `claim`
    /// until it is settled. Fails, with nothing spawned, outside a Tokio
    /// runtime. A spawn failure leaves nothing running: on Unix the command
    /// leads a new session from its first instruction (`setsid` before
    /// `exec`), and on Windows it is created inside a new job by
    /// `PROC_THREAD_ATTRIBUTE_JOB_LIST`, or not at all.
    pub async fn spawn(spec: &ProcessSpec, claim: Option<Claim>) -> io::Result<Self> {
        let runtime = Handle::try_current()
            .map_err(|_| io::Error::other("an owned process needs a Tokio runtime"))?;
        let spawned = spawn_leader(spec).await?;
        Ok(Self {
            pid: spawned.pid,
            leader: Some(spawned.leader),
            tree: Some(TrackedTree::new(spawned.tree)),
            stdin: spawned.stdin,
            stdout: spawned.stdout,
            stderr: spawned.stderr,
            killed: false,
            exited: false,
            claim,
            runtime,
            handed_off: false,
        })
    }

    /// The leader's process id, as spawned. While the process is unsettled
    /// it names this leader; once settled it may name an unrelated process.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Takes the parent's end of standard input, when it was piped.
    pub fn take_stdin(&mut self) -> Option<ProcessStdin> {
        self.stdin.take()
    }

    /// Takes the parent's end of standard output.
    pub fn take_stdout(&mut self) -> Option<ProcessStdout> {
        self.stdout.take()
    }

    /// Takes the parent's end of standard error.
    pub fn take_stderr(&mut self) -> Option<ProcessStderr> {
        self.stderr.take()
    }

    /// Whether the leader's exit has been observed (it stays unreaped).
    pub fn has_exited(&self) -> bool {
        self.exited
    }

    /// Whether the leader was reaped by something other than this owner
    /// (Unix only), so its tree is no longer signalled or awaited.
    pub fn identity_lost(&self) -> bool {
        self.leader.as_ref().is_some_and(Leader::lost)
    }

    /// Checks, without waiting, that the leader has not been reaped
    /// elsewhere, and once its identity is lost disarms the tree for good:
    /// its group id may now name another group, so nothing may signal it.
    fn check_identity(&mut self) {
        if let Some(leader) = &mut self.leader {
            leader.check_identity();
        }
        if self.identity_lost()
            && let Some(tree) = self.tree.take()
        {
            tree.disarm();
        }
    }

    /// Kills the tree, and the leader itself in case the tree is unknown,
    /// and selects the kill policy for [`Self::settle`], which awaits it.
    /// Before or after the leader's exit; the leader is unreaped here, so
    /// neither signal can reach a reused id.
    pub fn kill(&mut self) {
        self.killed = true;
        self.check_identity();
        if let Some(tree) = &self.tree {
            tree.kill();
        }
        if let Some(leader) = &mut self.leader {
            leader.start_kill();
        }
    }

    /// Asks the tree to stop: `SIGTERM` to its group on Unix, while the
    /// leader's identity is held. Windows has no such request; this does
    /// nothing there. Selects no policy and awaits nothing.
    pub fn terminate(&mut self) {
        self.check_identity();
        if let Some(tree) = &self.tree {
            tree.terminate();
        }
    }

    /// Completes once the leader has exited, without reaping it. Cancel
    /// safe. Fails once the leader's identity is lost
    /// ([`Self::identity_lost`]).
    pub async fn wait_exit(&mut self) -> io::Result<()> {
        if self.exited {
            return Ok(());
        }
        let Some(leader) = self.leader.as_mut() else {
            return Err(io::Error::other("the process is already settled"));
        };
        let waited = leader.wait_exit().await;
        self.check_identity();
        waited?;
        self.exited = true;
        Ok(())
    }

    /// Settles the process in place and releases its claim. A killed
    /// process is waited for, then its whole tree ([`TreeState::Gone`]);
    /// one that exited by itself is detached ([`TreeState::LeftRunning`]).
    /// Either way the tree is disarmed before the leader is reaped. A
    /// process that has neither exited nor been killed is killed first.
    ///
    /// Cancel safe: dropping the future part way keeps everything owned,
    /// and the next call resumes. After `Ok`, the process is settled and
    /// later calls fail.
    pub async fn finish(&mut self) -> io::Result<Settled> {
        if self.leader.is_none() {
            return Err(io::Error::other("the process is already settled"));
        }
        if !self.exited && !self.killed {
            self.kill();
        }
        let tree = if self.killed {
            self.wait_exit().await?;
            if let Some(tree) = &self.tree {
                tree.gone().await;
            }
            TreeState::Gone
        } else {
            // Before anything is released: should this fail, the error
            // returns with the tree still owned, armed, and (on Windows)
            // kill-on-close.
            if let Some(tree) = &self.tree {
                tree.detach()?;
            }
            TreeState::LeftRunning
        };
        if let Some(tree) = self.tree.take() {
            tree.disarm();
        }
        let Some(leader) = self.leader.as_mut() else {
            return Err(io::Error::other("the process is already settled"));
        };
        let reaped = leader.reap().await;
        self.check_identity();
        let status = reaped?;
        self.leader = None;
        self.claim = None;
        Ok(Settled { status, tree })
    }

    /// [`Self::finish`], consuming the process: dropped part way, or on an
    /// error, the process is dropped, which kills it and hands what is left
    /// to its claim's settling task.
    pub async fn settle(mut self) -> io::Result<Settled> {
        self.finish().await
    }
}

/// A just-spawned leader and what it was spawned with.
struct Spawned {
    pid: u32,
    leader: Leader,
    tree: ProcessTree,
    stdin: Option<ProcessStdin>,
    stdout: Option<ProcessStdout>,
    stderr: Option<ProcessStderr>,
}

#[cfg(unix)]
#[allow(
    clippy::unused_async,
    reason = "Windows connects its pipes asynchronously"
)]
async fn spawn_leader(spec: &ProcessSpec) -> io::Result<Spawned> {
    use std::process::Stdio;
    let mut command = Command::new(&spec.program);
    command.args(&spec.args);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    if let Some(env) = &spec.env {
        command
            .env_clear()
            .envs(env.iter().map(|(key, value)| (key, value)));
    }
    command
        .stdin(if spec.stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // No `kill_on_drop`: the owner kills explicitly, and must be able not
    // to once the leader's identity is lost.
    configure(&mut command);
    let mut child = command.spawn()?;
    let Some(tree) = ProcessTree::adopt(&child) else {
        // No pid: Tokio has already reaped it, so its group cannot be
        // named safely. Dropping the child leaves nothing of it to own.
        return Err(io::Error::other("the spawned command has no pid"));
    };
    Ok(Spawned {
        pid: tree.pid(),
        stdin: child.stdin.take(),
        stdout: child.stdout.take(),
        stderr: child.stderr.take(),
        tree,
        leader: Leader::new(child),
    })
}

#[cfg(windows)]
async fn spawn_leader(spec: &ProcessSpec) -> io::Result<Spawned> {
    let spawned = windows::spawn(spec).await?;
    Ok(Spawned {
        pid: spawned.leader.pid(),
        tree: ProcessTree::with_job(spawned.leader.pid(), spawned.job, spawned.port),
        leader: spawned.leader,
        stdin: spawned.stdin,
        stdout: spawned.stdout,
        stderr: spawned.stderr,
    })
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if self.leader.is_none() {
            return;
        }
        self.check_identity();
        if self.identity_lost() {
            // Nothing is signalled or awaited: the identity is gone.
            self.leader = None;
            self.claim = None;
            return;
        }
        // With the child still here, so the leader is killed even when no
        // tree was adopted.
        self.kill();
        let claim = if self.handed_off {
            None
        } else {
            self.claim.take()
        };
        match claim {
            Some(claim) => {
                let mut rest = OwnedProcess {
                    pid: self.pid,
                    leader: self.leader.take(),
                    tree: self.tree.take(),
                    stdin: None,
                    stdout: None,
                    stderr: None,
                    killed: true,
                    exited: self.exited,
                    claim: Some(claim),
                    runtime: self.runtime.clone(),
                    handed_off: true,
                };
                // The claim inside keeps its owner waiting until the settle
                // ends. If the runtime has shut down, the task is dropped at
                // once, and `rest` takes the branch below.
                drop(self.runtime.spawn(async move {
                    let _ = rest.finish().await;
                }));
            }
            None => {
                if let Some(tree) = self.tree.take() {
                    tree.disarm();
                }
                // Killed above. Dropping the leader hands it to Tokio's
                // orphan reaping (Unix) or closes its handle (Windows).
                self.leader = None;
                self.claim = None;
            }
        }
    }
}

#[cfg(unix)]
mod unix {
    use std::io;
    use std::process::ExitStatus;
    use std::time::Duration;

    use tokio::process::Child;

    /// A Unix leader: Tokio's child, never waited on through Tokio until it
    /// is reaped. Spawned without Tokio's `kill_on_drop`: every kill is
    /// explicit, so that one whose identity is lost sends none.
    pub(super) struct Leader {
        /// `None` once its identity is lost: the struct is then forgotten,
        /// since dropping it would let Tokio `waitpid` the pid later.
        pub(super) child: Option<Child>,
    }

    impl Leader {
        pub(super) fn new(child: Child) -> Self {
            Self { child: Some(child) }
        }

        /// Whether the leader was reaped by something other than this
        /// owner, so its pid, and its group's id, may name another process.
        pub(super) fn lost(&self) -> bool {
            self.child.is_none()
        }

        /// Records that the leader's identity is lost and gives up the
        /// child without dropping it: Tokio's drop would retry `waitpid` on
        /// the pid from its orphan queue, which could reap a later child
        /// that reused it. This leaks the small `Child` struct; its pipes
        /// were taken at spawn.
        fn lose(&mut self) -> io::Error {
            if let Some(child) = self.child.take() {
                std::mem::forget(child);
            }
            io::Error::other(
                "the command's process was reaped outside its owner; its tree is no longer signalled or awaited",
            )
        }

        /// Probes, without waiting or reaping, whether something else has
        /// reaped the leader (`ECHILD`), and if so records the loss.
        pub(super) fn check_identity(&mut self) {
            let Some(child) = &self.child else {
                return;
            };
            let Some(pid) = child.id() else {
                let _ = self.lose();
                return;
            };
            if exited_unreaped(libc::id_t::from(pid))
                .is_err_and(|error| error.raw_os_error() == Some(libc::ECHILD))
            {
                let _ = self.lose();
            }
        }

        pub(super) fn start_kill(&mut self) {
            if let Some(child) = &mut self.child {
                // The pid is unreaped (identity intact), so this reaches
                // the leader. Its result is not read: only the exit wait
                // says whether it ended.
                let _ = child.start_kill();
            }
        }

        /// Completes once the leader has exited, leaving it unreaped. An
        /// `ECHILD` means something else reaped it: the identity is lost.
        pub(super) async fn wait_exit(&mut self) -> io::Result<()> {
            let Some(child) = &self.child else {
                return Err(io::Error::other("the command's process identity is lost"));
            };
            let Some(pid) = child.id() else {
                // Tokio already reaped it, which this owner never asks for.
                return Err(self.lose());
            };
            let pid = libc::id_t::from(pid);
            // Registered before each check, so an exit between the check
            // and the wait still wakes it; also polled, in case this
            // runtime has no signal driver or a signal is coalesced away.
            let mut sigchld =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child()).ok();
            let mut pause = Duration::from_millis(1);
            loop {
                match exited_unreaped(pid) {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {
                        return Err(self.lose());
                    }
                    Err(error) => return Err(error),
                }
                match sigchld.as_mut() {
                    Some(signal) => {
                        tokio::select! {
                            _ = signal.recv() => {}
                            () = tokio::time::sleep(Duration::from_millis(50)) => {}
                        }
                    }
                    None => {
                        tokio::time::sleep(pause).await;
                        pause = (pause * 2).min(Duration::from_millis(20));
                    }
                }
            }
        }

        /// Reaps the leader, returning its status.
        pub(super) async fn reap(&mut self) -> io::Result<ExitStatus> {
            let Some(child) = &mut self.child else {
                return Err(io::Error::other("the command's process identity is lost"));
            };
            match child.wait().await {
                Err(error) if error.raw_os_error() == Some(libc::ECHILD) => Err(self.lose()),
                result => result,
            }
        }
    }

    /// Whether `pid`, a child of this process, has exited; it is left
    /// unreaped. `EINTR` is retried.
    fn exited_unreaped(pid: libc::id_t) -> io::Result<bool> {
        loop {
            // SAFETY: a zeroed `siginfo_t` is valid output space.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: plain syscall with a valid out pointer; `WNOWAIT`
            // leaves the child waitable, so its pid stays reserved.
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result == 0 {
                return Ok(siginfo_pid(&info) != 0);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn siginfo_pid(info: &libc::siginfo_t) -> libc::pid_t {
        // SAFETY: `waitid` filled a `SIGCHLD` record or left it zeroed.
        unsafe { info.si_pid() }
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn siginfo_pid(info: &libc::siginfo_t) -> libc::pid_t {
        info.si_pid
    }
}

#[cfg(windows)]
mod windows;

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;

    const BOUND: Duration = Duration::from_secs(10);

    // Review finding: a dropped process must kill its leader itself, not
    // only through its tree (the drop used to take the child before the
    // kill that falls back to it). The tree is left out here, which no
    // production spawn does, so only that fallback can end the leader. The
    // claim is a channel whose closing says the settling task released it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dropped_process_without_a_tree_kills_its_leader() {
        let (claim, mut released) = tokio::sync::oneshot::channel::<()>();
        let mut command = Command::new("sleep");
        command
            .arg("1000")
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().expect("spawn");
        let mut stdout = child.stdout.take().expect("stdout");
        let process = OwnedProcess {
            pid: child.id().expect("pid"),
            leader: Some(unix::Leader::new(child)),
            tree: None,
            stdin: None,
            stdout: None,
            stderr: None,
            killed: false,
            exited: false,
            claim: Some(Box::new(claim)),
            runtime: Handle::current(),
            handed_off: false,
        };
        drop(process);
        let mut byte = [0u8; 1];
        let eof = tokio::time::timeout(Duration::from_secs(2), stdout.read(&mut byte)).await;
        let closed = tokio::time::timeout(BOUND, &mut released).await;
        assert!(
            matches!(closed, Ok(Err(_))),
            "the claim was not released by the settle: {closed:?}"
        );
        assert!(
            matches!(eof, Ok(Ok(0))),
            "the leader outlived its dropped owner: {eof:?}"
        );
    }
}
