//! The stateful agent: transcript, queues, runs, and listeners.
//!
//! Ported from Pi `packages/agent/src/agent.ts` (v1.1.0). An [`Agent`] owns
//! the transcript, runs one prompt or continuation at a time through
//! [`crate::agent_loop`](mod@crate::agent_loop), reduces the loop's events into its state, and
//! awaits its listeners in subscription order.
//!
//! Each run executes on a Tokio task that the agent owns. `prompt` and
//! `continue_run` start it and wait until the run is idle: every event has
//! been processed and every listener for `agent_end` has settled. Dropping
//! those futures does not stop the run; [`Agent::wait_for_idle`] still
//! observes it. Ownership rules:
//!
//! - [`Agent::abort`] signals the run, as Pi's `abort()` does; the provider
//!   and tools are expected to honor the signal.
//! - [`Agent::shutdown`] also cancels the run's task and waits until it is
//!   gone; afterwards no hook or listener runs and new runs are refused.
//! - Dropping the agent does the same without waiting: a listener or hook
//!   already running finishes its current step, and none starts after.
//! - The agent is idle only after the run's task finished its last step, so
//!   no listener or hook runs once [`Agent::wait_for_idle`] returns.
//! - The provider observers in [`AgentOptions`] (`on_payload`,
//!   `on_response`, `on_provider_stream_event`) may be called from a task
//!   the provider spawns. The agent admits them only while their run is
//!   live, and the run ends only after a call already running has returned,
//!   so the two guarantees above cover them too.
//! - Listeners and hooks should hold the agent through a [`Weak`]
//!   reference, as in `agent.abort()` from a listener: an `Arc<Agent>`
//!   inside one is a cycle, so the agent is never dropped.
//!
//! Pi lets several tools that run in parallel emit at once; here the agent
//! processes one event at a time, so listeners never overlap.

use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, Weak};

use bake_ai::options::{OnPayload, OnProviderStreamEvent, OnResponse};
use bake_ai::transcript::{
    create_initial_system_message, get_current_system_message, get_current_system_prompt,
};
use bake_ai::{
    AbortController, AbortSignal, AssistantContentBlock, AssistantMessage, ImageContent, Message,
    Model, ModelCost, ModelThinkingLevel, SimpleStreamOptions, StopReason, StreamOptions,
    TextContent, ThinkingBudgets, Tool, Usage, UserContent, UserContentBlock, UserMessage,
};
use tokio::sync::watch;

use crate::agent_loop::{catching, run_agent_loop, run_agent_loop_continue};
use crate::hook;
use crate::stream_fn::default_stream_fn;
use crate::types::{
    AfterToolCall, AgentContext, AgentEvent, AgentEventSink, AgentLoopConfig, AgentLoopTurnUpdate,
    AgentMessage, AgentState, AgentTool, AgentTurnContext, BeforeToolCall, BoxFuture, ConvertToLlm,
    FinishTurn, GetApiKey, PrepareNextTurn, PrepareRequest, QueueMode, StreamFn, ToolExecutionMode,
    TransformContext,
};

/// Why an agent call was refused; the messages are Pi's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    /// `prompt` while a run is active.
    AlreadyProcessingPrompt,
    /// `continue_run` while a run is active.
    AlreadyProcessingContinue,
    /// `reset` while a run is active.
    AlreadyProcessingReset,
    /// `continue_run` with an empty or system-only transcript.
    NoMessagesToContinue,
    /// `continue_run` from an assistant message with nothing queued.
    ContinueFromAssistant,
    /// No stream function was passed or installed.
    NoStreamFn,
    /// A run was started outside a Tokio runtime.
    NoRuntime,
    /// The agent was shut down.
    Closed,
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AlreadyProcessingPrompt => {
                "Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion."
            }
            Self::AlreadyProcessingContinue => {
                "Agent is already processing. Wait for completion before continuing."
            }
            Self::AlreadyProcessingReset => {
                "Agent is already processing. Wait for completion before resetting."
            }
            Self::NoMessagesToContinue => "No messages to continue from",
            Self::ContinueFromAssistant => "Cannot continue from message role: assistant",
            Self::NoStreamFn => crate::stream_fn::NO_DEFAULT_STREAM_FN,
            Self::NoRuntime => "The agent needs a Tokio runtime to run",
            Self::Closed => "The agent is shut down",
        })
    }
}

impl std::error::Error for AgentError {}

/// Initial state of an [`Agent`], Pi's `AgentInitialState`. `system_prompt`
/// and `tools` become the leading system message unless `messages` already
/// starts with one.
#[derive(Debug, Clone, Default)]
pub struct AgentInitialState {
    /// The system prompt.
    pub system_prompt: Option<String>,
    /// The model; Pi's `unknown` placeholder when `None`.
    pub model: Option<Model>,
    /// The thinking level; `off` when `None`.
    pub thinking_level: Option<ModelThinkingLevel>,
    /// Executable tools.
    pub tools: Vec<Arc<AgentTool>>,
    /// The transcript.
    pub messages: Vec<AgentMessage>,
}

/// An event listener; it receives the run's abort signal.
pub type AgentListener =
    Arc<dyn for<'a> Fn(&'a AgentEvent, &'a AbortSignal) -> BoxFuture<'a, ()> + Send + Sync>;

/// Pi's legacy `prepareNextTurn(signal)` agent option.
pub type AgentPrepareNextTurn = Arc<
    dyn Fn(Option<AbortSignal>) -> BoxFuture<'static, Option<AgentLoopTurnUpdate>> + Send + Sync,
>;

/// Pi's `prepareNextTurnWithContext(context, signal)` agent option.
pub type AgentPrepareNextTurnWithContext = Arc<
    dyn for<'a> Fn(
            AgentTurnContext<'a>,
            Option<AbortSignal>,
        ) -> BoxFuture<'a, Option<AgentLoopTurnUpdate>>
        + Send
        + Sync,
>;

/// Options of [`Agent::new`], Pi's `AgentOptions`. Pi's `transport` is not
/// ported: `bake-ai` streams over HTTP only.
#[derive(Clone, Default)]
pub struct AgentOptions {
    /// The initial state.
    pub initial_state: AgentInitialState,
    /// Converts the transcript for the model; by default LLM messages pass
    /// and custom messages are dropped.
    pub convert_to_llm: Option<ConvertToLlm>,
    /// Rewrites the transcript before conversion.
    pub transform_context: Option<TransformContext>,
    /// Starts provider requests; the installed default when `None`.
    pub stream_fn: Option<StreamFn>,
    /// Resolves the API key per request.
    pub get_api_key: Option<GetApiKey>,
    /// Replaces request payloads.
    ///
    /// This and the two observers below run only while their run is live,
    /// and the run waits for a running call before it reports idle, holding
    /// a Tokio worker meanwhile: keep them short, and never block on the
    /// agent from one.
    pub on_payload: Option<OnPayload>,
    /// Observes responses.
    pub on_response: Option<OnResponse>,
    /// Observes provider stream events.
    pub on_provider_stream_event: Option<OnProviderStreamEvent>,
    /// Runs before each tool.
    pub before_tool_call: Option<BeforeToolCall>,
    /// Runs after each tool.
    pub after_tool_call: Option<AfterToolCall>,
    /// Runs after each turn, before `turn_end`.
    pub finish_turn: Option<FinishTurn>,
    /// Runs before each provider request.
    pub prepare_request: Option<PrepareRequest>,
    /// Runs before each later turn with the run's signal only.
    pub prepare_next_turn: Option<AgentPrepareNextTurn>,
    /// Runs before each later turn with the completed turn; preferred over
    /// `prepare_next_turn`.
    pub prepare_next_turn_with_context: Option<AgentPrepareNextTurnWithContext>,
    /// How steering drains; one at a time by default.
    pub steering_mode: QueueMode,
    /// How follow-ups drain; one at a time by default.
    pub follow_up_mode: QueueMode,
    /// Session id forwarded to providers.
    pub session_id: Option<String>,
    /// Per-level thinking budgets forwarded to providers.
    pub thinking_budgets: Option<ThinkingBudgets>,
    /// Cap of provider-requested retry delays.
    pub max_retry_delay_ms: Option<u64>,
    /// Tool execution mode; parallel by default.
    pub tool_execution: ToolExecutionMode,
}

/// What to prompt with, Pi's `prompt(input, images)` overloads.
#[derive(Debug, Clone)]
pub enum PromptInput {
    /// Text, with images after it, as one user message.
    Text {
        /// The text.
        text: String,
        /// Images after the text.
        images: Vec<ImageContent>,
    },
    /// Messages as given.
    Messages(Vec<AgentMessage>),
}

impl PromptInput {
    /// Text with images.
    pub fn with_images(text: impl Into<String>, images: Vec<ImageContent>) -> Self {
        Self::Text {
            text: text.into(),
            images,
        }
    }

    fn into_messages(self) -> Vec<AgentMessage> {
        match self {
            Self::Messages(messages) => messages,
            Self::Text { text, images } => {
                let mut content = vec![UserContentBlock::Text(TextContent::new(text))];
                content.extend(images.into_iter().map(UserContentBlock::Image));
                vec![
                    UserMessage {
                        content: UserContent::Blocks(content),
                        timestamp: bake_ai::now_ms(),
                    }
                    .into(),
                ]
            }
        }
    }
}

impl From<&str> for PromptInput {
    fn from(text: &str) -> Self {
        Self::with_images(text, Vec::new())
    }
}

impl From<String> for PromptInput {
    fn from(text: String) -> Self {
        Self::with_images(text, Vec::new())
    }
}

impl From<AgentMessage> for PromptInput {
    fn from(message: AgentMessage) -> Self {
        Self::Messages(vec![message])
    }
}

impl From<UserMessage> for PromptInput {
    fn from(message: UserMessage) -> Self {
        Self::Messages(vec![message.into()])
    }
}

impl From<Vec<AgentMessage>> for PromptInput {
    fn from(messages: Vec<AgentMessage>) -> Self {
        Self::Messages(messages)
    }
}

/// Pi's `PendingMessageQueue`.
#[derive(Debug, Default)]
struct PendingMessageQueue {
    messages: VecDeque<AgentMessage>,
    mode: QueueMode,
}

impl PendingMessageQueue {
    fn new(mode: QueueMode) -> Self {
        Self {
            messages: VecDeque::new(),
            mode,
        }
    }

    fn peek(&self) -> Vec<AgentMessage> {
        match self.mode {
            QueueMode::All => self.messages.iter().cloned().collect(),
            QueueMode::OneAtATime => self.messages.front().cloned().into_iter().collect(),
        }
    }

    fn drain(&mut self) -> Vec<AgentMessage> {
        match self.mode {
            QueueMode::All => self.messages.drain(..).collect(),
            QueueMode::OneAtATime => self.messages.pop_front().into_iter().collect(),
        }
    }
}

/// Pi's `DEFAULT_MODEL` placeholder.
fn default_model() -> Model {
    Model {
        id: "unknown".to_owned(),
        name: "unknown".to_owned(),
        api: "unknown".to_owned(),
        provider: "unknown".to_owned(),
        base_url: String::new(),
        input: Vec::new(),
        input_limits: None,
        cost: ModelCost::default(),
        headers: None,
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 0,
        max_tokens: 0,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    }
}

fn default_convert_to_llm() -> ConvertToLlm {
    hook::convert_to_llm(|messages| {
        let converted: Vec<Message> = messages
            .iter()
            .filter_map(|m| m.as_llm().cloned())
            .collect();
        Box::pin(async move { converted })
    })
}

fn system_messages(messages: &[AgentMessage]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(|message| message.as_system().cloned().map(Message::System))
        .collect()
}

struct ActiveRun {
    controller: AbortController,
    idle: watch::Receiver<bool>,
    task: Option<tokio::task::AbortHandle>,
    observers: Arc<ObserverGate>,
}

/// Admits the run's provider observers (`on_payload`, `on_response`, and
/// `on_provider_stream_event`) only while the run is live.
///
/// A provider may call them from a task the agent does not own, such as the
/// one `bake-ai` spawns per stream. Each call holds a read lock; closing
/// takes the write lock, so once [`ObserverGate::close`] returns no call is
/// running and none starts.
struct ObserverGate {
    live: AtomicBool,
    running: RwLock<()>,
}

impl ObserverGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            live: AtomicBool::new(true),
            running: RwLock::new(()),
        })
    }

    /// Runs `observe` unless the run has ended.
    fn enter<R>(&self, observe: impl FnOnce() -> R) -> Option<R> {
        let _running = self.running.read().unwrap_or_else(PoisonError::into_inner);
        self.live.load(Ordering::SeqCst).then(observe)
    }

    /// Refuses later calls and waits for a running one to return.
    ///
    /// The wait is a blocking lock taken on the run task as it ends, so it
    /// holds that Tokio worker thread for as long as the running observer
    /// takes. That suits `bake-ai`'s observers, which are synchronous and
    /// short; an observer must not block or wait on the agent.
    fn close(&self) {
        self.close_without_waiting();
        drop(self.running.write().unwrap_or_else(PoisonError::into_inner));
    }

    /// Refuses later calls; a running one may still finish.
    fn close_without_waiting(&self) {
        self.live.store(false, Ordering::SeqCst);
    }

    fn on_payload(self: &Arc<Self>, observer: Option<&OnPayload>) -> Option<OnPayload> {
        let observer = Arc::clone(observer?);
        let gate = Arc::clone(self);
        Some(Arc::new(move |payload, model| {
            gate.enter(|| observer(payload, model)).flatten()
        }))
    }

    fn on_response(self: &Arc<Self>, observer: Option<&OnResponse>) -> Option<OnResponse> {
        let observer = Arc::clone(observer?);
        let gate = Arc::clone(self);
        Some(Arc::new(move |response, model| {
            gate.enter(|| observer(response, model));
        }))
    }

    fn on_provider_stream_event(
        self: &Arc<Self>,
        observer: Option<&OnProviderStreamEvent>,
    ) -> Option<OnProviderStreamEvent> {
        let observer = Arc::clone(observer?);
        let gate = Arc::clone(self);
        Some(Arc::new(move |event, model| {
            gate.enter(|| observer(event, model));
        }))
    }
}

struct Shared {
    model: Model,
    thinking_level: ModelThinkingLevel,
    tools: Vec<Arc<AgentTool>>,
    messages: Vec<AgentMessage>,
    is_streaming: bool,
    streaming_message: Option<AgentMessage>,
    pending_tool_calls: BTreeSet<String>,
    error_message: Option<String>,
    steering: PendingMessageQueue,
    follow_up: PendingMessageQueue,
    session_id: Option<String>,
    thinking_budgets: Option<ThinkingBudgets>,
    max_retry_delay_ms: Option<u64>,
    tool_execution: ToolExecutionMode,
    active: Option<ActiveRun>,
    closed: bool,
}

struct Hooks {
    convert_to_llm: ConvertToLlm,
    transform_context: Option<TransformContext>,
    stream_fn: StreamFn,
    get_api_key: Option<GetApiKey>,
    on_payload: Option<OnPayload>,
    on_response: Option<OnResponse>,
    on_provider_stream_event: Option<OnProviderStreamEvent>,
    before_tool_call: Option<BeforeToolCall>,
    after_tool_call: Option<AfterToolCall>,
    finish_turn: Option<FinishTurn>,
    prepare_request: Option<PrepareRequest>,
    prepare_next_turn: Option<AgentPrepareNextTurn>,
    prepare_next_turn_with_context: Option<AgentPrepareNextTurnWithContext>,
}

struct Inner {
    shared: Mutex<Shared>,
    listeners: Mutex<Vec<(u64, AgentListener)>>,
    next_listener: AtomicU64,
    /// Serializes event processing so listeners never overlap.
    event_order: tokio::sync::Mutex<()>,
    hooks: Hooks,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        // State updates are single assignments, so a poisoned lock is still
        // consistent.
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn listeners(&self) -> MutexGuard<'_, Vec<(u64, AgentListener)>> {
        self.listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// A listener registration from [`Agent::subscribe`].
#[derive(Debug)]
pub struct Subscription {
    inner: Weak<Inner>,
    id: u64,
}

impl Subscription {
    /// Removes the listener. A run already calling it finishes that call.
    pub fn unsubscribe(self) {
        if let Some(inner) = self.inner.upgrade() {
            inner.listeners().retain(|(id, _)| *id != self.id);
        }
    }
}

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Agent")
    }
}

enum RunInput {
    Prompt {
        messages: Vec<AgentMessage>,
        skip_initial_steering_poll: bool,
    },
    Continue,
}

/// Pi's `Agent`: the stateful wrapper around the agent loop.
pub struct Agent {
    inner: Arc<Inner>,
}

impl fmt::Debug for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let shared = self.inner.lock();
        f.debug_struct("Agent")
            .field("model", &shared.model.id)
            .field("messages", &shared.messages.len())
            .field("is_streaming", &shared.is_streaming)
            .finish_non_exhaustive()
    }
}

impl Agent {
    /// An agent with `options`. Without a stream function in the options or
    /// installed with [`crate::set_default_stream_fn`], this fails as Pi's
    /// constructor throws.
    pub fn new(options: AgentOptions) -> Result<Self, AgentError> {
        let stream_fn = match options.stream_fn {
            Some(stream_fn) => stream_fn,
            None => default_stream_fn().map_err(|_| AgentError::NoStreamFn)?,
        };
        let initial = options.initial_state;
        let mut messages = initial.messages;
        let declarations: Vec<Tool> = initial
            .tools
            .iter()
            .map(|tool| tool.declaration())
            .collect();
        let initial_message =
            create_initial_system_message(initial.system_prompt.as_deref(), Some(&declarations));
        if let Some(system) = initial_message
            && messages
                .first()
                .is_none_or(|first| first.as_system().is_none())
        {
            messages.insert(0, system.into());
        }
        let shared = Shared {
            model: initial.model.unwrap_or_else(default_model),
            thinking_level: initial.thinking_level.unwrap_or(ModelThinkingLevel::Off),
            tools: initial.tools,
            messages,
            is_streaming: false,
            streaming_message: None,
            pending_tool_calls: BTreeSet::new(),
            error_message: None,
            steering: PendingMessageQueue::new(options.steering_mode),
            follow_up: PendingMessageQueue::new(options.follow_up_mode),
            session_id: options.session_id,
            thinking_budgets: options.thinking_budgets,
            max_retry_delay_ms: options.max_retry_delay_ms,
            tool_execution: options.tool_execution,
            active: None,
            closed: false,
        };
        let hooks = Hooks {
            convert_to_llm: options
                .convert_to_llm
                .unwrap_or_else(default_convert_to_llm),
            transform_context: options.transform_context,
            stream_fn,
            get_api_key: options.get_api_key,
            on_payload: options.on_payload,
            on_response: options.on_response,
            on_provider_stream_event: options.on_provider_stream_event,
            before_tool_call: options.before_tool_call,
            after_tool_call: options.after_tool_call,
            finish_turn: options.finish_turn,
            prepare_request: options.prepare_request,
            prepare_next_turn: options.prepare_next_turn,
            prepare_next_turn_with_context: options.prepare_next_turn_with_context,
        };
        Ok(Self {
            inner: Arc::new(Inner {
                shared: Mutex::new(shared),
                listeners: Mutex::new(Vec::new()),
                next_listener: AtomicU64::new(0),
                event_order: tokio::sync::Mutex::new(()),
                hooks,
            }),
        })
    }

    /// Subscribes to events. Listeners are awaited in subscription order and
    /// are part of the run: the agent is idle only after the `agent_end`
    /// listeners settle. Each receives the run's abort signal.
    ///
    /// A listener that needs the agent should capture a
    /// [`Weak`] reference: one that owns an `Arc<Agent>`
    /// keeps the agent alive, so dropping the other handles never cancels
    /// its run.
    pub fn subscribe<F>(&self, listener: F) -> Subscription
    where
        F: for<'a> Fn(&'a AgentEvent, &'a AbortSignal) -> BoxFuture<'a, ()> + Send + Sync + 'static,
    {
        let id = self.inner.next_listener.fetch_add(1, Ordering::Relaxed);
        self.inner.listeners().push((id, Arc::new(listener)));
        Subscription {
            inner: Arc::downgrade(&self.inner),
            id,
        }
    }

    /// Subscribes a synchronous listener.
    pub fn subscribe_fn<F>(&self, listener: F) -> Subscription
    where
        F: Fn(&AgentEvent, &AbortSignal) + Send + Sync + 'static,
    {
        self.subscribe(move |event, signal| {
            listener(event, signal);
            Box::pin(async {})
        })
    }

    /// A snapshot of the state, Pi's `state`.
    pub fn state(&self) -> AgentState {
        let shared = self.inner.lock();
        AgentState {
            system_prompt: get_current_system_prompt(&system_messages(&shared.messages)),
            model: shared.model.clone(),
            thinking_level: shared.thinking_level,
            tools: shared.tools.clone(),
            messages: shared.messages.clone(),
            is_streaming: shared.is_streaming,
            streaming_message: shared.streaming_message.clone(),
            pending_tool_calls: shared.pending_tool_calls.clone(),
            error_message: shared.error_message.clone(),
        }
    }

    /// The system prompt replayed from the transcript.
    pub fn system_prompt(&self) -> String {
        get_current_system_prompt(&system_messages(&self.inner.lock().messages))
    }

    /// The transcript.
    pub fn messages(&self) -> Vec<AgentMessage> {
        self.inner.lock().messages.clone()
    }

    /// Replaces the transcript.
    pub fn set_messages(&self, messages: Vec<AgentMessage>) {
        self.inner.lock().messages = messages;
    }

    /// Edits the transcript in place.
    pub fn update_messages<R>(&self, edit: impl FnOnce(&mut Vec<AgentMessage>) -> R) -> R {
        edit(&mut self.inner.lock().messages)
    }

    /// The executable tools.
    pub fn tools(&self) -> Vec<Arc<AgentTool>> {
        self.inner.lock().tools.clone()
    }

    /// Replaces the executable tools; the next request announces the
    /// difference to the model.
    pub fn set_tools(&self, tools: Vec<Arc<AgentTool>>) {
        self.inner.lock().tools = tools;
    }

    /// The model for future turns.
    pub fn model(&self) -> Model {
        self.inner.lock().model.clone()
    }

    /// Sets the model for future turns.
    pub fn set_model(&self, model: Model) {
        self.inner.lock().model = model;
    }

    /// The thinking level for future turns.
    pub fn thinking_level(&self) -> ModelThinkingLevel {
        self.inner.lock().thinking_level
    }

    /// Sets the thinking level for future turns.
    pub fn set_thinking_level(&self, level: ModelThinkingLevel) {
        self.inner.lock().thinking_level = level;
    }

    /// Whether a run is processing, until its `agent_end` listeners settle.
    pub fn is_streaming(&self) -> bool {
        self.inner.lock().is_streaming
    }

    /// The partial assistant message being streamed.
    pub fn streaming_message(&self) -> Option<AgentMessage> {
        self.inner.lock().streaming_message.clone()
    }

    /// Ids of executing tool calls.
    pub fn pending_tool_calls(&self) -> BTreeSet<String> {
        self.inner.lock().pending_tool_calls.clone()
    }

    /// The error of the latest failed or aborted assistant turn.
    pub fn error_message(&self) -> Option<String> {
        self.inner.lock().error_message.clone()
    }

    /// The session id forwarded to providers.
    pub fn session_id(&self) -> Option<String> {
        self.inner.lock().session_id.clone()
    }

    /// Sets the session id for later runs.
    pub fn set_session_id(&self, session_id: Option<String>) {
        self.inner.lock().session_id = session_id;
    }

    /// The thinking budgets forwarded to providers.
    pub fn thinking_budgets(&self) -> Option<ThinkingBudgets> {
        self.inner.lock().thinking_budgets
    }

    /// Sets the thinking budgets for later runs.
    pub fn set_thinking_budgets(&self, budgets: Option<ThinkingBudgets>) {
        self.inner.lock().thinking_budgets = budgets;
    }

    /// The cap of provider-requested retry delays.
    pub fn max_retry_delay_ms(&self) -> Option<u64> {
        self.inner.lock().max_retry_delay_ms
    }

    /// Sets the retry-delay cap for later runs.
    pub fn set_max_retry_delay_ms(&self, max: Option<u64>) {
        self.inner.lock().max_retry_delay_ms = max;
    }

    /// The tool execution mode.
    pub fn tool_execution(&self) -> ToolExecutionMode {
        self.inner.lock().tool_execution
    }

    /// Sets the tool execution mode for later runs.
    pub fn set_tool_execution(&self, mode: ToolExecutionMode) {
        self.inner.lock().tool_execution = mode;
    }

    /// How steering drains.
    pub fn steering_mode(&self) -> QueueMode {
        self.inner.lock().steering.mode
    }

    /// Sets how steering drains.
    pub fn set_steering_mode(&self, mode: QueueMode) {
        self.inner.lock().steering.mode = mode;
    }

    /// How follow-ups drain.
    pub fn follow_up_mode(&self) -> QueueMode {
        self.inner.lock().follow_up.mode
    }

    /// Sets how follow-ups drain.
    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        self.inner.lock().follow_up.mode = mode;
    }

    /// Queues a message for after the current assistant turn.
    pub fn steer(&self, message: impl Into<AgentMessage>) {
        self.inner
            .lock()
            .steering
            .messages
            .push_back(message.into());
    }

    /// Queues a message for when the agent would otherwise stop.
    pub fn follow_up(&self, message: impl Into<AgentMessage>) {
        self.inner
            .lock()
            .follow_up
            .messages
            .push_back(message.into());
    }

    /// Clears queued steering.
    pub fn clear_steering_queue(&self) {
        self.inner.lock().steering.messages.clear();
    }

    /// Clears queued follow-ups.
    pub fn clear_follow_up_queue(&self) {
        self.inner.lock().follow_up.messages.clear();
    }

    /// Clears both queues.
    pub fn clear_all_queues(&self) {
        let mut shared = self.inner.lock();
        shared.steering.messages.clear();
        shared.follow_up.messages.clear();
    }

    /// Whether either queue holds messages.
    pub fn has_queued_messages(&self) -> bool {
        let shared = self.inner.lock();
        !shared.steering.messages.is_empty() || !shared.follow_up.messages.is_empty()
    }

    /// The messages the next drain would take, without taking them.
    pub fn peek_queued_messages(&self) -> Vec<AgentMessage> {
        let shared = self.inner.lock();
        let steering = shared.steering.peek();
        if steering.is_empty() {
            shared.follow_up.peek()
        } else {
            steering
        }
    }

    /// The active run's abort signal.
    pub fn signal(&self) -> Option<AbortSignal> {
        self.inner
            .lock()
            .active
            .as_ref()
            .map(|run| run.controller.signal())
    }

    /// Signals the active run to abort, if one is active.
    pub fn abort(&self) {
        if let Some(run) = &self.inner.lock().active {
            run.controller.abort();
        }
    }

    /// Waits until the active run, its events, and its listeners are done.
    pub async fn wait_for_idle(&self) {
        let idle = self
            .inner
            .lock()
            .active
            .as_ref()
            .map(|run| run.idle.clone());
        if let Some(idle) = idle {
            wait_idle(idle).await;
        }
    }

    /// Aborts and cancels the active run, waits until its task is gone, and
    /// refuses later runs.
    pub async fn shutdown(&self) {
        let idle = {
            let mut shared = self.inner.lock();
            shared.closed = true;
            shared.active.as_ref().map(|run| {
                run.controller.abort();
                if let Some(task) = &run.task {
                    task.abort();
                }
                run.idle.clone()
            })
        };
        if let Some(idle) = idle {
            wait_idle(idle).await;
        }
    }

    /// Clears the transcript to its replayed system baseline and empties the
    /// queues.
    pub fn reset(&self) -> Result<(), AgentError> {
        let mut shared = self.inner.lock();
        if shared.active.is_some() {
            return Err(AgentError::AlreadyProcessingReset);
        }
        let baseline = get_current_system_message(&system_messages(&shared.messages));
        shared.messages = baseline.map(AgentMessage::from).into_iter().collect();
        shared.is_streaming = false;
        shared.streaming_message = None;
        shared.pending_tool_calls.clear();
        shared.error_message = None;
        shared.steering.messages.clear();
        shared.follow_up.messages.clear();
        Ok(())
    }

    /// Starts a prompt and waits until the agent is idle.
    pub async fn prompt(&self, input: impl Into<PromptInput>) -> Result<(), AgentError> {
        let messages = input.into().into_messages();
        let idle = self.begin(|shared| {
            if shared.active.is_some() {
                return Err(AgentError::AlreadyProcessingPrompt);
            }
            Ok(RunInput::Prompt {
                messages,
                skip_initial_steering_poll: false,
            })
        })?;
        wait_idle(idle).await;
        Ok(())
    }

    /// Continues from the transcript, Pi's `continue()`. From an assistant
    /// tail it runs queued steering, else queued follow-ups.
    pub async fn continue_run(&self) -> Result<(), AgentError> {
        let idle = self.begin(|shared| {
            if shared.active.is_some() {
                return Err(AgentError::AlreadyProcessingContinue);
            }
            let Some(last) = shared.messages.last() else {
                return Err(AgentError::NoMessagesToContinue);
            };
            if shared.messages.iter().all(|m| m.as_system().is_some()) {
                return Err(AgentError::NoMessagesToContinue);
            }
            if last.as_assistant().is_none() {
                return Ok(RunInput::Continue);
            }
            let steering = shared.steering.drain();
            if !steering.is_empty() {
                return Ok(RunInput::Prompt {
                    messages: steering,
                    skip_initial_steering_poll: true,
                });
            }
            let follow_ups = shared.follow_up.drain();
            if !follow_ups.is_empty() {
                return Ok(RunInput::Prompt {
                    messages: follow_ups,
                    skip_initial_steering_poll: false,
                });
            }
            Err(AgentError::ContinueFromAssistant)
        })?;
        wait_idle(idle).await;
        Ok(())
    }

    /// Selects the run under the state lock, so the check, the queue drain,
    /// and the activation are one step, then spawns it.
    fn begin(
        &self,
        select: impl FnOnce(&mut Shared) -> Result<RunInput, AgentError>,
    ) -> Result<watch::Receiver<bool>, AgentError> {
        let handle = tokio::runtime::Handle::try_current().map_err(|_| AgentError::NoRuntime)?;
        let mut shared = self.inner.lock();
        if shared.closed {
            return Err(AgentError::Closed);
        }
        let input = select(&mut shared)?;
        let controller = AbortController::new();
        let signal = controller.signal();
        let skip_initial_steering_poll = matches!(
            input,
            RunInput::Prompt {
                skip_initial_steering_poll: true,
                ..
            }
        );
        let context = AgentContext {
            messages: shared.messages.clone(),
            tools: shared.tools.clone(),
        };
        let observers = ObserverGate::new();
        let config = self.loop_config(&shared, skip_initial_steering_poll, &signal, &observers);
        shared.is_streaming = true;
        shared.streaming_message = None;
        shared.error_message = None;

        let (idle_sender, idle) = watch::channel(false);
        let guard = RunGuard {
            inner: Arc::clone(&self.inner),
            idle: idle_sender,
            observers: Arc::clone(&observers),
        };
        // The task waits for this lock before it touches the state, so the
        // run is registered before it can finish.
        let task = handle.spawn(run_task(guard, input, context, config, signal));
        shared.active = Some(ActiveRun {
            controller,
            idle: idle.clone(),
            task: Some(task.abort_handle()),
            observers,
        });
        Ok(idle)
    }

    fn loop_config(
        &self,
        shared: &Shared,
        skip_initial_steering_poll: bool,
        signal: &AbortSignal,
        observers: &Arc<ObserverGate>,
    ) -> AgentLoopConfig {
        let hooks = &self.inner.hooks;
        let mut config = AgentLoopConfig::new(shared.model.clone(), hooks.convert_to_llm.clone());
        config.stream_options = SimpleStreamOptions {
            base: StreamOptions {
                session_id: shared.session_id.clone(),
                on_payload: observers.on_payload(hooks.on_payload.as_ref()),
                on_response: observers.on_response(hooks.on_response.as_ref()),
                on_provider_stream_event: observers
                    .on_provider_stream_event(hooks.on_provider_stream_event.as_ref()),
                max_retry_delay_ms: shared.max_retry_delay_ms,
                ..StreamOptions::default()
            },
            tool_choice: None,
            reasoning: shared.thinking_level.level(),
            thinking_budgets: shared.thinking_budgets,
        };
        config.tool_execution = shared.tool_execution;
        config.transform_context = hooks.transform_context.clone();
        config.get_api_key = hooks.get_api_key.clone();
        config.before_tool_call = hooks.before_tool_call.clone();
        config.after_tool_call = hooks.after_tool_call.clone();
        config.finish_turn = hooks.finish_turn.clone();
        config.prepare_request = hooks.prepare_request.clone();
        config.prepare_next_turn = prepare_next_turn(hooks, signal);

        let skip = Arc::new(AtomicBool::new(skip_initial_steering_poll));
        let inner = Arc::clone(&self.inner);
        config.get_steering_messages = Some(hook::get_messages(move || {
            let messages = if skip.swap(false, Ordering::SeqCst) {
                Vec::new()
            } else {
                inner.lock().steering.drain()
            };
            Box::pin(async move { messages })
        }));
        let inner = Arc::clone(&self.inner);
        config.get_follow_up_messages = Some(hook::get_messages(move || {
            let messages = inner.lock().follow_up.drain();
            Box::pin(async move { messages })
        }));
        config
    }
}

fn prepare_next_turn(hooks: &Hooks, signal: &AbortSignal) -> Option<PrepareNextTurn> {
    let with_context = hooks.prepare_next_turn_with_context.clone();
    let legacy = hooks.prepare_next_turn.clone();
    if with_context.is_none() && legacy.is_none() {
        return None;
    }
    let signal = signal.clone();
    Some(hook::prepare_next_turn(move |turn| {
        match (&with_context, &legacy) {
            (Some(with_context), _) => with_context(turn, Some(signal.clone())),
            (None, Some(legacy)) => legacy(Some(signal.clone())),
            (None, None) => Box::pin(async { None }),
        }
    }))
}

impl Drop for Agent {
    fn drop(&mut self) {
        let mut shared = self.inner.lock();
        shared.closed = true;
        if let Some(run) = &shared.active {
            run.controller.abort();
            run.observers.close_without_waiting();
            if let Some(task) = &run.task {
                task.abort();
            }
        }
    }
}

async fn wait_idle(mut idle: watch::Receiver<bool>) {
    // An error means the run's guard is gone, which also means idle.
    let _ = idle.wait_for(|done| *done).await;
}

/// Ends a run however its task ends, Pi's `finishRun`: completion, panic,
/// or cancellation.
struct RunGuard {
    inner: Arc<Inner>,
    idle: watch::Sender<bool>,
    observers: Arc<ObserverGate>,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        // A provider task can outlive the run; its observers stop here.
        self.observers.close();
        {
            let mut shared = self.inner.lock();
            shared.is_streaming = false;
            shared.streaming_message = None;
            shared.pending_tool_calls.clear();
            shared.active = None;
        }
        self.idle.send_replace(true);
    }
}

async fn run_task(
    guard: RunGuard,
    input: RunInput,
    context: AgentContext,
    config: AgentLoopConfig,
    signal: AbortSignal,
) {
    let inner = Arc::clone(&guard.inner);
    let sink = event_sink(Arc::clone(&inner), signal.clone());
    let stream_fn = Some(inner.hooks.stream_fn.clone());
    let run_signal = Some(signal.clone());
    let run: BoxFuture<'static, Result<(), String>> = match input {
        RunInput::Prompt { messages, .. } => Box::pin(async move {
            run_agent_loop(messages, context, config, sink, run_signal, stream_fn)
                .await
                .map(drop)
                .map_err(|error| error.to_string())
        }),
        RunInput::Continue => Box::pin(async move {
            run_agent_loop_continue(context, config, sink, run_signal, stream_fn)
                .await
                .map(drop)
                .map_err(|error| error.to_string())
        }),
    };
    if let Err(error) = catching(run).await {
        let failure = handle_run_failure(&inner, error, &signal);
        // A listener that panics again only ends the failure report.
        let _ = catching(Box::pin(async move {
            failure.await;
            Ok(())
        }))
        .await;
    }
    drop(guard);
}

/// Pi's `handleRunFailure`: reports a failed run as a failed turn.
async fn handle_run_failure(inner: &Inner, error: String, signal: &AbortSignal) {
    let model = inner.lock().model.clone();
    let failure: AgentMessage = AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent::new(""))],
        api: model.api,
        provider: model.provider,
        model: model.id,
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: if signal.aborted() {
            StopReason::Aborted
        } else {
            StopReason::Error
        },
        deferred: None,
        error_message: Some(error),
        raw_stop_reason: None,
        end_turn: None,
        timestamp: bake_ai::now_ms(),
        duration_ms: None,
    }
    .into();
    for event in [
        AgentEvent::MessageStart {
            message: failure.clone(),
        },
        AgentEvent::MessageEnd {
            message: failure.clone(),
        },
        AgentEvent::TurnEnd {
            message: failure.clone(),
            tool_results: Vec::new(),
        },
        AgentEvent::AgentEnd {
            messages: vec![failure],
        },
    ] {
        process_event(inner, event, signal).await;
    }
}

fn event_sink(inner: Arc<Inner>, signal: AbortSignal) -> AgentEventSink {
    Arc::new(move |event| {
        let inner = Arc::clone(&inner);
        let signal = signal.clone();
        Box::pin(async move { process_event(&inner, event, &signal).await })
    })
}

/// Pi's `processEvents`: reduces the event into the state, then awaits each
/// listener in subscription order.
async fn process_event(inner: &Inner, event: AgentEvent, signal: &AbortSignal) {
    let _order = inner.event_order.lock().await;
    {
        let mut shared = inner.lock();
        if shared.closed {
            return;
        }
        match &event {
            AgentEvent::MessageStart { message } | AgentEvent::MessageUpdate { message, .. } => {
                shared.streaming_message = Some(message.clone());
            }
            AgentEvent::MessageEnd { message } => {
                shared.streaming_message = None;
                shared.messages.push(message.clone());
            }
            AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                shared.pending_tool_calls.insert(tool_call_id.clone());
            }
            AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
                shared.pending_tool_calls.remove(tool_call_id);
            }
            AgentEvent::TurnEnd { message, .. } => {
                if let Some(error) = message
                    .as_assistant()
                    .and_then(|assistant| assistant.error_message.clone())
                {
                    shared.error_message = Some(error);
                }
            }
            AgentEvent::AgentEnd { .. } => shared.streaming_message = None,
            AgentEvent::AgentStart
            | AgentEvent::TurnStart
            | AgentEvent::ToolExecutionUpdate { .. } => {}
        }
    }
    let listeners: Vec<AgentListener> = inner
        .listeners()
        .iter()
        .map(|(_, listener)| Arc::clone(listener))
        .collect();
    for listener in listeners {
        if inner.lock().closed {
            return;
        }
        listener(&event, signal).await;
    }
}
