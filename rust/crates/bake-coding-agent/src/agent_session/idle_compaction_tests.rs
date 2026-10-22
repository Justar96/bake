//! [`AgentSession::compact_when_idle`], the manual compaction interactive
//! `/compact` runs, over the faux provider.
//!
//! Adapted from Pi v1.2.0 (`06a466935536f8c9bcacb2d5efc0fc021be1bdff`)
//! `packages/coding-agent/src/core/agent-session.ts` (`compact`,
//! `abortCompaction`) and its tests `test/agent-session-compaction.test.ts`
//! ("should trigger manual compaction via compact()", "should maintain
//! valid session state after compaction"),
//! `test/suite/regressions/7150-rpc-prompt-during-compaction.test.ts` (a
//! prompt during manual compaction is refused with Pi's message), and
//! `test/suite/regressions/9340-9777-auto-compaction-cancellation.test.ts`
//! (a cancelled compaction writes no entry). Pi's
//! `7253-manual-compact-during-response` is not followed: this variant
//! refuses while a response runs instead of aborting it. Bake's own cases
//! cover the cancel at each boundary (before the claim, during the branch
//! read, during the summary request, at the commit, after the commit) and
//! shutdown.

use bake_ai::providers::faux::FauxFuture;

use super::*;

fn compaction_entries(session: &AgentSession) -> usize {
    entry_types(session)
        .iter()
        .filter(|kind| *kind == "compaction")
        .count()
}

/// The roles and texts of the agent's transcript, system messages left out.
fn transcript(session: &AgentSession) -> Vec<String> {
    session
        .messages()
        .iter()
        .filter(|message| message.role() != "system")
        .map(|message| {
            let stored = StoredMessage::from(message);
            let json = stored.as_json();
            format!(
                "{}: {}",
                message.role(),
                json.get("content")
                    .or_else(|| json.get("summary"))
                    .map(|content| match content {
                        Value::String(text) => text.clone(),
                        other => crate::agent_session::navigation::content_text(Some(other)),
                    })
                    .unwrap_or_default()
            )
        })
        .collect()
}

/// A session over three answered prompts, keeping one token, with the
/// faux provider answering with `steps` from then on.
async fn three_turns(h: &Harness, settings: Settings) -> Arc<AgentSession> {
    h.faux.set_responses(vec![
        faux_assistant_message("Echo: one.").into(),
        faux_assistant_message("Echo: two.").into(),
        faux_assistant_message("Echo: three.").into(),
    ]);
    let session = Arc::new(
        AgentSession::create(options(h, in_memory(), settings))
            .await
            .expect("session"),
    );
    for text in ["one", "two", "three"] {
        session.prompt(text, Vec::new()).await.expect("prompt");
    }
    session
}

/// A summary response that says it started on `started`, then waits for
/// the request's signal and streams as aborted.
fn held_until_aborted(started: tokio::sync::mpsc::UnboundedSender<()>) -> FauxResponseStep {
    FauxResponseStep::Factory(Arc::new(move |_, options, _, _| {
        let signal = options.base.signal.clone();
        let started = started.clone();
        let wait: FauxFuture = Box::pin(async move {
            let _ = started.send(());
            if let Some(signal) = signal {
                signal.cancelled().await;
            }
            Ok(faux_assistant_message("## Goal\nNever written"))
        });
        wait
    }))
}

// Dropping the public caller after the append must not abandon the matching
// live-context update or let a prompt overtake it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_compaction_caller_keeps_commit_and_publication_exclusive() {
    let dir = TempDir::new("dropped-compaction-publication");
    let h = harness(200_000);
    h.faux.set_responses(vec![
        faux_assistant_message("Echo: one.").into(),
        faux_assistant_message("Echo: two.").into(),
        faux_assistant_message("Echo: three.").into(),
    ]);
    let session = Arc::new(
        AgentSession::create(options(&h, file_session(&dir), keep_one()))
            .await
            .expect("session"),
    );
    for text in ["one", "two", "three"] {
        session.prompt(text, Vec::new()).await.expect("prompt");
    }
    let file = session.session_file().expect("persisted session");
    h.faux.set_responses(vec![
        faux_assistant_message("## Goal\nOwned summary").into(),
        faux_assistant_message("## Original Request\nthree").into(),
    ]);
    let (reached_tx, reached) = tokio::sync::oneshot::channel();
    let (release_tx, release) = std::sync::mpsc::channel();
    session.core.hooks().after_compaction_commit = Some(Box::new(move || {
        let _ = reached_tx.send(());
        let _ = release.recv();
    }));
    let compacting = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.compact(None).await })
    };
    reached
        .await
        .expect("the entry was appended before publication");
    compacting.abort();
    assert!(compacting.await.expect_err("caller dropped").is_cancelled());
    assert!(
        !session.core.activity.borrow().idle(),
        "the owned commit retains exclusion"
    );
    let refused = tokio::select! {
        biased;
        result = session.prompt("too early", Vec::new()) => result,
        () = std::future::ready(()) => panic!("a competing prompt must be refused before its first await"),
    }.expect_err("publication is still pending");
    assert!(refused.contains("context"), "{refused}");
    release_tx.send(()).expect("release publication");
    session.core.wait_for_idle().await;
    assert_eq!(compaction_entries(&session), 1);
    assert!(transcript(&session)[0].starts_with("compactionSummary: ## Goal\nOwned summary"));
    session.shutdown().await;
    let reopened = SessionManager::open(&file, None, None).expect("open committed session");
    assert!(
        reopened
            .build_session_projection()
            .messages
            .iter()
            .any(|message| {
                serde_json::to_string(message.as_json())
                    .unwrap()
                    .contains("Owned summary")
            })
    );
}

// Pi "should trigger manual compaction via compact()" and "should maintain
// valid session state after compaction", through the idle-only entry: the
// instructions reach the summary request, one entry is written, the
// transcript becomes the summary and the kept history, and the next prompt
// is sent with the summary.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_compaction_summarizes_with_instructions_and_continues() {
    let dir = TempDir::new("idle-compact");
    let h = harness(200_000);
    h.faux.set_responses(vec![
        faux_assistant_message("Echo: one.").into(),
        faux_assistant_message("Echo: two.").into(),
        faux_assistant_message("Echo: three.").into(),
    ]);
    let session = AgentSession::create(options(&h, file_session(&dir), keep_one()))
        .await
        .expect("session");
    for text in ["one", "two", "three"] {
        session.prompt(text, Vec::new()).await.expect("prompt");
    }
    let (events, _subscription) = record(&session);
    let seen = Arc::new(Mutex::new(Vec::new()));
    h.faux.set_responses(vec![
        capture(&seen, faux_assistant_message("## Goal\nCount to three")),
        capture(&seen, faux_assistant_message("## Original Request\nthree")),
        capture(&seen, faux_assistant_message("Echo: four.")),
    ]);
    let result = session
        .compact_when_idle(Some("keep the numbers"), AbortController::new().signal())
        .await
        .expect("compacted");
    assert!(result.summary.starts_with("## Goal\nCount to three"));
    assert!(result.tokens_before > 0);
    let requests = seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(requests.len(), 2, "the history and the split turn's prefix");
    assert!(
        user_texts(&requests[0])
            .concat()
            .contains("Additional focus: keep the numbers"),
        "{:?}",
        user_texts(&requests[0])
    );
    assert_eq!(compaction_entries(&session), 1);
    assert_eq!(
        transcript(&session),
        [
            "compactionSummary: ".to_owned() + &result.summary,
            "assistant: Echo: three.".to_owned(),
        ]
    );
    let compaction = of_type(&events, &["compaction_start", "compaction_end"]);
    assert_eq!(compaction.len(), 2);
    assert_eq!(compaction[0]["reason"], "manual");
    assert_eq!(compaction[1]["aborted"], false);

    session.prompt("four", Vec::new()).await.expect("four");
    let requests = seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let next = user_texts(&requests[2]);
    assert!(
        next[0].starts_with(COMPACTION_SUMMARY_PREFIX) && next[0].contains("Count to three"),
        "{next:?}"
    );
    assert_eq!(next.last().map(String::as_str), Some("four"));
    assert!(!next.iter().any(|text| text == "one" || text == "two"));

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

// Pi: an empty session has nothing to compact, a branch ending in a
// compaction is already compacted, and no model fails before any request;
// none writes anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_compaction_reports_pi_errors_and_changes_nothing() {
    let h = harness(200_000);
    let empty = AgentSession::create(options(&h, in_memory(), keep_one()))
        .await
        .expect("session");
    let (events, _subscription) = record(&empty);
    let never = AbortController::new();
    assert_eq!(
        empty.compact_when_idle(None, never.signal()).await,
        Err("Nothing to compact (session too small)".into())
    );
    assert_eq!(
        of_type(&events, &["compaction_end"])[0]["errorMessage"],
        "Compaction failed: Nothing to compact (session too small)"
    );
    empty.shutdown().await;

    let session = three_turns(&h, keep_one()).await;
    h.faux.set_responses(vec![
        faux_assistant_message("## Goal\nCount").into(),
        faux_assistant_message("## Original Request\nthree").into(),
    ]);
    session
        .compact_when_idle(None, never.signal())
        .await
        .expect("compacted");
    assert_eq!(
        session.compact_when_idle(None, never.signal()).await,
        Err("Already compacted".into())
    );
    assert_eq!(compaction_entries(&session), 1);
    assert_eq!(h.faux.call_count(), 5, "no request for the refusal");
    session.shutdown().await;

    let mut no_model = options(&h, in_memory(), keep_one());
    no_model.registry = Arc::new(ModelRegistry::new(
        ModelConfig::from_value(json!({}), "models.json"),
        AuthStorage::empty(),
    ))
    .into();
    no_model.model = None;
    let no_model = AgentSession::create(no_model).await.expect("session");
    let calls = h.faux.call_count();
    let error = no_model
        .compact_when_idle(None, never.signal())
        .await
        .expect_err("no model");
    assert!(error.contains("No model selected"), "{error}");
    assert_eq!(h.faux.call_count(), calls);
    no_model.shutdown().await;
}

// Pi regression 7150: a prompt during manual compaction is refused with
// Pi's message. This variant is idle only: it is refused while a response
// runs, and a second compaction is refused while the first runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_compaction_never_overlaps_a_prompt() {
    let h = harness(200_000);
    let session = three_turns(&h, keep_one()).await;
    let release = Arc::new(tokio::sync::Notify::new());
    let (started_tx, mut started) = tokio::sync::mpsc::unbounded_channel();
    let held = {
        let release = Arc::clone(&release);
        FauxResponseStep::Factory(Arc::new(move |_, _, _, _| {
            let release = Arc::clone(&release);
            let started = started_tx.clone();
            let wait: FauxFuture = Box::pin(async move {
                let _ = started.send(());
                release.notified().await;
                Ok(faux_assistant_message("Echo: four."))
            });
            wait
        }))
    };
    h.faux.set_responses(vec![held]);
    let run = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("four", Vec::new()).await })
    };
    started.recv().await.expect("the response started");
    let never = AbortController::new();
    assert_eq!(
        session.compact_when_idle(None, never.signal()).await,
        Err(COMPACTION_WHILE_STREAMING.into())
    );
    release.notify_one();
    run.await.expect("joined").expect("four");
    assert_eq!(compaction_entries(&session), 0);

    let (started_tx, mut started) = tokio::sync::mpsc::unbounded_channel();
    h.faux.set_responses(vec![held_until_aborted(started_tx)]);
    let cancel = AbortController::new();
    let compacting = {
        let session = Arc::clone(&session);
        let signal = cancel.signal();
        tokio::spawn(async move { session.compact_when_idle(None, signal).await })
    };
    started.recv().await.expect("the summary request started");
    assert!(session.is_compacting());
    assert_eq!(
        session.prompt("blocked", Vec::new()).await,
        Err(
            "Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry."
                .into()
        )
    );
    assert_eq!(
        session.compact_when_idle(None, never.signal()).await,
        Err(COMPACTION_WHILE_BUSY.into())
    );
    cancel.abort();
    assert_eq!(
        compacting.await.expect("joined"),
        Err(COMPACTION_CANCELLED.into())
    );
    assert!(!session.is_compacting());
    session.shutdown().await;
}

// Bake: a cancel before the claim sends nothing and emits nothing; one
// during the branch read sends no summary request; one during the request
// aborts it. Each writes nothing, leaves the transcript as it was, and the
// next prompt is sent with the whole history.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_idle_compaction_leaves_the_session_usable() {
    let h = harness(200_000);
    let session = three_turns(&h, keep_one()).await;
    let before = transcript(&session);
    let (events, _subscription) = record(&session);

    let early = AbortController::new();
    early.abort();
    assert_eq!(
        session.compact_when_idle(None, early.signal()).await,
        Err(COMPACTION_CANCELLED.into())
    );
    assert!(of_type(&events, &["compaction_start"]).is_empty());
    assert_eq!(h.faux.call_count(), 3);

    let hold = SessionHold::on(&session, "compaction_start");
    let during_read = AbortController::new();
    let compacting = {
        let session = Arc::clone(&session);
        let signal = during_read.signal();
        tokio::spawn(async move { session.compact_when_idle(None, signal).await })
    };
    hold.wait_held().await;
    hold.wait_blocked_io(&session).await;
    during_read.abort();
    hold.release();
    assert_eq!(
        compacting.await.expect("joined"),
        Err(COMPACTION_CANCELLED.into())
    );
    assert_eq!(h.faux.call_count(), 3, "no summary request");

    let (started_tx, mut started) = tokio::sync::mpsc::unbounded_channel();
    h.faux.set_responses(vec![held_until_aborted(started_tx)]);
    let during_request = AbortController::new();
    let compacting = {
        let session = Arc::clone(&session);
        let signal = during_request.signal();
        tokio::spawn(async move { session.compact_when_idle(None, signal).await })
    };
    started.recv().await.expect("the summary request started");
    during_request.abort();
    assert_eq!(
        compacting.await.expect("joined"),
        Err(COMPACTION_CANCELLED.into())
    );

    let ends = of_type(&events, &["compaction_end"]);
    assert_eq!(ends.len(), 2, "{ends:?}");
    assert!(ends.iter().all(|end| end["aborted"] == true));
    assert!(ends.iter().all(|end| end.get("errorMessage").is_none()));
    assert_eq!(compaction_entries(&session), 0);
    assert_eq!(transcript(&session), before);

    let seen = Arc::new(Mutex::new(Vec::new()));
    h.faux
        .set_responses(vec![capture(&seen, faux_assistant_message("Echo: four."))]);
    session.prompt("four", Vec::new()).await.expect("four");
    let sent = user_texts(&seen.lock().unwrap_or_else(PoisonError::into_inner)[0]);
    assert_eq!(sent, ["one", "two", "three", "four"]);
    session.shutdown().await;
}

// Bake: the commit boundary. A cancel taken while the write waits for the
// session lock writes nothing; without one the entry is written; a cancel
// after the write is reported as the success it is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cancel_is_checked_at_the_commit_under_the_session_lock() {
    for cancel in [true, false] {
        let h = harness(200_000);
        let session = three_turns(&h, keep_one()).await;
        let before = transcript(&session);
        h.faux.set_responses(vec![
            faux_assistant_message("## Goal\nCount").into(),
            faux_assistant_message("## Original Request\nthree").into(),
        ]);
        let (reached_tx, reached) = std::sync::mpsc::channel::<()>();
        let (release_tx, release) = std::sync::mpsc::channel::<()>();
        session.core.hooks().before_compaction_commit = Some(Box::new(move || {
            let _ = reached_tx.send(());
            let _ = release.recv();
        }));
        let abort = AbortController::new();
        let compacting = {
            let session = Arc::clone(&session);
            let signal = abort.signal();
            tokio::spawn(async move { session.compact_when_idle(None, signal).await })
        };
        tokio::task::spawn_blocking(move || reached.recv().expect("held"))
            .await
            .expect("join");
        assert_eq!(h.faux.call_count(), 5, "the summary is done");
        if cancel {
            abort.abort();
        }
        release_tx.send(()).expect("release");
        let result = compacting.await.expect("joined");
        if cancel {
            assert_eq!(result, Err(COMPACTION_CANCELLED.into()));
            assert_eq!(compaction_entries(&session), 0);
            assert_eq!(transcript(&session), before);
        } else {
            assert!(result.expect("compacted").summary.starts_with("## Goal"));
            assert_eq!(compaction_entries(&session), 1);
        }
        session.shutdown().await;
    }

    let h = harness(200_000);
    let session = three_turns(&h, keep_one()).await;
    let (events, _subscription) = record(&session);
    h.faux.set_responses(vec![
        faux_assistant_message("## Goal\nCount").into(),
        faux_assistant_message("## Original Request\nthree").into(),
    ]);
    let abort = AbortController::new();
    let late = abort.clone();
    session.core.hooks().after_compaction_commit = Some(Box::new(move || late.abort()));
    let result = session.compact_when_idle(None, abort.signal()).await;
    assert!(abort.signal().aborted());
    assert!(result.expect("committed").summary.starts_with("## Goal"));
    assert_eq!(compaction_entries(&session), 1);
    let ends = of_type(&events, &["compaction_end"]);
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0]["aborted"], false);
    assert!(transcript(&session)[0].starts_with("compactionSummary: ## Goal"));
    session.shutdown().await;
}

// Bake: a failed summary request writes nothing and leaves the transcript
// as it was; the session takes the next prompt with its whole history.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_idle_compaction_leaves_the_session_usable() {
    let h = harness(200_000);
    let no_retry = settings(json!({
        "compaction": { "keepRecentTokens": 1 },
        "retry": { "enabled": false },
    }));
    let session = three_turns(&h, no_retry).await;
    let before = transcript(&session);
    let (events, _subscription) = record(&session);
    h.faux.set_responses(vec![failure("400 bad request")]);
    let error = session
        .compact_when_idle(None, AbortController::new().signal())
        .await
        .expect_err("failed");
    assert!(error.contains("400 bad request"), "{error}");
    let ends = of_type(&events, &["compaction_end"]);
    assert_eq!(ends[0]["aborted"], false);
    assert!(
        ends[0]["errorMessage"]
            .as_str()
            .is_some_and(|message| message.starts_with("Compaction failed: ")),
        "{ends:?}"
    );
    assert_eq!(compaction_entries(&session), 0);
    assert_eq!(transcript(&session), before);
    assert!(!session.is_compacting());
    let seen = Arc::new(Mutex::new(Vec::new()));
    h.faux
        .set_responses(vec![capture(&seen, faux_assistant_message("Echo: four."))]);
    session.prompt("four", Vec::new()).await.expect("four");
    let sent = user_texts(&seen.lock().unwrap_or_else(PoisonError::into_inner)[0]);
    assert_eq!(sent, ["one", "two", "three", "four"]);
    session.shutdown().await;
}

// Bake: shutdown cancels a running compaction and waits for it; nothing is
// written to the file and no event is reported after it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_cancels_an_idle_compaction_and_waits_for_it() {
    let dir = TempDir::new("idle-compact-shutdown");
    let h = harness(200_000);
    h.faux.set_responses(vec![
        faux_assistant_message("Echo: one.").into(),
        faux_assistant_message("Echo: two.").into(),
    ]);
    let session = Arc::new(
        AgentSession::create(options(&h, file_session(&dir), keep_one()))
            .await
            .expect("session"),
    );
    for text in ["one", "two"] {
        session.prompt(text, Vec::new()).await.expect("prompt");
    }
    let (events, _subscription) = record(&session);
    let (started_tx, mut started) = tokio::sync::mpsc::unbounded_channel();
    h.faux.set_responses(vec![held_until_aborted(started_tx)]);
    let compacting = {
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            session
                .compact_when_idle(None, AbortController::new().signal())
                .await
        })
    };
    started.recv().await.expect("the summary request started");
    tokio::time::timeout(Duration::from_secs(5), session.shutdown())
        .await
        .expect("shutdown settles");
    assert_eq!(
        compacting.await.expect("joined"),
        Err(COMPACTION_CANCELLED.into())
    );
    assert!(
        of_type(&events, &["compaction_end"]).is_empty(),
        "{events:?}"
    );
    let file = session.session_file().expect("persisted");
    let reopened = SessionManager::open(&file, None, None).expect("open");
    assert!(
        !reopened
            .entries()
            .iter()
            .any(|entry| entry.entry_type() == "compaction")
    );
    assert_eq!(
        session
            .compact_when_idle(None, AbortController::new().signal())
            .await,
        Err("The session is shut down.".into())
    );
}

// Bake: a failed write of the entry is not a no-change error. It is
// reported as the write failure, even when a cancel came right after it,
// and stops the session as every failed write does: nothing more is
// written, the file holds no compaction, and later prompts are refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_compaction_write_stops_the_session_and_is_not_a_cancel() {
    let dir = TempDir::new("idle-compact-write");
    let h = harness(200_000);
    h.faux.set_responses(vec![
        faux_assistant_message("Echo: one.").into(),
        faux_assistant_message("Echo: two.").into(),
        faux_assistant_message("## Goal\nCount").into(),
        faux_assistant_message("## Original Request\ntwo").into(),
        faux_assistant_message("Echo: never.").into(),
    ]);
    let session = AgentSession::create(options(&h, file_session(&dir), keep_one()))
        .await
        .expect("session");
    for text in ["one", "two"] {
        session.prompt(text, Vec::new()).await.expect("prompt");
    }
    let (events, _subscription) = record(&session);
    let abort = AbortController::new();
    let late = abort.clone();
    session.core.hooks().fail_compaction_entry = Some(Box::new(move || late.abort()));
    let error = session
        .compact_when_idle(None, abort.signal())
        .await
        .expect_err("the write failed");
    assert!(
        abort.signal().aborted(),
        "the cancel came after the failure"
    );
    assert_ne!(error, COMPACTION_CANCELLED);
    assert!(
        error.contains("could not be written") && error.contains("injected failure"),
        "{error}"
    );
    let ends = of_type(&events, &["compaction_end"]);
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0]["aborted"], false);
    assert!(
        ends[0]["errorMessage"]
            .as_str()
            .is_some_and(|message| message.contains("injected failure")),
        "{ends:?}"
    );
    let refused = session
        .prompt("three", Vec::new())
        .await
        .expect_err("stopped");
    assert!(refused.contains("could not be written"), "{refused}");
    assert_eq!(h.faux.call_count(), 4, "no request after the failure");
    assert_eq!(
        session
            .compact_when_idle(None, AbortController::new().signal())
            .await
            .map(|_| ()),
        Err(refused)
    );
    let file = session.session_file().expect("persisted");
    session.shutdown().await;
    let reopened = SessionManager::open(&file, None, None).expect("open");
    assert!(
        !reopened
            .entries()
            .iter()
            .any(|entry| entry.entry_type() == "compaction")
    );
}

// Pi: `/compact` compacts whatever the automatic-compaction toggle says;
// `compaction.enabled` turns only threshold and overflow compaction off.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_compaction_runs_with_automatic_compaction_off() {
    let h = harness(200_000);
    let off = settings(json!({ "compaction": { "enabled": false, "keepRecentTokens": 1 } }));
    let session = three_turns(&h, off).await;
    assert!(!session.auto_compaction_enabled());
    h.faux.set_responses(vec![
        faux_assistant_message("## Goal\nCount").into(),
        faux_assistant_message("## Original Request\nthree").into(),
    ]);
    session
        .compact_when_idle(None, AbortController::new().signal())
        .await
        .expect("compacted");
    assert_eq!(compaction_entries(&session), 1);
    session.shutdown().await;
}
