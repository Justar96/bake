//! Compaction and auto-retry in a session: ports of Pi v1.1.0
//! `test/agent-session-compaction.test.ts` (its live-model cases over the
//! faux provider), `test/agent-session-auto-compaction-queue.test.ts` (its
//! spied `_runAutoCompaction` cases as [`decide_compaction`] decisions),
//! and `test/agent-session-retry.test.ts`, named after the Pi test each
//! follows; plus overflow recovery, threshold compaction after and within a
//! run, abort during the retry wait, an abort during the session I/O
//! before a retry wait or a compaction request, an aborted run signal
//! between turns, shutdown without late events, and resuming a compacted
//! session.
//!
//! Pi's extension-hook cases (`compaction-extensions*.test.ts`,
//! `session_before_compact`, `session_compact`) are not ported: extensions
//! are not.

use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use bake_agent::AgentToolResult;
use bake_ai::providers::faux::{
    FauxProvider, FauxProviderOptions, FauxResponseStep, faux_assistant_blocks,
    faux_assistant_message, faux_text, faux_tool_call,
};
use bake_ai::{
    AbortController, ApiRegistry, AssistantMessage, StopReason, TranscriptContext, Usage,
};
use serde_json::{Value, json};

use super::recovery::{CheckInput, Decision, OVERFLOW_RECOVERY_FAILED, decide_compaction};
use super::*;
use crate::auth_storage::AuthStorage;
use crate::model_registry::config::ModelConfig;
use crate::session::messages::{COMPACTION_SUMMARY_PREFIX, COMPACTION_SUMMARY_SUFFIX};
use crate::session::{AgentMessage as StoredMessage, NewSessionOptions, SessionEntry};
use crate::system_prompt::DocsPaths;
use crate::test_support::TempDir;

struct Harness {
    faux: Arc<FauxProvider>,
    registry: Arc<ModelRegistry>,
    apis: Arc<ApiRegistry>,
}

fn harness(context_window: u64) -> Harness {
    let faux = Arc::new(FauxProvider::new(FauxProviderOptions::default()));
    let apis = Arc::new(ApiRegistry::new());
    apis.register(faux.clone());
    let mut registry = ModelRegistry::new(
        ModelConfig::from_value(json!({}), "models.json"),
        AuthStorage::empty(),
    );
    registry
        .add_provider_config(
            "faux",
            json!({
                "baseUrl": "http://127.0.0.1:9/unused",
                "api": faux.model().api,
                "models": [{ "id": "faux-1", "contextWindow": context_window, "maxTokens": 4096 }],
            }),
            Some("test-key".into()),
        )
        .expect("faux provider");
    Harness {
        faux,
        registry: Arc::new(registry),
        apis,
    }
}

fn settings(value: Value) -> Settings {
    Settings::from_object(value.as_object().expect("settings object"))
}

fn options(h: &Harness, session: SessionManager, settings: Settings) -> AgentSessionOptions {
    AgentSessionOptions {
        session,
        registry: Arc::clone(&h.registry),
        apis: Arc::clone(&h.apis),
        settings,
        model: h.registry.model("faux", "faux-1"),
        thinking_level: None,
        tools: Vec::new(),
        active_tool_names: Vec::new(),
        tool_prompts: Vec::new(),
        system_prompt: SystemPromptOptions::new(
            "/tmp",
            DocsPaths {
                readme: "/opt/pi/README.md".into(),
                docs: "/opt/pi/docs".into(),
                examples: "/opt/pi/examples".into(),
            },
        ),
    }
}

fn in_memory() -> SessionManager {
    SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
        .expect("session")
}

fn file_session(dir: &TempDir) -> SessionManager {
    SessionManager::create(
        &dir.path().to_string_lossy(),
        Some(&dir.join("sessions")),
        NewSessionOptions::default(),
    )
    .expect("session")
}

/// Every event as its JSON line's `type`, with the members tests check.
fn record(session: &AgentSession) -> (Arc<Mutex<Vec<Value>>>, SessionSubscription) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&events);
    let subscription = session.subscribe(move |event| {
        log.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(crate::json_event::session_event_json(event));
    });
    (events, subscription)
}

fn of_type(events: &Mutex<Vec<Value>>, kinds: &[&str]) -> Vec<Value> {
    events
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter(|event| kinds.iter().any(|kind| event["type"] == *kind))
        .cloned()
        .collect()
}

fn entry_types(session: &AgentSession) -> Vec<String> {
    lock(&session.session)
        .entries()
        .iter()
        .map(|entry| entry.entry_type().to_owned())
        .collect()
}

/// A faux response factory recording the request's context.
fn capture(seen: &Arc<Mutex<Vec<TranscriptContext>>>, reply: AssistantMessage) -> FauxResponseStep {
    let seen = Arc::clone(seen);
    FauxResponseStep::from_fn(move |context, _, _, _| {
        seen.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(context.clone());
        Ok(reply.clone())
    })
}

fn failure(message: &'static str) -> FauxResponseStep {
    FauxResponseStep::from_fn(move |_, _, _, _| Err(message.to_owned()))
}

fn user_texts(context: &TranscriptContext) -> Vec<String> {
    context
        .messages()
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => {
                Some(bake_ai::utils::text::user_content_text(&user.content, "\n"))
            }
            _ => None,
        })
        .collect()
}

fn keep_one() -> Settings {
    settings(json!({ "compaction": { "keepRecentTokens": 1 } }))
}

// ----------------------------------------------------------------------------
// Pi `test/agent-session-compaction.test.ts`
// ----------------------------------------------------------------------------

// Pi: "should trigger manual compaction via compact()", "should persist
// compaction to session file", and "should emit compaction events during
// manual compaction".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_compaction_summarizes_persists_and_reports() {
    let dir = TempDir::new("manual-compact");
    let h = harness(200_000);
    h.faux.set_responses(vec![
        faux_assistant_message("4").into(),
        faux_assistant_message("6").into(),
        // Keeping one token splits the last turn: the history and the
        // turn prefix are summarized separately.
        faux_assistant_message("## Goal\nArithmetic").into(),
        faux_assistant_message("## Original Request\n3+3").into(),
    ]);
    let session = AgentSession::create(options(&h, file_session(&dir), keep_one()))
        .await
        .expect("session");
    let (events, _subscription) = record(&session);
    session
        .prompt("What is 2+2?", Vec::new())
        .await
        .expect("one");
    session
        .prompt("What is 3+3?", Vec::new())
        .await
        .expect("two");
    let result = session.compact(None).await.expect("compacted");
    assert!(result.summary.starts_with(
        "## Goal\nArithmetic\n\n---\n\n**Turn Context (split turn):**\n\n## Original Request\n3+3"
    ));
    assert!(result.tokens_before > 0);
    assert!(result.estimated_tokens_after.is_some());
    // The recorded system message comes first, then the summary.
    let roles: Vec<String> = session
        .messages()
        .iter()
        .map(|message| message.role().to_owned())
        .filter(|role| role != "system")
        .collect();
    assert_eq!(roles.first().map(String::as_str), Some("compactionSummary"));

    let compactions: Vec<SessionEntry> = lock(&session.session)
        .entries()
        .into_iter()
        .filter(|entry| entry.entry_type() == "compaction")
        .cloned()
        .collect();
    assert_eq!(compactions.len(), 1);
    let entry = &compactions[0];
    assert_eq!(
        entry.get("summary").and_then(Value::as_str),
        Some(result.summary.as_str())
    );
    assert!(entry.get("firstKeptEntryId").is_some_and(Value::is_string));
    assert!(
        entry
            .get("tokensBefore")
            .and_then(Value::as_u64)
            .is_some_and(|n| n > 0)
    );
    assert_eq!(entry.get("fromHook"), Some(&json!(false)));

    let compaction_events = of_type(&events, &["compaction_start", "compaction_end"]);
    assert_eq!(compaction_events.len(), 2);
    assert_eq!(
        compaction_events[0],
        json!({ "type": "compaction_start", "reason": "manual" })
    );
    assert_eq!(compaction_events[1]["reason"], "manual");
    assert_eq!(compaction_events[1]["aborted"], false);
    assert_eq!(compaction_events[1]["willRetry"], false);
    assert!(!of_type(&events, &["message_end"]).is_empty());

    // On disk too.
    let file = session.session_file().expect("persisted");
    session.shutdown().await;
    let reopened = SessionManager::open(&file, None, None).expect("open");
    assert_eq!(
        reopened
            .entries()
            .iter()
            .filter(|entry| entry.entry_type() == "compaction")
            .count(),
        1
    );
}

// Pi: "should maintain valid session state after compaction" and "should
// work with --no-session mode (in-memory only)".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_compacted_in_memory_session_keeps_working() {
    let h = harness(200_000);
    h.faux.set_responses(vec![
        faux_assistant_message("Paris").into(),
        faux_assistant_message("Berlin").into(),
        faux_assistant_message("## Goal\nCapitals").into(),
        faux_assistant_message("## Original Request\nGermany").into(),
        faux_assistant_message("Rome").into(),
    ]);
    let session = AgentSession::create(options(&h, in_memory(), keep_one()))
        .await
        .expect("session");
    session.prompt("France?", Vec::new()).await.expect("one");
    session.prompt("Germany?", Vec::new()).await.expect("two");
    let result = session.compact(None).await.expect("compacted");
    assert!(!result.summary.is_empty());
    assert_eq!(
        entry_types(&session)
            .iter()
            .filter(|t| *t == "compaction")
            .count(),
        1
    );
    // A branch that ends in a compaction is already compacted.
    assert_eq!(
        session.compact(None).await.map(|_| ()),
        Err("Already compacted".into())
    );
    session.prompt("Italy?", Vec::new()).await.expect("three");
    let last = session
        .messages()
        .last()
        .and_then(|m| m.as_assistant().cloned());
    assert_eq!(
        last.map(|m| bake_ai::utils::text::assistant_text(&m))
            .as_deref(),
        Some("Rome")
    );
    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_compaction_errors_follow_pi() {
    let h = harness(200_000);
    let session = AgentSession::create(options(&h, in_memory(), keep_one()))
        .await
        .expect("session");
    let (events, _subscription) = record(&session);
    assert_eq!(
        session.compact(None).await.map(|_| ()),
        Err("Nothing to compact (session too small)".into())
    );
    let end = of_type(&events, &["compaction_end"]);
    assert_eq!(
        end,
        [json!({
            "type": "compaction_end", "reason": "manual", "aborted": false, "willRetry": false,
            "errorMessage": "Compaction failed: Nothing to compact (session too small)",
        })]
    );
    session.shutdown().await;
}

// ----------------------------------------------------------------------------
// Pi `test/agent-session-auto-compaction-queue.test.ts`
// ----------------------------------------------------------------------------

fn usage(input: u64, output: u64) -> Usage {
    Usage {
        input,
        output,
        total_tokens: input + output,
        ..Usage::default()
    }
}

fn faux_response(
    text: &str,
    usage_value: Usage,
    stop: StopReason,
    timestamp: i64,
) -> AssistantMessage {
    let mut message = faux_assistant_blocks(vec![faux_text(text)], stop);
    message.api = "faux".into();
    message.provider = "faux".into();
    message.model = "faux-1".into();
    message.usage = usage_value;
    message.timestamp = timestamp;
    message
}

fn error_response(error: &str, timestamp: i64) -> AssistantMessage {
    let mut message = faux_response("", Usage::default(), StopReason::Error, timestamp);
    message.error_message = Some(error.to_owned());
    message
}

fn user_message(text: &str, timestamp: i64) -> UserMessage {
    UserMessage {
        content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))]),
        timestamp,
    }
}

/// `_checkCompaction`'s decision over `manager`'s branch and `messages`.
fn decide(
    manager: &SessionManager,
    model: &Model,
    assistant: &AssistantMessage,
    messages: &[LiveMessage],
    skip_aborted_check: bool,
    overflow_recovery_attempted: bool,
) -> Decision {
    let branch: Vec<SessionEntry> = manager.branch_entries(None).into_iter().cloned().collect();
    let projection = manager.build_session_projection();
    let stored: Vec<StoredMessage> = messages.iter().map(StoredMessage::from).collect();
    decide_compaction(&CheckInput {
        settings: crate::compaction::DEFAULT_COMPACTION_SETTINGS,
        model: Some(model),
        assistant,
        assistant_entry_id: None,
        branch: &branch,
        projection: &projection,
        messages: &stored,
        skip_aborted_check,
        overflow_recovery_attempted,
    })
}

// Pi: "should resume after threshold compaction when only agent-level
// queued messages exist".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn threshold_compaction_resumes_for_queued_messages() {
    let h = harness(200_000);
    h.faux
        .set_responses(vec![faux_assistant_message("compacted").into()]);
    let now = bake_ai::now_ms();
    let mut manager = in_memory();
    manager
        .append_message(user_message("message to compact", now - 1000))
        .expect("user");
    manager
        .append_message(faux_response(
            "assistant response to compact",
            usage(100, 0),
            StopReason::Stop,
            now - 500,
        ))
        .expect("assistant");
    let session = AgentSession::create(options(&h, manager, keep_one()))
        .await
        .expect("session");
    session
        .agent()
        .follow_up(LiveMessage::Custom(Arc::new(CustomMessage {
            custom_type: "test".into(),
            content: UserContent::Text("Queued custom".into()),
            display: false,
            details: None,
            timestamp: now,
        })));
    assert!(session.agent().has_queued_messages());
    let resume = session
        .core
        .run_auto_compaction(CompactionReason::Threshold, false, None)
        .await;
    assert_eq!(resume, Ok(true));
    assert!(entry_types(&session).contains(&"compaction".to_owned()));
    session.shutdown().await;
}

// Pi: "should not compact repeatedly after overflow recovery already
// attempted".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overflow_recovery_runs_once() {
    let h = harness(200_000);
    let model = h.registry.model("faux", "faux-1").expect("model");
    let overflow = error_response("prompt is too long", bake_ai::now_ms());
    let manager = in_memory();
    assert_eq!(
        decide(&manager, &model, &overflow, &[], true, false),
        Decision::Run {
            reason: CompactionReason::Overflow,
            will_retry: true,
        }
    );
    let mut later = overflow.clone();
    later.timestamp += 1;
    assert_eq!(
        decide(&manager, &model, &later, &[], true, true),
        Decision::RecoveryExhausted(OVERFLOW_RECOVERY_FAILED)
    );

    // The executed check: the first attempt finds nothing to compact; the
    // second reports Pi's error.
    let session = AgentSession::create(options(&h, manager, Settings::default()))
        .await
        .expect("session");
    let (events, _subscription) = record(&session);
    let core = &session.core;
    assert_eq!(
        core.check_compaction(&overflow, None, true, Vec::new())
            .await,
        Ok(false)
    );
    assert!(core.state().overflow_recovery_attempted);
    assert_eq!(
        core.check_compaction(&later, None, true, Vec::new()).await,
        Ok(false)
    );
    assert_eq!(
        of_type(&events, &["compaction_start", "compaction_end"]),
        [json!({
            "type": "compaction_end", "reason": "overflow", "aborted": false, "willRetry": false,
            "errorMessage": OVERFLOW_RECOVERY_FAILED,
        })]
    );
    session.shutdown().await;
}

// Pi: "should ignore stale pre-compaction assistant usage on pre-prompt
// compaction checks".
#[test]
fn stale_pre_compaction_usage_is_ignored() {
    let model = harness(200_000)
        .registry
        .model("faux", "faux-1")
        .expect("model");
    let stale_at = bake_ai::now_ms() - 10_000;
    let stale = faux_response(
        "large response before compaction",
        usage(600_000, 10_000),
        StopReason::Stop,
        stale_at,
    );
    let mut manager = in_memory();
    manager
        .append_message(user_message("before compaction", stale_at - 1000))
        .expect("user");
    manager.append_message(stale.clone()).expect("assistant");
    let first = manager.entries()[0].id().to_owned();
    manager
        .append_compaction("summary", Some(&first), 610_000, None, Some(false), None)
        .expect("compaction");
    manager
        .append_message(user_message("session recovery payload", bake_ai::now_ms()))
        .expect("user");
    assert_eq!(
        decide(&manager, &model, &stale, &[], false, false),
        Decision::Skip
    );
}

// Pi: "should trigger threshold compaction for error messages using last
// successful usage".
#[test]
fn errors_fall_back_to_the_last_successful_usage() {
    let model = harness(200_000)
        .registry
        .model("faux", "faux-1")
        .expect("model");
    let threshold = model.context_window - 16384 + 1;
    let now = bake_ai::now_ms();
    let successful = faux_response(
        "large successful response",
        usage(threshold - 10_000, 10_000),
        StopReason::Stop,
        now,
    );
    let error = error_response("529 overloaded", now + 1000);
    let messages: Vec<LiveMessage> = vec![
        user_message("hello", now - 1000).into(),
        successful.into(),
        user_message("another prompt", now + 500).into(),
        error.clone().into(),
    ];
    assert_eq!(
        decide(&in_memory(), &model, &error, &messages, true, false),
        Decision::Run {
            reason: CompactionReason::Threshold,
            will_retry: false,
        }
    );
    // Pi: "should not trigger threshold compaction for error messages when
    // no prior usage exists".
    let messages: Vec<LiveMessage> = vec![
        user_message("hello", now - 1000).into(),
        error.clone().into(),
    ];
    assert_eq!(
        decide(&in_memory(), &model, &error, &messages, true, false),
        Decision::Skip
    );
}

// Pi: "should not trigger threshold compaction for error messages when only
// kept pre-compaction usage exists".
#[test]
fn kept_pre_compaction_usage_does_not_trigger() {
    let model = harness(200_000)
        .registry
        .model("faux", "faux-1")
        .expect("model");
    let before = bake_ai::now_ms() - 10_000;
    let kept = faux_response(
        "kept response from before compaction",
        usage(180_000, 10_000),
        StopReason::Stop,
        before,
    );
    let mut manager = in_memory();
    manager
        .append_message(user_message("before compaction", before - 1000))
        .expect("user");
    manager.append_message(kept.clone()).expect("assistant");
    let first = manager.entries()[0].id().to_owned();
    manager
        .append_compaction("summary", Some(&first), 190_000, None, Some(false), None)
        .expect("compaction");
    let error = error_response("529 overloaded", bake_ai::now_ms());
    let messages: Vec<LiveMessage> = vec![
        user_message("kept user msg", before - 1000).into(),
        kept.into(),
        user_message("new prompt", bake_ai::now_ms() - 500).into(),
        error.clone().into(),
    ];
    assert_eq!(
        decide(&manager, &model, &error, &messages, true, false),
        Decision::Skip
    );
}

// ----------------------------------------------------------------------------
// Automatic compaction end to end
// ----------------------------------------------------------------------------

// Pi `_checkCompaction` case 1: an overflow error is omitted from context,
// the session compacts, and the turn is retried once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overflow_errors_compact_and_retry_the_turn() {
    let h = harness(200_000);
    let seen = Arc::new(Mutex::new(Vec::new()));
    h.faux.set_responses(vec![
        faux_assistant_message("first answer").into(),
        failure("prompt is too long: 250000 tokens > 200000 maximum"),
        faux_assistant_message("## Goal\nRecovered").into(),
        faux_assistant_message("## Original Request\nsecond").into(),
        capture(&seen, faux_assistant_message("second answer")),
    ]);
    let session = AgentSession::create(options(&h, in_memory(), keep_one()))
        .await
        .expect("session");
    let (events, _subscription) = record(&session);
    session.prompt("first", Vec::new()).await.expect("one");
    session.prompt("second", Vec::new()).await.expect("two");
    assert_eq!(h.faux.call_count(), 5);
    let compaction = of_type(
        &events,
        &["compaction_start", "compaction_end", "agent_end"],
    );
    let kinds: Vec<(&str, Value)> = compaction
        .iter()
        .map(|event| {
            (
                event["type"].as_str().unwrap_or_default(),
                event.get("willRetry").cloned().unwrap_or(Value::Null),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("agent_end", json!(false)),
            ("agent_end", json!(false)),
            ("compaction_start", Value::Null),
            ("compaction_end", json!(true)),
            ("agent_end", json!(false)),
        ]
    );
    assert_eq!(compaction[2]["reason"], "overflow");
    // The failed attempt stays in the file, omitted by a context edit.
    let types = entry_types(&session);
    assert!(types.contains(&"context_edit".to_owned()), "{types:?}");
    // The retried request starts from the summary, without the error.
    let context = seen.lock().unwrap_or_else(PoisonError::into_inner)[0].clone();
    let texts = user_texts(&context);
    assert!(texts[0].starts_with(COMPACTION_SUMMARY_PREFIX), "{texts:?}");
    assert!(!context.messages().iter().any(|message| matches!(
        message,
        Message::Assistant(assistant) if assistant.stop_reason == StopReason::Error
    )));
    session.shutdown().await;
}

// Pi `_checkCompaction` case 3 after a run, and
// `_compactBeforeNextAssistantResponse` between the turns of a run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_threshold_compacts_between_turns_and_after_runs() {
    let h = harness(200_000);
    let seen = Arc::new(Mutex::new(Vec::new()));
    h.faux.set_responses(vec![
        faux_assistant_blocks(
            vec![faux_tool_call("dummy", json!({}), Some("call_1"))],
            StopReason::ToolUse,
        )
        .into(),
        faux_assistant_message("## Goal\nBetween turns").into(),
        {
            // Answered after the compaction it follows, so the check after
            // the run trusts its usage: Pi skips a response stamped in or
            // before the compaction's millisecond, so this one waits for
            // the clock to move on.
            let seen = Arc::clone(&seen);
            FauxResponseStep::from_fn(move |context, _, _, _| {
                seen.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(context.clone());
                std::thread::sleep(Duration::from_millis(3));
                Ok(faux_assistant_message("done"))
            })
        },
        faux_assistant_message("## Goal\nAfter the run").into(),
    ]);
    let tool = AgentTool::new(
        "dummy",
        "dummy",
        "Dummy tool",
        json!({ "type": "object" }),
        |_, _, _, _| {
            Box::pin(async {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new("tool output"))],
                    ..AgentToolResult::default()
                })
            })
        },
    );
    // A context over 10 tokens crosses the threshold.
    let settings = settings(json!({
        "compaction": { "reserveTokens": 199_990, "keepRecentTokens": 1 },
    }));
    let session = AgentSession::create(AgentSessionOptions {
        tools: vec![Arc::new(tool)],
        active_tool_names: vec!["dummy".into()],
        ..options(&h, in_memory(), settings)
    })
    .await
    .expect("session");
    let (events, _subscription) = record(&session);
    session
        .prompt("run the tool", Vec::new())
        .await
        .expect("prompt");
    assert_eq!(h.faux.call_count(), 4);
    let order: Vec<String> = of_type(
        &events,
        &[
            "tool_execution_end",
            "compaction_start",
            "compaction_end",
            "message_start",
            "agent_end",
        ],
    )
    .iter()
    .map(|event| {
        let kind = event["type"].as_str().unwrap_or_default();
        match event.get("reason") {
            Some(reason) => format!("{kind}:{}", reason.as_str().unwrap_or_default()),
            None => kind.to_owned(),
        }
    })
    .collect();
    assert_eq!(
        order,
        [
            "message_start",
            "message_start",
            "message_start",
            "tool_execution_end",
            "message_start",
            "compaction_start:threshold",
            "compaction_end:threshold",
            "message_start",
            "agent_end",
            "compaction_start:threshold",
            "compaction_end:threshold",
        ]
    );
    // The turn after the mid-run compaction sends the compacted context.
    let context = seen.lock().unwrap_or_else(PoisonError::into_inner)[0].clone();
    assert!(user_texts(&context)[0].contains("Between turns"));
    assert_eq!(
        entry_types(&session)
            .iter()
            .filter(|t| *t == "compaction")
            .count(),
        2
    );
    session.shutdown().await;
}

// Resuming a compacted session sends exactly the compacted context: the
// recorded system message, the summary, the kept messages, and the prompt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resumed_session_sends_exactly_the_compacted_context() {
    let dir = TempDir::new("resume-compacted");
    let h = harness(200_000);
    let seen = Arc::new(Mutex::new(Vec::new()));
    h.faux.set_responses(vec![
        faux_assistant_message("first answer").into(),
        faux_assistant_message("second answer").into(),
        faux_assistant_message("## Goal\nResume me").into(),
        faux_assistant_message("## Original Request\nsecond question").into(),
        capture(&seen, faux_assistant_message("resumed answer")),
    ]);
    let session = AgentSession::create(options(&h, file_session(&dir), keep_one()))
        .await
        .expect("session");
    session
        .prompt("first question", Vec::new())
        .await
        .expect("one");
    session
        .prompt("second question", Vec::new())
        .await
        .expect("two");
    let result = session.compact(None).await.expect("compacted");
    let live: Vec<Message> = convert_to_llm(&session.messages());
    let file = session.session_file().expect("persisted");
    session.shutdown().await;

    let reopened = SessionManager::open(&file, None, None).expect("open");
    let resumed = AgentSession::create(AgentSessionOptions {
        model: None,
        ..options(&h, reopened, keep_one())
    })
    .await
    .expect("resumed");
    resumed
        .prompt("third question", Vec::new())
        .await
        .expect("three");
    let context = seen.lock().unwrap_or_else(PoisonError::into_inner)[0].clone();
    let sent = context.messages();
    // Everything before the new prompt is the live session's compacted
    // transcript, byte for byte.
    assert_eq!(sent.len(), live.len() + 1);
    assert_eq!(&sent[..live.len()], live.as_slice());
    let texts = user_texts(&context);
    assert_eq!(
        texts[0],
        format!(
            "{COMPACTION_SUMMARY_PREFIX}{}{COMPACTION_SUMMARY_SUFFIX}",
            result.summary
        )
    );
    assert_eq!(texts.last().map(String::as_str), Some("third question"));
    assert!(!texts.iter().any(|text| text == "first question"));
    assert!(matches!(sent[0], Message::System(_)));
    resumed.shutdown().await;
}

// ----------------------------------------------------------------------------
// Pi `test/agent-session-retry.test.ts`
// ----------------------------------------------------------------------------

fn retry_settings(max_retries: u32, max_agent_delay_ms: u64) -> Settings {
    settings(json!({
        "retry": {
            "enabled": true, "maxRetries": max_retries, "baseDelayMs": 1,
            "maxAgentDelayMs": max_agent_delay_ms,
        },
    }))
}

/// Faux responses: `fail` errors with `error`, then successes.
fn failing(h: &Harness, fail: usize, error: &'static str) {
    let mut responses: Vec<FauxResponseStep> = (0..fail).map(|_| failure(error)).collect();
    responses.extend((0..3).map(|_| faux_assistant_message("Success").into()));
    h.faux.set_responses(responses);
}

fn retry_log(events: &Mutex<Vec<Value>>) -> Vec<String> {
    of_type(events, &["auto_retry_start", "auto_retry_end"])
        .iter()
        .map(|event| match event["type"].as_str() {
            Some("auto_retry_start") => format!("start:{}", event["attempt"]),
            _ => format!("end:success={}", event["success"]),
        })
        .collect()
}

// Pi: "retries after a transient error and succeeds".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retries_a_transient_error() {
    let h = harness(200_000);
    failing(&h, 1, "overloaded_error");
    let session = AgentSession::create(options(&h, in_memory(), retry_settings(3, 60_000)))
        .await
        .expect("session");
    let (events, _subscription) = record(&session);
    session.prompt("Test", Vec::new()).await.expect("prompt");
    assert_eq!(h.faux.call_count(), 2);
    assert_eq!(retry_log(&events), ["start:1", "end:success=true"]);
    assert!(!session.is_retrying());
    // The failed run announced its retry.
    let ends = of_type(&events, &["agent_end"]);
    assert_eq!(ends[0]["willRetry"], true);
    assert_eq!(ends[1]["willRetry"], false);
    // The failed attempt is kept in the file and omitted from context.
    assert!(entry_types(&session).contains(&"context_edit".to_owned()));
    assert!(!session.messages().iter().any(|message| {
        message
            .as_assistant()
            .is_some_and(|assistant| assistant.stop_reason == StopReason::Error)
    }));
    session.shutdown().await;
}

// Pi: "exhausts max retries and emits failure".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exhausted_retries_report_failure() {
    let h = harness(200_000);
    failing(&h, 99, "overloaded_error");
    let session = AgentSession::create(options(&h, in_memory(), retry_settings(2, 60_000)))
        .await
        .expect("session");
    let (events, _subscription) = record(&session);
    session.prompt("Test", Vec::new()).await.expect("prompt");
    assert_eq!(h.faux.call_count(), 3);
    let log = retry_log(&events);
    assert!(log.contains(&"start:1".to_owned()));
    assert!(log.contains(&"start:2".to_owned()));
    assert!(log.contains(&"end:success=false".to_owned()));
    let end = of_type(&events, &["auto_retry_end"]);
    assert_eq!(end[0]["finalError"], "overloaded_error");
    assert!(!session.is_retrying());
    assert_eq!(session.retry_attempt(), 0);
    session.shutdown().await;
}

// Pi: "caps agent retry delay" (#8826).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_delays_are_capped() {
    let h = harness(200_000);
    failing(&h, 4, "overloaded_error");
    let session = AgentSession::create(options(&h, in_memory(), retry_settings(5, 5)))
        .await
        .expect("session");
    let (events, _subscription) = record(&session);
    session.prompt("Test", Vec::new()).await.expect("prompt");
    let delays: Vec<Value> = of_type(&events, &["auto_retry_start"])
        .iter()
        .map(|event| event["delayMs"].clone())
        .collect();
    assert_eq!(delays, [json!(1), json!(2), json!(4), json!(5)]);
    session.shutdown().await;
}

// Pi: "prompt waits for retry completion even when assistant message_end
// handling is delayed", with a slow agent listener in place of the
// extension.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_waits_for_a_retry_behind_slow_listeners() {
    let h = harness(200_000);
    failing(&h, 1, "overloaded_error");
    let session = AgentSession::create(options(&h, in_memory(), retry_settings(3, 60_000)))
        .await
        .expect("session");
    let _slow = session.agent().subscribe(|event, _| {
        let slow =
            matches!(event, AgentEvent::MessageEnd { message } if message.as_assistant().is_some());
        Box::pin(async move {
            if slow {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        })
    });
    session.prompt("Test", Vec::new()).await.expect("prompt");
    assert_eq!(h.faux.call_count(), 2);
    assert!(!session.is_retrying());
    session.shutdown().await;
}

// Pi: "retries provider network_error failures".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retries_network_errors() {
    let h = harness(200_000);
    failing(&h, 1, "Provider finish_reason: network_error");
    let session = AgentSession::create(options(&h, in_memory(), retry_settings(3, 60_000)))
        .await
        .expect("session");
    let (events, _subscription) = record(&session);
    session.prompt("Test", Vec::new()).await.expect("prompt");
    assert_eq!(h.faux.call_count(), 2);
    assert_eq!(retry_log(&events), ["start:1", "end:success=true"]);
    session.shutdown().await;
}

// Pi: "prompt waits for full agent loop when retry produces tool calls".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retry_runs_its_whole_tool_loop() {
    let h = harness(200_000);
    h.faux.set_responses(vec![
        failure("overloaded_error"),
        faux_assistant_blocks(
            vec![
                faux_text("Looking that up now."),
                faux_tool_call("echo", json!({ "text": "hello" }), Some("call_1")),
            ],
            StopReason::ToolUse,
        )
        .into(),
        faux_assistant_message("Final answer.").into(),
        faux_assistant_message("Follow-up answer.").into(),
    ]);
    let executed = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&executed);
    let echo = AgentTool::new(
        "echo",
        "Echo",
        "Echo text back",
        json!({ "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] }),
        move |_, _, _, _| {
            count.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new("echoed"))],
                    ..AgentToolResult::default()
                })
            })
        },
    );
    let session = AgentSession::create(AgentSessionOptions {
        tools: vec![Arc::new(echo)],
        active_tool_names: vec!["echo".into()],
        ..options(&h, in_memory(), retry_settings(3, 60_000))
    })
    .await
    .expect("session");
    session.prompt("Test", Vec::new()).await.expect("prompt");
    assert_eq!(h.faux.call_count(), 3);
    assert_eq!(executed.load(Ordering::SeqCst), 1);
    assert!(!session.is_streaming());
    session
        .prompt("Follow-up", Vec::new())
        .await
        .expect("follow-up");
    assert_eq!(h.faux.call_count(), 4);
    session.shutdown().await;
}

// Pi `_prepareRetry`: an abort during the backoff cancels the retry and
// ends the run at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abort_cancels_the_retry_wait() {
    let h = harness(200_000);
    failing(&h, 1, "overloaded_error");
    let slow = settings(json!({ "retry": { "baseDelayMs": 60_000 } }));
    let session = Arc::new(
        AgentSession::create(options(&h, in_memory(), slow))
            .await
            .expect("session"),
    );
    let (events, _subscription) = record(&session);
    let run = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("Test", Vec::new()).await })
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !session.is_retrying() {
        assert!(tokio::time::Instant::now() < deadline, "never retried");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    tokio::time::timeout(Duration::from_secs(5), session.abort())
        .await
        .expect("abort settles");
    let result = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("prompt ends")
        .expect("joined");
    assert_eq!(result, Ok(()));
    assert_eq!(h.faux.call_count(), 1, "the retry never ran");
    assert_eq!(
        of_type(&events, &["auto_retry_end", "agent_settled"]),
        [
            json!({ "type": "auto_retry_end", "success": false, "attempt": 1, "finalError": "Retry cancelled" }),
            json!({ "type": "agent_settled", "aborted": true }),
        ]
    );
    assert!(!session.is_retrying());
    assert_eq!(session.retry_attempt(), 0);
    session.shutdown().await;
}

/// Holds the session's lock on another thread from the first `kind` event
/// until [`Self::release`], so the session's next I/O waits there.
struct SessionHold {
    held: Arc<AtomicBool>,
    release: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
    _subscription: SessionSubscription,
}

impl SessionHold {
    fn on(session: &AgentSession, kind: &'static str) -> Self {
        let held = Arc::new(AtomicBool::new(false));
        let thread = Arc::new(Mutex::new(None));
        let (release, released) = std::sync::mpsc::channel::<()>();
        let released = Mutex::new(Some(released));
        let shared = Arc::clone(&session.session);
        let subscription = session.subscribe({
            let held = Arc::clone(&held);
            let thread = Arc::clone(&thread);
            move |event| {
                if crate::json_event::session_event_json(event)["type"] != kind {
                    return;
                }
                let Some(released) = released
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                else {
                    return;
                };
                let (shared, held) = (Arc::clone(&shared), Arc::clone(&held));
                let (locked, is_locked) = std::sync::mpsc::channel::<()>();
                let handle = std::thread::spawn(move || {
                    let _guard = lock(&shared);
                    held.store(true, Ordering::SeqCst);
                    let _ = locked.send(());
                    let _ = released.recv();
                });
                // The listener returns only once the lock is held, so the
                // session's next I/O after this event waits for it.
                let _ = is_locked.recv();
                *thread.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);
            }
        });
        Self {
            held,
            release: Mutex::new(Some(release)),
            thread,
            _subscription: subscription,
        }
    }

    async fn wait_held(&self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !self.held.load(Ordering::SeqCst) {
            assert!(tokio::time::Instant::now() < deadline, "never held");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    fn release(&self) {
        self.release
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let handle = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(handle) = handle {
            handle.join().expect("hold thread");
        }
    }
}

/// Starts `session.abort()` and returns once it requested the abort, while
/// it waits for the session to settle.
async fn start_abort(session: &Arc<AgentSession>) -> tokio::task::JoinHandle<()> {
    let abort = {
        let session = Arc::clone(session);
        tokio::spawn(async move { session.abort().await })
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !session.core.aborted() {
        assert!(tokio::time::Instant::now() < deadline, "never aborted");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    // Past `abort_retry_and_compaction`, which found no controller.
    tokio::time::sleep(Duration::from_millis(20)).await;
    abort
}

// Pi creates `_retryAbortController` before its first `await`: an abort
// while the failed attempt is being omitted, before the backoff starts,
// still cancels the wait.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abort_during_the_retry_omission_cancels_the_wait() {
    let h = harness(200_000);
    failing(&h, 1, "overloaded_error");
    let slow = settings(json!({ "retry": { "baseDelayMs": 60_000 } }));
    let session = Arc::new(
        AgentSession::create(options(&h, in_memory(), slow))
            .await
            .expect("session"),
    );
    let (events, _subscription) = record(&session);
    let hold = SessionHold::on(&session, "auto_retry_start");
    let run = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("Test", Vec::new()).await })
    };
    hold.wait_held().await;
    let abort = start_abort(&session).await;
    hold.release();
    tokio::time::timeout(Duration::from_secs(5), abort)
        .await
        .expect("abort settles")
        .expect("joined");
    let result = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("prompt ends")
        .expect("joined");
    assert_eq!(result, Ok(()));
    assert_eq!(h.faux.call_count(), 1, "the retry never ran");
    assert_eq!(
        of_type(&events, &["auto_retry_end", "agent_settled"]),
        [
            json!({ "type": "auto_retry_end", "success": false, "attempt": 1, "finalError": "Retry cancelled" }),
            json!({ "type": "agent_settled", "aborted": true }),
        ]
    );
    assert!(!session.is_retrying());
    session.shutdown().await;
}

// Pi creates `_autoCompactionAbortController` before its first `await`: an
// abort while the post-run check reads the branch cancels the compaction
// before its summarization request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abort_during_the_compaction_check_cancels_the_compaction() {
    let h = harness(200_000);
    h.faux.set_responses(vec![
        faux_assistant_message("first answer").into(),
        faux_assistant_message("## Goal\nNever sent").into(),
        faux_assistant_message("## Original Request\nNever sent").into(),
    ]);
    // A context over 10 tokens crosses the threshold.
    let settings = settings(json!({
        "compaction": { "reserveTokens": 199_990, "keepRecentTokens": 1 },
    }));
    let session = Arc::new(
        AgentSession::create(options(&h, in_memory(), settings))
            .await
            .expect("session"),
    );
    let (events, _subscription) = record(&session);
    let hold = SessionHold::on(&session, "agent_end");
    let run = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("a question to compact", Vec::new()).await })
    };
    hold.wait_held().await;
    let abort = start_abort(&session).await;
    hold.release();
    tokio::time::timeout(Duration::from_secs(5), abort)
        .await
        .expect("abort settles")
        .expect("joined");
    let result = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("prompt ends")
        .expect("joined");
    assert_eq!(result, Ok(()));
    assert_eq!(h.faux.call_count(), 1, "no summarization request");
    let compaction = of_type(&events, &["compaction_start", "compaction_end"]);
    assert_eq!(compaction.len(), 2, "{compaction:?}");
    assert_eq!(compaction[1]["reason"], "threshold");
    assert_eq!(compaction[1]["aborted"], true);
    assert!(!entry_types(&session).contains(&"compaction".to_owned()));
    session.shutdown().await;
}

// `_compactBeforeNextAssistantResponse` runs under the run's signal: an
// aborted run compacts nothing between its turns.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_aborted_run_signal_cancels_compaction_between_turns() {
    let h = harness(200_000);
    let now = bake_ai::now_ms();
    let mut manager = in_memory();
    manager
        .append_message(user_message("message to compact", now - 1000))
        .expect("user");
    manager
        .append_message(faux_response(
            "assistant response to compact",
            usage(100, 0),
            StopReason::Stop,
            now - 500,
        ))
        .expect("assistant");
    // A context over 10 tokens crosses the threshold.
    let settings = settings(json!({
        "compaction": { "reserveTokens": 199_990, "keepRecentTokens": 1 },
    }));
    let session = AgentSession::create(options(&h, manager, settings))
        .await
        .expect("session");
    let (events, _subscription) = record(&session);
    let run = AbortController::new();
    run.abort();
    let signal = run.signal();
    assert_eq!(
        session
            .core
            .run_auto_compaction(CompactionReason::Threshold, false, Some(&signal))
            .await,
        Ok(false)
    );
    assert!(
        session
            .core
            .compact_before_next_response(Vec::new(), Some(signal))
            .await
            .is_none()
    );
    assert_eq!(h.faux.call_count(), 0);
    let ends = of_type(&events, &["compaction_end"]);
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0]["aborted"], true);
    assert!(!entry_types(&session).contains(&"compaction".to_owned()));
    session.shutdown().await;
}

// Pi's `dispose` clears `_eventListeners`: the retry wait and run that
// shutdown cancels end without events.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_cancels_without_late_events() {
    let h = harness(200_000);
    failing(&h, 1, "overloaded_error");
    let slow = settings(json!({ "retry": { "baseDelayMs": 60_000 } }));
    let session = Arc::new(
        AgentSession::create(options(&h, in_memory(), slow))
            .await
            .expect("session"),
    );
    let (events, _subscription) = record(&session);
    let run = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("Test", Vec::new()).await })
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !session.is_retrying() {
        assert!(tokio::time::Instant::now() < deadline, "never retried");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    tokio::time::timeout(Duration::from_secs(5), session.shutdown())
        .await
        .expect("shutdown settles");
    tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("prompt ends")
        .expect("joined")
        .expect("prompt");
    assert!(
        of_type(&events, &["auto_retry_end", "agent_settled"]).is_empty(),
        "{events:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_retry_and_compaction_leave_errors_alone() {
    let h = harness(200_000);
    failing(&h, 1, "overloaded_error");
    let off =
        settings(json!({ "retry": { "enabled": false }, "compaction": { "enabled": false } }));
    let session = AgentSession::create(options(&h, in_memory(), off))
        .await
        .expect("session");
    let (events, _subscription) = record(&session);
    assert!(!session.auto_retry_enabled());
    assert!(!session.auto_compaction_enabled());
    session.prompt("Test", Vec::new()).await.expect("prompt");
    assert_eq!(h.faux.call_count(), 1);
    assert!(retry_log(&events).is_empty());
    session.shutdown().await;
}
