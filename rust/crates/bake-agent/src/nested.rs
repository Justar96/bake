//! Tool calls a tool makes while it runs, Pi's `ctx.executeTool()`.
//!
//! Ported from Pi `packages/coding-agent/src/core/nested-tool-calls.ts`
//! (v1.3.0, revision `71023aec1546d6d677a65669c5665c0927ce0dd0`, MIT; see
//! the crate's `NOTICE`), with its tests.
//!
//! The agent loop serves every tool it runs a [`NestedToolExecutor`]
//! through the call's [`ToolContext`]. A nested call gets the id
//! `<caller id>/<n>` and runs through the loop's own pipeline, exactly as a
//! model-issued call: argument preparation and validation,
//! `before_tool_call`, execution, and `after_tool_call`, against the run's
//! tools. Its `tool_execution_*` events carry `parent_tool_call_id`; it
//! never enters the transcript. The calls below one model-issued call, at
//! every depth, are recorded on that call's tool result as `nested_calls`
//! (bounded by [`NESTED_CALL_LIMITS`]), and their usage is added to its
//! `usage`.
//!
//! Nothing here runs until a tool calls [`NestedToolExecutor::execute`].
//!
//! Differences from Pi:
//!
//! - Pi's session hosts the runner. Here the agent loop does, so the hooks
//!   a session installs on its agent (and the tools it activates) are the
//!   nested pipeline, and no session code runs nested calls.
//! - Exclusivity follows the loop's own rule for running a batch one call
//!   at a time: a nested call runs exclusively when the agent executes
//!   sequentially, when its tool is sequential, or, with
//!   `max_concurrent_read_tools`, when its tool is not read-only.
//! - A calling tool's nested calls cannot outlive it. When its execution
//!   settles, its executor refuses further calls, aborts the calls still
//!   running, and the loop waits for them before it finalizes the caller's
//!   result. A nested call also stops when the caller's signal aborts.
//! - Arguments that are not a JSON object (or null) fail validation without
//!   reaching the hooks, since a tool call's arguments are an object; their
//!   record keeps only their size.
//! - Error text in the record is cut at 500 characters, not UTF-16 units.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use bake_ai::{
    AbortController, AbortSignal, AssistantMessage, JsonObject, NestedToolCallRecord,
    NestedToolCalls, ToolCall, Usage, UserContentBlock,
};
use serde_json::Value;
use tokio::sync::{OnceCell, mpsc, oneshot, watch};

use crate::agent_loop::{
    ToolCallHooks, create_error_tool_result, elapsed_ms, find_tool, run_tool_call_in,
};
use crate::types::{
    AfterToolCall, AgentContext, AgentEvent, AgentEventSink, AgentTool, AgentToolCallOutcome,
    AgentToolResult, AgentToolUpdateCallback, BeforeToolCall, BoxFuture, ToolContext,
    ToolExecutionMode,
};
use crate::validation::validate_tool_arguments;

/// Limits of the nested-call record on a tool result, Pi's
/// `NESTED_CALL_LIMITS`: arguments over the per-call or total size are
/// omitted, calls beyond the count are dropped, and the record is then
/// marked incomplete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NestedCallLimits {
    /// Calls recorded per model-issued call.
    pub max_calls: usize,
    /// UTF-8 bytes of one call's arguments as JSON.
    pub max_argument_bytes_per_call: usize,
    /// UTF-8 bytes of all recorded arguments as JSON.
    pub max_argument_bytes_total: usize,
    /// Characters of a failed call's error text.
    pub max_error_chars: usize,
}

/// Pi's limits.
pub const NESTED_CALL_LIMITS: NestedCallLimits = NestedCallLimits {
    max_calls: 256,
    max_argument_bytes_per_call: 8 * 1024,
    max_argument_bytes_total: 32 * 1024,
    max_error_chars: 500,
};

/// Pi's error text for a nested call where no agent runs the tool.
pub const NESTED_CALLS_UNAVAILABLE: &str = "Nested tool calls are not available in this context";

/// The error text of a nested call made after its caller settled.
pub const NESTED_CALLER_FINISHED: &str =
    "Nested tool calls are not available after the calling tool call finished";

/// What the nested calls of one model-issued call leave on its tool result,
/// Pi's `NestedCallSummary`.
#[derive(Debug, Clone, PartialEq)]
pub struct NestedCallSummary {
    /// Becomes the result's `nested_calls`; `None` when no call was made.
    pub calls: Option<NestedToolCalls>,
    /// Summed usage of the nested results, added to the result's `usage`.
    pub usage: Option<Usage>,
}

/// Collects the nested calls of one model-issued call, including calls made
/// by nested tools, Pi's `NestedCallRecorder`.
#[derive(Debug)]
pub(crate) struct NestedCallRecorder {
    calls: Vec<NestedToolCallRecord>,
    started: Vec<Option<tokio::time::Instant>>,
    complete: bool,
    argument_bytes: usize,
    /// Summed usage of every nested result, including dropped calls.
    usage: Option<Usage>,
}

impl NestedCallRecorder {
    pub(crate) fn new() -> Self {
        Self {
            calls: Vec::new(),
            started: Vec::new(),
            complete: true,
            argument_bytes: 0,
            usage: None,
        }
    }

    /// Records a call as it starts; `None` when the call is dropped.
    pub(crate) fn start(&mut self, id: &str, name: &str, arguments: &Value) -> Option<usize> {
        if self.calls.len() >= NESTED_CALL_LIMITS.max_calls {
            self.complete = false;
            return None;
        }
        let json = serde_json::to_string(arguments).unwrap_or_default();
        let bytes = json.len();
        let mut record = NestedToolCallRecord {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments: None,
            arguments_bytes: None,
            status: "unfinished".to_owned(),
            duration_ms: None,
            error: None,
        };
        match arguments {
            Value::Object(object)
                if bytes <= NESTED_CALL_LIMITS.max_argument_bytes_per_call
                    && self.argument_bytes + bytes
                        <= NESTED_CALL_LIMITS.max_argument_bytes_total =>
            {
                record.arguments = Some(object.clone());
                self.argument_bytes += bytes;
            }
            _ => {
                record.arguments_bytes = Some(bytes as u64);
                self.complete = false;
            }
        }
        self.calls.push(record);
        self.started.push(Some(tokio::time::Instant::now()));
        Some(self.calls.len() - 1)
    }

    pub(crate) fn finish(&mut self, index: Option<usize>, is_error: bool, error_text: &str) {
        let Some(index) = index else {
            return;
        };
        let (Some(record), Some(started)) =
            (self.calls.get_mut(index), self.started.get_mut(index))
        else {
            return;
        };
        record.status = if is_error { "error" } else { "ok" }.to_owned();
        record.duration_ms = Some(started.take().map_or(0, elapsed_ms));
        if is_error && !error_text.is_empty() {
            record.error = Some(
                error_text
                    .chars()
                    .take(NESTED_CALL_LIMITS.max_error_chars)
                    .collect(),
            );
        }
    }

    pub(crate) fn add_usage(&mut self, usage: &Usage) {
        self.usage = Some(match &self.usage {
            Some(total) => total.combine(usage),
            None => *usage,
        });
    }

    pub(crate) fn total_usage(&self) -> Option<Usage> {
        self.usage
    }

    /// A copy of the record so far; `None` when no nested call was made.
    pub(crate) fn snapshot(&self) -> Option<NestedToolCalls> {
        if self.calls.is_empty() && self.complete {
            return None;
        }
        let calls = self.calls.clone();
        let complete = self.complete && calls.iter().all(|call| call.status != "unfinished");
        Some(NestedToolCalls { calls, complete })
    }
}

/// Options of [`NestedToolExecutor::execute`], Pi's `ExecuteToolOptions`.
#[derive(Debug, Clone, Default)]
pub struct NestedToolCallOptions {
    /// Cancels the nested call, besides the calling tool's signal, which
    /// always does.
    pub signal: Option<AbortSignal>,
    /// Receives the nested tool's partial results, besides the
    /// `tool_execution_update` events.
    pub on_update: Option<AgentToolUpdateCallback>,
}

/// The transcript and assistant message a batch's nested calls see, as the
/// batch's own calls do.
struct CallSite {
    context: AgentContext,
    assistant_message: AssistantMessage,
}

/// Asks the loop for its call site.
pub(crate) type CallSiteRequests = mpsc::UnboundedSender<oneshot::Sender<CallSiteSnapshot>>;

/// The loop's call site, copied for nested calls.
#[derive(Clone)]
pub(crate) struct CallSiteSnapshot(Arc<CallSite>);

impl CallSiteSnapshot {
    pub(crate) fn new(context: &AgentContext, assistant_message: &AssistantMessage) -> Self {
        Self(Arc::new(CallSite {
            context: context.clone(),
            assistant_message: assistant_message.clone(),
        }))
    }
}

/// The agent's tool pipeline, as the loop runs a batch: its tools, hooks,
/// event sink, and execution mode.
pub(crate) struct NestedPipeline {
    /// Copied from the loop on the first nested call, so a batch without
    /// one copies nothing.
    site: OnceCell<CallSiteSnapshot>,
    requests: CallSiteRequests,
    before: Option<BeforeToolCall>,
    after: Option<AfterToolCall>,
    sink: AgentEventSink,
    tool_execution: ToolExecutionMode,
    max_concurrent_read_tools: Option<NonZeroUsize>,
}

impl NestedPipeline {
    pub(crate) fn new(
        requests: CallSiteRequests,
        before: Option<BeforeToolCall>,
        after: Option<AfterToolCall>,
        sink: AgentEventSink,
        tool_execution: ToolExecutionMode,
        max_concurrent_read_tools: Option<NonZeroUsize>,
    ) -> Self {
        Self {
            site: OnceCell::new(),
            requests,
            before,
            after,
            sink,
            tool_execution,
            max_concurrent_read_tools,
        }
    }

    async fn site(&self) -> Option<Arc<CallSite>> {
        self.site
            .get_or_try_init(|| async {
                let (reply, answer) = oneshot::channel();
                self.requests.send(reply).map_err(|_| ())?;
                answer.await.map_err(|_| ())
            })
            .await
            .ok()
            .map(|snapshot| Arc::clone(&snapshot.0))
    }

    /// The loop's rule for running a call alone.
    fn is_exclusive(&self, tool: Option<&AgentTool>) -> bool {
        self.tool_execution == ToolExecutionMode::Sequential
            || match tool {
                Some(tool) => {
                    tool.execution_mode == Some(ToolExecutionMode::Sequential)
                        || (self.max_concurrent_read_tools.is_some() && !tool.read_only)
                }
                None => self.max_concurrent_read_tools.is_some(),
            }
    }

    async fn emit(&self, event: AgentEvent) {
        (self.sink)(event).await;
    }
}

/// Runs the nested calls of one batch, Pi's `NestedToolCallRunner`.
pub(crate) struct NestedToolCallRunner {
    pipeline: NestedPipeline,
    /// Serializes nested calls that must not run concurrently; FIFO, as
    /// Pi's promise chain.
    queue: Arc<tokio::sync::Mutex<()>>,
}

impl NestedToolCallRunner {
    pub(crate) fn new(pipeline: NestedPipeline) -> Arc<Self> {
        Arc::new(Self {
            pipeline,
            queue: Arc::default(),
        })
    }

    /// The executor of the model-issued call `caller_id`, whose `signal`
    /// its nested calls follow. Every call below it shares its record.
    pub(crate) fn executor(
        self: &Arc<Self>,
        caller_id: impl Into<String>,
        signal: Option<AbortSignal>,
    ) -> NestedToolExecutor {
        NestedToolExecutor {
            caller_id: caller_id.into(),
            bound: Some(Arc::new(Bound {
                runner: Arc::clone(self),
                scope: CallScope::new(Arc::new(Mutex::new(NestedCallRecorder::new())), false),
                signal,
            })),
        }
    }
}

/// Whether a scope still admits calls, and how many of its calls run.
#[derive(Debug, Default)]
struct Gate {
    closed: bool,
    running: usize,
}

/// The nested calls of one calling tool call.
struct CallScope {
    /// Shared by every call below one model-issued call.
    recorder: Arc<Mutex<NestedCallRecorder>>,
    next_id: AtomicU64,
    /// Set inside a call that holds the exclusive queue, so its own nested
    /// calls do not wait on it.
    holds_queue: bool,
    gate: watch::Sender<Gate>,
    /// Aborted when the caller settles, stopping its running calls.
    closing: AbortController,
}

impl CallScope {
    fn new(recorder: Arc<Mutex<NestedCallRecorder>>, holds_queue: bool) -> Arc<Self> {
        Arc::new(Self {
            recorder,
            next_id: AtomicU64::new(1),
            holds_queue,
            gate: watch::Sender::new(Gate::default()),
            closing: AbortController::new(),
        })
    }

    fn recorder(&self) -> std::sync::MutexGuard<'_, NestedCallRecorder> {
        self.recorder.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Admits one call, unless the scope closed.
    fn enter(self: &Arc<Self>) -> Option<Running> {
        let admitted = self.gate.send_if_modified(|gate| {
            if gate.closed {
                return false;
            }
            gate.running += 1;
            true
        });
        admitted.then(|| Running(Arc::clone(self)))
    }
}

/// One admitted call of a scope; dropping it ends the call's count.
struct Running(Arc<CallScope>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.gate.send_modify(|gate| gate.running -= 1);
    }
}

struct Bound {
    runner: Arc<NestedToolCallRunner>,
    scope: Arc<CallScope>,
    /// The calling tool's signal, the default of its nested calls.
    signal: Option<AbortSignal>,
}

/// Runs session tools on behalf of one calling tool call, the
/// `executeTool` of Pi's `ExtensionToolContext`. Reached through
/// [`ToolContext::nested`]; cheap to clone.
#[derive(Clone)]
pub struct NestedToolExecutor {
    caller_id: String,
    bound: Option<Arc<Bound>>,
}

impl std::fmt::Debug for NestedToolExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NestedToolExecutor")
            .field("caller_id", &self.caller_id)
            .field("available", &self.bound.is_some())
            .finish()
    }
}

impl NestedToolExecutor {
    /// An executor that refuses every call, for a tool run outside an agent.
    pub(crate) fn unavailable(caller_id: impl Into<String>) -> Self {
        Self {
            caller_id: caller_id.into(),
            bound: None,
        }
    }

    /// The id of the calling tool call; empty when detached.
    pub fn caller_id(&self) -> &str {
        &self.caller_id
    }

    /// The tools [`Self::execute`] can call, Pi's `ctx.tools`: the tools of
    /// the run, as the model-issued calls see them. Empty where no agent
    /// runs the caller.
    pub async fn tools(&self) -> Vec<Arc<AgentTool>> {
        match &self.bound {
            Some(bound) => match bound.runner.pipeline.site().await {
                Some(site) => site.context.tools.clone(),
                None => Vec::new(),
            },
            None => Vec::new(),
        }
    }

    /// Runs the tool `name` with `args` on behalf of the caller, Pi's
    /// `ctx.executeTool`. The call gets the id `<caller id>/<n>` and goes
    /// through the agent's tool pipeline; its events carry
    /// `parent_tool_call_id`, and its result is not added to the transcript.
    ///
    /// Never fails with `Err`: unknown tools, invalid arguments, blocked or
    /// refused calls, and tool errors come back as `is_error` outcomes
    /// carrying the error text.
    pub async fn execute(
        &self,
        name: &str,
        args: Value,
        options: NestedToolCallOptions,
    ) -> AgentToolCallOutcome {
        let Some(bound) = &self.bound else {
            return self.refused(name, NESTED_CALLS_UNAVAILABLE);
        };
        let Some(_running) = bound.scope.enter() else {
            return self.refused(name, NESTED_CALLER_FINISHED);
        };
        let id = format!(
            "{}/{}",
            self.caller_id,
            bound.scope.next_id.fetch_add(1, Ordering::SeqCst)
        );
        let args = if args.is_null() {
            Value::Object(JsonObject::new())
        } else {
            args
        };
        let record = bound.scope.recorder().start(&id, name, &args);
        let pipeline = &bound.runner.pipeline;
        pipeline
            .emit(AgentEvent::ToolExecutionStart {
                tool_call_id: id.clone(),
                tool_name: name.to_owned(),
                args: args.clone(),
                parent_tool_call_id: Some(self.caller_id.clone()),
            })
            .await;

        let outcome = self.run(bound, id, name, args, options).await;

        let error_text = text_of(&outcome.result);
        bound
            .scope
            .recorder()
            .finish(record, outcome.is_error, &error_text);
        // Nested results are not persisted, so their usage is only counted
        // through the recorder.
        if let Some(usage) = &outcome.result.usage {
            bound.scope.recorder().add_usage(usage);
        }
        pipeline
            .emit(AgentEvent::ToolExecutionEnd {
                tool_call_id: outcome.tool_call.id.clone(),
                tool_name: name.to_owned(),
                result: outcome.result.clone(),
                is_error: outcome.is_error,
                duration_ms: outcome.duration_ms,
                parent_tool_call_id: Some(self.caller_id.clone()),
            })
            .await;
        outcome
    }

    async fn run(
        &self,
        bound: &Bound,
        id: String,
        name: &str,
        args: Value,
        options: NestedToolCallOptions,
    ) -> AgentToolCallOutcome {
        let runner = &bound.runner;
        let pipeline = &runner.pipeline;
        let mut tool_call = ToolCall {
            id,
            name: name.to_owned(),
            ..ToolCall::default()
        };
        let Some(site) = pipeline.site().await else {
            return immediate(tool_call, NESTED_CALLER_FINISHED);
        };
        let tool = find_tool(&site.context.tools, name);
        tool_call.arguments = match args {
            Value::Object(arguments) => arguments,
            other => {
                let error = match tool {
                    None => format!("Tool {name} not found"),
                    Some(tool) => validate_tool_arguments(&tool.parameters, name, &other)
                        .err()
                        .unwrap_or_else(|| {
                            format!(
                                "Validation failed for tool \"{name}\": arguments must be an object"
                            )
                        }),
                };
                return immediate(tool_call, &error);
            }
        };

        // The call stops when its own signal or the caller's aborts, or when
        // the caller settles.
        let controller = AbortController::new();
        let cancelled = {
            let closing = bound.scope.closing.signal();
            let own = options.signal;
            let caller = bound.signal.clone();
            async move {
                tokio::select! {
                    () = closing.cancelled() => {}
                    () = cancelled_by(own.as_ref()) => {}
                    () = cancelled_by(caller.as_ref()) => {}
                }
            }
        };
        tokio::pin!(cancelled);

        let exclusive = !bound.scope.holds_queue && pipeline.is_exclusive(tool.map(AsRef::as_ref));
        // An aborted call skips the queue; the pipeline then refuses it.
        let _permit = if exclusive {
            tokio::select! {
                permit = Arc::clone(&runner.queue).lock_owned() => Some(permit),
                () = &mut cancelled => {
                    controller.abort();
                    None
                }
            }
        } else {
            None
        };

        let child = NestedToolExecutor {
            caller_id: tool_call.id.clone(),
            bound: Some(Arc::new(Bound {
                runner: Arc::clone(runner),
                scope: CallScope::new(
                    Arc::clone(&bound.scope.recorder),
                    bound.scope.holds_queue || exclusive,
                ),
                signal: Some(controller.signal()),
            })),
        };
        let on_update = options.on_update;
        let caller_id = self.caller_id.clone();
        let update_call = tool_call.clone();
        let updates = move |partial_result: AgentToolResult| -> BoxFuture<'_, ()> {
            if let Some(on_update) = &on_update {
                on_update.update(partial_result.clone());
            }
            Box::pin(pipeline.emit(AgentEvent::ToolExecutionUpdate {
                tool_call_id: update_call.id.clone(),
                tool_name: update_call.name.clone(),
                args: Value::Object(update_call.arguments.clone()),
                partial_result,
                parent_tool_call_id: Some(caller_id.clone()),
            }))
        };
        let signal = controller.signal();
        let running = run_tool_call_in(
            &tool_call,
            &site.context.tools,
            &site.assistant_message,
            &site.context,
            ToolCallHooks {
                before: pipeline.before.as_ref(),
                after: pipeline.after.as_ref(),
            },
            Some(&signal),
            Some(&self.caller_id),
            ToolContext::new(child),
            &updates,
        );
        tokio::pin!(running);
        if controller.signal().aborted() {
            return running.await;
        }
        tokio::select! {
            outcome = &mut running => outcome,
            () = &mut cancelled => {
                controller.abort();
                running.await
            }
        }
    }

    /// Refuses later calls, aborts the running ones, and waits for them.
    /// The loop calls it when the caller's execution settles.
    pub(crate) async fn close(&self) {
        let Some(bound) = &self.bound else {
            return;
        };
        let scope = &bound.scope;
        scope.gate.send_modify(|gate| gate.closed = true);
        scope.closing.abort();
        let mut gate = scope.gate.subscribe();
        // The sender lives in `scope`, so the wait ends only at zero.
        let _ = gate.wait_for(|gate| gate.running == 0).await;
    }

    /// Removes and returns the record of the calls below this model-issued
    /// call, Pi's `takeRecord`; `None` when no call was made.
    pub(crate) fn take_summary(&self) -> Option<NestedCallSummary> {
        let bound = self.bound.as_ref()?;
        let recorder = std::mem::replace(&mut *bound.scope.recorder(), NestedCallRecorder::new());
        let calls = recorder.snapshot();
        let usage = recorder.total_usage();
        (calls.is_some() || usage.is_some()).then_some(NestedCallSummary { calls, usage })
    }

    /// Pi's outcome for a call no executor runs: `<caller id>/0`.
    fn refused(&self, name: &str, text: &str) -> AgentToolCallOutcome {
        immediate(
            ToolCall {
                id: format!("{}/0", self.caller_id),
                name: name.to_owned(),
                ..ToolCall::default()
            },
            text,
        )
    }
}

/// Completes when `signal` aborts; never without one.
async fn cancelled_by(signal: Option<&AbortSignal>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending().await,
    }
}

fn immediate(tool_call: ToolCall, text: &str) -> AgentToolCallOutcome {
    AgentToolCallOutcome {
        tool_call,
        result: create_error_tool_result(text),
        is_error: true,
        duration_ms: None,
        nested: None,
    }
}

/// The text blocks of `result`, joined by newlines.
fn text_of(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "nested_tests.rs"]
mod tests;
