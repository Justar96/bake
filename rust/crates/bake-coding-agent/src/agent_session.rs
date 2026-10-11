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
//! does.
//!
//! Session I/O is blocking and runs on Tokio's blocking pool; message
//! persistence happens inside the agent's event listener, so a run is idle
//! only after its messages are written. A failed write aborts the run and
//! no later message is written, so the file never skips a message; Pi's
//! throwing listener likewise ends the run. Session writes and model
//! requests hold permits of the session's [`WorkGate`], which
//! [`AgentSession::shutdown`] closes, so none outlives the session.
//!
//! Not ported (deferred): compaction, auto-retry, extensions and their
//! events, skills, prompt templates, MCP, bash `!` commands, the cache
//! warmer, scoped models, virtual models, queued steering and follow-up
//! bookkeeping, tool-loadout changes between turns, and image resizing.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use bake_agent::{
    Agent, AgentError, AgentEvent, AgentInitialState, AgentMessage as LiveMessage, AgentOptions,
    AgentTool, PromptInput, Subscription, hook,
};
use bake_ai::transcript::get_current_system_message;
use bake_ai::{
    ApiRegistry, ImageContent, Message, Model, ModelThinkingLevel, SystemContent, SystemMessage,
    TextContent, UserContent, UserContentBlock, UserMessage,
};

use crate::model_registry::ModelRegistry;
use crate::model_registry::resolver::{DEFAULT_THINKING_LEVEL, find_initial_model};
use crate::owned_work::WorkGate;
use crate::session::SessionManager;
use crate::session::messages::{CustomMessage, convert_to_llm};
use crate::settings::Settings;
use crate::system_prompt::{
    SystemPromptOptions, build_system_prompt_sections, diff_system_prompt_sections,
    normalize_prompt_guidelines, normalize_prompt_snippet,
};

/// Pi's `getProviderLoginHelp`, naming the docs directory Bake's prompt uses.
fn provider_login_help(docs: &str) -> String {
    let join = |name: &str| {
        PathBuf::from(docs)
            .join(name)
            .to_string_lossy()
            .into_owned()
    };
    format!(
        "Use /login to log into a provider via OAuth or API key. See:\n  {}\n  {}",
        join("providers.md"),
        join("models.md")
    )
}

/// Pi's `formatNoModelsAvailableMessage`.
pub fn format_no_models_available_message(docs: &str) -> String {
    format!("No models available. {}", provider_login_help(docs))
}

/// Pi's `formatNoModelSelectedMessage`.
pub fn format_no_model_selected_message(docs: &str) -> String {
    format!(
        "No model selected.\n\n{}\n\nThen use /model to select a model.",
        provider_login_help(docs)
    )
}

/// Pi's `formatNoApiKeyFoundMessage`.
pub fn format_no_api_key_found_message(provider: &str, docs: &str) -> String {
    let display = if provider == "unknown" {
        "the selected model"
    } else {
        provider
    };
    format!(
        "No API key found for {display}.\n\n{}",
        provider_login_help(docs)
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
    /// The active tools by name, Pi's `initialActiveToolNames`: the prompt
    /// lists these, and requests declare the ones present in `tools`.
    pub active_tool_names: Vec<String>,
    /// What the prompt says about each tool, Pi's tool-definition
    /// `promptSnippet` and `promptGuidelines`. A tool without a snippet is
    /// not listed under the prompt's available tools, as in Pi.
    pub tool_prompts: Vec<ToolPrompt>,
    /// The system-prompt inputs; `selected_tools` is replaced by the active
    /// tools, and [`Self::tool_prompts`] are added to `tool_snippets` and
    /// `tool_guidelines`, winning over entries for the same tool.
    pub system_prompt: SystemPromptOptions,
}

/// The prompt text of one tool, from Pi's `ToolDefinition`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolPrompt {
    /// The tool name.
    pub name: String,
    /// Pi's `promptSnippet`: the tool's line under the available tools,
    /// collapsed to one line.
    pub snippet: Option<String>,
    /// Pi's `promptGuidelines`: bullets added to the guidelines, trimmed
    /// and deduplicated.
    pub guidelines: Vec<String>,
}

/// An event of the session, Pi's `AgentSessionEvent` without the deferred
/// features' events.
#[derive(Debug)]
pub enum SessionEvent<'a> {
    /// An agent event; Pi's `agent_end` gains `willRetry`, always false
    /// without auto-retry.
    Agent(&'a AgentEvent),
    /// A prompt finished, Pi's `agent_settled`.
    AgentSettled {
        /// Whether it was aborted.
        aborted: bool,
    },
    /// The thinking level changed, Pi's `thinking_level_changed`.
    ThinkingLevelChanged(ModelThinkingLevel),
}

type Listener = Arc<dyn Fn(&SessionEvent<'_>) + Send + Sync>;

#[derive(Default)]
struct Listeners {
    next: AtomicU64,
    list: Mutex<Vec<(u64, Listener)>>,
}

impl Listeners {
    fn emit(&self, event: &SessionEvent<'_>) {
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
    work: WorkGate,
    session: SharedSession,
    registry: Arc<ModelRegistry>,
    settings: Settings,
    tools: Vec<Arc<AgentTool>>,
    active_tool_names: Vec<String>,
    prompt_options: SystemPromptOptions,
    listeners: Arc<Listeners>,
    persist_error: Arc<Mutex<Option<String>>>,
    abort_requested: AtomicBool,
    has_model: AtomicBool,
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
    docs: &'a str,
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
        docs,
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
            (None, _) => Some(format_no_models_available_message(docs)),
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
fn persist(session: &mut SessionManager, message: &LiveMessage) -> Result<(), String> {
    let result = match message {
        LiveMessage::Llm(llm) => session.append_message(llm.clone()).map(drop),
        LiveMessage::Custom(custom) => match custom.as_any().downcast_ref::<CustomMessage>() {
            Some(custom) => session
                .append_custom_message_entry(
                    &custom.custom_type,
                    custom.content.clone(),
                    custom.display,
                    custom.details.clone(),
                )
                .map(drop),
            None => Ok(()),
        },
    };
    result.map_err(|error| error.to_string())
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
        } = options;
        // Pi's `_toolPromptSnippets` and `_toolPromptGuidelines`.
        for prompt in &tool_prompts {
            if let Some(snippet) = normalize_prompt_snippet(prompt.snippet.as_deref()) {
                system_prompt
                    .tool_snippets
                    .push((prompt.name.clone(), snippet));
            }
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
                docs: &system_prompt.docs.docs,
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
            stream_fn: Some(registry.stream_fn_owned(Arc::clone(&apis), Some(work.clone()))),
            steering_mode: settings.steering_mode,
            follow_up_mode: settings.follow_up_mode,
            session_id: Some(session_id),
            ..AgentOptions::default()
        })
        .map_err(|error| error.to_string())?;
        let agent = Arc::new(agent);

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
        let listeners = Arc::new(Listeners::default());
        let persist_error = Arc::new(Mutex::new(None));
        let created = Self {
            agent,
            work,
            session,
            registry,
            settings,
            tools,
            active_tool_names,
            prompt_options: system_prompt,
            has_model: AtomicBool::new(selection.model.is_some()),
            listeners,
            persist_error,
            abort_requested: AtomicBool::new(false),
            subscription: Mutex::new(None),
            model_fallback_message: selection.fallback_message,
        };
        created.connect();
        Ok(created)
    }

    /// Subscribes the session to its agent: listeners first, then
    /// persistence, as Pi's `_handleAgentEvent` orders them.
    fn connect(&self) {
        let listeners = Arc::clone(&self.listeners);
        let session = Arc::clone(&self.session);
        let persist_error = Arc::clone(&self.persist_error);
        let work = self.work.clone();
        // Weak: the agent owns this listener.
        let agent = Arc::downgrade(&self.agent);
        let subscription = self.agent.subscribe(move |event, _signal| {
            let listeners = Arc::clone(&listeners);
            let session = Arc::clone(&session);
            let persist_error = Arc::clone(&persist_error);
            let work = work.clone();
            let agent = agent.clone();
            Box::pin(async move {
                listeners.emit(&SessionEvent::Agent(event));
                let AgentEvent::MessageEnd { message } = event else {
                    return;
                };
                // After a failed write, later messages would leave a gap.
                if persist_error
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .is_some()
                {
                    return;
                }
                // Closed at shutdown: Pi's `dispose` disconnects without
                // persisting the aborted message.
                let Some(permit) = work.enter() else {
                    return;
                };
                let message = message.clone();
                let session = Arc::clone(&session);
                // The permit lives in the blocking work, which keeps
                // running if this run is cancelled.
                let result = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    persist(&mut lock(&session), &message)
                })
                .await
                .unwrap_or_else(|error| Err(format!("Session I/O task failed: {error}")));
                if let Err(error) = result {
                    persist_error
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .get_or_insert(error);
                    if let Some(agent) = agent.upgrade() {
                        agent.abort();
                    }
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
        self.has_model
            .load(Ordering::SeqCst)
            .then(|| self.agent.model())
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
        self.has_model.store(true, Ordering::SeqCst);
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
    /// prepends the system-prompt patch, runs the agent until it is idle,
    /// and reports the first persistence failure.
    pub async fn prompt(&self, text: &str, images: Vec<ImageContent>) -> Result<(), String> {
        // Pi queues with a `streamingBehavior`; queueing is not ported.
        if self.agent.is_streaming() {
            return Err("Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.".into());
        }
        let docs = &self.prompt_options.docs.docs;
        let Some(model) = self.model() else {
            return Err(format_no_model_selected_message(docs));
        };
        if !self.registry.has_configured_auth(&model.provider)
            && self.registry.check_auth(&model.provider).is_none()
        {
            return Err(format_no_api_key_found_message(&model.provider, docs));
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
        self.abort_requested.store(false, Ordering::SeqCst);
        let result = self.agent.prompt(PromptInput::Messages(messages)).await;
        let aborted = self.abort_requested.load(Ordering::SeqCst);
        self.listeners.emit(&SessionEvent::AgentSettled { aborted });
        result.map_err(|error: AgentError| error.to_string())?;
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

    /// Pi's `abort`: signals the run and waits until it is idle.
    pub async fn abort(&self) {
        self.abort_requested.store(true, Ordering::SeqCst);
        self.agent.abort();
        self.agent.wait_for_idle().await;
    }

    /// Pi's `dispose`, awaited: aborts and cancels the run, waits for its
    /// task, its session writes, and its model requests, and disconnects
    /// from the agent. Later prompts fail.
    pub async fn shutdown(&self) {
        self.abort_requested.store(true, Ordering::SeqCst);
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
    }
}

#[cfg(test)]
mod tests;
