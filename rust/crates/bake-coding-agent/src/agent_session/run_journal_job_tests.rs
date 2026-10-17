//! Job notices remain inside the selected run's durable admission interval.

use super::*;
use crate::jobs::{JobOutcome, JobSpec, OutputMode};

fn held_job(session: &AgentSession) -> Arc<tokio::sync::Notify> {
    let release = Arc::new(tokio::sync::Notify::new());
    let held = Arc::clone(&release);
    session
        .jobs()
        .start(
            JobSpec {
                kind: "bash",
                label: "held fixture job".into(),
                output: OutputMode::Stream,
            },
            move |context| async move {
                tokio::select! {
                    () = held.notified() => JobOutcome::completed("fixture completed"),
                    () = context.signal().cancelled() => JobOutcome::killed("fixture stopped"),
                }
            },
        )
        .unwrap();
    release
}

fn wait_boundary(session: &AgentSession) -> Arc<tokio::sync::Notify> {
    let entered = Arc::new(tokio::sync::Notify::new());
    let reached = Arc::clone(&entered);
    session.core.hooks().after_run_job_wait = Some(Box::new(move || reached.notify_one()));
    entered
}

#[tokio::test]
async fn job_notice_turns_finish_inside_the_original_journal_and_allowance() {
    for limit in [1, 3] {
        let dir = TempDir::new("journal-notice-budget");
        let h = harness(200_000);
        h.faux.set_responses(vec![
            faux_assistant_message("initial answer").into(),
            faux_assistant_message("job answer").into(),
        ]);
        let session = Arc::new(
            AgentSession::create(options(&h, file_session(&dir), Settings::default()))
                .await
                .unwrap(),
        );
        let release = held_job(&session);
        let entered = wait_boundary(&session);
        let owner = Arc::clone(&session);
        let run = tokio::spawn(async move {
            owner
                .prompt_recoverable(
                    "work",
                    Vec::new(),
                    RunLimits {
                        max_model_calls: Some(limit),
                        deadline: None,
                    },
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(10), entered.notified())
            .await
            .unwrap();
        assert!(!run.is_finished(), "the journal closed before its job wait");
        release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let state = session.inspect_run(&result.run_id).unwrap();
        let calls = h.faux.call_count();
        let later = held_job(&session);
        later.notify_one();
        let repeated = session.resume_run(&result.run_id).await.unwrap();
        session.shutdown().await;
        assert_eq!(
            repeated, result,
            "terminal selection changed or opened another turn"
        );
        assert_eq!(h.faux.call_count(), calls);
        assert_eq!(state.model_calls, if limit == 1 { 1 } else { 2 });
        if limit == 1 {
            assert!(matches!(
                result.outcome,
                RunOutcome::BudgetExhausted {
                    limit: bake_agent::BudgetLimit::ModelCalls
                }
            ));
        } else {
            assert_eq!(result.outcome, RunOutcome::FinishedTurn);
            assert_eq!(
                result.final_message.unwrap().content,
                faux_assistant_message("job answer").content
            );
        }
    }
}

#[tokio::test]
async fn zero_recoverable_job_wait_admits_no_post_prompt_notice() {
    let dir = TempDir::new("journal-zero-job-wait");
    let h = harness(200_000);
    h.faux
        .set_responses(vec![faux_assistant_message("initial answer").into()]);
    let session = AgentSession::create(options(&h, file_session(&dir), Settings::default()))
        .await
        .unwrap();
    let _release = held_job(&session);
    let result = session
        .prompt_recoverable_with_job_wait("work", Vec::new(), RunLimits::default(), Duration::ZERO)
        .await
        .unwrap();
    let entries: Vec<Value> = lock(&session.session)
        .entries()
        .iter()
        .map(|entry| Value::Object(entry.as_json().clone()))
        .collect();
    session.shutdown().await;
    assert_eq!(result.outcome, RunOutcome::FinishedTurn);
    assert_eq!(h.faux.call_count(), 1);
    assert!(
        !entries
            .iter()
            .any(|entry| entry["data"]["event"]["kind"] == "job_wait_started")
    );
}

#[tokio::test]
async fn recoverable_job_deadline_awaits_an_admitted_notice_and_its_cleanup() {
    let dir = TempDir::new("journal-job-deadline");
    let h = harness(200_000);
    let entered = Arc::new(tokio::sync::Notify::new());
    let cancelled = Arc::new(tokio::sync::Notify::new());
    let cleanup = Arc::new(tokio::sync::Notify::new());
    h.faux.set_responses(vec![
        faux_assistant_message("initial answer").into(),
        FauxResponseStep::Factory(Arc::new({
            let (entered, cancelled, cleanup) = (
                Arc::clone(&entered),
                Arc::clone(&cancelled),
                Arc::clone(&cleanup),
            );
            move |_, options, _, _| {
                let signal = options.base.signal.clone().unwrap();
                let (entered, cancelled, cleanup) = (
                    Arc::clone(&entered),
                    Arc::clone(&cancelled),
                    Arc::clone(&cleanup),
                );
                Box::pin(async move {
                    entered.notify_one();
                    signal.cancelled().await;
                    cancelled.notify_one();
                    cleanup.notified().await;
                    Err("cancelled notice".into())
                })
            }
        })),
    ]);
    let session = Arc::new(
        AgentSession::create(options(&h, file_session(&dir), Settings::default()))
            .await
            .unwrap(),
    );
    let release = held_job(&session);
    let waiting = wait_boundary(&session);
    let owner = Arc::clone(&session);
    let run = tokio::spawn(async move {
        owner
            .prompt_recoverable_with_job_wait(
                "work",
                Vec::new(),
                RunLimits::default(),
                Duration::from_secs(60),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), waiting.notified())
        .await
        .unwrap();
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::time::resume();
    let stopped = tokio::time::timeout(Duration::from_secs(10), cancelled.notified())
        .await
        .is_ok();
    let premature = run.is_finished();
    cleanup.notify_one();
    if !stopped {
        session.abort().await;
    }
    let result = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let state = session.inspect_run(&result.run_id).unwrap();
    session.shutdown().await;
    assert!(stopped, "notice request escaped the original job wait");
    assert!(!premature, "journal closed before owned cleanup");
    assert_eq!(result.outcome, RunOutcome::Cancelled);
    assert_eq!(
        state.disposition,
        RunRecoveryDisposition::Completed {
            outcome: RunOutcome::Cancelled
        }
    );
    assert_eq!(h.faux.call_count(), 2);
}

#[tokio::test]
async fn an_owned_job_notice_resets_completion_and_an_unowned_notice_refuses_recovery() {
    for (ack, valid) in [(true, true), (false, true), (true, false)] {
        let dir = TempDir::new("journal-notice-projection");
        let mut manager = file_session(&dir);
        checkpoint_input(&mut manager, "selected", 3, false);
        let assistant = manager
            .append_message(faux_assistant_message("earlier answer"))
            .unwrap();
        append_fact(
            &mut manager,
            "selected",
            json!({"kind":"message","entry_id":assistant}),
        );
        let id = manager
            .append_durable_group(vec![GroupEntry::CustomMessage {
                custom_type: crate::agent_session::job_notices::NOTICE_TYPE.into(),
                content: UserContent::Text("completed background job".into()),
                display: true,
                details: Some(
                    json!({"jobId":"bash-1","status": if valid { "completed" } else { "running" }}),
                ),
            }])
            .unwrap()
            .remove(0);
        if ack {
            append_fact(
                &mut manager,
                "selected",
                json!({"kind":"message","entry_id":id}),
            );
        }
        let h = harness(200_000);
        h.faux
            .set_responses(vec![faux_assistant_message("noticed answer").into()]);
        let session = AgentSession::create(options(&h, manager, Settings::default()))
            .await
            .unwrap();
        let result = session.resume_run("selected").await;
        session.shutdown().await;
        if ack && valid {
            assert_eq!(
                result.unwrap().final_message.unwrap().content,
                faux_assistant_message("noticed answer").content
            );
            assert_eq!(h.faux.call_count(), 1);
        } else {
            assert!(result.is_err());
            assert_eq!(h.faux.call_count(), 0);
        }
    }
}

const JOB_CHILD_DIR: &str = "BAKE_TEST_RUN_JOB_CRASH_DIR";

#[tokio::test]
async fn recovery_delivers_an_unfinished_job_as_unknown_before_its_first_request() {
    let dir = TempDir::new("journal-job-unknown");
    let mut manager = file_session(&dir);
    checkpoint_input(&mut manager, "selected", 3, false);
    let invocation = manager
        .append_message(faux_assistant_blocks(
            vec![faux_tool_call(
                "bash",
                json!({"command":"effect","background":true}),
                Some("start-job"),
            )],
            StopReason::ToolUse,
        ))
        .unwrap();
    append_fact(
        &mut manager,
        "selected",
        json!({"kind":"message","entry_id":invocation}),
    );
    append_fact(
        &mut manager,
        "selected",
        json!({"kind":"dispatch","assistant_entry_id":invocation,"tool_call_id":"start-job","tool_name":"bash"}),
    );
    manager.append_durable_group(vec![GroupEntry::Custom {
        custom_type: "bake.job.state".into(),
        data: Some(json!({"version":1,"phase":"started","jobId":"bash-1","kind":"bash","label":"prior process effect","status":"running"})),
    }]).unwrap();
    let started = manager.append_message(bake_ai::ToolResultMessage {
        tool_call_id: "start-job".into(), tool_name: "bash".into(),
        content: vec![UserContentBlock::Text(TextContent::new("background job bash-1 is running"))],
        details: Some(json!({crate::tools::bash::BACKGROUND_JOB_DETAIL:{"id":"bash-1","kind":"bash","label":"prior process effect","status":"running"}})),
        timestamp:2, ..Default::default()
    }).unwrap();
    append_fact(
        &mut manager,
        "selected",
        json!({"kind":"message","entry_id":started}),
    );
    append_fact(
        &mut manager,
        "selected",
        json!({"kind":"admitted","admission":{"ordinal":2,"kind":"agent","provider":"faux","model":"faux-1"}}),
    );
    let assistant = manager
        .append_message(faux_assistant_message(
            "initial response before the job completed",
        ))
        .unwrap();
    append_fact(
        &mut manager,
        "selected",
        json!({"kind":"message","entry_id":assistant}),
    );
    let h = harness(200_000);
    let seen = Arc::new(Mutex::new(Vec::new()));
    h.faux.set_responses(vec![capture(
        &seen,
        faux_assistant_message("checking unknown effects"),
    )]);
    let session = AgentSession::create(options(&h, manager, Settings::default()))
        .await
        .unwrap();
    let state = session.inspect_run("selected").unwrap();
    let result = session.resume_run("selected").await;
    let live_jobs = session.jobs().list();
    session.shutdown().await;
    assert_eq!(state.disposition, RunRecoveryDisposition::Resumable);
    assert_eq!(result.unwrap().outcome, RunOutcome::FinishedTurn);
    assert_eq!(h.faux.call_count(), 1);
    assert!(live_jobs.is_empty(), "recovery restarted the old producer");
    assert!(
        user_texts(&seen.lock().unwrap()[0])
            .iter()
            .any(|text| text.contains("no recorded final outcome")
                && text.contains("Check whether its effects completed"))
    );
}

#[tokio::test]
async fn print_reports_a_required_job_terminal_write_failure_during_shutdown() {
    use crate::print_mode::{
        PrintModeOptions, PrintOutput, PrintRunRecovery, SharedWriter, run_print_mode,
    };
    use crate::session::{Fault, FaultKind};
    let dir = TempDir::new("print-job-terminal-failure");
    let h = harness(200_000);
    h.faux
        .set_responses(vec![faux_assistant_message("initial answer").into()]);
    let session = AgentSession::create(options(&h, file_session(&dir), Settings::default()))
        .await
        .unwrap();
    let _release = held_job(&session);
    let store = Arc::clone(&session.session);
    let _subscription = session.subscribe(move |event| {
        if matches!(event, SessionEvent::AgentSettled { .. }) {
            lock(&store).arm_group_fault(Fault {
                line: 0,
                kind: FaultKind::Refuse,
            });
        }
    });
    let output: SharedWriter = Arc::new(Mutex::new(Vec::<u8>::new()));
    let errors = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr: SharedWriter = errors.clone();
    let code = run_print_mode(
        &session,
        PrintModeOptions {
            control: RunControl::default(),
            recovery: PrintRunRecovery::Disabled,
            mode: PrintOutput::Text,
            initial_message: Some("work".into()),
            initial_images: Vec::new(),
            messages: Vec::new(),
            job_wait: Duration::ZERO,
        },
        output,
        stderr,
    )
    .await;
    assert_eq!(code, 1, "print hid a failed mandatory terminal write");
    assert!(session.persistence_failure().is_some());
    assert!(!errors.lock().unwrap().is_empty());
}

#[tokio::test]
async fn print_resume_cannot_replace_the_selected_runs_spent_allowance() {
    use crate::print_mode::{
        PrintModeOptions, PrintOutput, PrintRunRecovery, SharedWriter, run_print_mode,
    };
    let dir = TempDir::new("print-selected-limits");
    let mut manager = file_session(&dir);
    checkpoint_input(&mut manager, "selected", 2, false);
    let h = harness(200_000);
    h.faux.set_responses(vec![
        faux_assistant_blocks(
            vec![faux_tool_call("inspect", json!({}), Some("inspect-call"))],
            StopReason::ToolUse,
        )
        .into(),
        faux_assistant_message("unexpected refunded request").into(),
    ]);
    let tool = AgentTool::new(
        "inspect",
        "inspect",
        "Inspect the fixture",
        json!({"type":"object"}),
        |_, _, _, _| Box::pin(async { Ok(AgentToolResult::text("fixture")) }),
    );
    let session = AgentSession::create(AgentSessionOptions {
        tools: vec![Arc::new(tool)],
        active_tool_names: vec!["inspect".into()],
        ..options(&h, manager, Settings::default())
    })
    .await
    .unwrap();
    let out: SharedWriter = Arc::new(Mutex::new(Vec::<u8>::new()));
    let err: SharedWriter = Arc::new(Mutex::new(Vec::<u8>::new()));
    let replacement = RunControl::new(RunLimits {
        max_model_calls: Some(100),
        deadline: None,
    });
    let code = run_print_mode(
        &session,
        PrintModeOptions {
            control: replacement.clone(),
            recovery: PrintRunRecovery::Resume("selected".into()),
            mode: PrintOutput::Json,
            initial_message: None,
            initial_images: Vec::new(),
            messages: Vec::new(),
            job_wait: Duration::ZERO,
        },
        out,
        err,
    )
    .await;
    let state = session.inspect_run("selected").unwrap();
    assert_eq!(code, 1);
    assert_eq!(h.faux.call_count(), 1);
    assert_eq!(replacement.model_calls(), 0);
    assert_eq!(state.model_calls, 2);
    assert_eq!(
        state.disposition,
        RunRecoveryDisposition::Completed {
            outcome: RunOutcome::BudgetExhausted {
                limit: bake_agent::BudgetLimit::ModelCalls
            }
        }
    );
}

#[test]
fn job_wait_crash_child() {
    let Some(directory) = std::env::var_os(JOB_CHILD_DIR) else {
        return;
    };
    let directory = PathBuf::from(directory);
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let h = harness(200_000);
            h.faux
                .set_responses(vec![faux_assistant_message("initial answer").into()]);
            let manager = SessionManager::create(
                &directory.to_string_lossy(),
                Some(&directory.join("sessions")),
                NewSessionOptions::default(),
            )
            .unwrap();
            std::fs::write(
                directory.join("session-path"),
                manager.session_file().unwrap().to_string_lossy().as_bytes(),
            )
            .unwrap();
            let session = AgentSession::create(options(&h, manager, Settings::default()))
                .await
                .unwrap();
            let side_effect = directory.join("effect");
            let entered = Arc::new(tokio::sync::Notify::new());
            let produced = Arc::clone(&entered);
            session
                .jobs()
                .start(
                    JobSpec {
                        kind: "bash",
                        label: "uncertain fixture effect".into(),
                        output: OutputMode::Stream,
                    },
                    move |context| async move {
                        std::fs::write(side_effect, "effect once\n").unwrap();
                        produced.notify_one();
                        context.signal().cancelled().await;
                        JobOutcome::killed("stopped")
                    },
                )
                .unwrap();
            entered.notified().await;
            let output = directory.clone();
            let _subscription = session.subscribe(move |event| {
                if let SessionEvent::RecoverableRunStarted { run_id } = event {
                    std::fs::write(output.join("run-id"), run_id).unwrap();
                }
            });
            session.core.hooks().after_run_job_wait = Some(Box::new(|| std::process::exit(78)));
            session
                .prompt_recoverable(
                    "work",
                    Vec::new(),
                    RunLimits {
                        max_model_calls: Some(4),
                        deadline: None,
                    },
                )
                .await
                .unwrap();
            panic!("child missed its durable wait boundary");
        });
}

#[tokio::test]
async fn crash_after_job_wait_start_refuses_a_refunded_deadline_and_never_replays_the_job() {
    let dir = TempDir::new("journal-wait-crash");
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "agent_session::recovery_tests::run_journal_tests::jobs::job_wait_crash_child",
            "--nocapture",
        ])
        .env(JOB_CHILD_DIR, dir.path())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let status = match tokio::time::timeout(Duration::from_secs(20), child.wait()).await {
        Ok(status) => status.unwrap(),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            panic!("job-wait child did not exit at its boundary");
        }
    };
    assert_eq!(status.code(), Some(78));
    let path = std::fs::read_to_string(dir.join("session-path")).unwrap();
    let run_id = std::fs::read_to_string(dir.join("run-id")).unwrap();
    let h = harness(200_000);
    let session = AgentSession::create(options(
        &h,
        SessionManager::open(std::path::Path::new(&path), None, None).unwrap(),
        Settings::default(),
    ))
    .await
    .unwrap();
    let result = session.resume_run(&run_id).await;
    let pending = session.pending_notices.lock().unwrap().clone();
    session.shutdown().await;
    assert!(result.unwrap_err().contains("job-wait deadline"));
    assert_eq!(h.faux.call_count(), 0);
    assert_eq!(
        std::fs::read_to_string(dir.join("effect")).unwrap(),
        "effect once\n"
    );
    assert!(pending.iter().any(|notice| {
        notice
            .details
            .as_ref()
            .is_some_and(|details| details["status"] == "lost")
    }));
}
