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
//!   account that falls short never settles. The account is kept from the
//!   spawn on by an owned pump task (`pump`), started in the same poll as
//!   the owner and holding only a weak reference to the tree. It drains the
//!   job's messages in bounded steps and closes ended members' handles, so
//!   the kernel queue holds about what arrives between steps while the pump
//!   gets CPU as fast as the tree starts processes; starved, it grows until
//!   the pump catches up (see `pump`). A settled owner stops and awaits it
//!   before its claim goes; a drop without a claim aborts it.
//! - **Drop.** Dropping an unsettled process kills its tree at once. With a
//!   claim, the process and its claim move to a task on the runtime it was
//!   spawned on, which settles it, so the claim's owner waits for that
//!   task; without one the drop only kills, the leader is reaped by Tokio
//!   (Unix) or its handle closed (Windows), and the tree is not awaited.
//!
//! Unix trees are a new session and process group (`setsid` before `exec`);
//! Windows trees are a kill-on-close job the process is born in
//! (`PROC_THREAD_ATTRIBUTE_JOB_LIST`), with only its standard handles
//! inherited and overlapped named pipes connected before creation. A Unix
//! descendant that leaves the tree (`setsid`) is outside this ownership:
//! the tree is cleanup, not containment, and D24's sandbox is the
//! boundary. One Linux exception: the init of a PID namespace a member
//! started, such as bubblewrap's, is pinned by a pidfd, killed with the
//! tree, and awaited, which ends and awaits its whole namespace. A Windows
//! descendant cannot leave: the job allows no breakaway, and one from a
//! job nested in it stays in it.
//!
//! A normal exit's background descendants and a descendant that left its
//! tree outlive their owner by design, as in Pi. A host killed with
//! `SIGKILL` settles nothing; on Linux only a leader spawned with
//! [`OwnedProcess::spawn_tied_to_thread`] is then killed by the kernel,
//! and the rest of every tree is left running.
//!
//! On Windows the job and its account are `windows_job::Job`, public so
//! that `bake-sandbox`'s synchronous Windows runner keeps its confined tree
//! in the same kind of job and settles it on the same account: created
//! empty with its completion port already associated, pumped while members
//! run, and never read as ended from its active count alone.
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

pub mod command_line;
pub mod environment;
#[cfg(any(windows, test))]
mod job_ledger;
#[cfg(windows)]
mod pump;
mod tree;
#[cfg(windows)]
pub mod windows_job;

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
    /// The tree's live job pump (`pump`), started with the owner and
    /// stopped and awaited before the claim is released.
    #[cfg(windows)]
    pump: Option<pump::Pump>,
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
        Self::spawn_with(spec, claim, false).await
    }

    /// [`Self::spawn`], with the leader also killed (`SIGKILL`) by the
    /// kernel when the thread that spawns it ends, which covers this
    /// process being killed outright (`PR_SET_PDEATHSIG`). Only the leader
    /// is tied: the rest of its tree survives a killed host as before.
    ///
    /// Safe only where the spawning thread outlives the process, such as a
    /// dedicated thread that settles it before it returns. Tokio's blocking
    /// threads end when idle, and a worker thread can hand its work to
    /// another one, so a process spawned from either would be killed while
    /// its owner still runs; never use it from a shared runtime.
    #[cfg(target_os = "linux")]
    pub async fn spawn_tied_to_thread(
        spec: &ProcessSpec,
        claim: Option<Claim>,
    ) -> io::Result<Self> {
        Self::spawn_with(spec, claim, true).await
    }

    async fn spawn_with(
        spec: &ProcessSpec,
        claim: Option<Claim>,
        tied_to_thread: bool,
    ) -> io::Result<Self> {
        let runtime = Handle::try_current()
            .map_err(|_| io::Error::other("an owned process needs a Tokio runtime"))?;
        let spawned = spawn_leader(spec, tied_to_thread).await?;
        // From here to the return nothing awaits: the owner, and on Windows
        // its pump, exist in the same poll as the process.
        let tree = TrackedTree::new(spawned.tree);
        #[cfg(windows)]
        let pump = Some(pump::Pump::start(tree.downgrade(), &runtime));
        Ok(Self {
            pid: spawned.pid,
            leader: Some(spawned.leader),
            tree: Some(tree),
            stdin: spawned.stdin,
            stdout: spawned.stdout,
            stderr: spawned.stderr,
            killed: false,
            exited: false,
            claim,
            runtime,
            handed_off: false,
            #[cfg(windows)]
            pump,
        })
    }

    /// Stops the live pump and awaits it; cancel safe.
    #[cfg(windows)]
    async fn stop_pump(&mut self) {
        if let Some(pump) = &mut self.pump {
            pump.stop().await;
        }
        self.pump = None;
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
        // The tree's end is decided; the pump has nothing left to keep
        // bounded. Awaited before the claim goes.
        #[cfg(windows)]
        self.stop_pump().await;
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
async fn spawn_leader(spec: &ProcessSpec, tied_to_thread: bool) -> io::Result<Spawned> {
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
    #[cfg(target_os = "linux")]
    if tied_to_thread {
        crate::tree::tie_to_spawning_thread(&mut command);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = tied_to_thread;
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
async fn spawn_leader(spec: &ProcessSpec, _tied_to_thread: bool) -> io::Result<Spawned> {
    let spawned = windows::spawn(spec).await?;
    Ok(Spawned {
        pid: spawned.leader.pid(),
        tree: ProcessTree::with_job(spawned.leader.pid(), spawned.job),
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
            #[cfg(windows)]
            if let Some(mut pump) = self.pump.take() {
                pump.abort();
            }
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
                    #[cfg(windows)]
                    pump: self.pump.take(),
                };
                // The claim inside keeps its owner waiting until the settle
                // ends. If the runtime has shut down, the task is dropped at
                // once, and `rest` takes the branch below.
                drop(self.runtime.spawn(async move {
                    let _ = rest.finish().await;
                }));
            }
            None => {
                // Kill only: the pump is stopped, not awaited.
                #[cfg(windows)]
                if let Some(mut pump) = self.pump.take() {
                    pump.abort();
                }
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

// The live job pump's ownership, on real jobs. Each case would fail if the
// pump were not started with the owner, did not drain and prune while the
// owner is idle, or were not stopped and awaited where the owner says.
#[cfg(all(test, windows))]
mod pump_tests {
    use super::*;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const BOUND: Duration = Duration::from_secs(20);

    /// `cmd.exe` running `script` (`/d`: no AutoRun; `/c`: then exit).
    fn cmd(script: &str) -> ProcessSpec {
        ProcessSpec {
            program: "cmd.exe".into(),
            args: vec!["/d".into(), "/q".into(), "/c".into(), script.into()],
            cwd: None,
            env: None,
            stdin: false,
        }
    }

    /// A leader that keeps starting short-lived children (`ping -n 1`).
    const CHURN: &str = "for /L %i in (1,1,100000) do @ping -n 1 127.0.0.1 >nul";
    /// A leader that lives about 30 s.
    const LONG: &str = "ping -n 30 127.0.0.1 >nul";

    async fn spawn(spec: &ProcessSpec, claim: Option<Claim>) -> OwnedProcess {
        tokio::time::timeout(BOUND, OwnedProcess::spawn(spec, claim))
            .await
            .expect("spawn within the bound")
            .expect("spawn")
    }

    fn control(process: &OwnedProcess) -> Arc<pump::Control> {
        process.pump.as_ref().expect("a pump").control()
    }

    /// Waits, bounded, until only `holder` keeps `control`: the pump task
    /// has ended and its owner's handle is gone.
    async fn task_ended(control: &Arc<pump::Control>, holders: usize) {
        let deadline = Instant::now() + BOUND;
        while Arc::strong_count(control) > holders {
            assert!(Instant::now() < deadline, "the pump task did not end");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    // While the owner does nothing (no wait, no finish), the pump takes the
    // job's messages from the real port into the account and closes the
    // handles of children that ended. Without the pump the account stays
    // empty until a settle; without pruning, the held handles grow with
    // every child.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_pump_drains_and_prunes_while_the_owner_is_idle() {
        let mut process = spawn(&cmd(CHURN), None).await;
        let control = control(&process);
        let deadline = Instant::now() + BOUND;
        loop {
            let (counted, open) = process
                .tree
                .as_ref()
                .expect("tree")
                .tree()
                .account_snapshot();
            // The leader, its console host, and a child or two in flight.
            if counted >= 40 && open <= 8 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "idle owner: {counted} members counted, {open} still held"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        process.kill();
        let settled = tokio::time::timeout(BOUND, process.finish())
            .await
            .expect("settled within the bound")
            .expect("settled");
        assert_eq!(settled.tree, TreeState::Gone);
        assert!(process.pump.is_none());
        assert_eq!(
            Arc::strong_count(&control),
            1,
            "finish left the pump running"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_natural_exit_stops_and_awaits_the_pump() {
        let mut process = spawn(&cmd("exit 0"), None).await;
        let control = control(&process);
        tokio::time::timeout(BOUND, process.wait_exit())
            .await
            .expect("exit within the bound")
            .expect("exit");
        let settled = tokio::time::timeout(BOUND, process.finish())
            .await
            .expect("settled within the bound")
            .expect("settled");
        assert_eq!(settled.tree, TreeState::LeftRunning);
        assert_eq!(
            Arc::strong_count(&control),
            1,
            "detach left the pump running"
        );
    }

    // A finish dropped part way keeps the pump owned; the next finish stops
    // and awaits it. The pump task is held at its stop by a test barrier, so
    // the first finish cannot complete wherever it is when it is dropped:
    // that poll is pending by construction, not by timing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_cancelled_finish_keeps_the_pump_until_resumed() {
        let mut process = spawn(&cmd(LONG), None).await;
        let control = control(&process);
        let barrier = control.hold_stop();
        process.kill();
        let first = tokio::select! {
            biased;
            settled = process.finish() => Some(settled.map(|settled| settled.tree)),
            () = std::future::ready(()) => None,
        };
        assert!(
            first.is_none(),
            "a finish completed while its pump was held: {first:?}"
        );
        assert!(process.pump.is_some(), "a cancelled finish lost the pump");
        assert!(
            process.leader.is_some(),
            "a cancelled finish settled the process"
        );
        barrier.add_permits(1);
        let settled = tokio::time::timeout(BOUND, process.finish())
            .await
            .expect("settled within the bound")
            .expect("settled");
        assert_eq!(settled.tree, TreeState::Gone);
        assert!(process.pump.is_none());
        assert_eq!(
            Arc::strong_count(&control),
            1,
            "the resumed finish left the pump running"
        );
    }

    /// A claim that reports, when released, whether the pump task had
    /// already ended.
    struct Claimed {
        control: Arc<pump::Control>,
        released: Option<tokio::sync::oneshot::Sender<usize>>,
    }

    impl Drop for Claimed {
        fn drop(&mut self) {
            if let Some(released) = self.released.take() {
                let _ = released.send(Arc::strong_count(&self.control));
            }
        }
    }

    // A dropped owner with a claim hands the pump to its settling task,
    // which stops and awaits it before the claim is released.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dropped_claim_is_released_only_after_the_pump_ended() {
        let (released, receive) = tokio::sync::oneshot::channel();
        let process = spawn(&cmd(LONG), None).await;
        let control = control(&process);
        let mut process = process;
        process.claim = Some(Box::new(Claimed {
            control: Arc::clone(&control),
            released: Some(released),
        }));
        drop(process);
        let holders = tokio::time::timeout(BOUND, receive)
            .await
            .expect("the claim was released within the bound")
            .expect("the claim reported");
        // The claim's own reference and this test's.
        assert_eq!(holders, 2, "the claim was released while the pump ran");
    }

    // Without a claim a drop only kills: the pump is aborted, not awaited,
    // and its task still ends.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dropped_owner_without_a_claim_aborts_the_pump() {
        let process = spawn(&cmd(LONG), None).await;
        let control = control(&process);
        drop(process);
        task_ended(&control, 1).await;
    }
}
