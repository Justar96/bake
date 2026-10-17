//! The parent side: [`WasmPluginHost`] owns one helper process for one
//! plugin generation, from spawn to reap.
//!
//! The host is created without a process ([`WasmPluginHost::new`]); the
//! helper is spawned by [`WasmPluginHost::initialize`] and stored in the
//! host before the first await, so from then on the host is its owner. No
//! operation takes the helper out of the host across an await: the state
//! lock is held while an operation runs, and the helper stays in place.
//! The host spawns no task and keeps no callback.
//!
//! Teardown is resumable. When an operation is cancelled, misses its
//! deadline, or sees the helper break the protocol, it kills the helper
//! (or, for [`WasmPluginHost::shutdown`] of an idle helper, asks it to
//! exit first), closes its input, awaits its exit, and drains its output,
//! recording each step in the helper. If that future is dropped part way,
//! the helper and the progress stay in the host, and the next operation or
//! `shutdown` continues from there. Each wait is bounded by
//! [`HostLimits::teardown_timeout`]; when it runs out, the returned
//! [`ExitReport`] says what is unfinished ([`ExitReport::is_complete`] is
//! false), the host keeps the helper, and a later `shutdown` resumes.
//! The host reports [`HostStatus::Stopped`] only once the helper has been
//! reaped and its pipes have closed.
//!
//! Dropping an operation's future while the helper runs a request sends
//! the kill at once (the request's guard does), but only a later operation
//! can await the exit. Dropping the host itself kills an unreaped helper
//! without awaiting it: on Unix, Tokio reaps it in the background; on
//! Windows, the Job ends it. Only a `shutdown` whose report is complete
//! establishes that the helper has exited and its pipes have closed.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use bake_ai::{AbortController, AbortSignal};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, watch};
use tokio::time::Instant;

use super::engine::{MAX_LOG_LINE, MAX_LOGS_PER_CALL};
use super::manifest::WasmPackage;
use super::process::{self, Exit, KillSwitch, PipeIn, PipeOut, Process};
use super::protocol::{
    self, CallRequest, FrameError, HEADER_LEN, Hello, Kind, LimitMechanism, LoadFailure, LogLine,
    MemoryLimit, Outcome, PROTOCOL_VERSION, Reply, TrapKind,
};
use super::{
    BUILD, HELPER_ARG, MAX_ARGS_BYTES, MAX_ARGS_DEPTH, MAX_COMPONENT_BYTES, MAX_SESSION_LOG_BYTES,
    MAX_TOOL_CALL_ID_BYTES,
};
use crate::plugins::{MAX_REASON_BYTES, PluginManifest, ToolCallVeto, ToolCallView};

/// The parent's deadline for a call is the helper's own plus this, so the
/// helper's structured deadline normally answers first.
const CALL_GRACE: Duration = Duration::from_secs(2);
/// How long a helper that closed its output may take to finish exiting
/// before it is killed.
const EXIT_GRACE: Duration = Duration::from_secs(2);
/// The helper stderr kept for reports, in bytes: the end of it.
const STDERR_TAIL: usize = 4 * 1024;

/// How to start the helper: a Bake executable and the arguments that
/// select its helper entry point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperCommand {
    program: PathBuf,
    leading_args: Vec<OsString>,
}

impl HelperCommand {
    /// This process's own executable with [`HELPER_ARG`], the production
    /// helper. On Linux the program is `/proc/self/exe`, the image that is
    /// running even if its file has since been replaced.
    pub fn current_exe() -> std::io::Result<Self> {
        #[cfg(target_os = "linux")]
        let program = PathBuf::from("/proc/self/exe");
        #[cfg(not(target_os = "linux"))]
        let program = std::env::current_exe()?;
        Ok(Self::new(program, [HELPER_ARG]))
    }

    /// `program` run with `leading_args`, which must select a Bake helper
    /// entry point (`[HELPER_ARG]` for a Bake binary). The helper's own
    /// arguments follow them.
    pub fn new<I, S>(program: impl Into<PathBuf>, leading_args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Self {
            program: program.into(),
            leading_args: leading_args.into_iter().map(Into::into).collect(),
        }
    }
}

/// The limits one helper runs under. [`Default`] gives the starting
/// values; every field is checked by [`WasmPluginHost::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostLimits {
    /// The helper's memory budget, 32 MiB to 4 GiB.
    ///
    /// - **Linux and macOS:** address space above the helper's own size
    ///   when it set the limit: its hard `RLIMIT_AS` is that startup size
    ///   (the baseline) plus the budget. On Linux the helper and the parent
    ///   also refuse a baseline over [`super::MAX_LINUX_BASELINE`], so the
    ///   absolute address-space limit is at most the budget plus 256 MiB.
    ///   macOS processes start with tens of GiB mapped, so only the
    ///   increment is bounded there.
    /// - **Windows:** the Job's per-process committed-memory limit.
    ///
    /// None of these bounds total memory: kernel memory, the page cache,
    /// and data written through a file descriptor without a mapping are
    /// outside an address-space limit.
    pub memory_budget: u64,
    /// The largest component accepted, at most [`MAX_COMPONENT_BYTES`].
    pub max_component_bytes: usize,
    /// CPU fuel for one call; running out traps the call.
    pub call_fuel: u64,
    /// The helper's deadline for one call, including host calls the guest
    /// awaits. The parent kills the helper two seconds after it.
    pub call_timeout: Duration,
    /// From the queue admission to the helper's verified `Hello`, and the
    /// longest wait for the queue.
    pub startup_timeout: Duration,
    /// From sending the component to the helper's answer, compiling
    /// included.
    pub compile_timeout: Duration,
    /// How long an orderly shutdown may take before it becomes a kill.
    pub shutdown_timeout: Duration,
    /// The longest one operation waits for a killed or exiting helper to
    /// be reaped, and then for its pipes to close. When either runs out,
    /// the host keeps the helper and reports the teardown unfinished.
    pub teardown_timeout: Duration,
}

impl Default for HostLimits {
    fn default() -> Self {
        Self {
            memory_budget: 512 << 20,
            max_component_bytes: 8 << 20,
            call_fuel: 2_000_000_000,
            call_timeout: crate::plugins::DEFAULT_CALL_TIMEOUT,
            startup_timeout: Duration::from_secs(5),
            compile_timeout: Duration::from_secs(30),
            shutdown_timeout: Duration::from_secs(2),
            teardown_timeout: Duration::from_secs(10),
        }
    }
}

impl HostLimits {
    fn check(&self) -> Result<(), String> {
        if !(32 << 20..=4 << 30).contains(&self.memory_budget) {
            return Err(format!(
                "memory budget {} is outside 32 MiB to 4 GiB",
                self.memory_budget
            ));
        }
        if self.max_component_bytes == 0 || self.max_component_bytes > MAX_COMPONENT_BYTES {
            return Err(format!(
                "component limit {} is outside 1 to {MAX_COMPONENT_BYTES}",
                self.max_component_bytes
            ));
        }
        if self.call_fuel == 0 {
            return Err("call fuel is zero".to_owned());
        }
        for (name, value) in [
            ("call", self.call_timeout),
            ("startup", self.startup_timeout),
            ("compile", self.compile_timeout),
            ("shutdown", self.shutdown_timeout),
            ("teardown", self.teardown_timeout),
        ] {
            if value < Duration::from_millis(1) || value > Duration::from_secs(600) {
                return Err(format!("{name} timeout {value:?} is outside 1 ms to 600 s"));
            }
        }
        Ok(())
    }
}

/// Admits one helper start at a time, compile included, and refuses a
/// start when `max_waiting` others already wait. Share one queue among the
/// hosts that should not compile at once.
///
/// A start's slot belongs to its helper, not to the `initialize` future:
/// it is held until the component has loaded, or until a teardown has
/// observed the helper's exit (reaped it). A start that was dropped,
/// cancelled, timed out, or refused therefore keeps the slot while its
/// helper may still be compiling, until [`WasmPluginHost::shutdown`] or
/// another operation observes the exit. Waiting only for the helper's
/// pipes to close does not hold the slot. Dropping the host releases the
/// slot with the helper, whose kill was sent but whose exit nobody awaits.
#[derive(Debug, Clone)]
pub struct CompileQueue {
    inner: Arc<QueueInner>,
}

#[derive(Debug)]
struct QueueInner {
    permits: Arc<Semaphore>,
    waiting: AtomicUsize,
    max_waiting: usize,
}

impl CompileQueue {
    /// A queue that admits one start and lets `max_waiting` more wait.
    pub fn new(max_waiting: usize) -> Self {
        Self {
            inner: Arc::new(QueueInner {
                permits: Arc::new(Semaphore::new(1)),
                waiting: AtomicUsize::new(0),
                max_waiting,
            }),
        }
    }

    async fn admit(
        &self,
        deadline: Instant,
        signal: Option<&AbortSignal>,
        closing: &AbortSignal,
    ) -> Result<OwnedSemaphorePermit, StartError> {
        struct Waiting<'a>(&'a AtomicUsize);
        impl Drop for Waiting<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        if let Ok(permit) = Arc::clone(&self.inner.permits).try_acquire_owned() {
            return Ok(permit);
        }
        // The count only rises past `max_waiting` by the failed attempts
        // themselves, each undone at once.
        if self.inner.waiting.fetch_add(1, Ordering::SeqCst) >= self.inner.max_waiting {
            self.inner.waiting.fetch_sub(1, Ordering::SeqCst);
            return Err(StartError::QueueFull);
        }
        let _waiting = Waiting(&self.inner.waiting);
        tokio::select! {
            biased;
            () = cancelled(signal) => Err(StartError::Cancelled { exit: None }),
            () = closing.cancelled() => Err(StartError::ShutDown { exit: ExitReport::default() }),
            permit = Arc::clone(&self.inner.permits).acquire_owned() => {
                permit.map_err(|_| StartError::QueueFull)
            }
            () = tokio::time::sleep_until(deadline) => Err(StartError::QueueTimeout),
        }
    }
}

/// How a helper ended, each fact on its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExitReport {
    /// A helper process was created. `false` (the [`Default`]) means none
    /// was, and every other field is empty.
    pub spawned: bool,
    /// The parent killed it.
    pub killed: bool,
    /// Its exit code, when it exited.
    pub code: Option<i32>,
    /// The signal that ended it (Unix).
    pub signal: Option<i32>,
    /// Its exit was awaited.
    pub reaped: bool,
    /// Its output pipes reached end of file.
    pub drained: bool,
    /// The end of its standard error, at most 4 KiB, with control
    /// characters escaped.
    pub stderr: String,
}

impl ExitReport {
    /// Whether teardown is finished: no helper was spawned, or it was
    /// reaped and its pipes closed. An unfinished report means the host
    /// still owns the helper and a later [`WasmPluginHost::shutdown`]
    /// continues the teardown.
    pub fn is_complete(&self) -> bool {
        !self.spawned || (self.reaped && self.drained)
    }
}

/// What the host is doing, for diagnostics and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostStatus {
    /// Created; [`WasmPluginHost::initialize`] has not spawned a helper.
    New,
    /// Waiting for the compile queue, or spawning the helper.
    Starting,
    /// The helper with this process id runs and is setting up its limit.
    Spawned(Option<u32>),
    /// The helper with this process id is compiling the component.
    Compiling(Option<u32>),
    /// Ready for a call.
    Idle,
    /// The helper is running the call with this request id.
    Calling(u64),
    /// Teardown has started and is not finished; the host still owns the
    /// helper.
    Stopping,
    /// The helper has been reaped and its pipes have closed, or none was
    /// spawned.
    Stopped,
}

/// Why [`WasmPluginHost::new`] or [`WasmPluginHost::initialize`] failed.
/// Each `exit` is the teardown's report so far; when it is not
/// [complete](ExitReport::is_complete), the host still owns the helper and
/// [`WasmPluginHost::shutdown`] continues the teardown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// A [`HostLimits`] field or the component's size is out of range.
    InvalidLimits(String),
    /// `initialize` was already called on this host.
    AlreadyStarted,
    /// Too many starts already wait in the [`CompileQueue`].
    QueueFull,
    /// The queue did not admit the start within the startup timeout.
    QueueTimeout,
    /// The start was cancelled.
    Cancelled {
        /// The helper's exit, when it had started.
        exit: Option<ExitReport>,
    },
    /// [`WasmPluginHost::shutdown`] interrupted the start.
    ShutDown {
        /// The helper's exit.
        exit: ExitReport,
    },
    /// The helper could not be created; no process exists.
    Spawn(String),
    /// The helper did not send a valid `Hello` with a verified memory limit
    /// in time: it failed its limit setup, exited, or is another build.
    Handshake {
        /// What was wrong.
        message: String,
        /// The helper's exit.
        exit: ExitReport,
    },
    /// The helper refused the component: invalid, failed to compile, or
    /// does not match the host's bindings.
    Refused {
        /// Which step refused it.
        failure: LoadFailure,
        /// The helper's message.
        message: String,
        /// The helper's exit after the orderly shutdown.
        exit: ExitReport,
    },
    /// Compiling did not finish within the compile timeout.
    CompileTimeout {
        /// The helper's exit.
        exit: ExitReport,
    },
    /// The helper died or broke the protocol while loading.
    Helper {
        /// What happened.
        message: String,
        /// The helper's exit.
        exit: ExitReport,
    },
}

impl StartError {
    /// The teardown report, when a helper had been spawned.
    pub fn exit(&self) -> Option<&ExitReport> {
        match self {
            Self::Cancelled { exit } => exit.as_ref(),
            Self::ShutDown { exit }
            | Self::Handshake { exit, .. }
            | Self::Refused { exit, .. }
            | Self::CompileTimeout { exit }
            | Self::Helper { exit, .. } => Some(exit),
            _ => None,
        }
    }
}

impl std::fmt::Display for StartError {
    /// The messages may quote helper or guest text; escape them before
    /// showing them on a terminal.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidLimits(message) => write!(f, "invalid plugin host limits: {message}"),
            Self::AlreadyStarted => f.write_str("the plugin host was already started"),
            Self::QueueFull => f.write_str("too many plugins are waiting to compile"),
            Self::QueueTimeout => f.write_str("the plugin waited too long to compile"),
            Self::Cancelled { .. } => f.write_str("the plugin start was cancelled"),
            Self::ShutDown { .. } => f.write_str("the plugin host was shut down while starting"),
            Self::Spawn(message) => write!(f, "cannot start the plugin helper: {message}"),
            Self::Handshake { message, .. } => {
                write!(f, "the plugin helper did not start: {message}")
            }
            Self::Refused { message, .. } => {
                write!(f, "the plugin component was refused: {message}")
            }
            Self::CompileTimeout { .. } => f.write_str("compiling the plugin took too long"),
            Self::Helper { message, .. } => write!(f, "the plugin helper failed: {message}"),
        }
    }
}

impl std::error::Error for StartError {}

/// A checked veto: the plugin's answer, within the contract's bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedVeto {
    /// The answer; a block reason is at most [`MAX_REASON_BYTES`].
    pub veto: ToolCallVeto,
    /// The log lines the call staged, kept within the call's 64 KiB and the
    /// host's 1 MiB quota. Never model-visible.
    pub logs: Vec<LogLine>,
    /// Lines dropped by either quota.
    pub dropped_logs: u32,
    /// Which instance of this generation answered, from 1.
    pub instance: u32,
}

/// Why a call has no answer. [`CallError::Guest`], [`CallError::Trap`],
/// [`CallError::NotListed`], [`CallError::InvalidCall`], and
/// [`CallError::ArgumentsTooLarge`] leave the helper running (a trap with a
/// fresh instance for the next call); every other variant means the
/// helper has been stopped, and its `exit` says whether teardown finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// The manifest does not list the tool.
    NotListed,
    /// The call id is longer than [`MAX_TOOL_CALL_ID_BYTES`], the
    /// arguments nest deeper than [`MAX_ARGS_DEPTH`], or the request ids
    /// are exhausted; the helper was not asked.
    InvalidCall(String),
    /// The arguments' JSON is longer than [`MAX_ARGS_BYTES`]; serializing
    /// stopped at the limit and the helper was not asked.
    ArgumentsTooLarge,
    /// The host has not been initialized, or has stopped. Any unfinished
    /// teardown was continued first.
    Unavailable,
    /// The call's signal aborted. The helper was killed and torn down if
    /// the call had reached it.
    Cancelled {
        /// The helper's exit, when it was killed.
        exit: Option<ExitReport>,
    },
    /// [`WasmPluginHost::shutdown`] interrupted the call.
    ShutDown {
        /// The helper's exit.
        exit: ExitReport,
    },
    /// The helper did not answer within the call timeout and its grace.
    Timeout {
        /// The helper's exit.
        exit: ExitReport,
    },
    /// The plugin answered with an error. The text is the guest's.
    Guest(String),
    /// The guest trapped or hit a limit; the helper discarded the instance.
    Trap {
        /// Which limit or failure.
        kind: TrapKind,
        /// Wasmtime's or the helper's text, which may quote the guest.
        message: String,
    },
    /// The helper died or broke the protocol.
    Helper {
        /// What happened.
        message: String,
        /// The helper's exit.
        exit: ExitReport,
    },
}

impl std::fmt::Display for CallError {
    /// The messages may quote guest text; escape them before showing them
    /// on a terminal.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotListed => f.write_str("the plugin does not veto this tool"),
            Self::InvalidCall(message) => write!(f, "the call cannot be sent: {message}"),
            Self::ArgumentsTooLarge => write!(
                f,
                "the call's arguments are over the plugin limit of {MAX_ARGS_BYTES} bytes"
            ),
            Self::Unavailable => f.write_str("the plugin is not running"),
            Self::Cancelled { .. } => f.write_str("the plugin call was cancelled"),
            Self::ShutDown { .. } => f.write_str("the plugin was shut down"),
            Self::Timeout { .. } => f.write_str("the plugin did not answer in time"),
            Self::Guest(message) => write!(f, "the plugin failed: {message}"),
            Self::Trap { message, .. } => write!(f, "the plugin trapped: {message}"),
            Self::Helper { message, .. } => write!(f, "the plugin helper failed: {message}"),
        }
    }
}

impl std::error::Error for CallError {}

/// What [`WasmPluginHost::new`] needs; the host keeps it.
#[derive(Debug)]
pub struct HostRequest {
    /// The helper executable.
    pub command: HelperCommand,
    /// The plugin.
    pub package: WasmPackage,
    /// The helper's limits.
    pub limits: HostLimits,
    /// The queue shared by hosts that should not compile at once.
    pub queue: CompileQueue,
    /// The plugin generation, which every request and reply carries; a
    /// restart uses a new host with a new one.
    pub generation: u64,
    /// Receives the host's [`HostStatus`]; the host keeps it. `None`
    /// creates one; [`WasmPluginHost::status`] follows either.
    pub status: Option<watch::Sender<HostStatus>>,
}

/// One plugin generation and the helper process that runs it.
#[derive(Debug)]
pub struct WasmPluginHost {
    command: HelperCommand,
    package: WasmPackage,
    limits: HostLimits,
    queue: CompileQueue,
    generation: u64,
    limit: OnceLock<MemoryLimit>,
    pid: OnceLock<Option<u32>>,
    closing: AbortController,
    status: watch::Sender<HostStatus>,
    session_log_bytes: AtomicUsize,
    state: Mutex<State>,
}

#[derive(Debug)]
enum State {
    /// No helper yet.
    New,
    /// The helper, running or being torn down; the host owns it.
    Live(Box<Helper>),
    /// Teardown finished, or no helper was spawned.
    Stopped(ExitReport),
}

/// How to end a helper, from gentlest to strongest; a teardown already in
/// progress only escalates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Plan {
    /// Ask it to exit, then kill it after the shutdown timeout.
    Graceful,
    /// It closed its output: await its own exit briefly, then kill.
    Exited,
    /// Kill it now.
    Kill,
}

impl WasmPluginHost {
    /// A host for `request`, with no process yet. Checks the limits and
    /// the component's size.
    pub fn new(request: HostRequest) -> Result<Self, StartError> {
        request.limits.check().map_err(StartError::InvalidLimits)?;
        if request.package.component().len() > request.limits.max_component_bytes {
            return Err(StartError::InvalidLimits(format!(
                "the component is {} bytes, over the {}-byte limit",
                request.package.component().len(),
                request.limits.max_component_bytes
            )));
        }
        let status = request
            .status
            .unwrap_or_else(|| watch::channel(HostStatus::New).0);
        status.send_replace(HostStatus::New);
        Ok(Self {
            command: request.command,
            package: request.package,
            limits: request.limits,
            queue: request.queue,
            generation: request.generation,
            limit: OnceLock::new(),
            pid: OnceLock::new(),
            closing: AbortController::new(),
            status,
            session_log_bytes: AtomicUsize::new(0),
            state: Mutex::new(State::New),
        })
    }

    /// Starts the helper and loads the component: waits for the queue,
    /// spawns the helper (which then belongs to this host), waits for its
    /// `Hello` with a verified memory limit, sends the component, and
    /// waits until it is checked, compiled, and linked. Runs at most once.
    ///
    /// A cancellation, deadline, refusal, or protocol error tears the
    /// helper down before this returns; the error's `exit` says whether
    /// that finished. Dropping this future after the spawn kills the
    /// helper at once and leaves it in the host, where
    /// [`Self::shutdown`] awaits it.
    pub async fn initialize(&self, signal: Option<&AbortSignal>) -> Result<(), StartError> {
        if cancelled_now(signal) {
            return Err(StartError::Cancelled { exit: None });
        }
        let closing = self.closing.signal();
        let mut state = tokio::select! {
            biased;
            () = cancelled(signal) => return Err(StartError::Cancelled { exit: None }),
            state = self.state.lock() => state,
        };
        if !matches!(*state, State::New) {
            return Err(StartError::AlreadyStarted);
        }
        if closing.aborted() {
            // Shut down before anything was spawned.
            return Err(StartError::ShutDown {
                exit: ExitReport::default(),
            });
        }
        self.status.send_replace(HostStatus::Starting);
        let admitted = Instant::now() + self.limits.startup_timeout;
        // The permit moves into the helper below: it is released when the
        // component has loaded, or when teardown has observed the helper's
        // exit, never merely when this future ends.
        let permit = match self.queue.admit(admitted, signal, &closing).await {
            Ok(permit) => permit,
            Err(error) => {
                self.status.send_replace(HostStatus::New);
                return Err(error);
            }
        };
        let spawned = match spawn(&self.command, &self.limits, self.generation).await {
            Ok(spawned) => spawned,
            Err(message) => {
                *state = State::Stopped(ExitReport::default());
                self.status.send_replace(HostStatus::Stopped);
                return Err(StartError::Spawn(message));
            }
        };
        let pid = spawned.process.id();
        let _ = self.pid.set(pid);
        // From here the host owns the helper; nothing moves it out.
        *state = State::Live(Box::new(Helper::new(spawned, permit)));
        self.status.send_replace(HostStatus::Spawned(pid));
        let State::Live(helper) = &mut *state else {
            return Err(StartError::Spawn("the helper was not stored".to_owned()));
        };
        // Until the start settles, dropping this future kills the helper.
        helper.abandoned = true;
        let kill = KillOnDrop(Some(helper.process.kill_switch()));

        // `Hello`, with the limit the helper verified before reading input.
        let hello = exchange(
            helper,
            None,
            Instant::now() + self.limits.startup_timeout,
            signal,
            &closing,
            |kind, payload| match kind {
                Kind::Hello => Hello::decode(&payload)
                    .map(Some)
                    .map_err(|error| error.to_string()),
                _ => Err(format!("{kind:?} before Hello")),
            },
        )
        .await;
        let hello = match hello {
            Ok(hello) => match check_hello(&hello, &self.limits, self.generation) {
                Ok(()) => hello,
                Err(message) => {
                    kill.disarm();
                    helper.abandoned = false;
                    let exit = self.teardown(&mut state, Plan::Kill).await;
                    return Err(StartError::Handshake { message, exit });
                }
            },
            Err(interrupt) => {
                kill.disarm();
                helper.abandoned = false;
                let exit = self.teardown(&mut state, interrupt.plan()).await;
                return Err(match interrupt {
                    Interrupt::Cancelled => StartError::Cancelled { exit: Some(exit) },
                    Interrupt::Closing => StartError::ShutDown { exit },
                    other => StartError::Handshake {
                        message: other.describe("before its Hello"),
                        exit,
                    },
                });
            }
        };

        // The component: checked, compiled, and linked in the helper.
        let load = protocol::frame(
            Kind::Load,
            &protocol::encode_load(self.package.manifest().api(), self.package.component()),
        );
        let status = &self.status;
        let loaded = exchange(
            helper,
            Some(load),
            Instant::now() + self.limits.compile_timeout,
            signal,
            &closing,
            |kind, payload| match kind {
                Kind::Compiling if payload.is_empty() => {
                    status.send_replace(HostStatus::Compiling(pid));
                    Ok(None)
                }
                Kind::Loaded => protocol::decode_loaded(&payload)
                    .map(Some)
                    .map_err(|error| error.to_string()),
                Kind::Fatal => Err(describe_fatal(&payload)),
                _ => Err(format!("unexpected {kind:?} while loading")),
            },
        )
        .await;
        kill.disarm();
        helper.abandoned = false;
        match loaded {
            Ok(Ok(_micros)) => {
                helper.ready = true;
                // Loaded: compiling is over, so the next start may compile.
                helper.admission = None;
                let _ = self.limit.set(hello.limit);
                self.status.send_replace(HostStatus::Idle);
                Ok(())
            }
            Ok(Err((failure, message))) => {
                let exit = self.teardown(&mut state, Plan::Graceful).await;
                Err(StartError::Refused {
                    failure,
                    message,
                    exit,
                })
            }
            Err(interrupt) => {
                let exit = self.teardown(&mut state, interrupt.plan()).await;
                Err(match interrupt {
                    Interrupt::Cancelled => StartError::Cancelled { exit: Some(exit) },
                    Interrupt::Closing => StartError::ShutDown { exit },
                    Interrupt::Deadline => StartError::CompileTimeout { exit },
                    other => StartError::Helper {
                        message: other.describe("while loading"),
                        exit,
                    },
                })
            }
        }
    }

    /// The plugin's id, version, and vetoed tools.
    pub fn manifest(&self) -> &PluginManifest {
        self.package.manifest().plugin()
    }

    /// The plugin generation.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The memory limit the helper verified before it read any input, once
    /// [`Self::initialize`] has succeeded.
    pub fn memory_limit(&self) -> Option<MemoryLimit> {
        self.limit.get().copied()
    }

    /// The helper's process id, once it was spawned.
    pub fn pid(&self) -> Option<u32> {
        self.pid.get().copied().flatten()
    }

    /// Follows the host's status.
    pub fn status(&self) -> watch::Receiver<HostStatus> {
        self.status.subscribe()
    }

    /// Asks the plugin about `call`, a call to a tool its manifest lists.
    /// Calls are serialized: one runs at a time, and the others wait for it.
    ///
    /// The call is checked before anything is sent: the id's length, the
    /// arguments' depth (walked without recursion), and their JSON length,
    /// which is serialized into a buffer that stops at the limit.
    ///
    /// Cancelling `signal` while the helper runs the call kills it and
    /// tears it down before this returns, and nothing the call staged is
    /// returned. So does a missed deadline, a protocol error, or
    /// [`Self::shutdown`]. Dropping the future instead kills the helper at
    /// once and leaves the teardown to the next operation.
    pub async fn on_tool_call(
        &self,
        call: ToolCallView<'_>,
        signal: Option<&AbortSignal>,
    ) -> Result<CheckedVeto, CallError> {
        if !self.manifest().vetoes(call.tool_name) {
            return Err(CallError::NotListed);
        }
        if call.tool_call_id.len() > MAX_TOOL_CALL_ID_BYTES {
            return Err(CallError::InvalidCall(format!(
                "the call id is {} bytes, over {MAX_TOOL_CALL_ID_BYTES}",
                call.tool_call_id.len()
            )));
        }
        let args_json = encode_args(call.args)?;
        let closing = self.closing.signal();
        let mut state = tokio::select! {
            biased;
            () = cancelled(signal) => return Err(CallError::Cancelled { exit: None }),
            () = closing.cancelled() => return Err(CallError::Unavailable),
            state = self.state.lock() => state,
        };
        let helper = match &mut *state {
            State::New | State::Stopped(_) => return Err(CallError::Unavailable),
            State::Live(helper) if helper.abandoned || helper.stop.is_some() || !helper.ready => {
                // An earlier future was dropped mid-request (its kill was
                // sent then), or a teardown is unfinished: continue it.
                self.teardown(&mut state, Plan::Kill).await;
                return Err(CallError::Unavailable);
            }
            State::Live(helper) => helper,
        };
        if cancelled_now(signal) {
            return Err(CallError::Cancelled { exit: None });
        }
        let Some(id) = helper.next_id.checked_add(1) else {
            return Err(CallError::InvalidCall(
                "the request ids are exhausted".to_owned(),
            ));
        };
        helper.next_id = id;
        let request = CallRequest {
            id,
            generation: self.generation,
            tool_call_id: call.tool_call_id.to_owned(),
            tool_name: call.tool_name.to_owned(),
            args_json,
        };
        // Until the exchange settles, dropping this future kills the helper
        // and leaves it for the next operation to tear down.
        helper.abandoned = true;
        let kill = KillOnDrop(Some(helper.process.kill_switch()));
        let mut started = false;
        let status = &self.status;
        let generation = self.generation;
        let result = exchange(
            helper,
            Some(protocol::frame(Kind::Call, &request.encode())),
            Instant::now() + self.limits.call_timeout + CALL_GRACE,
            signal,
            &closing,
            |kind, payload| match kind {
                Kind::Started => {
                    let started_id =
                        protocol::decode_started(&payload).map_err(|error| error.to_string())?;
                    if started || started_id != id {
                        return Err(format!("Started for request {started_id}, not {id}"));
                    }
                    started = true;
                    status.send_replace(HostStatus::Calling(id));
                    Ok(None)
                }
                Kind::Reply if started => {
                    let reply = Reply::decode(&payload).map_err(|error| error.to_string())?;
                    check_reply(&reply, id, generation)?;
                    Ok(Some(reply))
                }
                Kind::Fatal => Err(describe_fatal(&payload)),
                _ => Err(format!("unexpected {kind:?} during a call")),
            },
        )
        .await;
        kill.disarm();
        // The call settled here; nothing was killed on a drop.
        helper.abandoned = false;
        let reply = match result {
            Ok(reply) => {
                self.status.send_replace(HostStatus::Idle);
                reply
            }
            Err(interrupt) => {
                let exit = self.teardown(&mut state, interrupt.plan()).await;
                return Err(match interrupt {
                    Interrupt::Cancelled => CallError::Cancelled { exit: Some(exit) },
                    Interrupt::Closing => CallError::ShutDown { exit },
                    Interrupt::Deadline => CallError::Timeout { exit },
                    other => CallError::Helper {
                        message: other.describe("during a call"),
                        exit,
                    },
                });
            }
        };
        drop(state);
        self.answer(reply)
    }

    /// Maps a checked reply to the caller's result, applying the host's log
    /// quota.
    fn answer(&self, reply: Reply) -> Result<CheckedVeto, CallError> {
        let veto = match reply.outcome {
            Outcome::Continue => ToolCallVeto::Continue,
            Outcome::Block { reason, terminate } => ToolCallVeto::Block { reason, terminate },
            Outcome::GuestError(message) => return Err(CallError::Guest(message)),
            Outcome::Trap(kind, message) => return Err(CallError::Trap { kind, message }),
        };
        let mut dropped_logs = reply.dropped_logs;
        let mut logs = Vec::with_capacity(reply.logs.len());
        for line in reply.logs {
            let kept = self
                .session_log_bytes
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                    used.checked_add(line.text.len())
                        .filter(|total| *total <= MAX_SESSION_LOG_BYTES)
                })
                .is_ok();
            if kept {
                logs.push(line);
            } else {
                dropped_logs = dropped_logs.saturating_add(1);
            }
        }
        Ok(CheckedVeto {
            veto,
            logs,
            dropped_logs,
            instance: reply.instance,
        })
    }

    /// Stops the helper and awaits it. A pending call or start is
    /// interrupted (it then kills the helper and returns `ShutDown`); an
    /// idle helper is asked to exit and killed if it does not within the
    /// shutdown timeout; a teardown left unfinished by a dropped or
    /// bounded operation is continued from where it stopped. Later calls
    /// return [`CallError::Unavailable`].
    ///
    /// The report is complete ([`ExitReport::is_complete`]) once the helper
    /// has been reaped and its pipes have closed; every later `shutdown`
    /// returns that same report. An unfinished report means a wait ran out
    /// after [`HostLimits::teardown_timeout`]: the host keeps the helper,
    /// and calling `shutdown` again continues. Dropping this future at any
    /// point leaves the helper and the progress in the host.
    pub async fn shutdown(&self) -> ExitReport {
        self.closing.abort();
        let mut state = self.state.lock().await;
        let plan = match &*state {
            State::New => {
                *state = State::Stopped(ExitReport::default());
                self.status.send_replace(HostStatus::Stopped);
                return ExitReport::default();
            }
            State::Stopped(exit) => return exit.clone(),
            State::Live(helper) if helper.abandoned || !helper.ready => Plan::Kill,
            State::Live(_) => Plan::Graceful,
        };
        self.teardown(&mut state, plan).await
    }

    /// Runs or continues the teardown of the helper in `state`, which
    /// stays there until it is complete.
    async fn teardown(&self, state: &mut State, plan: Plan) -> ExitReport {
        let helper = match state {
            State::New => return ExitReport::default(),
            State::Stopped(exit) => return exit.clone(),
            State::Live(helper) => helper,
        };
        self.status.send_replace(HostStatus::Stopping);
        let report = helper.settle(plan, &self.limits).await;
        if report.is_complete() {
            *state = State::Stopped(report.clone());
            self.status.send_replace(HostStatus::Stopped);
        }
        report
    }
}

/// The arguments as JSON, refused when they nest deeper than
/// [`MAX_ARGS_DEPTH`] or serialize to more than [`MAX_ARGS_BYTES`].
///
/// [`walk_args`] checks both before serializing, which recurses: without
/// recursion, and stopping as soon as a lower bound on the serialized
/// length passes the limit, so it visits at most about `MAX_ARGS_BYTES`
/// values however wide the input. The writer then also stops at the
/// limit. The caller's `Value` itself, already allocated, is outside the
/// host's bounds.
fn encode_args(args: &Value) -> Result<String, CallError> {
    walk_args(args).0?;
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len() + bytes.len() > MAX_ARGS_BYTES {
                return Err(std::io::ErrorKind::FileTooLarge.into());
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Bounded(Vec::new());
    serde_json::to_writer(&mut out, args).map_err(|_| CallError::ArgumentsTooLarge)?;
    // serde_json writes valid UTF-8.
    String::from_utf8(out.0).map_err(|_| CallError::ArgumentsTooLarge)
}

/// The pre-serialization check of [`encode_args`], and how many values it
/// visited. Each value adds at least the bytes its JSON must contain: one
/// for a number, four for `null`, `true`, or `false` (five not needed for
/// a bound), two plus the length for a string, two for a container, and
/// three plus the length for each object key; separators are not counted.
fn walk_args(args: &Value) -> (Result<(), CallError>, usize) {
    enum Level<'a> {
        Array(std::slice::Iter<'a, Value>),
        Object(serde_json::map::Iter<'a>),
    }
    // One iterator per open container: the stack never holds more than
    // `MAX_ARGS_DEPTH` entries, whatever the input's size.
    let mut stack: Vec<Level<'_>> = Vec::new();
    let mut next = Some(args);
    let mut lower = 0usize;
    let mut visited = 0usize;
    loop {
        if let Some(value) = next.take() {
            visited += 1;
            lower = lower.saturating_add(match value {
                Value::Null | Value::Bool(_) => 4,
                Value::Number(_) => 1,
                Value::String(text) => text.len().saturating_add(2),
                Value::Array(_) | Value::Object(_) => 2,
            });
            if lower > MAX_ARGS_BYTES {
                return (Err(CallError::ArgumentsTooLarge), visited);
            }
            let level = match value {
                Value::Array(items) => Some(Level::Array(items.iter())),
                Value::Object(members) => Some(Level::Object(members.iter())),
                _ => None,
            };
            if let Some(level) = level {
                if stack.len() >= MAX_ARGS_DEPTH {
                    return (
                        Err(CallError::InvalidCall(format!(
                            "the arguments nest deeper than {MAX_ARGS_DEPTH} levels"
                        ))),
                        visited,
                    );
                }
                stack.push(level);
            }
        }
        let Some(top) = stack.last_mut() else {
            break;
        };
        next = match top {
            Level::Array(items) => items.next(),
            Level::Object(members) => members.next().map(|(key, value)| {
                lower = lower.saturating_add(key.len().saturating_add(3));
                value
            }),
        };
        if next.is_none() {
            stack.pop();
        }
    }
    (Ok(()), visited)
}

fn check_hello(hello: &Hello, limits: &HostLimits, generation: u64) -> Result<(), String> {
    if hello.protocol != PROTOCOL_VERSION {
        return Err(format!(
            "protocol {} is not {PROTOCOL_VERSION}",
            hello.protocol
        ));
    }
    if hello.build != BUILD {
        return Err(format!(
            "the helper is build {:?}, not {BUILD:?}",
            protocol::bounded(&hello.build, 128)
        ));
    }
    if hello.generation != generation {
        return Err(format!(
            "generation {} is not {generation}",
            hello.generation
        ));
    }
    let limit = hello.limit;
    let expected = if cfg!(windows) {
        LimitMechanism::JobCommit
    } else {
        LimitMechanism::AddressSpace
    };
    let consistent = match limit.mechanism {
        LimitMechanism::AddressSpace => {
            limit.baseline.checked_add(limit.budget) == Some(limit.limit)
                && (!cfg!(target_os = "linux") || limit.baseline <= super::MAX_LINUX_BASELINE)
        }
        LimitMechanism::JobCommit => limit.limit == limit.budget,
    };
    if limit.mechanism != expected || limit.budget != limits.memory_budget || !consistent {
        return Err(format!(
            "the helper reports an unexpected memory limit {limit:?}"
        ));
    }
    Ok(())
}

fn check_reply(reply: &Reply, id: u64, generation: u64) -> Result<(), String> {
    if reply.id != id || reply.generation != generation {
        return Err(format!(
            "a reply for request {} generation {}, not {id} generation {generation}",
            reply.id, reply.generation
        ));
    }
    if let Outcome::Block { reason, .. } = &reply.outcome
        && reason.len() > MAX_REASON_BYTES
    {
        return Err("a block reason over the limit".to_owned());
    }
    let mut total = 0usize;
    for line in &reply.logs {
        total = total.saturating_add(line.text.len());
        if line.text.len() > MAX_LOG_LINE || total > MAX_LOGS_PER_CALL {
            return Err("log lines over the call's quota".to_owned());
        }
    }
    Ok(())
}

fn describe_fatal(payload: &[u8]) -> String {
    match protocol::decode_fatal(payload) {
        Ok((code, message)) => {
            format!("the helper reported a protocol error (exit {code}): {message}")
        }
        Err(error) => format!("a malformed Fatal frame: {error}"),
    }
}

fn cancelled_now(signal: Option<&AbortSignal>) -> bool {
    signal.is_some_and(AbortSignal::aborted)
}

async fn cancelled(signal: Option<&AbortSignal>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending().await,
    }
}

/// Spawns the helper with its limits as arguments ([`process::spawn`] owns
/// the platform details; it creates the process after its last await).
async fn spawn(
    command: &HelperCommand,
    limits: &HostLimits,
    generation: u64,
) -> Result<process::Spawned, String> {
    let mut args = command.leading_args.clone();
    for (flag, value) in [
        ("--generation", generation.to_string()),
        ("--memory-budget", limits.memory_budget.to_string()),
        ("--max-component", limits.max_component_bytes.to_string()),
        ("--call-fuel", limits.call_fuel.to_string()),
        (
            "--call-timeout-ms",
            limits.call_timeout.as_millis().to_string(),
        ),
    ] {
        args.push(flag.into());
        args.push(value.into());
    }
    process::spawn(&command.program, &args, limits.memory_budget).await
}

/// The helper process, its pipes, and its teardown progress.
struct Helper {
    process: Process,
    stdin: Option<PipeIn>,
    stdout: Option<PipeOut>,
    reader: FrameReader,
    stderr: StderrTail,
    next_id: u64,
    /// The component is loaded; calls may be sent.
    ready: bool,
    /// The [`CompileQueue`] slot, held from admission until the component
    /// has loaded or teardown has observed the helper's exit. A dropped or
    /// failed start keeps it while the helper may still be compiling.
    /// Dropping the host drops it with the helper.
    admission: Option<OwnedSemaphorePermit>,
    /// A request's future was dropped mid-request: the helper was killed,
    /// and the next operation must tear it down.
    abandoned: bool,
    /// Teardown has started; what it has done so far.
    stop: Option<Stop>,
}

impl std::fmt::Debug for Helper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Helper")
            .field("pid", &self.process.id())
            .field("ready", &self.ready)
            .field("abandoned", &self.abandoned)
            .field("admitted", &self.admission.is_some())
            .field("stop", &self.stop)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct Stop {
    plan: Plan,
    /// When a graceful or exited plan stops waiting and kills.
    deadline: Instant,
    shutdown_sent: bool,
    killed: bool,
    exit: Option<Exit>,
}

struct KillOnDrop(Option<KillSwitch>);

impl KillOnDrop {
    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(switch) = self.0.take() {
            switch.kill();
        }
    }
}

impl Helper {
    fn new(spawned: process::Spawned, admission: OwnedSemaphorePermit) -> Self {
        Self {
            process: spawned.process,
            stdin: Some(spawned.stdin),
            stdout: Some(spawned.stdout),
            reader: FrameReader::default(),
            stderr: StderrTail {
                pipe: Some(spawned.stderr),
                tail: Vec::new(),
            },
            next_id: 0,
            ready: false,
            admission: Some(admission),
            abandoned: false,
            stop: None,
        }
    }

    /// Runs or continues teardown under `plan` (a plan already in progress
    /// only escalates) and reports the state reached. Every step is
    /// recorded in `self` before or as it completes and every await is
    /// cancel-safe, so a dropped call loses nothing and the next continues.
    ///
    /// 1. A graceful plan sends `Shutdown` once. A kill is sent now for a
    ///    kill plan, before the input closes; then the input is closed.
    /// 2. Unless the plan is a kill, the helper's own exit is awaited until
    ///    the plan's deadline; then it is killed if it has not exited.
    /// 3. The exit is awaited for up to `teardown_timeout`, then the pipes
    ///    are drained to end of file for up to as long again.
    async fn settle(&mut self, plan: Plan, limits: &HostLimits) -> ExitReport {
        let Self {
            process,
            stdin,
            stdout,
            stderr,
            stop,
            abandoned,
            admission,
            ..
        } = self;
        let grace = match plan {
            Plan::Graceful => limits.shutdown_timeout,
            Plan::Exited => EXIT_GRACE,
            Plan::Kill => Duration::ZERO,
        };
        let stop = stop.get_or_insert_with(|| Stop {
            plan,
            deadline: Instant::now() + grace,
            shutdown_sent: false,
            killed: false,
            exit: None,
        });
        stop.plan = stop.plan.max(plan);
        if *abandoned {
            // The dropped request's guard already sent the kill.
            stop.killed = true;
        }

        if stop.plan == Plan::Graceful && !stop.shutdown_sent {
            stop.shutdown_sent = true;
            if let Some(input) = stdin.as_mut() {
                let shutdown = protocol::frame(Kind::Shutdown, &[]);
                let _ = tokio::time::timeout_at(stop.deadline, input.write_all(&shutdown)).await;
            }
        }
        if stop.exit.is_none() {
            stop.exit = process.try_exit();
        }
        // A kill goes before the input closes, so the helper's watchdog
        // cannot exit on its own first and blur why it ended.
        if stop.plan == Plan::Kill && stop.exit.is_none() && !stop.killed {
            process.kill();
            stop.killed = true;
        }
        *stdin = None;
        if stop.exit.is_none() && !stop.killed && stop.plan != Plan::Kill {
            stop.exit = wait_exit(process, stderr, stop.deadline).await;
        }
        if stop.exit.is_none() && !stop.killed {
            process.kill();
            stop.killed = true;
        }
        if stop.exit.is_none() {
            stop.exit = wait_exit(process, stderr, Instant::now() + limits.teardown_timeout).await;
        }
        if stop.exit.is_some() {
            // The exit is observed: the helper compiles nothing more, even
            // if its pipes are still open (another process may hold them).
            *admission = None;
        }
        let drained = stop.exit.is_some()
            && tokio::time::timeout(limits.teardown_timeout, drain(stdout, stderr))
                .await
                .is_ok();
        ExitReport {
            spawned: true,
            killed: stop.killed,
            code: stop.exit.and_then(|exit| exit.code),
            signal: stop.exit.and_then(|exit| exit.signal),
            reaped: stop.exit.is_some(),
            drained,
            stderr: escape(&stderr.tail),
        }
    }
}

/// Awaits the helper's exit until `deadline`, reading stderr meanwhile so
/// the helper never blocks on a full pipe.
async fn wait_exit(
    process: &mut Process,
    stderr: &mut StderrTail,
    deadline: Instant,
) -> Option<Exit> {
    tokio::time::timeout_at(deadline, async {
        loop {
            tokio::select! {
                exit = process.wait() => return exit.ok(),
                () = stderr.pump() => {}
            }
        }
    })
    .await
    .ok()
    .flatten()
}

/// Reads stdout and stderr to end of file. Cancel-safe: a pipe is cleared
/// only at its end, and stderr keeps its tail.
async fn drain(stdout: &mut Option<PipeOut>, stderr: &mut StderrTail) {
    let mut sink = vec![0u8; 16 * 1024];
    while stdout.is_some() || stderr.pipe.is_some() {
        tokio::select! {
            read = read_some(stdout.as_mut(), &mut sink) => {
                if !matches!(read, Ok(count) if count > 0) {
                    *stdout = None;
                }
            }
            () = stderr.pump() => {}
        }
    }
}

/// `bytes` as text with control characters other than newline and tab
/// escaped, so helper output cannot drive a terminal.
fn escape(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .flat_map(|ch| -> Box<dyn Iterator<Item = char>> {
            if ch.is_control() && ch != '\n' && ch != '\t' {
                Box::new(ch.escape_default())
            } else {
                Box::new(std::iter::once(ch))
            }
        })
        .collect()
}

async fn read_some(pipe: Option<&mut PipeOut>, buffer: &mut [u8]) -> std::io::Result<usize> {
    match pipe {
        Some(pipe) => pipe.read(buffer).await,
        None => std::future::pending().await,
    }
}

/// The end of the helper's standard error, read whenever the parent
/// awaits the helper, so the helper never blocks on a full pipe.
struct StderrTail {
    pipe: Option<PipeOut>,
    tail: Vec<u8>,
}

impl StderrTail {
    /// Reads one chunk; cancel-safe, and pending forever once closed.
    async fn pump(&mut self) {
        let Some(pipe) = self.pipe.as_mut() else {
            return std::future::pending().await;
        };
        let mut chunk = [0u8; 4096];
        match pipe.read(&mut chunk).await {
            Ok(count) if count > 0 => {
                self.tail.extend_from_slice(&chunk[..count]);
                if self.tail.len() > STDERR_TAIL {
                    let excess = self.tail.len() - STDERR_TAIL;
                    self.tail.drain(..excess);
                }
            }
            _ => self.pipe = None,
        }
    }
}

/// Reads frames from the helper's stdout. Its state lives here, not in the
/// future, so `next` is cancel-safe: dropping it loses no bytes.
#[derive(Debug, Default)]
struct FrameReader {
    header: [u8; HEADER_LEN],
    filled: usize,
    payload: Option<(Kind, Vec<u8>, usize)>,
}

impl FrameReader {
    async fn next(
        &mut self,
        stdout: &mut (impl AsyncRead + Unpin + ?Sized),
    ) -> Result<Option<(Kind, Vec<u8>)>, FrameError> {
        loop {
            if let Some((_, buffer, filled)) = self.payload.as_mut() {
                if *filled == buffer.len() {
                    self.filled = 0;
                    return Ok(self.payload.take().map(|(kind, buffer, _)| (kind, buffer)));
                }
                let count = stdout
                    .read(&mut buffer[*filled..])
                    .await
                    .map_err(FrameError::Io)?;
                if count == 0 {
                    return Err(FrameError::Truncated);
                }
                *filled += count;
                continue;
            }
            let count = stdout
                .read(&mut self.header[self.filled..])
                .await
                .map_err(FrameError::Io)?;
            if count == 0 {
                return if self.filled == 0 {
                    Ok(None)
                } else {
                    Err(FrameError::Truncated)
                };
            }
            self.filled += count;
            if self.filled == HEADER_LEN {
                // The cap is checked before the payload is allocated.
                let (kind, len) = protocol::check_header(self.header, protocol::parent_cap)?;
                self.payload = Some((kind, vec![0u8; len], 0));
            }
        }
    }
}

/// Why an exchange with the helper stopped without its answer.
enum Interrupt {
    Cancelled,
    Closing,
    Deadline,
    /// The helper closed its output: it is exiting.
    Exited,
    Broken(String),
}

impl Interrupt {
    fn describe(&self, when: &str) -> String {
        match self {
            Self::Cancelled => format!("cancelled {when}"),
            Self::Closing => format!("shut down {when}"),
            Self::Deadline => format!("timed out {when}"),
            Self::Exited => format!("exited {when}"),
            Self::Broken(message) => format!("{message} ({when})"),
        }
    }

    /// A helper that closed its output is exiting by itself: its exit is
    /// awaited briefly, so a crash is not reported as the parent's kill.
    /// Anything else kills it at once.
    fn plan(&self) -> Plan {
        match self {
            Self::Exited => Plan::Exited,
            _ => Plan::Kill,
        }
    }
}

/// Writes `send`, if any, and reads frames until `on_frame` returns an
/// answer, while draining stderr. Every branch is cancel-safe, so the loop
/// never loses a byte of either stream.
async fn exchange<T>(
    helper: &mut Helper,
    send: Option<Vec<u8>>,
    deadline: Instant,
    signal: Option<&AbortSignal>,
    closing: &AbortSignal,
    mut on_frame: impl FnMut(Kind, Vec<u8>) -> Result<Option<T>, String>,
) -> Result<T, Interrupt> {
    let Helper {
        stdin,
        stdout,
        reader,
        stderr,
        ..
    } = helper;
    let Some(stdout) = stdout.as_mut() else {
        return Err(Interrupt::Exited);
    };
    let mut pending = send.unwrap_or_default();
    let mut written = 0usize;
    loop {
        let writing = written < pending.len();
        tokio::select! {
            biased;
            () = closing.cancelled() => return Err(Interrupt::Closing),
            () = cancelled(signal) => return Err(Interrupt::Cancelled),
            () = tokio::time::sleep_until(deadline) => return Err(Interrupt::Deadline),
            frame = reader.next(stdout) => match frame {
                Ok(Some((kind, payload))) => match on_frame(kind, payload) {
                    Ok(Some(answer)) => return Ok(answer),
                    Ok(None) => {}
                    Err(message) => return Err(Interrupt::Broken(message)),
                },
                Ok(None) => return Err(Interrupt::Exited),
                Err(error) => return Err(Interrupt::Broken(error.to_string())),
            },
            result = write_some(stdin.as_mut(), &pending[written..]), if writing => match result {
                Ok(count) => written += count,
                // The helper closed its input; read on for its report.
                Err(_) => {
                    pending.clear();
                    written = 0;
                }
            },
            () = stderr.pump() => {}
        }
    }
}

async fn write_some(stdin: Option<&mut PipeIn>, bytes: &[u8]) -> std::io::Result<usize> {
    match stdin {
        Some(stdin) => match stdin.write(bytes).await? {
            0 => Err(std::io::ErrorKind::WriteZero.into()),
            count => Ok(count),
        },
        None => Err(std::io::ErrorKind::BrokenPipe.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_are_checked() {
        assert!(HostLimits::default().check().is_ok());
        let mut limits = HostLimits {
            memory_budget: 1 << 20,
            ..HostLimits::default()
        };
        assert!(limits.check().unwrap_err().contains("memory budget"));
        limits.memory_budget = 64 << 20;
        limits.max_component_bytes = MAX_COMPONENT_BYTES + 1;
        assert!(limits.check().unwrap_err().contains("component limit"));
        limits.max_component_bytes = 1;
        limits.call_timeout = Duration::ZERO;
        assert!(limits.check().unwrap_err().contains("call timeout"));
        limits.call_timeout = Duration::from_secs(1);
        limits.teardown_timeout = Duration::from_micros(10);
        assert!(limits.check().unwrap_err().contains("teardown timeout"));
    }

    #[test]
    fn hellos_must_match_this_build_and_limit() {
        let limits = HostLimits::default();
        let mut hello = Hello {
            protocol: PROTOCOL_VERSION,
            build: BUILD.to_owned(),
            generation: 2,
            limit: MemoryLimit {
                mechanism: if cfg!(windows) {
                    LimitMechanism::JobCommit
                } else {
                    LimitMechanism::AddressSpace
                },
                baseline: if cfg!(windows) { 1 } else { 100 },
                limit: if cfg!(windows) {
                    limits.memory_budget
                } else {
                    100 + limits.memory_budget
                },
                budget: limits.memory_budget,
            },
        };
        assert!(check_hello(&hello, &limits, 2).is_ok());
        assert!(
            check_hello(&hello, &limits, 3)
                .unwrap_err()
                .contains("generation")
        );
        hello.limit.limit += 1;
        assert!(
            check_hello(&hello, &limits, 2)
                .unwrap_err()
                .contains("memory limit")
        );
        hello.limit.limit -= 1;
        hello.build = "other".to_owned();
        assert!(
            check_hello(&hello, &limits, 2)
                .unwrap_err()
                .contains("build")
        );
        hello.build = BUILD.to_owned();
        hello.protocol += 1;
        assert!(
            check_hello(&hello, &limits, 2)
                .unwrap_err()
                .contains("protocol")
        );
        hello.protocol -= 1;
        // On Linux the absolute address-space limit is bounded too.
        #[cfg(target_os = "linux")]
        {
            hello.limit.baseline = super::super::MAX_LINUX_BASELINE + 1;
            hello.limit.limit = hello.limit.baseline + limits.memory_budget;
            assert!(
                check_hello(&hello, &limits, 2)
                    .unwrap_err()
                    .contains("memory limit")
            );
        }
    }

    #[test]
    fn replies_must_match_the_request_and_quotas() {
        let reply = |id, generation, outcome, logs| Reply {
            id,
            generation,
            instance: 1,
            outcome,
            logs,
            dropped_logs: 0,
        };
        assert!(check_reply(&reply(1, 1, Outcome::Continue, Vec::new()), 1, 1).is_ok());
        assert!(check_reply(&reply(2, 1, Outcome::Continue, Vec::new()), 1, 1).is_err());
        assert!(check_reply(&reply(1, 2, Outcome::Continue, Vec::new()), 1, 1).is_err());
        let long = Outcome::Block {
            reason: "r".repeat(MAX_REASON_BYTES + 1),
            terminate: false,
        };
        assert!(check_reply(&reply(1, 1, long, Vec::new()), 1, 1).is_err());
        let line = |len| LogLine {
            level: protocol::LogLevel::Info,
            text: "l".repeat(len),
        };
        assert!(
            check_reply(
                &reply(1, 1, Outcome::Continue, vec![line(MAX_LOG_LINE + 1)]),
                1,
                1
            )
            .is_err()
        );
        let many = (0..17).map(|_| line(MAX_LOG_LINE)).collect();
        assert!(check_reply(&reply(1, 1, Outcome::Continue, many), 1, 1).is_err());
    }

    #[test]
    fn stderr_is_escaped() {
        assert_eq!(escape(b"a\x1b[31mb\n\tc\x07"), "a\\u{1b}[31mb\n\tc\\u{7}");
    }

    /// Builds `depth` nested arrays without recursion and forgets the value
    /// afterwards: `serde_json::Value`'s own drop recurses.
    fn nested(depth: usize) -> std::mem::ManuallyDrop<Value> {
        let mut value = Value::from(0);
        for _ in 0..depth {
            value = Value::Array(vec![value]);
        }
        std::mem::ManuallyDrop::new(value)
    }

    #[test]
    fn arguments_are_bounded_before_and_while_serializing() {
        assert_eq!(
            encode_args(&serde_json::json!({ "path": "a" })).unwrap(),
            r#"{"path":"a"}"#
        );
        // Exactly the depth limit serializes; one more is refused before
        // serializing. Deep enough to overflow a recursive walk is refused
        // too (the thread's stack is the default 2 MiB).
        let ok = nested(MAX_ARGS_DEPTH);
        assert!(encode_args(&ok).is_ok());
        let deep = nested(MAX_ARGS_DEPTH + 1);
        assert!(matches!(encode_args(&deep), Err(CallError::InvalidCall(_))));
        let huge_depth = nested(1_000_000);
        assert!(matches!(
            // `ManuallyDrop` keeps the value's recursive drop from running.
            std::thread::spawn(move || encode_args(&huge_depth))
                .join()
                .expect("no stack overflow"),
            Err(CallError::InvalidCall(_))
        ));
        // Objects count as levels too, and wide values are fine.
        let mut object = Value::from(0);
        for _ in 0..=MAX_ARGS_DEPTH {
            object = serde_json::json!({ "k": object });
        }
        assert!(matches!(
            encode_args(&object),
            Err(CallError::InvalidCall(_))
        ));
        let wide = Value::Array((0..10_000).map(Value::from).collect());
        assert!(encode_args(&wide).is_ok());
        // Width: an array of more numbers than the byte limit is refused
        // after visiting about `MAX_ARGS_BYTES` of them, not all of them.
        let count = MAX_ARGS_BYTES + 200_000;
        let too_wide = Value::Array(vec![Value::from(0); count]);
        let (result, visited) = walk_args(&too_wide);
        assert_eq!(result, Err(CallError::ArgumentsTooLarge));
        assert!(visited <= MAX_ARGS_BYTES, "visited {visited} of {count}");
        // A long key counts too, before its value is visited.
        let mut members = serde_json::Map::new();
        members.insert("k".repeat(MAX_ARGS_BYTES), Value::Null);
        assert_eq!(
            walk_args(&Value::Object(members)),
            (Err(CallError::ArgumentsTooLarge), 2)
        );
        // The length limit: exactly at it passes, one byte over fails.
        let at = Value::String("a".repeat(MAX_ARGS_BYTES - 2));
        assert_eq!(encode_args(&at).unwrap().len(), MAX_ARGS_BYTES);
        let over = Value::String("a".repeat(MAX_ARGS_BYTES - 1));
        assert_eq!(encode_args(&over), Err(CallError::ArgumentsTooLarge));
    }
}
