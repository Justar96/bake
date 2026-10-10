//! The agent loop: turns, streamed assistant messages, and tool execution.
//!
//! Ported from Pi `packages/agent/src/agent-loop.ts` (v1.1.0). The loop
//! works on [`AgentMessage`]s and converts them to LLM messages only at the
//! provider boundary. It emits Pi's events in Pi's order and runs Pi's hooks
//! at the same points.
//!
//! Differences from Pi, none of them visible in the event sequence:
//!
//! - Pi swaps the streamed partial into the context on every delta; nothing
//!   reads the context while a response streams, so here only the final
//!   message is stored, which saves a copy per delta.
//! - Tools that run in parallel are polled concurrently on the loop's own
//!   task, never spawned, so no tool future outlives the loop. A tool's
//!   updates are emitted one at a time, in the order it sent them, while the
//!   tool keeps running; its duration ends when it settles, so slow
//!   listeners do not lengthen it.
//! - A tool, `prepareArguments`, `beforeToolCall`, or `afterToolCall` that
//!   panics is treated as Pi treats one that throws: an error result with the
//!   panic's message.
//! - Durations use Tokio's monotonic clock, so paused-clock tests measure
//!   them exactly.
//! - [`agent_loop`] runs on a spawned Tokio task that the returned
//!   [`AgentLoopStream`] owns: dropping the stream cancels the run.

use std::any::Any;
use std::fmt;
use std::future::{Future, poll_fn};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context as TaskContext, Poll};

use bake_ai::transcript::{get_current_tools, get_tool_state_changes, normalize_context};
use bake_ai::{
    AbortSignal, AssistantMessage, AssistantMessageEvent, Context, EventStream, Message,
    ModelThinkingLevel, StopReason, SystemContent, SystemMessage, ThinkingLevel, Tool, ToolCall,
    ToolReference, ToolResultMessage, event_channel,
};
use serde_json::{Value, json};

use crate::stream_fn::default_stream_fn;
use crate::types::{
    AfterToolCall, AfterToolCallContext, AgentContext, AgentEvent, AgentEventSink, AgentLoopConfig,
    AgentMessage, AgentTool, AgentToolCallOutcome, AgentToolResult, AgentToolUpdateCallback,
    AgentTurnContext, AgentTurnDecision, BeforeToolCall, BeforeToolCallContext, BoxFuture,
    PrepareRequestContext, StreamFn, ToolExecutionMode,
};
use crate::validation::validate_tool_arguments;

/// Why a loop could not start or stopped without `agent_end`; Pi throws
/// these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentLoopError {
    /// A continuation needs a message to continue from.
    NoMessages,
    /// A continuation cannot start from an assistant message.
    ContinueFromAssistant,
    /// No stream function was passed or installed.
    NoStreamFn,
    /// The stream function failed instead of returning a stream.
    Stream(String),
}

impl fmt::Display for AgentLoopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoMessages => f.write_str("Cannot continue: no messages in context"),
            Self::ContinueFromAssistant => {
                f.write_str("Cannot continue from message role: assistant")
            }
            Self::NoStreamFn => f.write_str(crate::stream_fn::NO_DEFAULT_STREAM_FN),
            Self::Stream(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for AgentLoopError {}

fn resolve_stream_fn(stream_fn: Option<StreamFn>) -> Result<StreamFn, AgentLoopError> {
    match stream_fn {
        Some(stream_fn) => Ok(stream_fn),
        None => default_stream_fn().map_err(|_| AgentLoopError::NoStreamFn),
    }
}

fn check_continuable(context: &AgentContext) -> Result<(), AgentLoopError> {
    match context.messages.last() {
        None => Err(AgentLoopError::NoMessages),
        Some(message) if message.as_assistant().is_some() => {
            Err(AgentLoopError::ContinueFromAssistant)
        }
        Some(_) => Ok(()),
    }
}

/// The events and final messages of a loop started by [`agent_loop`] or
/// [`agent_loop_continue`]. Dropping it cancels the run's task.
pub struct AgentLoopStream {
    events: EventStream<AgentEvent, Vec<AgentMessage>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl AgentLoopStream {
    /// The next event, or `None` once the run ended and its events are read.
    pub async fn next(&self) -> Option<AgentEvent> {
        self.events.next().await
    }

    /// The run's new messages once it ends; `None` when it ended without
    /// `agent_end`.
    pub async fn result(&self) -> Option<Vec<AgentMessage>> {
        self.events.result().await
    }
}

impl Drop for AgentLoopStream {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl fmt::Debug for AgentLoopStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentLoopStream").finish_non_exhaustive()
    }
}

fn spawn_loop<F, Fut>(run: F) -> AgentLoopStream
where
    F: FnOnce(AgentEventSink) -> Fut,
    Fut: Future<Output = Result<Vec<AgentMessage>, AgentLoopError>> + Send + 'static,
{
    let (sender, events) = event_channel(
        |event: &AgentEvent| matches!(event, AgentEvent::AgentEnd { .. }),
        |event: &AgentEvent| match event {
            AgentEvent::AgentEnd { messages } => Some(messages.clone()),
            _ => None,
        },
    );
    let sender = Arc::new(sender);
    let sink: AgentEventSink = {
        let sender = Arc::clone(&sender);
        Arc::new(move |event| {
            sender.push(event);
            Box::pin(async {})
        })
    };
    let future = run(sink);
    let task = match tokio::runtime::Handle::try_current() {
        Ok(handle) => Some(handle.spawn(async move {
            let messages = future.await.ok();
            sender.end(messages);
        })),
        // Without a runtime the run cannot start; the stream ends empty.
        Err(_) => {
            sender.end(None);
            None
        }
    };
    AgentLoopStream { events, task }
}

/// Starts a loop with new prompt messages, Pi's `agentLoop`. The prompts are
/// added to the context with events. Needs a Tokio runtime; without one the
/// stream ends at once with no result.
pub fn agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> AgentLoopStream {
    spawn_loop(move |sink| run_agent_loop(prompts, context, config, sink, signal, stream_fn))
}

/// Continues a loop from the current context, Pi's `agentLoopContinue`.
///
/// The last message must convert to a `user` or `toolResult` message through
/// `convert_to_llm`; only an empty context or an assistant tail is rejected
/// here.
pub fn agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Result<AgentLoopStream, AgentLoopError> {
    check_continuable(&context)?;
    Ok(spawn_loop(move |sink| {
        run_agent_loop_continue(context, config, sink, signal, stream_fn)
    }))
}

async fn emit(sink: &AgentEventSink, event: AgentEvent) {
    sink(event).await;
}

/// Pi's `runAgentLoop`: runs a prompt to its end on the caller's task and
/// returns the new messages.
pub async fn run_agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    sink: AgentEventSink,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Result<Vec<AgentMessage>, AgentLoopError> {
    let stream_fn = resolve_stream_fn(stream_fn)?;
    let initial = declare_tool_changes(&context, prompts);
    let mut new_messages = initial.clone();
    let mut current = context;
    current.messages.extend(initial.iter().cloned());

    emit(&sink, AgentEvent::AgentStart).await;
    emit(&sink, AgentEvent::TurnStart).await;
    for message in initial {
        emit(
            &sink,
            AgentEvent::MessageStart {
                message: message.clone(),
            },
        )
        .await;
        emit(&sink, AgentEvent::MessageEnd { message }).await;
    }

    run_loop(
        current,
        &mut new_messages,
        &config,
        signal,
        &sink,
        &stream_fn,
    )
    .await?;
    Ok(new_messages)
}

/// Pi's `runAgentLoopContinue`: continues from `context` on the caller's
/// task and returns the new messages.
pub async fn run_agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    sink: AgentEventSink,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Result<Vec<AgentMessage>, AgentLoopError> {
    check_continuable(&context)?;
    let stream_fn = resolve_stream_fn(stream_fn)?;
    let mut new_messages = Vec::new();

    emit(&sink, AgentEvent::AgentStart).await;
    emit(&sink, AgentEvent::TurnStart).await;

    run_loop(
        context,
        &mut new_messages,
        &config,
        signal,
        &sink,
        &stream_fn,
    )
    .await?;
    Ok(new_messages)
}

async fn poll_messages(source: Option<&crate::types::GetMessages>) -> Vec<AgentMessage> {
    match source {
        Some(source) => source().await,
        None => Vec::new(),
    }
}

fn thinking_level(reasoning: Option<ThinkingLevel>) -> ModelThinkingLevel {
    reasoning.map_or(ModelThinkingLevel::Off, ModelThinkingLevel::from)
}

/// Pi's `runLoop`, shared by both entry points.
async fn run_loop(
    initial_context: AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    config: &AgentLoopConfig,
    signal: Option<AbortSignal>,
    sink: &AgentEventSink,
    stream_fn: &StreamFn,
) -> Result<(), AgentLoopError> {
    let mut current = initial_context;
    let mut model = config.model.clone();
    let mut reasoning = config.stream_options.reasoning;
    let mut last_completed: Option<(AssistantMessage, Vec<ToolResultMessage>)> = None;
    let mut explicit_continuation = false;
    // Steering typed while the run was starting.
    let mut pending = poll_messages(config.get_steering_messages.as_ref()).await;

    loop {
        let mut has_more_tool_calls = true;

        while has_more_tool_calls || !pending.is_empty() {
            let mut prepared = Vec::new();
            if let Some((message, tool_results)) = &last_completed {
                if let Some(prepare) = &config.prepare_next_turn {
                    let update = prepare(AgentTurnContext {
                        message,
                        tool_results,
                        context: &current,
                        new_messages,
                    })
                    .await;
                    if let Some(update) = update {
                        if let Some(context) = update.context {
                            current = context;
                        }
                        prepared = update.messages;
                        if let Some(next) = update.model {
                            model = next;
                        }
                        if let Some(level) = update.thinking_level {
                            reasoning = level.level();
                        }
                    }
                }
                // Preparation can be long-running (compaction, say): pick up
                // steering queued meanwhile, but only if the earlier poll
                // returned nothing, or one-at-a-time mode would deliver two.
                if pending.is_empty() {
                    pending = poll_messages(config.get_steering_messages.as_ref()).await;
                }
                emit(sink, AgentEvent::TurnStart).await;
            }

            prepared.append(&mut pending);
            for message in declare_tool_changes(&current, prepared) {
                emit(
                    sink,
                    AgentEvent::MessageStart {
                        message: message.clone(),
                    },
                )
                .await;
                emit(
                    sink,
                    AgentEvent::MessageEnd {
                        message: message.clone(),
                    },
                )
                .await;
                current.messages.push(message.clone());
                new_messages.push(message);
            }

            if let Some(prepare) = &config.prepare_request {
                let update = prepare(
                    PrepareRequestContext {
                        context: &current,
                        model: &model,
                        thinking_level: thinking_level(reasoning),
                    },
                    signal.clone(),
                )
                .await;
                if let Some(update) = update {
                    if let Some(context) = update.context {
                        current = context;
                    }
                    if let Some(next) = update.model {
                        model = next;
                    }
                    if let Some(level) = update.thinking_level {
                        reasoning = level.level();
                    }
                }
            }

            let message = stream_assistant_response(
                &mut current,
                config,
                &model,
                reasoning,
                signal.as_ref(),
                sink,
                stream_fn,
            )
            .await?;
            new_messages.push(message.clone().into());

            if matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) {
                if let Some(finish) = &config.finish_turn {
                    finish(
                        AgentTurnContext {
                            message: &message,
                            tool_results: &[],
                            context: &current,
                            new_messages,
                        },
                        signal.clone(),
                    )
                    .await;
                }
                emit(
                    sink,
                    AgentEvent::TurnEnd {
                        message: message.into(),
                        tool_results: Vec::new(),
                    },
                )
                .await;
                emit(
                    sink,
                    AgentEvent::AgentEnd {
                        messages: new_messages.clone(),
                    },
                )
                .await;
                return Ok(());
            }

            let tool_calls: Vec<ToolCall> = message.tool_calls().cloned().collect();
            let mut tool_results = Vec::new();
            has_more_tool_calls = false;
            if !tool_calls.is_empty() {
                // A `length` stop cut the output at the token limit, so any
                // call may carry truncated arguments: fail them all.
                let batch = if message.stop_reason == StopReason::Length {
                    fail_tool_calls_from_truncated_message(&tool_calls, sink).await
                } else {
                    execute_tool_calls(
                        &current,
                        &message,
                        &tool_calls,
                        config,
                        signal.as_ref(),
                        sink,
                    )
                    .await
                };
                tool_results = batch.messages;
                has_more_tool_calls = !batch.terminate;
                for result in &tool_results {
                    current.messages.push(result.clone().into());
                    new_messages.push(result.clone().into());
                }
            }

            let decision = match &config.finish_turn {
                Some(finish) => {
                    finish(
                        AgentTurnContext {
                            message: &message,
                            tool_results: &tool_results,
                            context: &current,
                            new_messages,
                        },
                        signal.clone(),
                    )
                    .await
                }
                None => None,
            };
            emit(
                sink,
                AgentEvent::TurnEnd {
                    message: message.clone().into(),
                    tool_results: tool_results.clone(),
                },
            )
            .await;
            last_completed = Some((message, tool_results));

            if decision == Some(AgentTurnDecision::End) {
                emit(
                    sink,
                    AgentEvent::AgentEnd {
                        messages: new_messages.clone(),
                    },
                )
                .await;
                return Ok(());
            }

            explicit_continuation = decision == Some(AgentTurnDecision::Continue);
            pending = poll_messages(config.get_steering_messages.as_ref()).await;
            if has_more_tool_calls || !pending.is_empty() {
                explicit_continuation = false;
            }
        }

        // The agent would stop here; follow-ups keep it going.
        let follow_ups = poll_messages(config.get_follow_up_messages.as_ref()).await;
        if !follow_ups.is_empty() {
            explicit_continuation = false;
            pending = follow_ups;
            continue;
        }

        // No natural request was selected: honor `Continue` with one
        // context-only turn.
        if explicit_continuation {
            explicit_continuation = false;
            continue;
        }

        break;
    }

    emit(
        sink,
        AgentEvent::AgentEnd {
            messages: new_messages.clone(),
        },
    )
    .await;
    Ok(())
}

/// Pi's `declareToolChanges`: announces tool loadout changes to the model.
///
/// `context.tools` is what the run can execute; the transcript's system
/// messages declare what the model may call. When a pending system message
/// exists, its tool fields are replaced by the delta between the committed
/// transcript and the executable set; otherwise a new system message goes
/// before the first non-system pending message.
fn declare_tool_changes(context: &AgentContext, pending: Vec<AgentMessage>) -> Vec<AgentMessage> {
    let system_index = pending
        .iter()
        .rposition(|message| message.as_system().is_some());
    let pending_system = system_index.and_then(|index| pending.get(index)?.as_system().cloned());
    let baseline: Vec<AgentMessage> = match (&pending_system, system_index) {
        (Some(system), Some(index)) => pending
            .iter()
            .enumerate()
            .map(|(position, message)| {
                if position == index {
                    with_tool_changes(system, Vec::new(), Vec::new()).into()
                } else {
                    message.clone()
                }
            })
            .collect(),
        _ => pending.clone(),
    };
    let systems: Vec<Message> = context
        .messages
        .iter()
        .chain(&baseline)
        .filter_map(|message| message.as_system().cloned().map(Message::System))
        .collect();
    let executable: Vec<Tool> = context
        .tools
        .iter()
        .map(|tool| tool.declaration())
        .collect();
    let (added, removed) = get_tool_state_changes(&get_current_tools(&systems), &executable);
    let unchanged = added.is_empty() && removed.is_empty();

    if let (Some(system), Some(index)) = (pending_system, system_index) {
        // Keep the caller's message when it already declares no changes.
        let declares_nothing = system.tools_added.as_ref().is_none_or(Vec::is_empty)
            && system.tools_removed.as_ref().is_none_or(Vec::is_empty);
        if unchanged && declares_nothing {
            return pending;
        }
        let mut messages = baseline;
        if let Some(slot) = messages.get_mut(index) {
            *slot = with_tool_changes(&system, added, removed).into();
        }
        return messages;
    }
    if unchanged {
        return pending;
    }
    let update = with_tool_changes(
        &SystemMessage {
            content: SystemContent::Text(String::new()),
            timestamp: bake_ai::now_ms(),
            ..SystemMessage::default()
        },
        added,
        removed,
    );
    let index = pending
        .iter()
        .position(|message| message.as_system().is_none())
        .unwrap_or(pending.len());
    let mut messages = pending;
    messages.insert(index, update.into());
    messages
}

/// Pi's `withToolChanges`: empty lists omit the field.
fn with_tool_changes(
    message: &SystemMessage,
    added: Vec<Tool>,
    removed: Vec<ToolReference>,
) -> SystemMessage {
    SystemMessage {
        tools_added: (!added.is_empty()).then_some(added),
        tools_removed: (!removed.is_empty()).then_some(removed),
        ..message.clone()
    }
}

fn event_partial(event: &AssistantMessageEvent) -> Option<&AssistantMessage> {
    match event {
        AssistantMessageEvent::TextStart { partial, .. }
        | AssistantMessageEvent::TextDelta { partial, .. }
        | AssistantMessageEvent::TextEnd { partial, .. }
        | AssistantMessageEvent::ThinkingStart { partial, .. }
        | AssistantMessageEvent::ThinkingDelta { partial, .. }
        | AssistantMessageEvent::ThinkingEnd { partial, .. }
        | AssistantMessageEvent::ToolCallStart { partial, .. }
        | AssistantMessageEvent::ToolCallDelta { partial, .. }
        | AssistantMessageEvent::ToolCallEnd { partial, .. } => Some(partial),
        AssistantMessageEvent::Start { .. }
        | AssistantMessageEvent::Done { .. }
        | AssistantMessageEvent::Error { .. } => None,
    }
}

/// Pi's `streamAssistantResponse`: the one place agent messages become LLM
/// messages.
async fn stream_assistant_response(
    context: &mut AgentContext,
    config: &AgentLoopConfig,
    model: &bake_ai::Model,
    reasoning: Option<ThinkingLevel>,
    signal: Option<&AbortSignal>,
    sink: &AgentEventSink,
    stream_fn: &StreamFn,
) -> Result<AssistantMessage, AgentLoopError> {
    let llm_messages = match &config.transform_context {
        Some(transform) => {
            let transformed = transform(context.messages.clone(), signal.cloned()).await;
            (config.convert_to_llm)(&transformed).await
        }
        None => (config.convert_to_llm)(&context.messages).await,
    };
    let llm_context = normalize_context(Context {
        system_prompt: None,
        messages: llm_messages,
        tools: None,
    });

    // Resolved per request, for tokens that expire during long tool runs.
    let resolved_key = match &config.get_api_key {
        Some(get_api_key) => get_api_key(&model.provider).await,
        None => None,
    };
    let mut options = config.stream_options.clone();
    options.reasoning = reasoning;
    options.base.api_key = resolved_key
        .filter(|key| !key.is_empty())
        .or_else(|| options.base.api_key.clone().filter(|key| !key.is_empty()));
    options.base.signal = signal.cloned();

    let response = stream_fn(model, &llm_context, options).map_err(AgentLoopError::Stream)?;
    let level = thinking_level(reasoning);
    let mut added_partial = false;

    while let Some(event) = response.next().await {
        match event {
            AssistantMessageEvent::Start { partial } => {
                added_partial = true;
                emit(
                    sink,
                    AgentEvent::MessageStart {
                        message: partial.into(),
                    },
                )
                .await;
            }
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => break,
            event => {
                if added_partial && let Some(partial) = event_partial(&event) {
                    let message = partial.clone().into();
                    emit(
                        sink,
                        AgentEvent::MessageUpdate {
                            message,
                            assistant_message_event: event,
                        },
                    )
                    .await;
                }
            }
        }
    }

    let mut final_message = match response.result().await {
        Some(message) => message,
        None => {
            // A provider that ends its stream without a final message is a
            // failed request, not a hang.
            let mut failed = AssistantMessage::pending(model);
            failed.stop_reason = StopReason::Error;
            failed.error_message = Some(bake_ai::RegistryError::NoResult.to_string());
            failed
        }
    };
    // Record the requested level, whichever stream function answered.
    final_message.thinking_level = Some(level);
    context.messages.push(final_message.clone().into());
    if !added_partial {
        emit(
            sink,
            AgentEvent::MessageStart {
                message: final_message.clone().into(),
            },
        )
        .await;
    }
    emit(
        sink,
        AgentEvent::MessageEnd {
            message: final_message.clone().into(),
        },
    )
    .await;
    Ok(final_message)
}

struct ExecutedToolCallBatch {
    messages: Vec<ToolResultMessage>,
    terminate: bool,
}

/// Pi's `failToolCallsFromTruncatedMessage`.
async fn fail_tool_calls_from_truncated_message(
    tool_calls: &[ToolCall],
    sink: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut messages = Vec::new();
    for tool_call in tool_calls {
        emit_tool_execution_start(tool_call, sink).await;
        let finalized = AgentToolCallOutcome {
            tool_call: tool_call.clone(),
            result: create_error_tool_result(format!(
                "Tool call \"{}\" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.",
                tool_call.name
            )),
            is_error: true,
            duration_ms: None,
        };
        emit_tool_execution_end(&finalized, sink).await;
        let message = create_tool_result_message(&finalized);
        emit_tool_result_message(&message, sink).await;
        messages.push(message);
    }
    ExecutedToolCallBatch {
        messages,
        terminate: false,
    }
}

fn find_tool<'t>(tools: &'t [Arc<AgentTool>], name: &str) -> Option<&'t Arc<AgentTool>> {
    tools.iter().find(|tool| tool.name == name)
}

/// The tool-call hooks of a config or of [`RunToolCallOptions`].
#[derive(Clone, Copy)]
struct ToolCallHooks<'h> {
    before: Option<&'h BeforeToolCall>,
    after: Option<&'h AfterToolCall>,
}

impl<'h> ToolCallHooks<'h> {
    fn of(config: &'h AgentLoopConfig) -> Self {
        Self {
            before: config.before_tool_call.as_ref(),
            after: config.after_tool_call.as_ref(),
        }
    }
}

/// Pi's `executeToolCalls`.
async fn execute_tool_calls(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[ToolCall],
    config: &AgentLoopConfig,
    signal: Option<&AbortSignal>,
    sink: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let has_sequential_tool_call = tool_calls.iter().any(|tool_call| {
        find_tool(&context.tools, &tool_call.name)
            .is_some_and(|tool| tool.execution_mode == Some(ToolExecutionMode::Sequential))
    });
    let hooks = ToolCallHooks::of(config);
    if config.tool_execution == ToolExecutionMode::Sequential || has_sequential_tool_call {
        execute_tool_calls_sequential(context, assistant_message, tool_calls, hooks, signal, sink)
            .await
    } else {
        execute_tool_calls_parallel(context, assistant_message, tool_calls, hooks, signal, sink)
            .await
    }
}

fn is_aborted(signal: Option<&AbortSignal>) -> bool {
    signal.is_some_and(AbortSignal::aborted)
}

async fn execute_tool_calls_sequential(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[ToolCall],
    hooks: ToolCallHooks<'_>,
    signal: Option<&AbortSignal>,
    sink: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut finalized_calls = Vec::new();
    let mut messages = Vec::new();

    for tool_call in tool_calls {
        emit_tool_execution_start(tool_call, sink).await;
        let preparation = prepare_tool_call(
            context,
            assistant_message,
            tool_call,
            hooks,
            signal,
            &context.tools,
        )
        .await;
        let finalized = match preparation {
            Preparation::Immediate { result, is_error } => AgentToolCallOutcome {
                tool_call: tool_call.clone(),
                result,
                is_error,
                duration_ms: None,
            },
            Preparation::Prepared { tool, args } => {
                let updates = tool_update_sink(tool_call, sink);
                let executed =
                    execute_prepared_tool_call(&tool, tool_call, &args, signal, &updates).await;
                finalize_executed_tool_call(
                    context,
                    assistant_message,
                    tool_call,
                    &args,
                    executed,
                    hooks,
                    signal,
                )
                .await
            }
        };

        emit_tool_execution_end(&finalized, sink).await;
        let message = create_tool_result_message(&finalized);
        emit_tool_result_message(&message, sink).await;
        finalized_calls.push(finalized);
        messages.push(message);

        if is_aborted(signal) {
            break;
        }
    }

    ExecutedToolCallBatch {
        messages,
        terminate: should_terminate_tool_batch(&finalized_calls),
    }
}

async fn execute_tool_calls_parallel(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[ToolCall],
    hooks: ToolCallHooks<'_>,
    signal: Option<&AbortSignal>,
    sink: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut entries: Vec<BoxFuture<'_, AgentToolCallOutcome>> = Vec::new();

    for tool_call in tool_calls {
        emit_tool_execution_start(tool_call, sink).await;
        let preparation = prepare_tool_call(
            context,
            assistant_message,
            tool_call,
            hooks,
            signal,
            &context.tools,
        )
        .await;
        match preparation {
            Preparation::Immediate { result, is_error } => {
                let finalized = AgentToolCallOutcome {
                    tool_call: tool_call.clone(),
                    result,
                    is_error,
                    duration_ms: None,
                };
                emit_tool_execution_end(&finalized, sink).await;
                entries.push(Box::pin(std::future::ready(finalized)));
            }
            Preparation::Prepared { tool, args } => {
                // Runs only once every call is prepared, as Pi's deferred
                // closures do.
                entries.push(Box::pin(async move {
                    if is_aborted(signal) {
                        let finalized = AgentToolCallOutcome {
                            tool_call: tool_call.clone(),
                            result: create_error_tool_result("Operation aborted"),
                            is_error: true,
                            duration_ms: None,
                        };
                        emit_tool_execution_end(&finalized, sink).await;
                        return finalized;
                    }
                    let updates = tool_update_sink(tool_call, sink);
                    let executed =
                        execute_prepared_tool_call(&tool, tool_call, &args, signal, &updates).await;
                    let finalized = finalize_executed_tool_call(
                        context,
                        assistant_message,
                        tool_call,
                        &args,
                        executed,
                        hooks,
                        signal,
                    )
                    .await;
                    emit_tool_execution_end(&finalized, sink).await;
                    finalized
                }));
            }
        }
        if is_aborted(signal) {
            break;
        }
    }

    let ordered = join_all(entries).await;
    let mut messages = Vec::new();
    for finalized in &ordered {
        let message = create_tool_result_message(finalized);
        emit_tool_result_message(&message, sink).await;
        messages.push(message);
    }
    ExecutedToolCallBatch {
        messages,
        terminate: should_terminate_tool_batch(&ordered),
    }
}

/// Polls every future on the current task and returns their outputs in
/// order, Pi's `Promise.all`.
async fn join_all<T>(mut futures: Vec<BoxFuture<'_, T>>) -> Vec<T> {
    let mut outputs: Vec<Option<T>> = futures.iter().map(|_| None).collect();
    poll_fn(|cx| {
        let mut pending = false;
        for (future, output) in futures.iter_mut().zip(outputs.iter_mut()) {
            if output.is_some() {
                continue;
            }
            match future.as_mut().poll(cx) {
                Poll::Ready(value) => *output = Some(value),
                Poll::Pending => pending = true,
            }
        }
        if pending {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    })
    .await;
    outputs.into_iter().flatten().collect()
}

// Short-lived and never stored in bulk.
#[allow(clippy::large_enum_variant)]
enum Preparation {
    Prepared {
        tool: Arc<AgentTool>,
        args: Value,
    },
    Immediate {
        result: AgentToolResult,
        is_error: bool,
    },
}

fn immediate_error(message: impl Into<String>) -> Preparation {
    Preparation::Immediate {
        result: create_error_tool_result(message),
        is_error: true,
    }
}

fn should_terminate_tool_batch(finalized_calls: &[AgentToolCallOutcome]) -> bool {
    !finalized_calls.is_empty() && finalized_calls.iter().all(|call| call.result.terminate)
}

/// The message of a caught panic.
fn panic_message(payload: Box<dyn Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => match payload.downcast::<&'static str>() {
            Ok(message) => (*message).to_owned(),
            Err(_) => "panicked".to_owned(),
        },
    }
}

/// A future whose panic becomes an `Err` with the panic's message.
struct CatchUnwind<'a, T>(BoxFuture<'a, T>);

impl<T> Future for CatchUnwind<'_, T> {
    type Output = Result<T, String>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let future = self.0.as_mut();
        match catch_unwind(AssertUnwindSafe(|| future.poll(cx))) {
            Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
            Ok(Poll::Pending) => Poll::Pending,
            Err(payload) => Poll::Ready(Err(panic_message(payload))),
        }
    }
}

/// Runs `future`, turning a panic into `Err`; `Err` results pass through.
pub(crate) async fn catching<T>(future: BoxFuture<'_, Result<T, String>>) -> Result<T, String> {
    CatchUnwind(future).await.and_then(|result| result)
}

/// Calls a hook and runs the future it returns, turning a panic in either
/// the call or the future into `Err`.
async fn calling<'a, T>(
    call: impl FnOnce() -> BoxFuture<'a, Result<T, String>>,
) -> Result<T, String> {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(future) => catching(future).await,
        Err(payload) => Err(panic_message(payload)),
    }
}

/// Pi's `prepareToolCall`: finds the tool, prepares and validates its
/// arguments, and runs `beforeToolCall`.
async fn prepare_tool_call(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_call: &ToolCall,
    hooks: ToolCallHooks<'_>,
    signal: Option<&AbortSignal>,
    tools: &[Arc<AgentTool>],
) -> Preparation {
    let Some(tool) = find_tool(tools, &tool_call.name) else {
        return immediate_error(format!("Tool {} not found", tool_call.name));
    };

    let raw = Value::Object(tool_call.arguments.clone());
    let prepared = match &tool.prepare_arguments {
        Some(prepare) => match catch_unwind(AssertUnwindSafe(|| prepare(raw))) {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(error)) => return immediate_error(error),
            Err(payload) => return immediate_error(panic_message(payload)),
        },
        None => raw,
    };
    let mut args = match validate_tool_arguments(&tool.parameters, &tool_call.name, &prepared) {
        Ok(args) => args,
        Err(error) => return immediate_error(error),
    };

    if let Some(before) = hooks.before {
        let outcome = calling(|| {
            before(
                BeforeToolCallContext {
                    assistant_message,
                    tool_call,
                    args: &mut args,
                    context,
                },
                signal.cloned(),
            )
        })
        .await;
        let before_result = match outcome {
            Ok(result) => result,
            Err(error) => return immediate_error(error),
        };
        if is_aborted(signal) {
            return immediate_error("Operation aborted");
        }
        if let Some(before_result) = before_result.filter(|result| result.block) {
            let mut result = create_error_tool_result(
                before_result
                    .reason
                    .filter(|reason| !reason.is_empty())
                    .unwrap_or_else(|| "Tool execution was blocked".to_owned()),
            );
            result.terminate = before_result.terminate;
            return Preparation::Immediate {
                result,
                is_error: true,
            };
        }
    }
    if is_aborted(signal) {
        return immediate_error("Operation aborted");
    }
    Preparation::Prepared {
        tool: Arc::clone(tool),
        args,
    }
}

/// Receives a tool's partial results.
type ToolUpdateSink<'s> = dyn Fn(AgentToolResult) -> BoxFuture<'s, ()> + Send + Sync + 's;

fn tool_update_sink<'s>(
    tool_call: &'s ToolCall,
    sink: &'s AgentEventSink,
) -> Box<ToolUpdateSink<'s>> {
    Box::new(move |partial_result| {
        Box::pin(emit(
            sink,
            AgentEvent::ToolExecutionUpdate {
                tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(),
                args: Value::Object(tool_call.arguments.clone()),
                partial_result,
            },
        ))
    })
}

struct ExecutedToolCallOutcome {
    result: AgentToolResult,
    is_error: bool,
    duration_ms: u64,
}

fn elapsed_ms(started: tokio::time::Instant) -> u64 {
    let millis = started.elapsed().as_secs_f64() * 1000.0;
    // Pi rounds `performance.now()` differences.
    millis.round() as u64
}

/// Pi's `executePreparedToolCall`: runs the tool, forwards its updates until
/// it settles, and ignores updates after that.
async fn execute_prepared_tool_call(
    tool: &AgentTool,
    tool_call: &ToolCall,
    args: &Value,
    signal: Option<&AbortSignal>,
    on_update: &ToolUpdateSink<'_>,
) -> ExecutedToolCallOutcome {
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel::<AgentToolResult>();
    let accepting = Arc::new(AtomicBool::new(true));
    let callback = {
        let accepting = Arc::clone(&accepting);
        AgentToolUpdateCallback::new(move |partial| {
            if accepting.load(Ordering::SeqCst) {
                // The receiver closes once the tool settles; a late update
                // is ignored.
                let _ = sender.send(partial);
            }
        })
    };
    let mut emitting: Option<BoxFuture<'_, ()>> = None;
    let started = tokio::time::Instant::now();
    let execute = Arc::clone(&tool.execute);
    let future = catch_unwind(AssertUnwindSafe(|| {
        execute(
            tool_call.id.clone(),
            args.clone(),
            signal.cloned(),
            callback,
        )
    }));
    let outcome = match future {
        Ok(future) => {
            let mut running = CatchUnwind(future);
            loop {
                // At most one update is emitted at a time, in the order the
                // tool sent them, while the tool keeps running.
                if emitting.is_none()
                    && let Ok(update) = updates.try_recv()
                {
                    emitting = Some(on_update(update));
                }
                tokio::select! {
                    biased;
                    () = async {
                        if let Some(emit) = emitting.as_mut() {
                            emit.await;
                        }
                    }, if emitting.is_some() => emitting = None,
                    Some(update) = updates.recv(), if emitting.is_none() => {
                        emitting = Some(on_update(update));
                    }
                    outcome = &mut running => break outcome.and_then(|result| result),
                }
            }
        }
        Err(payload) => Err(panic_message(payload)),
    };
    // As in Pi, the duration ends when the tool settles, before the updates
    // it sent are done emitting.
    let duration_ms = elapsed_ms(started);
    accepting.store(false, Ordering::SeqCst);
    updates.close();
    if let Some(emit) = emitting.take() {
        emit.await;
    }
    while let Ok(update) = updates.try_recv() {
        on_update(update).await;
    }
    match outcome {
        Ok(result) => {
            let is_error = result.is_error;
            ExecutedToolCallOutcome {
                result,
                is_error,
                duration_ms,
            }
        }
        Err(error) => ExecutedToolCallOutcome {
            result: create_error_tool_result(error),
            is_error: true,
            duration_ms,
        },
    }
}

/// Pi's `finalizeExecutedToolCall`: applies `afterToolCall`.
async fn finalize_executed_tool_call(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_call: &ToolCall,
    args: &Value,
    executed: ExecutedToolCallOutcome,
    hooks: ToolCallHooks<'_>,
    signal: Option<&AbortSignal>,
) -> AgentToolCallOutcome {
    let mut result = executed.result;
    let mut is_error = executed.is_error;

    if let Some(after) = hooks.after {
        let outcome = calling(|| {
            after(
                AfterToolCallContext {
                    assistant_message,
                    tool_call,
                    args,
                    result: &result,
                    is_error,
                    context,
                },
                signal.cloned(),
            )
        })
        .await;
        match outcome {
            Ok(Some(after_result)) => {
                // Structured content not replaced along with the content may
                // no longer match it.
                let structured_content = match after_result.structured_content {
                    Some(structured) => Some(structured),
                    None if after_result.content.is_some() => None,
                    None => result.structured_content.take(),
                };
                if let Some(content) = after_result.content {
                    result.content = content;
                }
                if let Some(details) = after_result.details {
                    result.details = Some(details);
                }
                if let Some(usage) = after_result.usage {
                    result.usage = Some(usage);
                }
                if let Some(terminate) = after_result.terminate {
                    result.terminate = terminate;
                }
                result.structured_content = structured_content;
                if let Some(flag) = after_result.is_error {
                    is_error = flag;
                }
            }
            Ok(None) => {}
            Err(error) => {
                result = create_error_tool_result(error);
                is_error = true;
            }
        }
    }

    AgentToolCallOutcome {
        tool_call: tool_call.clone(),
        result,
        is_error,
        duration_ms: Some(executed.duration_ms),
    }
}

fn create_error_tool_result(message: impl Into<String>) -> AgentToolResult {
    AgentToolResult {
        details: Some(json!({})),
        ..AgentToolResult::text(message)
    }
}

async fn emit_tool_execution_start(tool_call: &ToolCall, sink: &AgentEventSink) {
    emit(
        sink,
        AgentEvent::ToolExecutionStart {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: Value::Object(tool_call.arguments.clone()),
        },
    )
    .await;
}

async fn emit_tool_execution_end(finalized: &AgentToolCallOutcome, sink: &AgentEventSink) {
    emit(
        sink,
        AgentEvent::ToolExecutionEnd {
            tool_call_id: finalized.tool_call.id.clone(),
            tool_name: finalized.tool_call.name.clone(),
            result: finalized.result.clone(),
            is_error: finalized.is_error,
            duration_ms: finalized.duration_ms,
        },
    )
    .await;
}

fn create_tool_result_message(finalized: &AgentToolCallOutcome) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        content: finalized.result.content.clone(),
        details: finalized.result.details.clone(),
        usage: finalized.result.usage,
        nested_calls: None,
        is_error: finalized.is_error,
        timestamp: bake_ai::now_ms(),
        duration_ms: finalized.duration_ms,
    }
}

async fn emit_tool_result_message(message: &ToolResultMessage, sink: &AgentEventSink) {
    emit(
        sink,
        AgentEvent::MessageStart {
            message: message.clone().into(),
        },
    )
    .await;
    emit(
        sink,
        AgentEvent::MessageEnd {
            message: message.clone().into(),
        },
    )
    .await;
}

/// Options of [`run_tool_call`], Pi's `RunToolCallOptions`.
#[derive(Clone)]
pub struct RunToolCallOptions<'a> {
    /// Tools the call resolves against.
    pub tools: &'a [Arc<AgentTool>],
    /// Passed to the hooks as the message that issued the call.
    pub assistant_message: &'a AssistantMessage,
    /// Passed to the hooks as the current context.
    pub context: &'a AgentContext,
    /// Runs before execution.
    pub before_tool_call: Option<BeforeToolCall>,
    /// Runs after execution.
    pub after_tool_call: Option<AfterToolCall>,
    /// Cancels the call.
    pub signal: Option<AbortSignal>,
    /// Receives partial results.
    pub on_update: Option<AgentToolUpdateCallback>,
}

/// Pi's `runToolCall`: runs one call through argument preparation, schema
/// validation, `beforeToolCall`, execution, and `afterToolCall`, with no
/// events and no messages. Tools that call other tools use it so the hooks
/// apply to those calls too. Failures come back as `is_error` outcomes.
pub async fn run_tool_call(
    tool_call: &ToolCall,
    options: RunToolCallOptions<'_>,
) -> AgentToolCallOutcome {
    let hooks = ToolCallHooks {
        before: options.before_tool_call.as_ref(),
        after: options.after_tool_call.as_ref(),
    };
    let signal = options.signal.as_ref();
    let preparation = prepare_tool_call(
        options.context,
        options.assistant_message,
        tool_call,
        hooks,
        signal,
        options.tools,
    )
    .await;
    let (tool, args) = match preparation {
        Preparation::Immediate { result, is_error } => {
            return AgentToolCallOutcome {
                tool_call: tool_call.clone(),
                result,
                is_error,
                duration_ms: None,
            };
        }
        Preparation::Prepared { tool, args } => (tool, args),
    };
    let on_update = options.on_update.clone();
    let updates = move |partial: AgentToolResult| -> BoxFuture<'static, ()> {
        if let Some(on_update) = &on_update {
            on_update.update(partial);
        }
        Box::pin(async {})
    };
    let executed = execute_prepared_tool_call(&tool, tool_call, &args, signal, &updates).await;
    finalize_executed_tool_call(
        options.context,
        options.assistant_message,
        tool_call,
        &args,
        executed,
        hooks,
        signal,
    )
    .await
}
