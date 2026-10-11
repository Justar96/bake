//! Branch summarization for tree navigation.
//!
//! Ported from Pi
//! `packages/coding-agent/src/core/compaction/branch-summarization.ts`
//! (v1.1.0): when navigation leaves a branch, the entries it abandons are
//! summarized so their context is not lost.

use bake_agent::StreamFn;
use bake_ai::utils::retry::RetryPolicy;
use bake_ai::utils::text::assistant_text;
use bake_ai::{
    AbortSignal, Context, Message, Model, ProviderHeaders, SimpleStreamOptions, StopReason,
    TextContent, Usage, UserContent, UserContentBlock, UserMessage,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

use super::utils::{
    FileOperations, SUMMARIZATION_SYSTEM_PROMPT, compute_file_lists, create_file_ops,
    extract_file_ops_from_message, format_file_operations, serialize_conversation,
};
use super::{RetryObserver, complete_summarization, estimate_tokens, get_summarization_failure};
use crate::session::json::{JsonObject, js_truthy};
use crate::session::messages::convert_to_llm;
use crate::session::time::js_date_ms;
use crate::session::{AgentMessage, SessionEntry, SessionManager};

/// Pi's `BranchSummaryResult`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BranchSummaryResult {
    /// The summary.
    pub summary: Option<String>,
    /// The request's usage.
    pub usage: Option<Usage>,
    /// Files only read.
    pub read_files: Option<Vec<String>>,
    /// Files written or edited.
    pub modified_files: Option<Vec<String>>,
    /// Whether the request was aborted.
    pub aborted: bool,
    /// Why the summary failed.
    pub error: Option<String>,
}

/// Pi's `BranchPreparation`.
#[derive(Debug, Clone, PartialEq)]
pub struct BranchPreparation {
    /// Messages to summarize, oldest first.
    pub messages: Vec<AgentMessage>,
    /// File operations of every entry.
    pub file_ops: FileOperations,
    /// The messages' estimated tokens.
    pub total_tokens: u64,
}

/// Pi's `CollectEntriesResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectEntriesResult {
    /// Entries to summarize, oldest first.
    pub entries: Vec<SessionEntry>,
    /// The deepest entry on both paths.
    pub common_ancestor_id: Option<String>,
}

/// Pi's `GenerateBranchSummaryOptions`.
#[derive(Clone)]
pub struct GenerateBranchSummaryOptions {
    /// The summarizing model.
    pub model: Model,
    /// The request path; it resolves auth unless `api_key` is set.
    pub stream_fn: StreamFn,
    /// An explicit API key.
    pub api_key: Option<String>,
    /// Request headers.
    pub headers: Option<ProviderHeaders>,
    /// Provider-scoped environment values.
    pub env: Option<BTreeMap<String, String>>,
    /// Cancels the request.
    pub signal: AbortSignal,
    /// Focus for the summary.
    pub custom_instructions: Option<String>,
    /// Whether `custom_instructions` replaces the prompt.
    pub replace_instructions: bool,
    /// Tokens reserved when choosing history; 16384 by default.
    pub reserve_tokens: Option<u64>,
    /// Retry of transient errors.
    pub retry: Option<RetryPolicy>,
    /// Retry reporting.
    pub on_retry: Option<RetryObserver>,
}

/// Pi's `collectEntriesForBranchSummary`: the entries from `old_leaf_id`
/// back to the common ancestor with `target_id`, oldest first. Compaction
/// entries are included; their summaries become context.
pub fn collect_entries_for_branch_summary(
    session: &SessionManager,
    old_leaf_id: Option<&str>,
    target_id: &str,
) -> CollectEntriesResult {
    let Some(old_leaf_id) = old_leaf_id.filter(|id| !id.is_empty()) else {
        return CollectEntriesResult {
            entries: Vec::new(),
            common_ancestor_id: None,
        };
    };
    let old_path: HashSet<&str> = session
        .branch_entries(Some(old_leaf_id))
        .iter()
        .map(|entry| entry.id())
        .collect();
    let common_ancestor_id = session
        .branch_entries(Some(target_id))
        .iter()
        .rev()
        .find(|entry| old_path.contains(entry.id()))
        .map(|entry| entry.id().to_owned());
    let mut entries = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut current = Some(old_leaf_id.to_owned());
    while let Some(id) = current.filter(|id| !id.is_empty()) {
        if Some(&id) == common_ancestor_id.as_ref() || !seen.insert(id.clone()) {
            break;
        }
        let Some(entry) = session.entry(&id) else {
            break;
        };
        entries.push(entry.clone());
        current = entry.parent_id().map(str::to_owned);
    }
    entries.reverse();
    CollectEntriesResult {
        entries,
        common_ancestor_id,
    }
}

fn copy(target: &mut JsonObject, source: &JsonObject, key: &str) {
    if let Some(value) = source.get(key) {
        target.insert(key.to_owned(), value.clone());
    }
}

fn timestamp(entry: &SessionEntry) -> Value {
    js_date_ms(entry.get("timestamp")).map_or(Value::Null, Value::from)
}

/// Pi's `getMessageFromEntry`: the message an entry contributes to a branch
/// summary. Tool results are skipped; their calls carry the context.
fn message_from_entry(entry: &SessionEntry) -> Option<AgentMessage> {
    let raw = entry.as_json();
    let mut message = JsonObject::new();
    match entry.entry_type() {
        "message" => {
            if entry.message_role() == Some("toolResult") {
                return None;
            }
            return entry
                .get("message")
                .and_then(Value::as_object)
                .map(|message| AgentMessage::from_json(message.clone()));
        }
        "custom_message" => {
            message.insert("role".into(), "custom".into());
            copy(&mut message, raw, "customType");
            copy(&mut message, raw, "content");
            copy(&mut message, raw, "display");
            copy(&mut message, raw, "details");
        }
        "branch_summary" => {
            message.insert("role".into(), "branchSummary".into());
            copy(&mut message, raw, "summary");
            copy(&mut message, raw, "fromId");
        }
        "compaction" => {
            message.insert("role".into(), "compactionSummary".into());
            copy(&mut message, raw, "summary");
            copy(&mut message, raw, "tokensBefore");
        }
        _ => return None,
    }
    message.insert("timestamp".into(), timestamp(entry));
    Some(AgentMessage::from_json(message))
}

/// Pi's `prepareBranchEntries`: the newest messages that fit
/// `token_budget` (0 is no limit), and the file operations of every entry,
/// including earlier branch summaries' recorded files.
pub fn prepare_branch_entries(entries: &[SessionEntry], token_budget: u64) -> BranchPreparation {
    let mut messages: Vec<AgentMessage> = Vec::new();
    let mut file_ops = create_file_ops();
    let mut total_tokens = 0u64;
    for entry in entries {
        if entry.entry_type() == "branch_summary"
            && !js_truthy(entry.get("fromHook"))
            && let Some(details) = entry.get("details").filter(|d| js_truthy(Some(d)))
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
    }
    for entry in entries.iter().rev() {
        let Some(message) = message_from_entry(entry) else {
            continue;
        };
        extract_file_ops_from_message(&message, &mut file_ops);
        let tokens = estimate_tokens(&message);
        if token_budget > 0 && total_tokens.saturating_add(tokens) > token_budget {
            // A summary is important context: keep it when there is room.
            if matches!(entry.entry_type(), "compaction" | "branch_summary")
                && (total_tokens as f64) < token_budget as f64 * 0.9
            {
                messages.push(message);
                total_tokens = total_tokens.saturating_add(tokens);
            }
            break;
        }
        messages.push(message);
        total_tokens = total_tokens.saturating_add(tokens);
    }
    messages.reverse();
    BranchPreparation {
        messages,
        file_ops,
        total_tokens,
    }
}

const BRANCH_SUMMARY_PREAMBLE: &str = "The user explored a different conversation branch before returning here.\nSummary of that exploration:\n\n";

const BRANCH_SUMMARY_PROMPT: &str =
    "Create a structured summary of this conversation branch for context when returning later.

Use this EXACT format:

## Goal
[What was the user trying to accomplish in this branch?]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned]
- [Or \"(none)\" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Work that was started but not finished]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [What should happen next to continue this work]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

/// Pi's `generateBranchSummary`: summarize abandoned `entries` (oldest
/// first). Failures and aborts are results; only a stream function that
/// fails to start is an `Err`.
pub async fn generate_branch_summary(
    entries: &[SessionEntry],
    options: &GenerateBranchSummaryOptions,
) -> Result<BranchSummaryResult, String> {
    let model = &options.model;
    let context_window = if model.context_window > 0 {
        model.context_window
    } else {
        128_000
    };
    let reserve = options.reserve_tokens.unwrap_or(16384);
    // A negative budget is no limit, as `tokenBudget > 0` reads it.
    let token_budget = context_window.saturating_sub(reserve);
    let preparation = prepare_branch_entries(entries, token_budget);
    if preparation.messages.is_empty() {
        return Ok(BranchSummaryResult {
            summary: Some("No content to summarize".to_owned()),
            ..BranchSummaryResult::default()
        });
    }
    let live: Vec<bake_agent::AgentMessage> = preparation
        .messages
        .iter()
        .map(AgentMessage::to_agent)
        .collect();
    let conversation = serialize_conversation(&convert_to_llm(&live));
    let custom = options
        .custom_instructions
        .as_deref()
        .filter(|text| !text.is_empty());
    let instructions = match custom {
        Some(custom) if options.replace_instructions => custom.to_owned(),
        Some(custom) => format!("{BRANCH_SUMMARY_PROMPT}\n\nAdditional focus: {custom}"),
        None => BRANCH_SUMMARY_PROMPT.to_owned(),
    };
    let prompt = format!("<conversation>\n{conversation}\n</conversation>\n\n{instructions}");
    let max_tokens = if model.max_tokens > 0 {
        model.max_tokens.min(4096)
    } else {
        4096
    };
    let context = bake_ai::transcript::normalize_context(Context {
        system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_owned()),
        messages: vec![Message::User(UserMessage {
            content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(prompt))]),
            timestamp: bake_ai::now_ms(),
        })],
        tools: None,
    });
    let mut request = SimpleStreamOptions::default();
    request.base.api_key = options.api_key.clone();
    request.base.headers = options.headers.clone();
    request.base.env = options.env.clone();
    request.base.signal = Some(options.signal.clone());
    request.base.max_tokens = Some(max_tokens);
    let response = complete_summarization(
        model,
        &context,
        request,
        &options.stream_fn,
        options.retry,
        options.on_retry.as_ref(),
    )
    .await?;
    if response.stop_reason == StopReason::Aborted {
        return Ok(BranchSummaryResult {
            aborted: true,
            ..BranchSummaryResult::default()
        });
    }
    if let Some(failure) = get_summarization_failure(&response, "Branch summarization") {
        return Ok(BranchSummaryResult {
            error: Some(failure),
            ..BranchSummaryResult::default()
        });
    }
    if super::has_tool_call(&response) {
        return Ok(BranchSummaryResult {
            error: Some("Branch summarization attempted to call a tool".to_owned()),
            ..BranchSummaryResult::default()
        });
    }
    let mut summary = format!("{BRANCH_SUMMARY_PREAMBLE}{}", assistant_text(&response));
    let (read_files, modified_files) = compute_file_lists(&preparation.file_ops);
    summary.push_str(&format_file_operations(&read_files, &modified_files));
    Ok(BranchSummaryResult {
        summary: Some(summary),
        usage: Some(response.usage),
        read_files: Some(read_files),
        modified_files: Some(modified_files),
        aborted: false,
        error: None,
    })
}
