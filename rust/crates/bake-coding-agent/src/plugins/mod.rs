//! Trusted native plugins compiled into `bake-rs`: the ordered tool-call
//! veto, the first slice of the proposed native plugin design under D6
//! (`docs/roadmap/rust-0.4/native-plugins.md`, in review). Third-party Wasm
//! plugins, the other contributions, plugin tools, and management are not
//! here.
//!
//! A [`Plugin`] is Bake's own Rust code. Its [`PluginManifest`] names the
//! tools whose calls it may veto, and an [`AgentSession`] built with a
//! [`PluginRegistry`] asks those plugins, before each call to a listed tool,
//! whether the call may run. Semantics follow Pi v1.3.0
//! (`71023aec1546d6d677a65669c5665c0927ce0dd0`, MIT; see the crate's
//! `NOTICE`): `ExtensionRunner.emitToolCall` in
//! `src/core/extensions/runner.ts` asks handlers in order and returns the
//! first `block`, and `AgentSession._beforeToolCall` in
//! `src/core/agent-session.ts` blocks the call when a handler fails
//! ("Extension failed, blocking execution"). Bake differs from Pi where the
//! design requires:
//!
//! - Plugins run in configured order, then by id, and only for the tools
//!   their manifest lists. The registry is fixed when the session is
//!   created, so every call sees the same snapshot.
//! - A plugin sees the call's arguments read-only: there is no allow and no
//!   argument rewrite (Pi's handlers may mutate `event.input`), so the
//!   persisted assistant `toolCall` is what runs. A plugin can only add a
//!   denial; the D24 permission checks inside the tool still decide whatever
//!   it lets through.
//! - A plugin that returns an error, panics, misses its deadline, or
//!   returns an oversized reason is failed: that call is blocked, and every
//!   later call to its listed tools is blocked without asking it, for the
//!   life of the [`AgentSession`] that holds the registry. The failure is
//!   kept in memory only: a session reopened from its file (resume,
//!   `/resume`, a fork, or a new process) asks the plugin again. Restoring
//!   it, for example from the [`DECISION_ENTRY_TYPE`] entries, is open work
//!   that configuration must settle before it registers any plugin. This
//!   slice has no disable, restart, or reload operation.
//! - Cancelling a call fails no plugin. When the call's signal has aborted
//!   before a plugin is asked, or aborts while one runs, its future is
//!   dropped, nothing is decided or recorded, and the next call asks the
//!   plugin again; the agent loop answers the cancelled call `Operation
//!   aborted`. Cancellation is checked before a plugin's earlier failure,
//!   but does not clear it: a plugin that failed before the cancellation
//!   stays failed and blocks the next call.
//! - Every block is written to the session file as a Pi `custom` entry of
//!   type [`DECISION_ENTRY_TYPE`] before the denial `toolResult`; a failed
//!   write stops the run before the tool or another model request. The
//!   call's signal is checked again when that write is admitted under the
//!   session lock: cancelled by then, nothing is written. Cancelled after
//!   admission, the entry stays, but the call's result is still `Operation
//!   aborted`, since the agent loop checks the signal before it reads the
//!   block. That is the tested case in which a decision has no matching
//!   denial; a crash or shutdown before the result is written, or a failed
//!   write of the result, leaves the same trace.
//!
//! [`AgentSession`]: crate::agent_session::AgentSession

mod protected_paths;
pub(crate) mod veto;
pub mod wasm;

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use bake_agent::BoxFuture;
use serde_json::Value;

pub use protected_paths::ProtectedPaths;

/// The `customType` of the session entry that records a plugin's block.
pub const DECISION_ENTRY_TYPE: &str = "bake.plugin.decision";

/// The longest block reason a plugin may return, in bytes; a longer one is
/// an invalid return and fails the plugin.
pub const MAX_REASON_BYTES: usize = 16 * 1024;

/// How long a plugin may take to answer one call unless the registry sets
/// another deadline ([`PluginRegistry::with_call_timeout`]).
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// The most tools one manifest may list.
pub const MAX_VETO_TOOLS: usize = 32;

/// A plugin id: 1 to 32 characters of `a-z`, `0-9`, and `-`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PluginId(String);

impl PluginId {
    /// Validates `id`.
    pub fn new(id: &str) -> Result<Self, PluginError> {
        let valid = (1..=32).contains(&id.len())
            && id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if valid {
            Ok(Self(id.to_owned()))
        } else {
            Err(PluginError::InvalidId(bounded(id)))
        }
    }

    /// The id.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a plugin declares: its id, its version, and the tools whose calls
/// it may veto. Validated when built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginManifest {
    id: PluginId,
    version: String,
    veto_tools: BTreeSet<String>,
}

impl PluginManifest {
    /// A manifest for `id` at `version` that vetoes calls to `veto_tools`.
    /// The version is 1 to 64 printable ASCII characters; each tool name is
    /// 1 to 64 characters of `A-Z`, `a-z`, `0-9`, `_`, and `-`; at least one
    /// and at most [`MAX_VETO_TOOLS`] distinct tools are listed.
    pub fn new<I, S>(id: &str, version: &str, veto_tools: I) -> Result<Self, PluginError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let id = PluginId::new(id)?;
        if !(1..=64).contains(&version.len()) || !version.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(PluginError::InvalidVersion(bounded(version)));
        }
        let mut tools = BTreeSet::new();
        for tool in veto_tools {
            let tool = tool.into();
            let valid = (1..=64).contains(&tool.len())
                && tool
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
            if !valid {
                return Err(PluginError::InvalidToolName(bounded(&tool)));
            }
            tools.insert(tool);
            if tools.len() > MAX_VETO_TOOLS {
                return Err(PluginError::TooManyTools);
            }
        }
        if tools.is_empty() {
            return Err(PluginError::NoContributions);
        }
        Ok(Self {
            id,
            version: version.to_owned(),
            veto_tools: tools,
        })
    }

    /// The plugin id.
    pub fn id(&self) -> &PluginId {
        &self.id
    }

    /// The plugin version.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// The tools whose calls the plugin may veto, sorted.
    pub fn veto_tools(&self) -> impl Iterator<Item = &str> {
        self.veto_tools.iter().map(String::as_str)
    }

    /// Whether the plugin is asked about calls to `tool`.
    pub fn vetoes(&self, tool: &str) -> bool {
        self.veto_tools.contains(tool)
    }
}

/// Why a manifest or a registration was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginError {
    /// The id is not 1 to 32 characters of `a-z`, `0-9`, and `-`.
    InvalidId(String),
    /// The version is empty, too long, or not printable ASCII.
    InvalidVersion(String),
    /// A listed tool name is empty, too long, or has other characters.
    InvalidToolName(String),
    /// More than [`MAX_VETO_TOOLS`] tools are listed.
    TooManyTools,
    /// No tool is listed, so the plugin would contribute nothing.
    NoContributions,
    /// A plugin with this id is already registered.
    Duplicate(PluginId),
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId(id) => write!(
                f,
                "invalid plugin id {id:?}: use 1 to 32 characters of a-z, 0-9, and -"
            ),
            Self::InvalidVersion(version) => write!(f, "invalid plugin version {version:?}"),
            Self::InvalidToolName(tool) => write!(f, "invalid tool name {tool:?}"),
            Self::TooManyTools => write!(f, "a plugin may veto at most {MAX_VETO_TOOLS} tools"),
            Self::NoContributions => write!(f, "the plugin lists no tool to veto"),
            Self::Duplicate(id) => write!(f, "plugin {id} is already registered"),
        }
    }
}

impl std::error::Error for PluginError {}

/// A tool call as a plugin sees it, Pi's `ToolCallEvent` without the
/// parent call. The arguments are the validated ones the tool will run
/// with, and they are read-only:
///
/// ```compile_fail,E0596
/// # use bake_coding_agent::plugins::ToolCallView;
/// fn rewrite(call: ToolCallView<'_>) {
///     call.args["path"] = serde_json::json!("elsewhere");
/// }
/// ```
#[derive(Debug, Clone, Copy)]
pub struct ToolCallView<'a> {
    /// The call id the model gave.
    pub tool_call_id: &'a str,
    /// The tool name.
    pub tool_name: &'a str,
    /// The validated arguments.
    pub args: &'a Value,
}

/// A plugin's answer about one tool call, Pi's `ToolCallEventResult`
/// without argument mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallVeto {
    /// No objection: the next plugin is asked, then the tool runs with its
    /// own permission checks.
    Continue,
    /// The call does not run; the model sees `reason` as its error result.
    Block {
        /// The denial text, at most [`MAX_REASON_BYTES`]; empty gives Pi's
        /// "Tool execution was blocked".
        reason: String,
        /// Asks the agent to stop after the batch, as Pi's `terminate`.
        terminate: bool,
    },
}

/// A trusted plugin compiled into `bake-rs`.
///
/// Obligations the dispatcher relies on, since it cannot enforce them on
/// in-process code:
///
/// - The future [`Self::on_tool_call`] returns owns all of its work.
///   Dropping it, which the dispatcher does on a timeout, a cancellation,
///   or session shutdown, must stop that work; it spawns nothing that
///   outlives it and keeps no callback that runs later.
/// - It has no effect on the host beyond its answer: it does not write the
///   session, run tools, or change what the tool will receive.
/// - It does not block its thread, and it starts no blocking, spawned, or
///   threaded work: dropping a Tokio `JoinHandle` (from `spawn` or
///   `spawn_blocking`) does not stop that work, so it would outlive the
///   call and the session's shutdown. A plugin that needs such work is out
///   of this slice's scope.
pub trait Plugin: Send + Sync + 'static {
    /// The plugin's declaration, read once when it is registered.
    fn manifest(&self) -> PluginManifest;

    /// Pi's `tool_call` handler: called before each call to a tool the
    /// manifest lists, unless an earlier plugin blocked it. `Err` fails the
    /// plugin, which then blocks its listed tools.
    fn on_tool_call<'a>(
        &'a self,
        call: ToolCallView<'a>,
    ) -> BoxFuture<'a, Result<ToolCallVeto, String>>;
}

/// The plugins a session dispatches to, by configured order and id; an
/// empty registry, the default, changes nothing.
#[derive(Clone)]
pub struct PluginRegistry {
    plugins: Vec<Registered>,
    call_timeout: Duration,
}

#[derive(Clone)]
pub(crate) struct Registered {
    pub order: i32,
    pub manifest: PluginManifest,
    pub plugin: Arc<dyn Plugin>,
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self {
            plugins: Vec::new(),
            call_timeout: DEFAULT_CALL_TIMEOUT,
        }
    }
}

impl fmt::Debug for PluginRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginRegistry")
            .field(
                "plugins",
                &self
                    .plugins
                    .iter()
                    .map(|registered| (registered.order, &registered.manifest))
                    .collect::<Vec<_>>(),
            )
            .field("call_timeout", &self.call_timeout)
            .finish()
    }
}

impl PluginRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `plugin` at `order`: lower orders run first, and equal orders
    /// run by id. Reads its manifest once; a second plugin with the same id
    /// is refused.
    pub fn register(&mut self, plugin: Arc<dyn Plugin>, order: i32) -> Result<(), PluginError> {
        let manifest = plugin.manifest();
        if self
            .plugins
            .iter()
            .any(|registered| registered.manifest.id == manifest.id)
        {
            return Err(PluginError::Duplicate(manifest.id));
        }
        self.plugins.push(Registered {
            order,
            manifest,
            plugin,
        });
        Ok(())
    }

    /// Sets how long one plugin may take to answer one call.
    pub fn with_call_timeout(mut self, timeout: Duration) -> Self {
        self.call_timeout = timeout;
        self
    }

    /// Whether no plugin is registered.
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// The registered manifests in dispatch order.
    pub fn manifests(&self) -> Vec<&PluginManifest> {
        let mut sorted: Vec<&Registered> = self.plugins.iter().collect();
        sorted.sort_by(|a, b| (a.order, &a.manifest.id).cmp(&(b.order, &b.manifest.id)));
        sorted.into_iter().map(|r| &r.manifest).collect()
    }
}

/// `text` cut to at most 64 bytes on a character boundary, for errors.
fn bounded(text: &str) -> String {
    truncate(text, 64).to_owned()
}

/// `text` cut to at most `max` bytes on a character boundary.
pub(crate) fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
pub(crate) mod tests;
