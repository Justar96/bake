//! The scope 10 compaction proof: tool-call pairing across every cut and
//! every compaction reason, summary retries, prompts queued behind a manual
//! compaction, restart after compaction, and a writer killed while it
//! appends the compaction entry.
//!
//! Adapted from Pi v1.3.0 (`71023aec1546d6d677a65669c5665c0927ce0dd0`),
//! MIT: `packages/coding-agent/src/core/agent-session.ts` (`compact`,
//! `_runDefaultCompaction`, `abortCompaction`),
//! `src/core/compaction/compaction.ts` (`findCutPoint`,
//! `prepareCompaction`), and the regression test
//! `test/suite/regressions/6647-compaction-retries-transient-stream-drop.test.ts`,
//! whose five cases the summary-retry tests below follow over the faux
//! provider, named after the Pi case each follows. Pi has no test for the
//! other cases; they are Bake's own and check the session file, the
//! rebuilt request, and the queue as the scope 10 proof requires.

use std::collections::HashSet;

use bake_ai::providers::faux::FauxFuture;
use tokio::sync::Notify;

use super::*;
use crate::compaction::{CompactionSettings, DEFAULT_COMPACTION_SETTINGS, prepare_compaction};
use crate::session::{LeafSelector, build_session_context, unanswered_tool_calls};

// ----------------------------------------------------------------------------
// Shared checks
// ----------------------------------------------------------------------------

/// What a provider would refuse in `messages`: a tool result that answers
/// no call of the assistant message it follows, or a call left without a
/// result before the next message. The rule of `session::unanswered` and
/// of the group tests.
fn provider_problem(messages: &[Message]) -> Option<String> {
    let mut open: Vec<String> = Vec::new();
    let mut after_assistant = false;
    for message in messages {
        match message {
            Message::ToolResult(result) => {
                if !after_assistant || !open.contains(&result.tool_call_id) {
                    return Some(format!("{} answers no open call", result.tool_call_id));
                }
                open.retain(|id| id != &result.tool_call_id);
            }
            other => {
                if !open.is_empty() {
                    return Some(format!("calls {open:?} have no result"));
                }
                after_assistant = false;
                if let Message::Assistant(assistant) = other {
                    open = assistant.tool_calls().map(|call| call.id.clone()).collect();
                    after_assistant = true;
                }
            }
        }
    }
    (!open.is_empty()).then(|| format!("calls {open:?} have no result"))
}

fn tool_results(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult(result) => Some(result.tool_call_id.clone()),
            _ => None,
        })
        .collect()
}

/// The requests that were sent with a compacted context: their first user
/// message is a compaction summary. Summary requests are not.
fn compacted_requests(seen: &Mutex<Vec<TranscriptContext>>) -> Vec<Vec<Message>> {
    seen.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter(|context| {
            user_texts(context)
                .first()
                .is_some_and(|text| text.starts_with(COMPACTION_SUMMARY_PREFIX))
        })
        .map(|context| context.messages().to_vec())
        .collect()
}

/// The session's projection pairs every call with its result, and every
/// compacted request it sent does.
fn assert_paired(session: &AgentSession, seen: &Mutex<Vec<TranscriptContext>>, label: &str) {
    let projection = lock(&session.session).build_session_projection();
    assert_eq!(unanswered_tool_calls(&projection), [], "{label}");
    assert!(
        projection
            .messages
            .iter()
            .any(|message| message.role() == Some("compactionSummary")),
        "{label}: the session compacted"
    );
    if let Some(problem) = provider_problem(&llm_of_stored(&projection.messages)) {
        panic!("{label}: projection: {problem}");
    }
    let requests = compacted_requests(seen);
    assert!(!requests.is_empty(), "{label}: no compacted request");
    for request in &requests {
        if let Some(problem) = provider_problem(request) {
            panic!("{label}: request: {problem}");
        }
    }
}

/// A tool answering `tool output`.
fn dummy_tool() -> AgentTool {
    AgentTool::new(
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
    )
}

/// An assistant calling the dummy tool twice in parallel, with arguments
/// large enough that keeping the calls' results and a short answer
/// still needs the calls.
fn parallel_calls() -> FauxResponseStep {
    faux_assistant_blocks(
        vec![
            faux_tool_call("dummy", json!({ "pad": "x".repeat(400) }), Some("call_1")),
            faux_tool_call("dummy", json!({}), Some("call_2")),
        ],
        StopReason::ToolUse,
    )
    .into()
}

fn with_tool(h: &Harness, session: SessionManager, settings: Settings) -> AgentSessionOptions {
    AgentSessionOptions {
        tools: vec![Arc::new(dummy_tool())],
        active_tool_names: vec!["dummy".into()],
        ..options(h, session, settings)
    }
}

/// `count` replies that record their request.
fn captures(
    seen: &Arc<Mutex<Vec<TranscriptContext>>>,
    text: &str,
    count: usize,
) -> Vec<FauxResponseStep> {
    (0..count)
        .map(|_| capture(seen, faux_assistant_message(text)))
        .collect()
}

fn compactions_in(session: &AgentSession) -> usize {
    entry_types(session)
        .iter()
        .filter(|kind| *kind == "compaction")
        .count()
}

// ----------------------------------------------------------------------------
// 1. Tool calls and their results across compaction
// ----------------------------------------------------------------------------

fn stored(value: Value) -> StoredMessage {
    StoredMessage::from_json(value.as_object().expect("object").clone())
}

fn assistant_json(content: Value, stop: &str, timestamp: i64) -> Value {
    json!({
        "role": "assistant", "content": content, "api": "faux", "provider": "faux",
        "model": "faux-1", "usage": serde_json::to_value(usage(10, 1)).expect("usage"),
        "stopReason": stop, "timestamp": timestamp,
    })
}

fn call(id: &str, pad: usize) -> Value {
    json!({ "type": "toolCall", "id": id, "name": "read", "arguments": { "pad": "p".repeat(pad) } })
}

fn result_json(id: &str, size: usize, timestamp: i64) -> Value {
    json!({
        "role": "toolResult", "toolCallId": id, "toolName": "read",
        "content": [{ "type": "text", "text": "r".repeat(size) }], "isError": false,
        "timestamp": timestamp,
    })
}

/// Two tasks: the first calls two tools in parallel and then a third
/// alone, the second calls one; each ends with an answer. Tool results and
/// arguments of different sizes put the cut at every kind of entry.
fn tool_conversation() -> SessionManager {
    let mut manager = in_memory();
    let messages = [
        json!({ "role": "user", "content": "first task", "timestamp": 1 }),
        assistant_json(
            json!([{ "type": "text", "text": "looking" }, call("c1", 40), call("c2", 120)]),
            "toolUse",
            2,
        ),
        result_json("c1", 400, 3),
        result_json("c2", 40, 4),
        assistant_json(json!([call("c3", 200)]), "toolUse", 5),
        result_json("c3", 200, 6),
        assistant_json(json!([{ "type": "text", "text": "first done" }]), "stop", 7),
        json!({ "role": "user", "content": "second task", "timestamp": 8 }),
        assistant_json(json!([call("c4", 16)]), "toolUse", 9),
        result_json("c4", 800, 10),
        assistant_json(
            json!([{ "type": "text", "text": "second done" }]),
            "stop",
            11,
        ),
    ];
    for message in messages {
        manager.append_message(stored(message)).expect("append");
    }
    manager
}

// Pi `findCutPoint` never cuts at a tool result. For every recent-token
// budget: the first kept entry is a user or assistant message, the
// summarized history and turn prefix each pair their calls, and the
// context rebuilt after the compaction entry pairs every call with its
// result and leaves no call for the synthetic result to close.
#[test]
fn no_cut_point_separates_a_tool_call_from_its_result() {
    let mut kept_roles = HashSet::new();
    let mut kept_ids = HashSet::new();
    let mut kept_at_calls = 0;
    for keep in 1..=700 {
        let mut manager = tool_conversation();
        let branch: Vec<SessionEntry> = manager.branch_entries(None).into_iter().cloned().collect();
        let settings = CompactionSettings {
            keep_recent_tokens: keep,
            ..DEFAULT_COMPACTION_SETTINGS
        };
        let Some(preparation) = prepare_compaction(&branch, settings) else {
            continue;
        };
        let first_kept = branch
            .iter()
            .find(|entry| entry.id() == preparation.first_kept_entry_id)
            .expect("the first kept entry is on the branch");
        let role = first_kept.message_role().unwrap_or_default().to_owned();
        assert_ne!(role, "toolResult", "keep {keep}");
        if first_kept
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
            .is_some_and(|blocks| blocks.iter().any(|block| block["type"] == "toolCall"))
        {
            kept_at_calls += 1;
        }
        kept_roles.insert(role);
        kept_ids.insert(preparation.first_kept_entry_id.clone());
        for (part, messages) in [
            ("history", &preparation.messages_to_summarize),
            ("turn prefix", &preparation.turn_prefix_messages),
        ] {
            if let Some(problem) = provider_problem(&llm_of_stored(messages)) {
                panic!("keep {keep}: {part}: {problem}");
            }
        }
        manager
            .append_compaction(
                "summary",
                Some(&preparation.first_kept_entry_id),
                preparation.tokens_before,
                None,
                Some(false),
                None,
            )
            .expect("compaction");
        let projection = manager.build_session_projection();
        assert_eq!(unanswered_tool_calls(&projection), [], "keep {keep}");
        let request = llm_of_stored(&projection.messages);
        if let Some(problem) = provider_problem(&request) {
            panic!("keep {keep}: {problem}");
        }
        // Every result in the kept tail kept its call.
        let results = tool_results(&request);
        for id in &results {
            assert!(
                request.iter().any(|message| matches!(
                    message,
                    Message::Assistant(assistant)
                        if assistant.tool_calls().any(|call| &call.id == id)
                )),
                "keep {keep}: {id}"
            );
        }
    }
    // The sweep cut at user messages and at assistant messages holding calls.
    assert!(kept_roles.contains("user"), "{kept_roles:?}");
    assert!(kept_roles.contains("assistant"), "{kept_roles:?}");
    assert!(kept_ids.len() >= 5, "{kept_ids:?}");
    assert!(kept_at_calls > 0, "no cut at an assistant holding calls");
}

// Threshold compaction between the turns of a run, after the tool results:
// the kept tail starts at the assistant message holding the calls, so the
// next request sends the summary, both calls, and both results.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn threshold_compaction_between_turns_keeps_calls_with_their_results() {
    let h = harness(200_000);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut responses = vec![
        parallel_calls(),
        faux_assistant_message("## Goal\nBetween turns").into(),
    ];
    responses.extend(captures(&seen, "done", 4));
    h.faux.set_responses(responses);
    let settings = settings(json!({
        "compaction": { "reserveTokens": 199_990, "keepRecentTokens": 1 },
    }));
    let session = AgentSession::create(with_tool(&h, in_memory(), settings))
        .await
        .expect("session");
    session
        .prompt("run the tool", Vec::new())
        .await
        .expect("prompt");
    assert_paired(&session, &seen, "threshold");
    let first = compacted_requests(&seen).remove(0);
    assert_eq!(tool_results(&first), ["call_1", "call_2"]);
    session.shutdown().await;
}

// Overflow recovery after a tool turn: the failed attempt is omitted, the
// session compacts, and the retried request pairs every call it sends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overflow_compaction_keeps_calls_with_their_results() {
    let h = harness(200_000);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut responses = vec![
        parallel_calls(),
        failure("prompt is too long: 250000 tokens > 200000 maximum"),
    ];
    responses.extend(captures(&seen, "## Goal\nRecovered", 5));
    h.faux.set_responses(responses);
    let session = AgentSession::create(with_tool(&h, in_memory(), keep_one()))
        .await
        .expect("session");
    let (events, _subscription) = record(&session);
    session
        .prompt("run the tool", Vec::new())
        .await
        .expect("prompt");
    let ends = of_type(&events, &["compaction_end"]);
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0]["reason"], "overflow");
    assert_eq!(ends[0]["willRetry"], true);
    assert_paired(&session, &seen, "overflow");
    session.shutdown().await;
}

// Manual compaction after a finished tool turn, with a budget the short
// answer and the two results do not fill: the cut lands on the calls, and
// the next prompt's request sends them with their results.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_compaction_keeps_calls_with_their_results() {
    let h = harness(200_000);
    let seen = Arc::new(Mutex::new(Vec::new()));
    h.faux.set_responses(vec![
        parallel_calls(),
        faux_assistant_message("ok").into(),
        faux_assistant_message("## Goal\nManual").into(),
        capture(&seen, faux_assistant_message("next answer")),
    ]);
    let settings = settings(json!({ "compaction": { "keepRecentTokens": 20 } }));
    let session = AgentSession::create(with_tool(&h, in_memory(), settings))
        .await
        .expect("session");
    session
        .prompt("run the tool", Vec::new())
        .await
        .expect("prompt");
    session.compact(None).await.expect("compacted");
    session.prompt("next", Vec::new()).await.expect("next");
    assert_eq!(h.faux.call_count(), 4);
    assert_paired(&session, &seen, "manual");
    let request = compacted_requests(&seen).remove(0);
    assert_eq!(tool_results(&request), ["call_1", "call_2"]);
    session.shutdown().await;
}

// ----------------------------------------------------------------------------
// 2. Summary requests that fail: Pi #6647
// ----------------------------------------------------------------------------

/// Pi's `seedCompactableSession`: one exchange, keeping one token, so a
/// compaction sends exactly one summary request, the split turn's prefix.
fn seeded(manager: &mut SessionManager) {
    let now = bake_ai::now_ms();
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
}

fn retrying(enabled: bool, max_retries: u32, base_delay_ms: u64) -> Settings {
    settings(json!({
        "compaction": { "keepRecentTokens": 1 },
        "retry": { "enabled": enabled, "maxRetries": max_retries, "baseDelayMs": base_delay_ms },
    }))
}

async fn seeded_session(
    h: &Harness,
    dir: &TempDir,
    settings: Settings,
) -> (Arc<AgentSession>, std::path::PathBuf) {
    let mut manager = file_session(dir);
    seeded(&mut manager);
    let file = manager.session_file().expect("file").to_path_buf();
    let session = Arc::new(
        AgentSession::create(options(h, manager, settings))
            .await
            .expect("session"),
    );
    (session, file)
}

fn retry_events(events: &Mutex<Vec<Value>>, kind: &str) -> Vec<Value> {
    of_type(events, &[kind])
}

// Pi: "retries a transient `terminated` summarization error and compacts
// successfully"; and the next prompt is sent with the summary.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transient_summary_error_retries_and_compacts() {
    let dir = TempDir::new("summary-retry");
    let h = harness(200_000);
    let seen = Arc::new(Mutex::new(Vec::new()));
    h.faux.set_responses(vec![
        failure("terminated"),
        failure("terminated"),
        faux_assistant_message("recovered summary").into(),
        capture(&seen, faux_assistant_message("after")),
    ]);
    let (session, _) = seeded_session(&h, &dir, retrying(true, 3, 0)).await;
    let (events, _subscription) = record(&session);
    let result = session.compact(None).await.expect("compacted");
    assert!(result.summary.contains("recovered summary"));
    assert_eq!(
        h.faux.call_count(),
        3,
        "1 prefix-summary attempt + 2 retries"
    );
    let starts = retry_events(&events, "summarization_retry_scheduled");
    let ends = retry_events(&events, "summarization_retry_finished");
    assert_eq!(starts.len(), 2);
    assert_eq!(ends.len(), 1);
    assert_eq!(starts[0]["attempt"], 1);
    assert_eq!(starts[0]["maxAttempts"], 3);
    assert_eq!(starts[0]["errorMessage"], "terminated");
    assert_eq!(starts[1]["attempt"], 2);
    assert_eq!(starts[1]["maxAttempts"], 3);
    assert_eq!(compactions_in(&session), 1);
    session.prompt("next", Vec::new()).await.expect("next");
    assert_eq!(compacted_requests(&seen).len(), 1);
    session.shutdown().await;
}

// Pi: "does not retry a non-retryable error (insufficient_quota)" and
// "does not retry when retry is disabled".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_retryable_or_unretried_summary_errors_fail_at_once() {
    for (error, settings) in [
        ("insufficient_quota", retrying(true, 3, 0)),
        ("terminated", retrying(false, 3, 0)),
    ] {
        let dir = TempDir::new("summary-no-retry");
        let h = harness(200_000);
        h.faux.set_responses(vec![failure(error)]);
        let (session, file) = seeded_session(&h, &dir, settings).await;
        let bytes = std::fs::read(&file).expect("read");
        let (events, _subscription) = record(&session);
        let failed = session.compact(None).await.expect_err("failed");
        assert!(failed.contains(error), "{failed}");
        assert_eq!(h.faux.call_count(), 1);
        assert!(retry_events(&events, "summarization_retry_scheduled").is_empty());
        assert_eq!(compactions_in(&session), 0);
        session.shutdown().await;
        assert_eq!(std::fs::read(&file).expect("read"), bytes, "{error}");
    }
}

// Pi: "stops retrying after maxRetries and reports failure". The session
// writes nothing, reports the failure, and goes on with its old context.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exhausted_summary_retries_leave_the_old_context() {
    let dir = TempDir::new("summary-exhausted");
    let h = harness(200_000);
    let seen = Arc::new(Mutex::new(Vec::new()));
    h.faux.set_responses(vec![
        failure("terminated"),
        failure("terminated"),
        failure("terminated"),
        capture(&seen, faux_assistant_message("still here")),
    ]);
    let (session, file) = seeded_session(&h, &dir, retrying(true, 2, 0)).await;
    let bytes = std::fs::read(&file).expect("read");
    let before = session.messages();
    let (events, _subscription) = record(&session);
    let failed = session.compact(None).await.expect_err("failed");
    assert!(failed.contains("terminated"), "{failed}");
    assert_eq!(h.faux.call_count(), 3, "1 initial + 2 retries");
    assert_eq!(
        retry_events(&events, "summarization_retry_scheduled").len(),
        2
    );
    assert_eq!(
        retry_events(&events, "summarization_retry_finished").len(),
        1
    );
    let end = of_type(&events, &["compaction_end"]).remove(0);
    assert_eq!(end["aborted"], false);
    assert_eq!(end["errorMessage"], format!("Compaction failed: {failed}"));
    assert_eq!(compactions_in(&session), 0);
    assert_eq!(std::fs::read(&file).expect("read"), bytes);
    assert_eq!(convert_to_llm(&session.messages()), convert_to_llm(&before));
    assert!(!session.is_compacting());

    // The next prompt is sent with the old, uncompacted context.
    session.prompt("next", Vec::new()).await.expect("next");
    let request = seen.lock().unwrap_or_else(PoisonError::into_inner)[0].clone();
    assert_eq!(
        user_texts(&request),
        ["message to compact".to_owned(), "next".to_owned()]
    );
    session.shutdown().await;
}

// Threshold compaction whose summary keeps failing, before the prompt and
// after its run: each reports its failure, writes nothing, and the prompt
// is sent and answered with the old context.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exhausted_automatic_summary_retries_leave_the_old_context() {
    let dir = TempDir::new("auto-summary-exhausted");
    let h = harness(200_000);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut responses: Vec<FauxResponseStep> = (0..3).map(|_| failure("terminated")).collect();
    responses.push(capture(&seen, faux_assistant_message("still here")));
    responses.extend((0..3).map(|_| failure("terminated")));
    h.faux.set_responses(responses);
    // Any context over 10 tokens crosses the threshold.
    let settings = settings(json!({
        "compaction": { "reserveTokens": 199_990, "keepRecentTokens": 1 },
        "retry": { "enabled": true, "maxRetries": 2, "baseDelayMs": 0 },
    }));
    let (session, file) = seeded_session(&h, &dir, settings).await;
    let bytes = std::fs::read(&file).expect("read");
    let (events, _subscription) = record(&session);
    session.prompt("next", Vec::new()).await.expect("answered");
    assert_eq!(h.faux.call_count(), 7);
    let ends = of_type(&events, &["compaction_end"]);
    assert_eq!(ends.len(), 2, "{ends:?}");
    for end in &ends {
        assert_eq!(end["reason"], "threshold");
        assert_eq!(end["aborted"], false);
        assert!(
            end["errorMessage"]
                .as_str()
                .is_some_and(|message| message.starts_with("Auto-compaction failed: ")
                    && message.contains("terminated")),
            "{end}"
        );
    }
    assert_eq!(compactions_in(&session), 0);
    let request = seen.lock().unwrap_or_else(PoisonError::into_inner)[0].clone();
    assert_eq!(
        user_texts(&request),
        ["message to compact".to_owned(), "next".to_owned()]
    );
    session.shutdown().await;
    let entries = read_entries(&file);
    assert!(
        !entries
            .iter()
            .any(|entry| entry.entry_type() == "compaction")
    );
    assert!(std::fs::read(&file).expect("read").starts_with(&bytes));
}

/// Waits for the first `summarization_retry_scheduled` event.
fn on_retry_scheduled(session: &AgentSession) -> (Arc<Notify>, SessionSubscription) {
    let scheduled = Arc::new(Notify::new());
    let notify = Arc::clone(&scheduled);
    let subscription = session.subscribe(move |event| {
        if matches!(event, SessionEvent::SummarizationRetryScheduled { .. }) {
            notify.notify_one();
        }
    });
    (scheduled, subscription)
}

// Pi: "aborts an in-flight retry backoff via abortCompaction"; and through
// the idle-only entry's own signal. Neither writes anything, and the
// session takes the next prompt with its old context.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_during_the_summary_retry_wait_writes_nothing() {
    for idle in [false, true] {
        let dir = TempDir::new("summary-retry-cancel");
        let h = harness(200_000);
        h.faux.set_responses(vec![
            failure("terminated"),
            failure("terminated"),
            failure("terminated"),
        ]);
        let (session, file) = seeded_session(&h, &dir, retrying(true, 5, 30_000)).await;
        let bytes = std::fs::read(&file).expect("read");
        let (events, _subscription) = record(&session);
        let (scheduled, _scheduled_subscription) = on_retry_scheduled(&session);
        let controller = AbortController::new();
        let compacting = {
            let session = Arc::clone(&session);
            let signal = controller.signal();
            tokio::spawn(async move {
                if idle {
                    session.compact_when_idle(None, signal).await
                } else {
                    session.compact(None).await
                }
            })
        };
        tokio::time::timeout(Duration::from_secs(10), scheduled.notified())
            .await
            .expect("the retry wait started");
        if idle {
            controller.abort();
        } else {
            session.abort_compaction();
        }
        let failed = tokio::time::timeout(Duration::from_secs(10), compacting)
            .await
            .expect("the wait was cancelled")
            .expect("joined")
            .expect_err("cancelled");
        assert_eq!(failed, COMPACTION_CANCELLED, "idle {idle}");
        assert_eq!(h.faux.call_count(), 1, "no retry after the cancel");
        let end = of_type(&events, &["compaction_end"]).remove(0);
        assert_eq!(end["aborted"], true);
        assert_eq!(compactions_in(&session), 0);
        assert_eq!(std::fs::read(&file).expect("read"), bytes);
        let seen = Arc::new(Mutex::new(Vec::new()));
        h.faux
            .set_responses(vec![capture(&seen, faux_assistant_message("still here"))]);
        session.prompt("next", Vec::new()).await.expect("next");
        let request = seen.lock().unwrap_or_else(PoisonError::into_inner)[0].clone();
        assert_eq!(
            user_texts(&request),
            ["message to compact".to_owned(), "next".to_owned()],
            "idle {idle}"
        );
        session.shutdown().await;
    }
}

// ----------------------------------------------------------------------------
// 3. Prompts queued behind a manual compaction
// ----------------------------------------------------------------------------

/// A summary that records its request and waits for `release` before it
/// answers, saying on `started` that it was requested.
fn held_summary(
    seen: &Arc<Mutex<Vec<TranscriptContext>>>,
    started: Arc<Notify>,
    release: Arc<Notify>,
) -> FauxResponseStep {
    let seen = Arc::clone(seen);
    FauxResponseStep::Factory(Arc::new(move |context, _, _, _| {
        seen.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(context.clone());
        let started = Arc::clone(&started);
        let release = Arc::clone(&release);
        let wait: FauxFuture = Box::pin(async move {
            started.notify_one();
            release.notified().await;
            Ok(faux_assistant_message("## Goal\nHeld summary"))
        });
        wait
    }))
}

/// The user texts of `context`, in order, without the summary.
fn prompt_texts(context: &TranscriptContext) -> Vec<String> {
    user_texts(context)
        .into_iter()
        .filter(|text| !text.starts_with(COMPACTION_SUMMARY_PREFIX))
        .collect()
}

// A prompt sent while a manual compaction runs is refused with Pi's
// message (#7150); a steer and a follow-up queued meanwhile wait in the
// queue, reach no summary request, and are delivered once by the next
// prompt, against the compacted context: the steer with its first turn,
// the follow-up when that turn would stop. Pi's `compact` and the
// idle-only `/compact` entry both.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_queued_during_a_manual_compaction_is_delivered_once_after_it() {
    for idle in [false, true] {
        let h = harness(200_000);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let session = Arc::new(
            AgentSession::create(options(&h, in_memory(), keep_one()))
                .await
                .expect("session"),
        );
        h.faux.set_responses(vec![
            faux_assistant_message("Echo: one.").into(),
            faux_assistant_message("Echo: two.").into(),
        ]);
        for text in ["one", "two"] {
            session.prompt(text, Vec::new()).await.expect("prompt");
        }
        let calls = h.faux.call_count();
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        h.faux.set_responses(vec![
            held_summary(&seen, Arc::clone(&started), Arc::clone(&release)),
            capture(&seen, faux_assistant_message("## Original Request\ntwo")),
            capture(&seen, faux_assistant_message("answer to next")),
            capture(&seen, faux_assistant_message("answer to the follow-up")),
        ]);
        let compacting = {
            let session = Arc::clone(&session);
            tokio::spawn(async move {
                if idle {
                    session
                        .compact_when_idle(None, AbortController::new().signal())
                        .await
                } else {
                    session.compact(None).await
                }
            })
        };
        tokio::time::timeout(Duration::from_secs(10), started.notified())
            .await
            .expect("the summary was requested");
        assert!(session.is_compacting());
        let refused = session
            .prompt("too early", Vec::new())
            .await
            .expect_err("refused while compacting");
        assert_eq!(
            refused,
            "Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry."
        );
        session.steer("steered", Vec::new()).await.expect("steer");
        session
            .follow_up("followed", Vec::new())
            .await
            .expect("follow up");
        release.notify_one();
        compacting
            .await
            .expect("joined")
            .expect("compacted with input queued");
        // Compaction delivers nothing; the queue waits for the next prompt.
        assert_eq!(session.queued_messages().steering, ["steered"]);
        assert_eq!(session.queued_messages().follow_up, ["followed"]);
        assert_eq!(h.faux.call_count() - calls, 2, "idle {idle}");

        session.prompt("next", Vec::new()).await.expect("next");
        assert!(session.queued_messages().is_empty());
        assert!(!session.agent().has_queued_messages());
        let requests = seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
        assert_eq!(requests.len(), 4, "idle {idle}");
        // Neither summary request saw the queued input.
        for summary in &requests[..2] {
            let text = user_texts(summary).concat();
            assert!(!text.contains("steered") && !text.contains("followed"));
            assert!(!text.contains("too early"));
        }
        // Both turns start from the summary.
        for turn in &requests[2..] {
            assert!(user_texts(turn)[0].starts_with(COMPACTION_SUMMARY_PREFIX));
        }
        assert_eq!(
            prompt_texts(&requests[2])
                .iter()
                .rev()
                .take(2)
                .rev()
                .cloned()
                .collect::<Vec<_>>(),
            ["next", "steered"]
        );
        let last = prompt_texts(&requests[3]);
        for text in ["next", "steered", "followed"] {
            assert_eq!(
                last.iter().filter(|seen| *seen == text).count(),
                1,
                "idle {idle}: {text} in {last:?}"
            );
        }
        assert_eq!(last.last().map(String::as_str), Some("followed"));
        assert!(!last.iter().any(|text| text == "too early"));
        session.shutdown().await;
    }
}

// ----------------------------------------------------------------------------
// 4. Restart after compaction
// ----------------------------------------------------------------------------

fn file_context(file: &std::path::Path, leaf: LeafSelector<'_>) -> crate::session::SessionContext {
    build_session_context(&read_entries(file), leaf)
}

fn message_entries(entries: &[SessionEntry]) -> Vec<SessionEntry> {
    entries
        .iter()
        .filter(|entry| entry.entry_type() == "message")
        .cloned()
        .collect()
}

// Reopening a compacted session file rebuilds exactly the compacted
// context, the summary and the kept tail, while every replaced message
// stays in the file, so the context before each compaction rebuilds from
// the entry before it. Twice, so the second compaction's summary and tail
// replace the first's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reopened_session_rebuilds_the_compacted_context_and_keeps_history() {
    let dir = TempDir::new("restart-compacted");
    let h = harness(200_000);
    let session = AgentSession::create(with_tool(&h, file_session(&dir), keep_one()))
        .await
        .expect("session");
    let file = session.session_file().expect("persisted");
    let mut contexts = Vec::new();
    let mut summaries = Vec::new();
    for round in ["first", "second"] {
        h.faux.set_responses(vec![
            parallel_calls(),
            faux_assistant_message(&format!("{round} answer")).into(),
            faux_assistant_message(&format!("## Goal\n{round} summary")).into(),
            faux_assistant_message(&format!("## Original Request\n{round}")).into(),
        ]);
        session
            .prompt(&format!("{round} task"), Vec::new())
            .await
            .expect("prompt");
        let before = convert_to_llm(&session.messages());
        let leaf = lock(&session.session).leaf_id().map(str::to_owned);
        let messages_before = message_entries(&read_entries(&file));
        let result = session.compact(None).await.expect("compacted");
        summaries.push(result.summary.clone());
        contexts.push((leaf, before, messages_before));
        let live = convert_to_llm(&session.messages());
        // The file alone rebuilds the live, compacted transcript. The live
        // session holds the writer lock, so it is read as a reader reads it.
        let rebuilt = file_context(&file, LeafSelector::Last);
        let rebuilt_live: Vec<_> = rebuilt
            .messages
            .iter()
            .map(StoredMessage::to_agent)
            .collect();
        assert_eq!(convert_to_llm(&rebuilt_live), live, "{round}");
        assert_eq!(summary_of(&rebuilt.messages), Some(result.summary.clone()));
        assert_paired_context(&rebuilt.messages, round);
    }
    session.shutdown().await;

    let entries = read_entries(&file);
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.entry_type() == "compaction")
            .count(),
        2
    );
    let messages = message_entries(&entries);
    for (leaf, before, messages_before) in &contexts {
        // Nothing written before a compaction was removed or changed.
        for message in messages_before {
            assert!(messages.contains(message), "{:?}", message.id());
        }
        let leaf = leaf.as_deref().expect("leaf");
        assert_eq!(
            &llm_of_stored(&file_context(&file, LeafSelector::Id(leaf)).messages),
            before
        );
    }
    // The original prompts are in the file, and only the latest summary
    // is in the reopened context.
    let reopened = file_context(&file, LeafSelector::Last);
    let text = serde_json::to_string(&reopened.to_json()).expect("json");
    assert!(text.contains(&summaries[1].replace('\n', "\\n")));
    assert!(!text.contains("first task"));
    let raw = std::fs::read_to_string(&file).expect("read");
    assert!(raw.contains("first task") && raw.contains("second task"));
    // A writer reopening it after the restart rebuilds the same context.
    let restarted = SessionManager::open(&file, None, None).expect("restart");
    assert_eq!(restarted.build_session_context(), reopened);
}

/// The summary of the compaction `messages` start from, after the
/// recorded system message.
fn summary_of(messages: &[StoredMessage]) -> Option<String> {
    messages
        .iter()
        .find(|message| message.role() != Some("system"))
        .filter(|message| message.role() == Some("compactionSummary"))
        .and_then(|message| message.as_json().get("summary"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn assert_paired_context(messages: &[StoredMessage], label: &str) {
    if let Some(problem) = provider_problem(&llm_of_stored(messages)) {
        panic!("{label}: {problem}");
    }
}

// ----------------------------------------------------------------------------
// 5. A writer killed while it appends the compaction entry
// ----------------------------------------------------------------------------

/// The byte range of the one compaction line `after` adds to `before`.
fn compaction_line(before: &[u8], after: &[u8]) -> std::ops::Range<usize> {
    assert!(after.starts_with(before), "appends only");
    let mut start = before.len();
    for line in after[before.len()..].split_inclusive(|byte| *byte == b'\n') {
        let end = start + line.len();
        let value: Value = serde_json::from_slice(line).expect("whole line");
        if value["type"] == "compaction" {
            return start..end;
        }
        start = end;
    }
    panic!("no compaction line was appended");
}

// Truncating the file at every byte of the compaction line, as a process
// killed mid-append leaves it: reopening never panics, repairs the torn
// line so the next append starts its own, and rebuilds the context before
// the compaction until the whole entry's JSON is there, the compacted one
// from then on. A session over a torn file compacts again, and that
// compaction reopens with the torn fragment skipped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_torn_compaction_entry_reopens_to_the_old_context_and_compacts_again() {
    let dir = TempDir::new("torn-compaction");
    let h = harness(200_000);
    h.faux.set_responses(vec![
        faux_assistant_message("Echo: one.").into(),
        faux_assistant_message("Echo: two.").into(),
        faux_assistant_message("Echo: three.").into(),
        faux_assistant_message("## Goal\nCompacted").into(),
        faux_assistant_message("## Original Request\nthree").into(),
    ]);
    let session = AgentSession::create(options(&h, file_session(&dir), keep_one()))
        .await
        .expect("session");
    for text in ["one", "two", "three"] {
        session.prompt(text, Vec::new()).await.expect("prompt");
    }
    let file = session.session_file().expect("persisted");
    let before = std::fs::read(&file).expect("read");
    let live_before = convert_to_llm(&session.messages());
    session.compact(None).await.expect("compacted");
    let live_after = convert_to_llm(&session.messages());
    session.shutdown().await;
    let after = std::fs::read(&file).expect("read");
    let line = compaction_line(&before, &after);
    let json_end = line.end - 1;
    // What the file rebuilds before and after the entry is what the live
    // session held.
    let old_file = dir.join("old.jsonl");
    std::fs::write(&old_file, &after[..line.start]).expect("write old");
    let old_context = file_context(&old_file, LeafSelector::Last);
    let compacted_context = file_context(&file, LeafSelector::Last);
    assert_eq!(llm_of_stored(&old_context.messages), live_before);
    assert_eq!(llm_of_stored(&compacted_context.messages), live_after);
    assert_eq!(summary_of(&old_context.messages), None);

    let copy = dir.join("torn.jsonl");
    for cut in line.start..=line.end {
        let prefix = &after[..cut];
        std::fs::write(&copy, prefix).expect("write prefix");
        let reopened = SessionManager::open(&copy, None, None).expect("reopen");
        let expected = if cut >= json_end {
            &compacted_context
        } else {
            &old_context
        };
        assert_eq!(&reopened.build_session_context(), expected, "cut {cut}");
        drop(reopened);
        let repaired = std::fs::read(&copy).expect("read");
        if cut == line.start || cut == line.end {
            assert_eq!(repaired, prefix, "cut {cut}");
        } else {
            assert_eq!(&repaired[..cut], prefix, "cut {cut}");
            assert_eq!(&repaired[cut..], b"\n", "cut {cut}");
        }
    }

    // Compacting again over torn files.
    let mid = line.start + (json_end - line.start) / 2;
    for cut in [line.start + 1, mid, json_end - 1] {
        let torn = dir.join(&format!("torn-{cut}.jsonl"));
        std::fs::write(&torn, &after[..cut]).expect("write prefix");
        let manager = SessionManager::open(&torn, None, None).expect("reopen");
        h.faux.set_responses(vec![
            faux_assistant_message("## Goal\nAgain").into(),
            faux_assistant_message("## Original Request\nthree").into(),
        ]);
        let resumed = AgentSession::create(options(&h, manager, keep_one()))
            .await
            .expect("resumed");
        assert_eq!(
            convert_to_llm(&resumed.messages()),
            llm_of_stored(&old_context.messages),
            "cut {cut}"
        );
        let result = resumed.compact(None).await.expect("compacts again");
        assert!(result.summary.starts_with("## Goal\nAgain"));
        resumed.shutdown().await;
        let context = file_context(&torn, LeafSelector::Last);
        assert_eq!(
            summary_of(&context.messages),
            Some(result.summary.clone()),
            "cut {cut}"
        );
        let entries = read_entries(&torn);
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.entry_type() == "compaction")
                .count(),
            1,
            "cut {cut}: the torn entry is skipped"
        );
        // The torn fragment stays a line of its own.
        let raw = std::fs::read(&torn).expect("read");
        let fragment = &after[line.start..cut];
        assert!(
            raw.split(|byte| *byte == b'\n')
                .any(|line| line == fragment),
            "cut {cut}"
        );
    }
}
