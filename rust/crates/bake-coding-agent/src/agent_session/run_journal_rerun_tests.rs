//! Resuming a selected run reruns its unresolved calls only under Pi
//! Durable's replay rule (v1.3.0 `packages/durable/src/harness/tool.ts`,
//! `execute`): the class recorded with a call's dispatch and the class of
//! the tool registered now are both `Safe`. Each case writes a real session
//! file at the interruption, resumes it, and checks the effect counters, the
//! next request, and the file.

use std::path::Path;

use super::*;
use bake_agent::ToolReplay;

/// A tool of class `replay` that counts its executions.
fn counted(name: &str, replay: Option<ToolReplay>, count: &Arc<AtomicUsize>) -> Arc<AgentTool> {
    let count = Arc::clone(count);
    let label = name.to_owned();
    let mut tool = AgentTool::new(
        name,
        name,
        "Count executions",
        json!({"type":"object"}),
        move |_, _, _, _, _ctx| {
            let ran = count.fetch_add(1, Ordering::SeqCst) + 1;
            let text = format!("{label} ran {ran}");
            Box::pin(async move { Ok(AgentToolResult::text(text)) })
        },
    );
    tool.replay = replay;
    Arc::new(tool)
}

/// One call of the interrupted assistant: its tool, its id, and its
/// dispatch: `None` when never dispatched, else the recorded class.
type Call<'a> = (&'a str, &'a str, Option<Option<&'a str>>);

/// A run interrupted after its acknowledged assistant asked for `calls`.
fn interrupted(manager: &mut SessionManager, max_calls: u64, calls: &[Call<'_>]) -> String {
    checkpoint_input(manager, "selected", max_calls, false);
    let assistant = manager
        .append_message(faux_assistant_blocks(
            calls
                .iter()
                .map(|(tool, id, _)| faux_tool_call(tool, json!({}), Some(id)))
                .collect(),
            StopReason::ToolUse,
        ))
        .unwrap();
    append_fact(
        manager,
        "selected",
        json!({"kind":"message","entry_id":assistant}),
    );
    for (tool, id, dispatch) in calls {
        if let Some(replay) = dispatch {
            dispatched(manager, &assistant, tool, id, *replay);
        }
    }
    assistant
}

fn dispatched(
    manager: &mut SessionManager,
    assistant: &str,
    tool: &str,
    id: &str,
    replay: Option<&str>,
) {
    let mut fact = json!({"kind":"dispatch","assistant_entry_id":assistant,"tool_call_id":id,"tool_name":tool});
    if let Some(replay) = replay {
        fact["replay"] = json!(replay);
    }
    append_fact(manager, "selected", fact);
}

/// A durable, acknowledged result for call `id`.
fn completed(manager: &mut SessionManager, tool: &str, id: &str) {
    let result = manager
        .append_message(bake_ai::ToolResultMessage {
            tool_call_id: id.into(),
            tool_name: tool.into(),
            content: vec![UserContentBlock::Text(TextContent::new("completed effect"))],
            is_error: false,
            timestamp: 2,
            ..Default::default()
        })
        .unwrap();
    append_fact(
        manager,
        "selected",
        json!({"kind":"message","entry_id":result}),
    );
}

fn session_with(
    h: &Harness,
    manager: SessionManager,
    tools: Vec<Arc<AgentTool>>,
    active: &[&str],
) -> AgentSessionOptions {
    AgentSessionOptions {
        tools,
        active_tool_names: active.iter().map(|name| (*name).to_owned()).collect(),
        ..options(h, manager, Settings::default())
    }
}

fn text(content: &[UserContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect()
}

/// The request's conversation, one line per message, system messages left out.
pub(super) fn conversation(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => Some(format!(
                "user: {}",
                bake_ai::utils::text::user_content_text(&user.content, "\n")
            )),
            Message::Assistant(assistant) => Some(format!(
                "assistant: {}",
                assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        bake_ai::AssistantContentBlock::ToolCall(call) => Some(call.id.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            )),
            Message::ToolResult(result) => Some(format!(
                "result {}{}: {}",
                result.tool_call_id,
                if result.is_error { " (error)" } else { "" },
                text(&result.content)
            )),
            _ => None,
        })
        .collect()
}

/// The session file's entries after the run's last resumption: journal
/// records as their kind, call, and class; messages as their role and call;
/// custom messages as their type.
fn after_resume(path: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    for line in std::fs::read_to_string(path).unwrap().lines() {
        let entry: Value = serde_json::from_str(line).unwrap();
        let described = match entry["type"].as_str() {
            Some("custom") if entry["customType"] == "bake.run.v1" => {
                let event = &entry["data"]["event"];
                [&event["kind"], &event["tool_call_id"], &event["replay"]]
                    .iter()
                    .filter_map(|value| value.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            }
            Some("message") => [&entry["message"]["role"], &entry["message"]["toolCallId"]]
                .iter()
                .filter_map(|value| value.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            Some("custom_message") => entry["customType"].as_str().unwrap().to_owned(),
            _ => continue,
        };
        if described == "resumed" {
            lines.clear();
        } else {
            lines.push(described);
        }
    }
    lines
}

pub(super) const NOTICE: &str = "user: Execution resumed from its durable checkpoint. Completed tool results are already recorded. Continue the task using the current tools and permission policy; do not repeat completed effects.";

#[tokio::test]
async fn safe_calls_rerun_under_the_current_policy_before_the_notice() {
    let dir = TempDir::new("run-rerun-safe");
    let mut manager = file_session(&dir);
    // One call was dispatched before the crash, the other never was.
    interrupted(
        &mut manager,
        3,
        &[
            ("probe", "first", Some(Some("safe"))),
            ("probe", "second", None),
        ],
    );
    let h = harness(200_000);
    let seen = Arc::new(Mutex::new(Vec::new()));
    h.faux
        .set_responses(vec![capture(&seen, faux_assistant_message("done"))]);
    let probes = Arc::new(AtomicUsize::new(0));
    let session = AgentSession::create(session_with(
        &h,
        manager,
        vec![counted("probe", Some(ToolReplay::Safe), &probes)],
        &["probe"],
    ))
    .await
    .unwrap();
    let (events, _subscription) = record(&session);
    let before = session.inspect_run("selected").unwrap();
    let result = session.resume_run("selected").await.unwrap();
    let after = session.inspect_run("selected").unwrap();
    let path = session.session_file().unwrap();
    session.shutdown().await;

    assert_eq!(before.disposition, RunRecoveryDisposition::Resumable);
    assert_eq!(result.outcome, RunOutcome::FinishedTurn);
    assert_eq!(
        after.disposition,
        RunRecoveryDisposition::Completed {
            outcome: RunOutcome::FinishedTurn
        }
    );
    assert_eq!(after.model_calls, 2);
    assert_eq!(probes.load(Ordering::SeqCst), 2);
    assert_eq!(h.faux.call_count(), 1);
    // The results answer the calls they follow, then the notice.
    assert_eq!(
        conversation(seen.lock().unwrap()[0].messages()),
        [
            "user: continue the task",
            "assistant: first,second",
            "result first: probe ran 1",
            "result second: probe ran 2",
            NOTICE,
        ]
    );
    let disk = after_resume(&path);
    assert_eq!(
        disk[..7],
        [
            "dispatch first safe",
            "toolResult first",
            "message",
            "dispatch second safe",
            "toolResult second",
            "message",
            "bake.run.recovery.v1",
        ],
        "{disk:?}"
    );
    assert_eq!(disk.last().map(String::as_str), Some("terminal"));
    assert_eq!(
        of_type(&events, &["tool_execution_start", "tool_execution_end"])
            .iter()
            .map(|event| format!(
                "{} {}",
                event["type"].as_str().unwrap(),
                event["toolCallId"].as_str().unwrap()
            ))
            .collect::<Vec<_>>(),
        [
            "tool_execution_start first",
            "tool_execution_end first",
            "tool_execution_start second",
            "tool_execution_end second",
        ]
    );
}

#[tokio::test]
async fn a_rerun_the_current_policy_denies_is_not_run() {
    for case in ["veto", "inactive", "never-now", "unclassified-now"] {
        let dir = TempDir::new(case);
        let mut manager = file_session(&dir);
        interrupted(&mut manager, 3, &[("probe", "first", Some(Some("safe")))]);
        let h = harness(200_000);
        let seen = Arc::new(Mutex::new(Vec::new()));
        h.faux
            .set_responses(vec![capture(&seen, faux_assistant_message("done"))]);
        let probes = Arc::new(AtomicUsize::new(0));
        let replay = match case {
            "never-now" => Some(ToolReplay::Never),
            "unclassified-now" => None,
            _ => Some(ToolReplay::Safe),
        };
        let active: &[&str] = if case == "inactive" { &[] } else { &["probe"] };
        let vetoes = Arc::new(crate::plugins::tests::Scripted::new(
            "deny",
            &["probe"],
            crate::plugins::tests::block("probe is denied now"),
        ));
        let mut plugins = crate::plugins::PluginRegistry::new();
        if case == "veto" {
            plugins.register(vetoes.clone(), 0).unwrap();
        }
        let session = AgentSession::create(AgentSessionOptions {
            plugins,
            ..session_with(&h, manager, vec![counted("probe", replay, &probes)], active)
        })
        .await
        .unwrap();
        let before = session.inspect_run("selected").unwrap();
        let result = session.resume_run("selected").await;
        let path = session.session_file().unwrap();
        session.shutdown().await;

        assert_eq!(probes.load(Ordering::SeqCst), 0, "{case}");
        if case == "veto" {
            // The current veto answers the call, as it would a new one; the
            // model sees the denial after the call, then the notice.
            assert_eq!(before.disposition, RunRecoveryDisposition::Resumable);
            assert_eq!(result.unwrap().outcome, RunOutcome::FinishedTurn);
            assert_eq!(vetoes.calls(), ["probe:first"]);
            let request = conversation(seen.lock().unwrap()[0].messages());
            assert!(
                request[2].starts_with("result first (error): "),
                "{request:?}"
            );
            assert!(request[2].contains("probe is denied now"), "{request:?}");
            assert_eq!(request[3], NOTICE);
            let disk = after_resume(&path);
            assert_eq!(
                disk[..3],
                ["toolResult first", "message", "bake.run.recovery.v1"]
            );
        } else {
            assert!(
                matches!(
                    before.disposition,
                    RunRecoveryDisposition::NeedsReconciliation { .. }
                ),
                "{case}: {before:?}"
            );
            assert!(result.unwrap_err().contains("external effect"), "{case}");
            assert_eq!(h.faux.call_count(), 0, "{case}");
            assert!(
                !std::fs::read_to_string(&path)
                    .unwrap()
                    .contains("\"kind\":\"resumed\"")
            );
        }
    }
}

#[tokio::test]
async fn a_call_that_is_not_safe_needs_reconciliation_and_never_repeats() {
    // (case, max model calls, calls, a user turn after the assistant)
    let cases: [(&str, u64, &[Call<'_>], bool); 7] = [
        (
            "never-dispatched",
            3,
            &[("effect", "first", Some(None))],
            false,
        ),
        ("never-undispatched", 3, &[("effect", "first", None)], false),
        (
            "recorded-unsafe",
            3,
            &[("probe", "first", Some(None))],
            false,
        ),
        (
            "recorded-never",
            3,
            &[("probe", "first", Some(Some("never")))],
            false,
        ),
        (
            "mixed",
            3,
            &[
                ("probe", "first", Some(Some("safe"))),
                ("effect", "second", None),
            ],
            false,
        ),
        (
            "budget-spent",
            1,
            &[("probe", "first", Some(Some("safe")))],
            false,
        ),
        (
            "not-last",
            3,
            &[("probe", "first", Some(Some("safe")))],
            true,
        ),
    ];
    for (case, max_calls, calls, later_user) in cases {
        let dir = TempDir::new(case);
        let mut manager = file_session(&dir);
        interrupted(&mut manager, max_calls, calls);
        if later_user {
            let user = manager
                .append_message(UserMessage {
                    content: UserContent::Text("later".into()),
                    timestamp: 3,
                })
                .unwrap();
            append_fact(
                &mut manager,
                "selected",
                json!({"kind":"message","entry_id":user}),
            );
        }
        let h = harness(200_000);
        let (probes, effects) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let session = AgentSession::create(session_with(
            &h,
            manager,
            vec![
                counted("probe", Some(ToolReplay::Safe), &probes),
                counted("effect", Some(ToolReplay::Never), &effects),
            ],
            &["probe", "effect"],
        ))
        .await
        .unwrap();
        let before = session.inspect_run("selected").unwrap();
        let first = session.resume_run("selected").await;
        let second = session.resume_run("selected").await;
        let after = session.inspect_run("selected").unwrap();
        session.shutdown().await;

        assert!(
            matches!(
                before.disposition,
                RunRecoveryDisposition::NeedsReconciliation { .. }
            ),
            "{case}: {before:?}"
        );
        assert_eq!(after, before, "{case}");
        assert!(
            first.unwrap_err().contains("needs reconciliation"),
            "{case}"
        );
        assert!(
            second.unwrap_err().contains("needs reconciliation"),
            "{case}"
        );
        assert_eq!(h.faux.call_count(), 0, "{case}");
        assert_eq!(probes.load(Ordering::SeqCst), 0, "{case}");
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{case}");
    }
}

#[tokio::test]
async fn ordinary_recovery_answers_no_result_provided_and_reruns_nothing() {
    let dir = TempDir::new("run-ordinary-recovery");
    let mut manager = file_session(&dir);
    interrupted(&mut manager, 3, &[("probe", "first", Some(Some("safe")))]);
    let h = harness(200_000);
    let seen = Arc::new(Mutex::new(Vec::new()));
    h.faux
        .set_responses(vec![capture(&seen, faux_assistant_message("done"))]);
    let probes = Arc::new(AtomicUsize::new(0));
    let session = AgentSession::create(session_with(
        &h,
        manager,
        vec![counted("probe", Some(ToolReplay::Safe), &probes)],
        &["probe"],
    ))
    .await
    .unwrap();
    // An ordinary prompt is not a selected-run recovery.
    session.prompt("go on", Vec::new()).await.unwrap();
    let after = session.inspect_run("selected").unwrap();
    session.shutdown().await;

    assert_eq!(probes.load(Ordering::SeqCst), 0);
    assert_eq!(h.faux.call_count(), 1);
    let request = seen.lock().unwrap()[0].clone();
    // What every provider adapter sends for the unanswered call.
    let sent = bake_ai::api::transform_messages::transform_messages(
        request.messages(),
        h.faux.model(),
        None,
    );
    assert_eq!(
        conversation(&sent),
        [
            "user: continue the task",
            "assistant: first",
            "result first (error): No result provided",
            "user: go on",
        ]
    );
    assert!(matches!(
        after.disposition,
        RunRecoveryDisposition::Refused { .. }
    ));
}

#[tokio::test]
async fn completed_calls_on_the_same_assistant_do_not_block_a_rerun() {
    let dir = TempDir::new("run-rerun-mixed");
    let mut manager = file_session(&dir);
    let assistant = interrupted(
        &mut manager,
        3,
        &[("effect", "done", Some(None)), ("probe", "pending", None)],
    );
    completed(&mut manager, "effect", "done");
    dispatched(&mut manager, &assistant, "probe", "pending", Some("safe"));
    let h = harness(200_000);
    let seen = Arc::new(Mutex::new(Vec::new()));
    h.faux
        .set_responses(vec![capture(&seen, faux_assistant_message("done"))]);
    let (probes, effects) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let session = AgentSession::create(session_with(
        &h,
        manager,
        vec![
            counted("probe", Some(ToolReplay::Safe), &probes),
            counted("effect", Some(ToolReplay::Never), &effects),
        ],
        &["probe", "effect"],
    ))
    .await
    .unwrap();
    let result = session.resume_run("selected").await;
    session.shutdown().await;

    assert_eq!(result.unwrap().outcome, RunOutcome::FinishedTurn);
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(probes.load(Ordering::SeqCst), 1);
    assert_eq!(
        conversation(seen.lock().unwrap()[0].messages())[1..],
        [
            "assistant: done,pending",
            "result done: completed effect",
            "result pending: probe ran 1",
            NOTICE,
        ]
    );
}

#[tokio::test]
async fn only_a_later_resumption_dispatches_a_safe_call_again() {
    // (case, facts after the first dispatch, resumable)
    for (case, again) in [
        ("interrupted-rerun", true),
        ("no-resumption", false),
        ("recorded-unsafe", false),
        ("after-its-result", false),
    ] {
        let dir = TempDir::new(case);
        let mut manager = file_session(&dir);
        let first = if case == "recorded-unsafe" {
            None
        } else {
            Some("safe")
        };
        let assistant = interrupted(&mut manager, 3, &[("probe", "first", Some(first))]);
        if case == "after-its-result" {
            completed(&mut manager, "probe", "first");
        }
        if case != "no-resumption" {
            append_fact(
                &mut manager,
                "selected",
                json!({"kind":"resumed","model_calls":1}),
            );
        }
        dispatched(&mut manager, &assistant, "probe", "first", Some("safe"));
        let h = harness(200_000);
        h.faux
            .set_responses(vec![faux_assistant_message("done").into()]);
        let probes = Arc::new(AtomicUsize::new(0));
        let session = AgentSession::create(session_with(
            &h,
            manager,
            vec![counted("probe", Some(ToolReplay::Safe), &probes)],
            &["probe"],
        ))
        .await
        .unwrap();
        let inspected = session.inspect_run("selected");
        let result = session.resume_run("selected").await;
        session.shutdown().await;
        if again {
            assert_eq!(
                inspected.unwrap().disposition,
                RunRecoveryDisposition::Resumable
            );
            assert_eq!(result.unwrap().outcome, RunOutcome::FinishedTurn);
            assert_eq!(probes.load(Ordering::SeqCst), 1);
        } else {
            let error = inspected.unwrap_err();
            assert!(
                error.contains("duplicate tool identity")
                    || error.contains("follows its tool result"),
                "{case}: {error}"
            );
            assert!(result.is_err(), "{case}");
            assert_eq!(probes.load(Ordering::SeqCst), 0, "{case}");
            assert_eq!(h.faux.call_count(), 0, "{case}");
        }
    }
}

#[tokio::test]
async fn a_dispatch_records_only_a_safe_class_and_reads_no_other() {
    let dir = TempDir::new("run-dispatch-class");
    let h = harness(200_000);
    h.faux.set_responses(vec![
        faux_assistant_blocks(
            vec![
                faux_tool_call("effect", json!({}), Some("a")),
                faux_tool_call("probe", json!({}), Some("b")),
                faux_tool_call("plain", json!({}), Some("c")),
            ],
            StopReason::ToolUse,
        )
        .into(),
        faux_assistant_message("done").into(),
    ]);
    let counts: [Arc<AtomicUsize>; 3] = Default::default();
    let session = AgentSession::create(session_with(
        &h,
        file_session(&dir),
        vec![
            counted("effect", Some(ToolReplay::Never), &counts[0]),
            counted("probe", Some(ToolReplay::Safe), &counts[1]),
            counted("plain", None, &counts[2]),
        ],
        &["effect", "probe", "plain"],
    ))
    .await
    .unwrap();
    let result = session
        .prompt_recoverable("go", Vec::new(), RunLimits::default())
        .await
        .unwrap();
    let path = session.session_file().unwrap();
    session.shutdown().await;
    assert_eq!(result.outcome, RunOutcome::FinishedTurn);
    let mut dispatches: Vec<String> = after_resume(&path)
        .into_iter()
        .filter(|line| line.starts_with("dispatch"))
        .collect();
    dispatches.sort();
    assert_eq!(dispatches, ["dispatch a", "dispatch b safe", "dispatch c"]);

    // A class other than the two Bake writes is not evidence of either.
    for class in [json!("unsafe"), json!("SAFE"), json!(true), Value::Null] {
        let dir = TempDir::new("run-dispatch-class-read");
        let mut manager = file_session(&dir);
        let assistant = interrupted(&mut manager, 3, &[("probe", "first", None)]);
        append_fact(
            &mut manager,
            "selected",
            json!({"kind":"dispatch","assistant_entry_id":assistant,"tool_call_id":"first","tool_name":"probe","replay":class}),
        );
        let h = harness(200_000);
        let probes = Arc::new(AtomicUsize::new(0));
        let session = AgentSession::create(session_with(
            &h,
            manager,
            vec![counted("probe", Some(ToolReplay::Safe), &probes)],
            &["probe"],
        ))
        .await
        .unwrap();
        let inspected = session.inspect_run("selected");
        let resumed = session.resume_run("selected").await;
        session.shutdown().await;
        assert!(
            inspected.unwrap_err().contains("Invalid run journal entry"),
            "{class}"
        );
        assert!(resumed.is_err(), "{class}");
        assert_eq!(probes.load(Ordering::SeqCst), 0, "{class}");
        assert_eq!(h.faux.call_count(), 0, "{class}");
    }
}
