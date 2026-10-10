//! Agent messages, tools, events, hooks, and loop configuration.
//!
//! Ported from Pi `packages/agent/src/types.ts` (v1.1.0). Pi's hooks are
//! JavaScript callbacks that may return promises; here each is an `Arc`'d
//! closure returning a boxed future. Hooks that Pi hands a live object
//! receive borrowed views ([`BeforeToolCallContext`], [`AgentTurnContext`],
//! and so on) whose futures may borrow them. Pi lets a hook throw; here a
//! fallible hook returns `Err(message)`, which the loop treats as Pi treats
//! the thrown error's message.

use std::any::Any;
use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use bake_ai::{
    AbortSignal, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream,
    ConstrainedSampling, Message, Model, ModelThinkingLevel, SimpleStreamOptions, SystemMessage,
    Tool, ToolCall, ToolResultMessage, TranscriptContext, Usage, UserContentBlock, UserMessage,
};
use serde::Serialize;
use serde::ser::Serializer;
use serde_json::Value;

/// A boxed, sendable future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Pi's `StreamFn`: starts a provider request for a normalized transcript.
///
/// The loop passes a transcript whose system messages carry the prompt and
/// tool declarations. Request, model, and runtime failures belong in the
/// returned stream as an `error` event whose message has stop reason `error`
/// or `aborted`. `Err` stands for Pi's thrown error, which the low-level loop
/// propagates and [`crate::Agent`] turns into a failed turn.
pub type StreamFn = Arc<
    dyn Fn(
            &Model,
            &TranscriptContext,
            SimpleStreamOptions,
        ) -> Result<AssistantMessageEventStream, String>
        + Send
        + Sync,
>;

/// How the tool calls of one assistant message run, Pi's `ToolExecutionMode`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolExecutionMode {
    /// Each call is prepared, executed, and finalized before the next starts.
    Sequential,
    /// Calls are prepared in order, then allowed calls execute concurrently.
    /// `tool_execution_end` follows completion order; tool-result messages
    /// follow the assistant's order.
    #[default]
    Parallel,
}

/// How many queued messages a drain point takes, Pi's `QueueMode`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize)]
pub enum QueueMode {
    /// Every queued message.
    #[serde(rename = "all")]
    All,
    /// Only the oldest queued message.
    #[default]
    #[serde(rename = "one-at-a-time")]
    OneAtATime,
}

/// An application message kind beyond the four LLM roles, Pi's
/// `CustomAgentMessages` extension point.
///
/// Custom messages live in the transcript and reach the model only through
/// [`AgentLoopConfig::convert_to_llm`], which maps them to LLM messages or
/// drops them; the default converter of [`crate::Agent`] drops them, as Pi's
/// does. Later crates add kinds by implementing this trait and downcast with
/// [`CustomAgentMessage::as_any`].
pub trait CustomAgentMessage: fmt::Debug + Send + Sync + 'static {
    /// The message's `role`, distinct from the LLM roles.
    fn role(&self) -> &str;

    /// Unix milliseconds.
    fn timestamp(&self) -> i64;

    /// The message as JSON, in the shape a session stores.
    fn to_json(&self) -> Value;

    /// For downcasting to the concrete kind.
    fn as_any(&self) -> &dyn Any;
}

/// Pi's `AgentMessage`: an LLM message or a custom application message.
// Unboxed like `bake_ai::Message`, so matching and construction stay plain.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum AgentMessage {
    /// A system, user, assistant, or tool-result message.
    Llm(Message),
    /// An application message; see [`CustomAgentMessage`].
    Custom(Arc<dyn CustomAgentMessage>),
}

impl AgentMessage {
    /// The `role` spelling.
    pub fn role(&self) -> &str {
        match self {
            Self::Llm(message) => message.role(),
            Self::Custom(message) => message.role(),
        }
    }

    /// Unix milliseconds.
    pub fn timestamp(&self) -> i64 {
        match self {
            Self::Llm(message) => message.timestamp(),
            Self::Custom(message) => message.timestamp(),
        }
    }

    /// The LLM message, if this is one.
    pub fn as_llm(&self) -> Option<&Message> {
        match self {
            Self::Llm(message) => Some(message),
            Self::Custom(_) => None,
        }
    }

    /// The system message, if this is one.
    pub fn as_system(&self) -> Option<&SystemMessage> {
        match self {
            Self::Llm(Message::System(message)) => Some(message),
            _ => None,
        }
    }

    /// The user message, if this is one.
    pub fn as_user(&self) -> Option<&UserMessage> {
        match self {
            Self::Llm(Message::User(message)) => Some(message),
            _ => None,
        }
    }

    /// The assistant message, if this is one.
    pub fn as_assistant(&self) -> Option<&AssistantMessage> {
        match self {
            Self::Llm(Message::Assistant(message)) => Some(message),
            _ => None,
        }
    }

    /// The tool-result message, if this is one.
    pub fn as_tool_result(&self) -> Option<&ToolResultMessage> {
        match self {
            Self::Llm(Message::ToolResult(message)) => Some(message),
            _ => None,
        }
    }

    /// The custom message, if this is one.
    pub fn as_custom(&self) -> Option<&Arc<dyn CustomAgentMessage>> {
        match self {
            Self::Custom(message) => Some(message),
            Self::Llm(_) => None,
        }
    }
}

/// LLM messages compare by value; custom messages by identity.
impl PartialEq for AgentMessage {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Llm(left), Self::Llm(right)) => left == right,
            (Self::Custom(left), Self::Custom(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }
}

impl Serialize for AgentMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Llm(message) => message.serialize(serializer),
            Self::Custom(message) => message.to_json().serialize(serializer),
        }
    }
}

impl From<Message> for AgentMessage {
    fn from(message: Message) -> Self {
        Self::Llm(message)
    }
}

impl From<SystemMessage> for AgentMessage {
    fn from(message: SystemMessage) -> Self {
        Self::Llm(Message::System(message))
    }
}

impl From<UserMessage> for AgentMessage {
    fn from(message: UserMessage) -> Self {
        Self::Llm(Message::User(message))
    }
}

impl From<AssistantMessage> for AgentMessage {
    fn from(message: AssistantMessage) -> Self {
        Self::Llm(Message::Assistant(message))
    }
}

impl From<ToolResultMessage> for AgentMessage {
    fn from(message: ToolResultMessage) -> Self {
        Self::Llm(Message::ToolResult(message))
    }
}

/// Final or partial result of a tool, Pi's `AgentToolResult`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolResult {
    /// Text or image content returned to the model.
    pub content: Vec<UserContentBlock>,
    /// Structured details for logs or the UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// Machine-readable result matching the tool's output schema, for
    /// programmatic callers; never sent to the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    /// Usage of the tool execution itself; not part of context accounting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Reports a failure without an `Err`: the model sees `content` as an
    /// error result, and `details` and `structured_content` are kept.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
    /// Asks the agent to stop after the current tool batch; it stops only
    /// when every finalized result in the batch asks.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub terminate: bool,
}

impl AgentToolResult {
    /// A result holding one text block.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![UserContentBlock::Text(bake_ai::TextContent::new(text))],
            ..Self::default()
        }
    }
}

/// Final outcome of a tool call after its hooks ran, Pi's
/// `AgentToolCallOutcome`.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentToolCallOutcome {
    /// The call.
    pub tool_call: ToolCall,
    /// The result.
    pub result: AgentToolResult,
    /// Whether the result counts as an error.
    pub is_error: bool,
    /// Milliseconds `execute` took on a monotonic clock; `None` when the tool
    /// did not run.
    pub duration_ms: Option<u64>,
}

/// Streams a tool's partial results, Pi's `AgentToolUpdateCallback`.
///
/// The callback belongs to one execution; calls after that execution
/// settled are ignored.
#[derive(Clone)]
pub struct AgentToolUpdateCallback(Arc<dyn Fn(AgentToolResult) + Send + Sync>);

impl AgentToolUpdateCallback {
    /// Wraps `callback`.
    pub fn new(callback: impl Fn(AgentToolResult) + Send + Sync + 'static) -> Self {
        Self(Arc::new(callback))
    }

    /// A callback that ignores every update.
    pub fn ignore() -> Self {
        Self::new(|_| {})
    }

    /// Reports a partial result.
    pub fn update(&self, partial: AgentToolResult) {
        (self.0)(partial);
    }
}

impl fmt::Debug for AgentToolUpdateCallback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AgentToolUpdateCallback")
    }
}

/// A tool's `execute`: `(tool_call_id, params, signal, on_update)`.
///
/// Return `Err(message)` on failure, or a result with `is_error`; do not
/// only describe the failure in `content`. A panic counts as a failure with
/// the panic's message.
pub type ToolExecute = Arc<
    dyn Fn(
            String,
            Value,
            Option<AbortSignal>,
            AgentToolUpdateCallback,
        ) -> BoxFuture<'static, Result<AgentToolResult, String>>
        + Send
        + Sync,
>;

/// Rewrites raw tool-call arguments before schema validation, Pi's
/// `prepareArguments`. `Err(message)` rejects the call as Pi's throwing
/// shim does: the call ends with an error result carrying the message.
pub type PrepareArguments = Arc<dyn Fn(Value) -> Result<Value, String> + Send + Sync>;

/// Recovery policy for an effect whose outcome is unknown, Pi's `replay`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolReplay {
    /// Never replay.
    Never,
    /// Safe to replay.
    Safe,
}

/// A tool the agent can run, Pi's `AgentTool`.
#[derive(Clone)]
pub struct AgentTool {
    /// The name the model calls.
    pub name: String,
    /// Human-readable label for the UI.
    pub label: String,
    /// What the tool does, for the model.
    pub description: String,
    /// JSON schema of the arguments.
    pub parameters: Value,
    /// Provider-side constrained sampling.
    pub constrained_sampling: Option<ConstrainedSampling>,
    /// JSON schema of `structured_content` in successful results.
    pub output_schema: Option<Value>,
    /// Compatibility shim applied to raw arguments before validation.
    pub prepare_arguments: Option<PrepareArguments>,
    /// Runs the call.
    pub execute: ToolExecute,
    /// Recovery policy for an unknown outcome.
    pub replay: Option<ToolReplay>,
    /// Per-tool execution mode; `sequential` forces the whole batch to run
    /// one call at a time.
    pub execution_mode: Option<ToolExecutionMode>,
}

impl AgentTool {
    /// A tool with the given basics and no optional members.
    pub fn new<F>(
        name: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
        execute: F,
    ) -> Self
    where
        F: Fn(
                String,
                Value,
                Option<AbortSignal>,
                AgentToolUpdateCallback,
            ) -> BoxFuture<'static, Result<AgentToolResult, String>>
            + Send
            + Sync
            + 'static,
    {
        Self {
            name: name.into(),
            label: label.into(),
            description: description.into(),
            parameters,
            constrained_sampling: None,
            output_schema: None,
            prepare_arguments: None,
            execute: Arc::new(execute),
            replay: None,
            execution_mode: None,
        }
    }

    /// The declaration the model sees, Pi's `toToolDeclaration`.
    pub fn declaration(&self) -> Tool {
        Tool {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
            constrained_sampling: self.constrained_sampling.clone(),
        }
    }
}

impl fmt::Debug for AgentTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentTool")
            .field("name", &self.name)
            .field("label", &self.label)
            .field("execution_mode", &self.execution_mode)
            .finish_non_exhaustive()
    }
}

/// The transcript and tools the loop works on, Pi's `AgentContext`.
#[derive(Debug, Clone, Default)]
pub struct AgentContext {
    /// The transcript visible to the model.
    pub messages: Vec<AgentMessage>,
    /// Tools the run may execute.
    pub tools: Vec<Arc<AgentTool>>,
}

/// Returned by [`AgentLoopConfig::before_tool_call`], Pi's
/// `BeforeToolCallResult`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BeforeToolCallResult {
    /// Prevents execution; the loop emits an error result instead.
    pub block: bool,
    /// The text of that error result.
    pub reason: Option<String>,
    /// For a blocked call, asks the agent to stop after the batch.
    pub terminate: bool,
}

/// Field-wise override returned by [`AgentLoopConfig::after_tool_call`],
/// Pi's `AfterToolCallResult`. A `Some` replaces that field in full;
/// `content` without `structured_content` drops the structured content.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AfterToolCallResult {
    /// Replaces the content.
    pub content: Option<Vec<UserContentBlock>>,
    /// Replaces the details.
    pub details: Option<Value>,
    /// Replaces the structured content.
    pub structured_content: Option<Value>,
    /// Replaces the error flag.
    pub is_error: Option<bool>,
    /// Replaces the usage.
    pub usage: Option<Usage>,
    /// Replaces the early-termination hint.
    pub terminate: Option<bool>,
}

/// What [`AgentLoopConfig::before_tool_call`] sees, Pi's
/// `BeforeToolCallContext`. `args` are the validated arguments; changes to
/// them reach `execute` without revalidation.
#[derive(Debug)]
pub struct BeforeToolCallContext<'a> {
    /// The message that requested the call.
    pub assistant_message: &'a AssistantMessage,
    /// The raw tool call.
    pub tool_call: &'a ToolCall,
    /// The validated arguments.
    pub args: &'a mut Value,
    /// The context when the call is prepared.
    pub context: &'a AgentContext,
}

/// What [`AgentLoopConfig::after_tool_call`] sees, Pi's
/// `AfterToolCallContext`.
#[derive(Debug, Clone, Copy)]
pub struct AfterToolCallContext<'a> {
    /// The message that requested the call.
    pub assistant_message: &'a AssistantMessage,
    /// The raw tool call.
    pub tool_call: &'a ToolCall,
    /// The validated arguments.
    pub args: &'a Value,
    /// The executed result before overrides.
    pub result: &'a AgentToolResult,
    /// Whether that result counts as an error.
    pub is_error: bool,
    /// The context when the call is finalized.
    pub context: &'a AgentContext,
}

/// A completed turn, Pi's `AgentTurnContext` and `PrepareNextTurnContext`.
#[derive(Debug, Clone, Copy)]
pub struct AgentTurnContext<'a> {
    /// The assistant message that completed the turn.
    pub message: &'a AssistantMessage,
    /// The turn's tool results.
    pub tool_results: &'a [ToolResultMessage],
    /// The context after the turn's messages were appended.
    pub context: &'a AgentContext,
    /// What the loop returns if it exits here: prompt runs include the
    /// prompt messages; continuations exclude pre-existing context.
    pub new_messages: &'a [AgentMessage],
}

/// The decision of [`AgentLoopConfig::finish_turn`], Pi's
/// `AgentTurnDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTurnDecision {
    /// Ensure one more provider request.
    Continue,
    /// End the run without polling queues.
    End,
}

/// Replacement state before another provider request, Pi's
/// `AgentLoopTurnUpdate`.
#[derive(Debug, Clone, Default)]
pub struct AgentLoopTurnUpdate {
    /// Context for the next request.
    pub context: Option<AgentContext>,
    /// Messages appended before the next request, with lifecycle events.
    pub messages: Vec<AgentMessage>,
    /// Model for the next request.
    pub model: Option<Model>,
    /// Thinking level for the next request.
    pub thinking_level: Option<ModelThinkingLevel>,
}

/// Replacement state for the request being prepared, Pi's
/// `AgentRequestUpdate`.
#[derive(Debug, Clone, Default)]
pub struct AgentRequestUpdate {
    /// Context for this and later requests.
    pub context: Option<AgentContext>,
    /// Model for this and later requests.
    pub model: Option<Model>,
    /// Thinking level for this and later requests.
    pub thinking_level: Option<ModelThinkingLevel>,
}

/// What [`AgentLoopConfig::prepare_request`] sees, Pi's
/// `PrepareRequestContext`.
#[derive(Debug, Clone, Copy)]
pub struct PrepareRequestContext<'a> {
    /// The context, pending messages appended.
    pub context: &'a AgentContext,
    /// The model.
    pub model: &'a Model,
    /// The thinking level.
    pub thinking_level: ModelThinkingLevel,
}

/// Maps the transcript to LLM messages before each request.
pub type ConvertToLlm =
    Arc<dyn for<'a> Fn(&'a [AgentMessage]) -> BoxFuture<'a, Vec<Message>> + Send + Sync>;
/// Rewrites the transcript before [`ConvertToLlm`].
pub type TransformContext = Arc<
    dyn Fn(Vec<AgentMessage>, Option<AbortSignal>) -> BoxFuture<'static, Vec<AgentMessage>>
        + Send
        + Sync,
>;
/// Resolves an API key for a provider before each request.
pub type GetApiKey = Arc<dyn for<'a> Fn(&'a str) -> BoxFuture<'a, Option<String>> + Send + Sync>;
/// Drains queued messages (steering or follow-up).
pub type GetMessages = Arc<dyn Fn() -> BoxFuture<'static, Vec<AgentMessage>> + Send + Sync>;
/// Pi's `beforeToolCall`.
pub type BeforeToolCall = Arc<
    dyn for<'a> Fn(
            BeforeToolCallContext<'a>,
            Option<AbortSignal>,
        ) -> BoxFuture<'a, Result<Option<BeforeToolCallResult>, String>>
        + Send
        + Sync,
>;
/// Pi's `afterToolCall`.
pub type AfterToolCall = Arc<
    dyn for<'a> Fn(
            AfterToolCallContext<'a>,
            Option<AbortSignal>,
        ) -> BoxFuture<'a, Result<Option<AfterToolCallResult>, String>>
        + Send
        + Sync,
>;
/// Pi's `FinishTurn`.
pub type FinishTurn = Arc<
    dyn for<'a> Fn(
            AgentTurnContext<'a>,
            Option<AbortSignal>,
        ) -> BoxFuture<'a, Option<AgentTurnDecision>>
        + Send
        + Sync,
>;
/// Pi's `PrepareRequest`.
pub type PrepareRequest = Arc<
    dyn for<'a> Fn(
            PrepareRequestContext<'a>,
            Option<AbortSignal>,
        ) -> BoxFuture<'a, Option<AgentRequestUpdate>>
        + Send
        + Sync,
>;
/// Pi's `prepareNextTurn` of the loop config.
pub type PrepareNextTurn = Arc<
    dyn for<'a> Fn(AgentTurnContext<'a>) -> BoxFuture<'a, Option<AgentLoopTurnUpdate>>
        + Send
        + Sync,
>;

/// Pi's `AgentLoopConfig`.
#[derive(Clone)]
pub struct AgentLoopConfig {
    /// The model of the first request.
    pub model: Model,
    /// Options passed to every request; `reasoning` is the thinking level,
    /// and the loop sets `signal` and the resolved `api_key`.
    pub stream_options: SimpleStreamOptions,
    /// Converts the transcript to LLM messages before each request. Messages
    /// that cannot be converted, such as UI-only notices, are dropped.
    pub convert_to_llm: ConvertToLlm,
    /// Applied to the transcript before `convert_to_llm`, for pruning or
    /// injection at the agent-message level.
    pub transform_context: Option<TransformContext>,
    /// Resolves the API key per request; `stream_options.base.api_key` is the
    /// fallback.
    pub get_api_key: Option<GetApiKey>,
    /// Runs after a turn's assistant message and tool results, before
    /// `turn_end`. `End` stops the run; `Continue` ensures one more request.
    /// Error and aborted responses end the run regardless.
    pub finish_turn: Option<FinishTurn>,
    /// Runs before every provider request; its replacements last for the
    /// rest of the run. It does not poll queues.
    pub prepare_request: Option<PrepareRequest>,
    /// Runs after `turn_end` when the loop continues, before the next turn.
    pub prepare_next_turn: Option<PrepareNextTurn>,
    /// Steering messages, injected after the current turn's tool calls.
    pub get_steering_messages: Option<GetMessages>,
    /// Follow-up messages, processed when the agent would otherwise stop.
    pub get_follow_up_messages: Option<GetMessages>,
    /// Tool execution mode; parallel by default.
    pub tool_execution: ToolExecutionMode,
    /// Runs before a tool executes, after validation; may block the call.
    pub before_tool_call: Option<BeforeToolCall>,
    /// Runs after a tool executes; may override parts of its result.
    pub after_tool_call: Option<AfterToolCall>,
}

impl AgentLoopConfig {
    /// A configuration with `model`, `convert_to_llm`, and no hooks.
    pub fn new(model: Model, convert_to_llm: ConvertToLlm) -> Self {
        Self {
            model,
            stream_options: SimpleStreamOptions::default(),
            convert_to_llm,
            transform_context: None,
            get_api_key: None,
            finish_turn: None,
            prepare_request: None,
            prepare_next_turn: None,
            get_steering_messages: None,
            get_follow_up_messages: None,
            tool_execution: ToolExecutionMode::Parallel,
            before_tool_call: None,
            after_tool_call: None,
        }
    }
}

impl fmt::Debug for AgentLoopConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentLoopConfig")
            .field("model", &self.model.id)
            .field("stream_options", &self.stream_options)
            .field("tool_execution", &self.tool_execution)
            .finish_non_exhaustive()
    }
}

/// Pi's `AgentEvent`. A turn is one assistant response and its tool calls.
// Events move once from the loop to the sink; boxing would only add an
// allocation per event.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// The run started.
    AgentStart,
    /// The run ended; the last event of a run.
    AgentEnd {
        /// The run's new messages.
        messages: Vec<AgentMessage>,
    },
    /// A turn started.
    TurnStart,
    /// A turn ended.
    TurnEnd {
        /// The turn's assistant message.
        message: AgentMessage,
        /// The turn's tool results.
        tool_results: Vec<ToolResultMessage>,
    },
    /// A message started: system, user, assistant, tool result, or custom.
    MessageStart {
        /// The message so far.
        message: AgentMessage,
    },
    /// A streamed assistant message changed.
    MessageUpdate {
        /// The message so far.
        message: AgentMessage,
        /// The provider event.
        assistant_message_event: AssistantMessageEvent,
    },
    /// A message is complete.
    MessageEnd {
        /// The message.
        message: AgentMessage,
    },
    /// A tool call started.
    ToolExecutionStart {
        /// The call id.
        tool_call_id: String,
        /// The tool name.
        tool_name: String,
        /// The raw arguments.
        args: Value,
    },
    /// A tool reported a partial result.
    ToolExecutionUpdate {
        /// The call id.
        tool_call_id: String,
        /// The tool name.
        tool_name: String,
        /// The raw arguments.
        args: Value,
        /// The partial result.
        partial_result: AgentToolResult,
    },
    /// A tool call finished.
    ToolExecutionEnd {
        /// The call id.
        tool_call_id: String,
        /// The tool name.
        tool_name: String,
        /// The final result.
        result: AgentToolResult,
        /// Whether it counts as an error.
        is_error: bool,
        /// Milliseconds `execute` took; `None` when the tool did not run.
        duration_ms: Option<u64>,
    },
}

impl AgentEvent {
    /// The `type` spelling.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::AgentStart => "agent_start",
            Self::AgentEnd { .. } => "agent_end",
            Self::TurnStart => "turn_start",
            Self::TurnEnd { .. } => "turn_end",
            Self::MessageStart { .. } => "message_start",
            Self::MessageUpdate { .. } => "message_update",
            Self::MessageEnd { .. } => "message_end",
            Self::ToolExecutionStart { .. } => "tool_execution_start",
            Self::ToolExecutionUpdate { .. } => "tool_execution_update",
            Self::ToolExecutionEnd { .. } => "tool_execution_end",
        }
    }
}

/// Receives the loop's events, Pi's `AgentEventSink`. The loop awaits each
/// returned future before it goes on, except that tools running in parallel
/// may emit concurrently.
pub type AgentEventSink = Arc<dyn Fn(AgentEvent) -> BoxFuture<'static, ()> + Send + Sync>;

/// Public agent state, Pi's `AgentState`, as a snapshot.
#[derive(Debug, Clone)]
pub struct AgentState {
    /// The system prompt replayed from the transcript's system messages.
    pub system_prompt: String,
    /// The model for future turns.
    pub model: Model,
    /// The thinking level for future turns.
    pub thinking_level: ModelThinkingLevel,
    /// Executable tools.
    pub tools: Vec<Arc<AgentTool>>,
    /// The transcript.
    pub messages: Vec<AgentMessage>,
    /// True while a prompt or continuation runs, until `agent_end`
    /// listeners settle.
    pub is_streaming: bool,
    /// The partial assistant message being streamed.
    pub streaming_message: Option<AgentMessage>,
    /// Ids of executing tool calls.
    pub pending_tool_calls: BTreeSet<String>,
    /// The error of the latest failed or aborted assistant turn.
    pub error_message: Option<String>,
}
