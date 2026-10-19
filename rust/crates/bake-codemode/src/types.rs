//! The engine's public data: what a script run takes, what it reports, and
//! how it ends. Ported from Pi's `packages/codemode/src/types.ts` and
//! `runtime/protocol.ts` (v1.3.0; MIT, see `NOTICE`), with the host's
//! decoding moved here from `runtime/execution.ts`.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::source::Source;

/// The QuickJS heap limit by default: an allocation beyond it is a catchable
/// `InternalError: out of memory` in the script.
pub const DEFAULT_HEAP_BYTES: u32 = 256 << 20;
/// The guest's linear-memory limit by default, a little above the heap limit
/// for the allocator's own overhead; growing past it ends the run as
/// [`Failure::OutOfMemory`].
pub const DEFAULT_LINEAR_MEMORY_BYTES: usize = 320 << 20;
/// The most calls a script may have waiting for results; the next one fails
/// the run as [`Failure::Protocol`]. Bake 0.3's `maxPendingCalls`. Pi has no
/// limit, but a script that calls in a loop without awaiting would otherwise
/// fill the heap with calls the VM can never settle.
pub const MAX_PENDING_CALLS: usize = 128;
/// QuickJS's guard on its own stack, `quickjs-wasi`'s `MAX_STACK_SIZE`: deep
/// recursion is a catchable `RangeError`.
pub const MAX_STACK_BYTES: u32 = 512 * 1024;

/// Values a script reads with `load(key)`, by key.
pub type StoreValues = Map<String, Value>;

/// A tool as the script sees it: `tools.<js_name>`, `tools["<name>"]`, and its
/// `ALL_TOOLS` entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolDeclaration {
    /// The tool's name; calls report it.
    pub name: String,
    /// The identifier the script uses. Default: `to_codemode_identifier(name)`.
    /// A host that replays a run passes the one the run started with.
    pub js_name: Option<String>,
    /// Listed in `ALL_TOOLS`.
    pub description: Option<String>,
}

impl ToolDeclaration {
    /// A tool declared by name alone.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }
}

/// A function the script calls by its own name instead of on `tools`, such as
/// the discovery functions: an identifier, or `namespace.member`, which groups
/// members into a frozen namespace object. Calls to globals are not tool calls.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlobalDeclaration {
    /// The global's name.
    pub name: String,
    /// The host receives every argument as an array instead of the first one.
    pub spread: bool,
}

impl GlobalDeclaration {
    /// A global that receives its first argument.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            spread: false,
        }
    }

    /// A global that receives every argument as an array.
    pub fn spread(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            spread: true,
        }
    }
}

/// Pi's discovery functions, which the host answers: `searchTools(query,
/// { limit?, namespace? })`, `describeTool(name)`, and
/// `describeNamespace(name)`, each receiving its arguments as an array.
pub fn discovery_globals() -> Vec<GlobalDeclaration> {
    ["searchTools", "describeTool", "describeNamespace"]
        .into_iter()
        .map(GlobalDeclaration::spread)
        .collect()
}

/// Resource limits of one run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// QuickJS's heap limit (`qjs_set_memory_limit`); `None` leaves only the
    /// linear-memory limit.
    pub heap_bytes: Option<u32>,
    /// The store's limit on the guest's linear memory (`StoreLimits`).
    pub linear_memory_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            heap_bytes: Some(DEFAULT_HEAP_BYTES),
            linear_memory_bytes: DEFAULT_LINEAR_MEMORY_BYTES,
        }
    }
}

/// Deterministic mode (Pi's `CodemodeSandboxBase.start()`): the same script,
/// start time, seed, declarations, and store, answered with the same results
/// in the same order, makes the same calls and output. The VM runs in UTC,
/// its clock reads `started_at_ms` and then the time of the result being
/// processed, its randomness comes from `seed`, `WeakRef` and
/// `FinalizationRegistry` do not exist, and answers are applied in `seq`
/// order ([`crate::Run::answer_in_order`]). The run reports
/// [`Event::Started`] and [`Event::Idle`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Deterministic {
    /// The VM clock's start, in milliseconds since the Unix epoch.
    pub started_at_ms: f64,
    /// Seeds `Math.random()` and the VM's other randomness.
    pub seed: u32,
}

/// Called once when a run's script thread has exited, or when the run could
/// not start: Pi's `acquireWorker` release, for admission limits that must
/// count the thread until it is gone.
pub struct ExitHook(Box<dyn FnOnce() + Send>);

impl ExitHook {
    /// Wraps the callback.
    pub fn new(callback: impl FnOnce() + Send + 'static) -> Self {
        Self(Box::new(callback))
    }

    pub(crate) fn call(self) {
        (self.0)();
    }
}

impl fmt::Debug for ExitHook {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ExitHook")
    }
}

/// Everything one script run needs.
#[derive(Debug, Default)]
pub struct ExecuteRequest {
    /// The script, an async function body, with its options; `timeout_ms` is
    /// the run's deadline, including time spent in calls. No deadline without
    /// it.
    pub source: Source,
    /// The tools on `tools`. Names must be unique.
    pub tools: Vec<ToolDeclaration>,
    /// Globals such as [`discovery_globals`].
    pub globals: Vec<GlobalDeclaration>,
    /// What `load()` reads.
    pub store: StoreValues,
    /// Heap and memory limits.
    pub limits: Limits,
    /// Run deterministically.
    pub deterministic: Option<Deterministic>,
    /// Called when the script thread has exited.
    pub on_exit: Option<ExitHook>,
}

impl ExecuteRequest {
    /// A request for `code` with no options, tools, globals, or store.
    pub fn new(code: impl Into<String>) -> Self {
        Self {
            source: Source::plain(code),
            ..Self::default()
        }
    }

    pub(crate) fn timeout(&self) -> Option<Duration> {
        self.source.options.timeout_ms.map(Duration::from_millis)
    }
}

/// A call the script made. The host answers it with [`crate::Run::answer`].
#[derive(Clone, Debug, PartialEq)]
pub enum HostCall {
    /// `tools.<name>(args)`.
    Tool {
        /// The VM's number for the call, `1`, `2`, ... in the order the script
        /// made its calls, tools and globals together.
        id: u32,
        /// The tool's declared name.
        name: String,
        /// What the script passed, after a JSON round trip; `None` for
        /// `undefined`.
        args: Option<Value>,
    },
    /// A call to a global, such as `searchTools(query)`.
    Global {
        /// The VM's number for the call.
        id: u32,
        /// The global's declared name.
        name: String,
        /// The first argument, or for a spread global every argument as an array.
        args: Option<Value>,
    },
}

impl HostCall {
    /// The call's number.
    pub fn id(&self) -> u32 {
        match self {
            Self::Tool { id, .. } | Self::Global { id, .. } => *id,
        }
    }

    /// The tool's or global's name.
    pub fn name(&self) -> &str {
        match self {
            Self::Tool { name, .. } | Self::Global { name, .. } => name,
        }
    }

    /// The arguments.
    pub fn args(&self) -> Option<&Value> {
        match self {
            Self::Tool { args, .. } | Self::Global { args, .. } => args.as_ref(),
        }
    }
}

/// One item of the script's output, in the order the script produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputItem {
    /// `text()`, or `console.*` with `console` set.
    Text {
        /// The text.
        text: String,
        /// Whether `console.*` produced it.
        console: bool,
    },
    /// `image()`: base64 data of a PNG, JPEG, GIF, or WebP image.
    Image {
        /// Base64 data, without line breaks.
        data: String,
        /// The type detected from the data.
        mime_type: String,
    },
}

/// What a run reports, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Deterministic mode, before the script runs: what the VM is, so a
    /// replay can refuse a different one.
    Started {
        /// Engine versions, prelude hash, deterministic version, and limits.
        fingerprint: String,
    },
    /// The script made a call.
    Call(HostCall),
    /// The script produced output.
    Output(OutputItem),
    /// Deterministic mode: having processed result `seq` (0: the script's
    /// first run), the VM waits again, having issued `issued` calls in total.
    Idle {
        /// The last applied result.
        seq: u32,
        /// Calls issued so far.
        issued: u32,
    },
    /// The run ended; always the last event. Shared, so a large return value
    /// is not copied when [`crate::Run::next`] reports it again.
    Done(Arc<Outcome>),
}

/// Keys the script changed with `store()`. Only successful scripts report
/// writes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StoreWrites {
    /// Keys stored with a value.
    pub set: Map<String, Value>,
    /// Keys stored as `undefined`.
    pub delete: Vec<String>,
}

/// A successful script's result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Completion {
    /// The returned value after a JSON round trip; `None` for `undefined` and
    /// `exit()`.
    pub value: Option<Value>,
    /// The script's `store()` writes; persisting them is up to the caller.
    pub store_writes: StoreWrites,
}

/// An error the script threw or failed to parse.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScriptError {
    /// The error's `name`, when it is an `Error`.
    pub name: Option<String>,
    /// Its message, or the thrown value's text.
    pub message: String,
    /// `Name: message` followed by the script's frames, V8 style.
    pub stack: Option<String>,
}

/// How a run failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The script threw or failed to parse (Pi's `script` kind).
    Exception(ScriptError),
    /// The script waits on a promise nothing can settle: no call is pending
    /// and the VM has no timers. Pi reports it as a script error with this
    /// message.
    NeverSettles(ScriptError),
    /// The deadline expired.
    Timeout {
        /// `Execution timed out after N ms`.
        message: String,
    },
    /// The run was aborted.
    Aborted {
        /// `Execution aborted`, or the message given to
        /// [`crate::Run::abort_with`].
        message: String,
    },
    /// The guest's linear memory reached its limit.
    OutOfMemory {
        /// What could not grow.
        message: String,
    },
    /// The host's own check of the output limits found more output than the
    /// prelude allows. The prelude's limit error is an [`Failure::Exception`].
    OutputLimit {
        /// The limit passed.
        message: String,
    },
    /// The VM failed outside the script's control: a malformed bridge
    /// message, a guest pointer outside its memory, a trap, or a call past
    /// [`MAX_PENDING_CALLS`] (Pi's `sandbox` kind).
    Protocol {
        /// What failed.
        message: String,
    },
}

impl Failure {
    /// The failure's message.
    pub fn message(&self) -> &str {
        match self {
            Self::Exception(error) | Self::NeverSettles(error) => &error.message,
            Self::Timeout { message }
            | Self::Aborted { message }
            | Self::OutOfMemory { message }
            | Self::OutputLimit { message }
            | Self::Protocol { message } => message,
        }
    }
}

/// Deterministic mode: the result the VM was processing when the script
/// ended (0: its first run), and the calls issued by then.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct During {
    /// The result being processed.
    pub seq: u32,
    /// Calls issued.
    pub issued: u32,
}

/// How a run ended. Output was reported as it was produced, so a failed run
/// keeps the output up to its failure.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    /// The completion, or why the run failed.
    pub result: Result<Completion, Failure>,
    /// Deterministic mode, for a script that ended itself: where it ended.
    pub during: Option<During>,
}

impl Outcome {
    pub(crate) fn failed(failure: Failure) -> Self {
        Self {
            result: Err(failure),
            during: None,
        }
    }
}

/// A call's failure. The script's promise rejects with an `Error` whose message
/// is `message` and which carries `properties` (except `name`, `message`, and
/// `stack`), for example `{ structuredOutput }`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CallError {
    /// The error message.
    pub message: String,
    /// Own properties of the script's `Error`.
    pub properties: Option<Map<String, Value>>,
}

impl CallError {
    /// An error with `message` only.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            properties: None,
        }
    }
}

/// A call's answer: the resolved value (`None` for `undefined`) or a rejection.
pub type CallResult = Result<Option<Value>, CallError>;

/// Why an answer was refused; the run is unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnswerError {
    /// The call is not waiting for an answer.
    NotWaiting(u32),
    /// Deterministic mode: the result number was used before or is 0.
    SequenceUsed(u32),
    /// Deterministic mode: the clock value is not finite.
    ClockNotFinite(u32),
    /// Deterministic runs take [`crate::Run::answer_in_order`], others
    /// [`crate::Run::answer`].
    WrongMode,
}

impl fmt::Display for AnswerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotWaiting(id) => write!(formatter, "Call {id} is not waiting for a result"),
            Self::SequenceUsed(seq) => write!(
                formatter,
                "Result number {seq} is not a positive integer used once"
            ),
            Self::ClockNotFinite(seq) => write!(
                formatter,
                "The clock value of result {seq} is not a finite number"
            ),
            Self::WrongMode => formatter.write_str(
                "deterministic runs are answered with answer_in_order, other runs with answer",
            ),
        }
    }
}

impl std::error::Error for AnswerError {}

/// The engine could not be compiled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineError(pub String);

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "the code mode engine is unavailable: {}", self.0)
    }
}

impl std::error::Error for EngineError {}

/// Why a run did not start. Its [`ExitHook`] has been called.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartError {
    /// The request is invalid, for example a global that shadows a built-in.
    InvalidRequest(String),
    /// The script thread could not be started.
    Thread(String),
}

impl fmt::Display for StartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(message) => formatter.write_str(message),
            Self::Thread(message) => {
                write!(formatter, "the script thread could not start: {message}")
            }
        }
    }
}

impl std::error::Error for StartError {}
