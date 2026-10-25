//! Ports of Pi v1.1.0 `test/compaction.test.ts` and
//! `test/compaction-summary-reasoning.test.ts`, named after the Pi test each
//! follows. Not ported: the "Large session fixture" cases, whose 1 MB
//! fixture `tests/compaction.rs`'s Pi oracle replaces, and the
//! "LLM summarization" cases, which need a live model and a key; and
//! `compaction-summary-reasoning.test.ts`'s "leaves Anthropic refusal
//! fallback handling to pi-ai model metadata" and "does not set Anthropic
//! refusal fallback for models without allowed fallback targets", because
//! `bake-ai` models and options carry no refusal fallbacks to leave unset.

use std::sync::{Arc, Mutex, PoisonError};

use bake_ai::providers::faux::{faux_assistant_blocks, faux_model, faux_text, faux_tool_call};
use bake_ai::{
    AssistantMessage, CacheRetention, ThinkingLevel, ToolChoice, assistant_message_channel,
    transcript::normalize_context,
};
use serde_json::json;

use super::*;
use crate::session::build_session_context;
use crate::session::json::JsonObject;

#[path = "attempt_tests.rs"]
mod attempt;

struct Entries {
    counter: usize,
    last: Option<String>,
}

impl Entries {
    fn new() -> Self {
        Self {
            counter: 0,
            last: None,
        }
    }

    fn push(&mut self, kind: &str, mut fields: JsonObject) -> SessionEntry {
        let id = format!("test-id-{}", self.counter);
        self.counter += 1;
        fields.insert("type".into(), kind.into());
        fields.insert("id".into(), id.clone().into());
        fields.insert(
            "parentId".into(),
            self.last.clone().map_or(Value::Null, Value::String),
        );
        fields.insert("timestamp".into(), crate::session::time::now_iso().into());
        self.last = Some(id);
        SessionEntry::from_json(fields).unwrap_or_else(|_| panic!("entry"))
    }

    fn message(&mut self, message: Value) -> SessionEntry {
        self.push("message", object(json!({ "message": message })))
    }

    fn compaction(&mut self, summary: &str, first_kept: &str) -> SessionEntry {
        self.push(
            "compaction",
            object(json!({ "summary": summary, "firstKeptEntryId": first_kept, "tokensBefore": 10000 })),
        )
    }

    fn custom_message(&mut self, content: &str) -> SessionEntry {
        self.push(
            "custom_message",
            object(json!({ "customType": "test", "content": content, "display": true })),
        )
    }
}

fn object(value: Value) -> JsonObject {
    match value {
        Value::Object(object) => object,
        _ => JsonObject::new(),
    }
}

fn mock_usage(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Value {
    json!({
        "input": input, "output": output, "cacheRead": cache_read, "cacheWrite": cache_write,
        "totalTokens": input + output + cache_read + cache_write,
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 },
    })
}

fn user(text: &str) -> Value {
    json!({ "role": "user", "content": text, "timestamp": bake_ai::now_ms() })
}

fn assistant(text: &str, usage: Option<Value>) -> Value {
    json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": text }],
        "usage": usage.unwrap_or_else(|| mock_usage(100, 50, 0, 0)),
        "stopReason": "stop",
        "timestamp": bake_ai::now_ms(),
        "api": "anthropic-messages",
        "provider": "anthropic",
        "model": "claude-sonnet-4-5",
    })
}

fn stored(value: Value) -> AgentMessage {
    AgentMessage::from_json(object(value))
}

fn message_of(entry: &SessionEntry) -> AgentMessage {
    stored(entry.get("message").cloned().unwrap_or(Value::Null))
}

fn text_of(messages: &[AgentMessage]) -> String {
    messages
        .iter()
        .map(|message| {
            let object = message.as_json();
            match object.get("content") {
                Some(Value::String(text)) => text.clone(),
                Some(Value::Array(blocks)) => blocks
                    .iter()
                    .filter_map(|block| block.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => object
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// Pi "Token calculation": "should calculate total context tokens from
// usage" and "should handle zero values".
#[test]
fn calculates_context_tokens() {
    let usage: Usage = serde_json::from_value(mock_usage(1000, 500, 200, 100)).unwrap_or_default();
    assert_eq!(calculate_context_tokens(&usage), 1800);
    assert_eq!(calculate_context_tokens(&Usage::default()), 0);
}

// Pi "getLastAssistantUsage": all four cases.
#[test]
fn finds_the_last_valid_assistant_usage() {
    let mut e = Entries::new();
    let entries = vec![
        e.message(user("Hello")),
        e.message(assistant("Hi", Some(mock_usage(100, 50, 0, 0)))),
        e.message(user("How are you?")),
        e.message(assistant("Good", Some(mock_usage(200, 100, 0, 0)))),
    ];
    assert_eq!(
        get_last_assistant_usage(&entries).map(|u| u.input),
        Some(200)
    );

    let mut aborted = assistant("Aborted", Some(mock_usage(300, 150, 0, 0)));
    aborted["stopReason"] = json!("aborted");
    let mut e = Entries::new();
    let entries = vec![
        e.message(user("Hello")),
        e.message(assistant("Hi", Some(mock_usage(100, 50, 0, 0)))),
        e.message(user("How are you?")),
        e.message(aborted),
    ];
    assert_eq!(
        get_last_assistant_usage(&entries).map(|u| u.input),
        Some(100)
    );

    let mut e = Entries::new();
    let entries = vec![
        e.message(user("Hello")),
        e.message(assistant("Hi", Some(mock_usage(100, 50, 0, 0)))),
        e.message(user("continue")),
        e.message(assistant("Partial", Some(mock_usage(0, 0, 0, 0)))),
    ];
    assert_eq!(
        get_last_assistant_usage(&entries).map(|u| u.input),
        Some(100)
    );

    let mut e = Entries::new();
    assert_eq!(get_last_assistant_usage(&[e.message(user("Hello"))]), None);
}

// Pi "estimateContextTokens": "uses the last non-zero assistant usage as
// the context anchor".
#[test]
fn anchors_the_estimate_on_the_last_valid_usage() {
    let messages = [
        stored(user("Hello")),
        stored(assistant("Hi", Some(mock_usage(100, 50, 0, 0)))),
        stored(user("continue")),
        stored(assistant("Partial thinking", Some(mock_usage(0, 0, 0, 0)))),
    ];
    let estimate = estimate_context_tokens(&messages);
    assert_eq!(estimate.usage_tokens, 150);
    assert_eq!(estimate.last_usage_index, Some(1));
    assert!(estimate.trailing_tokens > 0);
    assert_eq!(estimate.tokens, 150 + estimate.trailing_tokens);
}

// Pi "shouldCompact": both cases.
#[test]
fn compacts_above_the_reserve_unless_disabled() {
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 10000,
        keep_recent_tokens: 20000,
    };
    assert!(should_compact(95000, 100000, &settings));
    assert!(!should_compact(89000, 100000, &settings));
    let disabled = CompactionSettings {
        enabled: false,
        ..settings
    };
    assert!(!should_compact(95000, 100000, &disabled));
    // A reserve above the window compacts any context.
    assert!(should_compact(
        1,
        10,
        &CompactionSettings {
            reserve_tokens: 20,
            ..settings
        }
    ));
}

// Pi "findCutPoint": "should find cut point based on actual token
// differences".
#[test]
fn cuts_at_a_user_or_assistant_message() {
    let mut e = Entries::new();
    let mut entries = Vec::new();
    for i in 0..10u64 {
        entries.push(e.message(user(&format!("User {i}"))));
        entries.push(e.message(assistant(
            &format!("Assistant {i}"),
            Some(mock_usage(0, 100, (i + 1) * 1000, 0)),
        )));
    }
    let result = find_cut_point(&entries, 0, entries.len(), 2500);
    let entry = &entries[result.first_kept_entry_index];
    assert_eq!(entry.entry_type(), "message");
    assert!(matches!(entry.message_role(), Some("user" | "assistant")));
}

// Pi: "should return startIndex if no valid cut points in range" and
// "should keep everything if all messages fit within budget".
#[test]
fn keeps_from_the_start_without_cut_points_or_when_all_fits() {
    let mut e = Entries::new();
    let entries = vec![e.message(assistant("a", None))];
    assert_eq!(
        find_cut_point(&entries, 0, 1, 1000).first_kept_entry_index,
        0
    );

    let mut e = Entries::new();
    let entries = vec![
        e.message(user("1")),
        e.message(assistant("a", Some(mock_usage(0, 50, 500, 0)))),
        e.message(user("2")),
        e.message(assistant("b", Some(mock_usage(0, 50, 1000, 0)))),
    ];
    assert_eq!(
        find_cut_point(&entries, 0, entries.len(), 50000).first_kept_entry_index,
        0
    );
}

// Pi: "should indicate split turn when cutting at assistant message".
#[test]
fn a_cut_at_an_assistant_message_splits_its_turn() {
    let mut e = Entries::new();
    let entries = vec![
        e.message(user("Turn 1")),
        e.message(assistant("A1", Some(mock_usage(0, 100, 1000, 0)))),
        e.message(user("Turn 2")),
        e.message(assistant("A2-1", Some(mock_usage(0, 100, 5000, 0)))),
        e.message(assistant("A2-2", Some(mock_usage(0, 100, 8000, 0)))),
        e.message(assistant("A2-3", Some(mock_usage(0, 100, 10000, 0)))),
    ];
    let result = find_cut_point(&entries, 0, entries.len(), 3000);
    if entries[result.first_kept_entry_index].message_role() == Some("assistant") {
        assert!(result.is_split_turn);
        assert_eq!(result.turn_start_index, Some(2));
    }
}

// Pi: "should budget context-visible custom message entries".
#[test]
fn custom_message_entries_count_toward_the_budget() {
    let mut e = Entries::new();
    let entries = vec![
        e.message(user("hi")),
        e.message(assistant("hello", None)),
        e.custom_message(&"x".repeat(4000)),
        e.message(assistant("ok", None)),
    ];
    let tiny = find_cut_point(&entries, 0, entries.len(), 1);
    assert_eq!(tiny.first_kept_entry_index, 3);
    assert!(tiny.is_split_turn);
    assert_eq!(tiny.turn_start_index, Some(2));
    let fits = find_cut_point(&entries, 0, entries.len(), 2);
    assert_eq!(fits.first_kept_entry_index, 2);
    assert!(!fits.is_split_turn);
    assert_eq!(fits.turn_start_index, None);
}

// Pi: "should fall back to the latest valid cut point before oversized
// trailing tool results" (#9740).
#[test]
fn oversized_trailing_tool_results_keep_their_call() {
    let mut e = Entries::new();
    let old_user = e.message(user("old history"));
    let old_assistant = e.message(assistant("old answer", None));
    let current_user = e.message(user("read the large file"));
    let mut call = assistant("", None);
    call["content"] = json!([{ "type": "toolCall", "id": "call-1", "name": "read", "arguments": { "path": "big.txt" } }]);
    call["stopReason"] = json!("toolUse");
    let tool_call = e.message(call);
    let tool_result = e.message(json!({
        "role": "toolResult", "toolCallId": "call-1", "toolName": "read",
        "content": [{ "type": "text", "text": "x".repeat(8000) }], "isError": false,
        "timestamp": bake_ai::now_ms(),
    }));
    let entries = vec![
        old_user.clone(),
        old_assistant.clone(),
        current_user.clone(),
        tool_call.clone(),
        tool_result,
    ];
    assert_eq!(
        find_cut_point(&entries, 0, entries.len(), 1000),
        CutPointResult {
            first_kept_entry_index: 3,
            turn_start_index: Some(2),
            is_split_turn: true,
        }
    );
    let preparation = prepare_compaction(
        &entries,
        CompactionSettings {
            keep_recent_tokens: 1000,
            ..DEFAULT_COMPACTION_SETTINGS
        },
    )
    .unwrap_or_else(|| panic!("preparation"));
    assert_eq!(preparation.first_kept_entry_id, tool_call.id());
    assert_eq!(
        preparation.messages_to_summarize,
        [message_of(&old_user), message_of(&old_assistant)]
    );
    assert_eq!(
        preparation.turn_prefix_messages,
        [message_of(&current_user)]
    );
}

// Pi "buildSessionContext": all five cases.
#[test]
fn session_context_follows_the_latest_compaction() {
    let mut e = Entries::new();
    let entries = vec![
        e.message(user("1")),
        e.message(assistant("a", None)),
        e.message(user("2")),
        e.message(assistant("b", None)),
    ];
    let loaded = build_session_context(&entries, LeafSelector::Last);
    assert_eq!(loaded.messages.len(), 4);
    assert_eq!(loaded.thinking_level, "off");
    assert_eq!(
        loaded.model.map(|m| (m.provider, m.model_id)),
        Some(("anthropic".into(), "claude-sonnet-4-5".into()))
    );

    let mut e = Entries::new();
    let u1 = e.message(user("1"));
    let a1 = e.message(assistant("a", None));
    let u2 = e.message(user("2"));
    let a2 = e.message(assistant("b", None));
    let compaction = e.compaction("Summary of 1,a,2,b", u2.id());
    let u3 = e.message(user("3"));
    let a3 = e.message(assistant("c", None));
    let loaded = build_session_context(&[u1, a1, u2, a2, compaction, u3, a3], LeafSelector::Last);
    assert_eq!(loaded.messages.len(), 5);
    assert_eq!(loaded.messages[0].role(), Some("compactionSummary"));
    assert!(text_of(&loaded.messages[..1]).contains("Summary of 1,a,2,b"));

    let mut e = Entries::new();
    let u1 = e.message(user("1"));
    let a1 = e.message(assistant("a", None));
    let compact1 = e.compaction("First summary", u1.id());
    let u2 = e.message(user("2"));
    let b = e.message(assistant("b", None));
    let u3 = e.message(user("3"));
    let c = e.message(assistant("c", None));
    let compact2 = e.compaction("Second summary", u3.id());
    let u4 = e.message(user("4"));
    let d = e.message(assistant("d", None));
    let loaded = build_session_context(
        &[u1, a1, compact1, u2, b, u3, c, compact2, u4, d],
        LeafSelector::Last,
    );
    assert_eq!(loaded.messages.len(), 5);
    assert!(text_of(&loaded.messages[..1]).contains("Second summary"));

    let mut e = Entries::new();
    let u1 = e.message(user("1"));
    let a1 = e.message(assistant("a", None));
    let compact1 = e.compaction("First summary", u1.id());
    let u2 = e.message(user("2"));
    let b = e.message(assistant("b", None));
    let loaded = build_session_context(&[u1, a1, compact1, u2, b], LeafSelector::Last);
    assert_eq!(loaded.messages.len(), 5);

    let mut e = Entries::new();
    let entries = vec![
        e.message(user("1")),
        e.push(
            "model_change",
            object(json!({ "provider": "openai", "modelId": "gpt-4" })),
        ),
        e.message(assistant("a", None)),
        e.push(
            "thinking_level_change",
            object(json!({ "thinkingLevel": "high" })),
        ),
    ];
    let loaded = build_session_context(&entries, LeafSelector::Last);
    assert_eq!(
        loaded.model.map(|m| (m.provider, m.model_id)),
        Some(("anthropic".into(), "claude-sonnet-4-5".into()))
    );
    assert_eq!(loaded.thinking_level, "high");
}

// Pi "prepareCompaction": "does not treat system messages as conversation
// history".
#[test]
fn system_messages_are_not_summarized() {
    let mut e = Entries::new();
    let system = e.message(json!({
        "role": "system", "content": "", "sections": { "preamble": "current prompt" },
        "timestamp": bake_ai::now_ms(),
    }));
    let user_entry = e.message(user("one long turn"));
    let assistant_entry = e.message(assistant("assistant suffix", None));
    let preparation = prepare_compaction(
        &[system, user_entry.clone(), assistant_entry.clone()],
        CompactionSettings {
            keep_recent_tokens: 1,
            ..DEFAULT_COMPACTION_SETTINGS
        },
    )
    .unwrap_or_else(|| panic!("preparation"));
    assert_eq!(preparation.first_kept_entry_id, assistant_entry.id());
    assert!(preparation.is_split_turn);
    assert!(preparation.messages_to_summarize.is_empty());
    assert_eq!(preparation.turn_prefix_messages, [message_of(&user_entry)]);
}

// Pi "prepareCompaction with previous compaction": both cases.
#[test]
fn repeated_compaction_follows_the_recent_window() {
    let mut e = Entries::new();
    let u1 = e.message(user("user msg 1 (summarized by compaction1)"));
    let a1 = e.message(assistant("assistant msg 1", None));
    let u2 = e.message(user("user msg 2 - kept by compaction1"));
    let a2 = e.message(assistant("assistant msg 2", None));
    let u3 = e.message(user("user msg 3 - kept by compaction1"));
    let a3 = e.message(assistant(
        "assistant msg 3",
        Some(mock_usage(5000, 1000, 0, 0)),
    ));
    let compaction1 = e.compaction("First summary", u2.id());
    let u4 = e.message(user("user msg 4 (new after compaction1)"));
    let a4 = e.message(assistant(
        "assistant msg 4",
        Some(mock_usage(8000, 2000, 0, 0)),
    ));
    assert_eq!(
        prepare_compaction(
            &[u1, a1, u2, a2, u3, a3, compaction1, u4, a4],
            DEFAULT_COMPACTION_SETTINGS
        ),
        None
    );

    let mut e = Entries::new();
    let u1 = e.message(user(&"user msg 1 (summarized by compaction1)".repeat(4)));
    let a1 = e.message(assistant(&"assistant msg 1".repeat(4), None));
    let u2 = e.message(user(&"user msg 2 - kept by compaction1 ".repeat(12)));
    let a2 = e.message(assistant(&"assistant msg 2 ".repeat(12), None));
    let u3 = e.message(user(&"user msg 3 - kept by compaction1 ".repeat(12)));
    let a3 = e.message(assistant(
        &"assistant msg 3 ".repeat(12),
        Some(mock_usage(5000, 1000, 0, 0)),
    ));
    let compaction1 = e.compaction("First summary", u2.id());
    let u4 = e.message(user(&"user msg 4 (new after compaction1) ".repeat(12)));
    let a4 = e.message(assistant(
        &"assistant msg 4 ".repeat(12),
        Some(mock_usage(8000, 2000, 0, 0)),
    ));
    let preparation = prepare_compaction(
        &[u1, a1, u2, a2, u3, a3, compaction1, u4, a4],
        CompactionSettings {
            keep_recent_tokens: 100,
            ..DEFAULT_COMPACTION_SETTINGS
        },
    )
    .unwrap_or_else(|| panic!("preparation"));
    let text = text_of(&preparation.messages_to_summarize);
    assert!(text.contains("user msg 2 - kept by compaction1"));
    assert!(text.contains("user msg 3 - kept by compaction1"));
    assert!(!text.contains("First summary"));
    assert_eq!(
        preparation.previous_summary.as_deref(),
        Some("First summary")
    );
}

// ----------------------------------------------------------------------------
// Pi `test/compaction-summary-reasoning.test.ts`
// ----------------------------------------------------------------------------

/// A stream function answering every request with `response`, recording
/// the contexts and options it was called with.
#[derive(Clone)]
struct Mock {
    calls: Arc<Mutex<Vec<(TranscriptContext, SimpleStreamOptions)>>>,
    responses: Arc<Mutex<Vec<AssistantMessage>>>,
}

impl Mock {
    fn new() -> Self {
        Self {
            calls: Arc::default(),
            responses: Arc::default(),
        }
    }

    fn once(&self, response: AssistantMessage) {
        self.responses
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(response);
    }

    fn stream_fn(&self) -> StreamFn {
        let mock = self.clone();
        Arc::new(move |_model, context, options| {
            mock.calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((context.clone(), options));
            let response = {
                let mut responses = mock
                    .responses
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if responses.is_empty() {
                    summary_response()
                } else {
                    responses.remove(0)
                }
            };
            let (sender, stream) = assistant_message_channel();
            sender.finish(response);
            Ok(stream)
        })
    }

    fn options(&self) -> Vec<SimpleStreamOptions> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(_, options)| options.clone())
            .collect()
    }
}

fn summary_response() -> AssistantMessage {
    let mut message =
        faux_assistant_blocks(vec![faux_text("## Goal\nTest summary")], StopReason::Stop);
    message.usage = Usage {
        input: 10,
        output: 10,
        total_tokens: 20,
        ..Usage::default()
    };
    message
}

fn tool_call_response() -> AssistantMessage {
    let mut message = summary_response();
    message.content = vec![faux_tool_call(
        "read",
        json!({ "path": "README.md" }),
        Some("tool-call-1"),
    )];
    message.stop_reason = StopReason::ToolUse;
    message
}

fn length_response() -> AssistantMessage {
    let mut message = summary_response();
    message.content = vec![faux_text("partial")];
    message.stop_reason = StopReason::Length;
    message
}

fn test_model(reasoning: bool, max_tokens: u64) -> Model {
    let mut model = faux_model(if reasoning {
        "reasoning-model"
    } else {
        "non-reasoning-model"
    });
    model.reasoning = reasoning;
    model.context_window = 200_000;
    model.max_tokens = max_tokens;
    model
}

fn summarize_this() -> Vec<AgentMessage> {
    vec![stored(
        json!({ "role": "user", "content": "Summarize this.", "timestamp": 1 }),
    )]
}

fn key() -> SummarizationOptions {
    SummarizationOptions {
        api_key: Some("test-key".into()),
        ..SummarizationOptions::default()
    }
}

fn split_preparation(previous: Option<&str>, reserve: u64, history: bool) -> CompactionPreparation {
    CompactionPreparation {
        first_kept_entry_id: "entry-keep".into(),
        messages_to_summarize: if history {
            summarize_this()
        } else {
            Vec::new()
        },
        turn_prefix_messages: summarize_this(),
        is_split_turn: true,
        tokens_before: 100,
        previous_summary: previous.map(str::to_owned),
        file_ops: FileOperations::default(),
        settings: CompactionSettings {
            enabled: true,
            reserve_tokens: reserve,
            keep_recent_tokens: 20,
        },
    }
}

// Pi: "uses the provided thinking level for reasoning-capable models".
#[tokio::test]
async fn reasoning_models_use_the_thinking_level() {
    let mock = Mock::new();
    let options = SummarizationOptions {
        thinking_level: Some(ModelThinkingLevel::Medium),
        ..key()
    };
    let result = generate_summary_with_usage(
        &summarize_this(),
        &test_model(true, 8192),
        None,
        None,
        &mock.stream_fn(),
        &options,
    )
    .await;
    let result = result.unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(result.text, "## Goal\nTest summary");
    assert_eq!(result.usage, summary_response().usage);
    let sent = mock.options();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].reasoning, Some(ThinkingLevel::Medium));
    assert_eq!(sent[0].base.api_key.as_deref(), Some("test-key"));
}

// Pi: "preserves the string result from generateSummary" and "uses fresh
// routing sessions without prompt caching".
#[tokio::test]
async fn summaries_route_fresh_sessions_without_caching() {
    let mock = Mock::new();
    let stream_fn = mock.stream_fn();
    for _ in 0..2 {
        let text = generate_summary(
            &summarize_this(),
            &test_model(false, 8192),
            None,
            None,
            &stream_fn,
            &key(),
        )
        .await;
        assert_eq!(text.as_deref(), Ok("## Goal\nTest summary"));
    }
    let sent = mock.options();
    assert_eq!(sent.len(), 2);
    assert!(
        sent.iter()
            .all(|options| options.base.cache_retention == Some(CacheRetention::None))
    );
    assert!(sent[0].base.session_id.is_some());
    assert_ne!(sent[0].base.session_id, sent[1].base.session_id);
}

// Pi: "honors caller-supplied routing session and tool choice without
// prompt caching".
#[tokio::test]
async fn caller_routing_and_tool_choice_survive() {
    let mock = Mock::new();
    let mut options = SimpleStreamOptions::default();
    options.base.session_id = Some("current-routing-session".into());
    options.base.cache_retention = Some(CacheRetention::Long);
    options.tool_choice = Some(ToolChoice::Auto);
    let context = normalize_context(Context {
        system_prompt: Some("Summarize".into()),
        messages: Vec::new(),
        tools: None,
    });
    let response = complete_summarization(
        &test_model(false, 8192),
        &context,
        options,
        &mock.stream_fn(),
        None,
        None,
    )
    .await;
    assert!(response.is_ok());
    let sent = mock.options();
    assert_eq!(
        sent[0].base.session_id.as_deref(),
        Some("current-routing-session")
    );
    assert_eq!(sent[0].base.cache_retention, Some(CacheRetention::None));
    assert_eq!(sent[0].tool_choice, Some(ToolChoice::Auto));
}

// Pi: "preserves the previous summary without an empty history request for
// a split turn" (#9652).
#[tokio::test]
async fn a_split_turn_without_history_keeps_the_previous_summary() {
    let mock = Mock::new();
    let result = compact(
        &split_preparation(Some("previous checkpoint"), 2000, false),
        &test_model(false, 8192),
        None,
        &mock.stream_fn(),
        &key(),
    )
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    let calls = mock
        .calls
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(calls.len(), 1);
    assert!(result.summary.contains("previous checkpoint"));
    let prompt = serde_json::to_string(&calls[0].0.messages()).unwrap_or_default();
    assert!(prompt.contains("# Conversation\\n[User]: Summarize this."));
    assert!(prompt.contains(
        "# Instructions\\nThe messages above are earlier context from an ongoing conversation."
    ));
}

// Pi: "rejects tool calls from conversation summaries" and "rejects tool
// calls from split-turn summaries".
#[tokio::test]
async fn tool_calls_are_rejected() {
    let mock = Mock::new();
    mock.once(tool_call_response());
    let result = generate_summary_with_usage(
        &summarize_this(),
        &test_model(false, 8192),
        None,
        None,
        &mock.stream_fn(),
        &key(),
    )
    .await;
    assert_eq!(result, Err("Summarization attempted to call a tool".into()));

    mock.once(tool_call_response());
    let result = compact(
        &split_preparation(None, 2000, false),
        &test_model(false, 8192),
        None,
        &mock.stream_fn(),
        &key(),
    )
    .await;
    assert_eq!(
        result,
        Err("Turn prefix summarization attempted to call a tool".into())
    );
}

// Pi: "rejects a length-limited history summary" and "rejects a
// length-limited split-turn summary".
#[tokio::test]
async fn length_stops_are_rejected() {
    let mock = Mock::new();
    mock.once(length_response());
    let result = generate_summary_with_usage(
        &summarize_this(),
        &test_model(false, 8192),
        None,
        None,
        &mock.stream_fn(),
        &key(),
    )
    .await;
    assert!(result.is_err_and(|error| error.contains("generation hit the token cap")));
    mock.once(length_response());
    let result = compact(
        &split_preparation(None, 2000, false),
        &test_model(false, 8192),
        None,
        &mock.stream_fn(),
        &key(),
    )
    .await;
    assert!(result.is_err_and(|error| error.contains("generation hit the token cap")));
}

// Pi: "does not set reasoning when thinking is off" and "does not set
// reasoning for non-reasoning models".
#[tokio::test]
async fn reasoning_is_left_unset_when_off_or_unsupported() {
    for (reasoning, level) in [
        (true, ModelThinkingLevel::Off),
        (false, ModelThinkingLevel::Medium),
    ] {
        let mock = Mock::new();
        let options = SummarizationOptions {
            thinking_level: Some(level),
            ..key()
        };
        let text = generate_summary(
            &summarize_this(),
            &test_model(reasoning, 8192),
            None,
            None,
            &mock.stream_fn(),
            &options,
        )
        .await;
        assert!(text.is_ok());
        let sent = mock.options();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].base.api_key.as_deref(), Some("test-key"));
        assert_eq!(sent[0].reasoning, None);
    }
}

// Pi v1.4.0: "combines usage from split-turn summaries". Neither request
// sets an output cap, so both use the model's limit (#9512).
#[tokio::test]
async fn split_turn_summaries_combine_usage_without_an_output_cap() {
    let mock = Mock::new();
    let mut preparation = split_preparation(None, 16384, true);
    preparation.tokens_before = 600_000;
    let result = compact(
        &preparation,
        &test_model(false, 128_000),
        None,
        &mock.stream_fn(),
        &key(),
    )
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        result.usage,
        Some(Usage {
            input: 20,
            output: 20,
            total_tokens: 40,
            ..Usage::default()
        })
    );
    let caps: Vec<Option<u64>> = mock.options().iter().map(|o| o.base.max_tokens).collect();
    assert_eq!(caps, [None, None]);
}

// Pi v1.4.0: "applies custom instructions to the turn prefix summary of a
// split turn" (#11058).
#[tokio::test]
async fn a_split_turn_prefix_summary_takes_the_custom_instructions() {
    let mock = Mock::new();
    compact(
        &split_preparation(Some("previous checkpoint"), 2000, false),
        &test_model(false, 8192),
        Some("keep migration decisions"),
        &mock.stream_fn(),
        &key(),
    )
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    let calls = mock
        .calls
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(calls.len(), 1);
    let prompt = serde_json::to_string(&calls[0].0.messages()).unwrap_or_default();
    assert!(prompt.contains("Additional focus: keep migration decisions"));
}

#[tokio::test]
async fn a_stream_function_that_fails_to_start_is_an_error() {
    let stream_fn: StreamFn = Arc::new(|_, _, _| Err("no provider".to_owned()));
    let result = generate_summary(
        &summarize_this(),
        &test_model(false, 8192),
        None,
        None,
        &stream_fn,
        &key(),
    )
    .await;
    assert_eq!(result, Err("no provider".into()));
}

// Pi's `completeSummarization` rethrows when the stream function throws,
// so `retryAssistantCall` never reaches `onRetryFinished`.
#[tokio::test]
async fn a_stream_function_failing_after_a_retry_reports_no_finish() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    let stream_fn: StreamFn = Arc::new(move |_, _, _| {
        if count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0 {
            return Err("no provider".to_owned());
        }
        let mut overloaded = summary_response();
        overloaded.stop_reason = StopReason::Error;
        overloaded.error_message = Some("overloaded_error".into());
        let (sender, stream) = assistant_message_channel();
        sender.finish(overloaded);
        Ok(stream)
    });
    let events: Arc<Mutex<Vec<RetryEvent>>> = Arc::default();
    let seen = Arc::clone(&events);
    let options = SummarizationOptions {
        retry: Some(RetryPolicy {
            enabled: true,
            max_retries: 2,
            base_delay_ms: 1,
            max_agent_delay_ms: None,
        }),
        on_retry: Some(Arc::new(move |event| {
            seen.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(event);
        })),
        ..key()
    };
    let result = generate_summary(
        &summarize_this(),
        &test_model(false, 8192),
        None,
        None,
        &stream_fn,
        &options,
    )
    .await;
    assert_eq!(result, Err("no provider".into()));
    let events = events
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(matches!(events[0], RetryEvent::Scheduled { .. }));
    assert!(matches!(events[1], RetryEvent::AttemptStart));
}

#[tokio::test]
async fn transient_summary_errors_retry_under_the_policy() {
    let mock = Mock::new();
    let mut overloaded = summary_response();
    overloaded.stop_reason = StopReason::Error;
    overloaded.error_message = Some("overloaded_error".into());
    mock.once(overloaded);
    let events: Arc<Mutex<Vec<RetryEvent>>> = Arc::default();
    let seen = Arc::clone(&events);
    let options = SummarizationOptions {
        retry: Some(RetryPolicy {
            enabled: true,
            max_retries: 2,
            base_delay_ms: 1,
            max_agent_delay_ms: None,
        }),
        on_retry: Some(Arc::new(move |event| {
            seen.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(event);
        })),
        ..key()
    };
    let text = generate_summary(
        &summarize_this(),
        &test_model(false, 8192),
        None,
        None,
        &mock.stream_fn(),
        &options,
    )
    .await;
    assert_eq!(text.as_deref(), Ok("## Goal\nTest summary"));
    let events = events
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(events.len(), 3);
    assert!(matches!(
        events[2],
        RetryEvent::Finished { success: true, .. }
    ));
}

#[tokio::test]
async fn run_control_denies_second_split_summary_without_fabricating_a_response() {
    let mock = Mock::new();
    let control = bake_agent::RunControl::new(bake_agent::RunLimits {
        max_model_calls: Some(1),
        ..Default::default()
    });
    let options = SummarizationOptions {
        run_control: Some(control.clone()),
        ..key()
    };
    let result = compact(
        &split_preparation(None, 2000, true),
        &test_model(false, 8192),
        None,
        &mock.stream_fn(),
        &options,
    )
    .await;
    assert!(result.unwrap_err().contains("model-call allowance"));
    assert_eq!(control.model_calls(), 1);
    assert_eq!(
        control.stopped(),
        Some(bake_agent::RunOutcome::BudgetExhausted {
            limit: bake_agent::BudgetLimit::ModelCalls
        })
    );
}

#[tokio::test]
async fn run_control_charges_each_summary_retry() {
    let mock = Mock::new();
    let mut overloaded = summary_response();
    overloaded.stop_reason = StopReason::Error;
    overloaded.error_message = Some("overloaded_error".into());
    mock.once(overloaded);
    let control = bake_agent::RunControl::new(bake_agent::RunLimits {
        max_model_calls: Some(1),
        ..Default::default()
    });
    let options = SummarizationOptions {
        run_control: Some(control.clone()),
        retry: Some(RetryPolicy {
            enabled: true,
            max_retries: 2,
            base_delay_ms: 0,
            max_agent_delay_ms: None,
        }),
        ..key()
    };
    let result = generate_summary(
        &summarize_this(),
        &test_model(false, 8192),
        None,
        None,
        &mock.stream_fn(),
        &options,
    )
    .await;
    assert!(result.unwrap_err().contains("model-call allowance"));
    assert_eq!(control.model_calls(), 1);
}
