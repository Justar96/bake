//! Recovery uses real session files and process exits at durable boundaries.
//! Each re-executed fixture owns its environment, paths, writer and runtime.

use super::*;
use bake_agent::RunLimits;

const CHILD_MODE: &str = "BAKE_TEST_RUN_JOURNAL_MODE";
const CHILD_DIR: &str = "BAKE_TEST_RUN_JOURNAL_DIR";

#[path = "run_journal_parked_tests.rs"]
mod parked;

fn append_fact(session: &mut SessionManager, run_id: &str, mut event: Value) -> String {
    if event["kind"] == "started" && event.get("session_id").is_none() {
        event["session_id"] = json!(session.session_id());
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
        move |_, _, _, _| {
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
            move |_, _, _, _| {
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
        let tool_dir = directory.clone();
        let tool_mode = mode.clone();
        let tool = AgentTool::new(
            "effect",
            "effect",
            "Write the fixture effect",
            json!({"type":"object"}),
            move |_, _, _, _| {
                let directory = tool_dir.clone();
                let mode = tool_mode.clone();
                Box::pin(async move {
                    let path = directory.join("effect");
                    let mut file = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                        .unwrap();
                    std::io::Write::write_all(&mut file, b"effect\n").unwrap();
                    file.sync_all().unwrap();
                    if mode == "dispatch" {
                        std::process::exit(73);
                    }
                    Ok(AgentToolResult::text("effect recorded"))
                })
            },
        );
        if mode == "admission" || mode == "timed" {
            h.faux
                .set_responses(vec![FauxResponseStep::from_fn(|_, _, _, _| {
                    std::process::exit(73)
                })]);
        } else if mode == "dispatch" || mode == "result" {
            h.faux.set_responses(vec![
                faux_assistant_blocks(
                    vec![faux_tool_call("effect", json!({}), Some("call"))],
                    StopReason::ToolUse,
                )
                .into(),
            ]);
        } else {
            h.faux
                .set_responses(vec![faux_assistant_message("complete").into()]);
        }
        let session = AgentSession::create(AgentSessionOptions {
            tools: vec![Arc::new(tool)],
            active_tool_names: vec!["effect".into()],
            ..options(&h, manager, Settings::default())
        })
        .await
        .unwrap();
        let observe_mode = mode.clone();
        let _subscription = session.subscribe(move |event| {
            if let SessionEvent::RecoverableRunStarted { run_id } = event {
                std::fs::write(directory.join("run-id"), run_id).unwrap();
                if observe_mode == "start" {
                    std::process::exit(73);
                }
            }
            if let SessionEvent::Agent(AgentEvent::MessageEnd { message }) = event
                && ((observe_mode == "result" && message.as_tool_result().is_some())
                    || (observe_mode == "final" && message.as_assistant().is_some()))
            {
                std::process::exit(73);
            }
        });
        let _ = session
            .prompt_recoverable(
                "perform effect",
                Vec::new(),
                RunLimits {
                    max_model_calls: Some(2),
                    deadline: (mode == "timed")
                        .then(|| tokio::time::Instant::now() + Duration::from_secs(60)),
                },
            )
            .await;
        panic!("fixture did not reach crash boundary");
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_crashes_preserve_recovery_boundaries_without_replaying_effects() {
    for mode in ["start", "admission", "timed", "dispatch", "result", "final"] {
        let dir = TempDir::new(&format!("run-journal-crash-{mode}"));
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("agent_session::recovery_tests::run_journal_tests::recoverable_run_crash_fixture")
            .arg("--nocapture")
            .env(CHILD_MODE, mode)
            .env(CHILD_DIR, dir.path())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let status = match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
            Ok(status) => status.unwrap(),
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                panic!("{mode} fixture did not exit at its boundary");
            }
        };
        assert_eq!(status.code(), Some(73), "{mode}");
        let run_id = std::fs::read_to_string(dir.join("run-id")).unwrap();
        let path = PathBuf::from(std::fs::read_to_string(dir.join("session-path")).unwrap());
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
