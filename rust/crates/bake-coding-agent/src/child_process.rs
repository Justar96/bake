//! Pi's `waitForChildProcess` over Bake's owned process tree, and the
//! owner of every child a tool spawns ([`OwnedProcess`]). The bash backend,
//! direct exec, and the grep and find tools spawn through it.
//!
//! [`wait_for_process`] is Pi's loop: exit and pipe closure are
//! independent, and post-exit output restarts the 100 ms idle grace.
//!
//! Ownership rules:
//!
//! - **Admission.** A process is spawned under a permit of its owner, a
//!   [`WorkGate`] (the session's, given to the tool that spawns it), so the
//!   owner's close waits for it; a closing owner refuses the spawn and
//!   nothing runs. Without an owner, only the call awaiting the process
//!   owns it.
//! - **Identity.** The leader is not reaped until its owner settles it: its
//!   exit is observed without reaping (`waitid` with `WNOWAIT`), so its pid,
//!   which is its process group's id, cannot be reused while the tree may
//!   still be signalled. The tree is disarmed before the leader is reaped
//!   ([`crate::tools::bash::tree`]).
//! - **Normal exit.** As in Pi, a command that exits by itself leaves its
//!   descendants running ([`TreeState::LeftRunning`]): the Windows job's
//!   kill-on-close limit is cleared first (if that fails, settling fails
//!   and the still-owned tree is killed and awaited as on a drop), then the
//!   tree is disarmed and released and its permit dropped. Until that
//!   detach, the job is kill-on-close, so Bake's death ends the tree. Such intentionally
//!   backgrounded descendants are outside every owner's drain, and the
//!   emergency kill no longer reaches them; owning them is a later
//!   session-jobs design, not this cleanup.
//! - **Kill.** After an abort, a timeout, or a caller's stop, the tree is
//!   killed and [`OwnedProcess::settle`] returns only once the leader has
//!   exited and no process is observed left in its tree
//!   ([`TreeState::Gone`]), with no time limit. A membership query that
//!   fails is retried, never read as empty, and the permit is held all the
//!   while, so the owner's close waits too. SIGKILL and job termination
//!   cannot be ignored: only a process in uninterruptible sleep, zombies
//!   no one reaps, or a host where membership stays unreadable hold it.
//! - **Drop.** Dropping an unsettled process (a dropped tool call) kills its
//!   tree at once and moves the process, with its permit, to a task on the
//!   runtime it was spawned on, which settles it; the owner's close waits
//!   for that task. Without an owner the drop only kills: the leader is
//!   reaped by Tokio (Unix) or its handle closed (Windows), and the tree is
//!   not awaited.
//!
//! A descendant that left the tree (`setsid`, a Windows job breakaway, or
//! the bubblewrap runner's `--new-session` payload, which its PID namespace
//! tears down instead) is outside this ownership: the tree is cleanup, not
//! containment, and D24's sandbox is the boundary.

use std::ffi::OsString;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::ExitStatus;
use std::time::Duration;

use bake_ai::AbortSignal;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
#[cfg(unix)]
use tokio::process::Command;
use tokio::runtime::Handle;
use tokio::time::Instant;

use crate::owned_work::{Refused, WorkGate, WorkPermit};
#[cfg(unix)]
use crate::tools::bash::tree::configure;
use crate::tools::bash::tree::{ProcessTree, TrackedTree};

pub use crate::tools::bash::tree::TreeState;

pub(crate) const EXIT_STDIO_GRACE: Duration = Duration::from_millis(100);
const READ_CHUNK: usize = 64 * 1024;

pub(crate) struct ProcessExit {
    pub status: Option<ExitStatus>,
    pub timed_out: bool,
    pub killed: bool,
    /// What became of the tree: left running after a normal exit, or
    /// killed and gone.
    #[allow(
        dead_code,
        reason = "read by tests; the tools' results do not report it"
    )]
    pub tree: TreeState,
}

/// A settled process: the leader's status and what became of its tree.
pub(crate) struct Settled {
    pub status: ExitStatus,
    pub tree: TreeState,
}

/// What to spawn: the narrow set of options every tool uses, so that each
/// platform can create the process inside its tree atomically (on Windows,
/// born in its job; Tokio's `Command` cannot ask for that). Standard
/// output and standard error are always piped.
#[derive(Debug, Clone)]
pub(crate) struct ProcessSpec {
    pub program: OsString,
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
pub(crate) type ProcessStdin = tokio::process::ChildStdin;
/// The parent's end of the child's standard output.
#[cfg(unix)]
pub(crate) type ProcessStdout = tokio::process::ChildStdout;
/// The parent's end of the child's standard error.
#[cfg(unix)]
pub(crate) type ProcessStderr = tokio::process::ChildStderr;
#[cfg(windows)]
pub(crate) type ProcessStdin = tokio::net::windows::named_pipe::NamedPipeServer;
#[cfg(windows)]
pub(crate) type ProcessStdout = tokio::net::windows::named_pipe::NamedPipeServer;
#[cfg(windows)]
pub(crate) type ProcessStderr = tokio::net::windows::named_pipe::NamedPipeServer;

#[cfg(unix)]
use self::unix::Leader;
#[cfg(windows)]
use self::windows::Leader;

/// A spawned child in a tree of its own, owned until it is settled.
pub(crate) struct OwnedProcess {
    /// `None` once settled or dropped.
    leader: Option<Leader>,
    tree: Option<TrackedTree>,
    stdin: Option<ProcessStdin>,
    stdout: Option<ProcessStdout>,
    stderr: Option<ProcessStderr>,
    killed: bool,
    /// The leader's exit was observed; it is unreaped until settled.
    exited: bool,
    /// The owner's claim, released once the process is settled.
    permit: Option<WorkPermit>,
    runtime: Handle,
    /// Set on the copy a drop moves to its settling task, whose own drop
    /// must not hand it off again.
    handed_off: bool,
}

impl OwnedProcess {
    /// Spawns `spec` as the leader of a new process tree under a permit of
    /// `owner`. Refused, with nothing spawned, once the owner is closing or
    /// outside a Tokio runtime. A spawn failure leaves nothing running: on
    /// Unix the command leads a new session from its first instruction
    /// (`setsid` before `exec`), and on Windows it is created inside a new
    /// job by `PROC_THREAD_ATTRIBUTE_JOB_LIST`, or not at all.
    ///
    /// The admission is taken before anything is created. The platform
    /// spawn suspends only before the process exists, and the owner is
    /// built in the same poll as its creation, so a future dropped while it
    /// spawns has created no process, and one created always has its owner.
    pub(crate) async fn spawn(spec: &ProcessSpec, owner: Option<&WorkGate>) -> io::Result<Self> {
        let runtime = Handle::try_current().map_err(|_| io::Error::other(Refused::NoRuntime))?;
        let permit = match owner {
            Some(gate) => Some(
                gate.enter()
                    .ok_or_else(|| io::Error::other(Refused::Closed))?,
            ),
            None => None,
        };
        let spawned = spawn_leader(spec).await?;
        Ok(Self {
            leader: Some(spawned.leader),
            tree: Some(TrackedTree::new(spawned.tree)),
            stdin: spawned.stdin,
            stdout: spawned.stdout,
            stderr: spawned.stderr,
            killed: false,
            exited: false,
            permit,
            runtime,
            handed_off: false,
        })
    }

    pub(crate) fn take_stdin(&mut self) -> Option<ProcessStdin> {
        self.stdin.take()
    }

    pub(crate) fn take_stdout(&mut self) -> Option<ProcessStdout> {
        self.stdout.take()
    }

    pub(crate) fn take_stderr(&mut self) -> Option<ProcessStderr> {
        self.stderr.take()
    }

    /// Once the leader's identity is lost, disarms the tree for good: its
    /// group id may now name another group, so nothing may signal it.
    fn note_lost_identity(&mut self) {
        if self.leader.as_ref().is_some_and(Leader::lost)
            && let Some(tree) = self.tree.take()
        {
            tree.disarm();
        }
    }

    /// Kills the tree, and the leader itself in case the tree is unknown.
    /// The kill is sent at once; [`Self::settle`] awaits it. The leader is
    /// unreaped here, so neither signal can reach a reused id.
    pub(crate) fn kill(&mut self) {
        self.killed = true;
        if let Some(tree) = &self.tree {
            tree.kill();
        }
        if let Some(leader) = &mut self.leader {
            leader.start_kill();
        }
    }

    /// Completes once the leader has exited, without reaping it. Cancel
    /// safe.
    pub(crate) async fn wait_exit(&mut self) -> io::Result<()> {
        if self.exited {
            return Ok(());
        }
        let Some(leader) = self.leader.as_mut() else {
            return Err(io::Error::other("the process is already settled"));
        };
        let waited = leader.wait_exit().await;
        self.note_lost_identity();
        waited?;
        self.exited = true;
        Ok(())
    }

    /// Settles the child and releases its permit. A killed child is waited
    /// for, then its whole tree ([`TreeState::Gone`]); one that exited by
    /// itself leaves its
    /// descendants running ([`TreeState::LeftRunning`]). Either way the tree
    /// is disarmed before the leader is reaped. A process that has neither
    /// exited nor been killed is killed first.
    ///
    /// The process stays owned while this waits: dropping the future part
    /// way drops the process, which hands what is left to its owner.
    pub(crate) async fn settle(mut self) -> io::Result<Settled> {
        if !self.exited && !self.killed {
            self.kill();
        }
        let tree = if self.killed {
            self.wait_exit().await?;
            let Some(tree) = &self.tree else {
                return Err(io::Error::other("the process has no tree"));
            };
            tree.gone().await;
            TreeState::Gone
        } else {
            // Before anything is released: should this fail, the error
            // returns with the tree still owned, armed, and (on Windows)
            // kill-on-close, and the drop then kills and settles it under
            // the owner, so the failure is reported, not hidden.
            if let Some(tree) = &self.tree {
                tree.detach()?;
            }
            TreeState::LeftRunning
        };
        if let Some(tree) = &self.tree {
            tree.disarm();
        }
        self.tree = None;
        let Some(leader) = self.leader.as_mut() else {
            return Err(io::Error::other("the process is already settled"));
        };
        let reaped = leader.reap().await;
        self.note_lost_identity();
        let status = reaped?;
        self.leader = None;
        self.permit = None;
        Ok(Settled { status, tree })
    }
}

/// A just-spawned leader and what it was spawned with.
struct Spawned {
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
        if self.leader.as_ref().is_some_and(Leader::lost) {
            // Nothing is signalled or awaited: the identity is gone, and
            // the error that found that has been returned.
            self.note_lost_identity();
            self.leader = None;
            self.permit = None;
            return;
        }
        // With the child still here, so the leader is killed even when no
        // tree was adopted.
        self.kill();
        let permit = if self.handed_off {
            None
        } else {
            self.permit.take()
        };
        match permit {
            Some(permit) => {
                let rest = OwnedProcess {
                    leader: self.leader.take(),
                    tree: self.tree.take(),
                    stdin: None,
                    stdout: None,
                    stderr: None,
                    killed: true,
                    exited: self.exited,
                    permit: Some(permit),
                    runtime: self.runtime.clone(),
                    handed_off: true,
                };
                // The permit inside keeps the owner's close waiting until
                // the settle ends. If the runtime has shut down, the task
                // is dropped at once, and `rest` takes the branch below.
                drop(self.runtime.spawn(async move {
                    let _ = rest.settle().await;
                }));
            }
            None => {
                if let Some(tree) = self.tree.take() {
                    tree.disarm();
                }
                // Killed above. Dropping the leader hands it to Tokio's
                // orphan reaping (Unix) or closes its handle (Windows).
                self.leader = None;
                self.permit = None;
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

async fn read_some<R: AsyncRead + Unpin>(
    reader: Option<&mut R>,
    buffer: &mut [u8],
) -> io::Result<usize> {
    match reader {
        Some(reader) => reader.read(buffer).await,
        None => std::future::pending().await,
    }
}

pub(crate) async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

pub(crate) async fn aborted(signal: Option<&AbortSignal>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending().await,
    }
}

async fn poll_some(task: Option<&mut Pin<Box<dyn Future<Output = ()> + Send>>>) {
    match task {
        Some(task) => task.await,
        None => std::future::pending().await,
    }
}

async fn wait_exit(process: &mut OwnedProcess, exited: bool) -> io::Result<()> {
    if exited {
        std::future::pending().await
    } else {
        process.wait_exit().await
    }
}

/// Own a spawned child, its optional input, and both output streams.
/// Callbacks run inline and cannot outlive the call. Like Pi, a normal
/// exit leaves descendants running; idle inherited pipes are closed. After
/// an abort or the deadline, the child is killed with its tree and the
/// call returns once the child and its tree are gone
/// ([`OwnedProcess::settle`]). Dropping the future drops the process,
/// which hands it to its owner.
pub(crate) async fn wait_for_process(
    mut shell: OwnedProcess,
    stdin_bytes: Option<Vec<u8>>,
    signal: Option<&AbortSignal>,
    deadline: Option<Instant>,
    mut on_stdout: impl FnMut(&[u8]),
    mut on_stderr: impl FnMut(&[u8]),
) -> io::Result<ProcessExit> {
    let mut stdin_write: Option<Pin<Box<dyn Future<Output = ()> + Send>>> =
        shell.take_stdin().map(|mut stdin| {
            let bytes = stdin_bytes.unwrap_or_default();
            Box::pin(async move {
                // Pi ignores standard-input errors; the shell may exit
                // without reading.
                let _ = stdin.write_all(&bytes).await;
                let _ = stdin.shutdown().await;
            }) as Pin<Box<dyn Future<Output = ()> + Send>>
        });
    let mut stdout = shell.take_stdout();
    let mut stderr = shell.take_stderr();
    let mut out_buffer = vec![0; READ_CHUNK];
    let mut err_buffer = vec![0; READ_CHUNK];
    let mut exited = false;
    let mut grace: Option<Instant> = None;
    let mut timeout_at = deadline;
    let mut abort_pending = signal.is_some();
    let mut timed_out = false;
    let mut killed = false;

    loop {
        if exited && stdout.is_none() && stderr.is_none() {
            break;
        }
        let abort_signal = signal.filter(|_| abort_pending);
        tokio::select! {
            result = wait_exit(&mut shell, exited) => {
                result?;
                exited = true;
                grace = Some(Instant::now() + EXIT_STDIO_GRACE);
            }
            read = read_some(stdout.as_mut(), &mut out_buffer) => match read {
                Ok(count) if count > 0 => {
                    on_stdout(&out_buffer[..count]);
                    if exited {
                        grace = Some(Instant::now() + EXIT_STDIO_GRACE);
                    }
                }
                _ => stdout = None,
            },
            read = read_some(stderr.as_mut(), &mut err_buffer) => match read {
                Ok(count) if count > 0 => {
                    on_stderr(&err_buffer[..count]);
                    if exited {
                        grace = Some(Instant::now() + EXIT_STDIO_GRACE);
                    }
                }
                _ => stderr = None,
            },
            () = poll_some(stdin_write.as_mut()) => stdin_write = None,
            () = sleep_until(grace) => break,
            () = aborted(abort_signal) => {
                abort_pending = false;
                killed = true;
                shell.kill();
            }
            () = sleep_until(timeout_at) => {
                timeout_at = None;
                timed_out = true;
                killed = true;
                shell.kill();
            }
        }
    }
    // Our ends of the pipes close before the tree is awaited: a process
    // that left the tree may hold the other ends.
    drop((stdout, stderr, stdin_write));
    let settled = shell.settle().await?;

    Ok(ProcessExit {
        status: Some(settled.status),
        timed_out,
        killed,
        tree: settled.tree,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Stdio;

    const BOUND: Duration = Duration::from_secs(10);

    // Review finding: a dropped process must kill its leader itself, not
    // only through its tree (the drop used to take the child before the
    // kill that falls back to it). The tree is left out here, which no
    // production spawn does, so only that fallback can end the leader.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dropped_process_without_a_tree_kills_its_leader() {
        let gate = WorkGate::default();
        let mut command = Command::new("sleep");
        command
            .arg("1000")
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().expect("spawn");
        let mut stdout = child.stdout.take().expect("stdout");
        let process = OwnedProcess {
            leader: Some(unix::Leader::new(child)),
            tree: None,
            stdin: None,
            stdout: None,
            stderr: None,
            killed: false,
            exited: false,
            permit: gate.enter(),
            runtime: Handle::current(),
            handed_off: false,
        };
        drop(process);
        let mut byte = [0u8; 1];
        let eof = tokio::time::timeout(Duration::from_secs(2), stdout.read(&mut byte)).await;
        tokio::time::timeout(BOUND, gate.close())
            .await
            .expect("the owner's close waits for the reap, which ends");
        assert!(
            matches!(eof, Ok(Ok(0))),
            "the leader outlived its dropped owner: {eof:?}"
        );
    }

    // A closing owner refuses the spawn, and nothing runs.
    #[tokio::test]
    async fn a_closing_owner_refuses_the_spawn() {
        let gate = WorkGate::default();
        let closed = gate.close();
        let spec = ProcessSpec {
            program: "sh".into(),
            args: vec!["-c".into(), "exit 0".into()],
            cwd: None,
            env: None,
            stdin: false,
        };
        let refused = OwnedProcess::spawn(&spec, Some(&gate)).await;
        tokio::time::timeout(BOUND, closed).await.expect("closed");
        let error = refused.err().expect("refused");
        assert_eq!(error.to_string(), Refused::Closed.to_string());
    }
}
