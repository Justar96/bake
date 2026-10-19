//! Recovery uses real session files and processes the test kills with
//! `SIGKILL` at durable boundaries. Each re-executed fixture owns its
//! environment, paths, writer and runtime.

use std::path::Path;

use super::*;
use bake_agent::RunLimits;

const CHILD_MODE: &str = "BAKE_TEST_RUN_JOURNAL_MODE";
const CHILD_DIR: &str = "BAKE_TEST_RUN_JOURNAL_DIR";

#[path = "run_journal_parked_tests.rs"]
mod parked;

#[path = "run_journal_job_tests.rs"]
mod jobs;

#[path = "run_journal_rerun_tests.rs"]
mod rerun;

#[path = "run_journal_kill_tests.rs"]
mod kill;

#[path = "run_journal_nested_tests.rs"]
mod nested;

fn append_fact(session: &mut SessionManager, run_id: &str, mut event: Value) -> String {
    if event["kind"] == "started" && event.get("session_id").is_none() {
        event["session_id"] = json!(session.session_id());
    }
    if event["kind"] == "started" && event.get("job_wait_ms").is_none() {
        event["job_wait_ms"] = json!(600_000);
    }
    session
        .append_durable_group(vec![GroupEntry::Custom {
            custom_type: "bake.run.v1".into(),
            data: Some(json!({"version":1,"runId":run_id,"event":event})),
        }])
        .unwrap()
        .remove(0)
}

fn checkpoint_input(session: &mut SessionManager, run: &str, max_calls: u64, timed: bool) {
    append_fact(
        session,
        run,
        json!({"kind":"started","max_model_calls":max_calls,"deadline_remaining_ms":if timed {Some(10_000)} else {None}}),
    );
    let user = session
        .append_message(UserMessage {
            content: UserContent::Text("continue the task".into()),
            timestamp: 1,
        })
        .unwrap();
    append_fact(session, run, json!({"kind":"message","entry_id":user}));
    append_fact(
        session,
        run,
        json!({"kind":"admitted","admission":{"ordinal":1,"kind":"agent","provider":"faux","model":"faux-1"}}),
    );
}

#[tokio::test]
async fn recoverable_run_records_debits_and_completion_before_reporting() {
    let dir = TempDir::new("run-journal-complete");
    let h = harness(200_000);
    h.faux
        .set_responses(vec![faux_assistant_message("done").into()]);
    let session = AgentSession::create(options(&h, file_session(&dir), Settings::default()))
        .await
        .unwrap();
    let (events, _subscription) = record(&session);
    let result = session
        .prompt_recoverable(
            "do the task",
            Vec::new(),
            RunLimits {
                max_model_calls: Some(2),
                deadline: None,
            },
        )
        .await
        .unwrap();
    let state = session.inspect_run(&result.run_id).unwrap();
    let path = session.session_file().unwrap();
    session.shutdown().await;
    assert_eq!(result.outcome, RunOutcome::FinishedTurn);
    assert_eq!(state.model_calls, 1);
    assert_eq!(
        state.disposition,
        RunRecoveryDisposition::Completed {
            outcome: RunOutcome::FinishedTurn
        }
    );
    assert_eq!(of_type(&events, &["recoverable_run_started"]).len(), 1);
    let reopened = SessionManager::open(&path, None, None).unwrap();
    let facts: Vec<_> = reopened
        .branch_entries(None)
        .iter()
        .filter_map(|entry| entry.get("data"))
        .collect();
    assert!(facts.iter().any(|fact| fact["event"]["kind"] == "terminal"));
}

#[tokio::test]
async fn recovery_preserves_spent_allowance_and_durable_notice() {
    let dir = TempDir::new("run-journal-resume");
    let mut manager = file_session(&dir);
    checkpoint_input(&mut manager, "selected", 2, false);
    let h = harness(200_000);
    let seen = Arc::new(Mutex::new(Vec::new()));
    h.faux
        .set_responses(vec![capture(&seen, faux_assistant_message("resumed"))]);
    let session = AgentSession::create(options(&h, manager, Settings::default()))
        .await
        .unwrap();
    let before = session.inspect_run("selected").unwrap();
    let result = session.resume_run("selected").await.unwrap();
    let after = session.inspect_run("selected").unwrap();
    let messages = session.messages();
    session.shutdown().await;
    assert_eq!(before.disposition, RunRecoveryDisposition::Resumable);
    assert_eq!(result.outcome, RunOutcome::FinishedTurn);
    assert_eq!(after.model_calls, 2);
    assert_eq!(h.faux.call_count(), 1);
    assert!(
        user_texts(&seen.lock().unwrap()[0])
            .iter()
            .any(|text| text.contains("durable checkpoint"))
    );
    assert!(messages.iter().any(|message| message.as_custom().is_some()));
}

#[tokio::test]
async fn missing_timed_and_exhausted_runs_make_no_provider_request() {
    for (name, timed, limit) in [("timed", true, 2), ("exhausted", false, 1)] {
        let dir = TempDir::new(name);
        let mut manager = file_session(&dir);
        checkpoint_input(&mut manager, name, limit, timed);
        let h = harness(200_000);
        let session = AgentSession::create(options(&h, manager, Settings::default()))
            .await
            .unwrap();
        assert!(
            session
                .resume_run("missing")
                .await
                .unwrap_err()
                .contains("not found")
        );
        let result = session.resume_run(name).await;
        session.shutdown().await;
        if timed {
            assert!(result.unwrap_err().contains("monotonic deadline"));
        } else {
            assert_eq!(
                result.unwrap().outcome,
                RunOutcome::BudgetExhausted {
                    limit: bake_agent::BudgetLimit::ModelCalls
                }
            );
        }
        assert_eq!(h.faux.call_count(), 0);
    }
}

#[tokio::test]
async fn recovery_rejects_unselected_and_malformed_branch_facts() {
    let dir = TempDir::new("run-branch-validation");
    let mut manager = file_session(&dir);
    let branch_point = manager.append_custom_entry("fixture", None).unwrap();
    checkpoint_input(&mut manager, "off-branch", 3, false);
    manager.branch(&branch_point).unwrap();
    checkpoint_input(&mut manager, "selected", 3, false);
    append_fact(
        &mut manager,
        "selected",
        json!({"kind":"admitted","admission":{"ordinal":3,"kind":"agent","provider":"faux","model":"faux-1"}}),
    );
    let h = harness(200_000);
    let session = AgentSession::create(options(&h, manager, Settings::default()))
        .await
        .unwrap();
    let missing = session.resume_run("off-branch").await.unwrap_err();
    let malformed = session.resume_run("selected").await.unwrap_err();
    session.shutdown().await;
    assert!(missing.contains("selected session branch"));
    assert!(malformed.contains("ordinals"));
    assert_eq!(h.faux.call_count(), 0);
}

#[tokio::test]
async fn completed_result_without_trailing_ack_is_recovered_by_its_dispatch_identity() {
    let dir = TempDir::new("run-result-prefix");
    let mut manager = file_session(&dir);
    checkpoint_input(&mut manager, "selected", 2, false);
    let assistant = manager
        .append_message(faux_assistant_blocks(
            vec![faux_tool_call("effect", json!({}), Some("same-id"))],
            StopReason::ToolUse,
        ))
        .unwrap();
    append_fact(
        &mut manager,
        "selected",
        json!({"kind":"message","entry_id":assistant}),
    );
    append_fact(
        &mut manager,
        "selected",
        json!({"kind":"dispatch","assistant_entry_id":assistant,"tool_call_id":"same-id","tool_name":"effect"}),
    );
    manager
        .append_message(bake_ai::ToolResultMessage {
            tool_call_id: "same-id".into(),
            tool_name: "effect".into(),
            content: vec![UserContentBlock::Text(TextContent::new(
                "completed external effect",
            ))],
            is_error: false,
            timestamp: 2,
            ..Default::default()
        })
        .unwrap();
    let h = harness(200_000);
    let seen = Arc::new(Mutex::new(Vec::new()));
    h.faux
        .set_responses(vec![capture(&seen, faux_assistant_message("done"))]);
    let session = AgentSession::create(options(&h, manager, Settings::default()))
        .await
        .unwrap();
    let before = session.inspect_run("selected").unwrap();
    let result = session.resume_run("selected").await;
    session.shutdown().await;
    assert_eq!(before.disposition, RunRecoveryDisposition::Resumable);
    assert_eq!(result.unwrap().outcome, RunOutcome::FinishedTurn);
    assert!(seen.lock().unwrap()[0].messages().iter().any(
        |message| matches!(message, Message::ToolResult(result) if result.tool_call_id == "same-id")
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_run_journal_admissions_preserve_durable_ordinal_order() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};
    let dir = TempDir::new("run-journal-order");
    let h = harness(200_000);
    h.faux.set_responses(vec![
        faux_assistant_blocks(
            vec![faux_tool_call("held", json!({}), Some("held-call"))],
            StopReason::ToolUse,
        )
        .into(),
    ]);
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let reached = Arc::clone(&entered);
    let held = Arc::clone(&release);
    let tool = AgentTool::new(
        "held",
        "held",
        "Wait for test admission writes",
        json!({"type":"object"}),
        move |_, _, _, _, _ctx| {
            let reached = Arc::clone(&reached);
            let held = Arc::clone(&held);
            Box::pin(async move {
                reached.notify_one();
                held.notified().await;
                Ok(AgentToolResult::text("done"))
            })
        },
    );
    let session = Arc::new(
        AgentSession::create(AgentSessionOptions {
            tools: vec![Arc::new(tool)],
            active_tool_names: vec!["held".into()],
            ..options(&h, file_session(&dir), Settings::default())
        })
        .await
        .unwrap(),
    );
    let owner = Arc::clone(&session);
    let run = tokio::spawn(async move {
        owner
            .prompt_recoverable(
                "run",
                Vec::new(),
                RunLimits {
                    deadline: None,
                    max_model_calls: Some(3),
                },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    let control = session.core.control().unwrap();
    let mut first =
        Box::pin(control.admit_model_call(bake_agent::ModelCallKind::Summary, h.faux.model()));
    let mut second =
        Box::pin(control.admit_model_call(bake_agent::ModelCallKind::Agent, h.faux.model()));
    let reserved_while_blocked = {
        // Neither blocking write can finish under this lock. Polling both
        // admissions proves that the second cannot reserve or schedule its
        // writer ahead of the first; no lock crosses an await.
        let _session_write = lock(&session.session);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(matches!(first.as_mut().poll(&mut cx), Poll::Pending));
        assert!(matches!(second.as_mut().poll(&mut cx), Poll::Pending));
        control.model_calls()
    };
    let (one, two) = tokio::join!(first, second);
    release.notify_one();
    let result = run.await.unwrap().unwrap();
    let rows: Vec<_> = lock(&session.session)
        .branch_entries(None)
        .iter()
        .filter_map(|entry| entry.get("data"))
        .filter(|data| data["event"]["kind"] == "admitted")
        .map(|data| data["event"]["admission"]["ordinal"].as_u64().unwrap())
        .collect();
    let recovered = session.inspect_run(&result.run_id).unwrap();
    session.shutdown().await;
    assert_eq!(reserved_while_blocked, 2);
    assert_eq!(one.unwrap().ordinal, 2);
    assert_eq!(two.unwrap().ordinal, 3);
    assert_eq!(rows, [1, 2, 3]);
    assert_eq!(recovered.model_calls, 3);
    assert_eq!(
        result.outcome,
        RunOutcome::BudgetExhausted {
            limit: bake_agent::BudgetLimit::ModelCalls
        }
    );
}

#[tokio::test]
async fn foreign_run_facts_and_contradictory_completion_are_refused() {
    for case in ["foreign", "incomplete"] {
        let dir = TempDir::new(case);
        let mut manager = file_session(&dir);
        checkpoint_input(&mut manager, "selected", 3, false);
        if case == "foreign" {
            append_fact(
                &mut manager,
                "another",
                json!({"kind":"admitted","admission":{"ordinal":1,"kind":"agent","provider":"faux","model":"faux-1"}}),
            );
        } else {
            let assistant = manager
                .append_message(faux_assistant_blocks(
                    vec![faux_tool_call("effect", json!({}), Some("call"))],
                    StopReason::ToolUse,
                ))
                .unwrap();
            append_fact(
                &mut manager,
                "selected",
                json!({"kind":"message","entry_id":assistant}),
            );
            append_fact(
                &mut manager,
                "selected",
                json!({"kind":"dispatch","assistant_entry_id":assistant,"tool_call_id":"call","tool_name":"effect"}),
            );
            append_fact(
                &mut manager,
                "selected",
                json!({"kind":"terminal","outcome":{"kind":"finished_turn"}}),
            );
        }
        let h = harness(200_000);
        let session = AgentSession::create(options(&h, manager, Settings::default()))
            .await
            .unwrap();
        let error = session.resume_run("selected").await.unwrap_err();
        session.shutdown().await;
        assert!(error.contains(if case == "foreign" {
            "attributed"
        } else {
            "contradicts"
        }));
        assert_eq!(h.faux.call_count(), 0);
    }
}

#[tokio::test]
async fn missing_original_limit_fields_do_not_become_unlimited_or_untimed() {
    for event in [
        json!({"kind":"started","max_model_calls":2}),
        json!({"kind":"started","deadline_remaining_ms":null}),
    ] {
        let dir = TempDir::new("run-missing-limits");
        let mut manager = file_session(&dir);
        append_fact(&mut manager, "selected", event);
        let h = harness(200_000);
        let session = AgentSession::create(options(&h, manager, Settings::default()))
            .await
            .unwrap();
        let error = session.resume_run("selected").await.unwrap_err();
        session.shutdown().await;
        assert!(error.contains("missing field"));
        assert_eq!(h.faux.call_count(), 0);
    }
}

#[tokio::test]
async fn copied_run_identity_and_foreign_custom_context_cannot_continue() {
    for foreign in [true, false] {
        let dir = TempDir::new("run-origin");
        let mut manager = file_session(&dir);
        if foreign {
            append_fact(
                &mut manager,
                "selected",
                json!({"kind":"started","session_id":"different-origin","max_model_calls":3,"deadline_remaining_ms":null}),
            );
        } else {
            checkpoint_input(&mut manager, "selected", 3, false);
        }
        manager
            .append_custom_message_entry(
                "other-extension",
                UserContent::Text("unrelated task".into()),
                true,
                None,
            )
            .unwrap();
        let h = harness(200_000);
        let session = AgentSession::create(options(&h, manager, Settings::default()))
            .await
            .unwrap();
        let error = session.resume_run("selected").await.unwrap_err();
        session.shutdown().await;
        assert!(error.contains(if foreign {
            "another session"
        } else {
            "attributed"
        }));
        assert_eq!(h.faux.call_count(), 0);
    }
}

#[tokio::test]
async fn branching_before_a_debit_cannot_refund_the_original_allowance() {
    let dir = TempDir::new("run-hidden-debit");
    let mut manager = file_session(&dir);
    checkpoint_input(&mut manager, "selected", 3, false);
    let admitted = manager.leaf_id().unwrap().to_owned();
    let branch_from = manager
        .entry(&admitted)
        .unwrap()
        .parent_id()
        .unwrap()
        .to_owned();
    manager.branch(&branch_from).unwrap();
    manager.append_custom_entry("new-branch", None).unwrap();
    let h = harness(200_000);
    let session = AgentSession::create(options(&h, manager, Settings::default()))
        .await
        .unwrap();
    let error = session.resume_run("selected").await.unwrap_err();
    session.shutdown().await;
    assert!(error.contains("outside the selected branch"));
    assert_eq!(h.faux.call_count(), 0);
}

#[tokio::test]
async fn an_aborted_assistant_without_terminal_is_not_restarted() {
    let dir = TempDir::new("run-cancelled-tail");
    let mut manager = file_session(&dir);
    checkpoint_input(&mut manager, "selected", 3, false);
    let mut assistant = faux_assistant_message("interrupted");
    assistant.stop_reason = StopReason::Aborted;
    let entry = manager.append_message(assistant).unwrap();
    append_fact(
        &mut manager,
        "selected",
        json!({"kind":"message","entry_id":entry}),
    );
    let h = harness(200_000);
    let session = AgentSession::create(options(&h, manager, Settings::default()))
        .await
        .unwrap();
    let error = session.resume_run("selected").await.unwrap_err();
    session.shutdown().await;
    assert!(error.contains("cancelled"));
    assert_eq!(h.faux.call_count(), 0);
}

#[tokio::test]
async fn completed_selection_returns_its_own_assistant_before_later_turns() {
    let dir = TempDir::new("run-selected-answer");
    let h = harness(200_000);
    h.faux.set_responses(vec![
        faux_assistant_message("selected answer").into(),
        faux_assistant_message("later answer").into(),
    ]);
    let session = AgentSession::create(options(&h, file_session(&dir), Settings::default()))
        .await
        .unwrap();
    let first = session
        .prompt_recoverable("first", Vec::new(), RunLimits::default())
        .await
        .unwrap();
    session.prompt("later", Vec::new()).await.unwrap();
    let recovered = session.resume_run(&first.run_id).await.unwrap();
    session.shutdown().await;
    assert_eq!(recovered.final_message, first.final_message);
    assert_eq!(
        recovered.final_message.unwrap().content,
        faux_assistant_message("selected answer").content
    );
    assert_eq!(h.faux.call_count(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_commit_closes_the_interval_and_required_failure_still_wins() {
    use crate::session::{Fault, FaultKind};
    for phase in ["before", "after", "failed"] {
        let dir = TempDir::new("run-terminal-close");
        let h = harness(200_000);
        h.faux
            .set_responses(vec![faux_assistant_message("done").into()]);
        let session = Arc::new(
            AgentSession::create(options(&h, file_session(&dir), Settings::default()))
                .await
                .unwrap(),
        );
        let entered = Arc::new(tokio::sync::Notify::new());
        let reached = Arc::clone(&entered);
        let (release, held) = std::sync::mpsc::channel();
        let hook: Box<dyn FnOnce() + Send> = Box::new(move || {
            reached.notify_one();
            held.recv_timeout(Duration::from_secs(10)).unwrap();
        });
        {
            let mut hooks = session.core.hooks.lock().unwrap();
            if phase == "before" {
                hooks.before_run_terminal = Some(hook);
            } else {
                hooks.after_run_terminal_admission = Some(hook);
            }
        }
        let store = Arc::clone(&session.session);
        let _subscription = session.subscribe(move |event| {
            if phase == "failed" && matches!(event, SessionEvent::Agent(AgentEvent::MessageEnd { message }) if message.as_assistant().is_some()) {
                lock(&store).arm_group_fault(Fault { line:0, kind:FaultKind::Refuse });
            }
        });
        let owner = Arc::clone(&session);
        let run = tokio::spawn(async move {
            owner
                .prompt_recoverable("run", Vec::new(), RunLimits::default())
                .await
        });
        tokio::time::timeout(Duration::from_secs(10), entered.notified())
            .await
            .unwrap();
        let control = session.core.control().unwrap();
        control.cancel();
        release.send(()).unwrap();
        let result = run.await.unwrap().unwrap();
        let persisted = session.inspect_run(&result.run_id).unwrap();
        session.shutdown().await;
        match phase {
            "before" => {
                assert_eq!(result.outcome, RunOutcome::Cancelled);
                assert_eq!(
                    persisted.disposition,
                    RunRecoveryDisposition::Completed {
                        outcome: RunOutcome::Cancelled
                    }
                );
            }
            "after" => {
                assert_eq!(result.outcome, RunOutcome::FinishedTurn);
                assert_eq!(control.stopped(), Some(RunOutcome::FinishedTurn));
            }
            "failed" => {
                assert!(matches!(result.outcome, RunOutcome::Failed { .. }));
                assert_eq!(control.stopped(), Some(result.outcome));
            }
            _ => unreachable!(),
        }
    }
}

#[tokio::test]
async fn resumed_tools_use_current_permissions_without_inheriting_approval() {
    use crate::permissions::{ApprovalPolicy, NeverApprover, StandingPermissions, ToolPermissions};
    let dir = TempDir::new("run-current-permissions");
    let mut manager = file_session(&dir);
    checkpoint_input(&mut manager, "selected", 4, false);
    let permissions = ToolPermissions::new(
        Arc::new(StandingPermissions::new(
            bake_sandbox::SandboxMode::ReadOnly,
            ApprovalPolicy::Never,
            "read-only",
            dir.path(),
            None,
        )),
        Arc::new(NeverApprover),
        Arc::new(bake_sandbox::UnavailableProvider::new(None)),
    );
    let tool =
        crate::permissions::fs::create_sandboxed_write_tool_definition(dir.path(), &permissions)
            .tool;
    let h = harness(200_000);
    h.faux.set_responses(vec![
        faux_assistant_blocks(vec![faux_tool_call("write", json!({"path":"must-not-exist","content":"effect","sandbox_permissions":"danger-full-access","justification":"complete the resumed task"}), Some("new-call"))], StopReason::ToolUse).into(),
        faux_assistant_message("permission denied").into(),
    ]);
    let session = AgentSession::create(AgentSessionOptions {
        tools: vec![Arc::new(tool)],
        active_tool_names: vec!["write".into()],
        ..options(&h, manager, Settings::default())
    })
    .await
    .unwrap();
    let result = session.resume_run("selected").await;
    let denied = session
        .messages()
        .iter()
        .filter_map(|message| message.as_tool_result())
        .any(|result| result.is_error);
    session.shutdown().await;
    assert_eq!(result.unwrap().outcome, RunOutcome::FinishedTurn);
    assert!(denied);
    assert!(!dir.join("must-not-exist").exists());
    assert_eq!(h.faux.call_count(), 2);
}

#[tokio::test]
async fn required_journal_write_failures_stop_provider_tool_and_success_publication() {
    use crate::session::{Fault, FaultKind};
    for phase in ["start", "admission", "dispatch", "terminal"] {
        let dir = TempDir::new(phase);
        let h = harness(200_000);
        h.faux.set_responses(vec![if phase == "dispatch" {
            faux_assistant_blocks(
                vec![faux_tool_call("effect", json!({}), Some("call"))],
                StopReason::ToolUse,
            )
            .into()
        } else {
            faux_assistant_message("done").into()
        }]);
        let effects = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&effects);
        let tool = AgentTool::new(
            "effect",
            "effect",
            "Count effects",
            json!({"type":"object"}),
            move |_, _, _, _, _ctx| {
                counter.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(AgentToolResult::text("effect")) })
            },
        );
        let session = AgentSession::create(AgentSessionOptions {
            tools: vec![Arc::new(tool)],
            active_tool_names: vec!["effect".into()],
            ..options(&h, file_session(&dir), Settings::default())
        })
        .await
        .unwrap();
        let store = Arc::clone(&session.session);
        let _subscription = session.subscribe(move |event| {
            if let SessionEvent::Agent(AgentEvent::MessageEnd { message }) = event {
                let arm = (phase == "admission" && message.as_user().is_some())
                    || ((phase == "dispatch" || phase == "terminal")
                        && message.as_assistant().is_some());
                if arm {
                    lock(&store).arm_group_fault(Fault {
                        line: 0,
                        kind: FaultKind::Refuse,
                    });
                }
            }
        });
        if phase == "start" {
            lock(&session.session).arm_group_fault(Fault {
                line: 0,
                kind: FaultKind::Refuse,
            });
        }
        let result = session
            .prompt_recoverable(
                "run",
                Vec::new(),
                RunLimits {
                    deadline: None,
                    max_model_calls: Some(3),
                },
            )
            .await
            .unwrap();
        let path = session.session_file().unwrap();
        session.shutdown().await;
        assert!(
            matches!(result.outcome, RunOutcome::Failed { .. }),
            "{phase}: {result:?}"
        );
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{phase}");
        assert_eq!(
            h.faux.call_count(),
            u64::from(phase == "dispatch" || phase == "terminal"),
            "{phase}"
        );
        if phase == "terminal" {
            let reopened = AgentSession::create(options(
                &h,
                SessionManager::open(&path, None, None).unwrap(),
                Settings::default(),
            ))
            .await
            .unwrap();
            let outcome = reopened.resume_run(&result.run_id).await.unwrap().outcome;
            reopened.shutdown().await;
            assert_eq!(outcome, RunOutcome::FinishedTurn);
            assert_eq!(h.faux.call_count(), 1);
        }
    }
}

/// Reports the fixture's kill point to the parent, then holds the process
/// there until the parent's `SIGKILL`.
fn halt(directory: &Path) -> ! {
    std::fs::write(directory.join("ready"), b"").unwrap();
    loop {
        std::thread::park();
    }
}

/// Where a [`fixture_tool`] holds its process for the parent's kill.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hold {
    Never,
    BeforeEffect,
    AfterEffect,
}

/// A tool of class `replay` whose effect appends its name to
/// `directory/<name>` and syncs it, so a parent counts effects across
/// processes. With `steer`, it first steers that session's running turn.
fn fixture_tool(
    name: &'static str,
    replay: bake_agent::ToolReplay,
    directory: &Path,
    hold: Hold,
    steer: Option<Arc<OnceLock<std::sync::Weak<AgentSession>>>>,
) -> Arc<AgentTool> {
    let directory = directory.to_owned();
    let mut tool = AgentTool::new(
        name,
        name,
        "Write the fixture effect",
        json!({"type":"object"}),
        move |_, _, _, _, _ctx| {
            let directory = directory.clone();
            let steer = steer.clone();
            Box::pin(async move {
                if let Some(session) = steer.as_ref().and_then(|slot| slot.get()?.upgrade()) {
                    session.steer("also this", Vec::new()).await.unwrap();
                }
                if hold == Hold::BeforeEffect {
                    halt(&directory);
                }
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(directory.join(name))
                    .unwrap();
                std::io::Write::write_all(&mut file, format!("{name}\n").as_bytes()).unwrap();
                file.sync_all().unwrap();
                if hold == Hold::AfterEffect {
                    halt(&directory);
                }
                Ok(AgentToolResult::text(format!("{name} done")))
            })
        },
    );
    tool.replay = Some(replay);
    Arc::new(tool)
}

/// Runs one prompt to the kill point its mode names and halts there; see
/// [`kill_at`]. `first-prompt` and `plain-assistant` are ordinary prompts,
/// every other mode a recoverable run.
#[test]
fn recoverable_run_crash_fixture() {
    let Ok(mode) = std::env::var(CHILD_MODE) else {
        return;
    };
    let directory = PathBuf::from(std::env::var_os(CHILD_DIR).unwrap());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move {
        let mode = mode.as_str();
        let h = harness(200_000);
        let manager = SessionManager::create(
            &directory.to_string_lossy(),
            Some(&directory.join("sessions")),
            NewSessionOptions::default(),
        )
        .unwrap();
        let path = manager.session_file().unwrap().to_owned();
        std::fs::write(
            directory.join("session-path"),
            path.to_string_lossy().as_bytes(),
        )
        .unwrap();
        let hold = |tool: &str| match (mode, tool) {
            ("pre-effect", "effect") | ("pre-read", "probe") => Hold::BeforeEffect,
            ("dispatch", "effect") | ("read", "probe") => Hold::AfterEffect,
            _ => Hold::Never,
        };
        let slot = Arc::new(OnceLock::new());
        let tools = vec![
            fixture_tool(
                "effect",
                bake_agent::ToolReplay::Never,
                &directory,
                hold("effect"),
                (mode == "steer").then(|| Arc::clone(&slot)),
            ),
            fixture_tool(
                "probe",
                bake_agent::ToolReplay::Safe,
                &directory,
                hold("probe"),
                None,
            ),
        ];
        let tool_use = |name: &str| -> FauxResponseStep {
            faux_assistant_blocks(
                vec![faux_tool_call(name, json!({}), Some("call"))],
                StopReason::ToolUse,
            )
            .into()
        };
        let halting = {
            let directory = directory.clone();
            FauxResponseStep::from_fn(move |_, _, _, _| halt(&directory))
        };
        // The job-notice job, settled while the first answer is produced
        // and ended before that answer returns, so its notice opens the
        // next request instead of a job wait, which a restart refuses.
        let settle = Arc::new(tokio::sync::Notify::new());
        let fixture_jobs: Arc<std::sync::OnceLock<crate::jobs::JobRegistry>> = Arc::default();
        let settle_then_answer = {
            let (settle, fixture_jobs) = (Arc::clone(&settle), Arc::clone(&fixture_jobs));
            FauxResponseStep::from_fn(move |_, _, _, _| {
                let jobs = fixture_jobs.get().ok_or("no fixture job")?;
                settle.notify_one();
                // `block_in_place` hands this worker's queue, which holds
                // the woken job, to another thread while this one waits.
                tokio::task::block_in_place(|| {
                    let bound = std::time::Instant::now() + Duration::from_secs(10);
                    while !jobs.list().iter().all(|job| job.status.is_terminal()) {
                        if std::time::Instant::now() >= bound {
                            return Err("the fixture job never ended".to_owned());
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Ok(())
                })?;
                Ok(faux_assistant_message("complete"))
            })
        };
        h.faux.set_responses(match mode {
            "admission" | "timed" | "first-prompt" => vec![halting],
            "assistant" | "plain-assistant" | "pre-effect" | "dispatch" | "result" => {
                vec![tool_use("effect")]
            }
            "pre-read" | "read" => vec![tool_use("probe")],
            "retry" => vec![failure("overloaded_error"), halting],
            "follow-up" => vec![faux_assistant_message("complete").into(), halting],
            "job-notice" => vec![settle_then_answer, halting],
            "steer" => vec![tool_use("effect"), halting],
            "compaction" => vec![
                tool_use("effect"),
                faux_assistant_message("## Goal\nCompacted").into(),
                halting,
            ],
            _ => vec![faux_assistant_message("complete").into()],
        });
        let settings = match mode {
            "retry" => retry_settings(3, 60_000),
            // A context over 10 tokens crosses the threshold.
            "compaction" => settings(json!({
                "compaction": { "reserveTokens": 199_990, "keepRecentTokens": 1 },
            })),
            _ => Settings::default(),
        };
        let session = Arc::new(
            AgentSession::create(AgentSessionOptions {
                tools,
                active_tool_names: vec!["effect".into(), "probe".into()],
                ..options(&h, manager, settings)
            })
            .await
            .unwrap(),
        );
        let _ = slot.set(Arc::downgrade(&session));
        if mode == "job-notice" {
            let settled = Arc::clone(&settle);
            session
                .jobs()
                .start(
                    crate::jobs::JobSpec {
                        kind: "bash",
                        label: "fixture job".into(),
                        output: crate::jobs::OutputMode::Stream,
                    },
                    move |_| async move {
                        settled.notified().await;
                        crate::jobs::JobOutcome::completed("fixture job done")
                    },
                )
                .unwrap();
            let _ = fixture_jobs.set(session.jobs().clone());
        }
        let observe_mode = mode.to_owned();
        let _subscription = session.subscribe(move |event| {
            if let SessionEvent::RecoverableRunStarted { run_id } = event {
                std::fs::write(directory.join("run-id"), run_id).unwrap();
                if observe_mode == "start" {
                    halt(&directory);
                }
            }
            if let SessionEvent::Agent(AgentEvent::MessageEnd { message }) = event {
                let calls = message
                    .as_assistant()
                    .is_some_and(|assistant| assistant.stop_reason == StopReason::ToolUse);
                let here = match observe_mode.as_str() {
                    "assistant" | "plain-assistant" => calls,
                    "result" => message.as_tool_result().is_some(),
                    "final" => message.as_assistant().is_some() && !calls,
                    _ => false,
                };
                if here {
                    halt(&directory);
                }
            }
        });
        if mode == "follow-up" {
            session.follow_up("then this", Vec::new()).await.unwrap();
        }
        if mode == "first-prompt" || mode == "plain-assistant" {
            let _ = session.prompt("perform effect", Vec::new()).await;
        } else {
            let _ = session
                .prompt_recoverable(
                    "perform effect",
                    Vec::new(),
                    RunLimits {
                        max_model_calls: Some(4),
                        deadline: (mode == "timed")
                            .then(|| tokio::time::Instant::now() + Duration::from_secs(60)),
                    },
                )
                .await;
        }
        panic!("fixture did not reach its kill point");
    });
}

/// What a killed fixture left: its directory, session file, and run.
struct Killed {
    dir: TempDir,
    path: PathBuf,
    run_id: Option<String>,
}

/// Runs [`recoverable_run_crash_fixture`] in `mode` as a child process and
/// kills it at its kill point; see [`kill_fixture`].
async fn kill_at(mode: &str) -> Killed {
    let dir = TempDir::new(&format!("run-journal-kill-{mode}"));
    kill_fixture(
        "agent_session::recovery_tests::run_journal_tests::recoverable_run_crash_fixture",
        &[
            (CHILD_MODE, mode.as_ref()),
            (CHILD_DIR, dir.path().as_os_str()),
        ],
        dir.path(),
    )
    .await;
    let path = PathBuf::from(std::fs::read_to_string(dir.join("session-path")).unwrap());
    let run_id = std::fs::read_to_string(dir.join("run-id")).ok();
    Killed { dir, path, run_id }
}

/// Runs the fixture test `name` as a child process with `env`, waits until
/// it reports its kill point in `directory` ([`halt`]), and kills it with
/// `SIGKILL`.
async fn kill_fixture(name: &str, env: &[(&str, &std::ffi::OsStr)], directory: &Path) {
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .envs(env.iter().copied())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    // Polled with a watchdog; a dropped child is killed.
    let watchdog = tokio::time::Instant::now() + Duration::from_secs(60);
    while !directory.join("ready").exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("{env:?}: the fixture exited ({status}) before its kill point");
        }
        assert!(
            tokio::time::Instant::now() < watchdog,
            "{env:?}: the fixture did not reach its kill point"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    child.kill().await.unwrap();
    let status = child.wait().await.unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{env:?}: {status}");
    }
    #[cfg(not(unix))]
    assert!(!status.success(), "{env:?}: {status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_crashes_preserve_recovery_boundaries_without_replaying_effects() {
    for mode in ["start", "admission", "timed", "dispatch", "result", "final"] {
        let Killed { dir, path, run_id } = kill_at(mode).await;
        let run_id = run_id.unwrap();
        let manager = SessionManager::open(&path, None, None).unwrap();
        let h = harness(200_000);
        h.faux
            .set_responses(vec![faux_assistant_message("recovered").into()]);
        let session = AgentSession::create(options(&h, manager, Settings::default()))
            .await
            .unwrap();
        let before = session.inspect_run(&run_id).unwrap();
        let result = session.resume_run(&run_id).await;
        let after = session.inspect_run(&run_id).unwrap();
        session.shutdown().await;
        match mode {
            "start" | "timed" => {
                assert!(result.is_err(), "{mode}: {result:?}");
                assert_eq!(h.faux.call_count(), 0);
            }
            "dispatch" => {
                assert!(matches!(
                    before.disposition,
                    RunRecoveryDisposition::NeedsReconciliation { .. }
                ));
                assert!(result.unwrap_err().contains("external effect"));
                assert_eq!(h.faux.call_count(), 0);
            }
            "admission" | "result" => {
                assert_eq!(result.unwrap().outcome, RunOutcome::FinishedTurn);
                assert_eq!(after.model_calls, 2);
                assert_eq!(h.faux.call_count(), 1);
            }
            "final" => {
                assert_eq!(result.unwrap().outcome, RunOutcome::FinishedTurn);
                assert_eq!(h.faux.call_count(), 0);
                let disk = std::fs::read_to_string(&path).unwrap();
                assert!(disk.contains("\"kind\":\"terminal\""));
            }
            _ => unreachable!(),
        }
        if mode == "dispatch" || mode == "result" {
            assert_eq!(
                std::fs::read_to_string(dir.join("effect")).unwrap(),
                "effect\n"
            );
        }
    }
}
