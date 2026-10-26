//! Cached compaction: the summary request repeats the conversation the
//! provider has cached, as the agent would send it next, and asks for the
//! summary as its next turn.
//!
//! Adapted from pi-cached-compaction 0.2.0
//! (`e1e20de0a7e433070414602f1105cdcac258ef4f`, MIT, Copyright (c) Nico
//! Bailon): `cached-request.ts` (`summaryPayload` with its prefix check,
//! `keptStart`, `summaryInstruction`, and `checkpointSummary`) and
//! `cache-state.ts` (`cacheState`), with their tests. The session captures
//! the agent's last request and supplies the conversation
//! ([`crate::agent_session`]); `compaction.cachedSummary` turns it on.
//!
//! The request keeps the agent's system prompt, tools, messages, model, and
//! request options, so the provider reads nearly all of it from the prompt
//! cache and the summary sees every tool result in full, where Pi's summary
//! ([`super::compact`]) sends the older messages as text under its own
//! prompt, with each tool result cut to 2,000 characters, and reads no
//! cache. One user message is added: [`summary_instruction`], which quotes
//! the first kept message so that the summary stops where the kept messages
//! begin. Before it is sent, the payload must repeat the captured payload
//! of the agent's last request ([`summary_payload`]); when it does not, or
//! anything else rules the cache out, nothing is sent and Pi's summary
//! runs ([`CachedSummary::Fallback`]).
//!
//! # Deviations from pi-cached-compaction
//!
//! - The conversation is the session's projection converted as the agent
//!   converts it, rather than the captured transcript and the entries after
//!   it, so the payload check also proves that the conversation only grew
//!   since the captured request.
//! - Kept messages are counted without system messages, which carry the
//!   prompt and tools rather than conversation.
//! - The request is not retried; a failure falls back to Pi's summary,
//!   which is.
//! - Not ported: the compaction card and its estimate of Pi's cost, the
//!   trace log, the cache countdown, compacting before the cache expires,
//!   the longer cache retention, the 12-hour GPT prefix, Kimi's cache
//!   option, `tool_choice: "none"`, the Claude bridge fork, and the
//!   `/cached-compaction` command. The lifetime of a cache is the model's
//!   declared one or the tier the provider reports; OpenAI's 30-minute
//!   minimum and Kimi's options are not assumed.

use std::sync::{Arc, Mutex, PoisonError};

use bake_agent::{RunControl, StreamFn};
use bake_ai::utils::text::{assistant_text, user_content_text};
use bake_ai::{
    AbortSignal, AssistantContentBlock, Context, Message, Model, SimpleStreamOptions, StopReason,
    TextContent, UserContent, UserContentBlock, UserMessage,
};
use indexmap::IndexSet;
use serde_json::{Map, Value};

use super::utils::{FileOperations, compute_file_lists, format_file_operations, strip_file_lists};
use super::{
    CompactionPreparation, CompactionResult, PayloadGate, SummaryRun,
    complete_summarization_with_run, has_tool_call,
};
use crate::session::SessionEntry;
use crate::session::json::{JsonObject, js_stringify_object};

/// The payload members that hold a request's messages: Anthropic's and
/// Chat Completions' `messages`, and the Responses API's `input`.
const MESSAGE_ARRAY_KEYS: [&str; 2] = ["messages", "input"];

/// Output limits, which the summary request may set differently.
const OUTPUT_LIMIT_KEYS: [&str; 3] = ["max_tokens", "max_output_tokens", "max_completion_tokens"];

/// The UTF-16 code units of a kept message the instruction quotes.
const PREVIEW_LENGTH: usize = 400;

/// The member of `payload` that holds its messages, when it has one.
pub fn message_array_key(payload: &Value) -> Option<&'static str> {
    MESSAGE_ARRAY_KEYS
        .into_iter()
        .find(|key| payload.get(*key).is_some_and(Value::is_array))
}

/// `value` without its `cache_control` members at any depth, compactly
/// serialized: a provider moves its cache breakpoint to the newest message
/// on every request. A missing value is `undefined`, as JavaScript's
/// `JSON.stringify(undefined) ?? "undefined"` reads.
fn canonical(value: Option<&Value>) -> String {
    fn strip(value: &Value) -> Value {
        match value {
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .filter(|(key, _)| *key != "cache_control")
                    .map(|(key, value)| (key.clone(), strip(value)))
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(strip).collect()),
            other => other.clone(),
        }
    }
    value.map_or_else(|| "undefined".to_owned(), |value| strip(value).to_string())
}

/// Why a summary request does not repeat the captured request, with up to
/// 200 characters of each around the first difference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixMismatch {
    /// The reason, such as `message 3 differs`.
    pub reason: String,
    /// The captured request around the difference.
    pub captured: Option<String>,
    /// The summary request around the difference.
    pub rebuilt: Option<String>,
}

impl PrefixMismatch {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            captured: None,
            rebuilt: None,
        }
    }
}

fn difference(reason: String, captured: &str, rebuilt: &str) -> PrefixMismatch {
    let at = captured
        .chars()
        .zip(rebuilt.chars())
        .take_while(|(was, now)| was == now)
        .count();
    let from = at.saturating_sub(40);
    let excerpt = |text: &str| text.chars().skip(from).take(at + 160 - from).collect();
    PrefixMismatch {
        reason,
        captured: Some(excerpt(captured)),
        rebuilt: Some(excerpt(rebuilt)),
    }
}

/// Whether a captured message is the reasoning effort Pi's serializers put
/// after the cache breakpoint, which the next request moves after its new
/// turns.
fn is_trailing_effort(item: &Value) -> bool {
    item.as_object().is_some_and(|item| {
        item.get("role").and_then(Value::as_str) == Some("system")
            || item.get("type").and_then(Value::as_str) == Some("configuration_update")
    })
}

/// Where `rebuilt` stops repeating `captured`. Fields `rebuilt` lacks are
/// skipped; [`summary_payload`] copies them from `captured`.
fn prefix_mismatch(
    captured: &Map<String, Value>,
    rebuilt: &Map<String, Value>,
    key: &str,
    cache_neutral: &[&str],
) -> Option<PrefixMismatch> {
    let fields: IndexSet<&String> = captured.keys().chain(rebuilt.keys()).collect();
    for field in fields {
        if field == key
            || OUTPUT_LIMIT_KEYS.contains(&field.as_str())
            || cache_neutral.contains(&field.as_str())
            || !rebuilt.contains_key(field)
        {
            continue;
        }
        let (was, now) = (
            canonical(captured.get(field)),
            canonical(rebuilt.get(field)),
        );
        if was != now {
            return Some(difference(
                format!("request field \"{field}\" differs"),
                &was,
                &now,
            ));
        }
    }
    let Some(before) = captured.get(key).and_then(Value::as_array) else {
        return Some(PrefixMismatch::new(format!(
            "the cached request has no \"{key}\" array"
        )));
    };
    let Some(after) = rebuilt.get(key).and_then(Value::as_array) else {
        return Some(PrefixMismatch::new(format!(
            "request has no \"{key}\" array"
        )));
    };
    let mut end = before.len();
    while end > 0 && before.get(end - 1).is_some_and(is_trailing_effort) {
        end -= 1;
    }
    if after.len() <= end {
        return Some(PrefixMismatch::new(
            "request is shorter than the cached one",
        ));
    }
    before
        .iter()
        .zip(after)
        .take(end)
        .enumerate()
        .find_map(|(index, (was, now))| {
            let (was, now) = (canonical(Some(was)), canonical(Some(now)));
            (was != now).then(|| difference(format!("message {index} differs"), &was, &now))
        })
}

/// pi-cached-compaction's `summaryPayload`: whether the summary request's
/// payload `params` repeats `captured`, the payload of the agent's last
/// request whose messages are under `key`. Every member must be the same,
/// output limits and `cache_neutral` members aside, and the messages must
/// extend the captured ones. A member only `captured` has, which an
/// `on_payload` hook added to the agent's request, is copied over: the
/// result is `Ok(Some(payload))` then, and `Ok(None)` when `params` is sent
/// as built.
pub fn summary_payload(
    params: &Value,
    captured: &Value,
    key: &str,
    cache_neutral: &[&str],
) -> Result<Option<Value>, PrefixMismatch> {
    let Some(rebuilt) = params.as_object() else {
        return Err(PrefixMismatch::new("the provider payload is not an object"));
    };
    let Some(captured) = captured.as_object() else {
        return Err(PrefixMismatch::new("the cached payload is not an object"));
    };
    if let Some(mismatch) = prefix_mismatch(captured, rebuilt, key, cache_neutral) {
        return Err(mismatch);
    }
    let missing: Vec<(&String, &Value)> = captured
        .iter()
        .filter(|(field, _)| !rebuilt.contains_key(*field))
        .collect();
    if missing.is_empty() {
        return Ok(None);
    }
    let mut payload = rebuilt.clone();
    for (field, value) in missing {
        payload.insert(field.clone(), value.clone());
    }
    Ok(Some(Value::Object(payload)))
}

/// The start of `text` no longer than `length` UTF-16 code units, with `…`
/// when it was cut, as JavaScript's `slice` cuts.
fn preview(text: &str, length: usize) -> String {
    if text.encode_utf16().count() <= length {
        return text.to_owned();
    }
    let mut units = 0;
    let mut cut: String = text
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= length
        })
        .collect();
    cut.push('…');
    cut
}

/// What the instruction quotes of a message: a user message's text, or an
/// assistant message's text and tool calls, written `[tool call: name
/// arguments]`, cut to 400 UTF-16 code units. `None` for a message with
/// nothing to quote.
pub fn message_preview(message: &Message) -> Option<String> {
    let text = match message {
        Message::User(user) => user_content_text(&user.content, "\n"),
        Message::Assistant(assistant) => assistant
            .content
            .iter()
            .filter_map(|block| match block {
                AssistantContentBlock::Text(text) => Some(text.text.clone()),
                AssistantContentBlock::ToolCall(call) => Some(format!(
                    "[tool call: {} {}]",
                    call.name,
                    js_stringify_object(&call.arguments)
                )),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Message::System(_) | Message::ToolResult(_) => return None,
    };
    let text = text.trim();
    (!text.is_empty()).then(|| preview(text, PREVIEW_LENGTH))
}

/// Where the kept messages start, as the instruction quotes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptStart {
    /// The first kept message's preview.
    pub text: String,
    /// Which of the kept messages that begin with `text` it is, counting
    /// back from the end.
    pub from_end: usize,
    /// How many messages are kept.
    pub count: usize,
}

/// pi-cached-compaction's `keptStart`: the first of the last `kept`
/// non-system messages of `conversation`, when it has text to quote.
pub fn kept_start(conversation: &[Message], kept: usize) -> Option<KeptStart> {
    let messages: Vec<&Message> = conversation
        .iter()
        .filter(|message| !matches!(message, Message::System(_)))
        .collect();
    let start = messages.len().checked_sub(kept).filter(|_| kept > 0)?;
    let previews: Vec<Option<String>> = messages[start..]
        .iter()
        .map(|message| message_preview(message))
        .collect();
    let text = previews.first().cloned().flatten()?;
    let from_end = previews
        .iter()
        .filter(|preview| preview.as_deref() == Some(text.as_str()))
        .count();
    Some(KeptStart {
        text,
        from_end,
        count: previews.len(),
    })
}

/// pi-cached-compaction's `summaryInstruction`: Pi's checkpoint format,
/// to be written as of the message before `kept`, folding in an earlier
/// summary, with `custom_instructions` as its focus.
pub fn summary_instruction(kept: &KeptStart, custom_instructions: Option<&str>) -> String {
    let repeated = if kept.from_end > 1 {
        format!(
            " Several kept messages begin with this text: it is number {} of them, counting back from the end.",
            kept.from_end
        )
    } else {
        String::new()
    };
    let tool_calls = if kept.text.contains("[tool call: ") {
        " Tool calls are written as [tool call: name arguments]."
    } else {
        ""
    };
    let focus = custom_instructions
        .map(str::trim)
        .filter(|focus| !focus.is_empty())
        .map(|focus| format!("\n\nAdditional focus: {focus}"))
        .unwrap_or_default();
    format!(
        "Stop working on the task and do not call any tools. Write a structured context checkpoint summary of the earlier part of the conversation above. Another LLM will continue the work from your summary, followed by the last {count} messages of the conversation, which stay in context verbatim.

The kept messages start with the message that begins with the text below.{repeated}{tool_calls}

<kept-start>
{text}
</kept-start>

Summarize only the messages before it, as if the conversation ended right there. Done lists only work finished before it. Whatever happens from that message on, including its tool calls, their results, and later replies, has not happened as far as your summary knows: leave it out, or list it under Next Steps if it was already planned.

If the conversation starts with an earlier summary, fold it into the new one: keep its facts, move finished items to Done, and update Next Steps.

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

Keep each section concise. Preserve exact file paths, function names, and error messages. Reply with the summary only, and stop it right before the message that begins with the <kept-start> text.{focus}",
        count = kept.count,
        text = kept.text,
    )
}

/// pi-cached-compaction's `checkpointSummary`: the model's text without
/// file lists it copied, then one set of lists from `file_ops`, which
/// already holds the previous compaction's. Returns the summary and the
/// read and modified files.
pub fn checkpoint_summary(
    text: &str,
    file_ops: &FileOperations,
) -> (String, Vec<String>, Vec<String>) {
    let mut summary = strip_file_lists(text).unwrap_or_else(|| text.trim().to_owned());
    let (read_files, modified_files) = compute_file_lists(file_ops);
    summary.push_str(&format_file_operations(&read_files, &modified_files));
    (summary, read_files, modified_files)
}

/// How long a provider keeps a prompt cache, and who says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheLifetime {
    /// Milliseconds after the request that wrote or read it.
    pub ms: i64,
    /// `declared by the model` or `reported by the provider`.
    pub source: &'static str,
}

/// pi-cached-compaction's `CacheState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheState {
    /// The cache's lifetime, when known.
    pub lifetime: Option<CacheLifetime>,
    /// Whether the last response since the latest compaction read or
    /// wrote the cache.
    pub cached: bool,
    /// When the cache expires, when its lifetime is known and it is cached.
    pub expires_at: Option<i64>,
    /// Whether a response came back since the latest compaction.
    pub replied: bool,
}

/// The cache tier the provider reports writing, Anthropic's split of
/// `cacheWrite` into its 1-hour part.
const REPORTED_TIERS: [(&str, i64); 2] = [("short", 300_000), ("long", 3_600_000)];

/// pi-cached-compaction's `cacheState` over the branch `entries`: the
/// cache is refreshed by each response that read or wrote it and reset by
/// a compaction; a failed response, which may not have reached the cache,
/// changes nothing but the tier it reports. The lifetime is the model's
/// `promptCache` figure for the reported tier, or for `long` or `short`
/// retention when none was reported, else the reported tier's.
pub fn cache_state(model: &Model, entries: &[SessionEntry], long_retention: bool) -> CacheState {
    let mut refreshed_at = None;
    let mut reported = None;
    let mut replied = false;
    for entry in entries {
        if entry.entry_type() == "compaction" {
            refreshed_at = None;
            reported = None;
            replied = false;
            continue;
        }
        if entry.entry_type() != "message" || entry.message_role() != Some("assistant") {
            continue;
        }
        let message = entry.get("message");
        let usage = message.and_then(|message| message.get("usage"));
        let count = |key: &str| {
            usage
                .and_then(|usage| usage.get(key))
                .and_then(Value::as_f64)
                .filter(|count| count.is_finite())
                .unwrap_or(0.0)
        };
        let (read, written) = (count("cacheRead"), count("cacheWrite"));
        if written > 0.0
            && let Some(long) = usage
                .and_then(|usage| usage.get("cacheWrite1h"))
                .and_then(Value::as_f64)
        {
            reported = Some(if long > 0.0 { "long" } else { "short" });
        }
        let stop = message
            .and_then(|message| message.get("stopReason"))
            .and_then(Value::as_str);
        if matches!(stop, Some("error" | "aborted")) {
            continue;
        }
        replied = true;
        refreshed_at = (read + written > 0.0)
            .then(|| {
                message
                    .and_then(|message| message.get("timestamp"))
                    .and_then(Value::as_f64)
                    .filter(|stamp| stamp.is_finite())
                    .map(|stamp| stamp as i64)
            })
            .flatten();
    }
    let tier = reported.unwrap_or(if long_retention { "long" } else { "short" });
    let declared = model
        .prompt_cache
        .as_ref()
        .and_then(|cache| cache.get(tier))
        .and_then(Value::as_f64)
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0);
    let lifetime = match (declared, reported) {
        (Some(seconds), _) => Some(CacheLifetime {
            ms: (seconds * 1000.0) as i64,
            source: "declared by the model",
        }),
        (None, Some(tier)) => {
            REPORTED_TIERS
                .iter()
                .find(|(name, _)| *name == tier)
                .map(|(_, ms)| CacheLifetime {
                    ms: *ms,
                    source: "reported by the provider",
                })
        }
        (None, None) => None,
    };
    CacheState {
        lifetime,
        cached: refreshed_at.is_some(),
        expires_at: lifetime
            .zip(refreshed_at)
            .map(|(lifetime, at)| at.saturating_add(lifetime.ms)),
        replied,
    }
}

/// What a cached summary works from.
pub struct CachedRequest {
    /// The payload of the agent's last request, as sent.
    pub payload: Arc<Value>,
    /// The options of that request; its signal and key are not used.
    pub options: SimpleStreamOptions,
    /// The conversation as the agent would send it next, system messages
    /// included.
    pub conversation: Vec<Message>,
    /// How many non-system messages at the conversation's end the
    /// compaction keeps.
    pub kept: usize,
}

/// How a cached summary ended.
#[derive(Debug, Clone, PartialEq)]
pub enum CachedSummary {
    /// The compaction, written from the cache.
    Done(Box<CompactionResult>),
    /// Nothing was summarized from the cache, for this reason; Pi's summary
    /// runs instead.
    Fallback(String),
    /// The compaction was cancelled.
    Cancelled,
    /// `compaction.cachedSummary` is off; Pi's summary runs.
    Off,
}

/// Summarizes the conversation of `request` for `preparation` in one
/// request that repeats the agent's last one, as [`super::compact`] would
/// return it. `signal` cancels it, and `control` admits it as a summary
/// call of the run.
pub async fn summarize(
    request: CachedRequest,
    preparation: &CompactionPreparation,
    model: &Model,
    custom_instructions: Option<&str>,
    stream_fn: &StreamFn,
    signal: AbortSignal,
    control: Option<&RunControl>,
) -> CachedSummary {
    let fallback = |reason: &str| CachedSummary::Fallback(reason.to_owned());
    let Some(key) = message_array_key(&request.payload) else {
        return CachedSummary::Fallback(format!("{} requests are not supported", model.api));
    };
    if preparation.first_kept_entry_id.is_empty() {
        return fallback("the first kept entry has no id");
    }
    let Some(kept) = kept_start(&request.conversation, request.kept) else {
        return fallback("no kept message has text to mark where the summary ends");
    };
    let mut messages = request.conversation;
    messages.push(Message::User(UserMessage {
        content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(
            summary_instruction(&kept, custom_instructions),
        ))]),
        timestamp: bake_ai::now_ms(),
    }));
    let context = bake_ai::transcript::normalize_context(Context {
        system_prompt: None,
        messages,
        tools: None,
    });
    let mut options = request.options;
    options.base.signal = Some(signal.clone());
    let hook = options.base.on_payload.take();
    let captured = request.payload;
    let mismatch: Arc<Mutex<Option<PrefixMismatch>>> = Arc::default();
    let seen = Arc::clone(&mismatch);
    let gate: PayloadGate = Arc::new(move |params, model| {
        // The agent's own hook runs first, as it did on the captured request.
        let replaced = hook.as_ref().and_then(|hook| hook(params, model));
        match summary_payload(replaced.as_ref().unwrap_or(params), &captured, key, &[]) {
            Ok(added) => Ok(added.or(replaced)),
            Err(found) => {
                *seen.lock().unwrap_or_else(PoisonError::into_inner) = Some(found);
                Err(())
            }
        }
    });
    let response = complete_summarization_with_run(
        model,
        &context,
        options,
        stream_fn,
        None,
        None,
        SummaryRun {
            control,
            keep_cache: true,
            gate: Some(&gate),
            ..SummaryRun::default()
        },
    )
    .await;
    if let Some(found) = mismatch
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
    {
        return CachedSummary::Fallback(found.reason);
    }
    let response = match response {
        Ok(response) => response,
        Err(_) if signal.aborted() => return CachedSummary::Cancelled,
        Err(error) => {
            return CachedSummary::Fallback(format!("the summary request failed: {error}"));
        }
    };
    match response.stop_reason {
        StopReason::Aborted if signal.aborted() => return CachedSummary::Cancelled,
        StopReason::Aborted => return fallback("the summary request was aborted"),
        StopReason::Error => {
            return CachedSummary::Fallback(
                response
                    .error_message
                    .filter(|message| !message.is_empty())
                    .unwrap_or_else(|| "the summary request failed".to_owned()),
            );
        }
        StopReason::Length => return fallback("the summary hit the output limit"),
        _ => {}
    }
    if response.stop_reason == StopReason::ToolUse || has_tool_call(&response) {
        return fallback("the model called a tool instead of writing the summary");
    }
    let text = assistant_text(&response);
    if text.trim().is_empty() {
        return fallback("the model returned no summary text");
    }
    let (summary, read_files, modified_files) = checkpoint_summary(&text, &preparation.file_ops);
    let mut details = JsonObject::new();
    details.insert("readFiles".into(), read_files.into());
    details.insert("modifiedFiles".into(), modified_files.into());
    CachedSummary::Done(Box::new(CompactionResult {
        summary,
        first_kept_entry_id: preparation.first_kept_entry_id.clone(),
        tokens_before: preparation.tokens_before,
        estimated_tokens_after: None,
        usage: Some(response.usage),
        details: Some(Value::Object(details)),
        path: super::SummaryPath::Cached,
    }))
}

#[cfg(test)]
#[path = "cached_tests.rs"]
mod tests;
