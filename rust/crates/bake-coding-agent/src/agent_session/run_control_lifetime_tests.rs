//! The caller may drop its waiter while Agent still owns the execution.
//! Its shared allowance must remain available to next-turn summaries.

use super::*;

const PATIENCE: Duration = Duration::from_secs(10);

#[tokio::test]
async fn dropping_the_prompt_waiter_keeps_summary_admission_on_the_same_control() {
    let h = harness(200_000);
    h.faux.set_responses(vec![
        faux_assistant_blocks(
            vec![faux_tool_call("held", json!({}), Some("call_1"))],
            StopReason::ToolUse,
        )
        .into(),
        faux_assistant_message("an uncounted summary must not run").into(),
    ]);
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (reached, held) = (Arc::clone(&entered), Arc::clone(&release));
    let tool = AgentTool::new(
        "held",
        "held",
        "Held tool",
        json!({"type":"object"}),
        move |_, _, _, _, _ctx| {
            let (reached, held) = (Arc::clone(&reached), Arc::clone(&held));
            Box::pin(async move {
                reached.notify_one();
                held.notified().await;
                Ok(AgentToolResult::text("tool output"))
            })
        },
    );
    let session = Arc::new(
        AgentSession::create(AgentSessionOptions {
            tools: vec![Arc::new(tool)],
            active_tool_names: vec!["held".into()],
            ..options(
                &h,
                in_memory(),
                settings(json!({
                    "compaction": { "reserveTokens": 199_990, "keepRecentTokens": 1 }
                })),
            )
        })
        .await
        .unwrap(),
    );
    let control = RunControl::new(bake_agent::RunLimits {
        max_model_calls: Some(1),
        ..Default::default()
    });
    let owner = Arc::clone(&session);
    let shared = control.clone();
    let caller = tokio::spawn(async move {
        owner
            .prompt_with_control("run the held tool", Vec::new(), shared)
            .await
    });
    tokio::time::timeout(PATIENCE, entered.notified())
        .await
        .expect("tool entered");
    caller.abort();
    let _ = caller.await;
    let run_still_owned = session.is_streaming();
    let control_still_owned = session.core.control().is_some();
    release.notify_one();
    tokio::time::timeout(PATIENCE, session.agent.wait_for_idle())
        .await
        .expect("agent settled");
    tokio::time::timeout(PATIENCE, session.core.wait_for_idle())
        .await
        .expect("session settled");
    let calls = h.faux.call_count();
    let compacted = entry_types(&session).contains(&"compaction".into());
    let control_released = session.core.control().is_none();
    session.shutdown().await;
    assert_eq!(
        calls, 1,
        "dropping the caller admitted an uncounted summary"
    );
    assert_eq!(control.model_calls(), 1);
    assert!(
        run_still_owned,
        "Agent remained active after its session claim was released"
    );
    assert!(
        control_still_owned,
        "a live Agent lost its shared allowance"
    );
    assert!(
        control_released,
        "an idle Agent kept its completed run claim"
    );
    assert!(!compacted, "an unadmitted summary changed the context");
}

#[tokio::test]
async fn shutdown_does_not_wait_for_a_parked_prompt_waiter() {
    let h = harness(200_000);
    let entered = Arc::new(tokio::sync::Notify::new());
    let reached = Arc::clone(&entered);
    h.faux
        .set_responses(vec![FauxResponseStep::Factory(Arc::new(
            move |_, options, _, _| {
                let reached = Arc::clone(&reached);
                let signal = options.base.signal.clone().expect("owned request signal");
                Box::pin(async move {
                    reached.notify_one();
                    signal.cancelled().await;
                    Err("request cancelled".into())
                })
            },
        ))]);
    let session = AgentSession::create(options(&h, in_memory(), Settings::default()))
        .await
        .unwrap();
    let mut caller = Box::pin(session.prompt_with_control("go", Vec::new(), RunControl::default()));
    tokio::select! {
        result = &mut caller => panic!("request ended before shutdown: {result:?}"),
        () = entered.notified() => {}
    }
    // The caller future stays alive but is not polled during shutdown.
    // A run guard must not hold a gate permit for this entire lifetime.
    let shutdown = tokio::time::timeout(PATIENCE, session.shutdown()).await;
    drop(caller);
    session.shutdown().await;
    assert!(
        shutdown.is_ok(),
        "shutdown depended on polling the caller again"
    );
}

#[tokio::test]
async fn abort_settles_a_parked_prompt_and_its_late_waiter_cannot_clear_the_successor() {
    let h = harness(200_000);
    let entered = Arc::new(tokio::sync::Notify::new());
    let successor_entered = Arc::new(tokio::sync::Notify::new());
    let release_successor = Arc::new(tokio::sync::Notify::new());
    let reached = Arc::clone(&entered);
    let (next_reached, next_release) = (
        Arc::clone(&successor_entered),
        Arc::clone(&release_successor),
    );
    h.faux.set_responses(vec![
        FauxResponseStep::Factory(Arc::new(move |_, options, _, _| {
            let reached = Arc::clone(&reached);
            let signal = options.base.signal.clone().expect("owned request signal");
            Box::pin(async move {
                reached.notify_one();
                signal.cancelled().await;
                Err("request cancelled".into())
            })
        })),
        FauxResponseStep::Factory(Arc::new(move |_, _, _, _| {
            let (reached, release) = (Arc::clone(&next_reached), Arc::clone(&next_release));
            Box::pin(async move {
                reached.notify_one();
                release.notified().await;
                Ok(faux_assistant_message("successor finished"))
            })
        })),
    ]);
    let session = AgentSession::create(options(&h, in_memory(), Settings::default()))
        .await
        .unwrap();
    let mut caller =
        Box::pin(session.prompt_with_control("first", Vec::new(), RunControl::default()));
    tokio::select! {
        result = &mut caller => panic!("request ended before abort: {result:?}"),
        () = entered.notified() => {}
    }
    let aborted = tokio::time::timeout(PATIENCE, session.abort()).await;
    if aborted.is_err() {
        drop(caller);
        session.shutdown().await;
        panic!("abort depended on polling the original caller again");
    }
    let next_control = RunControl::default();
    let mut successor =
        Box::pin(session.prompt_with_control("second", Vec::new(), next_control.clone()));
    tokio::select! {
        result = &mut successor => panic!("successor ended early: {result:?}"),
        () = successor_entered.notified() => {}
    }
    let previous_outcome = caller.await.unwrap();
    let successor_still_active = session.is_streaming();
    let successor_control_retained = session.core.control().is_some();
    release_successor.notify_one();
    let next_outcome = successor.await.unwrap();
    session.shutdown().await;
    assert_eq!(previous_outcome, RunOutcome::Cancelled);
    assert!(
        successor_still_active,
        "the old waiter released its successor"
    );
    assert!(
        successor_control_retained,
        "the old waiter cleared the current control"
    );
    assert_eq!(next_outcome, RunOutcome::FinishedTurn);
}

#[tokio::test]
async fn dropping_the_waiter_keeps_post_agent_retry_and_settlement_owned() {
    let h = harness(200_000);
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (reached, held) = (Arc::clone(&entered), Arc::clone(&release));
    h.faux.set_responses(vec![
        FauxResponseStep::Factory(Arc::new(move |_, _, _, _| {
            let (reached, held) = (Arc::clone(&reached), Arc::clone(&held));
            Box::pin(async move {
                reached.notify_one();
                held.notified().await;
                Ok(error_response("503 Service Unavailable", 1))
            })
        })),
        faux_assistant_message("retry finished").into(),
    ]);
    let session = AgentSession::create(options(
        &h,
        in_memory(),
        settings(json!({
            "compaction": { "enabled": false },
            "retry": { "enabled": true, "maxRetries": 1, "baseDelayMs": 0 }
        })),
    ))
    .await
    .unwrap();
    let (events, _subscription) = record(&session);
    let control = RunControl::new(bake_agent::RunLimits {
        max_model_calls: Some(2),
        ..Default::default()
    });
    let mut caller = Box::pin(session.prompt_with_control("go", Vec::new(), control.clone()));
    tokio::select! {
        result = &mut caller => panic!("first request ended early: {result:?}"),
        () = entered.notified() => {}
    }
    drop(caller);
    release.notify_one();
    tokio::time::timeout(PATIENCE, session.core.wait_for_idle())
        .await
        .expect("owned retry settled");
    let final_message = session
        .messages()
        .last()
        .and_then(|message| message.as_assistant())
        .map(bake_ai::utils::text::assistant_text);
    let settlements = of_type(&events, &["agent_settled"]);
    session.shutdown().await;
    assert_eq!(h.faux.call_count(), 2);
    assert_eq!(control.model_calls(), 2);
    assert_eq!(final_message.as_deref(), Some("retry finished"));
    assert_eq!(
        settlements.len(),
        1,
        "the owned run must settle exactly once"
    );
}

#[tokio::test]
async fn abort_drains_pre_prompt_admission_while_its_caller_stays_parked() {
    let h = harness(200_000);
    let mut manager = in_memory();
    manager
        .append_message(user_message("earlier question", 1))
        .unwrap();
    manager
        .append_message(faux_response(
            "earlier answer",
            usage(190_000, 100),
            StopReason::Stop,
            2,
        ))
        .unwrap();
    let session = AgentSession::create(options(&h, manager, keep_one()))
        .await
        .unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let written = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (reached, held, acknowledged) = (
        Arc::clone(&entered),
        Arc::clone(&release),
        Arc::clone(&written),
    );
    let observer: bake_agent::AdmissionObserver = Arc::new(move |_| {
        let (reached, held, acknowledged) = (
            Arc::clone(&reached),
            Arc::clone(&held),
            Arc::clone(&acknowledged),
        );
        Box::pin(async move {
            reached.notify_one();
            held.notified().await;
            acknowledged.store(true, Ordering::SeqCst);
            Ok(())
        })
    });
    let control = RunControl::with_admission_observer(Default::default(), Some(observer));
    let mut caller = Box::pin(session.prompt_with_control("continue", Vec::new(), control.clone()));
    tokio::select! {
        result = &mut caller => panic!("summary ended early: {result:?}"),
        () = entered.notified() => {}
    }
    let cancelled = control.signal();
    let release_write = tokio::spawn(async move {
        cancelled.cancelled().await;
        release.notify_one();
    });
    let aborted = tokio::time::timeout(PATIENCE, session.abort()).await;
    if aborted.is_err() {
        drop(caller);
        session.shutdown().await;
        release_write.await.unwrap();
        panic!("abort depended on polling the summary caller again");
    }
    let outcome = caller.await.unwrap();
    release_write.await.unwrap();
    session.shutdown().await;
    assert!(
        written.load(Ordering::SeqCst),
        "abort abandoned the admission write"
    );
    assert_eq!(outcome, RunOutcome::Cancelled);
    assert_eq!(h.faux.call_count(), 0);
}

#[tokio::test]
async fn a_dropped_prompt_waiter_cannot_hide_its_required_write_failure() {
    let dir = TempDir::new("dropped-run-write-failure");
    let h = harness(200_000);
    h.faux.set_responses(vec![
        faux_assistant_blocks(
            vec![faux_tool_call("held", json!({}), Some("call_1"))],
            StopReason::ToolUse,
        )
        .into(),
    ]);
    let manager = file_session(&dir);
    let file = manager.session_file().unwrap().to_path_buf();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (reached, held) = (Arc::clone(&entered), Arc::clone(&release));
    let tool = AgentTool::new(
        "held",
        "held",
        "Held tool",
        json!({"type":"object"}),
        move |_, _, _, _, _ctx| {
            let (reached, held, file) = (Arc::clone(&reached), Arc::clone(&held), file.clone());
            Box::pin(async move {
                reached.notify_one();
                held.notified().await;
                std::fs::remove_file(&file).expect("remove the test session file");
                std::fs::create_dir(&file).expect("block the next append");
                Ok(AgentToolResult::text("the result cannot be persisted"))
            })
        },
    );
    let session = Arc::new(
        AgentSession::create(AgentSessionOptions {
            tools: vec![Arc::new(tool)],
            active_tool_names: vec!["held".into()],
            ..options(&h, manager, Settings::default())
        })
        .await
        .unwrap(),
    );
    let control = RunControl::default();
    let shared = control.clone();
    let owner = Arc::clone(&session);
    let caller =
        tokio::spawn(async move { owner.prompt_with_control("go", Vec::new(), shared).await });
    tokio::time::timeout(PATIENCE, entered.notified())
        .await
        .expect("tool entered");
    caller.abort();
    let _ = caller.await;
    release.notify_one();
    tokio::time::timeout(PATIENCE, session.agent.wait_for_idle())
        .await
        .expect("agent idle");
    tokio::time::timeout(PATIENCE, session.core.wait_for_idle())
        .await
        .expect("session idle");
    let outcome = control.stopped();
    session.shutdown().await;
    assert!(
        matches!(outcome, Some(RunOutcome::Failed { .. })),
        "required write failure became {outcome:?}"
    );
}

#[tokio::test]
async fn shutdown_drives_a_parked_summary_admission_to_its_write_acknowledgement() {
    let h = harness(200_000);
    let mut manager = in_memory();
    manager
        .append_message(UserMessage {
            content: UserContent::Text("earlier question".into()),
            timestamp: 1,
        })
        .unwrap();
    let mut earlier = faux_assistant_message("earlier answer");
    earlier.usage = usage(190_000, 100);
    manager.append_message(earlier).unwrap();
    let session = AgentSession::create(options(&h, manager, keep_one()))
        .await
        .unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let written = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (reached, held, acknowledged) = (
        Arc::clone(&entered),
        Arc::clone(&release),
        Arc::clone(&written),
    );
    let observer: bake_agent::AdmissionObserver = Arc::new(move |admission| {
        let (reached, held, acknowledged) = (
            Arc::clone(&reached),
            Arc::clone(&held),
            Arc::clone(&acknowledged),
        );
        Box::pin(async move {
            assert_eq!(admission.kind, bake_agent::ModelCallKind::Summary);
            reached.notify_one();
            held.notified().await;
            acknowledged.store(true, Ordering::SeqCst);
            Ok(())
        })
    });
    let control = RunControl::with_admission_observer(Default::default(), Some(observer));
    let mut caller = Box::pin(session.prompt_with_control("continue", Vec::new(), control.clone()));
    tokio::select! {
        result = &mut caller => panic!("summary admission ended before shutdown: {result:?}"),
        () = entered.notified() => {}
    }
    let cancellation = control.signal();
    let release_write = tokio::spawn(async move {
        cancellation.cancelled().await;
        release.notify_one();
    });
    // Keep the original summary-admission future alive and unpolled.
    let result = tokio::time::timeout(PATIENCE, session.shutdown()).await;
    drop(caller);
    session.shutdown().await;
    release_write.await.unwrap();
    assert!(
        result.is_ok(),
        "shutdown could not take over the parked admission"
    );
    assert!(
        written.load(Ordering::SeqCst),
        "shutdown abandoned the required debit write"
    );
    assert_eq!(
        h.faux.call_count(),
        0,
        "shutdown dispatched the admitted summary"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dropped_prompt_keeps_its_control_until_an_owned_context_write_finishes() {
    let h = harness(200_000);
    let mut manager = in_memory();
    manager
        .append_message(UserMessage {
            content: UserContent::Text("earlier question".into()),
            timestamp: 1,
        })
        .unwrap();
    let mut earlier = faux_assistant_message("earlier answer");
    earlier.usage = usage(190_000, 100);
    manager.append_message(earlier).unwrap();
    let session = AgentSession::create(options(&h, manager, keep_one()))
        .await
        .unwrap();
    let admission_entered = Arc::new(tokio::sync::Notify::new());
    let release_admission = Arc::new(tokio::sync::Notify::new());
    let admission_written = Arc::new(tokio::sync::Notify::new());
    let (entered, release, written) = (
        Arc::clone(&admission_entered),
        Arc::clone(&release_admission),
        Arc::clone(&admission_written),
    );
    let observer: bake_agent::AdmissionObserver = Arc::new(move |_| {
        let (entered, release, written) = (
            Arc::clone(&entered),
            Arc::clone(&release),
            Arc::clone(&written),
        );
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            written.notify_one();
            Ok(())
        })
    });
    let control = RunControl::with_admission_observer(Default::default(), Some(observer));
    let mut caller = Box::pin(session.prompt_with_control("continue", Vec::new(), control.clone()));
    tokio::select! {
        result = &mut caller => panic!("summary admission ended early: {result:?}"),
        () = admission_entered.notified() => {}
    }
    let commit_entered = Arc::new(tokio::sync::Notify::new());
    let reached = Arc::clone(&commit_entered);
    let (release_commit, held) = std::sync::mpsc::channel();
    let commit = session
        .core
        .owned_context_commit(move |core| {
            reached.notify_one();
            held.recv().expect("release the owned context write");
            *super::super::lock_gap(&core.gap) = Some("required context write failed".into());
        })
        .unwrap();
    tokio::time::timeout(PATIENCE, commit_entered.notified())
        .await
        .expect("context write entered");
    control.cancel();
    drop(caller);
    release_admission.notify_one();
    tokio::time::timeout(PATIENCE, admission_written.notified())
        .await
        .expect("cleanup drained admission");
    // On this single worker the cleanup continues from the acknowledgement
    // until it waits for the owned commit, before this task can resume.
    let control_retained = session.core.control().is_some();
    let run_retained = session.core.activity.borrow().run;
    release_commit.send(()).unwrap();
    commit.await.unwrap();
    tokio::time::timeout(PATIENCE, session.core.wait_for_idle())
        .await
        .expect("context and run settled");
    let outcome = control.stopped();
    session.shutdown().await;
    assert!(
        control_retained,
        "cleanup released the write's control early"
    );
    assert!(run_retained, "cleanup released the write's run claim early");
    assert!(
        matches!(outcome, Some(RunOutcome::Failed { .. })),
        "the owned context failure became {outcome:?}"
    );
    assert_eq!(h.faux.call_count(), 0);
}
