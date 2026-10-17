//! Owning a stdio MCP server: its process, its process tree, its standard
//! error, and the client connected over its standard input and output.
//!
//! Adapted from Pi's `StdioTransport` (`packages/mcp/src/transports/stdio.ts`,
//! v1.3.0) and its tests (`test/stdio.test.ts`, `test/fixtures/`). Pi's
//! transport spawns, frames, and shuts down in one object. Here the work is
//! split between two owners, so nothing runs without one:
//!
//! - [`spawn_stdio`] starts the process and returns a [`StdioProcess`], which
//!   owns the child, its process tree, and the task that reads its standard
//!   error, and the [`Connecting`] the foundation client returns, which owns
//!   the protocol task and both message pipes. Neither starts anything the
//!   other cannot see.
//! - Shutdown is two awaited steps, in this order: the client's
//!   ([`Connecting::shutdown`] or [`McpClient::shutdown`]), which closes the
//!   server's standard input as the specification's stdio shutdown begins,
//!   then [`StdioProcess::shutdown`], which waits for the server to exit,
//!   escalates to `SIGTERM` and `SIGKILL`, signals what is left of its tree
//!   (Unix) or waits for its Job to empty (Windows), reaps it, and drains
//!   its standard error. The process owner keeps every
//!   handle (the child, its tree, the reader task) until that cleanup has
//!   finished: a cancelled or incomplete shutdown leaves them owned, and the
//!   next call resumes.
//!
//! # Caller obligations
//!
//! This module runs exactly the command it is given. It does not decide
//! whether a server may run, and it does not confine it. The caller must:
//!
//! - admit the command under Bake's permissions (D24) before spawning, and
//!   pass the D24 sandbox's wrapped command (`bake-sandbox`'s
//!   `ConfinedCommand`: its program, arguments, and runner environment) as
//!   the [`StdioCommand`] when the session's mode confines processes. No
//!   caller does yet: neither the CLI nor configuration starts MCP servers;
//! - build the environment explicitly: a [`StdioCommand`] starts with an
//!   empty one, and [`scrubbed_parent_environment`] is the scrubbed base Bake
//!   uses for servers, over which configured values are set;
//! - keep the [`StdioProcess`] and the [`Connecting`] (then the
//!   [`McpClient`]) together, and await both shutdowns before dropping them.
//!   Dropping either only signals: see [`StdioProcess`]'s drop contract.
//!
//! # Platforms
//!
//! Process groups and Jobs are cleanup ownership, not D24 containment.
//!
//! On Unix the server leads a new process group (set before `exec`). Its
//! exit is observed without reaping it (`waitid` with `WNOWAIT`, woken by
//! `SIGCHLD`), the group is killed while the leader is still unreaped, and
//! only then is the leader reaped. While it is unreaped its pid, and so the
//! group id, cannot be reused, so no signal can reach an unrelated group; no
//! group signal is sent after the reap, or once the leader is found reaped by
//! someone else (the host must not `waitpid(-1)` children it does not own).
//! On Unix this is signal-only cleanup: the final `SIGKILL` is sent, but the
//! group's other members are not awaited, so a completed shutdown does not
//! prove they have exited. Awaiting group membership before the leader is
//! reaped (Linux `/proc`, macOS `proc_listpids`) is an open integration and
//! release gap; [`TreeEnd::Killed`] reports this case.
//!
//! On Windows the server is created inside a new kill-on-close Job
//! (`PROC_THREAD_ATTRIBUTE_JOB_LIST`), inheriting only its pipe ends
//! (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`), so it runs nothing outside the
//! Job. Windows has no graceful termination signal: after the grace for
//! standard input the Job is terminated. While the child's pipe ends are
//! inheritable, between their creation and `CreateProcessW`, another thread
//! that creates a process inheriting every inheritable handle (as `std`'s
//! `Command` does) can inherit them too. The Windows path is compiled for
//! the GNU target and linted, not yet run in this repository's tests.
//!
//! A descendant that leaves the group (`setsid`, `setpgid`) or the Job
//! escapes cleanup; [`StdioProcess::shutdown`] bounds its wait for such a
//! descendant's standard error instead of waiting for it.

mod environment;
mod server;
mod stderr;

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

pub use self::environment::{is_scrubbed_name, scrubbed_parent_environment};
use self::server::Server;
pub use self::stderr::StderrSnapshot;
use self::stderr::StderrState;
use crate::client::{ClientOptions, ConnectError, Connecting, McpClient, OptionsError};
use crate::transport::{LineReader, LineWriter};

/// A server's program, arguments, working directory, and environment.
///
/// The program runs directly, never through a shell, so arguments are not
/// interpolated. The environment starts empty: nothing from this process is
/// inherited unless added, usually from [`scrubbed_parent_environment`].
/// With an empty `PATH`, a bare program name is resolved as the platform's
/// process creation does without one; pass a path to be sure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StdioCommand {
    program: OsString,
    args: Vec<OsString>,
    current_dir: Option<PathBuf>,
    env: Vec<(OsString, OsString)>,
}

impl StdioCommand {
    /// A command for `program` with no arguments and an empty environment.
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            current_dir: None,
            env: Vec::new(),
        }
    }

    /// Add one argument.
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Add arguments.
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Run in `dir` instead of this process's working directory.
    pub fn current_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(dir.into());
        self
    }

    /// Set one variable; a later value for the same name wins.
    pub fn env(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((name.into(), value.into()));
        self
    }

    /// Set variables in order; later values for a name win.
    pub fn envs(
        mut self,
        vars: impl IntoIterator<Item = (impl Into<OsString>, impl Into<OsString>)>,
    ) -> Self {
        self.env.extend(
            vars.into_iter()
                .map(|(name, value)| (name.into(), value.into())),
        );
        self
    }

    /// The program.
    pub fn program(&self) -> &OsString {
        &self.program
    }

    /// The arguments.
    pub fn arguments(&self) -> &[OsString] {
        &self.args
    }
}

/// Pi's grace between closing standard input and `SIGTERM`
/// (`STDIN_CLOSE_GRACE_MS`).
const STDIN_CLOSE_GRACE: Duration = Duration::from_millis(500);
/// Pi's wait between `SIGTERM` and `SIGKILL` (`DEFAULT_CLOSE_TIMEOUT_MS`).
const TERMINATE_GRACE: Duration = Duration::from_secs(2);
/// Pi's standard error tail (`DEFAULT_MAX_STDERR_BYTES`).
const MAX_STDERR_BYTES: usize = 64 * 1024;

/// How a stdio server is run and stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StdioOptions {
    /// The client's options. Its `max_message_bytes` also bounds each line
    /// read from the server's standard output.
    pub client: ClientOptions,
    /// How long the server may take to exit on its own once its standard
    /// input is closed. Default: 500 ms, as Pi.
    pub stdin_close_grace: Duration,
    /// How long after `SIGTERM` before `SIGKILL` (Unix). Default: 2 s, as Pi.
    pub terminate_grace: Duration,
    /// How long to wait for the server to be reaped after `SIGKILL` or Job
    /// termination. Default: 2 s.
    pub kill_wait: Duration,
    /// How long, once the server has exited, to wait for its standard error
    /// to end. A descendant that escaped the tree can hold it open; the
    /// reader is stopped after this. Default: 1 s.
    pub stderr_drain: Duration,
    /// The standard error tail kept. Default: 64 KiB, as Pi.
    pub max_stderr_bytes: NonZeroUsize,
}

impl StdioOptions {
    /// The defaults around `client`.
    pub fn new(client: ClientOptions) -> Self {
        Self {
            client,
            stdin_close_grace: STDIN_CLOSE_GRACE,
            terminate_grace: TERMINATE_GRACE,
            kill_wait: Duration::from_secs(2),
            stderr_drain: Duration::from_secs(1),
            max_stderr_bytes: NonZeroUsize::new(MAX_STDERR_BYTES).unwrap_or(NonZeroUsize::MIN),
        }
    }

    /// Check the client's options and that every wait here gives a deadline
    /// the clock can represent. [`spawn_stdio`] calls it before spawning.
    pub fn validate(&self) -> Result<(), OptionsError> {
        self.client.validate()?;
        let now = Instant::now();
        for (field, value) in [
            ("stdin_close_grace", self.stdin_close_grace),
            ("terminate_grace", self.terminate_grace),
            ("kill_wait", self.kill_wait),
            ("stderr_drain", self.stderr_drain),
        ] {
            if now.checked_add(value).is_none() {
                return Err(OptionsError::DurationTooLong { field, value });
            }
        }
        Ok(())
    }
}

/// Why [`spawn_stdio`] failed, and who owns what it started.
#[derive(Debug)]
#[non_exhaustive]
pub enum SpawnError {
    /// The options were refused; nothing was started.
    InvalidOptions(OptionsError),
    /// No Tokio runtime is running on this thread; nothing was started.
    NoRuntime,
    /// The process could not be created (a missing program, a refused
    /// working directory, an invalid environment, an OS limit), or on Unix
    /// the `SIGCHLD` listener could not be registered. Nothing is running.
    Spawn(io::Error),
    /// The process was started, but the client over its pipes was not. The
    /// returned owner holds the process, its tree, and its standard error
    /// reader: call [`StdioProcess::shutdown`] on it, as after any other
    /// failure, before treating the server as gone.
    Started {
        /// The owner of the started process.
        process: Box<StdioProcess>,
        /// Why the client was not started.
        error: Box<StartError>,
    },
}

/// Why the client over a started server was not started.
#[derive(Debug)]
#[non_exhaustive]
pub enum StartError {
    /// The client refused its options or found no runtime.
    Client(ConnectError),
    /// A standard stream was not piped.
    MissingPipe(&'static str),
}

impl fmt::Display for SpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOptions(error) => write!(f, "invalid MCP stdio options: {error}"),
            Self::NoRuntime => f.write_str("an MCP stdio server needs a running Tokio runtime"),
            Self::Spawn(error) => write!(f, "could not start the MCP server: {error}"),
            Self::Started { error, .. } => match &**error {
                StartError::Client(error) => write!(
                    f,
                    "the MCP server started, but its client did not; shut the process down: {error}"
                ),
                StartError::MissingPipe(stream) => write!(
                    f,
                    "the MCP server started without a piped {stream}; shut the process down"
                ),
            },
        }
    }
}

impl std::error::Error for SpawnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidOptions(error) => Some(error),
            Self::Spawn(error) => Some(error),
            Self::Started { error, .. } => match &**error {
                StartError::Client(error) => Some(error),
                StartError::MissingPipe(_) => None,
            },
            Self::NoRuntime => None,
        }
    }
}

/// How far the shutdown had to go.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Stop {
    /// It had exited, or exited within the grace after its standard input
    /// closed.
    Exited,
    /// It exited after `SIGTERM` to its group (Unix).
    Terminated,
    /// It was killed: `SIGKILL` to its group, or Job termination.
    Killed,
}

/// What is known about the server's process tree once it was reaped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TreeEnd {
    /// Windows: the Job was observed with no active process.
    Empty,
    /// Unix: `SIGKILL` was sent to the group while its leader was still
    /// unreaped, so it reached only this tree. That is a request, not an
    /// observed end: this owner does not yet await the group's other members
    /// (an open integration and release gap), so some may still be exiting.
    /// A member that left the group was not signalled at all.
    Killed,
    /// Unix: the leader was reaped by someone else, so its group was not
    /// signalled (the id may name unrelated processes).
    NotSignalled,
}

/// What a completed [`StdioProcess::shutdown`] did. Each fact is reported on
/// its own: a server can exit with status 0 after `SIGTERM`, and its standard
/// error can end or not regardless of how it stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessReport {
    /// The server's exit status. `None` only if something else in this
    /// process reaped it (a `waitpid(-1)` the owner does not control); its
    /// tree was then not signalled, as its process group id may already name
    /// unrelated processes.
    pub status: Option<ExitStatus>,
    /// The furthest step the shutdown took.
    pub stop: Stop,
    /// What is known about the rest of the tree, reported apart from the
    /// leader's status: a kill request is not every descendant's exit.
    pub tree: TreeEnd,
    /// Whether standard error ended (every holder closed it) within
    /// [`StdioOptions::stderr_drain`]. `false` means a descendant outside the
    /// tree, or a reader failure, kept it from ending; the reader was stopped
    /// and awaited, and the pipe's read end is closed.
    pub stderr_ended: bool,
    /// The standard error read.
    pub stderr: StderrSnapshot,
}

/// A shutdown that has not finished: the server was killed but has not
/// exited within [`StdioOptions::kill_wait`] (an uninterruptible process),
/// or, on Windows, it exited but its Job still had active processes after
/// another `kill_wait`. The owner still holds the process, its tree, and
/// the standard error reader; call [`StdioProcess::shutdown`] again to keep
/// waiting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShutdownIncomplete {
    /// The furthest step taken.
    pub stop: Stop,
    /// Whether the server itself has exited and been reaped; the rest of
    /// its tree is what is still awaited.
    pub reaped: bool,
    /// The standard error read so far.
    pub stderr: StderrSnapshot,
}

impl fmt::Display for ShutdownIncomplete {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the MCP server was killed but has not exited yet; shut it down again")
    }
}

impl std::error::Error for ShutdownIncomplete {}

/// A running stdio server's process, process tree, and standard error.
///
/// Call [`shutdown`](Self::shutdown) after the client's own shutdown, and
/// await it until it returns `Ok`. Dropping the owner without that is a
/// fallback, not a teardown: it kills the tree at once if the leader is
/// still unreaped (Unix: `SIGKILL` to the group; Windows: terminating and
/// closing the Job), aborts the standard error reader without awaiting it,
/// and leaves reaping the leader to Tokio's background reaper (Unix), so the
/// caller cannot tell when the processes are gone.
#[derive(Debug)]
pub struct StdioProcess {
    pid: u32,
    stderr: Arc<StderrState>,
    options: StdioOptions,
    inner: Mutex<Inner>,
}

/// Everything the owner holds. Every field stays here until its cleanup has
/// finished, so a shutdown future dropped part way loses nothing: the next
/// call resumes from `stop` and awaits what is left.
#[derive(Debug)]
struct Inner {
    server: Server,
    /// `None` only once the reader has been awaited to its end.
    stderr_task: Option<JoinHandle<()>>,
    /// The furthest escalation step taken so far.
    stop: Stop,
    /// Set once the leader is reaped (or found reaped elsewhere).
    status: Option<Option<ExitStatus>>,
    /// Set only once everything above is done.
    report: Option<ProcessReport>,
}

/// Spawn `command` as a stdio MCP server and start a client over its pipes.
///
/// Must be called inside a Tokio runtime. The options are validated before
/// anything is spawned. On success, the returned [`Connecting`] owns the
/// protocol task (initialize it, and keep it to shut it down if
/// initialization is cancelled), and the [`StdioProcess`] owns the process.
/// Errors before the process exists leave nothing running; a failure after
/// it exists is [`SpawnError::Started`], which hands back its owner.
///
/// The caller must have admitted and, where its mode requires, confined
/// `command` (see the [module documentation](self)).
///
/// A server that exits before answering fails initialization with
/// [`CloseReason::EndOfStream`](crate::CloseReason::EndOfStream), or with a
/// [`CloseReason::Send`](crate::CloseReason::Send) error (`BrokenPipe`) if the
/// write of `initialize` fails first; its status is in the shutdown report.
/// Writing to an exited server raises `SIGPIPE` on Unix, which Rust programs
/// ignore by default; a host that restores its default disposition is killed
/// by it.
pub fn spawn_stdio(
    command: StdioCommand,
    options: StdioOptions,
) -> Result<(StdioProcess, Connecting), SpawnError> {
    options.validate().map_err(SpawnError::InvalidOptions)?;
    let runtime = tokio::runtime::Handle::try_current().map_err(|_| SpawnError::NoRuntime)?;
    // `Command::spawn` and the `SIGCHLD` listener register with this
    // thread's runtime.
    let _enter = runtime.enter();
    let (server, pipes) = server::spawn(&command).map_err(SpawnError::Spawn)?;
    let state = StderrState::new(options.max_stderr_bytes.get());
    let stderr_task = pipes
        .stderr
        .map(|stderr| runtime.spawn(stderr::read(stderr, state.clone())));
    let missing = if stderr_task.is_none() {
        // Marked ended so the owner's drain does not wait for it.
        Some("standard error")
    } else {
        None
    };
    let process = StdioProcess {
        pid: server.pid(),
        stderr: state,
        options: options.clone(),
        inner: Mutex::new(Inner {
            server,
            stderr_task,
            stop: Stop::Exited,
            status: None,
            report: None,
        }),
    };
    let (Some(stdin), Some(stdout), None) = (pipes.stdin, pipes.stdout, missing) else {
        return Err(SpawnError::Started {
            process: Box::new(process),
            error: Box::new(StartError::MissingPipe(
                missing.unwrap_or("standard input or output"),
            )),
        });
    };
    let receiver = LineReader::new(stdout, options.client.limits.max_message_bytes);
    match McpClient::start(receiver, LineWriter::new(stdin), options.client) {
        Ok(connecting) => Ok((process, connecting)),
        Err(error) => Err(SpawnError::Started {
            process: Box::new(process),
            error: Box::new(StartError::Client(error)),
        }),
    }
}

impl StdioProcess {
    /// The server's process id, as spawned. While the owner has not reaped
    /// it, it names this server; after a completed shutdown it may name an
    /// unrelated process.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// What its standard error has shown so far.
    pub fn stderr(&self) -> StderrSnapshot {
        self.stderr.snapshot()
    }

    /// Changes whenever standard error is read or ends; its value is the
    /// total bytes read.
    pub fn stderr_updates(&self) -> watch::Receiver<u64> {
        self.stderr.subscribe()
    }

    /// Stop the server and wait until it is reaped and its standard error
    /// reader has ended.
    ///
    /// Call it after the client's shutdown, which closes the server's
    /// standard input. The steps, each bounded by [`StdioOptions`]: wait
    /// `stdin_close_grace` for the server to exit; on Unix send `SIGTERM` to
    /// its group and wait `terminate_grace`; then kill the tree and wait
    /// `kill_wait` for it to exit. Once it has exited, and before it is
    /// reaped, the tree is killed again (Pi sends `SIGTERM` there; Bake sends
    /// `SIGKILL`). On Windows the Job is then awaited until empty; on Unix the
    /// kill is sent but the group's members are not awaited, so `Ok` does not
    /// prove Unix tree quiescence ([`ProcessReport::tree`] says which). No
    /// group signal is ever sent after the leader is reaped. Finally standard error is
    /// given `stderr_drain` to end, then its reader is stopped and awaited.
    ///
    /// `Err(ShutdownIncomplete)` means the server was killed and has not
    /// exited within `kill_wait`: everything is still owned, and the next
    /// call keeps waiting. A shutdown future dropped part way also keeps
    /// everything, and the next call resumes where it stopped. Once a call
    /// returns `Ok`, every later call returns the same report at once; calls
    /// from several tasks run one at a time.
    pub async fn shutdown(&self) -> Result<ProcessReport, ShutdownIncomplete> {
        let mut inner = self.inner.lock().await;
        if let Some(report) = &inner.report {
            return Ok(report.clone());
        }
        let options = &self.options;
        if inner.status.is_none() {
            let mut exited = false;
            if inner.stop == Stop::Exited {
                exited = exit_within(&mut inner.server, options.stdin_close_grace).await;
            }
            if !exited && cfg!(unix) && inner.stop <= Stop::Terminated {
                inner.server.terminate();
                inner.stop = Stop::Terminated;
                exited = exit_within(&mut inner.server, options.terminate_grace).await;
            }
            if !exited {
                inner.server.kill_tree();
                inner.stop = Stop::Killed;
                exited = exit_within(&mut inner.server, options.kill_wait).await;
            }
            if !exited {
                return Err(ShutdownIncomplete {
                    stop: inner.stop,
                    reaped: false,
                    stderr: self.stderr.snapshot(),
                });
            }
            // Kills what is left of the tree while the leader's identity is
            // still held, then reaps; no await in between.
            let status = inner.server.reap();
            inner.status = Some(status);
        }
        // Windows: wait until the Job has no process (a terminate request is
        // not every process's exit). Unix: returns at once.
        if tokio::time::timeout(options.kill_wait, inner.server.tree_ended())
            .await
            .is_err()
        {
            return Err(ShutdownIncomplete {
                stop: inner.stop,
                reaped: true,
                stderr: self.stderr.snapshot(),
            });
        }
        let mut stderr_ended = self.stderr.snapshot().ended;
        if let Some(task) = inner.stderr_task.as_mut() {
            // Awaited in place: the handle leaves `inner` only once the task
            // has ended, so a dropped shutdown leaves it for the next call.
            if tokio::time::timeout(options.stderr_drain, &mut *task)
                .await
                .is_err()
            {
                task.abort();
                let _ = (&mut *task).await;
            }
            inner.stderr_task = None;
            stderr_ended = self.stderr.snapshot().ended;
        }
        let report = ProcessReport {
            status: inner.status.flatten(),
            stop: inner.stop,
            tree: inner.server.tree(),
            stderr_ended,
            stderr: self.stderr.snapshot(),
        };
        inner.report = Some(report.clone());
        Ok(report)
    }
}

impl Drop for StdioProcess {
    fn drop(&mut self) {
        // The server's own drop kills its tree only while the leader's
        // identity is held; the reader is aborted, not awaited.
        if let Some(task) = self.inner.get_mut().stderr_task.take() {
            task.abort();
        }
    }
}

/// Whether the server exits within `limit`; it stays unreaped. A zero limit
/// checks once, without waiting for the timer's next tick.
async fn exit_within(server: &mut Server, limit: Duration) -> bool {
    if limit.is_zero() {
        return tokio::select! {
            biased;
            () = server.exited() => true,
            () = std::future::ready(()) => false,
        };
    }
    tokio::time::timeout(limit, server.exited()).await.is_ok()
}
