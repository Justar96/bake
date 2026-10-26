//! Ports of pi-cached-compaction 0.2.0
//! (`e1e20de0a7e433070414602f1105cdcac258ef4f`) `tests/cached-request.test.ts`
//! and `tests/cache-state.test.ts`, named after the test each follows, and
//! Bake's cases for [`summarize`]. Not ported: the 12-hour GPT prefix, the
//! Kimi cache option, OpenAI's 30-minute minimum, cache-warming entries,
//! and `trailingMessages`, whose work the payload check does here.

use std::sync::{Arc, Mutex, PoisonError};

use bake_ai::providers::faux::{
    faux_assistant_blocks, faux_model, faux_text, faux_thinking, faux_tool_call,
};
use bake_ai::{
    AbortController, AssistantMessage, CacheRetention, ToolResultMessage, Usage,
    assistant_message_channel,
};
use serde_json::json;

use super::*;
use crate::compaction::CompactionSettings;
use crate::session::json::JsonObject;

fn object(value: Value) -> JsonObject {
    match value {
        Value::Object(object) => object,
        _ => JsonObject::new(),
    }
}

fn reason(result: Result<Option<Value>, PrefixMismatch>) -> Option<String> {
    result.err().map(|mismatch| mismatch.reason)
}

// "a summary request that only appends turns and moves the cache breakpoint
// is sent as Pi built it".
#[test]
fn a_request_that_only_appends_turns_is_sent_as_built() {
    let captured = json!({
        "model": "m",
        "system": [{ "type": "text", "text": "sys", "cache_control": { "type": "ephemeral" } }],
        "max_tokens": 100,
        "messages": [
            { "role": "user", "content": [{ "type": "text", "text": "hi", "cache_control": { "type": "ephemeral" } }] },
            { "role": "system", "content": [], "output_config": { "effort": "high" } },
        ],
    });
    let mut rebuilt = captured.clone();
    rebuilt["max_tokens"] = json!(90);
    rebuilt["messages"] = json!([
        { "role": "user", "content": [{ "type": "text", "text": "hi" }] },
        { "role": "assistant", "content": [{ "type": "text", "text": "hello" }] },
        { "role": "user", "content": [{ "type": "text", "text": "summarize", "cache_control": { "type": "ephemeral" } }] },
    ]);
    assert_eq!(
        summary_payload(&rebuilt, &captured, "messages", &[]),
        Ok(None)
    );
}

// "a changed message or request field is reported with the text around the
// change, and nothing is sent".
#[test]
fn a_changed_message_or_field_is_reported_with_its_context() {
    let captured = json!({ "model": "m", "messages": [{ "role": "user", "content": "hi" }] });
    let edited = json!({
        "model": "m",
        "messages": [{ "role": "user", "content": "edited" }, { "role": "user", "content": "x" }],
    });
    assert_eq!(
        summary_payload(&edited, &captured, "messages", &[]),
        Err(PrefixMismatch {
            reason: "message 0 differs".into(),
            captured: Some(r#"{"role":"user","content":"hi"}"#.into()),
            rebuilt: Some(r#"{"role":"user","content":"edited"}"#.into()),
        })
    );
    let other = json!({
        "model": "other",
        "messages": [{ "role": "user", "content": "hi" }, { "role": "user", "content": "x" }],
    });
    assert_eq!(
        summary_payload(&other, &captured, "messages", &[]),
        Err(PrefixMismatch {
            reason: "request field \"model\" differs".into(),
            captured: Some("\"m\"".into()),
            rebuilt: Some("\"other\"".into()),
        })
    );
    assert_eq!(
        reason(summary_payload(
            &json!("not a payload"),
            &captured,
            "messages",
            &[]
        ))
        .as_deref(),
        Some("the provider payload is not an object")
    );
    // Bake: a request no longer than the cached one cannot extend it.
    assert_eq!(
        reason(summary_payload(&captured, &captured, "messages", &[])).as_deref(),
        Some("request is shorter than the cached one")
    );
    assert_eq!(
        reason(summary_payload(
            &json!({ "model": "m" }),
            &captured,
            "messages",
            &[]
        ))
        .as_deref(),
        Some("request has no \"messages\" array")
    );
}

// Bake: the excerpts are 40 characters before the difference and 160 from
// it, counted in characters.
#[test]
fn a_difference_is_excerpted_around_its_first_character() {
    let long = "é".repeat(300);
    let mismatch = difference(
        "x".into(),
        &format!("{long}a{long}"),
        &format!("{long}b{long}"),
    );
    let captured = mismatch.captured.unwrap_or_default();
    assert_eq!(captured.chars().count(), 200);
    assert!(captured.starts_with(&"é".repeat(40)));
    assert_eq!(captured.chars().nth(40), Some('a'));
    assert_eq!(
        mismatch.rebuilt.unwrap_or_default().chars().nth(40),
        Some('b')
    );
}

// "fields an earlier extension added to the agent's request are sent with
// the summary request".
#[test]
fn fields_a_hook_added_to_the_agent_request_are_copied() {
    let captured = json!({
        "model": "gpt",
        "service_tier": "priority",
        "input": [{ "role": "user", "content": "hi" }],
    });
    let input =
        json!([{ "role": "user", "content": "hi" }, { "role": "user", "content": "summarize" }]);
    assert_eq!(
        summary_payload(
            &json!({ "model": "gpt", "input": input }),
            &captured,
            "input",
            &[]
        ),
        Ok(Some(json!({
            "model": "gpt",
            "input": input,
            "service_tier": "priority",
        })))
    );
}

// "a field the provider keeps out of the cached prefix may differ only when
// named".
#[test]
fn a_cache_neutral_field_may_differ_only_when_named() {
    let captured = json!({
        "model": "gpt",
        "tool_choice": "auto",
        "input": [{ "role": "user", "content": "hi" }],
    });
    let rebuilt = json!({
        "model": "gpt",
        "tool_choice": "none",
        "input": [{ "role": "user", "content": "hi" }, { "role": "user", "content": "summarize" }],
    });
    assert_eq!(
        reason(summary_payload(&rebuilt, &captured, "input", &[])).as_deref(),
        Some("request field \"tool_choice\" differs")
    );
    assert_eq!(
        summary_payload(&rebuilt, &captured, "input", &["tool_choice"]),
        Ok(None)
    );
    let mut with_effort = captured.clone();
    with_effort["input"] = json!([
        { "role": "user", "content": "hi" },
        { "type": "configuration_update", "reasoning": { "effort": "high" } },
    ]);
    assert_eq!(
        summary_payload(&rebuilt, &with_effort, "input", &["tool_choice"]),
        Ok(None)
    );
}

#[test]
fn the_message_array_is_messages_or_input() {
    assert_eq!(
        message_array_key(&json!({ "messages": [] })),
        Some("messages")
    );
    assert_eq!(message_array_key(&json!({ "input": [] })), Some("input"));
    assert_eq!(message_array_key(&json!({ "contents": [] })), None);
    assert_eq!(message_array_key(&json!({ "messages": "x" })), None);
}

fn user(text: &str, timestamp: i64) -> Message {
    Message::User(UserMessage {
        content: UserContent::Text(text.into()),
        timestamp,
    })
}

fn call(timestamp: i64) -> Message {
    let mut message = faux_assistant_blocks(
        vec![
            faux_thinking("..."),
            faux_tool_call(
                "bash",
                json!({ "command": "ls" }),
                Some(&format!("t{timestamp}")),
            ),
        ],
        StopReason::ToolUse,
    );
    message.timestamp = timestamp;
    Message::Assistant(message)
}

fn system() -> Message {
    bake_ai::transcript::normalize_context(Context {
        system_prompt: Some("You are Bake.".into()),
        messages: Vec::new(),
        tools: None,
    })
    .messages()[0]
        .clone()
}

// "the kept start is the first kept message the request carries, with tool
// calls quoted and repeats counted from the end".
#[test]
fn the_kept_start_quotes_tool_calls_and_counts_repeats_from_the_end() {
    let conversation = vec![system(), user("start", 0), call(1), call(2)];
    assert_eq!(
        kept_start(&conversation, 2),
        Some(KeptStart {
            text: r#"[tool call: bash {"command":"ls"}]"#.into(),
            from_end: 2,
            count: 2,
        })
    );
    assert_eq!(
        kept_start(&conversation, 3).map(|kept| kept.text),
        Some("start".into())
    );
    let mut thinking = faux_assistant_blocks(vec![faux_thinking("...")], StopReason::Stop);
    thinking.timestamp = 3;
    assert_eq!(kept_start(&[Message::Assistant(thinking)], 1), None);
    assert_eq!(kept_start(&conversation, 0), None);
    assert_eq!(kept_start(&conversation, 4), None);
}

// Bake: a long message is quoted to 400 UTF-16 code units, without
// splitting a character.
#[test]
fn a_long_kept_message_is_cut_to_four_hundred_code_units() {
    let text = format!("{}{}", "a".repeat(399), "😀 tail");
    let quoted = message_preview(&user(&text, 1)).unwrap_or_default();
    assert_eq!(quoted, format!("{}…", "a".repeat(399)));
    let short = "a".repeat(400);
    assert_eq!(message_preview(&user(&short, 1)), Some(short));
    assert_eq!(message_preview(&user("  ", 1)), None);
}

#[test]
fn the_instruction_quotes_the_kept_start_and_the_focus() {
    let kept = KeptStart {
        text: r#"[tool call: bash {"command":"ls"}]"#.into(),
        from_end: 2,
        count: 3,
    };
    let text = summary_instruction(&kept, Some("  the API  "));
    assert!(text.contains("followed by the last 3 messages of the conversation"));
    assert!(text.contains("it is number 2 of them, counting back from the end."));
    assert!(text.contains("Tool calls are written as [tool call: name arguments]."));
    assert!(text.contains("<kept-start>\n[tool call: bash {\"command\":\"ls\"}]\n</kept-start>"));
    assert!(text.ends_with("<kept-start> text.\n\nAdditional focus: the API"));
    let plain = summary_instruction(
        &KeptStart {
            text: "next".into(),
            from_end: 1,
            count: 1,
        },
        Some(" "),
    );
    assert!(!plain.contains("Several kept messages"));
    assert!(!plain.contains("Tool calls are written"));
    assert!(plain.ends_with("<kept-start> text."));
}

// "the summary ends with one set of file lists, merged with the previous
// compaction's, even when the model copied the old ones". Bake's
// preparation already holds the previous compaction's files.
#[test]
fn the_summary_ends_with_one_set_of_file_lists() {
    let text = "## Goal\nEdit notes.\n\n<modified-files>\nold.txt\n</modified-files>";
    let mut file_ops = FileOperations::default();
    file_ops
        .read
        .extend(["a.ts".into(), "notes.txt".into(), "b.ts".into()]);
    file_ops
        .edited
        .extend(["notes.txt".into(), "old.txt".into()]);
    let (summary, read, modified) = checkpoint_summary(text, &file_ops);
    assert_eq!(
        summary,
        "## Goal\nEdit notes.\n\n<read-files>\na.ts\nb.ts\n</read-files>\n\n<modified-files>\nnotes.txt\nold.txt\n</modified-files>"
    );
    assert_eq!(read, ["a.ts", "b.ts"]);
    assert_eq!(modified, ["notes.txt", "old.txt"]);
}

fn reply(timestamp: i64, usage: Value, stop: &str) -> SessionEntry {
    let mut counts = json!({ "input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0 });
    if let (Some(counts), Some(usage)) = (counts.as_object_mut(), usage.as_object()) {
        counts.extend(usage.clone());
    }
    SessionEntry::from_json(object(json!({
        "type": "message",
        "id": format!("a{timestamp}"),
        "parentId": null,
        "timestamp": "",
        "message": {
            "role": "assistant",
            "content": [],
            "timestamp": timestamp,
            "stopReason": stop,
            "usage": counts,
        },
    })))
    .unwrap_or_else(|_| panic!("entry"))
}

fn compaction() -> SessionEntry {
    SessionEntry::from_json(object(json!({
        "type": "compaction",
        "id": "c",
        "parentId": null,
        "timestamp": "",
    })))
    .unwrap_or_else(|_| panic!("entry"))
}

fn declared(short: u64, long: u64) -> Model {
    let mut model = faux_model("claude");
    model.prompt_cache = Some(json!({ "short": short, "long": long }));
    model
}

// "the cache expires a lifetime after the latest cached request or
// cache-warming refresh, and resets at compaction", without cache warming.
#[test]
fn the_cache_expires_a_lifetime_after_the_latest_cached_request() {
    let model = declared(300, 300);
    let lifetime = Some(CacheLifetime {
        ms: 300_000,
        source: "declared by the model",
    });
    let cached = reply(1_000, json!({ "cacheRead": 500 }), "stop");
    assert_eq!(
        cache_state(&model, std::slice::from_ref(&cached), false),
        CacheState {
            lifetime,
            cached: true,
            expires_at: Some(301_000),
            replied: true,
        }
    );
    assert_eq!(
        cache_state(&model, &[reply(1_000, json!({}), "stop")], false),
        CacheState {
            lifetime,
            cached: false,
            expires_at: None,
            replied: true,
        }
    );
    let failed = reply(9_000, json!({}), "error");
    assert_eq!(
        cache_state(&model, &[cached.clone(), failed], false).expires_at,
        Some(301_000)
    );
    assert_eq!(
        cache_state(&model, &[cached, compaction()], false),
        CacheState {
            lifetime,
            cached: false,
            expires_at: None,
            replied: false,
        }
    );
}

// "the lifetime is the tier the latest write reported, the model's figure
// for that tier, or OpenAI's minimum", without OpenAI's minimum.
#[test]
fn the_lifetime_is_the_reported_tier_or_the_model_figure_for_it() {
    let claude = faux_model("claude");
    let lifetime =
        |model: &Model, entries: &[SessionEntry]| cache_state(model, entries, false).lifetime;
    let reported = |ms| {
        Some(CacheLifetime {
            ms,
            source: "reported by the provider",
        })
    };
    let long = reply(1, json!({ "cacheWrite": 500, "cacheWrite1h": 500 }), "stop");
    assert_eq!(
        lifetime(&claude, std::slice::from_ref(&long)),
        reported(3_600_000)
    );
    let short = reply(2, json!({ "cacheWrite": 40, "cacheWrite1h": 0 }), "stop");
    assert_eq!(lifetime(&claude, &[long.clone(), short]), reported(300_000));
    let read = reply(2, json!({ "cacheRead": 500 }), "stop");
    assert_eq!(
        lifetime(&claude, &[long.clone(), read]),
        reported(3_600_000),
        "a turn that only read keeps the tier"
    );
    assert_eq!(
        cache_state(
            &claude,
            &[reply(1, json!({ "cacheWrite": 500 }), "stop")],
            false
        ),
        CacheState {
            lifetime: None,
            cached: true,
            expires_at: None,
            replied: true,
        }
    );
    let failed = reply(
        1,
        json!({ "cacheWrite": 500, "cacheWrite1h": 500 }),
        "error",
    );
    assert_eq!(
        lifetime(&claude, &[failed]),
        reported(3_600_000),
        "a failed turn still reports the tier it wrote"
    );
    assert_eq!(lifetime(&claude, &[long.clone(), compaction()]), None);
    assert_eq!(
        lifetime(&declared(60, 7200), &[long]),
        Some(CacheLifetime {
            ms: 7_200_000,
            source: "declared by the model",
        })
    );
    // Bake: without a reported tier, the retention picks the model's figure.
    let read = reply(1, json!({ "cacheRead": 5 }), "stop");
    assert_eq!(
        cache_state(&declared(60, 7200), std::slice::from_ref(&read), true)
            .lifetime
            .map(|lifetime| lifetime.ms),
        Some(7_200_000)
    );
}

/// A provider for [`summarize`]: it builds the payload `{ model, reasoning,
/// messages }`, passes it through `on_payload`, and records it as sent
/// unless the request's signal was aborted by then.
#[derive(Clone)]
struct Provider {
    sent: Arc<Mutex<Vec<(Value, SimpleStreamOptions)>>>,
    response: AssistantMessage,
}

impl Provider {
    fn new(response: AssistantMessage) -> Self {
        Self {
            sent: Arc::default(),
            response,
        }
    }

    fn payload(model: &Model, messages: &[Message], options: &SimpleStreamOptions) -> Value {
        json!({
            "model": model.id,
            "reasoning": options.reasoning.map(|level| format!("{level:?}")),
            "max_tokens": options.base.max_tokens,
            "messages": messages
                .iter()
                .filter(|message| !matches!(message, Message::System(_)))
                .map(|message| serde_json::to_value(message).unwrap_or(Value::Null))
                .collect::<Vec<_>>(),
        })
    }

    fn stream_fn(&self) -> StreamFn {
        let provider = self.clone();
        Arc::new(move |model, context, options| {
            let built = Self::payload(model, context.messages(), &options);
            let payload = match &options.base.on_payload {
                Some(hook) => hook(&built, model).unwrap_or(built),
                None => built,
            };
            let (sender, stream) = assistant_message_channel();
            let mut response = provider.response.clone();
            if options
                .base
                .signal
                .as_ref()
                .is_some_and(AbortSignal::aborted)
            {
                response.stop_reason = StopReason::Aborted;
            } else {
                provider
                    .sent
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push((payload, options));
            }
            sender.finish(response);
            Ok(stream)
        })
    }

    fn sent(&self) -> Vec<(Value, SimpleStreamOptions)> {
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn summary(text: &str) -> AssistantMessage {
    let mut message = faux_assistant_blocks(vec![faux_text(text)], StopReason::Stop);
    message.usage = Usage {
        input: 20,
        cache_read: 980,
        output: 50,
        total_tokens: 1050,
        ..Usage::default()
    };
    message
}

fn tool_result(timestamp: i64) -> Message {
    Message::ToolResult(ToolResultMessage {
        tool_call_id: format!("t{}", timestamp - 1),
        tool_name: "bash".into(),
        content: vec![UserContentBlock::Text(TextContent::new("x".repeat(5_000)))],
        details: None,
        usage: None,
        nested_calls: None,
        duration_ms: None,
        is_error: false,
        timestamp,
    })
}

/// The conversation of the agent's last request, `[system, user, call]`,
/// and the one a compaction sees after its result and a reply.
fn conversations() -> (Vec<Message>, Vec<Message>) {
    let earlier = vec![system(), user("fix the bug", 1), call(2)];
    let mut now = earlier.clone();
    now.push(tool_result(3));
    let mut done = faux_assistant_blocks(vec![faux_text("Fixed.")], StopReason::Stop);
    done.timestamp = 4;
    now.push(Message::Assistant(done));
    (earlier, now)
}

fn agent_options() -> SimpleStreamOptions {
    let mut options = SimpleStreamOptions {
        reasoning: Some(bake_ai::ThinkingLevel::High),
        ..SimpleStreamOptions::default()
    };
    options.base.session_id = Some("session-1".into());
    options.base.cache_retention = Some(CacheRetention::Long);
    options
}

fn preparation() -> CompactionPreparation {
    let mut file_ops = FileOperations::default();
    file_ops.read.insert("src/lib.rs".into());
    CompactionPreparation {
        first_kept_entry_id: "entry-keep".into(),
        messages_to_summarize: Vec::new(),
        turn_prefix_messages: Vec::new(),
        is_split_turn: false,
        tokens_before: 12_345,
        previous_summary: None,
        file_ops,
        settings: CompactionSettings {
            enabled: true,
            reserve_tokens: 16_384,
            keep_recent_tokens: 20,
            threshold_percent: None,
        },
    }
}

async fn run(
    provider: &Provider,
    captured: Value,
    options: SimpleStreamOptions,
    conversation: Vec<Message>,
    signal: AbortSignal,
) -> CachedSummary {
    let model = faux_model("faux-1");
    summarize(
        CachedRequest {
            payload: Arc::new(captured),
            options,
            conversation,
            kept: 1,
        },
        &preparation(),
        &model,
        Some("the bug"),
        &provider.stream_fn(),
        signal,
        None,
    )
    .await
}

// Bake: the summary request repeats the agent's request with its options
// and cache, appends the instruction, and keeps the full tool output.
#[tokio::test]
async fn a_summary_repeats_the_agent_request_and_keeps_its_cache() {
    let model = faux_model("faux-1");
    let options = agent_options();
    let (earlier, now) = conversations();
    let captured = Provider::payload(&model, &earlier, &options);
    let provider = Provider::new(summary("## Goal\nFix the bug."));
    let outcome = run(
        &provider,
        captured,
        options,
        now,
        AbortController::new().signal(),
    )
    .await;
    let CachedSummary::Done(result) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(
        result.summary,
        "## Goal\nFix the bug.\n\n<read-files>\nsrc/lib.rs\n</read-files>"
    );
    assert_eq!(result.first_kept_entry_id, "entry-keep");
    assert_eq!(result.tokens_before, 12_345);
    assert_eq!(result.usage.map(|usage| usage.cache_read), Some(980));
    assert_eq!(
        result.details,
        Some(json!({ "readFiles": ["src/lib.rs"], "modifiedFiles": [] }))
    );
    let sent = provider.sent();
    assert_eq!(sent.len(), 1);
    let (payload, options) = &sent[0];
    assert_eq!(options.base.cache_retention, Some(CacheRetention::Long));
    assert_eq!(options.base.session_id.as_deref(), Some("session-1"));
    assert_eq!(options.reasoning, Some(bake_ai::ThinkingLevel::High));
    let messages = payload["messages"].as_array().cloned().unwrap_or_default();
    assert_eq!(messages.len(), 5);
    let output = messages[2]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(output.len(), 5_000, "the tool output is sent whole");
    let instruction = messages[4]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(instruction.contains("<kept-start>\nFixed.\n</kept-start>"));
    assert!(instruction.ends_with("Additional focus: the bug"));
}

// Bake: a request that does not repeat the agent's is never sent, and Pi's
// summary runs.
#[tokio::test]
async fn a_request_that_differs_is_not_sent() {
    let model = faux_model("faux-1");
    let (earlier, now) = conversations();
    let mut thinking = agent_options();
    thinking.reasoning = Some(bake_ai::ThinkingLevel::Low);
    let captured = Provider::payload(&model, &earlier, &thinking);
    let provider = Provider::new(summary("## Goal"));
    let outcome = run(
        &provider,
        captured,
        agent_options(),
        now.clone(),
        AbortController::new().signal(),
    )
    .await;
    assert_eq!(
        outcome,
        CachedSummary::Fallback("request field \"reasoning\" differs".into())
    );
    // An edited earlier message differs too.
    let mut edited = earlier;
    edited[1] = user("fix the other bug", 1);
    let captured = Provider::payload(&model, &edited, &agent_options());
    let outcome = run(
        &provider,
        captured,
        agent_options(),
        now,
        AbortController::new().signal(),
    )
    .await;
    assert_eq!(outcome, CachedSummary::Fallback("message 0 differs".into()));
    assert!(provider.sent().is_empty());
}

// Bake: a tool call, a cut summary, an empty one, or a failed request
// falls back; a cancel is a cancel.
#[tokio::test]
async fn an_unusable_answer_falls_back_and_a_cancel_cancels() {
    let model = faux_model("faux-1");
    let (earlier, now) = conversations();
    let captured = Provider::payload(&model, &earlier, &agent_options());
    let mut tool = summary("");
    tool.content = vec![faux_tool_call("read", json!({ "path": "a" }), Some("t9"))];
    tool.stop_reason = StopReason::ToolUse;
    let mut length = summary("partial");
    length.stop_reason = StopReason::Length;
    let mut failed = summary("");
    failed.stop_reason = StopReason::Error;
    failed.error_message = Some("overloaded".into());
    for (response, expected) in [
        (
            tool,
            "the model called a tool instead of writing the summary",
        ),
        (length, "the summary hit the output limit"),
        (summary("  "), "the model returned no summary text"),
        (failed, "overloaded"),
    ] {
        let provider = Provider::new(response);
        let outcome = run(
            &provider,
            captured.clone(),
            agent_options(),
            now.clone(),
            AbortController::new().signal(),
        )
        .await;
        assert_eq!(outcome, CachedSummary::Fallback(expected.into()));
    }
    let cancel = AbortController::new();
    cancel.abort();
    let provider = Provider::new(summary("## Goal"));
    let outcome = run(
        &provider,
        captured,
        agent_options(),
        now.clone(),
        cancel.signal(),
    )
    .await;
    assert_eq!(outcome, CachedSummary::Cancelled);
    assert!(provider.sent().is_empty());
    // A request with no message array, or no kept message to quote, sends
    // nothing.
    let outcome = run(
        &provider,
        json!({ "contents": [] }),
        agent_options(),
        now,
        AbortController::new().signal(),
    )
    .await;
    assert_eq!(
        outcome,
        CachedSummary::Fallback("faux requests are not supported".into())
    );
    let thinking = faux_assistant_blocks(vec![faux_thinking("...")], StopReason::Stop);
    let outcome = run(
        &provider,
        json!({ "messages": [] }),
        agent_options(),
        vec![system(), user("go", 1), Message::Assistant(thinking)],
        AbortController::new().signal(),
    )
    .await;
    assert!(
        matches!(outcome, CachedSummary::Fallback(reason) if reason.contains("no kept message"))
    );
    assert!(provider.sent().is_empty());
}

mod properties {
    use proptest::prelude::*;

    use super::super::preview;
    use crate::compaction::utils::strip_file_lists;

    /// Text built from tag pieces, list lines, and other characters, so
    /// blocks open, close, nest, and stay unclosed in every order.
    fn summary() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                Just("<read-files>".to_owned()),
                Just("</read-files>".to_owned()),
                Just("<modified-files>".to_owned()),
                Just("</modified-files>".to_owned()),
                Just("\n".to_owned()),
                "[a-z ./é😀]{0,6}",
            ],
            0..24,
        )
        .prop_map(|pieces| pieces.concat())
    }

    /// Whether `text` holds a block the strip would remove.
    fn has_block(text: &str) -> bool {
        [
            ("<read-files>", "</read-files>"),
            ("<modified-files>", "</modified-files>"),
        ]
        .iter()
        .any(|(open, close)| {
            text.find(open)
                .is_some_and(|start| text[start + open.len()..].contains(close))
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        // A strip leaves no closed block, trims, and changes nothing more
        // when repeated; no strip means there was no block.
        #[test]
        fn stripping_file_lists_removes_every_closed_block(text in summary()) {
            match strip_file_lists(&text) {
                None => prop_assert!(!has_block(&text)),
                Some(stripped) => {
                    prop_assert!(!has_block(&stripped), "{stripped:?}");
                    prop_assert_eq!(stripped.trim(), stripped.as_str());
                    prop_assert!(stripped.len() < text.len());
                    prop_assert_eq!(strip_file_lists(&stripped), None);
                }
            }
        }

        // A preview is the text, or its start within the limit and `…`.
        #[test]
        fn a_preview_is_a_prefix_within_the_limit(text in "[a-z é😀\n]{0,40}", limit in 0usize..30) {
            let quoted = preview(&text, limit);
            if quoted == text {
                prop_assert!(text.encode_utf16().count() <= limit);
            } else {
                let cut = quoted.strip_suffix('…').unwrap_or_else(|| panic!("{quoted:?}"));
                prop_assert!(text.starts_with(cut));
                prop_assert!(cut.encode_utf16().count() <= limit);
                prop_assert!(text.encode_utf16().count() > limit);
            }
        }
    }
}
