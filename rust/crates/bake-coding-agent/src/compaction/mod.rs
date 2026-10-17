//! Context compaction for long sessions.
//!
//! Ported from Pi `packages/coding-agent/src/core/compaction/compaction.ts`
//! (v1.1.0): token estimation, cut points, [`prepare_compaction`], the
//! summarization prompts, [`compact`], file-operation tracking, and
//! [`estimate_projected_context_tokens`]. [`branch_summarization`] ports
//! `branch-summarization.ts` and [`utils`] ports `utils.ts`.
//!
//! These are pure functions over session entries and stored messages (the
//! JSON objects a session holds, read without validation as Pi reads them)
//! plus one summarization request through a [`StreamFn`]; the session
//! writes the result (see [`crate::agent_session`]). Lengths count UTF-16
//! code units, as JavaScript's `String.length` does.
//!
//! # Deviations from Pi
//!
//! - Pi falls back to `completeSimple` without a stream function; here the
//!   caller always passes one. A stream function that fails to start, or a
//!   stream that ends without a result, is an `Err`, Pi's thrown error.
//! - A message member of the wrong type counts zero characters where
//!   JavaScript would produce `NaN` or throw.

mod attempt;
pub mod branch_summarization;
pub mod utils;

pub use attempt::{SummaryAttempt, SummaryAttemptObserver};

use std::collections::BTreeMap;
use std::sync::Arc;

use bake_agent::StreamFn;
use bake_ai::utils::retry::{RetryEvent, RetryPolicy, try_retry_assistant_call};
use bake_ai::utils::text::assistant_text;
use bake_ai::{
    AbortSignal, AssistantContentBlock, AssistantMessage, CacheRetention, Context, Message, Model,
    ModelThinkingLevel, ProviderHeaders, SimpleStreamOptions, StopReason, TextContent,
    TranscriptContext, Usage, UsageCost, UserContent, UserContentBlock, UserMessage,
};
use serde::Serialize;
use serde_json::Value;

pub use bake_ai::utils::estimate::ContextUsageEstimate;

use crate::session::context::session_entry_to_context_messages;
use crate::session::json::{JsonObject, js_stringify, js_to_number, js_truthy};
use crate::session::messages::convert_to_llm;
use crate::session::{
    AgentMessage, LeafSelector, ProjectedSessionEntry, SessionEntry, SessionProjection,
    build_session_projection,
};
use utils::{
    FileOperations, SUMMARIZATION_SYSTEM_PROMPT, compute_file_lists, create_file_ops,
    extract_file_ops_from_message, format_file_operations, js_len, serialize_conversation,
};

// ============================================================================
// Types
// ============================================================================

/// Pi's `CompactionResult`: what the session records. The session manager
/// adds the entry id and parent.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionResult {
    /// The summary.
    pub summary: String,
    /// The first entry kept after the summary.
    pub first_kept_entry_id: String,
    /// Estimated context tokens before compaction.
    pub tokens_before: u64,
    /// Estimated context tokens after compaction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_tokens_after: Option<u64>,
    /// Usage of the summarization requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// `{ readFiles, modifiedFiles }` for Pi's own summaries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// Pi's `CompactionSettings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionSettings {
    /// Whether automatic compaction runs.
    pub enabled: bool,
    /// Tokens kept free for the prompt and the response.
    pub reserve_tokens: u64,
    /// Recent tokens kept unsummarized.
    pub keep_recent_tokens: u64,
}

/// Pi's `DEFAULT_COMPACTION_SETTINGS`.
pub const DEFAULT_COMPACTION_SETTINGS: CompactionSettings = CompactionSettings {
    enabled: true,
    reserve_tokens: 16384,
    keep_recent_tokens: 20000,
};

// ============================================================================
// Token calculation
// ============================================================================

/// A stored number as JavaScript arithmetic sees it; non-numbers are 0.
fn number(value: Option<&Value>) -> f64 {
    let number = js_to_number(value);
    if number.is_finite() { number } else { 0.0 }
}

fn to_tokens(value: f64) -> u64 {
    if value > 0.0 { value as u64 } else { 0 }
}

/// Pi's `calculateContextTokens`: `totalTokens`, else the sum of the
/// components.
pub fn calculate_context_tokens(usage: &Usage) -> u64 {
    bake_ai::utils::estimate::calculate_context_tokens(usage)
}

/// [`calculate_context_tokens`] over a stored usage object.
fn stored_context_tokens(usage: &Value) -> u64 {
    let total = number(usage.get("totalTokens"));
    if total != 0.0 {
        return to_tokens(total);
    }
    to_tokens(
        ["input", "output", "cacheRead", "cacheWrite"]
            .iter()
            .map(|key| number(usage.get(*key)))
            .sum(),
    )
}

/// The stored usage of an assistant message that has valid usage: not
/// aborted, not an error, and not all zero (Pi's `getAssistantUsage`).
fn assistant_usage(message: &AgentMessage) -> Option<&Value> {
    let object = message.as_json();
    if message.role() != Some("assistant") {
        return None;
    }
    let stop = object.get("stopReason").and_then(Value::as_str);
    if matches!(stop, Some("aborted" | "error")) {
        return None;
    }
    let usage = object.get("usage")?;
    (js_truthy(Some(usage)) && stored_context_tokens(usage) > 0).then_some(usage)
}

fn usage_from_json(usage: &Value) -> Usage {
    serde_json::from_value(usage.clone()).unwrap_or_else(|_| Usage {
        input: to_tokens(number(usage.get("input"))),
        output: to_tokens(number(usage.get("output"))),
        cache_read: to_tokens(number(usage.get("cacheRead"))),
        cache_write: to_tokens(number(usage.get("cacheWrite"))),
        total_tokens: to_tokens(number(usage.get("totalTokens"))),
        ..Usage::default()
    })
}

/// Pi's `getLastAssistantUsage`: the usage of the last assistant message
/// entry with valid usage.
pub fn get_last_assistant_usage(entries: &[SessionEntry]) -> Option<Usage> {
    entries.iter().rev().find_map(|entry| {
        if entry.entry_type() != "message" {
            return None;
        }
        let message = AgentMessage::from_json(entry.get("message")?.as_object()?.clone());
        assistant_usage(&message).map(usage_from_json)
    })
}

/// Pi's `estimateContextTokens`: the last valid assistant usage plus an
/// estimate of the messages after it, or an estimate of every message.
pub fn estimate_context_tokens(messages: &[AgentMessage]) -> ContextUsageEstimate {
    let usage = messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| assistant_usage(message).map(|usage| (index, usage)));
    let Some((index, usage)) = usage else {
        let estimated = messages.iter().map(estimate_tokens).sum();
        return ContextUsageEstimate {
            tokens: estimated,
            usage_tokens: 0,
            trailing_tokens: estimated,
            last_usage_index: None,
        };
    };
    let usage_tokens = stored_context_tokens(usage);
    let trailing_tokens: u64 = messages.iter().skip(index + 1).map(estimate_tokens).sum();
    ContextUsageEstimate {
        tokens: usage_tokens.saturating_add(trailing_tokens),
        usage_tokens,
        trailing_tokens,
        last_usage_index: Some(index),
    }
}

/// Pi's `estimateProjectedContextTokens`: the context estimate, without
/// trusting usage recorded before a later context edit or compaction.
pub fn estimate_projected_context_tokens(
    projection: &SessionProjection,
    branch_entries: &[SessionEntry],
) -> ContextUsageEstimate {
    let estimate = estimate_context_tokens(&projection.messages);
    if let Some(last_usage_index) = estimate.last_usage_index {
        let mut projected_index = 0;
        let mut usage_entry_id = None;
        for entry in &projection.entries {
            let next = projected_index + entry.messages.len();
            if last_usage_index < next {
                usage_entry_id = Some(entry.source_entry.id());
                break;
            }
            projected_index = next;
        }
        let usage_entry_index = usage_entry_id
            .filter(|id| !id.is_empty())
            .and_then(|id| branch_entries.iter().position(|entry| entry.id() == id));
        let latest_invalidating = branch_entries
            .iter()
            .rposition(|entry| matches!(entry.entry_type(), "context_edit" | "compaction"));
        let trusted = match (usage_entry_index, latest_invalidating) {
            (Some(usage), Some(invalidating)) => usage > invalidating,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if trusted {
            return estimate;
        }
    }
    let system = crate::session::manager::current_system_message(&projection.messages);
    let mut tokens = system
        .map(|system| estimate_tokens(&AgentMessage::from_json(system)))
        .unwrap_or(0);
    for message in &projection.messages {
        if message.role() != Some("system") {
            tokens += estimate_tokens(message);
        }
    }
    ContextUsageEstimate {
        tokens,
        usage_tokens: 0,
        trailing_tokens: tokens,
        last_usage_index: None,
    }
}

/// Pi's `shouldCompact`.
pub fn should_compact(
    context_tokens: u64,
    context_window: u64,
    settings: &CompactionSettings,
) -> bool {
    if !settings.enabled {
        return false;
    }
    i128::from(context_tokens) > i128::from(context_window) - i128::from(settings.reserve_tokens)
}

// ============================================================================
// Cut point detection
// ============================================================================

const ESTIMATED_IMAGE_CHARS: usize = 4800;

fn str_len(value: Option<&Value>) -> usize {
    value.and_then(Value::as_str).map_or(0, js_len)
}

fn content_chars(content: Option<&Value>) -> usize {
    match content {
        Some(Value::String(text)) => js_len(text),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|block| match block.get("type").and_then(Value::as_str) {
                Some("text") => str_len(block.get("text")),
                Some("image") => ESTIMATED_IMAGE_CHARS,
                _ => 0,
            })
            .sum(),
        _ => 0,
    }
}

fn chars_to_tokens(chars: usize) -> u64 {
    chars.div_ceil(4) as u64
}

/// Pi's `estimateTokens`: a conservative token estimate of one stored
/// message, characters divided by four.
pub fn estimate_tokens(message: &AgentMessage) -> u64 {
    let object = message.as_json();
    let Some(role) = message.role() else {
        return 0;
    };
    let chars = match role {
        "system" => {
            let mut chars = content_chars(object.get("content"));
            if let Some(Value::Object(sections)) = object.get("sections") {
                chars += sections
                    .values()
                    .map(|value| str_len(Some(value)))
                    .sum::<usize>();
            }
            if let Some(tools) = object
                .get("toolsAdded")
                .filter(|tools| js_truthy(Some(tools)))
            {
                chars += js_len(&js_stringify(tools));
            }
            chars
        }
        "user" | "custom" | "toolResult" => content_chars(object.get("content")),
        "assistant" => match object.get("content") {
            Some(Value::Array(blocks)) => blocks
                .iter()
                .map(|block| match block.get("type").and_then(Value::as_str) {
                    Some("text") => str_len(block.get("text")),
                    Some("thinking") => str_len(block.get("thinking")),
                    Some("toolCall") => {
                        str_len(block.get("name"))
                            + block
                                .get("arguments")
                                .map_or(0, |arguments| js_len(&js_stringify(arguments)))
                    }
                    _ => 0,
                })
                .sum(),
            _ => 0,
        },
        "bashExecution" => str_len(object.get("command")) + str_len(object.get("output")),
        "branchSummary" | "compactionSummary" => str_len(object.get("summary")),
        _ => return 0,
    };
    chars_to_tokens(chars)
}

/// [`estimate_tokens`] of a live message.
pub fn estimate_live_tokens(message: &bake_agent::AgentMessage) -> u64 {
    estimate_tokens(&AgentMessage::from(message))
}

fn is_cut_point_message(message: &AgentMessage) -> bool {
    matches!(
        message.role(),
        Some(
            "user"
                | "assistant"
                | "bashExecution"
                | "custom"
                | "branchSummary"
                | "compactionSummary"
        )
    )
}

fn is_turn_start_message(message: &AgentMessage) -> bool {
    matches!(
        message.role(),
        Some("user" | "bashExecution" | "custom" | "branchSummary" | "compactionSummary")
    )
}

fn is_turn_start_entry(entry: &SessionEntry) -> bool {
    entry.entry_type() != "compaction"
        && session_entry_to_context_messages(entry)
            .iter()
            .any(is_turn_start_message)
}

/// Pi's `findTurnStartIndex`: the entry at or before `entry_index` (and not
/// before `start_index`) that starts the turn containing it.
pub fn find_turn_start_index(
    entries: &[SessionEntry],
    entry_index: usize,
    start_index: usize,
) -> Option<usize> {
    (start_index..=entry_index)
        .rev()
        .find(|index| entries.get(*index).is_some_and(is_turn_start_entry))
}

/// Pi's `CutPointResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutPointResult {
    /// The first entry to keep.
    pub first_kept_entry_index: usize,
    /// The entry starting the turn being split; `None` when not splitting
    /// (Pi's `-1`).
    pub turn_start_index: Option<usize>,
    /// Whether the cut splits a turn.
    pub is_split_turn: bool,
}

fn entry_tokens(entry: &SessionEntry) -> u64 {
    session_entry_to_context_messages(entry)
        .iter()
        .map(estimate_tokens)
        .sum()
}

/// Pi's `findCutPoint`: walking back from the newest entry in
/// `start_index..end_index`, keep about `keep_recent_tokens` and cut at a
/// user-like or assistant message, never at a tool result.
pub fn find_cut_point(
    entries: &[SessionEntry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: u64,
) -> CutPointResult {
    let end_index = end_index.min(entries.len());
    let cut_points: Vec<usize> = (start_index..end_index)
        .filter(|index| {
            entries.get(*index).is_some_and(|entry| {
                entry.entry_type() != "compaction"
                    && session_entry_to_context_messages(entry)
                        .iter()
                        .any(is_cut_point_message)
            })
        })
        .collect();
    let (Some(&first), Some(&last)) = (cut_points.first(), cut_points.last()) else {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: None,
            is_split_turn: false,
        };
    };
    let mut accumulated = 0u64;
    let mut cut_index = first;
    for index in (start_index..end_index).rev() {
        let Some(entry) = entries.get(index) else {
            continue;
        };
        let tokens = entry_tokens(entry);
        if tokens == 0 {
            continue;
        }
        accumulated = accumulated.saturating_add(tokens);
        if accumulated >= keep_recent_tokens {
            cut_index = cut_points
                .iter()
                .copied()
                .find(|candidate| *candidate >= index)
                .unwrap_or(last);
            break;
        }
    }
    while cut_index > start_index {
        let Some(previous) = entries.get(cut_index - 1) else {
            break;
        };
        if previous.entry_type() == "compaction"
            || !session_entry_to_context_messages(previous).is_empty()
        {
            break;
        }
        cut_index -= 1;
    }
    let starts_turn = entries.get(cut_index).is_some_and(is_turn_start_entry);
    let turn_start_index = if starts_turn {
        None
    } else {
        find_turn_start_index(entries, cut_index, start_index)
    };
    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !starts_turn && turn_start_index.is_some(),
    }
}

// ============================================================================
// Summarization
// ============================================================================

const SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or \"(none)\" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or \"(none)\" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

const UPDATE_SUMMARIZATION_PROMPT: &str = "The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.

Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from \"In Progress\" to \"Done\" when completed
- UPDATE \"Next Steps\" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = "The messages above are earlier context from an ongoing conversation. Later messages are stored separately and do not need to be reconstructed.

Create a concise checkpoint of the user's request and the progress shown above. This checkpoint will be placed before the later messages so the conversation can continue with the necessary context.

## Original Request
[What did the user ask for?]

## Progress So Far
- [Key decisions and work completed in these messages]

## Context Needed to Continue
- [Information from these messages needed to understand the later work]

Only summarize information explicitly present above. Do not infer or recreate later messages.";

/// Pi's `getSummarizationFailure`: why a summarization response must not
/// become a checkpoint. A length stop holds partial text.
pub fn get_summarization_failure(response: &AssistantMessage, label: &str) -> Option<String> {
    match response.stop_reason {
        StopReason::Error => Some(format!(
            "{label} failed: {}",
            response
                .error_message
                .as_deref()
                .filter(|message| !message.is_empty())
                .unwrap_or("Unknown error")
        )),
        StopReason::Length => Some(format!(
            "{label} failed: generation hit the token cap and the summary is incomplete"
        )),
        _ => None,
    }
}

/// Observes summarization retries.
pub type RetryObserver = Arc<dyn Fn(RetryEvent) + Send + Sync>;

/// The request side of a summarization: Pi's `apiKey`, `headers`, `env`,
/// `signal`, `thinkingLevel`, `retry`, `callbacks`, and `sessionId`
/// arguments.
#[derive(Clone, Default)]
pub struct SummarizationOptions {
    /// Shared foreground allowance, including each summary and retry.
    pub run_control: Option<bake_agent::RunControl>,
    /// Calls preserved for foreground work. A speculative attempt refused
    /// by this reserve leaves the shared run open. Zero uses ordinary limits.
    pub foreground_call_reserve: u64,
    /// Observes every admitted attempt after provider cleanup, including
    /// retries and failed partial compactions. Panics are contained.
    pub on_attempt: Option<SummaryAttemptObserver>,
    /// An explicit API key; the stream function resolves one otherwise.
    pub api_key: Option<String>,
    /// Request headers.
    pub headers: Option<ProviderHeaders>,
    /// Provider-scoped environment values.
    pub env: Option<BTreeMap<String, String>>,
    /// Cancels the request and the retry backoff.
    pub signal: Option<AbortSignal>,
    /// The reasoning level; reasoning models use it unless it is `off`.
    pub thinking_level: Option<ModelThinkingLevel>,
    /// Retry of transient errors.
    pub retry: Option<RetryPolicy>,
    /// Retry reporting.
    pub on_retry: Option<RetryObserver>,
    /// A routing session id; a fresh one when `None`.
    pub session_id: Option<String>,
}

impl std::fmt::Debug for SummarizationOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SummarizationOptions")
            .field("thinking_level", &self.thinking_level)
            .field("retry", &self.retry)
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

/// Pi's `createSummarizationOptions`.
fn create_summarization_options(
    model: &Model,
    max_tokens: u64,
    options: &SummarizationOptions,
) -> SimpleStreamOptions {
    let mut request = SimpleStreamOptions::default();
    request.base.max_tokens = Some(max_tokens);
    request.base.signal = options.signal.clone();
    request.base.api_key = options.api_key.clone();
    request.base.headers = options.headers.clone();
    request.base.env = options.env.clone();
    request.base.session_id = options.session_id.clone();
    if model.reasoning {
        request.reasoning = options.thinking_level.and_then(ModelThinkingLevel::level);
    }
    request
}

/// `Math.min(Math.floor(fraction * reserveTokens), maxTokens || Infinity)`.
fn summary_max_tokens(model: &Model, fraction: f64, reserve_tokens: u64) -> u64 {
    let budget = (fraction * reserve_tokens as f64).floor() as u64;
    if model.max_tokens > 0 {
        budget.min(model.max_tokens)
    } else {
        budget
    }
}

/// Pi's `completeSummarization`: one summarization request with retry of
/// transient errors. The request never writes the prompt cache and is
/// routed by the caller's session id or a fresh one.
pub async fn complete_summarization(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
    stream_fn: &StreamFn,
    retry: Option<RetryPolicy>,
    on_retry: Option<&RetryObserver>,
) -> Result<AssistantMessage, String> {
    complete_summarization_with_control(model, context, options, stream_fn, retry, on_retry, None)
        .await
}

/// Summarizes under the same logical-call allowance as the owning run.
pub async fn complete_summarization_with_control(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
    stream_fn: &StreamFn,
    retry: Option<RetryPolicy>,
    on_retry: Option<&RetryObserver>,
    control: Option<&bake_agent::RunControl>,
) -> Result<AssistantMessage, String> {
    complete_summarization_with_run(
        model,
        context,
        options,
        stream_fn,
        retry,
        on_retry,
        SummaryRun {
            control,
            ..SummaryRun::default()
        },
    )
    .await
}

#[derive(Clone, Copy, Default)]
struct SummaryRun<'a> {
    control: Option<&'a bake_agent::RunControl>,
    foreground_reserve: u64,
    on_attempt: Option<&'a SummaryAttemptObserver>,
}

impl<'a> From<&'a SummarizationOptions> for SummaryRun<'a> {
    fn from(options: &'a SummarizationOptions) -> Self {
        Self {
            control: options.run_control.as_ref(),
            foreground_reserve: options.foreground_call_reserve,
            on_attempt: options.on_attempt.as_ref(),
        }
    }
}

async fn complete_summarization_with_run(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
    stream_fn: &StreamFn,
    retry: Option<RetryPolicy>,
    on_retry: Option<&RetryObserver>,
    run: SummaryRun<'_>,
) -> Result<AssistantMessage, String> {
    let control = run.control;
    let mut request = options;
    request.base.cache_retention = Some(CacheRetention::None);
    if request.base.session_id.is_none() {
        request.base.session_id = Some(crate::session::id::uuid_v7());
    }
    let external = request.base.signal.clone();
    let controller = bake_ai::AbortController::new();
    if external.as_ref().is_some_and(AbortSignal::aborted)
        || control.is_some_and(|control| control.stopped().is_some())
    {
        controller.abort();
    }
    let signal = controller.signal();
    request.base.signal = Some(signal.clone());
    let produce = || {
        let request = request.clone();
        async move {
            if request
                .base
                .signal
                .as_ref()
                .is_some_and(AbortSignal::aborted)
            {
                return Err("This operation was aborted".to_owned());
            }
            let admission = match control {
                Some(control) if run.foreground_reserve > 0 => Some(
                    control
                        .try_admit_model_call(
                            bake_agent::ModelCallKind::Summary,
                            model,
                            run.foreground_reserve,
                        )
                        .await
                        .map_err(|outcome| outcome.to_string())?
                        .ok_or_else(|| {
                            "Background compaction preserved the foreground model-call allowance"
                                .to_owned()
                        })?,
                ),
                Some(control) => Some(
                    control
                        .admit_model_call(bake_agent::ModelCallKind::Summary, model)
                        .await
                        .map_err(|outcome| outcome.to_string())?,
                ),
                None => None,
            };
            // A mandatory debit may outlast local cancellation. It remains
            // charged, but an already-cancelled request is never dispatched.
            let dispatched = !request
                .base
                .signal
                .as_ref()
                .is_some_and(AbortSignal::aborted);
            let started = tokio::time::Instant::now();
            let result = if dispatched {
                match stream_fn(model, context, request) {
                    Ok(stream) => {
                        let result = stream.result().await;
                        if control.is_some() {
                            let producer = stream.producer_watch();
                            drop(stream);
                            producer.ended().await;
                        }
                        result
                            .ok_or_else(|| "Summarization stream ended without a result".to_owned())
                    }
                    Err(error) => Err(error),
                }
            } else {
                Err("This operation was aborted".to_owned())
            };
            attempt::observe(run.on_attempt, admission, dispatched, &result, started);
            result
        }
    };
    let request = try_retry_assistant_call(produce, retry, Some(&signal), |event| {
        if let Some(on_retry) = on_retry {
            on_retry(event);
        }
    });
    tokio::pin!(request);
    let stop = async {
        match control {
            Some(control) => {
                control.wait_stopped().await;
            }
            None => std::future::pending::<()>().await,
        }
    };
    let result = tokio::select! {
        result = &mut request => result,
        _ = stop => { controller.abort(); request.await }
        _ = async { match external.as_ref() { Some(signal) => signal.cancelled().await, None => std::future::pending::<()>().await } } => { controller.abort(); request.await }
    };
    match control.and_then(bake_agent::RunControl::stopped) {
        Some(outcome) => Err(outcome.to_string()),
        None if signal.aborted() => Err("This operation was aborted".to_owned()),
        None => result,
    }
}

/// Pi's summarization context: the system prompt and one user message.
fn build_summarization_context(prompt_text: String) -> TranscriptContext {
    bake_ai::transcript::normalize_context(Context {
        system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_owned()),
        messages: vec![Message::User(UserMessage {
            content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(
                prompt_text,
            ))]),
            timestamp: bake_ai::now_ms(),
        })],
        tools: None,
    })
}

fn llm_messages(messages: &[AgentMessage]) -> Vec<Message> {
    let live: Vec<bake_agent::AgentMessage> = messages.iter().map(AgentMessage::to_agent).collect();
    convert_to_llm(&live)
}

fn has_tool_call(response: &AssistantMessage) -> bool {
    response
        .content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
}

/// A summary and the usage of its request.
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryText {
    /// The summary text.
    pub text: String,
    /// The provider usage.
    pub usage: Usage,
}

/// Pi's `generateSummaryWithUsage`: summarize `current_messages`, or merge
/// them into `previous_summary`.
pub async fn generate_summary_with_usage(
    current_messages: &[AgentMessage],
    model: &Model,
    reserve_tokens: u64,
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
    stream_fn: &StreamFn,
    options: &SummarizationOptions,
) -> Result<SummaryText, String> {
    let max_tokens = summary_max_tokens(model, 0.8, reserve_tokens);
    let previous_summary = previous_summary.filter(|summary| !summary.is_empty());
    let mut base_prompt = if previous_summary.is_some() {
        UPDATE_SUMMARIZATION_PROMPT.to_owned()
    } else {
        SUMMARIZATION_PROMPT.to_owned()
    };
    if let Some(focus) = custom_instructions.filter(|focus| !focus.is_empty()) {
        base_prompt = format!("{base_prompt}\n\nAdditional focus: {focus}");
    }
    let conversation = serialize_conversation(&llm_messages(current_messages));
    let mut prompt = format!("<conversation>\n{conversation}\n</conversation>\n\n");
    if let Some(previous) = previous_summary {
        prompt.push_str(&format!(
            "<previous-summary>\n{previous}\n</previous-summary>\n\n"
        ));
    }
    prompt.push_str(&base_prompt);
    let response = complete_summarization_with_run(
        model,
        &build_summarization_context(prompt),
        create_summarization_options(model, max_tokens, options),
        stream_fn,
        options.retry,
        options.on_retry.as_ref(),
        options.into(),
    )
    .await?;
    if let Some(failure) = get_summarization_failure(&response, "Summarization") {
        return Err(failure);
    }
    if has_tool_call(&response) {
        return Err("Summarization attempted to call a tool".to_owned());
    }
    Ok(SummaryText {
        text: assistant_text(&response),
        usage: response.usage,
    })
}

/// Pi's `generateSummary`: [`generate_summary_with_usage`]'s text.
pub async fn generate_summary(
    current_messages: &[AgentMessage],
    model: &Model,
    reserve_tokens: u64,
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
    stream_fn: &StreamFn,
    options: &SummarizationOptions,
) -> Result<String, String> {
    generate_summary_with_usage(
        current_messages,
        model,
        reserve_tokens,
        custom_instructions,
        previous_summary,
        stream_fn,
        options,
    )
    .await
    .map(|summary| summary.text)
}

// ============================================================================
// Compaction preparation
// ============================================================================

/// Pi's `CompactionPreparation`.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionPreparation {
    /// The first entry to keep.
    pub first_kept_entry_id: String,
    /// Messages summarized and dropped.
    pub messages_to_summarize: Vec<AgentMessage>,
    /// Messages of a split turn before the cut, summarized separately.
    pub turn_prefix_messages: Vec<AgentMessage>,
    /// Whether the cut splits a turn.
    pub is_split_turn: bool,
    /// The context estimate before compaction.
    pub tokens_before: u64,
    /// The previous compaction's summary, updated rather than replaced.
    pub previous_summary: Option<String>,
    /// File operations of the summarized messages and the previous
    /// compaction.
    pub file_ops: FileOperations,
    /// The settings.
    pub settings: CompactionSettings,
}

fn is_projected_turn_start(entry: &ProjectedSessionEntry) -> bool {
    entry.source_entry.entry_type() != "compaction"
        && entry.messages.iter().any(is_turn_start_message)
}

fn find_projected_turn_start_index(
    entries: &[ProjectedSessionEntry],
    entry_index: usize,
    start_index: usize,
) -> Option<usize> {
    (start_index..=entry_index)
        .rev()
        .find(|index| entries.get(*index).is_some_and(is_projected_turn_start))
}

fn find_projected_cut_point(
    entries: &[ProjectedSessionEntry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: u64,
) -> CutPointResult {
    let cut_points: Vec<usize> = (start_index..end_index)
        .filter(|index| {
            entries.get(*index).is_some_and(|entry| {
                entry.source_entry.entry_type() != "compaction"
                    && entry.messages.iter().any(is_cut_point_message)
            })
        })
        .collect();
    let (Some(&first), Some(&last)) = (cut_points.first(), cut_points.last()) else {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: None,
            is_split_turn: false,
        };
    };
    let mut accumulated = 0u64;
    let mut exceeded_budget = false;
    let mut cut_index = first;
    for index in (start_index..end_index).rev() {
        let Some(entry) = entries.get(index) else {
            continue;
        };
        let tokens: u64 = entry.messages.iter().map(estimate_tokens).sum();
        if tokens == 0 {
            continue;
        }
        accumulated = accumulated.saturating_add(tokens);
        if accumulated >= keep_recent_tokens {
            exceeded_budget = true;
            cut_index = cut_points
                .iter()
                .copied()
                .find(|candidate| *candidate >= index)
                .unwrap_or(last);
            break;
        }
    }

    // A recovery attempt and its omission edits are context-invisible after
    // the last visible input. Advance only for a closed suffix containing an
    // omitted assistant attempt; arbitrary metadata must not move the cut
    // past unsent input.
    let suffix = entries.get(cut_index + 1..end_index).unwrap_or(&[]);
    let intrinsically_visible = |entry: &ProjectedSessionEntry| {
        entry.source_entry.entry_type() != "context_edit"
            && !session_entry_to_context_messages(&entry.source_entry).is_empty()
    };
    let omitted =
        |entry: &ProjectedSessionEntry| intrinsically_visible(entry) && entry.messages.is_empty();
    let omitted_ids: Vec<&str> = suffix
        .iter()
        .filter(|entry| omitted(entry))
        .map(|entry| entry.source_entry.id())
        .collect();
    let has_external_replacement = suffix.iter().any(|entry| {
        entry.source_entry.entry_type() == "context_edit"
            && !matches!(entry.source_entry.get("replacement"), Some(Value::Null))
            && !entry
                .source_entry
                .get("targetId")
                .and_then(Value::as_str)
                .is_some_and(|target| omitted_ids.contains(&target))
    });
    let recovery_omission_suffix = exceeded_budget
        && !has_external_replacement
        && suffix.iter().any(|entry| {
            entry.source_entry.entry_type() == "message"
                && entry.source_entry.message_role() == Some("assistant")
                && omitted(entry)
        })
        && suffix.iter().all(|entry| {
            entry.source_entry.entry_type() != "compaction"
                && (!intrinsically_visible(entry) || omitted(entry))
        });
    if recovery_omission_suffix {
        cut_index += 1;
    }

    while cut_index > start_index {
        let Some(previous) = entries.get(cut_index - 1) else {
            break;
        };
        if previous.source_entry.entry_type() == "compaction" || !previous.messages.is_empty() {
            break;
        }
        cut_index -= 1;
    }
    let starts_turn = entries.get(cut_index).is_some_and(is_projected_turn_start);
    let turn_start_index = if starts_turn {
        None
    } else {
        find_projected_turn_start_index(entries, cut_index, start_index)
    };
    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !starts_turn && turn_start_index.is_some(),
    }
}

/// Messages a projected entry contributes to a summary: none for a
/// compaction, and no system messages (prompt state the compaction entry
/// records itself).
fn summary_messages(entry: &ProjectedSessionEntry) -> Vec<AgentMessage> {
    if entry.source_entry.entry_type() == "compaction" {
        return Vec::new();
    }
    entry
        .messages
        .iter()
        .filter(|message| message.role() != Some("system"))
        .cloned()
        .collect()
}

/// Pi's `extractFileOperations`: the previous compaction's recorded files,
/// then the summarized messages' calls.
fn extract_file_operations(
    messages: &[AgentMessage],
    previous_compaction: Option<&SessionEntry>,
) -> FileOperations {
    let mut file_ops = create_file_ops();
    if let Some(previous) = previous_compaction
        && !js_truthy(previous.get("fromHook"))
        && let Some(details) = previous.get("details").filter(|d| js_truthy(Some(d)))
    {
        let strings = |key: &str| {
            details
                .get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        file_ops.read.extend(strings("readFiles"));
        file_ops.edited.extend(strings("modifiedFiles"));
    }
    for message in messages {
        extract_file_ops_from_message(message, &mut file_ops);
    }
    file_ops
}

/// Pi's `prepareCompaction`: what a compaction of the branch `path_entries`
/// summarizes and keeps, or `None` when the branch ends in a compaction or
/// nothing would be summarized.
pub fn prepare_compaction(
    path_entries: &[SessionEntry],
    settings: CompactionSettings,
) -> Option<CompactionPreparation> {
    if path_entries
        .last()
        .is_some_and(|entry| entry.entry_type() == "compaction")
    {
        return None;
    }
    let projection = build_session_projection(path_entries, LeafSelector::Last);
    let entries = &projection.entries;
    // The newest compaction is projected first; older ones retained in its
    // kept range contribute nothing.
    let previous_index = entries.iter().position(|entry| {
        entry.source_entry.entry_type() == "compaction" && !entry.messages.is_empty()
    });
    let mut previous_summary = None;
    let mut boundary_start = 0;
    if let Some(index) = previous_index {
        previous_summary = entries
            .get(index)
            .and_then(|entry| entry.source_entry.get("summary"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        boundary_start = index + 1;
    }
    let boundary_end = entries.len();
    let tokens_before = estimate_projected_context_tokens(&projection, path_entries).tokens;
    let cut = find_projected_cut_point(
        entries,
        boundary_start,
        boundary_end,
        settings.keep_recent_tokens,
    );
    let first_kept = entries.get(cut.first_kept_entry_index)?;
    let first_kept_entry_id = first_kept.source_entry.id();
    if first_kept_entry_id.is_empty() {
        return None;
    }
    let history_end = match (cut.is_split_turn, cut.turn_start_index) {
        (true, Some(start)) => start,
        _ => cut.first_kept_entry_index,
    };
    let range = |start: usize, end: usize| -> Vec<AgentMessage> {
        entries
            .get(start..end.max(start))
            .unwrap_or(&[])
            .iter()
            .flat_map(summary_messages)
            .collect()
    };
    let messages_to_summarize = range(boundary_start, history_end);
    let turn_prefix_messages = match (cut.is_split_turn, cut.turn_start_index) {
        (true, Some(start)) => range(start, cut.first_kept_entry_index),
        _ => Vec::new(),
    };
    if messages_to_summarize.is_empty() && turn_prefix_messages.is_empty() {
        return None;
    }
    let mut file_ops = extract_file_operations(
        &messages_to_summarize,
        previous_index
            .and_then(|index| entries.get(index))
            .map(|entry| &entry.source_entry),
    );
    if cut.is_split_turn {
        for message in &turn_prefix_messages {
            extract_file_ops_from_message(message, &mut file_ops);
        }
    }
    Some(CompactionPreparation {
        first_kept_entry_id: first_kept_entry_id.to_owned(),
        messages_to_summarize,
        turn_prefix_messages,
        is_split_turn: cut.is_split_turn,
        tokens_before,
        previous_summary,
        file_ops,
        settings,
    })
}

// ============================================================================
// Main compaction function
// ============================================================================

/// Pi's `combineUsage`.
pub fn combine_usage(first: &Usage, second: &Usage) -> Usage {
    let optional = |a: Option<u64>, b: Option<u64>| {
        (a.is_some() || b.is_some()).then(|| a.unwrap_or(0).saturating_add(b.unwrap_or(0)))
    };
    Usage {
        input: first.input.saturating_add(second.input),
        output: first.output.saturating_add(second.output),
        cache_read: first.cache_read.saturating_add(second.cache_read),
        cache_write: first.cache_write.saturating_add(second.cache_write),
        cache_write_1h: optional(first.cache_write_1h, second.cache_write_1h),
        reasoning: optional(first.reasoning, second.reasoning),
        total_tokens: first.total_tokens.saturating_add(second.total_tokens),
        cost: UsageCost {
            input: first.cost.input + second.cost.input,
            output: first.cost.output + second.cost.output,
            cache_read: first.cost.cache_read + second.cost.cache_read,
            cache_write: first.cost.cache_write + second.cost.cache_write,
            total: first.cost.total + second.cost.total,
        },
    }
}

/// Pi's `generateTurnPrefixSummary`: a checkpoint of the split turn's
/// earlier messages, with half the reserve.
async fn generate_turn_prefix_summary(
    messages: &[AgentMessage],
    model: &Model,
    reserve_tokens: u64,
    stream_fn: &StreamFn,
    options: &SummarizationOptions,
) -> Result<SummaryText, String> {
    let max_tokens = summary_max_tokens(model, 0.5, reserve_tokens);
    let conversation = serialize_conversation(&llm_messages(messages));
    let prompt = format!(
        "# Conversation\n{conversation}\n\n# Instructions\n{TURN_PREFIX_SUMMARIZATION_PROMPT}"
    );
    let response = complete_summarization_with_run(
        model,
        &build_summarization_context(prompt),
        create_summarization_options(model, max_tokens, options),
        stream_fn,
        options.retry,
        options.on_retry.as_ref(),
        options.into(),
    )
    .await?;
    if let Some(failure) = get_summarization_failure(&response, "Turn prefix summarization") {
        return Err(failure);
    }
    if has_tool_call(&response) {
        return Err("Turn prefix summarization attempted to call a tool".to_owned());
    }
    Ok(SummaryText {
        text: assistant_text(&response),
        usage: response.usage,
    })
}

/// Pi's `compact`: the summaries of a prepared compaction, merged, with the
/// file lists appended.
pub async fn compact(
    preparation: &CompactionPreparation,
    model: &Model,
    custom_instructions: Option<&str>,
    stream_fn: &StreamFn,
    options: &SummarizationOptions,
) -> Result<CompactionResult, String> {
    let reserve = preparation.settings.reserve_tokens;
    let (mut summary, usage) =
        if preparation.is_split_turn && !preparation.turn_prefix_messages.is_empty() {
            let mut history_text = preparation
                .previous_summary
                .clone()
                .unwrap_or_else(|| "No prior history.".to_owned());
            let mut history_usage = None;
            if !preparation.messages_to_summarize.is_empty() {
                let history = generate_summary_with_usage(
                    &preparation.messages_to_summarize,
                    model,
                    reserve,
                    custom_instructions,
                    preparation.previous_summary.as_deref(),
                    stream_fn,
                    options,
                )
                .await?;
                history_text = history.text;
                history_usage = Some(history.usage);
            }
            let prefix = generate_turn_prefix_summary(
                &preparation.turn_prefix_messages,
                model,
                reserve,
                stream_fn,
                options,
            )
            .await?;
            let usage = match history_usage {
                Some(history) => combine_usage(&history, &prefix.usage),
                None => prefix.usage,
            };
            (
                format!(
                    "{history_text}\n\n---\n\n**Turn Context (split turn):**\n\n{}",
                    prefix.text
                ),
                usage,
            )
        } else {
            let result = generate_summary_with_usage(
                &preparation.messages_to_summarize,
                model,
                reserve,
                custom_instructions,
                preparation.previous_summary.as_deref(),
                stream_fn,
                options,
            )
            .await?;
            (result.text, result.usage)
        };
    let (read_files, modified_files) = compute_file_lists(&preparation.file_ops);
    summary.push_str(&format_file_operations(&read_files, &modified_files));
    if preparation.first_kept_entry_id.is_empty() {
        return Err("First kept entry has no UUID - session may need migration".to_owned());
    }
    let mut details = JsonObject::new();
    details.insert("readFiles".into(), read_files.into());
    details.insert("modifiedFiles".into(), modified_files.into());
    Ok(CompactionResult {
        summary,
        first_kept_entry_id: preparation.first_kept_entry_id.clone(),
        tokens_before: preparation.tokens_before,
        estimated_tokens_after: None,
        usage: Some(usage),
        details: Some(Value::Object(details)),
    })
}

#[cfg(test)]
mod tests;
