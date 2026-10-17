//! The real session, provider, owner gate and commit boundary cooperate here.

use std::time::Duration;

use bake_agent::{RunLimits, RunOutcome};
use bake_ai::providers::faux::{FauxProvider, FauxProviderOptions, FauxResponseStep};
use bake_ai::{AbortController, ApiRegistry, Usage};
use serde_json::json;
use tokio::sync::Notify;

use super::*;
use crate::agent_session::{AgentSession, AgentSessionOptions, SessionSubscription, lock};
use crate::auth_storage::AuthStorage;
use crate::model_registry::{ModelRegistry, config::ModelConfig};
use crate::settings::Settings;
use crate::system_prompt::SystemPromptOptions;

async fn fixture(
    max_calls: Option<u64>,
    enabled: bool,
) -> (Arc<AgentSession>, Arc<FauxProvider>, RunControl) {
    fixture_with_session(max_calls, enabled, None).await
}

async fn fixture_with_session(
    max_calls: Option<u64>,
    enabled: bool,
    manager: Option<SessionManager>,
) -> (Arc<AgentSession>, Arc<FauxProvider>, RunControl) {
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
                "baseUrl": "http://127.0.0.1:9/unused", "api": faux.model().api,
                "models": [{"id": "faux-1", "contextWindow": 8192, "maxTokens": 4096}],
            }),
            Some("test-key".into()),
        )
        .unwrap();
    let model = registry.model("faux", "faux-1");
    let mut manager = manager.unwrap_or_else(history);
    manager
        .append_message(UserMessage {
            content: UserContent::Text("current turn".into()),
            timestamp: bake_ai::now_ms(),
        })
        .unwrap();
    let mut anchor = faux_assistant_message("current response");
    anchor.usage = Usage {
        input: 6500,
        output: 100,
        total_tokens: 6600,
        ..Usage::default()
    };
    manager.append_message(anchor).unwrap();
    let settings = Settings::from_object(
        json!({
            "backgroundCompaction": enabled,
            "compaction": { "reserveTokens": 1024, "keepRecentTokens": 1 },
            "retry": { "enabled": false },
        })
        .as_object()
        .unwrap(),
    );
    let session = Arc::new(
        AgentSession::create(AgentSessionOptions {
            session: manager,
            registry: Arc::new(registry).into(),
            apis,
            settings,
            settings_store: None,
            selection: None,
            model,
            thinking_level: None,
            tools: Vec::new(),
            active_tool_names: Vec::new(),
            tool_prompts: Vec::new(),
            system_prompt: SystemPromptOptions::new("/tmp"),
            docs_command: "bake-rs docs".into(),
            plugins: crate::plugins::PluginRegistry::default(),
            work: None,
            jobs: None,
        })
        .await
        .unwrap(),
    );
    let control = RunControl::new(RunLimits {
        max_model_calls: max_calls,
        ..RunLimits::default()
    });
    *session.core.run_control.lock().unwrap() = Some(control.clone());
    (session, faux, control)
}

#[path = "crash_tests.rs"]
mod crash;

#[path = "print_tests.rs"]
mod print;

fn summaries(faux: &FauxProvider) {
    faux.set_responses(vec![
        faux_assistant_message("## Goal\nPreserve the task").into(),
        faux_assistant_message("## Original Request\ncurrent turn").into(),
    ]);
}

async fn finished(session: &AgentSession) {
    let mut completed = session
        .core
        .background_compaction
        .pending
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .completed
        .clone();
    tokio::time::timeout(Duration::from_secs(20), completed.wait_for(|done| *done))
        .await
        .expect("summary completed")
        .unwrap();
    // Completion marks provider cleanup; the task's final guard also has to
    // drop before the owner may collect its ready result.
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if session
                .core
                .background_compaction
                .pending
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .task
                .is_finished()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("summary task joined");
}

fn count(session: &AgentSession) -> usize {
    lock(&session.session)
        .entries()
        .iter()
        .filter(|entry| entry.entry_type() == "compaction")
        .count()
}

type EventLog = Arc<Mutex<Vec<(BackgroundCompactionStatus, Option<String>)>>>;

fn events(session: &AgentSession) -> (EventLog, SessionSubscription) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&log);
    let subscription = session.subscribe(move |event| {
        if let SessionEvent::BackgroundCompaction { status, reason, .. } = event {
            captured
                .lock()
                .unwrap()
                .push((*status, reason.map(str::to_owned)));
        }
    });
    (log, subscription)
}

#[tokio::test]
async fn disabled_or_foreground_only_allowance_starts_no_summary() {
    for (enabled, allowance) in [(false, None), (true, Some(1)), (true, Some(2))] {
        let (session, faux, control) = fixture(allowance, enabled).await;
        session
            .core
            .background_compaction_boundary(true)
            .await
            .unwrap();
        assert!(
            session
                .core
                .background_compaction
                .pending
                .lock()
                .unwrap()
                .is_none()
        );
        assert_eq!(faux.call_count(), 0);
        assert_eq!(control.model_calls(), 0);
        assert_eq!(control.stopped(), None);
        session.shutdown().await;
    }
}

#[tokio::test]
async fn split_summary_leaves_one_foreground_call_and_adopts_at_the_next_prompt() {
    let (session, faux, control) = fixture(Some(3), true).await;
    summaries(&faux);
    let (log, _subscription) = events(&session);
    assert!(
        !session
            .core
            .background_compaction_boundary(true)
            .await
            .unwrap()
    );
    finished(&session).await;
    assert_eq!(control.model_calls(), 2);
    assert_eq!(count(&session), 0, "a worker cannot commit its own summary");
    faux.set_responses(vec![faux_assistant_message("next response").into()]);
    assert_eq!(
        session
            .prompt_with_control("next prompt", Vec::new(), control.clone())
            .await
            .unwrap(),
        RunOutcome::FinishedTurn
    );
    assert_eq!(control.model_calls(), 3);
    assert_eq!(count(&session), 1);
    assert!(
        log.lock()
            .unwrap()
            .iter()
            .any(|event| event.0 == BackgroundCompactionStatus::Committed)
    );
    session.shutdown().await;
}

#[tokio::test]
async fn descendant_growth_is_retained_but_context_edits_reject_the_candidate() {
    for edit in [false, true] {
        let (session, faux, control) = fixture(None, true).await;
        summaries(&faux);
        session
            .core
            .background_compaction_boundary(true)
            .await
            .unwrap();
        finished(&session).await;
        {
            let mut manager = lock(&session.session);
            if edit {
                let target = manager
                    .branch_entries(None)
                    .into_iter()
                    .find(|entry| entry.entry_type() == "message")
                    .unwrap()
                    .id()
                    .to_owned();
                manager
                    .append_context_edit(&target, Some(EditContent::Text("changed input".into())))
                    .unwrap();
            } else {
                append_turn(&mut manager, "tail appended during summary");
            }
        }
        assert_eq!(
            session
                .core
                .background_compaction_boundary(false)
                .await
                .unwrap(),
            !edit
        );
        assert_eq!(count(&session), usize::from(!edit));
        assert_eq!(
            control.model_calls(),
            2,
            "a discarded debit is not refunded"
        );
        if !edit {
            assert!(session.messages().iter().any(|message| {
                serde_json::to_string(&crate::session::AgentMessage::from(message).as_json())
                    .unwrap()
                    .contains("tail appended during summary")
            }));
        }
        session.shutdown().await;
    }
}

fn held_summary(
    entered: Arc<Notify>,
    cancelled: Arc<Notify>,
    release: Arc<Notify>,
) -> FauxResponseStep {
    FauxResponseStep::Factory(Arc::new(move |_, options, _, _| {
        let signal = options.base.signal.clone().unwrap();
        let entered = Arc::clone(&entered);
        let cancelled = Arc::clone(&cancelled);
        let release = Arc::clone(&release);
        Box::pin(async move {
            entered.notify_one();
            signal.cancelled().await;
            cancelled.notify_one();
            release.notified().await;
            Ok(faux_assistant_message("cancelled summary"))
        })
    }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_keeps_the_one_task_slot_until_provider_cleanup() {
    let (session, faux, control) = fixture(None, true).await;
    let entered = Arc::new(Notify::new());
    let cancelled = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    faux.set_responses(vec![held_summary(
        Arc::clone(&entered),
        Arc::clone(&cancelled),
        Arc::clone(&release),
    )]);
    session
        .core
        .background_compaction_boundary(true)
        .await
        .unwrap();
    entered.notified().await;
    session
        .core
        .background_compaction_boundary(true)
        .await
        .unwrap();
    assert_eq!(faux.call_count(), 1);
    let cancelling = {
        let core = Arc::clone(&session.core);
        tokio::spawn(async move { core.cancel_background_compaction("manual").await })
    };
    cancelled.notified().await;
    assert!(
        !cancelling.is_finished(),
        "cancellation must await provider cleanup"
    );
    cancelling.abort();
    assert!(cancelling.await.unwrap_err().is_cancelled());
    session
        .core
        .background_compaction_boundary(true)
        .await
        .unwrap();
    assert_eq!(
        faux.call_count(),
        1,
        "a dropped cancellation waiter retains the pending slot"
    );
    release.notify_one();
    session.core.cancel_background_compaction("manual").await;
    assert!(
        session
            .core
            .background_compaction
            .pending
            .lock()
            .unwrap()
            .is_none()
    );
    assert_eq!(count(&session), 0);
    assert_eq!(control.model_calls(), 1);
    assert_eq!(
        control.stopped(),
        None,
        "speculation cancellation does not cancel foreground"
    );
    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_compaction_and_navigation_wait_for_speculative_provider_cleanup() {
    for navigate in [false, true] {
        let (session, faux, control) = fixture(None, true).await;
        let original_leaf = session.leaf_id();
        let target = lock(&session.session)
            .entries()
            .iter()
            .find(|entry| {
                entry.get("message").and_then(|message| message.get("role"))
                    == Some(&json!("assistant"))
            })
            .unwrap()
            .id()
            .to_owned();
        let entered = Arc::new(Notify::new());
        let cancelled = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        faux.set_responses(vec![held_summary(
            Arc::clone(&entered),
            Arc::clone(&cancelled),
            Arc::clone(&release),
        )]);
        session
            .core
            .background_compaction_boundary(true)
            .await
            .unwrap();
        entered.notified().await;
        summaries(&faux);
        let changing = {
            let session = Arc::clone(&session);
            tokio::spawn(async move {
                if navigate {
                    session.navigate_tree(&target, None).await.map(|_| ())
                } else {
                    session
                        .compact_when_idle(None, AbortController::new().signal())
                        .await
                        .map(|_| ())
                }
            })
        };
        tokio::time::timeout(Duration::from_secs(20), cancelled.notified())
            .await
            .expect("the public operation cancels the speculative request");
        assert!(!changing.is_finished());
        assert_eq!(session.leaf_id(), original_leaf);
        assert_eq!(count(&session), 0);
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(20), changing)
            .await
            .expect("the public operation finishes after provider cleanup")
            .unwrap()
            .unwrap();
        assert_ne!(session.leaf_id(), original_leaf);
        assert_eq!(count(&session), usize::from(!navigate));
        assert!(
            session
                .core
                .background_compaction
                .pending
                .lock()
                .unwrap()
                .is_none()
        );
        assert_eq!(control.model_calls(), if navigate { 1 } else { 3 });
        assert_eq!(control.stopped(), None);
        session.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_awaits_the_background_provider_without_late_publication() {
    let (session, faux, _) = fixture(None, true).await;
    let (log, _subscription) = events(&session);
    let entered = Arc::new(Notify::new());
    let cancelled = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    faux.set_responses(vec![held_summary(
        Arc::clone(&entered),
        Arc::clone(&cancelled),
        Arc::clone(&release),
    )]);
    session
        .core
        .background_compaction_boundary(true)
        .await
        .unwrap();
    entered.notified().await;
    let closing = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.shutdown().await })
    };
    cancelled.notified().await;
    assert!(!closing.is_finished());
    release.notify_one();
    closing.await.unwrap();
    assert_eq!(count(&session), 0);
    assert_eq!(
        log.lock().unwrap().len(),
        1,
        "shutdown closes observers before cleanup"
    );
}

#[tokio::test]
async fn speculative_retry_exhausts_only_its_share_and_leaves_foreground_running() {
    let (session, faux, control) = fixture(Some(3), true).await;
    {
        let mut settings = session.settings.write().unwrap();
        settings.retry.enabled = true;
        settings.retry.max_retries = 1;
        settings.retry.base_delay_ms = 0;
    }
    faux.set_responses(vec![
        FauxResponseStep::from_fn(|_, _, _, _| Err("503 service overloaded".into())),
        faux_assistant_message("## Goal\nRecovered summary").into(),
    ]);
    session
        .core
        .background_compaction_boundary(true)
        .await
        .unwrap();
    finished(&session).await;
    assert!(
        !session
            .core
            .background_compaction_boundary(false)
            .await
            .unwrap()
    );
    assert_eq!(control.model_calls(), 2);
    assert_eq!(control.stopped(), None);
    assert_eq!(
        count(&session),
        0,
        "the split summary's second request was not admitted"
    );
    faux.set_responses(vec![faux_assistant_message("foreground response").into()]);
    assert_eq!(
        session
            .prompt_with_control("keep working", Vec::new(), control.clone())
            .await
            .unwrap(),
        RunOutcome::FinishedTurn
    );
    assert_eq!(control.model_calls(), 3);
    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_preprompt_adoption_cannot_abandon_context_or_lifecycle_publication() {
    let (session, faux, control) = fixture(None, true).await;
    summaries(&faux);
    let (log, _subscription) = events(&session);
    session
        .core
        .background_compaction_boundary(true)
        .await
        .unwrap();
    finished(&session).await;
    let (entered_tx, entered) = tokio::sync::oneshot::channel();
    let (release_tx, release) = std::sync::mpsc::channel();
    session.core.hooks().after_compaction_commit = Some(Box::new(move || {
        let _ = entered_tx.send(());
        let _ = release.recv();
    }));
    let prompting = {
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            session
                .prompt_with_control("next", Vec::new(), control)
                .await
        })
    };
    entered.await.unwrap();
    prompting.abort();
    assert!(prompting.await.unwrap_err().is_cancelled());
    assert!(!session.core.activity.borrow().idle());
    let refused = tokio::select! {
        biased;
        result = session.prompt("cannot overtake", Vec::new()) => result,
        () = std::future::ready(()) => panic!("a prompt must refuse the owned context commit immediately"),
    }.unwrap_err();
    assert!(refused.contains("context"), "{refused}");
    assert_eq!(
        faux.call_count(),
        2,
        "no foreground request can overtake publication"
    );
    release_tx.send(()).unwrap();
    session.core.wait_for_idle().await;
    assert_eq!(count(&session), 1);
    assert!(
        session
            .messages()
            .iter()
            .any(|message| message.role() == "compactionSummary")
    );
    assert!(
        log.lock()
            .unwrap()
            .iter()
            .any(|event| event.0 == BackgroundCompactionStatus::Committed)
    );
    assert_eq!(
        faux.call_count(),
        3,
        "the owned prompt continues after publication despite losing its waiter"
    );
    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_before_commit_discards_but_after_append_publishes() {
    for late in [false, true] {
        let (session, faux, control) = fixture(None, true).await;
        summaries(&faux);
        session
            .core
            .background_compaction_boundary(true)
            .await
            .unwrap();
        finished(&session).await;
        let (entered_tx, entered) = tokio::sync::oneshot::channel();
        let (release_tx, release) = std::sync::mpsc::channel();
        let hook: Box<dyn FnOnce() + Send> = Box::new(move || {
            let _ = entered_tx.send(());
            let _ = release.recv();
        });
        if late {
            session.core.hooks().after_compaction_commit = Some(hook);
        } else {
            session.core.hooks().before_compaction_commit = Some(hook);
        }
        let adoption = {
            let core = Arc::clone(&session.core);
            tokio::spawn(async move { core.background_compaction_boundary(false).await })
        };
        entered.await.unwrap();
        control.cancel();
        release_tx.send(()).unwrap();
        assert_eq!(adoption.await.unwrap().unwrap(), late);
        assert_eq!(count(&session), usize::from(late));
        assert_eq!(
            session
                .messages()
                .iter()
                .any(|message| message.role() == "compactionSummary"),
            late
        );
        session.shutdown().await;
    }
}

#[tokio::test]
async fn a_failed_background_commit_outranks_racing_cancellation() {
    let (session, faux, control) = fixture(None, true).await;
    summaries(&faux);
    let (log, _subscription) = events(&session);
    session
        .core
        .background_compaction_boundary(true)
        .await
        .unwrap();
    finished(&session).await;
    let late = control.clone();
    session.core.hooks().fail_compaction_entry = Some(Box::new(move || late.cancel()));
    let error = session
        .core
        .background_compaction_boundary(false)
        .await
        .unwrap_err();
    assert!(error.contains("injected failure"), "{error}");
    assert!(
        matches!(control.stopped(), Some(RunOutcome::Failed { message }) if message.contains("injected failure"))
    );
    assert_eq!(count(&session), 0);
    assert!(
        log.lock()
            .unwrap()
            .iter()
            .any(|event| event.0 == BackgroundCompactionStatus::Discarded)
    );
    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_cancel_drains_a_reserved_debit_without_dispatch_or_foreground_cancel() {
    let (session, faux, _) = fixture(None, true).await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let admission_entered = Arc::clone(&entered);
    let admission_release = Arc::clone(&release);
    let observer: bake_agent::AdmissionObserver = Arc::new(move |_| {
        let entered = Arc::clone(&admission_entered);
        let release = Arc::clone(&admission_release);
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            Ok(())
        })
    });
    let control = RunControl::with_admission_observer(RunLimits::default(), Some(observer));
    *session.core.run_control.lock().unwrap() = Some(control.clone());
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&attempts);
    let _subscription = session.subscribe(move |event| {
        if let SessionEvent::BackgroundCompaction {
            status: BackgroundCompactionStatus::Discarded,
            attempts,
            ..
        } = event
        {
            observed.lock().unwrap().extend_from_slice(attempts);
        }
    });
    session
        .core
        .background_compaction_boundary(true)
        .await
        .unwrap();
    entered.notified().await;
    let signal = session
        .core
        .background_compaction
        .pending
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .signal
        .clone();
    let cancelling = {
        let core = Arc::clone(&session.core);
        tokio::spawn(async move { core.cancel_background_compaction("manual").await })
    };
    signal.cancelled().await;
    assert!(
        !cancelling.is_finished(),
        "the mandatory debit remains owned"
    );
    assert_eq!(control.stopped(), None);
    assert_eq!(control.model_calls(), 1);
    assert_eq!(faux.call_count(), 0);
    release.notify_one();
    cancelling.await.unwrap();
    assert_eq!(control.stopped(), None);
    assert_eq!(
        control.model_calls(),
        1,
        "the completed debit is not refunded"
    );
    assert_eq!(
        faux.call_count(),
        0,
        "local cancellation is rechecked after admission"
    );
    let observed = attempts.lock().unwrap().clone();
    assert_eq!(observed.len(), 1);
    assert!(!observed[0].dispatched);
    assert_eq!(observed[0].usage, None);
    assert_eq!(observed[0].admission.as_ref().unwrap().ordinal, 1);
    session.shutdown().await;
}
