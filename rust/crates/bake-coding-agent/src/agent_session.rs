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
//! throwing listener likewise ends the run. Session writes and model
//! requests hold permits of the session's [`WorkGate`], which
//! [`AgentSession::shutdown`] closes, so none outlives the session.
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
//! Not ported (deferred): extensions and their events (including
//! `session_before_compact`, `session_compact`, and
//! `session_compact_failed`), branch navigation and its summaries, the
//! `/skill:name` slash command list and its expansion error event, prompt
//! templates, MCP, bash `!` commands, the cache warmer, scoped
//! models, virtual models and their failed-response routing, the
//! `entry_appended` event, queued steering and follow-up bookkeeping,
//! tool-loadout changes between turns, and image resizing.

mod recovery;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

pub use recovery::CompactionReason;
use recovery::{Activity, Core, RunState};

use bake_agent::{
    Agent, AgentError, AgentEvent, AgentInitialState, AgentMessage as LiveMessage, AgentOptions,
    AgentTool, PromptInput, Subscription, ToolExecutionMode, hook,
};
use bake_ai::transcript::get_current_system_message;
use bake_ai::{
    ApiRegistry, ImageContent, Message, Model, ModelThinkingLevel, SystemContent, SystemMessage,
    TextContent, UserContent, UserContentBlock, UserMessage,
};

use crate::compaction::CompactionResult;
use crate::model_registry::ModelRegistry;
use crate::model_registry::resolver::{DEFAULT_THINKING_LEVEL, find_initial_model};
use crate::owned_work::WorkGate;
use crate::session::SessionManager;
use crate::session::messages::{CustomMessage, convert_to_llm};
use crate::settings::Settings;
use crate::skills::expand_skill_command;
use crate::system_prompt::{
    SystemPromptOptions, build_system_prompt_sections, diff_system_prompt_sections,
    normalize_prompt_guidelines,
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
    /// The models and their auth.
    pub registry: Arc<ModelRegistry>,
    /// The provider APIs requests stream through.
    pub apis: Arc<ApiRegistry>,
    /// Settings from the Bake home.
    pub settings: Settings,
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
}

type Listener = Arc<dyn Fn(&SessionEvent<'_>) + Send + Sync>;

#[derive(Default)]
struct Listeners {
    next: AtomicU64,
    list: Mutex<Vec<(u64, Listener)>>,
    /// Set by [`AgentSession::shutdown`]: later events reach no listener.
    closed: AtomicBool,
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

    fn emit(&self, event: &SessionEvent<'_>) {
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

fn lock(session: &SharedSession) -> MutexGuard<'_, SessionManager> {
    session.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs `work` on the session off the Tokio workers.
async fn with_session<R: Send + 'static>(
    session: &SharedSession,
    work: impl FnOnce(&mut SessionManager) -> R + Send + 'static,
) -> Result<R, String> {
    let session = Arc::clone(session);
    tokio::task::spawn_blocking(move || work(&mut lock(&session)))
        .await
        .map_err(|error| format!("Session I/O task failed: {error}"))
}

/// Pi's `AgentSession`, headless.
pub struct AgentSession {
    agent: Arc<Agent>,
    core: Arc<Core>,
    work: WorkGate,
    session: SharedSession,
    registry: Arc<ModelRegistry>,
    settings: Settings,
    tools: Vec<Arc<AgentTool>>,
    active_tool_names: Vec<String>,
    prompt_options: SystemPromptOptions,
    docs_command: String,
    listeners: Arc<Listeners>,
    persist_error: Arc<Mutex<Option<String>>>,
    subscription: Mutex<Option<Subscription>>,
    model_fallback_message: Option<String>,
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

/// Pi's `prepareNextTurnWithContext` hook: threshold compaction between
/// turns.
fn next_turn_hook(core: Arc<Core>) -> bake_agent::agent::AgentPrepareNextTurnWithContext {
    Arc::new(move |turn, signal| {
        let tools = turn.context.tools.clone();
        let core = Arc::clone(&core);
        Box::pin(async move { core.compact_before_next_response(tools, signal).await })
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
    // After a failed write, later messages would leave a gap.
    if persist_error
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_some()
    {
        return Ok(None);
    }
    let Some(permit) = core.work.enter() else {
        return Ok(None);
    };
    let message = message.clone();
    let session = Arc::clone(&core.session);
    // The permit lives in the blocking work, which keeps running if this
    // run is cancelled.
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        persist(&mut lock(&session), &message)
    })
    .await
    .unwrap_or_else(|error| Err(format!("Session I/O task failed: {error}")));
    result.map_err(|error| {
        persist_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert(error);
    })
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

impl AgentSession {
    /// Pi's `createAgentSession`: selects the model and thinking level,
    /// restores the transcript, records a new session's initial model and
    /// thinking level, and creates the agent. Must run inside a Tokio
    /// runtime.
    pub async fn create(options: AgentSessionOptions) -> Result<Self, String> {
        let AgentSessionOptions {
            session,
            registry,
            apis,
            settings,
            model,
            thinking_level,
            tools,
            active_tool_names,
            tool_prompts,
            mut system_prompt,
            docs_command,
        } = options;
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
        let session: SharedSession = Arc::new(Mutex::new(session));
        let (context, has_thinking_entry) = with_session(&session, |session| {
            let context = session.build_session_context();
            let has_thinking_entry = session
                .branch_entries(None)
                .iter()
                .any(|entry| entry.entry_type() == "thinking_level_change");
            (context, has_thinking_entry)
        })
        .await?;
        let has_existing_session = !context.messages.is_empty();
        let selection = select_model(
            SelectionInputs {
                registry: &registry,
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
        let work = WorkGate::default();
        let stream_fn = registry.stream_fn_owned(Arc::clone(&apis), Some(work.clone()));
        let listeners = Arc::new(Listeners::default());
        let core = Arc::new(Core {
            session: Arc::clone(&session),
            settings: settings.clone(),
            listeners: Arc::clone(&listeners),
            stream_fn: Arc::clone(&stream_fn),
            work: work.clone(),
            agent: OnceLock::new(),
            has_model: AtomicBool::new(selection.model.is_some()),
            abort_requested: AtomicBool::new(false),
            state: Mutex::new(RunState::default()),
            activity: tokio::sync::watch::Sender::new(Activity::default()),
            #[cfg(test)]
            session_io: tokio::sync::watch::Sender::new(0),
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
            stream_fn: Some(stream_fn),
            prepare_next_turn_with_context: Some(next_turn_hook(Arc::clone(&core))),
            steering_mode: settings.steering_mode,
            follow_up_mode: settings.follow_up_mode,
            session_id: Some(session_id),
            ..AgentOptions::default()
        })
        .map_err(|error| error.to_string())?;
        let agent = Arc::new(agent);
        let _ = core.agent.set(Arc::downgrade(&agent));

        let model = selection.model.clone();
        let thinking = selection.thinking_level;
        with_session(&session, move |session| {
            if has_existing_session {
                if !has_thinking_entry {
                    session.append_thinking_level_change(thinking.as_str())?;
                }
            } else {
                if let Some(model) = &model {
                    session.append_model_change(&model.provider, &model.id)?;
                }
                session.append_thinking_level_change(thinking.as_str())?;
            }
            Ok::<(), crate::session::SessionError>(())
        })
        .await?
        .map_err(|error| error.to_string())?;

        let active_tool_names: Vec<String> = active_tool_names
            .into_iter()
            .filter(|name| tools.iter().any(|tool| &tool.name == name))
            .collect();
        let persist_error = Arc::new(Mutex::new(None));
        let created = Self {
            agent,
            core,
            work,
            session,
            registry,
            settings,
            tools,
            active_tool_names,
            prompt_options: system_prompt,
            docs_command,
            listeners,
            persist_error,
            subscription: Mutex::new(None),
            model_fallback_message: selection.fallback_message,
        };
        created.connect();
        Ok(created)
    }

    /// Subscribes the session to its agent: listeners first, then
    /// persistence, as Pi's `_handleAgentEvent` orders them.
    fn connect(&self) {
        let core = Arc::clone(&self.core);
        let persist_error = Arc::clone(&self.persist_error);
        // Weak: the agent owns this listener.
        let agent = Arc::downgrade(&self.agent);
        let subscription = self.agent.subscribe(move |event, _signal| {
            let core = Arc::clone(&core);
            let persist_error = Arc::clone(&persist_error);
            let agent = agent.clone();
            Box::pin(async move {
                if let AgentEvent::MessageStart { message } = event
                    && message.role() == "user"
                {
                    core.state().overflow_recovery_attempted = false;
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
                        if entry_id.is_err()
                            && let Some(agent) = agent.upgrade()
                        {
                            agent.abort();
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

    /// The session id.
    pub fn session_id(&self) -> String {
        lock(&self.session).session_id().to_owned()
    }

    /// The transcript.
    pub fn messages(&self) -> Vec<LiveMessage> {
        self.agent.messages()
    }

    /// Pi's `setThinkingLevel`: clamps to the model, and records and
    /// announces a change.
    pub async fn set_thinking_level(&self, level: ModelThinkingLevel) -> Result<(), String> {
        let effective = match self.model() {
            Some(model) => bake_ai::models::clamp_thinking_level(&model, level),
            None => level,
        };
        let previous = self.agent.thinking_level();
        self.agent.set_thinking_level(effective);
        if effective != previous {
            with_session(&self.session, move |session| {
                session.append_thinking_level_change(effective.as_str())
            })
            .await?
            .map_err(|error| error.to_string())?;
            self.listeners
                .emit(&SessionEvent::ThinkingLevelChanged(effective));
        }
        Ok(())
    }

    /// Pi's `setModel` without persisting the default: refuses a model
    /// without auth, records the change, then applies the thinking level
    /// the model's settings choose.
    pub async fn set_model(&self, model: Model) -> Result<(), String> {
        if self.registry.check_auth(&model.provider).is_none() {
            return Err(format!("No API key for {}/{}", model.provider, model.id));
        }
        let level = self
            .settings
            .model_thinking_level(&model.provider, &model.id)
            .or(self.settings.default_thinking_level)
            .unwrap_or_else(|| self.agent.thinking_level());
        self.agent.set_model(model.clone());
        self.core.has_model.store(true, Ordering::SeqCst);
        with_session(&self.session, move |session| {
            session.append_model_change(&model.provider, &model.id)
        })
        .await?
        .map_err(|error| error.to_string())?;
        self.set_thinking_level(level).await
    }

    /// The system message patching the sections the transcript replays,
    /// Pi's `_preparePromptAndToolLoadout`; it also activates the tools.
    fn prepare_prompt_and_tools(&self) -> Result<Option<SystemMessage>, String> {
        let active: Vec<Arc<AgentTool>> = self
            .active_tool_names
            .iter()
            .filter_map(|name| self.tools.iter().find(|tool| &tool.name == name))
            .cloned()
            .collect();
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
        let Some(model) = self.model() else {
            return Err(format_no_model_selected_message(docs_command));
        };
        if !self.registry.has_configured_auth(&model.provider)
            && self.registry.check_auth(&model.provider).is_none()
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
        // Pi's `_expandSkillCommand`; the skill file is read off the runtime.
        let text = if text.starts_with("/skill:") {
            let skills = self.prompt_options.skills.clone();
            let command = text.to_owned();
            tokio::task::spawn_blocking(move || expand_skill_command(&command, &skills))
                .await
                .unwrap_or_else(|_| text.to_owned())
        } else {
            text.to_owned()
        };
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
        match self
            .persist_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
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
        self.core
            .manual_compaction(custom_instructions, no_model)
            .await
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

    /// Pi's `isCompacting`.
    pub fn is_compacting(&self) -> bool {
        let activity = *self.core.activity.borrow();
        activity.manual_compaction || activity.auto_compaction
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
        self.settings.compaction_enabled()
    }

    /// Pi's `autoRetryEnabled`.
    pub fn auto_retry_enabled(&self) -> bool {
        self.settings.retry.enabled
    }

    /// Pi's `abort`: signals the run, cancels a retry wait and a
    /// compaction, and waits until the session is idle.
    pub async fn abort(&self) {
        if self.core.activity.borrow().run {
            self.core.abort_requested.store(true, Ordering::SeqCst);
        }
        self.core.abort_retry_and_compaction();
        self.agent.abort();
        self.agent.wait_for_idle().await;
        self.core.wait_for_idle().await;
    }

    /// Pi's `dispose`, awaited: aborts and cancels the run, a retry wait,
    /// and a compaction, removes every listener so the cancelled work
    /// reports nothing, waits for the run's task, its session writes, and
    /// its model requests, disconnects from the agent, and closes the session
    /// file for writing, releasing its lock. Later prompts fail and emit no
    /// events, and a later model or thinking-level change is not written.
    pub async fn shutdown(&self) {
        // Closed before anything is cancelled: a cancelled retry wait or
        // compaction resumes on another worker at once, and Pi's
        // synchronous `dispose` clears its listeners before any of that
        // work's continuations run.
        self.listeners.close();
        self.core.abort_requested.store(true, Ordering::SeqCst);
        self.core.abort_retry_and_compaction();
        self.agent.shutdown().await;
        self.work.close().await;
        if let Some(subscription) = self
            .subscription
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            subscription.unsubscribe();
        }
        // Nothing writes the session any more: another writer may take
        // its file over, and a late change stays in memory.
        lock(&self.session).close_writer();
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
mod recovery_tests;
#[cfg(test)]
mod tests;
