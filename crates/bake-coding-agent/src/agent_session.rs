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
//! Every write is synced before the run goes on, so a killed process
//! reopens to three checkpoints. A model request follows the sync of
//! everything it sends: a new session's file is created at its first user
//! message, as Pi v1.3.0's `SessionManager` does (`_hasConversation`, Pi
//! #10000), and a retried attempt's omission and a compaction are written
//! before the request after them. A tool's first effect follows the sync of
//! the assistant message that called it and, in a recoverable run, of its
//! dispatch record. The next step's request follows the tool results.
//!
//! The session's [`WorkGate`] owns the work that runs outside the task
//! awaiting it: the full prompt driver, session writes and other blocking work, model requests,
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
//! v1.2.0's `navigateTree` and runtime `fork`, with Pi v1.3.0's `at`
//! position for `/clone` (in
//! `agent_session/navigation.rs`).
//!
//! Plugins ([`crate::plugins`], given as [`AgentSessionOptions::plugins`]):
//! trusted compiled-in ones and the Wasm plugins a Bake home enables
//! ([`crate::plugins::installed`]) veto tool calls through the agent's
//! `before_tool_call` hook, as Pi v1.3.0's `_beforeToolCall` runs
//! `tool_call` handlers (in `agent_session/plugin_veto.rs`); each block is
//! written as a `custom` entry before the denial result. Opt-in recoverable
//! runs journal allowed dispatches through the same boundary. The session
//! owns every Wasm helper and reaps it before shutdown returns. A
//! permission engine bound to the session ([`AgentSession::record_approvals`])
//! writes each approval request and its answer through that boundary too
//! (in `agent_session/approval_history.rs`), before the call can run.
//! A call a tool makes through `bake_agent::ToolContext::nested` is run by
//! the agent loop through the same hooks and tools, so the vetoes, the
//! journal (which records it by its own id and its parent's), the
//! permission wrappers, and the approvals apply to it as to the model's.
//!
//! External hooks ([`crate::hooks`], installed by
//! [`AgentSession::set_hooks`]) run at the same extension points (in
//! `agent_session/hooks.rs`): `SessionStart` and `UserPromptSubmit` before
//! a prompt, `PreToolUse` after the plugins' vetoes, `PostToolUse` before a
//! result's images are normalized, and `Stop` where a run would settle.
//! Each evaluation is written as a `custom` entry before it takes effect.
//!
//! Not ported (deferred): Pi's extensions and their other events (including
//! `session_before_compact`, `session_compact`, and
//! `session_compact_failed`), branch summaries during navigation, the
//! `/skill:name` slash command list and its expansion error event, prompt
//! templates, MCP, bash `!` commands, the cache warmer, scoped
//! models, virtual models and their failed-response routing, the
//! `entry_appended` event, and tool-loadout changes between turns.

mod approval_history;
mod background_compaction;
mod cached_compaction;
mod codemode;
mod context_masking;
mod control;
mod create;
mod events;
mod hooks;
mod input;
mod job_journal;
pub mod job_notices;
mod navigation;
mod persistence;
mod plugin_veto;
mod prompt_execution;
mod recovery;
mod run_journal;
pub(crate) mod schedules;
mod services;
pub mod share;
mod stats;
pub(crate) mod task_documents;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

pub use background_compaction::BackgroundCompactionStatus;
use control::Control;
use create::{model_profile, profile_tool_names};
use events::Listeners;
pub use events::{SessionEvent, SessionSubscription};
#[cfg(test)]
use input::IMAGE_READING_DISABLED;
pub use input::QueuedMessages;
use input::{Queue, block_images, normalize_prompt_images, with_tool_argument_whitespace};
pub use navigation::{
    BRANCH_SUMMARY_CANCELLED, ForkPoint, ForkPosition, ForkedSession, NAVIGATION_CANCELLED,
    NAVIGATION_WHILE_BUSY, NAVIGATION_WHILE_STREAMING, NO_SUMMARY_MODEL, TreeNavigation,
    TreeNavigationOptions,
};
#[cfg(test)]
use persistence::{Persisted, persist_after_gap_check};
use persistence::{
    WriteRefused, lock_gap, next_turn_hook, persist_message, track_message, write_session,
};
use recovery::{Activity, Core, RunState};
pub use recovery::{
    COMPACTION_CANCELLED, COMPACTION_WHILE_BUSY, COMPACTION_WHILE_STREAMING, CompactionReason,
    SummarizationSource,
};
pub use run_journal::{RecoverableRunResult, RunRecovery, RunRecoveryDisposition};
pub use stats::{ContextUsage, SessionStats, UsageTotals};

use bake_agent::{
    AfterToolCallResult, Agent, AgentError, AgentEvent, AgentInitialState,
    AgentMessage as LiveMessage, AgentOptions, AgentTool, PromptInput, QueueMode, RunControl,
    RunOutcome, Subscription, ToolEffect, ToolExecutionMode, hook,
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

/// Bake's form of Pi's `getProviderLoginHelp`. `bake` has no `/login`,
/// so the help names the files that configure a model and the `docs` topic
/// that describes them. `docs_command` runs `bake docs`
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
    /// The executable tools. A tool without an [`AgentTool::effect`] is run
    /// as [`ToolEffect::Exec`] unless it sets `execution_mode` to parallel.
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
    /// How to run `bake docs`, such as `/usr/local/bin/bake docs`,
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
    /// The session's background jobs, built over `work`
    /// ([`JobRegistry::new`]); `None` makes a registry of its own. Give the
    /// same registry to the job tools and to `bash`
    /// ([`crate::tools::index::ToolsOptions::jobs`]) so their jobs are the
    /// ones the session delivers notices for and shuts down. A registry
    /// over another gate is refused, as is a closed or nonempty registry:
    /// the session attaches its immutable journal before the first job.
    pub jobs: Option<JobRegistry>,
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
    executions: prompt_execution::SharedExecutions,
    work: WorkGate,
    /// The session's background jobs, owned by `work`.
    jobs: JobRegistry,
    /// Job notices that wait for the next prompt: lost jobs found at
    /// creation, and completions when the wake budget is spent.
    pending_notices: Arc<Mutex<Vec<CustomMessage>>>,
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
    mcp: Arc<OnceLock<McpServers>>,
    /// What the `codemode` tool reads of the session, and its loadout's.
    codemode: Arc<codemode::SessionCodemodeHost>,
    /// The declarations the active `codemode` tool's loadout hides from
    /// requests.
    hidden_declarations: codemode::HiddenDeclarations,
    /// The plugin dispatcher, when any plugin is registered.
    plugins: Option<Arc<VetoDispatcher>>,
    /// The external hooks [`Self::set_hooks`] installed, once it has.
    hooks: hooks::SharedHooks,
    /// The agent's tool-call hooks, for a resumed run's reruns.
    tool_hooks: run_journal::ToolHooks,
    /// The subagents this session starts, when `subagents` is on
    /// ([`Self::set_subagents`]).
    subagents: Option<crate::subagents::SubagentHost>,
    /// The session's telecom station, or why it did not start, when
    /// telecom is on ([`Self::set_telecom`]).
    telecom: Option<Result<crate::telecom::Station, String>>,
    /// The station's subscription to this session's runs.
    telecom_subscription: Option<SessionSubscription>,
    /// The queue of `ask_user` questions a frontend answers
    /// ([`Self::set_questions`]).
    questions: Option<crate::questions::QuestionBroker>,
    /// The headless agents this session starts ([`Self::set_spawned`]).
    spawned: Option<crate::spawned::SpawnHost>,
    /// The permission its tools resolve under, when the session was given
    /// one to switch ([`Self::set_permission`]).
    permission: Option<crate::permissions::LivePermission>,
    /// The reminder timer's handle, when `schedules` is on.
    #[cfg(test)]
    schedules: Option<Arc<schedules::Schedules>>,
}

impl std::fmt::Debug for AgentSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentSession")
            .field("agent", &self.agent)
            .finish_non_exhaustive()
    }
}

/// Runs under the session lock as an injected failure happens; what it
/// returns runs before the change goes on.
#[cfg(test)]
pub(crate) type FailHook = Box<dyn FnOnce() -> Box<dyn FnOnce() + Send> + Send>;

#[cfg(test)]
type JobRecordHook = Arc<dyn Fn(&crate::jobs::JobSnapshot) + Send + Sync>;

/// Failures tests inject at the session's write boundary.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestHooks {
    pub after_job_record: Option<JobRecordHook>,
    pub after_job_observation: Option<Box<dyn FnOnce() + Send>>,
    /// Before a recoverable interval closes under the session write lock.
    pub before_run_terminal: Option<Box<dyn FnOnce() + Send>>,
    /// After the interval closes, before its terminal record is appended.
    pub after_run_terminal_admission: Option<Box<dyn FnOnce() + Send>>,
    pub after_run_job_wait: Option<Box<dyn FnOnce() + Send>>,
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
    /// Before a masking batch checks cancellation and takes the session lock.
    pub before_context_masking: Option<Box<dyn FnOnce() + Send>>,
    /// Under the session lock after the masking group is fully synced.
    pub after_context_masking: Option<Box<dyn FnOnce() + Send>>,
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
    /// Runs on the blocking pool before each approval entry's write takes
    /// the session lock, that is before its cancellation admission.
    pub before_approval_admission: Option<ApprovalHook>,
    /// Runs under the session lock once each approval entry's write is
    /// admitted, with the entry admitted, before it is appended; a test can
    /// arm a fault on the manager here.
    pub approval_admitted: Option<ApprovalAdmittedHook>,
    /// Runs on the blocking pool after each approval entry is written and
    /// synced, with the session lock released, before the engine goes on.
    pub approval_written: Option<ApprovalHook>,
}

#[cfg(test)]
type ApprovalHook = Arc<dyn Fn(&crate::permissions::ApprovalEntry) + Send + Sync>;

#[cfg(test)]
type ApprovalAdmittedHook =
    Arc<dyn Fn(&crate::permissions::ApprovalEntry, &mut SessionManager) + Send + Sync>;

impl AgentSession {
    /// The reminder timer's handle, for tests that move its clock.
    #[cfg(test)]
    pub(crate) fn schedules(&self) -> Arc<schedules::Schedules> {
        Arc::clone(self.schedules.as_ref().expect("schedules is on"))
    }

    /// Subscribes to session events. Finished messages are persisted before
    /// observers run. Listeners run synchronously inside the agent's event
    /// dispatch; keep them short. A listener panic is isolated from later
    /// observers and execution; the process panic hook reports it.
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
    /// (`Self::run_change`) and refused after shutdown; a failed write
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

    /// The loaded skills (`Skill`) a `/skill:<name>` prompt expands
    /// ([`crate::skills`]), which the system prompt also lists.
    pub fn skills(&self) -> &[crate::skills::Skill] {
        &self.prompt_options.skills
    }

    /// Sets the prompt templates expanded by [`Self::prompt`].
    pub fn set_prompt_templates(&mut self, prompt_templates: Vec<PromptTemplate>) {
        self.prompt_templates = prompt_templates;
    }

    /// Records the host of the subagents this session's tools start, so a
    /// frontend can list and stop them ([`Self::subagents`]). The host is
    /// built over this session's work gate and jobs, which own its
    /// children: [`Self::shutdown`] stops them.
    pub fn set_subagents(&mut self, host: crate::subagents::SubagentHost) {
        self.subagents = Some(host);
    }

    /// Records the session's telecom station, or why it did not start. A
    /// station follows the session's runs to publish whether it is busy,
    /// and briefs each prompt on the team ([`crate::telecom`]); it is
    /// owned by the session's work gate, so [`Self::shutdown`] stops it and
    /// gives its callsign back.
    pub fn set_telecom(&mut self, telecom: Result<crate::telecom::Station, String>) {
        if let Some(old) = self.telecom_subscription.take() {
            old.unsubscribe();
        }
        if let Ok(station) = &telecom {
            let station = station.clone();
            self.telecom_subscription = Some(self.subscribe(move |event| station.observe(event)));
        }
        self.telecom = Some(telecom);
    }

    /// Records the queue the session's `ask_user` tool asks through; a
    /// frontend connects to it ([`Self::questions`]), and
    /// [`Self::shutdown`] closes it, declining what is still asked.
    pub fn set_questions(&mut self, broker: crate::questions::QuestionBroker) {
        self.questions = Some(broker);
    }

    /// Records the host of the headless agents this session's tools and
    /// frontend start; they are owned by its work gate, so
    /// [`Self::shutdown`] ends them.
    pub fn set_spawned(&mut self, host: crate::spawned::SpawnHost) {
        self.spawned = Some(host);
    }

    /// The headless agents this session starts, when it can.
    pub fn spawned(&self) -> Option<&crate::spawned::SpawnHost> {
        self.spawned.as_ref()
    }

    /// The queue of the person's questions, when the session can ask.
    pub fn questions(&self) -> Option<&crate::questions::QuestionBroker> {
        self.questions.as_ref()
    }

    /// Whether the session started with the tool `name` among its active
    /// tools.
    pub fn tool_active(&self, name: &str) -> bool {
        self.active_tool_names.iter().any(|active| active == name)
    }

    /// The session's telecom station: `None` when telecom is off,
    /// `Some(Err(_))` with why it did not start.
    pub fn telecom(&self) -> Option<Result<&crate::telecom::Station, &str>> {
        self.telecom
            .as_ref()
            .map(|telecom| telecom.as_ref().map_err(String::as_str))
    }

    /// Gives the session the permission its tools resolve under, so
    /// [`Self::set_permission_preset`] can switch it and each prompt states
    /// it as it is then: the `permissions` section and the tools'
    /// escalation offer ([`crate::permissions::refresh_escalation_offer`]).
    /// Without one, both stay as the session was created with.
    pub fn set_permission(&mut self, permission: crate::permissions::LivePermission) {
        self.permission = Some(permission);
    }

    /// The permission the session was given ([`Self::set_permission`]).
    pub fn permission(&self) -> Option<&crate::permissions::LivePermission> {
        self.permission.as_ref()
    }

    /// `/permission`: switches the session
    /// to preset `name`, writing the `bake.permission` entry for what
    /// changes ([`crate::permissions::state::preset_change`]) before the
    /// standing permission changes, so a resumed session keeps the switch.
    /// The next tool call to resolve its policy runs under it; a call
    /// already resolved, running or waiting on an approval, keeps its own.
    /// The next prompt restates the `permissions` section and the tools'
    /// escalation offer. With `persist`, the preset is also saved as
    /// `permission.defaultPreset` for new sessions.
    ///
    /// Serialized and owned as the other changes; an unknown preset changes
    /// nothing, and a failed session write stops the session as
    /// [`Self::set_thinking_level`]'s does.
    pub async fn set_permission_preset(
        &self,
        name: &str,
        persist: bool,
    ) -> Result<DefaultSave, String> {
        let Some(live) = self.permission.clone() else {
            return Err("This session's permission cannot be switched.".into());
        };
        let name = name.to_owned();
        self.run_change(move |control| async move {
            let spec = live.table.resolve(&name)?.clone();
            let change = crate::permissions::state::preset_change(
                &lock(&control.session),
                &live.table,
                crate::permissions::KnobDefaults::default(),
                &name,
            )?;
            if !change.is_empty() {
                let written = control.append(move |session| change.append(session)).await;
                if let Err(error) = written {
                    return Err(control.stop_after_failed_write(&error, "The permission change"));
                }
            }
            live.standing.set(spec.sandbox, spec.approval, name.clone());
            if !persist {
                return Ok(DefaultSave::NotRequested);
            }
            let preset = name.clone();
            let saved = control
                .save(move |store| store.set_default_permission_preset(&preset))
                .await;
            if saved.is_written() {
                let mut settings = write_settings(&control.settings);
                let section = settings
                    .permission
                    .get_or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
                if !section.is_object() {
                    *section = serde_json::Value::Object(serde_json::Map::new());
                }
                if let Some(section) = section.as_object_mut() {
                    section.insert("defaultPreset".into(), name.into());
                }
            }
            Ok(saved)
        })
        .await
    }

    /// The subagents this session starts; `None` when `subagents` is off,
    /// for a subagent itself, and for a session built without the parent
    /// tools.
    pub fn subagents(&self) -> Option<&crate::subagents::SubagentHost> {
        self.subagents.as_ref()
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

    /// Pi's `prompt` for text and images: checks the model and its auth,
    /// compacts first when the last response calls for it, prepends the
    /// system-prompt patch, runs the agent with its retries and
    /// compactions until the session is idle, and reports the first
    /// failure.
    pub async fn prompt(&self, text: &str, images: Vec<ImageContent>) -> Result<(), String> {
        match self
            .prompt_with_control(text, images, RunControl::default())
            .await?
        {
            RunOutcome::FinishedTurn | RunOutcome::Cancelled => Ok(()),
            outcome => Err(outcome.to_string()),
        }
    }

    /// Runs preparation, retries, summaries and queued turns under one
    /// caller-owned allowance. Cancellation awaits the active work. A claimed
    /// run publishes its final outcome only after required persistence.
    /// Dropping the waiter leaves the admitted execution running, including
    /// its preparation, retries, queued turns, and required writes.
    pub async fn prompt_with_control(
        &self,
        text: &str,
        images: Vec<ImageContent>,
        control: RunControl,
    ) -> Result<RunOutcome, String> {
        self.execute_prompt(
            prompt_execution::Input::Prompt {
                text: text.to_owned(),
                images,
            },
            control,
            None,
        )
        .await
        .map(|(outcome, _)| outcome)
    }

    /// Delivers idle completion notices under a fresh interval. Returns
    /// whether a notice turn ran; its owner survives a dropped waiter.
    /// Keeps for the next prompt the notices that could not open a turn
    /// now ([`job_notices::claim_unwaking`]); it never opens one. Held mail,
    /// and a notice that could wake, stays in the registry for a turn.
    pub fn keep_job_notices(&self) {
        let kept = job_notices::claim_unwaking(&self.jobs);
        if !kept.is_empty() {
            self.pending_notices
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend(kept);
        }
    }

    pub async fn deliver_job_notices(&self) -> Result<bool, String> {
        let (outcome, delivered) = self
            .deliver_job_notices_controlled(RunControl::default())
            .await?;
        match outcome {
            RunOutcome::FinishedTurn | RunOutcome::Cancelled => Ok(delivered),
            other => Err(other.to_string()),
        }
    }

    /// Delivers idle completion notices using the caller's original budget
    /// and deadline. No pending notice is a finished turn without a request.
    pub async fn deliver_job_notices_with_control(
        &self,
        control: RunControl,
    ) -> Result<RunOutcome, String> {
        self.deliver_job_notices_controlled(control)
            .await
            .map(|(outcome, _)| outcome)
    }

    pub(crate) async fn deliver_job_notices_controlled(
        &self,
        control: RunControl,
    ) -> Result<(RunOutcome, bool), String> {
        self.execute_prompt(prompt_execution::Input::Notices, control, None)
            .await
    }

    async fn execute_prompt(
        &self,
        input: prompt_execution::Input,
        control: RunControl,
        journal: Option<Arc<run_journal::Journal>>,
    ) -> Result<(RunOutcome, bool), String> {
        let delivery_only = matches!(&input, prompt_execution::Input::Notices);
        if let Some(reason) = self.closed_reason() {
            if delivery_only {
                return Ok((RunOutcome::FinishedTurn, false));
            }
            return Err(reason);
        }
        let mut refusal = None;
        let mut execution = None;
        let agent_streaming = self.agent.is_streaming();
        self.core.activity.send_if_modified(|activity| {
            if activity.context_commits != 0 {
                refusal = Some("Cannot submit a prompt while the session context is being updated. Wait for it to finish and retry.".to_owned());
                false
            } else if activity.manual_compaction {
                refusal = Some("Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry.".to_owned());
                false
            } else if activity.navigation {
                refusal = Some("Cannot submit a prompt while the session tree is being navigated. Wait for it to finish and retry.".to_owned());
                false
            } else if activity.run || agent_streaming {
                refusal = Some("Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.".to_owned());
                false
            } else {
                let claimed = self.executions.lock().unwrap_or_else(PoisonError::into_inner).claim(control.clone(), self.jobs.clone(), Arc::clone(&self.pending_notices), delivery_only, journal.clone());
                match claimed {
                    Ok(claimed) => execution = Some(claimed),
                    Err(error) => {
                        refusal = Some(error);
                        return false;
                    }
                }
                self.core.abort_requested.store(false, Ordering::SeqCst);
                *self.core.run_control.lock().unwrap_or_else(PoisonError::into_inner) = Some(control.clone());
                activity.run = true;
                true
            }
        });
        if let Some(refusal) = refusal {
            if delivery_only {
                return Ok((RunOutcome::FinishedTurn, false));
            }
            return Err(refusal);
        }
        let (execution, completion) =
            execution.ok_or_else(|| "The session did not admit its prompt.".to_owned())?;
        let driver = prompt_execution::PromptExecution::new(self);
        let owned_execution = Arc::clone(&execution);
        let owned_completion = completion.clone();
        let task = {
            // Admission may be called from another executor; the session's
            // runtime owns the driver just as it owns shutdown.
            let _runtime = self.runtime.enter();
            self.work
                .spawn(move |scope| driver.run(owned_execution, owned_completion, scope, input))
        };
        match task {
            Ok(task) => drop(task),
            Err(refused) => {
                let error = refused_message(refused);
                prompt_execution::reject_execution(
                    &self.core,
                    &self.executions,
                    &execution,
                    &completion,
                    error.clone(),
                );
                return Err(error);
            }
        }
        drop(completion);
        let outcome = execution.wait().await?;
        Ok((
            outcome,
            execution.notice_turn_started.load(Ordering::SeqCst),
        ))
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

    /// Waits until no prompt, compaction, or tree navigation runs.
    pub async fn wait_for_idle(&self) {
        self.core.wait_for_idle().await;
    }

    /// Pi's `autoCompactionEnabled`.
    pub fn auto_compaction_enabled(&self) -> bool {
        read_settings(&self.settings).compaction_enabled()
    }

    /// Pi's `autoRetryEnabled`.
    pub fn auto_retry_enabled(&self) -> bool {
        read_settings(&self.settings).retry.enabled
    }
}

#[cfg(test)]
mod approval_tests;
#[cfg(test)]
mod codemode_tests;
#[cfg(test)]
mod control_tests;
#[cfg(test)]
mod foreground_job_tests;
#[cfg(test)]
mod hook_tests;
#[cfg(test)]
mod job_tests;
#[cfg(test)]
mod model_profile_tests;
#[cfg(test)]
mod navigation_summary_tests;
#[cfg(test)]
mod navigation_tests;
#[cfg(test)]
mod nested_call_tests;
mod nested_context;
#[cfg(test)]
mod nested_context_tests;
#[cfg(test)]
mod plugin_tests;
#[cfg(test)]
mod print_result_tests;
#[cfg(test)]
mod recovery_tests;
#[cfg(test)]
mod service_tests;
#[cfg(test)]
mod share_tests;
#[cfg(test)]
pub(crate) mod tests;

/// The failure a test injects into a session write.
#[cfg(test)]
fn injected_failure() -> crate::session::SessionError {
    crate::session::SessionError::Io {
        path: "injected".into(),
        source: std::io::Error::other("injected failure"),
    }
}
