//! A headless agent session: an [`Agent`] whose transcript is a Pi session.
//!
//! Ported from Pi `packages/coding-agent/src/core/agent-session.ts` and the
//! session setup of `src/core/sdk.ts` (`createAgentSession`), v1.1.0. The
//! session restores the transcript, model, and thinking level from the
//! session file, records the initial model and thinking level of a new
//! session, and persists every finished system, user, assistant, tool
//! result, and custom message, as Pi's `_handleAgentEvent` does. Before
//! each prompt it sends the system-prompt sections that differ from the
//! ones the transcript replays, as Pi's `_preparePromptAndToolLoadout`
//! does. A prompt of the form `/skill:name args` is expanded to the skill's
//! body first, as Pi's `_expandSkillCommand` does.
//!
//! Session I/O is blocking and runs on Tokio's blocking pool; message
//! persistence happens inside the agent's event listener, so a run is idle
//! only after its messages are written. A failed write aborts the run and
//! no later message is written, so the file never skips a message; Pi's
//! throwing listener likewise ends the run.
//!
//! The session's [`WorkGate`] owns the work that runs outside the task
//! awaiting it: session writes and other blocking work, model requests,
//! model, thinking-level, and settings changes, idle compaction, and
//! background work ([`AgentSession::spawn_background`]) and background
//! jobs ([`AgentSession::jobs`]). Two lifetimes meet here. A turn's work
//! ends with [`AgentSession::abort`], which signals the run, a retry wait,
//! and a compaction, and leaves the gate's background tasks, jobs, and
//! changes running. The session's lifetime ends with
//! [`AgentSession::shutdown`], which refuses new work at once, cancels
//! both, and returns only after every owned task, its cleanup, every job
//! and the processes it started, and every listener call have finished.
//!
//! Compaction and auto-retry follow Pi (in `agent_session/recovery.rs`):
//! after each run a retryable error is retried with backoff, and a context
//! overflow, a recoverable length stop, or a context over the threshold
//! compacts the session through [`crate::compaction`]; between the turns of
//! a run the threshold is checked again; [`AgentSession::compact`] compacts
//! on request. A failed attempt that is retried is omitted from model context
//! by a context edit, so the file keeps it and a resumed session sends
//! exactly what the live one would.
//!
//! The steering and follow-up queues ([`AgentSession::steer`],
//! [`AgentSession::follow_up`], [`AgentSession::clear_queue`], and
//! [`SessionEvent::QueueUpdate`]) are ported from Pi v1.2.0's
//! `steer`, `followUp`, `_queueSteer`, `_queueFollowUp`, `clearQueue`, and
//! `message_start` queue bookkeeping, without extension-command checks or
//! input handlers.
//!
//! Tree navigation ([`AgentSession::navigate_tree`]) and the store of a
//! forked session ([`AgentSession::fork_session_manager`]) follow Pi
//! v1.2.0's `navigateTree` and runtime `fork` (in
//! `agent_session/navigation.rs`).
//!
//! Plugins ([`crate::plugins`], given as [`AgentSessionOptions::plugins`]):
//! trusted compiled-in ones and the Wasm plugins a Bake home enables
//! ([`crate::plugins::installed`]) veto tool calls through the agent's
//! `before_tool_call` hook, as Pi v1.3.0's `_beforeToolCall` runs
//! `tool_call` handlers (in `agent_session/plugin_veto.rs`); each block is
//! written as a `custom` entry before the denial result. The session's
//! [`WorkGate`] owns every Wasm helper: shutdown returns only after each
//! has been reaped. With no plugins no hook is installed.
//!
//! Not ported (deferred): Pi's extensions and their other events (including
//! `session_before_compact`, `session_compact`, and
//! `session_compact_failed`), branch summaries during navigation, the
//! `/skill:name` slash command list and its expansion error event, prompt
//! templates, MCP, bash `!` commands, the cache warmer, scoped
//! models, virtual models and their failed-response routing, the
//! `entry_appended` event, and tool-loadout changes between turns.

mod navigation;
mod plugin_veto;
mod recovery;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

pub use navigation::{
    ForkPoint, ForkedSession, NAVIGATION_CANCELLED, NAVIGATION_WHILE_BUSY,
    NAVIGATION_WHILE_STREAMING, TreeNavigation,
};
use recovery::{Activity, Core, RunState};
pub use recovery::{
    COMPACTION_CANCELLED, COMPACTION_WHILE_BUSY, COMPACTION_WHILE_STREAMING, CompactionReason,
};

use bake_agent::{
    AfterToolCallResult, Agent, AgentError, AgentEvent, AgentInitialState,
    AgentMessage as LiveMessage, AgentOptions, AgentTool, PromptInput, QueueMode, Subscription,
    ToolExecutionMode, hook,
};
use bake_ai::transcript::get_current_system_message;
use bake_ai::{
    AbortSignal, ApiRegistry, ImageContent, Message, Model, ModelThinkingLevel, SystemContent,
    SystemMessage, TextContent, UserContent, UserContentBlock, UserMessage,
};

use crate::compaction::CompactionResult;
use crate::jobs::{JobLimits, JobRegistry};
use crate::mcp::{McpOptions, McpServers};
use crate::model_registry::resolver::{DEFAULT_THINKING_LEVEL, find_initial_model};
use crate::model_registry::{ModelRegistry, ModelRuntime};
use crate::owned_work::{OwnedTask, Refused, TaskScope, WorkGate, WorkPermit};
use crate::plugins::PluginRegistry;
use crate::plugins::veto::VetoDispatcher;
use crate::prompt_templates::{PromptTemplate, expand_prompt_template};
use crate::session::messages::{CustomMessage, convert_to_llm};
use crate::session::{GroupEntry, SessionError, SessionManager};
use crate::settings::{Settings, SettingsStore};
use crate::skills::expand_skill_command;
use crate::system_prompt::{
    SystemPromptOptions, build_system_prompt_sections, diff_system_prompt_sections,
    normalize_prompt_guidelines,
};
use crate::tools::image::{
    ImageResizeOptions, ProcessedImage, normalize_tool_result_images, process_image,
};

/// Bake's form of Pi's `getProviderLoginHelp`. `bake-rs` has no `/login`,
/// so the help names the files that configure a model and the `docs` topic
/// that describes them. `docs_command` runs `bake-rs docs`
/// ([`AgentSessionOptions::docs_command`]).
fn provider_login_help(docs_command: &str) -> String {
    format!(
        "Add a provider to models.json in the Bake home, and its API key to models.json or auth.json. See:\n  {docs_command} models"
    )
}

/// Pi's `formatNoModelsAvailableMessage`, with Bake's help.
pub fn format_no_models_available_message(docs_command: &str) -> String {
    format!("No models available. {}", provider_login_help(docs_command))
}

/// Pi's `formatNoModelSelectedMessage`, with Bake's help and `--model` in
/// place of Pi's `/model`.
pub fn format_no_model_selected_message(docs_command: &str) -> String {
    format!(
        "No model selected.\n\n{}\n\nThen select one with --model.",
        provider_login_help(docs_command)
    )
}

/// Pi's `formatNoApiKeyFoundMessage`, with Bake's help.
pub fn format_no_api_key_found_message(provider: &str, docs_command: &str) -> String {
    let display = if provider == "unknown" {
        "the selected model"
    } else {
        provider
    };
    format!(
        "No API key found for {display}.\n\n{}",
        provider_login_help(docs_command)
    )
}

/// What [`AgentSession::create`] needs, Pi's `CreateAgentSessionOptions`
/// for a headless session.
pub struct AgentSessionOptions {
    /// The session store; its transcript, model, and thinking level are
    /// restored.
    pub session: SessionManager,
    /// The models and their auth, which a login or logout may replace
    /// while the session runs; each request uses the snapshot current when
    /// it starts.
    pub registry: ModelRuntime,
    /// The provider APIs requests stream through.
    pub apis: Arc<ApiRegistry>,
    /// Settings from the Bake home. The session keeps a live copy, which
    /// its own setters change.
    pub settings: Settings,
    /// Where [`AgentSession::set_model`] and the other setters save a
    /// default or a setting when asked; `None` saves nothing and reports
    /// that.
    pub settings_store: Option<SettingsStore>,
    /// The cell the session publishes its model and thinking level in, for
    /// tools that read them when they run (bash's `BAKE_MODEL`, read's
    /// image limits); `None` makes a private one.
    pub selection: Option<SessionSelection>,
    /// An explicit model, such as `--model`; otherwise restored or the
    /// default.
    pub model: Option<Model>,
    /// An explicit thinking level; otherwise restored or the default.
    pub thinking_level: Option<ModelThinkingLevel>,
    /// The executable tools.
    pub tools: Vec<Arc<AgentTool>>,
    /// The active tools by name, Pi's `initialActiveToolNames`: the
    /// prompt's rules follow these, and requests declare the ones present
    /// in `tools`.
    pub active_tool_names: Vec<String>,
    /// What each tool contributes to the prompt, from Pi's tool definitions;
    /// only the guidelines reach the prompt.
    pub tool_prompts: Vec<ToolPrompt>,
    /// The system-prompt inputs; `selected_tools` is replaced by the active
    /// tools, `docs_command` by [`Self::docs_command`], and the guidelines of
    /// [`Self::tool_prompts`] are added to `tool_guidelines`, winning over
    /// entries for the same tool.
    pub system_prompt: SystemPromptOptions,
    /// How to run `bake-rs docs`, such as `/usr/local/bin/bake-rs docs`,
    /// which the prompt's `docs` section and the no-model and no-key
    /// messages name.
    pub docs_command: String,
    /// The plugins whose tool-call vetoes run before each call to a tool
    /// they list ([`crate::plugins`]): trusted ones, and the Wasm plugins
    /// [`crate::plugins::installed::PluginHome::load`] registers; empty, the
    /// default, installs no hook.
    pub plugins: PluginRegistry,
    /// The gate that owns the session's work ([`WorkGate`]); `None` makes a
    /// private one. Give the same gate to the tools that spawn processes
    /// ([`crate::tools::bash::BashToolOptions::owner`],
    /// [`crate::tools::index::ToolsOptions::owner`]) so that
    /// [`AgentSession::shutdown`] refuses their later spawns and waits for
    /// every process tree they started, a dropped call's included. One gate
    /// per session: closing it is the session's shutdown.
    pub work: Option<WorkGate>,
}

/// Tools whose calls change files or run commands. The prompt's round-trip
/// rule sends an edit with the command that checks it in one response, so a
/// batch holding one of these runs one call at a time, in the order the
/// model wrote them; a batch of reads still runs in parallel. Pi runs every
/// batch in parallel.
const ORDERED_TOOLS: [&str; 3] = ["bash", "edit", "write"];

/// `tool`, made sequential when it is one of the [`ORDERED_TOOLS`] and sets
/// no mode of its own.
fn run_in_order(tool: Arc<AgentTool>) -> Arc<AgentTool> {
    if tool.execution_mode.is_some() || !ORDERED_TOOLS.contains(&tool.name.as_str()) {
        return tool;
    }
    let mut ordered = (*tool).clone();
    ordered.execution_mode = Some(ToolExecutionMode::Sequential);
    Arc::new(ordered)
}

/// The prompt text of one tool, from Pi's `ToolDefinition`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolPrompt {
    /// The tool name.
    pub name: String,
    /// Pi's `promptSnippet`, the tool's line in Pi's list of tools, which
    /// Bake's prompt does not have.
    pub snippet: Option<String>,
    /// Pi's `promptGuidelines`: bullets added to the guidelines, trimmed
    /// and deduplicated.
    pub guidelines: Vec<String>,
}

/// The model and thinking level a session runs with.
#[derive(Debug, Clone, PartialEq)]
pub struct Selected {
    /// The model, unless none is selected.
    pub model: Option<Model>,
    /// The thinking level.
    pub thinking_level: ModelThinkingLevel,
}

impl Default for Selected {
    fn default() -> Self {
        Self {
            model: None,
            thinking_level: ModelThinkingLevel::Off,
        }
    }
}

/// Where a session publishes its [`Selected`] model and thinking level.
/// The session writes it under the same lock as it changes its agent, so a
/// reader, such as a tool when it runs or the hook before the next model
/// request, sees the model and level of one change, never a mix.
#[derive(Debug, Clone, Default)]
pub struct SessionSelection(Arc<Mutex<Selected>>);

impl SessionSelection {
    /// The model and thinking level now.
    pub fn get(&self) -> Selected {
        self.lock().clone()
    }

    fn lock(&self) -> MutexGuard<'_, Selected> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Whether a setter saved its default or setting to `settings.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultSave {
    /// Nothing was asked to be saved.
    NotRequested,
    /// Saved.
    Saved,
    /// Written to `settings.json`, but its directory could not be flushed,
    /// so a power loss may still undo it; the reason says why.
    Unsynced(String),
    /// The session changed, but saving failed, for this reason.
    Failed(String),
}

impl DefaultSave {
    /// Whether the file now holds the setting.
    pub fn is_written(&self) -> bool {
        matches!(self, Self::Saved | Self::Unsynced(_))
    }
}

/// Pi's `SessionStats`, without its context usage.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionStats {
    /// The session file, when the session persists.
    pub session_file: Option<PathBuf>,
    pub session_id: String,
    /// The display name.
    pub name: Option<String>,
    pub user_messages: usize,
    pub assistant_messages: usize,
    pub tool_calls: usize,
    pub tool_results: usize,
    pub total_messages: usize,
    /// Uncached input tokens.
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// The cost in dollars.
    pub cost: f64,
}

impl SessionStats {
    /// The four token counts' sum, Pi's `tokens.total`.
    pub fn total_tokens(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }

    /// Pi's `addUsageToTotals` over a stored usage object.
    fn add_usage(&mut self, usage: &serde_json::Value) {
        let count = |key: &str| {
            usage
                .get(key)
                .and_then(serde_json::Value::as_f64)
                .filter(|value| value.is_finite() && *value > 0.0)
                .map_or(0, |value| value as u64)
        };
        self.input = self.input.saturating_add(count("input"));
        self.output = self.output.saturating_add(count("output"));
        self.cache_read = self.cache_read.saturating_add(count("cacheRead"));
        self.cache_write = self.cache_write.saturating_add(count("cacheWrite"));
        if let Some(cost) = usage
            .get("cost")
            .and_then(|cost| cost.get("total"))
            .and_then(serde_json::Value::as_f64)
            .filter(|cost| cost.is_finite())
        {
            self.cost += cost;
        }
    }
}

/// An event of the session, Pi's `AgentSessionEvent` without the deferred
/// features' events.
#[derive(Debug)]
pub enum SessionEvent<'a> {
    /// An agent event other than `agent_end`.
    Agent(&'a AgentEvent),
    /// Pi's `agent_end`, with whether auto-retry will continue the run.
    AgentEnd {
        /// The run's new messages.
        messages: &'a [LiveMessage],
        /// Whether a retry follows.
        will_retry: bool,
    },
    /// A prompt finished, Pi's `agent_settled`.
    AgentSettled {
        /// Whether it was aborted.
        aborted: bool,
    },
    /// The thinking level changed, Pi's `thinking_level_changed`.
    ThinkingLevelChanged(ModelThinkingLevel),
    /// The session's name changed, Pi's `session_info_changed`, with the
    /// name it now has.
    SessionInfoChanged(Option<String>),
    /// Pi's `compaction_start`.
    CompactionStart {
        /// What started it.
        reason: CompactionReason,
    },
    /// Pi's `compaction_end`.
    CompactionEnd {
        /// What started it.
        reason: CompactionReason,
        /// The recorded compaction, when it succeeded.
        result: Option<&'a CompactionResult>,
        /// Whether it was aborted.
        aborted: bool,
        /// Whether the interrupted turn continues.
        will_retry: bool,
        /// Why it failed, unless aborted.
        error_message: Option<String>,
    },
    /// Pi's `auto_retry_start`.
    AutoRetryStart {
        /// The retry number, from 1.
        attempt: u32,
        /// The retry budget.
        max_attempts: u32,
        /// The backoff before the retry.
        delay_ms: u64,
        /// The error retried.
        error_message: String,
    },
    /// Pi's `auto_retry_end`.
    AutoRetryEnd {
        /// Whether a retry succeeded.
        success: bool,
        /// The last retry number.
        attempt: u32,
        /// The final error, when retries ended without success.
        final_error: Option<String>,
    },
    /// Pi's `summarization_retry_scheduled`.
    SummarizationRetryScheduled {
        /// The retry number, from 1.
        attempt: u32,
        /// The retry budget.
        max_attempts: u32,
        /// The backoff.
        delay_ms: u64,
        /// The error retried.
        error_message: String,
    },
    /// Pi's `summarization_retry_attempt_start` of a compaction.
    SummarizationRetryAttemptStart {
        /// The compaction's reason.
        reason: CompactionReason,
    },
    /// Pi's `summarization_retry_finished`.
    SummarizationRetryFinished,
    /// Pi's `queue_update`: the steering and follow-up prompts still
    /// waiting, in the order they will be delivered.
    QueueUpdate {
        /// Prompts for the running turn's next step ([`AgentSession::steer`]).
        steering: &'a [String],
        /// Prompts for when the agent would stop ([`AgentSession::follow_up`]).
        follow_up: &'a [String],
    },
}

/// The session's queue of undelivered prompts and the order its updates
/// are announced in.
///
/// No lock is held while listeners run, so a listener may read or change
/// the queue. One caller at a time drains announcements: a change made
/// while another caller is announcing, from a listener or another thread,
/// only marks the queue changed, and the announcing caller announces the
/// latest queue again once its listeners return. Every listener therefore
/// sees the queue's states in order, ending with the latest, and never an
/// older queue after a newer one.
#[derive(Default)]
struct Queue {
    messages: Mutex<QueuedMessages>,
    drain: Mutex<Drain>,
}

/// Whether a caller is announcing, and whether the queue changed since its
/// last snapshot.
#[derive(Default)]
struct Drain {
    announcing: bool,
    changed: bool,
}

impl Queue {
    fn lock(&self) -> MutexGuard<'_, QueuedMessages> {
        self.messages.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn drain(&self) -> MutexGuard<'_, Drain> {
        self.drain.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Pi's `_emitQueueUpdate`: tells the listeners what waits now, or
    /// leaves that to the caller already announcing.
    fn announce(&self, core: &Core) {
        {
            let mut drain = self.drain();
            if drain.announcing {
                drain.changed = true;
                return;
            }
            drain.announcing = true;
        }
        loop {
            self.drain().changed = false;
            let snapshot = self.lock().clone();
            core.emit(&SessionEvent::QueueUpdate {
                steering: &snapshot.steering,
                follow_up: &snapshot.follow_up,
            });
            let mut drain = self.drain();
            if !drain.changed {
                drain.announcing = false;
                return;
            }
        }
    }
}

/// The prompts [`AgentSession::steer`] and [`AgentSession::follow_up`]
/// queued that the agent has not delivered yet, Pi's `_steeringMessages`
/// and `_followUpMessages`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueuedMessages {
    /// Steering prompts, oldest first.
    pub steering: Vec<String>,
    /// Follow-up prompts, oldest first.
    pub follow_up: Vec<String>,
}

impl QueuedMessages {
    /// Whether nothing waits.
    pub fn is_empty(&self) -> bool {
        self.steering.is_empty() && self.follow_up.is_empty()
    }

    /// Pi's `message_start` bookkeeping: a delivered user prompt leaves the
    /// steering queue first, else the follow-up queue. Returns whether the
    /// queue changed.
    fn delivered(&mut self, text: &str) -> bool {
        if let Some(index) = self.steering.iter().position(|queued| queued == text) {
            self.steering.remove(index);
            return true;
        }
        if let Some(index) = self.follow_up.iter().position(|queued| queued == text) {
            self.follow_up.remove(index);
            return true;
        }
        false
    }
}

type Listener = Arc<dyn Fn(&SessionEvent<'_>) + Send + Sync>;

#[derive(Default)]
struct Listeners {
    next: AtomicU64,
    list: Mutex<Vec<(u64, Listener)>>,
    /// Set by [`AgentSession::shutdown`]: later events reach no listener.
    closed: AtomicBool,
    /// Emits in progress, so shutdown can wait until no listener runs.
    emitting: std::sync::atomic::AtomicUsize,
    /// Notified when the last emit in progress ends.
    quiet: tokio::sync::Notify,
}

/// Counts one emit in progress until dropped.
struct Emitting<'a>(&'a Listeners);

impl Drop for Emitting<'_> {
    fn drop(&mut self) {
        if self.0.emitting.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.quiet.notify_waiters();
        }
    }
}

impl Listeners {
    /// Pi's `dispose` clearing `_eventListeners`: the work shutdown
    /// cancels ends without telling anyone.
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.list
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// Waits until no listener call that started before [`Self::close`]
    /// is still running. An emit counts itself before it checks `closed`,
    /// and `close` sets `closed` before this reads the count, so every
    /// emit either sees `closed` or is counted here.
    async fn quiesce(&self) {
        loop {
            let notified = self.quiet.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.emitting.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }

    fn emit(&self, event: &SessionEvent<'_>) {
        self.emitting.fetch_add(1, Ordering::SeqCst);
        let _emitting = Emitting(self);
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let listeners: Vec<Listener> = self
            .list
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect();
        for listener in listeners {
            listener(event);
        }
    }
}

/// A session listener registration; [`Self::unsubscribe`] removes it.
pub struct SessionSubscription {
    listeners: std::sync::Weak<Listeners>,
    id: u64,
}

impl SessionSubscription {
    /// Removes the listener.
    pub fn unsubscribe(self) {
        if let Some(listeners) = self.listeners.upgrade() {
            listeners
                .list
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(|(id, _)| *id != self.id);
        }
    }
}

type SharedSession = Arc<Mutex<SessionManager>>;

/// Settings the session and its core read at each use.
pub(crate) type SharedSettings = Arc<std::sync::RwLock<Settings>>;

fn read_settings(settings: &SharedSettings) -> std::sync::RwLockReadGuard<'_, Settings> {
    settings.read().unwrap_or_else(PoisonError::into_inner)
}

fn write_settings(settings: &SharedSettings) -> std::sync::RwLockWriteGuard<'_, Settings> {
    settings.write().unwrap_or_else(PoisonError::into_inner)
}

fn lock(session: &SharedSession) -> MutexGuard<'_, SessionManager> {
    session.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The message for work the session's gate refused.
fn refused_message(refused: Refused) -> String {
    match refused {
        Refused::Closed => "The session is shut down.".to_owned(),
        refused => format!("The session could not start its work: {refused}."),
    }
}

/// Runs `work` on the session off the Tokio workers, owned by `gate`.
async fn with_session<R: Send + 'static>(
    gate: &WorkGate,
    session: &SharedSession,
    work: impl FnOnce(&mut SessionManager) -> R + Send + 'static,
) -> Result<R, String> {
    let session = Arc::clone(session);
    gate.spawn_blocking(move || work(&mut lock(&session)))
        .map_err(refused_message)?
        .await
        .map_err(|error| format!("Session I/O task failed: {error}"))
}

/// Pi's `AgentSession`, headless.
pub struct AgentSession {
    /// The provider APIs requests stream through.
    apis: Arc<ApiRegistry>,
    agent: Arc<Agent>,
    core: Arc<Core>,
    work: WorkGate,
    /// The session's background jobs, owned by `work`.
    jobs: JobRegistry,
    session: SharedSession,
    registry: ModelRuntime,
    /// The live settings, shared with the core: the setters change them,
    /// and every use reads them then.
    settings: SharedSettings,
    settings_store: Option<SettingsStore>,
    selection: SessionSelection,
    /// Serializes the model, thinking-level, and settings changes, so the
    /// order of their log entries is the order they apply in.
    control: Arc<tokio::sync::Mutex<()>>,
    tools: Vec<Arc<AgentTool>>,
    active_tool_names: Vec<String>,
    prompt_options: SystemPromptOptions,
    prompt_templates: Vec<PromptTemplate>,
    docs_command: String,
    listeners: Arc<Listeners>,
    persist_error: Arc<Mutex<Option<String>>>,
    subscription: Arc<Mutex<Option<Subscription>>>,
    /// Set by the first [`Self::shutdown`]: the teardown's completion.
    shutdown: OnceLock<tokio::sync::watch::Receiver<bool>>,
    /// The runtime the session was created on, which runs its teardown
    /// whichever thread or executor calls [`Self::shutdown`].
    runtime: tokio::runtime::Handle,
    model_fallback_message: Option<String>,
    queue: Arc<Queue>,
    /// The first session write that failed. The file then misses a
    /// message, so no later prompt is accepted.
    write_failure: Arc<Mutex<Option<String>>>,
    /// The MCP servers [`Self::start_mcp`] started, once it has.
    mcp: OnceLock<McpServers>,
    /// The plugin dispatcher, when any plugin is registered.
    plugins: Option<Arc<VetoDispatcher>>,
}

impl std::fmt::Debug for AgentSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentSession")
            .field("agent", &self.agent)
            .finish_non_exhaustive()
    }
}

/// The model and thinking level a session starts with, Pi's
/// `createAgentSession` selection.
struct Selection {
    model: Option<Model>,
    thinking_level: ModelThinkingLevel,
    fallback_message: Option<String>,
}

struct SelectionInputs<'a> {
    registry: &'a ModelRegistry,
    settings: &'a Settings,
    docs_command: &'a str,
    model: Option<Model>,
    thinking_level: Option<ModelThinkingLevel>,
}

fn select_model(
    inputs: SelectionInputs<'_>,
    has_existing_session: bool,
    session_model: Option<(String, String)>,
    session_thinking: Option<String>,
) -> Selection {
    let SelectionInputs {
        registry,
        settings,
        docs_command,
        mut model,
        thinking_level: explicit_thinking,
    } = inputs;
    let mut fallback_message = None;
    if model.is_none()
        && has_existing_session
        && let Some((provider, id)) = &session_model
    {
        model = registry
            .model(provider, id)
            .filter(|restored| registry.has_configured_auth(&restored.provider));
        if model.is_none() {
            fallback_message = Some(format!("Could not restore model {provider}/{id}"));
        }
    }
    if model.is_none() {
        let initial = find_initial_model(
            settings.default_provider.as_deref(),
            settings.default_model.as_deref(),
            settings.default_thinking_level,
            &settings.model_thinking_levels,
            registry,
        );
        model = initial.model;
        fallback_message = match (&model, fallback_message) {
            (None, _) => Some(format_no_models_available_message(docs_command)),
            (Some(model), Some(message)) => {
                Some(format!("{message}. Using {}/{}", model.provider, model.id))
            }
            (Some(_), None) => None,
        };
    }
    let mut thinking_level = explicit_thinking;
    if thinking_level.is_none() && has_existing_session {
        thinking_level = Some(
            session_thinking
                .as_deref()
                .and_then(crate::model_registry::resolver::parse_thinking_level)
                .or(settings.default_thinking_level)
                .unwrap_or(DEFAULT_THINKING_LEVEL),
        );
    }
    if thinking_level.is_none()
        && let Some(model) = &model
    {
        thinking_level = settings.model_thinking_level(&model.provider, &model.id);
    }
    let thinking_level = thinking_level
        .or(settings.default_thinking_level)
        .unwrap_or(DEFAULT_THINKING_LEVEL);
    let thinking_level = match &model {
        None => ModelThinkingLevel::Off,
        Some(model) => bake_ai::models::clamp_thinking_level(model, thinking_level),
    };
    Selection {
        model,
        thinking_level,
        fallback_message,
    }
}

/// Pi's `_normalizePromptImages`: decodes each prompt image and runs
/// [`process_image`] with `auto_resize_images` and the active model's
/// `inputLimits.images.resize`.
fn normalize_prompt_images(
    images: &[ImageContent],
    auto_resize_images: bool,
    resize_options: ImageResizeOptions,
) -> (Vec<ImageContent>, Vec<String>) {
    use base64::Engine as _;
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    let mut normalized = Vec::with_capacity(images.len());
    let mut hints = Vec::new();
    for image in images {
        let cleaned: String = image
            .data
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '-' | '_'))
            .collect();
        let bytes = engine
            .decode(cleaned.replace('-', "+").replace('_', "/"))
            .unwrap_or_default();
        match process_image(&bytes, &image.mime_type, auto_resize_images, resize_options) {
            ProcessedImage::Failed(message) => hints.push(message),
            ProcessedImage::Ok {
                data,
                mime_type,
                hints: image_hints,
            } => {
                normalized.push(ImageContent { data, mime_type });
                hints.extend(image_hints);
            }
        }
    }
    (normalized, hints)
}

/// Applies `settings.json`'s `maxToolArgumentWhitespace` to every request
/// whose options leave it unset.
fn with_tool_argument_whitespace(
    inner: bake_agent::StreamFn,
    max: Option<usize>,
) -> bake_agent::StreamFn {
    let Some(max) = max else {
        return inner;
    };
    Arc::new(move |model, context, mut options| {
        options.base.max_tool_argument_whitespace.get_or_insert(max);
        inner(model, context, options)
    })
}

/// Pi's `convertToLlmWithBlockImages` replacement text.
const IMAGE_READING_DISABLED: &str = "Image reading is disabled.";

/// Pi's `blockImages` filter: images in user and tool-result messages
/// become one placeholder text per run of images.
fn block_images(messages: Vec<Message>) -> Vec<Message> {
    fn filter(blocks: &[UserContentBlock]) -> Option<Vec<UserContentBlock>> {
        if !blocks
            .iter()
            .any(|block| matches!(block, UserContentBlock::Image(_)))
        {
            return None;
        }
        let mut filtered: Vec<UserContentBlock> = Vec::with_capacity(blocks.len());
        for block in blocks {
            let block = match block {
                UserContentBlock::Image(_) => {
                    UserContentBlock::Text(TextContent::new(IMAGE_READING_DISABLED))
                }
                other => other.clone(),
            };
            let repeated = matches!(
                (&block, filtered.last()),
                (UserContentBlock::Text(text), Some(UserContentBlock::Text(previous)))
                    if text.text == IMAGE_READING_DISABLED && previous.text == IMAGE_READING_DISABLED
            );
            if !repeated {
                filtered.push(block);
            }
        }
        Some(filtered)
    }
    messages
        .into_iter()
        .map(|message| match message {
            Message::User(mut user) => {
                if let UserContent::Blocks(blocks) = &user.content
                    && let Some(filtered) = filter(blocks)
                {
                    user.content = UserContent::Blocks(filtered);
                }
                Message::User(user)
            }
            Message::ToolResult(mut result) => {
                if let Some(filtered) = filter(&result.content) {
                    result.content = filtered;
                }
                Message::ToolResult(result)
            }
            other => other,
        })
        .collect()
}

/// Persists a finished message as Pi's `_handleAgentEvent` does: custom
/// messages as `custom_message` entries, LLM messages as `message`
/// entries, and the other kinds not at all (they are written elsewhere).
/// Returns the entry id.
fn persist(session: &mut SessionManager, message: &LiveMessage) -> Result<Option<String>, String> {
    let result = match message {
        LiveMessage::Llm(llm) => session.append_message(llm.clone()).map(Some),
        LiveMessage::Custom(custom) => match custom.as_any().downcast_ref::<CustomMessage>() {
            Some(custom) => session
                .append_custom_message_entry(
                    &custom.custom_type,
                    custom.content.clone(),
                    custom.display,
                    custom.details.clone(),
                )
                .map(Some),
            None => Ok(None),
        },
    };
    result.map_err(|error| error.to_string())
}

/// Pi's `prepareNextTurnWithContext` hook (`_installAgentNextTurnRefresh`):
/// threshold compaction between turns, then the model and thinking level
/// selected now, so a change made while a run streams takes effect at its
/// next model request. The request already streaming keeps the model and
/// level it started with.
fn next_turn_hook(
    core: Arc<Core>,
    selection: SessionSelection,
) -> bake_agent::agent::AgentPrepareNextTurnWithContext {
    Arc::new(move |turn, signal| {
        let tools = turn.context.tools.clone();
        let core = Arc::clone(&core);
        let selection = selection.clone();
        Box::pin(async move {
            let mut update = core
                .compact_before_next_response(tools, signal)
                .await
                .unwrap_or_default();
            // One read, so the model and level come from one change.
            let selected = selection.get();
            if let Some(model) = selected.model {
                update.model = Some(model);
                update.thinking_level = Some(selected.thinking_level);
            }
            Some(update)
        })
    })
}

/// Persists a finished message unless an earlier write failed or the
/// session is shut down (Pi's `dispose` disconnects without persisting the
/// aborted message). `Err` when this write failed.
async fn persist_message(
    core: &Core,
    persist_error: &Mutex<Option<String>>,
    message: &LiveMessage,
) -> Result<Option<String>, ()> {
    let message = message.clone();
    let session = Arc::clone(&core.session);
    let gap = Arc::clone(&core.gap);
    // The gate owns the write, which keeps running if this run is
    // cancelled.
    let Ok(write) = core
        .work
        .spawn_blocking(move || persist_after_gap_check(&session, &gap, &message))
    else {
        return Ok(None);
    };
    let result = write
        .await
        .unwrap_or_else(|error| Err(format!("Session I/O task failed: {error}")));
    match result {
        Ok(Persisted::Written(entry_id)) => Ok(entry_id),
        // After a failed write, a later message would cross the gap.
        Ok(Persisted::AfterGap) => Err(()),
        Err(error) => {
            persist_error
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get_or_insert(error);
            Err(())
        }
    }
}

/// What [`persist_after_gap_check`] did.
#[derive(Debug, PartialEq, Eq)]
enum Persisted {
    Written(Option<String>),
    /// Refused: an earlier write failed.
    AfterGap,
}

/// Writes `message` through [`write_session`]. Blocks.
fn persist_after_gap_check(
    session: &SharedSession,
    gap: &Mutex<Option<String>>,
    message: &LiveMessage,
) -> Result<Persisted, String> {
    match write_session(session, gap, |session| persist(session, message)) {
        Ok(entry_id) => Ok(Persisted::Written(entry_id)),
        Err(WriteRefused::AfterGap(_)) => Ok(Persisted::AfterGap),
        Err(WriteRefused::Failed(error)) => Err(error),
    }
}

/// Why [`write_session`] wrote nothing more.
#[derive(Debug)]
enum WriteRefused<E = String> {
    /// An earlier write failed; its reason.
    AfterGap(String),
    /// This write failed; the session now has a gap.
    Failed(E),
}

impl<E: std::fmt::Display> WriteRefused<E> {
    /// The reason, as the session reports it.
    fn message(&self) -> String {
        match self {
            Self::AfterGap(error) => error.clone(),
            Self::Failed(error) => error.to_string(),
        }
    }
}

/// The session's write boundary: under the session lock, refuses once a
/// write has failed, and records this write's failure before the lock is
/// released, so a write queued behind a failing one sees it. A write that
/// fails part-way through a group of entries
/// ([`SessionManager::append_group`]) is a failure too: nothing is written
/// after its prefix, and the group is never written again. Blocks.
fn write_session<R, E: std::fmt::Display>(
    session: &SharedSession,
    gap: &Mutex<Option<String>>,
    write: impl FnOnce(&mut SessionManager) -> Result<R, E>,
) -> Result<R, WriteRefused<E>> {
    let mut session = lock(session);
    if let Some(error) = lock_gap(gap).clone() {
        return Err(WriteRefused::AfterGap(error));
    }
    write(&mut session).map_err(|error| {
        lock_gap(gap).get_or_insert_with(|| error.to_string());
        WriteRefused::Failed(error)
    })
}

/// The gap's lock, taken after the session lock and never before it.
fn lock_gap(gap: &Mutex<Option<String>>) -> std::sync::MutexGuard<'_, Option<String>> {
    gap.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs under the session lock as an injected failure happens; what it
/// returns runs before the change goes on.
#[cfg(test)]
pub(crate) type FailHook = Box<dyn FnOnce() -> Box<dyn FnOnce() + Send> + Send>;

/// Failures tests inject at the session's write boundary.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestHooks {
    /// Fails the next model change's group before its first line. Runs
    /// under the session lock as it fails; what it returns runs before the
    /// change goes on.
    pub fail_model_entry: Option<FailHook>,
    /// What [`Self::fail_model_entry`] returned.
    pub before_continuing: Option<Box<dyn FnOnce() + Send>>,
    /// Runs on the blocking pool after a navigation claimed the session
    /// and before it takes the session lock to move the leaf.
    pub before_navigation: Option<Box<dyn FnOnce() + Send>>,
    /// Runs on the blocking pool after a compaction's summary and before
    /// it takes the session lock to write its entry.
    pub before_compaction_commit: Option<Box<dyn FnOnce() + Send>>,
    /// Runs under the session lock right after a compaction's entry was
    /// written.
    pub after_compaction_commit: Option<Box<dyn FnOnce() + Send>>,
    /// Fails the next compaction entry at the session's write boundary,
    /// running the hook under the session lock as it fails.
    pub fail_compaction_entry: Option<Box<dyn FnOnce() + Send>>,
    /// Fails the next plugin decision entry at the session's write
    /// boundary.
    pub fail_plugin_decision: bool,
    /// Runs on the blocking pool before the next plugin decision write
    /// takes the session lock, that is before its cancellation admission.
    pub before_plugin_decision_admission: Option<Box<dyn FnOnce() + Send>>,
    /// Runs under the session lock once the next plugin decision write is
    /// admitted, before its entry is appended.
    pub plugin_decision_admitted: Option<Box<dyn FnOnce() + Send>>,
}

/// Pi's bookkeeping after persisting: the last assistant message and the
/// turn's tool results, overflow recovery, and the end of a successful
/// retry.
fn track_message(core: &Core, message: &LiveMessage, entry_id: Option<String>) {
    if message.as_tool_result().is_some() {
        core.state().turn_tool_results.push(entry_id);
        return;
    }
    let Some(assistant) = message.as_assistant() else {
        return;
    };
    let finished_retry = {
        let mut state = core.state();
        state.last_assistant = Some((assistant.clone(), entry_id));
        state.turn_tool_results.clear();
        if !matches!(
            assistant.stop_reason,
            bake_ai::StopReason::Error | bake_ai::StopReason::Length
        ) {
            state.overflow_recovery_attempted = false;
        }
        // A successful response ends the retry, so attempts do not
        // accumulate across the turns of a run.
        if assistant.stop_reason != bake_ai::StopReason::Error && state.retry_attempt > 0 {
            Some(std::mem::take(&mut state.retry_attempt))
        } else {
            None
        }
    };
    if let Some(attempt) = finished_retry {
        core.emit(&SessionEvent::AutoRetryEnd {
            success: true,
            attempt,
            final_error: None,
        });
    }
}

/// What one change ([`AgentSession::run_change`]) works with: the parts
/// of the session it reads and changes, owned by its task.
struct Control {
    agent: Arc<Agent>,
    core: Arc<Core>,
    session: SharedSession,
    registry: ModelRuntime,
    settings: SharedSettings,
    settings_store: Option<SettingsStore>,
    selection: SessionSelection,
    listeners: Arc<Listeners>,
    persist_error: Arc<Mutex<Option<String>>>,
    /// A share of the change's admission: its blocking work runs under it,
    /// so shutdown, which never refuses an admitted claim, waits for it.
    permit: WorkPermit,
}

impl Control {
    /// Runs `work` on the blocking pool under the change's permit.
    async fn blocking<R: Send + 'static>(
        &self,
        work: impl FnOnce() -> R + Send + 'static,
        what: &str,
    ) -> Result<R, String> {
        self.permit
            .spawn_blocking(work)
            .map_err(|refused| format!("{what} task failed: {refused}"))?
            .await
            .map_err(|error| format!("{what} task failed: {error}"))
    }

    /// Appends to the session file on the blocking pool, under the
    /// change's permit, so shutdown waits for it.
    async fn append(
        &self,
        write: impl FnOnce(&mut SessionManager) -> Result<(), crate::session::SessionError>
        + Send
        + 'static,
    ) -> Result<(), String> {
        self.write(write).await.map_err(|refused| refused.message())
    }

    /// [`Self::append`], keeping the session's error.
    async fn write(
        &self,
        write: impl FnOnce(&mut SessionManager) -> Result<(), crate::session::SessionError>
        + Send
        + 'static,
    ) -> Result<(), WriteRefused<crate::session::SessionError>> {
        let session = Arc::clone(&self.session);
        let core = Arc::clone(&self.core);
        let agent = Arc::clone(&self.agent);
        let written = self
            .permit
            .spawn_blocking(move || {
                let written = write_session(&session, &core.gap, write);
                // The run stops here, as soon as the write has failed and the
                // session lock is released, so a turn between steps takes no
                // further step while this change's task waits for a worker.
                if written.is_err() {
                    core.abort_requested.store(true, Ordering::SeqCst);
                    agent.abort();
                }
                written
            })
            // An admitted change's blocking work is never refused for a
            // closing gate; only a missing runtime refuses it, reported as
            // a write that never started.
            .map_err(|refused| {
                WriteRefused::AfterGap(format!("Session I/O task failed: {refused}"))
            })?
            .await
            // A task that did not finish reports only its reason, as a write
            // refused before it started does.
            .map_err(|error| WriteRefused::AfterGap(format!("Session I/O task failed: {error}")))?;
        // Off the async workers, with the hooks' lock released, so a test
        // that holds the change here never holds up the run it watches.
        #[cfg(test)]
        {
            let before = self.core.hooks().before_continuing.take();
            if let Some(before) = before {
                tokio::task::spawn_blocking(before)
                    .await
                    .expect("the test hook");
            }
        }
        written
    }

    /// A change's session write failed: the file now lacks an entry, so the
    /// running turn stops before its next request, no later message is
    /// written past the gap, and no later prompt is accepted. Returns the
    /// reason, naming `what` failed.
    fn stop_after_failed_write(&self, error: &str, what: &str) -> String {
        // The gap is recorded, and the run stopped, at the write boundary
        // ([`Self::append`]); this reports the failure to the run's prompt.
        self.persist_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(|| error.to_owned());
        format!(
            "{what} could not be written to the session file ({error}), so the session stopped and takes no more prompts."
        )
    }

    /// Applies a model and thinking level to the agent and the published
    /// selection together, so the next request reads one change.
    fn apply(&self, model: Option<Model>, level: ModelThinkingLevel) {
        let mut published = self.selection.lock();
        match &model {
            Some(model) => {
                self.agent
                    .set_model_and_thinking_level(model.clone(), level);
                self.core.has_model.store(true, Ordering::SeqCst);
            }
            None => self.agent.set_thinking_level(level),
        }
        *published = Selected {
            model: model.or_else(|| self.core.model()),
            thinking_level: level,
        };
    }

    /// Saves a setting on the blocking pool.
    async fn save(
        &self,
        save: impl FnOnce(&SettingsStore) -> Result<Settings, crate::json_store::StoreError>
        + Send
        + 'static,
    ) -> DefaultSave {
        let Some(store) = self.settings_store.clone() else {
            return DefaultSave::Failed("no settings file to save to".to_owned());
        };
        match self.blocking(move || save(&store), "Settings").await {
            Ok(Ok(_)) => DefaultSave::Saved,
            Ok(Err(error)) if error.is_committed() => DefaultSave::Unsynced(error.to_string()),
            Ok(Err(error)) => DefaultSave::Failed(error.to_string()),
            Err(error) => DefaultSave::Failed(error),
        }
    }
}

impl AgentSession {
    /// Pi's `createAgentSession`: selects the model and thinking level,
    /// restores the transcript, records a new session's initial model and
    /// thinking level as one group ([`SessionManager::append_group`]), and
    /// creates the agent. Must run inside a Tokio runtime.
    pub async fn create(options: AgentSessionOptions) -> Result<Self, String> {
        let AgentSessionOptions {
            session,
            registry,
            apis,
            settings,
            settings_store,
            selection: published,
            model,
            thinking_level,
            tools,
            active_tool_names,
            tool_prompts,
            mut system_prompt,
            docs_command,
            plugins,
            work,
        } = options;
        let plugins = VetoDispatcher::new(plugins).map(Arc::new);
        system_prompt.docs_command = docs_command.clone();
        let tools: Vec<Arc<AgentTool>> = tools.into_iter().map(run_in_order).collect();
        // Pi's `_toolPromptGuidelines`.
        for prompt in &tool_prompts {
            let guidelines = normalize_prompt_guidelines(&prompt.guidelines);
            if !guidelines.is_empty() {
                system_prompt
                    .tool_guidelines
                    .retain(|(name, _)| name != &prompt.name);
                system_prompt
                    .tool_guidelines
                    .push((prompt.name.clone(), guidelines));
            }
        }
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| "An agent session must be created inside a Tokio runtime.".to_owned())?;
        let session: SharedSession = Arc::new(Mutex::new(session));
        let work = work.unwrap_or_default();
        let (context, has_thinking_entry) = with_session(&work, &session, |session| {
            let context = session.build_session_context();
            let has_thinking_entry = session
                .branch_entries(None)
                .iter()
                .any(|entry| entry.entry_type() == "thinking_level_change");
            (context, has_thinking_entry)
        })
        .await?;
        let has_existing_session = !context.messages.is_empty();
        let snapshot = registry.snapshot();
        let selection = select_model(
            SelectionInputs {
                registry: &snapshot,
                settings: &settings,
                docs_command: &docs_command,
                model,
                thinking_level,
            },
            has_existing_session,
            context
                .model
                .as_ref()
                .map(|model| (model.provider.clone(), model.model_id.clone())),
            has_thinking_entry.then(|| context.thinking_level.clone()),
        );
        let messages: Vec<LiveMessage> = context
            .messages
            .iter()
            .map(crate::session::AgentMessage::to_agent)
            .collect();
        let block = settings.block_images;
        let session_id = lock(&session).session_id().to_owned();
        let stream_fn = with_tool_argument_whitespace(
            registry.stream_fn_owned(Arc::clone(&apis), Some(work.clone())),
            settings.max_tool_argument_whitespace,
        );
        let listeners = Arc::new(Listeners::default());
        let steering_mode = settings.steering_mode;
        let follow_up_mode = settings.follow_up_mode;
        let auto_resize_images = settings.image_auto_resize();
        let settings: SharedSettings = Arc::new(std::sync::RwLock::new(settings));
        let published = published.unwrap_or_default();
        *published.lock() = Selected {
            model: selection.model.clone(),
            thinking_level: selection.thinking_level,
        };
        let core = Arc::new(Core {
            session: Arc::clone(&session),
            settings: Arc::clone(&settings),
            listeners: Arc::clone(&listeners),
            stream_fn: Arc::clone(&stream_fn),
            work: work.clone(),
            agent: OnceLock::new(),
            has_model: AtomicBool::new(selection.model.is_some()),
            abort_requested: AtomicBool::new(false),
            state: Mutex::new(RunState::default()),
            activity: tokio::sync::watch::Sender::new(Activity::default()),
            gap: Arc::default(),
            #[cfg(test)]
            hooks: Mutex::default(),
            #[cfg(test)]
            session_io: tokio::sync::watch::Sender::new(0),
        });
        let after_tool_core = Arc::clone(&core);
        let persist_error = Arc::new(Mutex::new(None));
        let before_tool_call = plugins.as_ref().map(|plugins| {
            plugin_veto::plugin_veto_hook(
                Arc::clone(&core),
                Arc::clone(&persist_error),
                Arc::clone(plugins),
            )
        });
        let agent = Agent::new(AgentOptions {
            initial_state: AgentInitialState {
                system_prompt: None,
                model: selection.model.clone(),
                thinking_level: Some(selection.thinking_level),
                tools: Vec::new(),
                messages,
            },
            convert_to_llm: Some(hook::convert_to_llm(move |messages| {
                let converted = convert_to_llm(messages);
                Box::pin(async move {
                    if block {
                        block_images(converted)
                    } else {
                        converted
                    }
                })
            })),
            after_tool_call: Some(hook::after_tool_call(move |context, _signal| {
                let content = context.result.content.clone();
                let resize_options = after_tool_core
                    .model()
                    .and_then(|model| {
                        ImageResizeOptions::from_input_limits(model.input_limits.as_ref())
                    })
                    .unwrap_or_default();
                let work = after_tool_core.work.clone();
                Box::pin(async move {
                    if !content
                        .iter()
                        .any(|block| matches!(block, UserContentBlock::Image(_)))
                    {
                        return Ok(None);
                    }
                    // Owned by the gate, so a cancelled run's resize is
                    // still awaited by shutdown.
                    let Ok(normalizing) = work.spawn_blocking(move || {
                        normalize_tool_result_images(&content, auto_resize_images, resize_options)
                    }) else {
                        return Ok(None);
                    };
                    let normalized = normalizing.await.ok().flatten();
                    Ok(normalized.map(|content| AfterToolCallResult {
                        content: Some(content),
                        ..AfterToolCallResult::default()
                    }))
                })
            })),
            before_tool_call,
            stream_fn: Some(stream_fn),
            prepare_next_turn_with_context: Some(next_turn_hook(
                Arc::clone(&core),
                published.clone(),
            )),
            steering_mode,
            follow_up_mode,
            session_id: Some(session_id),
            ..AgentOptions::default()
        })
        .map_err(|error| error.to_string())?;
        let agent = Arc::new(agent);
        let _ = core.agent.set(Arc::downgrade(&agent));

        let model = selection.model.clone();
        let thinking = selection.thinking_level;
        with_session(&work, &session, move |session| {
            if has_existing_session {
                if !has_thinking_entry {
                    session.append_thinking_level_change(thinking.as_str())?;
                }
            } else {
                // One group. A new session writes nothing until its first
                // message, which creates the file with both entries; a
                // written file without a conversation gets them in order.
                let mut group = Vec::with_capacity(2);
                if let Some(model) = &model {
                    group.push(GroupEntry::model_change(&model.provider, &model.id));
                }
                group.push(GroupEntry::thinking_level_change(thinking.as_str()));
                session.append_group(group)?;
            }
            Ok::<(), crate::session::SessionError>(())
        })
        .await?
        .map_err(|error| error.to_string())?;

        let active_tool_names: Vec<String> = active_tool_names
            .into_iter()
            .filter(|name| tools.iter().any(|tool| &tool.name == name))
            .collect();
        // Last, after every fallible step: the keeper of the Wasm plugins'
        // helpers is owned work of the gate from here on.
        if let Some(plugins) = &plugins {
            plugins.attach(&work).map_err(refused_message)?;
        }
        let gap = Arc::clone(&core.gap);
        let jobs = JobRegistry::new(work.clone(), JobLimits::default());
        let created = Self {
            apis,
            agent,
            core,
            work,
            jobs,
            session,
            registry,
            settings,
            settings_store,
            selection: published,
            control: Arc::default(),
            tools,
            active_tool_names,
            prompt_options: system_prompt,
            prompt_templates: Vec::new(),
            docs_command,
            listeners,
            persist_error,
            subscription: Arc::default(),
            shutdown: OnceLock::new(),
            runtime,
            model_fallback_message: selection.fallback_message,
            queue: Arc::default(),
            write_failure: gap,
            mcp: OnceLock::new(),
            plugins,
        };
        created.connect();
        Ok(created)
    }

    /// Subscribes the session to its agent: listeners first, then
    /// persistence, as Pi's `_handleAgentEvent` orders them.
    fn connect(&self) {
        let core = Arc::clone(&self.core);
        let persist_error = Arc::clone(&self.persist_error);
        let queue = Arc::clone(&self.queue);
        let write_failure = Arc::clone(&self.write_failure);
        // Weak: the agent owns this listener.
        let agent = Arc::downgrade(&self.agent);
        let subscription = self.agent.subscribe(move |event, _signal| {
            let core = Arc::clone(&core);
            let persist_error = Arc::clone(&persist_error);
            let queue = Arc::clone(&queue);
            let write_failure = Arc::clone(&write_failure);
            let agent = agent.clone();
            Box::pin(async move {
                if let AgentEvent::MessageStart { message } = event
                    && let Some(user) = message.as_user()
                {
                    core.state().overflow_recovery_attempted = false;
                    // Pi removes a delivered prompt from its queue before
                    // emitting the message, so listeners see the new queue.
                    let text = bake_ai::utils::text::user_content_text(&user.content, "");
                    let delivered = !text.is_empty() && queue.lock().delivered(&text);
                    if delivered {
                        queue.announce(&core);
                    }
                }
                match event {
                    AgentEvent::AgentEnd { messages } => {
                        let will_retry = core.will_retry_after_agent_end(messages);
                        core.emit(&SessionEvent::AgentEnd {
                            messages,
                            will_retry,
                        });
                    }
                    event => core.emit(&SessionEvent::Agent(event)),
                }
                match event {
                    AgentEvent::MessageEnd { message } => {
                        let entry_id = persist_message(&core, &persist_error, message).await;
                        if entry_id.is_err() {
                            let failed = persist_error
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .clone();
                            write_failure
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .get_or_insert(failed.unwrap_or_default());
                            if let Some(agent) = agent.upgrade() {
                                agent.abort();
                            }
                        }
                        track_message(&core, message, entry_id.unwrap_or_default());
                    }
                    AgentEvent::TurnEnd { .. } => {
                        let mut state = core.state();
                        state.last_tool_results = std::mem::take(&mut state.turn_tool_results);
                    }
                    _ => {}
                }
            })
        });
        *self
            .subscription
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(subscription);
    }

    /// Subscribes to session events. Listeners run synchronously inside
    /// the agent's event dispatch; keep them short.
    pub fn subscribe(
        &self,
        listener: impl Fn(&SessionEvent<'_>) + Send + Sync + 'static,
    ) -> SessionSubscription {
        let id = self.listeners.next.fetch_add(1, Ordering::Relaxed);
        self.listeners
            .list
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((id, Arc::new(listener)));
        SessionSubscription {
            listeners: Arc::downgrade(&self.listeners),
            id,
        }
    }

    /// The agent.
    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    /// Why the session did not start with the model it would have restored
    /// or found, Pi's `modelFallbackMessage`.
    pub fn model_fallback_message(&self) -> Option<&str> {
        self.model_fallback_message.as_deref()
    }

    /// The model, unless none was available.
    pub fn model(&self) -> Option<Model> {
        self.core.model()
    }

    /// The thinking level.
    pub fn thinking_level(&self) -> ModelThinkingLevel {
        self.agent.thinking_level()
    }

    /// The session header as JSON, Pi's `getHeader()`.
    pub fn header_json(&self) -> Option<serde_json::Value> {
        lock(&self.session)
            .header()
            .map(|header| serde_json::Value::Object(header.as_json().clone()))
    }

    /// The session file, when the session persists.
    pub fn session_file(&self) -> Option<PathBuf> {
        let session = lock(&self.session);
        session
            .is_persisted()
            .then(|| session.session_file().map(PathBuf::from))
            .flatten()
    }

    /// The session's working directory.
    pub fn cwd(&self) -> String {
        lock(&self.session).cwd().to_owned()
    }

    /// Where the session's new sessions go, and whether it persists:
    /// Pi's `getSessionDir` and `isPersisted`.
    pub fn session_place(&self) -> (PathBuf, bool) {
        let session = lock(&self.session);
        (session.session_dir().to_path_buf(), session.is_persisted())
    }

    /// The display name, Pi's `getSessionName`.
    pub fn session_name(&self) -> Option<String> {
        lock(&self.session).session_name()
    }

    /// Pi's `setSessionName`: records `name` in a `session_info` entry,
    /// line breaks as spaces and trimmed, announces the name the session
    /// now has ([`SessionEvent::SessionInfoChanged`]), and returns it. Serialized and owned as the other changes
    /// ([`Self::run_change`]) and refused after shutdown; a failed write
    /// stops the session as any failed write does.
    pub async fn set_session_name(&self, name: &str) -> Result<Option<String>, String> {
        let name = name.to_owned();
        self.run_change(move |control| async move {
            let written = control
                .append(move |session| session.append_session_info(&name).map(|_| ()))
                .await;
            if let Err(error) = written {
                return Err(control.stop_after_failed_write(&error, "The session name"));
            }
            let session = Arc::clone(&control.session);
            let name = control
                .blocking(move || lock(&session).session_name(), "Session I/O")
                .await?;
            control
                .listeners
                .emit(&SessionEvent::SessionInfoChanged(name.clone()));
            Ok(name)
        })
        .await
    }

    /// Pi's `getSessionStats`, read from the session's entries: message
    /// counts by role, tool calls, and the token and cost totals of every
    /// usage the log records. Reads only; a malformed member counts as
    /// absent.
    pub fn session_stats(&self) -> SessionStats {
        let session = lock(&self.session);
        let mut stats = SessionStats {
            session_file: session
                .is_persisted()
                .then(|| session.session_file().map(PathBuf::from))
                .flatten(),
            session_id: session.session_id().to_owned(),
            name: session.session_name(),
            ..SessionStats::default()
        };
        for entry in session.entries() {
            let kind = entry.entry_type();
            if matches!(kind, "usage" | "branch_summary" | "compaction")
                && let Some(usage) = entry.get("usage")
            {
                stats.add_usage(usage);
            }
            if kind != "message" {
                continue;
            }
            let Some(message) = entry.get("message").and_then(serde_json::Value::as_object) else {
                continue;
            };
            stats.total_messages += 1;
            match message.get("role").and_then(serde_json::Value::as_str) {
                Some("user") => stats.user_messages += 1,
                Some("toolResult") => {
                    stats.tool_results += 1;
                    if let Some(usage) = message.get("usage") {
                        stats.add_usage(usage);
                    }
                }
                Some("assistant") => {
                    stats.assistant_messages += 1;
                    stats.tool_calls += message
                        .get("content")
                        .and_then(serde_json::Value::as_array)
                        .map_or(0, |blocks| {
                            blocks
                                .iter()
                                .filter(|block| {
                                    block.get("type").and_then(serde_json::Value::as_str)
                                        == Some("toolCall")
                                })
                                .count()
                        });
                    if let Some(usage) = message.get("usage") {
                        stats.add_usage(usage);
                    }
                }
                _ => {}
            }
        }
        stats
    }

    /// The session id.
    pub fn session_id(&self) -> String {
        lock(&self.session).session_id().to_owned()
    }

    /// The transcript.
    pub fn messages(&self) -> Vec<LiveMessage> {
        self.agent.messages()
    }

    /// The loaded prompt templates (`PromptTemplate`) used by [`Self::prompt`].
    pub fn prompt_templates(&self) -> &[PromptTemplate] {
        &self.prompt_templates
    }

    /// Sets the prompt templates expanded by [`Self::prompt`].
    pub fn set_prompt_templates(&mut self, prompt_templates: Vec<PromptTemplate>) {
        self.prompt_templates = prompt_templates;
    }

    /// The provider APIs the session's requests stream through.
    pub fn apis(&self) -> &Arc<ApiRegistry> {
        &self.apis
    }

    /// The models and their auth, which the frontend replaces after a
    /// login, logout, or catalog refresh.
    pub fn model_runtime(&self) -> &ModelRuntime {
        &self.registry
    }

    /// The live settings, including the changes the setters made.
    pub fn settings(&self) -> Settings {
        read_settings(&self.settings).clone()
    }

    /// The cell the session publishes its model and thinking level in.
    pub fn selection(&self) -> &SessionSelection {
        &self.selection
    }

    /// Pi's `getAvailableThinkingLevels`: the levels the model supports,
    /// or every level without a model.
    pub fn available_thinking_levels(&self) -> Vec<ModelThinkingLevel> {
        match self.model() {
            Some(model) => bake_ai::models::get_supported_thinking_levels(&model),
            None => vec![
                ModelThinkingLevel::Off,
                ModelThinkingLevel::Minimal,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
                ModelThinkingLevel::Xhigh,
                ModelThinkingLevel::Max,
            ],
        }
    }

    /// A work permit for a change, unless the session takes no more: a
    /// change after shutdown is refused rather than kept in memory.
    fn change_permit(&self) -> Result<WorkPermit, String> {
        if let Some(reason) = self.closed_reason() {
            return Err(reason);
        }
        self.work
            .enter()
            .ok_or_else(|| "The session is shut down.".to_owned())
    }

    /// Runs one change as owned work: it waits its turn behind the other
    /// changes, is admitted once with a work permit (refused after
    /// shutdown), and then runs to completion on its own task, which holds
    /// the turn and the permit. A caller that stops waiting therefore
    /// neither abandons a half-written change nor lets the next change
    /// start before it ends, and shutdown waits for it.
    async fn run_change<R, F>(&self, change: impl FnOnce(Control) -> F) -> Result<R, String>
    where
        R: Send + 'static,
        F: std::future::Future<Output = Result<R, String>> + Send + 'static,
    {
        let turn = Arc::clone(&self.control).lock_owned().await;
        let permit = self.change_permit()?;
        let work = change(Control {
            permit: permit.share(),
            agent: Arc::clone(&self.agent),
            core: Arc::clone(&self.core),
            session: Arc::clone(&self.session),
            registry: self.registry.clone(),
            settings: Arc::clone(&self.settings),
            settings_store: self.settings_store.clone(),
            selection: self.selection.clone(),
            listeners: Arc::clone(&self.listeners),
            persist_error: Arc::clone(&self.persist_error),
        });
        // A change ignores its task's cancellation: it is short, and a
        // half-applied change would leave the file and memory apart.
        permit
            .spawn(move |_scope| async move {
                let _turn = turn;
                work.await
            })
            .map_err(refused_message)?
            .await
            .map_err(|error| format!("The change failed: {error}"))?
    }

    /// Pi's `setThinkingLevel`: clamps to the model, records a change in the
    /// session file, then applies and announces it, and saves `level` as
    /// the default when `persist` is set.
    ///
    /// Serialized with the other changes and owned as [`Self::run_change`]
    /// describes; refused once the session is shut down. A change takes
    /// effect at the next model request, including the next request of a
    /// run that is streaming. When the session write fails, the change is
    /// not applied, the running turn stops, nothing more is written, and
    /// later prompts are refused, since the file would otherwise miss an
    /// entry. A failed save of the default leaves the session changed and
    /// is reported as [`DefaultSave::Failed`].
    pub async fn set_thinking_level(
        &self,
        level: ModelThinkingLevel,
        persist: bool,
    ) -> Result<DefaultSave, String> {
        self.run_change(move |control| async move {
            let effective = match control.core.model() {
                Some(model) => bake_ai::models::clamp_thinking_level(&model, level),
                None => level,
            };
            if effective != control.agent.thinking_level() {
                let written = control
                    .append(move |session| {
                        session
                            .append_thinking_level_change(effective.as_str())
                            .map(|_| ())
                    })
                    .await;
                if let Err(error) = written {
                    return Err(control.stop_after_failed_write(&error, "The thinking level"));
                }
                control.apply(None, effective);
                control
                    .listeners
                    .emit(&SessionEvent::ThinkingLevelChanged(effective));
            }
            if !persist {
                return Ok(DefaultSave::NotRequested);
            }
            let saved = control
                .save(move |store| store.set_default_thinking_level(level))
                .await;
            if saved.is_written() {
                write_settings(&control.settings).default_thinking_level = Some(level);
            }
            Ok(saved)
        })
        .await
    }

    /// Pi's `setModel`: refuses a model without auth, records the change and
    /// the thinking level the model's settings choose (a per-model level,
    /// else the default level, else the current one), applies both
    /// together, and saves the model as the default when `persist` is set.
    /// Serialized, owned, refused after shutdown, and effective from the
    /// next model request, as [`Self::set_thinking_level`] is. The model
    /// and thinking-level entries are one group
    /// ([`SessionManager::append_group`]), written in order with no entry
    /// between them, and the change is applied only once both are written.
    /// When the group's write fails, the session applies the entries known
    /// to be synced, then stops as for any failed write: nothing when the
    /// model entry is not known to be synced, else the new model with the
    /// previous level clamped to it. A line written without a sync may
    /// still be read by a later open of the file, so that open can differ
    /// ([`crate::session::GroupWriteError`]).
    pub async fn set_model(&self, model: Model, persist: bool) -> Result<DefaultSave, String> {
        self.run_change(move |control| async move {
            if control
                .registry
                .snapshot()
                .check_auth(&model.provider)
                .is_none()
            {
                return Err(format!("No API key for {}/{}", model.provider, model.id));
            }
            let previous = control.agent.thinking_level();
            let level = {
                let settings = read_settings(&control.settings);
                settings
                    .model_thinking_level(&model.provider, &model.id)
                    .or(settings.default_thinking_level)
                    .unwrap_or(previous)
            };
            let effective = bake_ai::models::clamp_thinking_level(&model, level);
            let changes_level = effective != previous;
            // One group, written under one hold of the session: no message
            // lands between the model and its level.
            let mut group = vec![GroupEntry::model_change(&model.provider, &model.id)];
            if changes_level {
                group.push(GroupEntry::thinking_level_change(effective.as_str()));
            }
            #[cfg(test)]
            let injected = control.core.hooks().fail_model_entry.take();
            #[cfg(test)]
            let core = Arc::clone(&control.core);
            let written = control
                .write(move |session| {
                    #[cfg(test)]
                    if let Some(hook) = injected {
                        core.hooks().before_continuing = Some(hook());
                        return Err(injected_failure());
                    }
                    session.append_group(group).map(|_| ())
                })
                .await;
            match written {
                Ok(()) => {}
                // The model change is synced and its thinking level is not
                // known to be: the session applies the known synced prefix,
                // the new model with the previous level clamped to it, and
                // halts. A reopen projects the same only when the level's
                // line is not readable; an unsynced line, or a torn one
                // whose whole JSON was written, reads as the new level.
                Err(WriteRefused::Failed(SessionError::GroupWrite(failure)))
                    if !failure.written.is_empty() =>
                {
                    let kept = bake_ai::models::clamp_thinking_level(&model, previous);
                    control.apply(Some(model.clone()), kept);
                    return Err(control.stop_after_failed_write(
                        &failure.to_string(),
                        "The model change was recorded, but its thinking level",
                    ));
                }
                Err(refused) => {
                    return Err(
                        control.stop_after_failed_write(&refused.message(), "The model change")
                    );
                }
            }
            control.apply(Some(model.clone()), effective);
            if changes_level {
                control
                    .listeners
                    .emit(&SessionEvent::ThinkingLevelChanged(effective));
            }
            if !persist {
                return Ok(DefaultSave::NotRequested);
            }
            let (provider, id) = (model.provider.clone(), model.id.clone());
            let saved = control
                .save(move |store| store.set_default_model_and_provider(&provider, &id))
                .await;
            if saved.is_written() {
                let mut settings = write_settings(&control.settings);
                settings.default_provider = Some(model.provider);
                settings.default_model = Some(model.id);
            }
            Ok(saved)
        })
        .await
    }

    /// Pi's `setSteeringMode`: applies the mode to the agent's steering
    /// queue now and saves it. Serialized and owned as the other changes.
    pub async fn set_steering_mode(&self, mode: QueueMode) -> Result<DefaultSave, String> {
        self.run_change(move |control| async move {
            control.agent.set_steering_mode(mode);
            write_settings(&control.settings).steering_mode = mode;
            Ok(control
                .save(move |store| store.set_steering_mode(mode))
                .await)
        })
        .await
    }

    /// Pi's `setFollowUpMode`: as [`Self::set_steering_mode`], for the
    /// follow-up queue.
    pub async fn set_follow_up_mode(&self, mode: QueueMode) -> Result<DefaultSave, String> {
        self.run_change(move |control| async move {
            control.agent.set_follow_up_mode(mode);
            write_settings(&control.settings).follow_up_mode = mode;
            Ok(control
                .save(move |store| store.set_follow_up_mode(mode))
                .await)
        })
        .await
    }

    /// Pi's `setAutoCompactionEnabled`: threshold and overflow compaction
    /// follow `enabled` from the next check, and it is saved as
    /// `compaction.enabled`.
    pub async fn set_auto_compaction_enabled(&self, enabled: bool) -> Result<DefaultSave, String> {
        self.run_change(move |control| async move {
            crate::settings::set_compaction_enabled_in(
                &mut write_settings(&control.settings),
                enabled,
            );
            Ok(control
                .save(move |store| store.set_compaction_enabled(enabled))
                .await)
        })
        .await
    }

    /// Pi's `steeringMode`.
    pub fn steering_mode(&self) -> QueueMode {
        self.agent.steering_mode()
    }

    /// Pi's `followUpMode`.
    pub fn follow_up_mode(&self) -> QueueMode {
        self.agent.follow_up_mode()
    }

    /// The system message patching the sections the transcript replays,
    /// Pi's `_preparePromptAndToolLoadout`; it also activates the tools.
    fn prepare_prompt_and_tools(&self) -> Result<Option<SystemMessage>, String> {
        let mut active: Vec<Arc<AgentTool>> = self
            .active_tool_names
            .iter()
            .filter_map(|name| self.tools.iter().find(|tool| &tool.name == name))
            .cloned()
            .collect();
        // The MCP tools published now, one generation per server; the run
        // keeps these for its whole length.
        if let Some(mcp) = self.mcp.get() {
            active.extend(mcp.tools());
        }
        self.agent.set_tools(active);
        let mut options = self.prompt_options.clone();
        options.selected_tools = self.active_tool_names.clone();
        options.hidden_tools = Vec::new();
        let sections = build_system_prompt_sections(&options).map_err(|error| error.to_string())?;
        let systems: Vec<Message> = self
            .agent
            .messages()
            .iter()
            .filter_map(|message| message.as_system().cloned().map(Message::System))
            .collect();
        let previous = get_current_system_message(&systems)
            .and_then(|system| system.sections)
            .unwrap_or_default();
        Ok(
            diff_system_prompt_sections(&previous, &sections).map(|sections| SystemMessage {
                content: SystemContent::Text(String::new()),
                sections: Some(sections),
                tools_added: None,
                tools_removed: None,
                timestamp: bake_ai::now_ms(),
            }),
        )
    }

    /// Pi's `prompt` for text and images: checks the model and its auth,
    /// compacts first when the last response calls for it, prepends the
    /// system-prompt patch, runs the agent with its retries and
    /// compactions until the session is idle, and reports the first
    /// failure.
    pub async fn prompt(&self, text: &str, images: Vec<ImageContent>) -> Result<(), String> {
        // Claimed at once: two concurrent prompts must not both start.
        let mut refusal = None;
        let agent_streaming = self.agent.is_streaming();
        self.core.activity.send_if_modified(|activity| {
            if activity.manual_compaction {
                refusal = Some("Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry.");
                false
            } else if activity.navigation {
                refusal = Some("Cannot submit a prompt while the session tree is being navigated. Wait for it to finish and retry.");
                false
            } else if activity.run || agent_streaming {
                // Pi queues with a `streamingBehavior`; queueing is not
                // ported.
                refusal = Some("Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.");
                false
            } else {
                activity.run = true;
                true
            }
        });
        if let Some(refusal) = refusal {
            return Err(refusal.to_owned());
        }
        let run = RunGuard(&self.core);
        // Reset at the claim, not at the run as in Pi, so an abort during
        // the compaction before the prompt also ends the prompt.
        self.core.abort_requested.store(false, Ordering::SeqCst);
        let docs_command = &self.docs_command;
        if let Some(reason) = self.closed_reason() {
            return Err(reason);
        }
        let Some(model) = self.model() else {
            return Err(format_no_model_selected_message(docs_command));
        };
        let registry = self.registry.snapshot();
        if !registry.has_configured_auth(&model.provider)
            && registry.check_auth(&model.provider).is_none()
        {
            return Err(format_no_api_key_found_message(
                &model.provider,
                docs_command,
            ));
        }
        // Compact before sending when the last response calls for it,
        // aborted ones included; the new prompt is sent either way.
        let last = self
            .agent
            .messages()
            .iter()
            .rev()
            .find_map(|message| message.as_assistant().cloned());
        if let Some(last) = last {
            self.core
                .check_compaction(&last, None, false, Vec::new())
                .await?;
        }
        let (text, images) = self.prepare_user_input(text, images, Some(model)).await?;
        // Preparation may outlast a shutdown or a failed write.
        if let Some(reason) = self.closed_reason() {
            return Err(reason);
        }
        // The first prompt waits for MCP servers still connecting, so their
        // tools are declared in its first request, as Pi's
        // `waitForDirectServers` does.
        if let Some(mcp) = self.mcp.get() {
            mcp.wait_for_startup(|| self.core.aborted() || self.closed_reason().is_some())
                .await;
            if let Some(reason) = self.closed_reason() {
                return Err(reason);
            }
        }
        let mut content = vec![UserContentBlock::Text(TextContent::new(text))];
        content.extend(images.into_iter().map(UserContentBlock::Image));
        let mut messages: Vec<LiveMessage> = Vec::with_capacity(2);
        if let Some(update) = self.prepare_prompt_and_tools()? {
            messages.push(update.into());
        }
        messages.push(
            UserMessage {
                content: UserContent::Blocks(content),
                timestamp: bake_ai::now_ms(),
            }
            .into(),
        );
        self.run_agent_prompt(messages, run).await
    }

    /// Pi's `_runAgentPrompt`: the run, then retries, overflow recovery,
    /// and queued messages until the session settles.
    async fn run_agent_prompt(
        &self,
        messages: Vec<LiveMessage>,
        run: RunGuard<'_>,
    ) -> Result<(), String> {
        let core = &self.core;
        {
            let mut state = core.state();
            state.last_assistant = None;
            state.last_tool_results.clear();
            state.turn_tool_results.clear();
        }
        let mut result = if core.aborted() {
            Ok(())
        } else {
            self.agent
                .prompt(PromptInput::Messages(messages))
                .await
                .map_err(|error: AgentError| error.to_string())
        };
        while result.is_ok() && !core.aborted() {
            match self.handle_post_agent_run().await {
                Ok(true) => {
                    if core.aborted() {
                        break;
                    }
                    result = self.agent.continue_run().await.map_err(|e| e.to_string());
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    result = Err(error);
                    break;
                }
            }
            // Without extensions, Pi's pre-settlement boundary continues
            // only for queued messages.
            if core.aborted() || !self.agent.has_queued_messages() {
                break;
            }
            result = self.agent.continue_run().await.map_err(|e| e.to_string());
        }
        if core.aborted() {
            core.finish_cancelled_retry();
        }
        let aborted = core.aborted();
        // Listeners of `agent_settled` may prompt again.
        drop(run);
        core.emit(&SessionEvent::AgentSettled { aborted });
        result?;
        let persisted = self
            .persist_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        // A prompt starts only without a gap, so one now opened during this
        // run, even when the change that failed has not reported it yet.
        let gap = || lock_gap(&self.core.gap).clone();
        match persisted.or_else(gap) {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Pi's `_handlePostAgentRun`: whether to continue the agent.
    async fn handle_post_agent_run(&self) -> Result<bool, String> {
        let core = &self.core;
        let (last, tool_results) = {
            let mut state = core.state();
            (
                state.last_assistant.take(),
                std::mem::take(&mut state.last_tool_results),
            )
        };
        if core.aborted() {
            core.finish_cancelled_retry();
            return Ok(false);
        }
        let Some((message, entry_id)) = last else {
            return Ok(self.agent.has_queued_messages());
        };
        if core.is_retryable_error(&message)
            && core.prepare_retry(&message, entry_id.clone()).await?
        {
            if core.aborted() {
                core.finish_cancelled_retry();
            }
            return Ok(!core.aborted());
        }
        if core.aborted() {
            core.finish_cancelled_retry();
            return Ok(false);
        }
        if message.stop_reason == bake_ai::StopReason::Error {
            let attempt = std::mem::take(&mut core.state().retry_attempt);
            if attempt > 0 {
                core.emit(&SessionEvent::AutoRetryEnd {
                    success: false,
                    attempt,
                    final_error: message.error_message.clone(),
                });
            }
        }
        if core
            .check_compaction(&message, entry_id, true, tool_results)
            .await?
        {
            return Ok(!core.aborted());
        }
        // The loop drains both queues before `agent_end`; messages queued
        // by `agent_end` listeners need a fresh run.
        Ok(!core.aborted() && self.agent.has_queued_messages())
    }

    /// Pi's `steer`: expands a `/skill:` command and prompt templates and
    /// normalizes images as [`Self::prompt`] does, then queues the prompt for
    /// the running turn's next step, after its tool calls and before its next
    /// model request. A prompt queued while the agent is idle is delivered
    /// with the next prompt's first turn. Refused once the session is shut
    /// down or a session write has failed.
    pub async fn steer(&self, text: &str, images: Vec<ImageContent>) -> Result<(), String> {
        self.queue_user_input(text, images, true).await
    }

    /// Pi's `followUp`: like [`Self::steer`], but delivered only when the
    /// agent would otherwise stop.
    pub async fn follow_up(&self, text: &str, images: Vec<ImageContent>) -> Result<(), String> {
        self.queue_user_input(text, images, false).await
    }

    /// The prompts queued and not delivered yet, Pi's
    /// `getSteeringMessages` and `getFollowUpMessages`.
    pub fn queued_messages(&self) -> QueuedMessages {
        self.queue.lock().clone()
    }

    /// Pi's `clearQueue`: empties both queues, the agent's included, and
    /// returns what they held, so an interface can give the text back.
    pub fn clear_queue(&self) -> QueuedMessages {
        let taken = {
            let mut queued = self.queue.lock();
            self.agent.clear_all_queues();
            std::mem::take(&mut *queued)
        };
        self.queue.announce(&self.core);
        taken
    }

    /// Why the session takes no more prompts: it is shut down, or a write
    /// to its file failed, so the file would miss a message.
    fn closed_reason(&self) -> Option<String> {
        // Listeners close first thing in shutdown, before the work gate.
        if self.listeners.closed.load(Ordering::SeqCst) || self.work.is_closing() {
            return Some("The session is shut down.".into());
        }
        self.write_failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|error| {
                format!("The session file could not be written ({error}), so the session takes no more prompts.")
            })
    }

    /// Pi's `_queueUserInput`, `_queueSteer`, and `_queueFollowUp`. The
    /// preparation's blocking work holds work permits, so shutdown waits
    /// for it; the session is checked again once it is done, and the prompt
    /// is queued under a permit, so admission cannot outlive shutdown.
    async fn queue_user_input(
        &self,
        text: &str,
        images: Vec<ImageContent>,
        steer: bool,
    ) -> Result<(), String> {
        if let Some(reason) = self.closed_reason() {
            return Err(reason);
        }
        let (text, images) = self.prepare_user_input(text, images, self.model()).await?;
        let permit = self.work.enter();
        if let Some(reason) = self.closed_reason() {
            return Err(reason);
        }
        // The transcript a queued prompt would join is being replaced.
        if self.core.activity.borrow().navigation {
            return Err(NAVIGATION_WHILE_BUSY.to_owned());
        }
        let Some(_permit) = permit else {
            return Err("The session is shut down.".into());
        };
        let mut content = vec![UserContentBlock::Text(TextContent::new(text.clone()))];
        content.extend(images.into_iter().map(UserContentBlock::Image));
        let message = UserMessage {
            content: UserContent::Blocks(content),
            timestamp: bake_ai::now_ms(),
        };
        {
            let mut queued = self.queue.lock();
            if steer {
                queued.steering.push(text);
                self.agent.steer(message);
            } else {
                queued.follow_up.push(text);
                self.agent.follow_up(message);
            }
        }
        self.queue.announce(&self.core);
        Ok(())
    }

    /// The preparation every user prompt gets, Pi's `_expandSkillCommand`,
    /// `expandPromptTemplate`, and `_normalizePromptImages`: the expanded
    /// text, with any image hints appended, and the images resized for
    /// `model`'s limits.
    async fn prepare_user_input(
        &self,
        text: &str,
        images: Vec<ImageContent>,
        model: Option<Model>,
    ) -> Result<(String, Vec<ImageContent>), String> {
        let shut_down = || "The session is shut down.".to_owned();
        let mut text = if text.starts_with("/skill:") {
            let skills = self.prompt_options.skills.clone();
            let command = text.to_owned();
            // The gate owns the work, so shutdown waits for it even when
            // this future is dropped.
            self.work
                .spawn_blocking(move || expand_skill_command(&command, &skills))
                .map_err(|_| shut_down())?
                .await
                .unwrap_or_else(|_| text.to_owned())
        } else {
            text.to_owned()
        };
        text = expand_prompt_template(&text, &self.prompt_templates);
        if images.is_empty() {
            return Ok((text, images));
        }
        let auto_resize = read_settings(&self.settings).image_auto_resize();
        let resize_options = model
            .and_then(|model| ImageResizeOptions::from_input_limits(model.input_limits.as_ref()))
            .unwrap_or_default();
        let (normalized, hints) = self
            .work
            .spawn_blocking(move || normalize_prompt_images(&images, auto_resize, resize_options))
            .map_err(|_| shut_down())?
            .await
            .map_err(|error| format!("Image processing task failed: {error}"))?;
        if !hints.is_empty() {
            text = format!("{text}\n\n{}", hints.join("\n"));
        }
        Ok((text, normalized))
    }

    /// Pi's `prompt` check before a request: a model is selected and its
    /// provider has an API key. Interactive mode runs it before it takes
    /// the terminal.
    pub fn check_model_auth(&self) -> Result<(), String> {
        let Some(model) = self.model() else {
            return Err(format_no_model_selected_message(&self.docs_command));
        };
        let registry = self.registry.snapshot();
        if !registry.has_configured_auth(&model.provider)
            && registry.check_auth(&model.provider).is_none()
        {
            return Err(format_no_api_key_found_message(
                &model.provider,
                &self.docs_command,
            ));
        }
        Ok(())
    }

    /// Pi's `compact`: aborts the current run, then summarizes the history
    /// before the kept recent messages, records the compaction, and
    /// replaces the transcript with the compacted context. Never retried or
    /// continued.
    pub async fn compact(
        &self,
        custom_instructions: Option<&str>,
    ) -> Result<CompactionResult, String> {
        self.abort().await;
        let no_model = format_no_model_selected_message(&self.docs_command);
        let busy = self.core.busy(
            |activity| activity.manual_compaction = true,
            |activity| activity.manual_compaction = false,
        );
        self.core
            .manual_compaction(custom_instructions, no_model, None, busy)
            .await
    }

    /// Pi's `compact` for interactive `/compact`, idle only: rather than
    /// aborting a running response, it is refused with
    /// [`COMPACTION_WHILE_STREAMING`] while one runs, and with
    /// [`COMPACTION_WHILE_BUSY`] while a compaction or tree navigation
    /// runs. The claim is atomic with the prompt's, so no prompt starts
    /// while it runs ([`Self::prompt`] refuses with Pi's message) and it
    /// never starts while one does.
    ///
    /// `signal` cancels it until its entry is written: a cancel before the
    /// claim, during the branch read, during the summary request, or while
    /// the write waits for the session lock fails with
    /// [`COMPACTION_CANCELLED`] and writes nothing, leaving the session
    /// and the agent's transcript as they were. A compaction whose entry
    /// was written reports success even when the cancel came after. A
    /// preparation or summary error, such as Pi's `Nothing to compact
    /// (session too small)`, `Already compacted`, or a failed summary
    /// request, writes nothing and leaves the session usable. A failed
    /// write of the entry is reported as that failure, even when a cancel
    /// came after it, and stops the session as every failed write does:
    /// the entry stays in memory, as in Pi, nothing more is written, and
    /// later prompts are refused.
    ///
    /// It runs as owned work on its own task, holding a work permit, so a
    /// caller that stops waiting neither abandons it nor outlives it:
    /// shutdown cancels it and waits for it. Refused once the session is
    /// shut down.
    pub async fn compact_when_idle(
        &self,
        custom_instructions: Option<&str>,
        signal: AbortSignal,
    ) -> Result<CompactionResult, String> {
        let permit = self.change_permit()?;
        let core = Arc::clone(&self.core);
        let agent = Arc::clone(&self.agent);
        let instructions = custom_instructions.map(str::to_owned);
        let no_model = format_no_model_selected_message(&self.docs_command);
        // Shutdown cancels it through the compaction's abort controller
        // (`abort_retry_and_compaction`), as `abort` does, before it closes
        // the gate.
        permit
            .spawn(move |_scope| async move {
                if signal.aborted() {
                    return Err(COMPACTION_CANCELLED.to_owned());
                }
                let busy = core.claim_manual_compaction(agent.is_streaming())?;
                core.manual_compaction(instructions.as_deref(), no_model, Some(signal), busy)
                    .await
            })
            .map_err(refused_message)?
            .await
            .map_err(|error| format!("The compaction task failed: {error}"))?
    }

    /// Pi's `abortCompaction`: cancels a running compaction.
    pub fn abort_compaction(&self) {
        let state = self.core.state();
        for controller in [&state.compaction_abort, &state.auto_compaction_abort]
            .into_iter()
            .flatten()
        {
            controller.abort();
        }
    }

    /// Pi's `abortRetry`: cancels the wait before a retry.
    pub fn abort_retry(&self) {
        if let Some(controller) = &self.core.state().retry_abort {
            controller.abort();
        }
    }

    /// Pi's `isCompacting`, which also counts a tree navigation.
    pub fn is_compacting(&self) -> bool {
        let activity = *self.core.activity.borrow();
        activity.manual_compaction || activity.auto_compaction || activity.navigation
    }

    /// Pi's `isRetrying`: waiting before a retry.
    pub fn is_retrying(&self) -> bool {
        self.core.state().retry_abort.is_some()
    }

    /// Pi's `retryAttempt`: 0 when not retrying.
    pub fn retry_attempt(&self) -> u32 {
        self.core.state().retry_attempt
    }

    /// Pi's `isStreaming`: a prompt and its post-run work are running.
    pub fn is_streaming(&self) -> bool {
        self.core.activity.borrow().run
    }

    /// Pi's `autoCompactionEnabled`.
    pub fn auto_compaction_enabled(&self) -> bool {
        read_settings(&self.settings).compaction_enabled()
    }

    /// Pi's `autoRetryEnabled`.
    pub fn auto_retry_enabled(&self) -> bool {
        read_settings(&self.settings).retry.enabled
    }

    /// The helpers this session's Wasm plugins have started and not yet
    /// seen stop, in dispatch order; empty without Wasm plugins. After
    /// [`Self::shutdown`] returns, every one of them has been reaped.
    pub fn plugin_helpers(&self) -> Vec<crate::plugins::wasm::HelperInfo> {
        self.plugins
            .as_ref()
            .map(|plugins| plugins.helpers())
            .unwrap_or_default()
    }

    /// Pi's `abort`: signals the run, cancels a retry wait and a
    /// compaction, and waits until the session is idle.
    pub async fn abort(&self) {
        if self.core.activity.borrow().run {
            self.core.abort_requested.store(true, Ordering::SeqCst);
        }
        self.core.abort_retry_and_compaction();
        if let Some(mcp) = self.mcp.get() {
            mcp.interrupt_startup_wait();
        }
        self.agent.abort();
        self.agent.wait_for_idle().await;
        self.core.wait_for_idle().await;
    }

    /// Starts the MCP servers of `options` ([`crate::mcp`]), each as a task
    /// the session's gate owns, admitted under the session's sandbox mode
    /// before it is spawned. Returns at once; the first prompt waits up to
    /// [`crate::mcp::McpTimings::startup_wait`] for servers still
    /// connecting, and each prompt declares the tools published when it
    /// starts. [`Self::shutdown`] stops every server and returns only once
    /// each server's process and tree are gone.
    ///
    /// Refused once shutdown has started, when called a second time, or,
    /// before anything starts, when a wait in `options` gives a deadline
    /// the clock cannot represent ([`crate::mcp`]'s checks).
    pub fn start_mcp(&self, options: McpOptions) -> Result<McpServers, String> {
        if self.work.is_closing() {
            return Err(refused_message(Refused::Closed));
        }
        // Before anything is stored or started, so the session may start
        // corrected options later.
        crate::mcp::validate(&options)?;
        let reserved: Vec<String> = self.tools.iter().map(|tool| tool.name.clone()).collect();
        let cwd = PathBuf::from(self.cwd());
        let mut started = None;
        let servers = self.mcp.get_or_init(|| {
            let (servers, refused) = crate::mcp::start(options, cwd, reserved, &self.work);
            started = Some(refused);
            servers
        });
        match started {
            None => Err("MCP servers were already started for this session.".to_owned()),
            Some(Some(refused)) => Err(format!(
                "The session could not start its MCP servers: {refused}."
            )),
            Some(None) => Ok(servers.clone()),
        }
    }

    /// The MCP servers [`Self::start_mcp`] started, if it has.
    pub fn mcp_servers(&self) -> Option<&McpServers> {
        self.mcp.get()
    }

    /// Starts work the session owns in the background, beside its turns:
    /// [`Self::abort`] and the end of a turn leave it running; only
    /// [`OwnedTask::cancel`] and [`Self::shutdown`] abort its
    /// [`TaskScope::signal`], and shutdown returns only after it, and every
    /// task it spawned from its scope, has ended. Work that ignores the
    /// signal holds shutdown until it ends.
    ///
    /// `work` is called at once, before this returns, with the task's
    /// scope; its future runs on a Tokio task. Dropping the returned task
    /// gives up its output only. Refused with `The session is shut down.`
    /// once shutdown has started.
    pub fn spawn_background<T, F, Fut>(&self, work: F) -> Result<OwnedTask<T>, String>
    where
        T: Send + 'static,
        F: FnOnce(TaskScope) -> Fut,
        Fut: std::future::Future<Output = T> + Send + 'static,
    {
        self.work.spawn(work).map_err(refused_message)
    }

    /// The session's background jobs ([`crate::jobs`]). Like
    /// [`Self::spawn_background`] work, a job outlives [`Self::abort`] and
    /// the turn that started it; [`Self::shutdown`] stops every job and
    /// returns only once each is terminal, which is after the processes
    /// and tasks it started are settled. Each session has its own registry,
    /// so another session's jobs are refused. Jobs end with the process:
    /// a resumed session starts with none.
    pub fn jobs(&self) -> &JobRegistry {
        &self.jobs
    }

    /// Pi's `dispose`, awaited. At the first call's first poll, before it
    /// waits for anything, it removes every listener, refuses new prompts,
    /// changes, owned work, and jobs, and aborts the run, a retry wait, a
    /// compaction, every background task ([`Self::spawn_background`]), and
    /// every job ([`Self::jobs`], claiming each job's report, since a
    /// session being torn down reads no notice). The rest runs on a task of
    /// its own: it waits for the run's task, the session's writes, model
    /// requests, changes, background tasks, and jobs, and for any
    /// listener call already running, then disconnects from the agent and
    /// closes the session file for writing, releasing its lock.
    ///
    /// The teardown runs on the runtime the session was created on, so
    /// `shutdown` may be called and awaited from any thread or executor.
    /// Every call, concurrent or later, returns once that teardown has
    /// finished, and then no listener, hook, or owned work runs or starts.
    /// Dropping a call's future stops neither the teardown nor the refusal:
    /// the session never reopens. Later prompts fail and emit no events,
    /// and a later model or thinking-level change is not written. If the
    /// session's runtime has shut down, or the teardown panics, no teardown
    /// can finish: calls then return without it, having done only the
    /// first poll's part, and the session file is released when the
    /// session is dropped.
    ///
    /// The drain also covers the processes of tools given this session's
    /// gate ([`AgentSessionOptions::work`]): each holds a permit from its
    /// spawn until its tree is settled, and the run's task, stopped first,
    /// drops a running call, which kills the call's tree and moves the
    /// process to a task that settles it under the same permit. Shutdown
    /// therefore returns only once every such tree is gone, with no time
    /// limit; a process that cannot die (uninterruptible sleep) holds it.
    /// Processes of other sessions, or of tools given no owner, are not
    /// waited for, and a descendant that left its tree is not owned
    /// ([`bake_process`]). A model request's task awaits the
    /// provider task behind its stream, so draining the gate awaits both.
    pub async fn shutdown(&self) {
        let mut done = self.shutdown.get_or_init(|| self.begin_shutdown()).clone();
        // An error means the teardown task ended without finishing: it
        // panicked, or the session's runtime shut down and dropped it.
        let _ = done.wait_for(|done| *done).await;
    }

    /// The synchronous half of [`Self::shutdown`], run once; spawns the
    /// awaited half on the session's runtime and returns its completion.
    fn begin_shutdown(&self) -> tokio::sync::watch::Receiver<bool> {
        // Closed before anything is cancelled: a cancelled retry wait or
        // compaction resumes on another worker at once, and Pi's
        // synchronous `dispose` clears its listeners before any of that
        // work's continuations run.
        self.listeners.close();
        self.core.abort_requested.store(true, Ordering::SeqCst);
        self.core.abort_retry_and_compaction();
        if let Some(mcp) = self.mcp.get() {
            mcp.interrupt_startup_wait();
        }
        // Also cancels every MCP server's task, which stops its server
        // before its permit is released.
        // The jobs' drivers are the gate's tasks, so the drain below waits
        // for every job to become terminal; this future need not be kept.
        drop(self.jobs.close());
        let drained = self.work.close();
        let agent = Arc::clone(&self.agent);
        let listeners = Arc::clone(&self.listeners);
        let subscription = Arc::clone(&self.subscription);
        let session = Arc::clone(&self.session);
        let (finished, done) = tokio::sync::watch::channel(false);
        let teardown = async move {
            agent.shutdown().await;
            // The run's task is gone, so a tool call it was running has
            // dropped its process, whose settling task holds a permit of
            // this gate: the drain waits for it.
            drained.await;
            listeners.quiesce().await;
            if let Some(subscription) = subscription
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                subscription.unsubscribe();
            }
            // Nothing writes the session any more: another writer may take
            // its file over, and a late change stays in memory.
            lock(&session).close_writer();
            finished.send_replace(true);
        };
        // Not owned by the gate, which it closes: every caller of
        // `shutdown` waits for it, and it runs whether or not one does, on
        // the session's runtime whatever thread or executor called.
        drop(self.runtime.spawn(teardown));
        done
    }
}

/// Clears the session's run flag however the prompt ends, including when
/// its future is dropped.
struct RunGuard<'a>(&'a Core);

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.0.activity.send_modify(|activity| activity.run = false);
    }
}

#[cfg(test)]
mod control_tests;
#[cfg(test)]
mod navigation_tests;
#[cfg(test)]
mod plugin_tests;
#[cfg(test)]
mod recovery_tests;
#[cfg(test)]
mod service_tests;
#[cfg(test)]
mod tests;

/// The failure a test injects into a session write.
#[cfg(test)]
fn injected_failure() -> crate::session::SessionError {
    crate::session::SessionError::Io {
        path: "injected".into(),
        source: std::io::Error::other("injected failure"),
    }
}
