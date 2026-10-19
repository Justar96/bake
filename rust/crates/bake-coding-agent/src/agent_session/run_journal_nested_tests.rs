//! Nested calls in the run journal: each is journaled by its own id with
//! its parent's, after its parent's dispatch, and recovery is the
//! model-issued call's. The live case runs the agent's real pipeline; the
//! others write the records a crash would leave.

use super::*;
use crate::agent_session::nested_call_tests::script_tool;

/// The `bake.run.v1` events of `run` in the session file at `path`.
fn facts(path: &Path, run: &str) -> Vec<Value> {
    SessionManager::open(path, None, None)
        .unwrap()
        .branch_entries(None)
        .iter()
        .filter(|entry| entry.get("customType").and_then(Value::as_str) == Some("bake.run.v1"))
        .filter_map(|entry| entry.get("data").cloned())
        .filter(|data| data["runId"] == run)
        .map(|data| data["event"].clone())
        .collect()
}

fn counted(name: &str, count: &Arc<AtomicUsize>) -> Arc<AgentTool> {
    let count = Arc::clone(count);
    Arc::new(AgentTool::new(
        name,
        name,
        "Count executions",
        json!({"type":"object"}),
        move |_, _, _, _, _ctx| {
            count.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(AgentToolResult::text("ran")) })
        },
    ))
}

#[tokio::test]
async fn nested_calls_are_journaled_by_their_own_ids_after_their_parent() {
    let dir = TempDir::new("run-journal-nested");
    let h = harness(200_000);
    let count = Arc::new(AtomicUsize::new(0));
    let calls = json!([
        { "name": "effect", "args": {} },
        { "name": "script", "args": { "calls": [{ "name": "effect", "args": {} }] } },
    ]);
    h.faux.set_responses(vec![
        faux_assistant_blocks(
            vec![faux_tool_call(
                "script",
                json!({ "calls": calls }),
                Some("c1"),
            )],
            StopReason::ToolUse,
        )
        .into(),
        faux_assistant_message("done").into(),
    ]);
    let mut options = options(&h, file_session(&dir), Settings::default());
    options.tools = vec![Arc::new(script_tool()), counted("effect", &count)];
    options.active_tool_names = vec!["script".into(), "effect".into()];
    let session = AgentSession::create(options).await.unwrap();
    let result = session
        .prompt_recoverable(
            "go",
            Vec::new(),
            RunLimits {
                max_model_calls: Some(3),
                deadline: None,
            },
        )
        .await
        .unwrap();
    let state = session.inspect_run(&result.run_id).unwrap();
    let path = session.session_file().unwrap();
    session.shutdown().await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert_eq!(
        state.disposition,
        RunRecoveryDisposition::Completed {
            outcome: RunOutcome::FinishedTurn
        }
    );
    let dispatches: Vec<_> = facts(&path, &result.run_id)
        .into_iter()
        .filter(|event| event["kind"] == "dispatch")
        .map(|event| {
            (
                event["tool_call_id"].as_str().unwrap().to_owned(),
                event["parent_tool_call_id"].as_str().map(str::to_owned),
            )
        })
        .collect();
    assert_eq!(
        dispatches,
        [
            ("c1".to_owned(), None),
            ("c1/1".to_owned(), Some("c1".to_owned())),
            ("c1/2".to_owned(), Some("c1".to_owned())),
            ("c1/2/1".to_owned(), Some("c1/2".to_owned())),
        ]
    );
}

/// A run interrupted while its call `p` ran: `p` and the nested calls in
/// `nested` (id and parent) were dispatched, and no result was written.
fn interrupted_in_nested_call(manager: &mut SessionManager, nested: &[(&str, &str)]) -> String {
    interrupted_in_nested_call_of(manager, None, nested)
}

/// [`interrupted_in_nested_call`] with `p` dispatched as `replay`.
fn interrupted_in_nested_call_of(
    manager: &mut SessionManager,
    replay: Option<&str>,
    nested: &[(&str, &str)],
) -> String {
    checkpoint_input(manager, "selected", 3, false);
    let assistant = manager
        .append_message(faux_assistant_blocks(
            vec![faux_tool_call("script", json!({}), Some("p"))],
            StopReason::ToolUse,
        ))
        .unwrap();
    append_fact(
        manager,
        "selected",
        json!({"kind":"message","entry_id":assistant}),
    );
    let mut dispatch = json!({"kind":"dispatch","assistant_entry_id":assistant,"tool_call_id":"p","tool_name":"script"});
    if let Some(replay) = replay {
        dispatch["replay"] = json!(replay);
    }
    append_fact(manager, "selected", dispatch);
    for (id, parent) in nested {
        nested_dispatch(manager, &assistant, id, parent);
    }
    assistant
}

fn nested_dispatch(manager: &mut SessionManager, assistant: &str, id: &str, parent: &str) {
    append_fact(
        manager,
        "selected",
        json!({"kind":"dispatch","assistant_entry_id":assistant,"tool_call_id":id,"tool_name":"effect","replay":"safe","parent_tool_call_id":parent}),
    );
}

#[tokio::test]
async fn a_crash_inside_a_nested_call_leaves_its_parent_interrupted() {
    let dir = TempDir::new("run-journal-nested-crash");
    let mut manager = file_session(&dir);
    interrupted_in_nested_call(&mut manager, &[("p/1", "p"), ("p/1/1", "p/1")]);
    let h = harness(200_000);
    let count = Arc::new(AtomicUsize::new(0));
    let mut options = options(&h, manager, Settings::default());
    // Even a `Safe` nested tool is not rerun: the parent decides.
    let mut effect = (*counted("effect", &count)).clone();
    effect.replay = Some(bake_agent::ToolReplay::Safe);
    options.tools = vec![Arc::new(script_tool()), Arc::new(effect)];
    options.active_tool_names = vec!["script".into(), "effect".into()];
    let session = AgentSession::create(options).await.unwrap();
    let state = session.inspect_run("selected").unwrap();
    let resumed = session.resume_run("selected").await;
    session.shutdown().await;
    assert_eq!(
        state.disposition,
        RunRecoveryDisposition::NeedsReconciliation {
            reason: "a recorded tool dispatch has no durable result; its external effect may already have occurred".into()
        }
    );
    assert!(resumed.unwrap_err().contains("needs reconciliation"));
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(h.faux.call_count(), 0);
}

#[tokio::test]
async fn a_nested_dispatch_without_its_parent_is_refused() {
    for nested in [
        // The parent was never dispatched.
        &[("q/1", "q")][..],
        // A repeated identity.
        &[("p/1", "p"), ("p/1", "p")][..],
    ] {
        let dir = TempDir::new("run-journal-nested-orphan");
        let mut manager = file_session(&dir);
        interrupted_in_nested_call(&mut manager, nested);
        let h = harness(200_000);
        let session = AgentSession::create(options(&h, manager, Settings::default()))
            .await
            .unwrap();
        let inspected = session.inspect_run("selected").unwrap_err();
        session.shutdown().await;
        assert!(
            inspected.contains("nested dispatch has no earlier parent dispatch"),
            "{inspected}"
        );
    }
}

/// A `Safe` call, recorded `Safe`, would rerun on resume; one that made a
/// nested call is not, since a rerun has no agent to serve its nested
/// calls: the run needs reconciliation and nothing runs.
#[tokio::test]
async fn a_safe_call_with_a_nested_dispatch_is_not_rerun() {
    let dir = TempDir::new("run-journal-nested-safe");
    let mut manager = file_session(&dir);
    interrupted_in_nested_call_of(&mut manager, Some("safe"), &[("p/1", "p")]);
    let h = harness(200_000);
    let script_runs = Arc::new(AtomicUsize::new(0));
    let effect_runs = Arc::new(AtomicUsize::new(0));
    let mut script = (*counted("script", &script_runs)).clone();
    script.replay = Some(bake_agent::ToolReplay::Safe);
    let mut options = options(&h, manager, Settings::default());
    options.tools = vec![Arc::new(script), counted("effect", &effect_runs)];
    options.active_tool_names = vec!["script".into(), "effect".into()];
    let session = AgentSession::create(options).await.unwrap();
    let state = session.inspect_run("selected").unwrap();
    let resumed = session.resume_run("selected").await;
    session.shutdown().await;
    assert_eq!(
        state.disposition,
        RunRecoveryDisposition::NeedsReconciliation {
            reason: "a recorded tool dispatch has no durable result; its external effect may already have occurred".into()
        }
    );
    assert!(resumed.unwrap_err().contains("needs reconciliation"));
    assert_eq!(script_runs.load(Ordering::SeqCst), 0);
    assert_eq!(effect_runs.load(Ordering::SeqCst), 0);
    assert_eq!(h.faux.call_count(), 0);
}

/// A nested call's dispatch is written before it runs, so before its
/// model-issued call's result; one recorded after that result is refused.
#[tokio::test]
async fn a_nested_dispatch_after_its_parents_result_is_refused() {
    let dir = TempDir::new("run-journal-nested-late");
    let mut manager = file_session(&dir);
    let assistant = interrupted_in_nested_call(&mut manager, &[("p/1", "p")]);
    let result = manager
        .append_message(bake_ai::ToolResultMessage {
            tool_call_id: "p".into(),
            tool_name: "script".into(),
            content: vec![UserContentBlock::Text(TextContent::new("done"))],
            timestamp: 2,
            ..Default::default()
        })
        .unwrap();
    append_fact(
        &mut manager,
        "selected",
        json!({"kind":"message","entry_id":result}),
    );
    nested_dispatch(&mut manager, &assistant, "p/2", "p");
    let h = harness(200_000);
    let session = AgentSession::create(options(&h, manager, Settings::default()))
        .await
        .unwrap();
    let inspected = session.inspect_run("selected").unwrap_err();
    session.shutdown().await;
    assert!(
        inspected.contains("nested dispatch follows its model-issued call's result"),
        "{inspected}"
    );
}
