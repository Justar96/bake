use bake_ai::providers::faux::{faux_assistant_blocks, faux_text, faux_tool_call};
use bake_ai::{Message, TextContent, ToolResultMessage, UserMessage};
use serde_json::json;

use super::calls::{argument, display_name, fields, outcome, target};
use super::*;

#[test]
fn background_commit_reports_context_without_replacing_foreground_phase() {
    let mut projector = Projector {
        phase: Some("running".into()),
        ..Projector::default()
    };
    let result = compaction_result();
    let started = SessionEvent::BackgroundCompaction {
        status: BackgroundCompactionStatus::Started,
        source_leaf_id: "source",
        result: None,
        reason: None,
        elapsed_ms: 0,
        attempts: &[],
    };
    assert!(projector.apply(&started).is_empty());
    let committed = SessionEvent::BackgroundCompaction {
        status: BackgroundCompactionStatus::Committed,
        source_leaf_id: "source",
        result: Some(&result),
        reason: None,
        elapsed_ms: 10,
        attempts: &[],
    };
    assert_eq!(
        projector.apply(&committed),
        vec![RuntimeUpdate::Commit(vec![Row::Compacted(Compacted {
            title: "Compacted in the background".into(),
            before: 8000,
            after: Some(1000),
            notes: Vec::new(),
        })])]
    );
    assert_eq!(projector.phase.as_deref(), Some("running"));
}

fn compaction_result() -> bake_coding_agent::compaction::CompactionResult {
    bake_coding_agent::compaction::CompactionResult {
        summary: "summary".into(),
        first_kept_entry_id: "kept".into(),
        tokens_before: 8000,
        estimated_tokens_after: Some(1000),
        usage: None,
        details: None,
        path: bake_coding_agent::compaction::SummaryPath::Standard,
    }
}

// An automatic compaction takes the bar from the run and hands it back,
// its summary says what it reads, and a summary's retry reads as a wait,
// then as the work it retries, as Pi's indicators do.
#[test]
fn compaction_and_summary_retries_move_the_bar_phase() {
    let result = compaction_result();
    let mut projector = Projector::default();
    assert_eq!(
        projector.apply(&SessionEvent::CompactionStart {
            reason: CompactionReason::Overflow,
        }),
        [
            RuntimeUpdate::CompactionStarted { interrupts: true },
            RuntimeUpdate::Phase("recovering from a context overflow".into()),
        ]
    );
    assert_eq!(
        projector.apply(&SessionEvent::CompactionSummary {
            reason: CompactionReason::Overflow,
            cached: false,
            tokens_before: 182_340,
            fallback: Some("the last request overflowed the context window"),
        }),
        [RuntimeUpdate::Phase("summarizing 182.3k tokens".into())]
    );
    let scheduled = SessionEvent::SummarizationRetryScheduled {
        attempt: 1,
        max_attempts: 3,
        delay_ms: 2000,
        error_message: "overloaded".into(),
    };
    assert_eq!(
        projector.apply(&scheduled),
        [
            RuntimeUpdate::Commit(vec![Row::Error("overloaded".into())]),
            RuntimeUpdate::Phase("waiting to retry".into()),
        ]
    );
    let retried = SessionEvent::SummarizationRetryAttemptStart {
        source: SummarizationSource::Compaction(CompactionReason::Overflow),
    };
    assert_eq!(
        projector.apply(&retried),
        [RuntimeUpdate::Phase("summarizing again".into())]
    );
    assert_eq!(
        projector.apply(&SessionEvent::SummarizationRetryFinished),
        []
    );
    let fell_back = bake_coding_agent::compaction::CompactionResult {
        path: bake_coding_agent::compaction::SummaryPath::Fallback(
            "the last request overflowed the context window".into(),
        ),
        ..result.clone()
    };
    let ended = SessionEvent::CompactionEnd {
        reason: CompactionReason::Overflow,
        result: Some(&fell_back),
        aborted: false,
        will_retry: true,
        error_message: None,
    };
    assert_eq!(
        projector.apply(&ended),
        [
            RuntimeUpdate::Commit(vec![Row::Compacted(Compacted {
                title: "Compacted after a context overflow".into(),
                before: 8000,
                after: Some(1000),
                notes: vec![
                    "The cached summary was not used: the last request overflowed the context window."
                        .into()
                ],
            })]),
            RuntimeUpdate::CompactionEnded,
            RuntimeUpdate::Phase(copy::FIRST_PHASE.into()),
        ]
    );
    // A cached summary says how much of its prompt the cache served.
    let cached = bake_coding_agent::compaction::CompactionResult {
        path: bake_coding_agent::compaction::SummaryPath::Cached,
        usage: Some(bake_ai::Usage {
            input: 1_000,
            cache_read: 182_000,
            ..bake_ai::Usage::default()
        }),
        ..result.clone()
    };
    assert_eq!(
        compacted_row("Auto-compacted", &cached),
        Row::Compacted(Compacted {
            title: "Auto-compacted".into(),
            before: 8000,
            after: Some(1000),
            notes: vec!["The summary read 99% of its 183k prompt tokens from the cache.".into()],
        })
    );
    assert_eq!(
        projector.apply(&SessionEvent::CompactionSummary {
            reason: CompactionReason::Threshold,
            cached: true,
            tokens_before: 950,
            fallback: None,
        }),
        [RuntimeUpdate::Phase(
            "reading 950 tokens from the cache".into()
        )]
    );
    // A failure reports itself and leaves the bar to the run as well.
    projector.phase = Some("compacting".into());
    let failed = SessionEvent::CompactionEnd {
        reason: CompactionReason::Threshold,
        result: None,
        aborted: false,
        will_retry: false,
        error_message: Some("no summary".into()),
    };
    assert_eq!(
        projector.apply(&failed),
        [
            RuntimeUpdate::Commit(vec![Row::Error("no summary".into())]),
            RuntimeUpdate::CompactionEnded,
            RuntimeUpdate::Phase(copy::FIRST_PHASE.into()),
        ]
    );
    // `/compact` names itself when it starts, then keeps its own phase and
    // outcome.
    assert_eq!(
        projector.apply(&SessionEvent::CompactionStart {
            reason: CompactionReason::Manual,
        }),
        [RuntimeUpdate::Phase("preparing".into())]
    );
    projector.phase = Some("compacting".into());
    let manual = SessionEvent::CompactionEnd {
        reason: CompactionReason::Manual,
        result: Some(&result),
        aborted: false,
        will_retry: false,
        error_message: None,
    };
    assert_eq!(projector.apply(&manual), []);
    assert_eq!(projector.phase.as_deref(), Some("compacting"));
    // `/tree`'s branch summary retries under its own phase.
    let branch = SessionEvent::SummarizationRetryAttemptStart {
        source: SummarizationSource::BranchSummary,
    };
    assert_eq!(
        projector.apply(&branch),
        [RuntimeUpdate::summarizing_branch()]
    );
    // A command shows phases this projector never sees, so a retry's
    // phases go out even when they match the last one it sent.
    assert_eq!(
        projector.apply(&branch),
        [RuntimeUpdate::summarizing_branch()]
    );
    assert_eq!(
        projector.apply(&retried),
        [RuntimeUpdate::Phase("summarizing again".into())]
    );
    assert_eq!(
        projector.apply(&retried),
        [RuntimeUpdate::Phase("summarizing again".into())]
    );
    assert_eq!(
        projector.apply(&scheduled)[1..],
        [RuntimeUpdate::Phase("waiting to retry".into())]
    );
    assert_eq!(
        projector.apply(&scheduled)[1..],
        [RuntimeUpdate::Phase("waiting to retry".into())]
    );
    let masked = SessionEvent::ContextMasking {
        source_leaf_id: "leaf",
        masked_results: 3,
        removed_bytes: 4096,
        retained_recent_batches: 8,
    };
    assert_eq!(projector.apply(&masked), []);
}

// The status line is read again after what can change it, and not
// after every streamed token.
#[test]
fn the_status_moves_with_responses_levels_and_context_reductions() {
    let result = compaction_result();
    let assistant: AgentMessage =
        faux_assistant_blocks(vec![faux_text("hi")], StopReason::Stop).into();
    let outcome = bake_agent::RunOutcome::FinishedTurn;
    let moving = [
        SessionEvent::Agent(&AgentEvent::MessageEnd {
            message: assistant.clone(),
        }),
        SessionEvent::Agent(&AgentEvent::TurnEnd {
            message: assistant.clone(),
            tool_results: Vec::new(),
        }),
        SessionEvent::AgentEnd {
            messages: &[],
            will_retry: false,
        },
        SessionEvent::AgentSettled {
            outcome: &outcome,
            aborted: false,
        },
        SessionEvent::ThinkingLevelChanged(bake_ai::ModelThinkingLevel::High),
        SessionEvent::CompactionEnd {
            reason: CompactionReason::Threshold,
            result: Some(&result),
            aborted: false,
            will_retry: false,
            error_message: None,
        },
        SessionEvent::BackgroundCompaction {
            status: BackgroundCompactionStatus::Committed,
            source_leaf_id: "leaf",
            result: Some(&result),
            reason: None,
            elapsed_ms: 10,
            attempts: &[],
        },
        SessionEvent::ContextMasking {
            source_leaf_id: "leaf",
            masked_results: 1,
            removed_bytes: 10,
            retained_recent_batches: 8,
        },
    ];
    for event in &moving {
        assert!(moves_status(event), "{event:?}");
    }
    let still = [
        SessionEvent::Agent(&AgentEvent::MessageStart {
            message: assistant.clone(),
        }),
        SessionEvent::CompactionStart {
            reason: CompactionReason::Threshold,
        },
        SessionEvent::BackgroundCompaction {
            status: BackgroundCompactionStatus::Discarded,
            source_leaf_id: "leaf",
            result: Some(&result),
            reason: Some("navigation"),
            elapsed_ms: 10,
            attempts: &[],
        },
        SessionEvent::SummarizationRetryFinished,
        SessionEvent::QueueUpdate {
            steering: &[],
            follow_up: &[],
        },
    ];
    for event in &still {
        assert!(!moves_status(event), "{event:?}");
    }
}

fn user(text: &str) -> AgentMessage {
    Message::User(UserMessage {
        content: UserContent::Text(text.into()),
        timestamp: 0,
    })
    .into()
}

fn result(id: &str, text: &str, is_error: bool, details: Option<Value>) -> AgentMessage {
    tool_result(id, "bash", text, is_error, details)
}

fn tool_result(
    id: &str,
    tool: &str,
    text: &str,
    is_error: bool,
    details: Option<Value>,
) -> AgentMessage {
    Message::ToolResult(ToolResultMessage {
        tool_call_id: id.into(),
        tool_name: tool.into(),
        content: vec![UserContentBlock::Text(TextContent::new(text))],
        details,
        usage: None,
        nested_calls: None,
        is_error,
        timestamp: 0,
        duration_ms: None,
    })
    .into()
}

#[test]
fn replay_settles_calls_by_their_results_and_marks_unfinished_ones() {
    let assistant = faux_assistant_blocks(
        vec![
            faux_text("Looking."),
            faux_tool_call("bash", json!({"command": "ls"}), Some("a")),
            faux_tool_call("edit", json!({"path": "src/x.rs"}), Some("b")),
            faux_tool_call("read", json!({"path": "gone.txt"}), Some("c")),
        ],
        StopReason::ToolUse,
    );
    let patch = "Index: src/x.rs\n===\n--- src/x.rs\n+++ src/x.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n";
    let messages = vec![
        user("go"),
        Message::Assistant(assistant).into(),
        result("a", "one\ntwo\n", false, None),
        result("b", "Edited", false, Some(json!({"patch": patch}))),
    ];
    let rows = replay(&messages);
    assert_eq!(
        rows,
        [
            Row::User("go".into()),
            Row::Answer("Looking.".into()),
            Row::Call {
                tool: "Bash".into(),
                argument: "ls".into(),
                fields: Vec::new(),
                id: None,
                state: CallState::Done,
                summary: None,
                output: vec!["one".into(), "two".into()],
                full_output_path: None,
            },
            Row::Call {
                tool: "Edit".into(),
                argument: "src/x.rs".into(),
                fields: Vec::new(),
                id: None,
                state: CallState::Done,
                summary: None,
                output: vec!["@@ -1,1 +1,1 @@".into(), "-old".into(), "+new".into()],
                full_output_path: None,
            },
            Row::Call {
                tool: "Read".into(),
                argument: "gone.txt".into(),
                fields: Vec::new(),
                id: None,
                state: CallState::Failed,
                summary: Some(INTERRUPTED.into()),
                output: Vec::new(),
                full_output_path: None,
            },
        ]
    );
}

#[test]
fn a_failed_response_fails_its_unrun_calls_with_its_error() {
    let mut failed = faux_assistant_blocks(
        vec![faux_tool_call(
            "grep",
            json!({"pattern": "x", "path": "src"}),
            Some("a"),
        )],
        StopReason::Error,
    );
    failed.error_message = Some("rate limited".into());
    let mut projector = Projector::default();
    let updates = projector.apply(&SessionEvent::Agent(&AgentEvent::MessageEnd {
        message: Message::Assistant(failed.clone()).into(),
    }));
    let failed_call = Row::Call {
        tool: "Grep".into(),
        argument: "x in src".into(),
        fields: Vec::new(),
        id: None,
        state: CallState::Failed,
        summary: None,
        output: vec!["rate limited".into()],
        full_output_path: None,
    };
    assert_eq!(
        updates,
        [RuntimeUpdate::Live(vec![
            failed_call.clone(),
            Row::Error("rate limited".into())
        ])]
    );
    assert_eq!(projector.last_stop(), Some(StopReason::Error));
    assert_eq!(
        replay(&[Message::Assistant(failed).into()]),
        [failed_call, Row::Error("rate limited".into())]
    );
}

#[test]
fn a_user_message_marks_the_prompt_admitted_and_commits_once() {
    let mut projector = Projector::default();
    projector.begin(Some(7));
    assert!(!projector.admitted());
    let updates = projector.apply(&SessionEvent::Agent(&AgentEvent::MessageStart {
        message: user("hello"),
    }));
    assert_eq!(
        updates,
        [
            RuntimeUpdate::Admitted(7),
            RuntimeUpdate::Commit(vec![Row::User("hello".into())])
        ]
    );
    assert!(projector.admitted());
    // A steer's message later in the prompt is not the prompt's admission.
    let updates = projector.apply(&SessionEvent::Agent(&AgentEvent::MessageStart {
        message: user("steer"),
    }));
    assert_eq!(
        updates,
        [RuntimeUpdate::Commit(vec![Row::User("steer".into())])]
    );
    // A turn's end commits what was live, failing a call left running.
    projector.apply(&SessionEvent::Agent(&AgentEvent::ToolExecutionStart {
        tool_call_id: "x".into(),
        tool_name: "bash".into(),
        args: json!({"command": "sleep 9"}),
        parent_tool_call_id: None,
    }));
    let updates = projector.finish();
    assert_eq!(
        updates,
        [RuntimeUpdate::Commit(vec![Row::Call {
            tool: "Bash".into(),
            argument: "sleep 9".into(),
            fields: Vec::new(),
            id: None,
            state: CallState::Failed,
            summary: Some(INTERRUPTED.into()),
            output: Vec::new(),
            full_output_path: None,
        }])]
    );
    assert_eq!(projector.finish(), []);
}

#[test]
fn nested_call_events_change_nothing() {
    let mut projector = Projector::default();
    projector.begin(None);
    let nested = |event: AgentEvent| match event {
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
            ..
        } => AgentEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
            parent_tool_call_id: Some("c".into()),
        },
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            tool_name,
            result,
            is_error,
            duration_ms,
            ..
        } => AgentEvent::ToolExecutionEnd {
            tool_call_id,
            tool_name,
            result,
            is_error,
            duration_ms,
            parent_tool_call_id: Some("c".into()),
        },
        other => other,
    };
    let start = nested(start("c/1", "bash", json!({"command": "ls"})));
    assert_eq!(projector.apply(&SessionEvent::Agent(&start)), []);
    let end = nested(end("c/1", "bash", "ok", None));
    assert_eq!(projector.apply(&SessionEvent::Agent(&end)), []);
    assert_eq!(projector.finish(), []);
}

#[test]
fn pending_lists_steering_first_then_the_next_turn() {
    assert_eq!(
        pending(&["s".into()], &["f".into()], &["w".into()]),
        [
            Pending {
                text: "s".into(),
                target: Target::NextStep
            },
            Pending {
                text: "f".into(),
                target: Target::NextTurn
            },
            Pending {
                text: "w".into(),
                target: Target::NextTurn
            },
        ]
    );
}

fn custom(message: CustomMessage) -> AgentMessage {
    AgentMessage::Custom(std::sync::Arc::new(message))
}

fn info(row: Option<Row>) -> Option<String> {
    match row? {
        Row::Info(text) => Some(text),
        _ => None,
    }
}

fn card(
    tool: &str,
    argument: &str,
    id: Option<&str>,
    state: CallState,
    summary: Option<&str>,
    output: &[&str],
) -> Row {
    Row::Call {
        tool: tool.into(),
        argument: argument.into(),
        fields: Vec::new(),
        id: id.map(Into::into),
        state,
        summary: summary.map(Into::into),
        output: output.iter().map(|line| (*line).into()).collect(),
        full_output_path: None,
    }
}

fn labels(pairs: &[(&str, &str)]) -> Labels {
    pairs
        .iter()
        .map(|(id, label)| ((*id).to_owned(), (*label).to_owned()))
        .collect()
}

/// A job notice as `completion_notice` and `recovered_completion` write
/// it, which need a registry's job id to call.
fn job_notice_message(text: &str) -> AgentMessage {
    custom(CustomMessage {
        custom_type: NOTICE_TYPE.into(),
        content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))]),
        display: true,
        details: Some(json!({ "jobId": "bash-1", "status": "completed" })),
        timestamp: 0,
    })
}

fn run(projector: &mut Projector, event: AgentEvent) -> Vec<RuntimeUpdate> {
    projector.apply(&SessionEvent::Agent(&event))
}

fn start(id: &str, tool: &str, args: Value) -> AgentEvent {
    AgentEvent::ToolExecutionStart {
        tool_call_id: id.into(),
        tool_name: tool.into(),
        args,
        parent_tool_call_id: None,
    }
}

fn end(id: &str, tool: &str, text: &str, details: Option<Value>) -> AgentEvent {
    AgentEvent::ToolExecutionEnd {
        tool_call_id: id.into(),
        tool_name: tool.into(),
        result: AgentToolResult {
            details,
            ..AgentToolResult::text(text)
        },
        is_error: false,
        duration_ms: None,
        parent_tool_call_id: None,
    }
}

/// The live rows the updates end with.
fn live_rows(updates: Vec<RuntimeUpdate>) -> Vec<Row> {
    updates
        .into_iter()
        .rev()
        .find_map(|update| match update {
            RuntimeUpdate::Live(rows) => Some(rows),
            _ => None,
        })
        .expect("a live update")
}

fn phases(updates: &[RuntimeUpdate]) -> Vec<&str> {
    updates
        .iter()
        .filter_map(|update| match update {
            RuntimeUpdate::Phase(phase) => Some(phase.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_subagent_settlement_is_the_card_of_the_agent_it_reports() {
    use bake_coding_agent::subagents::messages::{
        StopReason as Stop, agent_message, settlement_notice,
    };
    let known = labels(&[("child-1", "read the docs")]);
    let notice = |stop, closing| custom(settlement_notice("child-1", 1, stop, closing));
    let agent = |state, word, output: &[&str]| {
        Some(card(
            "Agent",
            "read the docs",
            Some("child-1"),
            state,
            Some(word),
            output,
        ))
    };
    assert_eq!(
        notice_row(&notice(Stop::Completed, Some("long\nanswer\n")), &known),
        agent(CallState::Done, "done", &["long", "answer"])
    );
    assert_eq!(
        notice_row(&notice(Stop::Aborted, None), &known),
        agent(CallState::Done, "stopped", &[])
    );
    assert_eq!(
        notice_row(&notice(Stop::MaxTokens, Some("partial")), &known),
        agent(CallState::Failed, "token limit", &["partial"])
    );
    assert_eq!(
        notice_row(&notice(Stop::Error, None), &known),
        agent(CallState::Failed, "failed", &[])
    );
    assert_eq!(
        notice_row(&notice(Stop::Interrupted, None), &known),
        agent(CallState::Failed, "interrupted", &[])
    );
    // An agent the session never saw start has no label to show.
    assert_eq!(
        notice_row(&notice(Stop::Completed, None), &Labels::new()),
        Some(card(
            "Agent",
            "",
            Some("child-1"),
            CallState::Done,
            Some("done"),
            &[]
        ))
    );
    // Without its details the notice reads as its first line.
    let mut bare = settlement_notice("child-1", 1, Stop::Completed, Some("answer"));
    bare.details = None;
    assert_eq!(
        info(notice_row(&custom(bare), &known)).as_deref(),
        Some(
            "Background subagent child-1 finished and will do no further work unless you send it more."
        )
    );
    let message = agent_message(
        "m",
        "child-1",
        "parent",
        &format!("{}\nsecond line", "x".repeat(60)),
    );
    assert_eq!(
        info(notice_row(&custom(message), &known)).as_deref(),
        Some(format!("← agent child-1: {}…", "x".repeat(48)).as_str())
    );
}

#[test]
fn a_stop_hook_cap_note_is_an_info_row_and_other_hook_context_is_not_shown() {
    let message = |details: Value| {
        custom(CustomMessage {
            custom_type: hooks::CONTEXT_MESSAGE_TYPE.into(),
            content: UserContent::Text("Stop hooks kept this run going".into()),
            display: true,
            details: Some(details),
            timestamp: 0,
        })
    };
    let capped = message(serde_json::json!({ "event": "Stop", "capped": true }));
    assert_eq!(
        info(notice_row(&capped, &Labels::new())).as_deref(),
        Some("Stop hooks kept this run going")
    );
    assert_eq!(
        replay(std::slice::from_ref(&capped)),
        [Row::Info("Stop hooks kept this run going".into())]
    );
    let context = message(serde_json::json!({ "event": "Stop" }));
    assert_eq!(notice_row(&context, &Labels::new()), None);
}

#[test]
fn calls_that_reach_a_job_or_agent_name_it_by_its_label_and_tag_its_id() {
    let known = labels(&[("bash-1", "sleep 30"), ("child-1", "read the docs")]);
    assert_eq!(
        argument(
            "subagent",
            &json!({ "description": "read\n a  file", "prompt": "p", "provider": "x", "model": "y" }),
            &known
        ),
        "read … · x/y"
    );
    let message = json!({ "agent_id": "c", "message": "hi\nthere" });
    assert_eq!(argument("send_message", &message, &known), "hi …");
    assert_eq!(target("send_message", &message).as_deref(), Some("c"));
    assert_eq!(argument("list_agents", &json!({}), &known), "");
    let output = json!({ "job_id": "bash-1" });
    assert_eq!(argument("job_output", &output, &known), "sleep 30");
    assert_eq!(target("job_output", &output).as_deref(), Some("bash-1"));
    assert_eq!(
        argument("job_kill", &json!({ "job_id": "bash-9" }), &known),
        ""
    );
    assert_eq!(
        argument("interrupt_agent", &json!({ "agent_id": "child-1" }), &known),
        "read the docs"
    );
    assert_eq!(target("job_list", &json!({})), None);
    assert_eq!(
        argument("web_search", &json!({ "queries": ["one", "two"] }), &known),
        "one, two"
    );
    assert_eq!(
        argument("web_fetch", &json!({ "url": "https://a.test/x" }), &known),
        "https://a.test/x"
    );
    assert_eq!(display_name("web_search"), "Web search");
    assert_eq!(display_name("job_output"), "Job output");
    assert_eq!(display_name("subagent"), "Agent");
    assert_eq!(display_name("bash"), "Bash");
}

#[test]
fn background_work_is_tagged_from_its_start_to_what_came_of_it() {
    let mut projector = Projector::default();
    projector.begin(None);
    run(
        &mut projector,
        start(
            "a",
            "bash",
            json!({ "command": "sleep 30", "run_in_background": true }),
        ),
    );
    let rows = live_rows(run(
        &mut projector,
        end(
            "a",
            "bash",
            "Started background job bash-1.",
            Some(json!({ BACKGROUND_JOB_DETAIL: { "id": "bash-1", "label": "sleep 30" } })),
        ),
    ));
    assert_eq!(
        rows,
        [card(
            "Bash",
            "sleep 30",
            Some("bash-1"),
            CallState::Done,
            Some("background"),
            &[]
        )]
    );
    let waiting = |state, summary, output: &[&str]| {
        card(
            "Job output",
            "sleep 30",
            Some("bash-1"),
            state,
            summary,
            output,
        )
    };
    let wait = json!({ "job_id": "bash-1", "wait": true });
    let rows = live_rows(run(&mut projector, start("b", "job_output", wait.clone())));
    assert_eq!(rows[1], waiting(CallState::Running, Some("waiting"), &[]));
    // Partial output keeps the card waiting.
    let rows = live_rows(run(
        &mut projector,
        AgentEvent::ToolExecutionUpdate {
            tool_call_id: "b".into(),
            tool_name: "job_output".into(),
            args: wait,
            partial_result: AgentToolResult::text("tick"),
            parent_tool_call_id: None,
        },
    ));
    assert_eq!(
        rows[1],
        waiting(CallState::Running, Some("waiting"), &["tick"])
    );
    let rows = live_rows(run(
        &mut projector,
        end(
            "b",
            "job_output",
            &format!("hello\n[status: running]\n{STILL_RUNNING}"),
            None,
        ),
    ));
    assert_eq!(
        rows[1],
        waiting(CallState::Done, Some("running"), &["hello"])
    );
    // A read with nothing new says only how the job ended.
    run(
        &mut projector,
        start("c", "job_output", json!({ "job_id": "bash-1" })),
    );
    let rows = live_rows(run(
        &mut projector,
        end(
            "c",
            "job_output",
            "(no new output)\n[status: completed, exit code: 1]",
            None,
        ),
    ));
    assert_eq!(rows[2], waiting(CallState::Done, Some("exit 1"), &[]));
    // A foreground subagent's answer is its output, under its id.
    run(
        &mut projector,
        start("d", "subagent", json!({ "description": "look" })),
    );
    let rows = live_rows(run(
        &mut projector,
        end(
            "d",
            "subagent",
            "the answer",
            Some(
                json!({ SUBAGENT_DETAIL: { "agentId": "child-1", "label": "look", "foreground": true } }),
            ),
        ),
    ));
    assert_eq!(
        rows[3],
        card(
            "Agent",
            "look",
            Some("child-1"),
            CallState::Done,
            None,
            &["the answer"]
        )
    );
    assert_eq!(
        projector.labels.get("child-1").map(String::as_str),
        Some("look")
    );
}

#[test]
fn job_and_agent_calls_say_what_came_of_them() {
    let say = |tool: &str, text: &str| {
        let content = [UserContentBlock::Text(TextContent::new(text))];
        let outcome = outcome(tool, false, &content, None);
        (outcome.summary, outcome.output)
    };
    let said = |summary: &str| (Some(summary.to_owned()), Vec::<String>::new());
    assert_eq!(
        say("job_kill", "requested cancellation of job bash-1"),
        said("stopping")
    );
    assert_eq!(
        say(
            "job_kill",
            "job bash-1 had already finished [status: completed, exit code: 0]"
        ),
        said("already finished · exit 0")
    );
    assert_eq!(
        say(
            "job_kill",
            "job bash-1 had already finished [status: killed, signal: SIGTERM]"
        ),
        said("already finished · stopped")
    );
    assert_eq!(
        say("send_message", "message delivered to agent child-1"),
        said("delivered")
    );
    assert_eq!(
        say("interrupt_agent", "interrupt requested for agent child-1"),
        said("stopping")
    );
    assert_eq!(say("job_list", "(no background jobs)"), said("none"));
    assert_eq!(say("list_agents", "(no subagents)"), said("none"));
    assert_eq!(
        say("job_list", "bash-1 [bash] running — sleep 30"),
        (None, vec!["bash-1 [bash] running — sleep 30".to_owned()])
    );
    // A failed call keeps the error the model read.
    let content = [UserContentBlock::Text(TextContent::new(
        "unknown job bash-9",
    ))];
    let failed = outcome("job_kill", true, &content, None);
    assert_eq!(failed.state, CallState::Failed);
    assert_eq!(failed.summary, None);
    assert_eq!(failed.output, ["unknown job bash-9"]);
}

#[test]
fn a_job_notice_is_the_card_of_the_job_it_reports() {
    use bake_coding_agent::agent_session::job_notices::lost_notice;
    let row = |text: &str| notice_row(&job_notice_message(text), &Labels::new());
    let bash = |label: &str, state, summary: &str| {
        Some(card(
            "Bash",
            label,
            Some("bash-1"),
            state,
            Some(summary),
            &[],
        ))
    };
    assert_eq!(
        row(
            "background job bash-1 (bash: sleep 30) finished [status: completed, exit code: 0]. Read its output with job_output."
        ),
        bash("sleep 30", CallState::Done, "exit 0")
    );
    assert_eq!(
        row(
            "background job bash-1 (bash: make) finished [status: completed, exit code: 2]. Read its output with job_output."
        ),
        bash("make", CallState::Failed, "exit 2")
    );
    assert_eq!(
        row(
            "background job bash-1 (bash: sleep 30) finished [status: killed]. The user stopped it. Read its output with job_output."
        ),
        bash("sleep 30", CallState::Done, "stopped")
    );
    // A label may hold `) ` and brackets of its own.
    assert_eq!(
        row(
            "background job bash-1 (bash: (cd x) && ls [a]) finished [status: failed, spawn error]. Read its output with job_output."
        ),
        bash("(cd x) && ls [a]", CallState::Failed, "failed")
    );
    assert_eq!(
        row(
            "background job bash-1 (bash: sleep 30) finished in its earlier process [status: completed, exit code: 0]. Its output was not retained across processes and is unavailable."
        ),
        bash("sleep 30", CallState::Done, "exit 0 · earlier process")
    );
    assert_eq!(
        notice_row(
            &custom(lost_notice("bash-1", "bash", "sleep 30")),
            &Labels::new()
        ),
        bash(
            "sleep 30",
            CallState::Failed,
            "outcome unknown · earlier process"
        )
    );
    // Text it cannot read stays information.
    assert_eq!(
        info(row("background job bash-1 finished")).as_deref(),
        Some("background job bash-1 finished")
    );
}

#[test]
fn a_resumed_session_names_its_background_work_by_label() {
    let call = faux_assistant_blocks(
        vec![
            faux_tool_call(
                "bash",
                json!({"command": "sleep 30", "run_in_background": true}),
                Some("a"),
            ),
            faux_tool_call("job_kill", json!({"job_id": "bash-1"}), Some("b")),
        ],
        StopReason::ToolUse,
    );
    let kill = tool_result(
        "b",
        "job_kill",
        "requested cancellation of job bash-1",
        false,
        None,
    );
    let messages = vec![
        user("go"),
        Message::Assistant(call).into(),
        result(
            "a",
            "Started background job bash-1.",
            false,
            Some(json!({ BACKGROUND_JOB_DETAIL: { "id": "bash-1", "label": "sleep 30" } })),
        ),
        kill,
        job_notice_message(
            "background job bash-1 (bash: sleep 30) finished [status: killed]. Read its output with job_output.",
        ),
    ];
    let tagged = |tool, summary| {
        card(
            tool,
            "sleep 30",
            Some("bash-1"),
            CallState::Done,
            Some(summary),
            &[],
        )
    };
    assert_eq!(
        replay(&messages),
        [
            Row::User("go".into()),
            tagged("Bash", "background"),
            tagged("Stop job", "stopping"),
            tagged("Bash", "stopped"),
        ]
    );
    // A live session resumed on them names the job too.
    let projector = Projector::with_history(&messages);
    assert_eq!(
        argument(
            "job_output",
            &json!({"job_id": "bash-1"}),
            &projector.labels
        ),
        "sleep 30"
    );
}

#[test]
fn the_phase_counts_the_calls_still_running() {
    let mut projector = Projector::default();
    projector.begin(None);
    let mut phase = |event| phases(&run(&mut projector, event)).join("|");
    assert_eq!(
        phase(start("a", "bash", json!({"command": "sleep 1"}))),
        "running sleep"
    );
    // The latest call names the phase; the others are counted.
    assert_eq!(
        phase(start("b", "read", json!({"path": "src/x.rs"}))),
        "reading x.rs +1"
    );
    assert_eq!(phase(end("b", "read", "x", None)), "running sleep");
    assert_eq!(phase(end("a", "bash", "done", None)), "thinking");
}

#[test]
fn the_phase_names_what_a_call_does_as_its_card_reads() {
    let labels = Labels::new();
    let named = |tool: &str, args: Value| action(tool, &args, &labels);
    assert_eq!(
        named(
            "read",
            json!({"path": "crates/bake-tui/src/transcript.rs", "offset": 4})
        ),
        "reading transcript.rs"
    );
    assert_eq!(
        named(
            "bash",
            json!({"command": "RUST_LOG=1 cargo test --locked -p x"})
        ),
        "running cargo test"
    );
    assert_eq!(
        named("bash", json!({"command": "rg -n x src"})),
        "running rg"
    );
    assert_eq!(
        named("bash", json!({"command": "/usr/bin/git status\necho"})),
        "running git status"
    );
    assert_eq!(
        named("grep", json!({"pattern": "ctx.effect", "path": "src"})),
        "searching ctx.effect"
    );
    assert_eq!(
        named("write", json!({"path": "a/notes.md"})),
        "writing notes.md"
    );
    assert_eq!(named("edit", json!({"path": "lib.rs"})), "editing lib.rs");
    assert_eq!(
        named("find", json!({"pattern": "*.toml"})),
        "finding *.toml"
    );
    assert_eq!(named("ls", json!({})), "listing .");
    assert_eq!(
        named("web_fetch", json!({"url": "https://www.docs.rs/ratatui"})),
        "fetching docs.rs"
    );
    assert_eq!(
        named("web_search", json!({"queries": ["ratatui wrap"]})),
        "searching the web for ratatui wrap"
    );
    assert_eq!(named("web_search", json!({})), "searching the web");
    assert_eq!(
        named("mcp__github__create_issue", json!({"title": "x"})),
        "github · create_issue"
    );
    // A missing argument names the tool, and any other call reads by its
    // title, lowercase; one phrase, cut, with no escape.
    assert_eq!(named("read", json!({})), "read");
    assert_eq!(named("job_output", json!({"job_id": "j1"})), "job output");
    let long = named(
        "grep",
        json!({"pattern": format!("\u{1b}[31m{}\u{7}\nx", "a".repeat(80))}),
    );
    assert_eq!(long.chars().count(), 40, "{long}");
    assert!(long.ends_with('…'), "{long}");
    assert!(!long.contains(char::is_control), "{long:?}");
    assert!(!long.contains("[31m"), "{long:?}");
}

#[test]
fn schedule_reminders_are_information_rows() {
    use bake_coding_agent::schedules::{Due, Record, Rule, delivery};
    let record = |id: &str, prompt: &str| Record {
        id: id.into(),
        rule: Rule::Every { seconds: 300 },
        prompt: prompt.into(),
        scheduled_at: 0,
    };
    let row = |due: Due| {
        let (message, _) = delivery(&due).expect("a delivery");
        info(notice_row(&custom(message), &Labels::new()))
    };
    assert_eq!(
        row(Due::OneShot(record(
            "schedule-1",
            &format!(
                "{}
more",
                "y".repeat(50)
            )
        )))
        .as_deref(),
        Some(format!("reminder schedule-1: {}…", "y".repeat(48)).as_str())
    );
    assert_eq!(
        row(Due::Every {
            reminders: vec![
                (record("schedule-1", "a"), 0),
                (record("schedule-2", "b"), 0)
            ],
            accepted_at: 0,
        })
        .as_deref(),
        Some("reminders schedule-1, schedule-2")
    );
}

const SCRIPT: &str = "const a = await tools.read({ path: \"a\" });\nreturn a;";
const HEADER: &str = "Script completed\nWall time 0.1 seconds\nOutput:\n";

fn codemode_update(calls: Value) -> AgentEvent {
    AgentEvent::ToolExecutionUpdate {
        tool_call_id: "c".into(),
        tool_name: CODEMODE_TOOL_NAME.into(),
        args: json!({ "code": SCRIPT }),
        partial_result: AgentToolResult {
            details: Some(json!({ "calls": calls })),
            ..AgentToolResult::default()
        },
        parent_tool_call_id: None,
    }
}

fn codemode_result() -> AgentToolResult {
    AgentToolResult {
        content: vec![
            UserContentBlock::Text(TextContent::new(HEADER)),
            UserContentBlock::Text(TextContent::new("alpha")),
        ],
        details: Some(json!({
            "calls": [
                { "id": "c/1", "name": "read", "args": "{\"path\":\"a\"}", "status": "ok", "durationMs": 5.2 },
                { "id": "c/2", "name": "bash", "args": "{\"command\":\"false\"}", "status": "error", "durationMs": 12, "error": "exit 1" },
            ],
            "fullOutputPath": "/tmp/out.txt",
        })),
        ..AgentToolResult::default()
    }
}

fn codemode(row: &Row) -> &Codemode {
    match row {
        Row::Codemode(script) => script,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_codemode_call_shows_its_calls_live_and_reads_the_same_replayed() {
    let assistant = faux_assistant_blocks(
        vec![faux_tool_call(
            CODEMODE_TOOL_NAME,
            json!({ "code": SCRIPT }),
            Some("c"),
        )],
        StopReason::ToolUse,
    );
    let mut projector = Projector::default();
    projector.begin(None);
    run(
        &mut projector,
        AgentEvent::MessageEnd {
            message: Message::Assistant(assistant.clone()).into(),
        },
    );
    let rows = live_rows(run(
        &mut projector,
        start("c", CODEMODE_TOOL_NAME, json!({ "code": SCRIPT })),
    ));
    let script = codemode(&rows[0]);
    assert_eq!(
        (script.code.as_str(), script.state, script.calls.len()),
        (SCRIPT, CallState::Running, 0)
    );
    // A nested call's own events stay out of the transcript.
    let nested = AgentEvent::ToolExecutionStart {
        tool_call_id: "c/1".into(),
        tool_name: "read".into(),
        args: json!({ "path": "a" }),
        parent_tool_call_id: Some("c".into()),
    };
    assert_eq!(run(&mut projector, nested), []);
    // Partial results start empty and carry every call so far.
    let rows = live_rows(run(&mut projector, codemode_update(json!([]))));
    assert!(codemode(&rows[0]).calls.is_empty());
    let rows = live_rows(run(
        &mut projector,
        codemode_update(json!([
            { "id": "c/1", "name": "read", "args": "{\"path\":\"a\"}", "status": "ok", "durationMs": 5.2 },
            { "id": "c/?", "name": "bash", "args": "{\"command\":\"false\"}", "status": "running" },
            { "id": "c/?", "name": "future", "args": "{}", "status": "paused" },
        ])),
    ));
    let script = codemode(&rows[0]);
    assert_eq!(script.state, CallState::Running);
    assert_eq!(
        script.calls,
        [
            NestedCall {
                name: "read".into(),
                args: r#"{"path":"a"}"#.into(),
                status: NestedStatus::Ok,
                duration: Some(Duration::from_micros(5_200)),
                error: None,
            },
            // A status this frontend does not know is left out.
            NestedCall {
                name: "bash".into(),
                args: r#"{"command":"false"}"#.into(),
                status: NestedStatus::Running,
                duration: None,
                error: None,
            },
        ]
    );
    // The result settles it: its calls, its output without the
    // header, the time the header states, and the full output's file.
    run(
        &mut projector,
        AgentEvent::ToolExecutionEnd {
            tool_call_id: "c".into(),
            tool_name: CODEMODE_TOOL_NAME.into(),
            result: codemode_result(),
            is_error: false,
            duration_ms: Some(130),
            parent_tool_call_id: None,
        },
    );
    let Some(RuntimeUpdate::Commit(committed)) = projector.finish().pop() else {
        panic!("the turn commits");
    };
    let settled = Codemode {
        code: SCRIPT.into(),
        state: CallState::Done,
        summary: None,
        calls: vec![
            script.calls[0].clone(),
            NestedCall {
                name: "bash".into(),
                args: r#"{"command":"false"}"#.into(),
                status: NestedStatus::Error,
                duration: Some(Duration::from_millis(12)),
                error: Some("exit 1".into()),
            },
        ],
        output: vec!["alpha".into()],
        full_output_path: Some("/tmp/out.txt".into()),
        wall_time: Some(Duration::from_millis(100)),
    };
    assert_eq!(committed, [Row::Codemode(settled.clone())]);
    // The session log replays the same block.
    let logged = Message::ToolResult(ToolResultMessage {
        tool_call_id: "c".into(),
        tool_name: CODEMODE_TOOL_NAME.into(),
        content: codemode_result().content,
        details: codemode_result().details,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 0,
        duration_ms: Some(130),
    });
    assert_eq!(
        replay(&[Message::Assistant(assistant).into(), logged.into()]),
        [Row::Codemode(settled)]
    );
}

#[test]
fn an_unfinished_codemode_call_is_interrupted_live_and_replayed() {
    let assistant = faux_assistant_blocks(
        vec![faux_tool_call(
            CODEMODE_TOOL_NAME,
            json!({ "code": SCRIPT }),
            Some("c"),
        )],
        StopReason::ToolUse,
    );
    let mut projector = Projector::default();
    projector.begin(None);
    run(
        &mut projector,
        AgentEvent::MessageEnd {
            message: Message::Assistant(assistant.clone()).into(),
        },
    );
    run(
        &mut projector,
        start("c", CODEMODE_TOOL_NAME, json!({ "code": SCRIPT })),
    );
    let rows = live_rows(run(
        &mut projector,
        codemode_update(json!([
            { "id": "c/?", "name": "bash", "args": "{}", "status": "running" },
        ])),
    ));
    assert_eq!(codemode(&rows[0]).calls.len(), 1);
    // The turn ends with no result: a teardown, not an Esc, which logs
    // one.
    let Some(RuntimeUpdate::Commit(committed)) = projector.finish().pop() else {
        panic!("the turn commits");
    };
    let script = codemode(&committed[0]);
    assert_eq!(
        (
            script.state,
            script.summary.as_deref(),
            script.calls.len(),
            script.output.len()
        ),
        (CallState::Failed, Some(INTERRUPTED), 0, 0)
    );
    // The log holds the call and no result, and replays the same block.
    assert_eq!(replay(&[Message::Assistant(assistant).into()]), committed);
}

#[test]
fn a_codemode_result_without_a_header_keeps_all_its_text() {
    let rejected = outcome(
        CODEMODE_TOOL_NAME,
        true,
        &[UserContentBlock::Text(TextContent::new(
            "The @options line must be followed by JavaScript source",
        ))],
        None,
    );
    assert_eq!(
        (rejected.state, rejected.output, rejected.wall_time),
        (
            CallState::Failed,
            vec!["The @options line must be followed by JavaScript source".to_owned()],
            None
        )
    );
}

/// A settled call's status and output from its result's text.
fn shown(tool: &str, text: &str, details: Option<Value>) -> (Option<String>, Vec<String>) {
    let content = [UserContentBlock::Text(TextContent::new(text))];
    let settled = outcome(tool, false, &content, details.as_ref());
    (settled.summary, settled.output)
}

fn status(text: &str) -> Option<String> {
    Some(text.to_owned())
}

fn lines(text: &[&str]) -> Vec<String> {
    text.iter().map(|line| (*line).to_owned()).collect()
}

#[test]
fn a_cut_output_names_the_file_holding_all_of_it() {
    let path = "/tmp/bake-bash-1.log";
    let details = json!({ "fullOutputPath": path, "truncation": { "truncated": true } });
    let text = format!(
        "line 1\nline 2\n\n[Showing lines 1-2 of 9000. Full output: {path}]\n\nCommand exited with code 1"
    );
    let content = [UserContentBlock::Text(TextContent::new(text))];
    let settled = outcome("bash", true, &content, Some(&details));
    assert_eq!(settled.full_output_path.as_deref(), Some(path));
    assert_eq!(
        settled.output,
        lines(&["line 1", "line 2", "", "Command exited with code 1"])
    );
    // A running call's partial result reads the same.
    let partial_result = AgentToolResult {
        content: content.to_vec(),
        details: Some(details.clone()),
        ..AgentToolResult::default()
    };
    let running = partial("bash", &partial_result);
    assert_eq!(running.full_output_path.as_deref(), Some(path));
    assert_eq!(running.output, settled.output);
    // An MCP tool's notice goes the same way; its warning stays.
    let mcp = format!(
        "Warning: truncated output (original token count: 9)\nTotal output lines: 2\n\nab\n\n[Full output: {path} (read it with offset/limit)]"
    );
    let content = [UserContentBlock::Text(TextContent::new(mcp))];
    let settled = outcome("mcp__docs__fetch", false, &content, Some(&details));
    assert_eq!(settled.full_output_path.as_deref(), Some(path));
    assert_eq!(settled.output.last().map(String::as_str), Some("ab"));
    // A card drawn from it names the file.
    let row = call_row(
        "bash",
        &json!({ "command": "make" }),
        &Labels::new(),
        outcome("bash", true, &content, Some(&details)),
    );
    assert!(matches!(row, Row::Call { full_output_path: Some(ref named), .. } if named == path));
    // Without the detail, a bracketed line stays.
    let content = [UserContentBlock::Text(TextContent::new(format!(
        "[{path}]"
    )))];
    let plain = outcome("bash", false, &content, None);
    assert_eq!(
        (plain.full_output_path, plain.output),
        (None, vec![format!("[{path}]")])
    );
}

#[test]
fn a_read_says_the_lines_it_asked_for_and_read() {
    let known = Labels::new();
    let read = |args: Value| argument("read", &args, &known);
    assert_eq!(
        read(json!({ "path": "src/a.rs", "offset": 120, "limit": 61 })),
        "src/a.rs:120-180"
    );
    assert_eq!(read(json!({ "path": "a", "offset": 120 })), "a:120");
    assert_eq!(read(json!({ "path": "a", "limit": 40 })), "a:1-40");
    assert_eq!(read(json!({ "path": "a" })), "a");
    // As Pi's range does, one ending at 0 is left open.
    assert_eq!(read(json!({ "path": "a", "offset": 1, "limit": 0 })), "a:1");
    // A read's card counts lines and keeps the file's text for the
    // expanded card, without the note a limited read ends with.
    assert_eq!(
        shown("read", "a\nb\nc\n", None),
        (status("3 lines"), lines(&["a", "b", "c"]))
    );
    assert_eq!(
        shown(
            "read",
            "a\n\n[10 more lines in file. Use offset=3 to continue.]",
            None
        ),
        (status("1 line"), lines(&["a"]))
    );
    // A file whose own last paragraph is bracketed keeps it.
    assert_eq!(
        shown("read", "a\n\n[section]", None),
        (status("3 lines"), lines(&["a", "", "[section]"]))
    );
    assert_eq!(shown("read", "", None), (status("empty"), vec![]));
    let truncated = json!({ "truncation": {
        "truncated": true, "outputLines": 2000, "firstLineExceedsLimit": false
    }});
    assert_eq!(
        shown(
            "read",
            "a\n\n[Showing lines 1-2000 of 9000. Use offset=2001 to continue.]",
            Some(truncated)
        ),
        (status("2000 lines · truncated"), lines(&["a"]))
    );
    // A line too long to read says so; the read's note on it is the
    // model's.
    let note = "[Line 1 is 80KB, exceeds 50KB limit. Use bash: sed -n '1p' a | head -c 51200]";
    let long = json!({ "truncation": { "truncated": true, "firstLineExceedsLimit": true }});
    assert_eq!(
        shown("read", note, Some(long)),
        (status("line too long"), vec![])
    );
    // An image says its type, and that it was left out when it was.
    let image = |text: &str| {
        let content = [
            UserContentBlock::Text(TextContent::new(text)),
            UserContentBlock::Image(bake_ai::ImageContent {
                data: String::new(),
                mime_type: "image/png".into(),
            }),
        ];
        let settled = outcome("read", false, &content, None);
        (settled.summary, settled.output)
    };
    assert_eq!(
        image("Read image file [image/png]"),
        (status("image/png"), vec![])
    );
    assert_eq!(
        image("Read image file [image/png]\n[Image: resized to 2000x1500]"),
        (status("image/png"), vec![])
    );
    assert_eq!(
        image(
            "Read image file [image/png]\n[Current model does not support images. The image will be omitted from this request.]"
        ),
        (status("image/png · omitted"), vec![])
    );
    assert_eq!(
        shown(
            "read",
            "Read image file [image/webp]\n[Image omitted: too large.]",
            None
        ),
        (status("image/webp · omitted"), vec![])
    );
    // A failed read shows its error.
    let content = [UserContentBlock::Text(TextContent::new(
        "ENOENT: no such file",
    ))];
    let failed = outcome("read", true, &content, None);
    assert_eq!(
        (failed.state, failed.summary, failed.output),
        (CallState::Failed, None, lines(&["ENOENT: no such file"]))
    );
}

#[test]
fn a_search_counts_its_matches_and_files_and_its_limits() {
    let known = Labels::new();
    assert_eq!(
        argument(
            "grep",
            &json!({ "pattern": "x", "path": "src", "glob": "*.rs" }),
            &known
        ),
        "x in src (*.rs)"
    );
    let found = "a.rs:1: x\na.rs-2- around\nb.rs:3: x";
    assert_eq!(
        shown("grep", found, None),
        (
            status("2 matches · 2 files"),
            lines(&["a.rs:1: x", "a.rs-2- around", "b.rs:3: x"])
        )
    );
    assert_eq!(
        shown(
            "grep",
            &format!(
                "{found}\n\n[2 matches limit reached. Use limit=4 for more, or refine pattern. 50KB limit reached]"
            ),
            Some(json!({ "matchLimitReached": 2, "truncation": { "truncated": true } }))
        ),
        (
            status("2 matches · 2 files · limit reached · truncated"),
            lines(&["a.rs:1: x", "a.rs-2- around", "b.rs:3: x"])
        )
    );
    assert_eq!(
        shown("grep", "No matches found", None),
        (status("no matches"), vec![])
    );
    // Without details, a last line in brackets is a match's, not a notice.
    assert_eq!(
        shown("grep", "[a].rs:1: x", None),
        (status("1 match · 1 file"), lines(&["[a].rs:1: x"]))
    );
}

#[test]
fn a_listing_counts_what_it_found() {
    assert_eq!(
        shown("find", "src/\nsrc/a.rs\nb.rs", None),
        (
            status("2 files · 1 directory"),
            lines(&["src/", "src/a.rs", "b.rs"])
        )
    );
    assert_eq!(
        shown(
            "find",
            "a.rs\n\n[1 results limit reached. Use limit=2 for more, or refine pattern]",
            Some(json!({ "resultLimitReached": 1 }))
        ),
        (status("1 file · limit reached"), lines(&["a.rs"]))
    );
    assert_eq!(
        shown("find", "No files found matching pattern", None),
        (status("no files"), vec![])
    );
    assert_eq!(
        shown(
            "ls",
            "a\nb/\n\n[2 entries limit reached. Use limit=4 for more]",
            Some(json!({ "entryLimitReached": 2 }))
        ),
        (status("2 entries · limit reached"), lines(&["a", "b/"]))
    );
    assert_eq!(
        shown("ls", "(empty directory)", None),
        (status("empty"), vec![])
    );
}

/// The card reads the ls tool's own results: since Pi v1.5.2 a listing
/// of exactly its limit has nothing more to report.
#[tokio::test]
async fn a_listing_reads_the_ls_tools_own_limit() {
    let dir = std::env::temp_dir().join(format!("bake-card-ls-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("b")).unwrap();
    std::fs::write(dir.join("a"), "").unwrap();
    let ls = bake_coding_agent::tools::ls::create_ls_tool(&dir);
    let mut cards = Vec::new();
    for limit in [2, 1] {
        let result = (ls.execute)(
            "call".into(),
            json!({ "limit": limit }),
            None,
            bake_agent::AgentToolUpdateCallback::ignore(),
            bake_agent::ToolContext::detached(),
        )
        .await
        .unwrap();
        let card = settled("ls", false, &result);
        cards.push((card.summary, card.output));
    }
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(
        cards,
        [
            (status("2 entries"), lines(&["a", "b/"])),
            (status("1 entry · limit reached"), lines(&["a"])),
        ]
    );
}

#[test]
fn a_bash_timeout_after_the_shell_exits_shows_as_it_came() {
    let error = "started\n\nCommand timed out after 1 seconds. The shell exited with code 0 after 0.1 seconds, but background processes it started kept writing output until the timeout.";
    let content = [UserContentBlock::Text(TextContent::new(error))];
    let failed = outcome("bash", true, &content, None);
    assert_eq!(
        (failed.state, failed.summary, failed.output),
        (
            CallState::Failed,
            None,
            lines(&["started", "", error.lines().last().unwrap()])
        )
    );
}

#[test]
fn web_calls_say_what_came_back() {
    use bake_coding_agent::tools::web_tools::EXTERNAL_WEB_CONTENT_NOTICE as NOTICE;
    let page = format!("Fetched https://a.test/ (HTTP 404)\n\n{NOTICE}\n\nnot found");
    assert_eq!(
        shown(
            "web_fetch",
            &page,
            Some(json!({ "url": "https://a.test/", "statusCode": 404, "truncated": false }))
        ),
        (status("HTTP 404 · 9 B"), lines(&["not found"]))
    );
    assert_eq!(
        shown(
            "web_fetch",
            &page,
            Some(json!({ "url": "https://a.test/", "statusCode": 200, "truncated": true }))
        )
        .0,
        status("HTTP 200 · 9 B · truncated")
    );
    let sources = json!({
        "sources": [
            { "url": "https://docs.rs/x", "title": "Docs\nfor x" },
            { "url": "https://www.a.test/b c" },
        ],
        "truncated": true,
    });
    assert_eq!(
        shown("web_search", "Sources: …", Some(sources)),
        (
            status("2 results · limit reached"),
            lines(&["Docs … https://docs.rs/x", " https://www.a.test/bc"])
        )
    );
    assert_eq!(
        shown(
            "web_search",
            "…",
            Some(json!({ "sources": [], "truncated": false, "answer": "It is 4.\n" }))
        ),
        (None, lines(&["It is 4."]))
    );
    assert_eq!(
        shown(
            "web_search",
            "No results found.",
            Some(json!({ "sources": [], "truncated": false }))
        ),
        (status("no results"), vec![])
    );
    // Without details, a search shows its text as it came.
    assert_eq!(
        shown("web_search", "No results found.", None),
        (None, lines(&["No results found."]))
    );
}

#[test]
fn an_mcp_call_reads_as_its_server_its_tool_and_its_fields() {
    let known = Labels::new();
    assert_eq!(
        display_name("mcp__github__create_issue"),
        "github · create_issue"
    );
    assert_eq!(display_name("mcp__a__b__c"), "a · b__c");
    assert_eq!(display_name("mcp____x"), "Mcp____x");
    assert_eq!(display_name("mcp__x__"), "Mcp__x__");
    let args = json!({
        "repo": "acme/app",
        "body": "first line\nsecond line",
        "count": 3,
        "draft": false,
        "empty": "",
        "labels": ["bug", "ui", "a-long-label-to-push-past-the-limit-of-the-field"],
    });
    let tool = "mcp__github__create_issue";
    assert_eq!(argument(tool, &args, &known), "");
    let pairs = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        fields(tool, &args, &known),
        pairs(&[
            ("repo", "acme/app"),
            ("body", "first line …"),
            ("count", "3"),
            ("draft", "false"),
            ("empty", r#""""#),
            (
                "labels",
                r#"["bug","ui","a-long-label-to-push-past-the-limi…"#
            ),
        ])
    );
    // Arguments that are not an object read as JSON, and a known tool's
    // card leads with its argument instead.
    assert_eq!(argument(tool, &json!([1, 2]), &known), "[1,2]");
    assert!(fields(tool, &json!([1, 2]), &known).is_empty());
    assert!(fields("bash", &json!({ "command": "ls" }), &known).is_empty());
    assert_eq!(argument("job_list", &json!({}), &known), "");
    assert!(fields("job_list", &json!({}), &known).is_empty());
}

#[test]
fn a_write_shows_its_content_live_and_replayed() {
    let args = json!({ "path": "a.txt", "content": "one\ntwo\n" });
    let assistant = faux_assistant_blocks(
        vec![faux_tool_call("write", args.clone(), Some("w"))],
        StopReason::ToolUse,
    );
    let written = card(
        "Write",
        "a.txt",
        None,
        CallState::Running,
        None,
        &["+one", "+two"],
    );
    let mut projector = Projector::default();
    projector.begin(None);
    run(
        &mut projector,
        AgentEvent::MessageEnd {
            message: Message::Assistant(assistant.clone()).into(),
        },
    );
    // The content shows from the call's arguments while it runs.
    assert_eq!(
        live_rows(run(&mut projector, start("w", "write", args.clone()))),
        [written]
    );
    let done = live_rows(run(
        &mut projector,
        end("w", "write", "Successfully wrote 8 bytes to a.txt", None),
    ));
    let settled = card(
        "Write",
        "a.txt",
        None,
        CallState::Done,
        None,
        &["+one", "+two"],
    );
    assert_eq!(done, std::slice::from_ref(&settled));
    let logged = tool_result(
        "w",
        "write",
        "Successfully wrote 8 bytes to a.txt",
        false,
        None,
    );
    assert_eq!(
        replay(&[Message::Assistant(assistant.clone()).into(), logged]),
        [settled]
    );
    // A write cut off before its result shows none, as its replay does.
    let mut projector = Projector::default();
    projector.begin(None);
    run(
        &mut projector,
        AgentEvent::MessageEnd {
            message: Message::Assistant(assistant.clone()).into(),
        },
    );
    run(&mut projector, start("w", "write", args));
    let Some(RuntimeUpdate::Commit(committed)) = projector.finish().pop() else {
        panic!("the turn commits");
    };
    let interrupted = card(
        "Write",
        "a.txt",
        None,
        CallState::Failed,
        Some(INTERRUPTED),
        &[],
    );
    assert_eq!(committed, [interrupted]);
    assert_eq!(replay(&[Message::Assistant(assistant).into()]), committed);
    // A failed write shows its error.
    let failed = outcome(
        "write",
        true,
        &[UserContentBlock::Text(TextContent::new("EACCES"))],
        None,
    );
    assert_eq!(
        call_row(
            "write",
            &json!({ "path": "a", "content": "x" }),
            &Labels::new(),
            failed
        ),
        card("Write", "a", None, CallState::Failed, None, &["EACCES"])
    );
}

mod properties {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        // Any arguments read as one field each, on one line, inside its limits.
        #[test]
        fn any_arguments_read_as_bounded_one_line_fields(
            args in prop::collection::vec(
                (".{0,40}", prop_oneof![
                    ".{0,80}".prop_map(Value::from),
                    any::<i64>().prop_map(Value::from),
                    prop::collection::vec(".{0,20}", 0..6).prop_map(Value::from),
                ]),
                0..8,
            ),
        ) {
            let object: serde_json::Map<String, Value> = args.into_iter().collect();
            let count = object.len();
            let read = fields("mcp__s__t", &Value::Object(object), &Labels::new());
            prop_assert_eq!(read.len(), count);
            for (key, value) in &read {
                prop_assert!(key.chars().count() <= 32, "{key:?}");
                prop_assert!(value.chars().count() <= 48, "{value:?}");
                prop_assert!(!value.is_empty());
                prop_assert!(!key.contains(['\n', '\r']) && !value.contains(['\n', '\r']));
            }
        }

        // Any result text reads without a panic for every tool with a card of
        // its own, and a notice the details flag never reaches the card.
        #[test]
        fn any_result_reads_without_a_panic(
            text in ".{0,200}|[a-z/.:\\- 0-9\n\\[\\]]{0,200}",
            limit in any::<bool>(),
        ) {
            for tool in ["read", "grep", "find", "ls", "web_fetch", "web_search"] {
                let noticed = format!("{text}\n\n[1 limit reached]");
                let details = json!({
                    "matchLimitReached": limit, "resultLimitReached": limit,
                    "entryLimitReached": limit, "statusCode": 200, "sources": [],
                });
                let content = [UserContentBlock::Text(TextContent::new(noticed))];
                let settled = outcome(tool, false, &content, Some(&details));
                if matches!(tool, "grep" | "find" | "ls") {
                    prop_assert!(settled.output.last().is_none_or(|line| line != "[1 limit reached]"));
                }
            }
        }
    }
}

/// The `visualize` call `id` with the arguments `text` parsed as a stream
/// parses them, as one assistant message.
fn visual_message(text: &str, stop: StopReason) -> AssistantMessage {
    let arguments = bake_ai::utils::json_parse::parse_streaming_json_object(Some(text));
    faux_assistant_blocks(
        vec![faux_tool_call(
            VISUALIZE_TOOL_NAME,
            Value::Object(arguments),
            Some("v"),
        )],
        stop,
    )
}

/// The update a provider sends as the arguments reach `text`.
fn visual_delta(text: &str) -> AgentEvent {
    let partial = visual_message(text, StopReason::ToolUse);
    AgentEvent::MessageUpdate {
        message: Message::Assistant(partial.clone()).into(),
        assistant_message_event: bake_ai::AssistantMessageEvent::ToolCallDelta {
            content_index: 0,
            delta: String::new(),
            partial,
        },
    }
}

/// The update a provider sends once the arguments are complete.
fn visual_closed(text: &str) -> AgentEvent {
    let partial = visual_message(text, StopReason::ToolUse);
    let AssistantContentBlock::ToolCall(tool_call) = partial.content[0].clone() else {
        unreachable!()
    };
    AgentEvent::MessageUpdate {
        message: Message::Assistant(partial.clone()).into(),
        assistant_message_event: bake_ai::AssistantMessageEvent::ToolCallEnd {
            content_index: 0,
            tool_call,
            partial,
        },
    }
}

fn shown_visual(row: &Row) -> &bake_tui_view::visual::Visual {
    match row {
        Row::Visual(visual) => visual,
        other => panic!("{other:?}"),
    }
}

const VISUAL_ARGS: &str = r#"{"kind":"bar","title":"Build","columns":["crate","s"],"y":{"unit":"s"},"rows":[["a",1],["b",125],["c",3]]}"#;

#[test]
fn a_visual_draws_while_its_arguments_stream_then_settles_and_replays() {
    let mut projector = Projector::default();
    projector.begin(None);
    // Cut inside `125`: the cut row is left out, and the card streams.
    let at = VISUAL_ARGS.find("125").expect("value") + 2;
    let rows = live_rows(run(&mut projector, visual_delta(&VISUAL_ARGS[..at])));
    let streaming = shown_visual(&rows[0]);
    assert!(streaming.streaming);
    assert_eq!(streaming.state, CallState::Running);
    assert_eq!(streaming.spec.rows.len(), 1);
    assert_eq!(streaming.spec.title, "Build");
    assert_eq!(streaming.spec.unit, "s");
    // Once the provider closes the arguments, every row shows.
    let rows = live_rows(run(&mut projector, visual_closed(VISUAL_ARGS)));
    let closed = shown_visual(&rows[0]);
    assert!(!closed.streaming);
    assert_eq!(closed.spec.rows.len(), 3);
    let message = visual_message(VISUAL_ARGS, StopReason::ToolUse);
    run(
        &mut projector,
        AgentEvent::MessageEnd {
            message: Message::Assistant(message.clone()).into(),
        },
    );
    let args = Value::Object(bake_ai::utils::json_parse::parse_streaming_json_object(
        Some(VISUAL_ARGS),
    ));
    run(&mut projector, start("v", VISUALIZE_TOOL_NAME, args));
    let text = "Shown to the user: bar chart \"Build\" (3 rows, 1 series). The user can see it; do not repeat the data in prose.";
    let done = live_rows(run(
        &mut projector,
        end("v", VISUALIZE_TOOL_NAME, text, None),
    ));
    let settled = shown_visual(&done[0]);
    assert_eq!(settled.state, CallState::Done);
    assert!(settled.error.is_empty() && settled.summary.is_none());
    assert_eq!(settled.spec, closed.spec);
    let logged = tool_result("v", VISUALIZE_TOOL_NAME, text, false, None);
    assert_eq!(replay(&[Message::Assistant(message).into(), logged]), done);
}

#[test]
fn an_aborted_visual_keeps_its_drawing_marked_interrupted_live_and_replayed() {
    let mut projector = Projector::default();
    projector.begin(None);
    let at = VISUAL_ARGS.find("125").expect("value") + 2;
    run(&mut projector, visual_delta(&VISUAL_ARGS[..at]));
    let aborted = visual_message(&VISUAL_ARGS[..at], StopReason::Aborted);
    let rows = live_rows(run(
        &mut projector,
        AgentEvent::MessageEnd {
            message: Message::Assistant(aborted.clone()).into(),
        },
    ));
    let cut = shown_visual(&rows[0]);
    assert_eq!(
        (cut.state, cut.summary.as_deref(), cut.streaming),
        (CallState::Failed, Some(INTERRUPTED), false)
    );
    assert_eq!(cut.spec.rows.len(), 1, "the cut row stays out");
    assert_eq!(rows[1], Row::Error(ABORTED.into()));
    let Some(RuntimeUpdate::Commit(committed)) = projector.finish().pop() else {
        panic!("the turn commits");
    };
    assert_eq!(committed, rows);
    assert_eq!(replay(&[Message::Assistant(aborted).into()]), committed);
    // A complete call that never ran keeps its whole drawing.
    let mut projector = Projector::default();
    projector.begin(None);
    let message = visual_message(VISUAL_ARGS, StopReason::ToolUse);
    run(
        &mut projector,
        AgentEvent::MessageEnd {
            message: Message::Assistant(message.clone()).into(),
        },
    );
    let Some(RuntimeUpdate::Commit(committed)) = projector.finish().pop() else {
        panic!("the turn commits");
    };
    let unrun = shown_visual(&committed[0]);
    assert_eq!(
        (unrun.state, unrun.summary.as_deref(), unrun.spec.rows.len()),
        (CallState::Failed, Some(INTERRUPTED), 3)
    );
    assert_eq!(replay(&[Message::Assistant(message).into()]), committed);
}

#[test]
fn a_visual_the_tool_refused_shows_the_error() {
    let refused = outcome(
        VISUALIZE_TOOL_NAME,
        true,
        &[UserContentBlock::Text(TextContent::new(
            "rows[4] has 3 values; columns has 2",
        ))],
        None,
    );
    let row = call_row(
        VISUALIZE_TOOL_NAME,
        &json!({ "kind": "bar", "columns": ["a", "b"], "rows": [["x", 1]] }),
        &Labels::new(),
        refused,
    );
    let shown = shown_visual(&row);
    assert_eq!(shown.state, CallState::Failed);
    assert_eq!(shown.error, ["rows[4] has 3 values; columns has 2"]);
}

#[test]
fn a_visual_too_large_to_redraw_waits_for_its_arguments() {
    let rows: Vec<String> = (0..2000).map(|i| format!("[\"row {i}\",{i}]")).collect();
    let text = format!(
        r#"{{"kind":"bar","columns":["k","v"],"rows":[{}]}}"#,
        rows.join(",")
    );
    let mut projector = Projector::default();
    projector.begin(None);
    let live = live_rows(run(&mut projector, visual_delta(&text[..text.len() - 2])));
    let streaming = shown_visual(&live[0]);
    assert!(streaming.too_large && streaming.spec.rows.is_empty());
    let live = live_rows(run(&mut projector, visual_closed(&text)));
    let closed = shown_visual(&live[0]);
    assert!(!closed.too_large);
    assert_eq!(
        closed.spec.rows.len(),
        bake_coding_agent::tools::visualize::MAX_ROWS
    );
}

/// A `visualize` diagram: a sequence whose `loop` block stays open for
/// three lines while it streams.
const DIAGRAM_ARGS: &str = r#"{"kind":"diagram","title":"Turn","source":"sequenceDiagram\n    participant U as User\n    U->>Bake: prompt\n    loop each tool\n        Bake->>Tool: call\n        Tool-->>Bake: result\n    end\n    Bake-->>U: answer"}"#;

/// Whether a diagram card's drawing is its source as code.
fn shows_source(lines: &[String]) -> bool {
    lines
        .first()
        .is_some_and(|line| line.starts_with("╭─ ") || line.starts_with("+- "))
}

fn drawn_text(
    spec: &bake_tui_view::visual::Spec,
    width: usize,
    look: bake_tui_view::transcript::Look,
) -> Vec<String> {
    bake_tui_view::visual::draw(spec, width, look)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect()
}

#[test]
fn a_diagram_draws_while_its_source_streams_then_settles_and_replays() {
    let look = bake_tui_view::transcript::Look::default();
    let mut projector = Projector::default();
    projector.begin(None);
    let mut drew = false;
    let mut shown_lines = 0;
    let ends: Vec<usize> = DIAGRAM_ARGS
        .char_indices()
        .map(|(at, c)| at + c.len_utf8())
        .collect();
    // Small chunks, as a provider streams them.
    for &at in ends.iter().step_by(3) {
        let rows = live_rows(run(&mut projector, visual_delta(&DIAGRAM_ARGS[..at])));
        let shown = shown_visual(&rows[0]);
        assert!(shown.streaming);
        let lines = drawn_text(&shown.spec, 60, look);
        if drew {
            assert!(
                !shows_source(&lines),
                "turned back at {}",
                &DIAGRAM_ARGS[..at]
            );
            assert!(
                lines.len() >= shown_lines,
                "shrank at {}",
                &DIAGRAM_ARGS[..at]
            );
        }
        if !lines.is_empty() && !shows_source(&lines) {
            drew = true;
            shown_lines = lines.len();
        }
    }
    assert!(drew);
    let rows = live_rows(run(&mut projector, visual_closed(DIAGRAM_ARGS)));
    let closed = shown_visual(&rows[0]);
    assert!(!closed.streaming && !closed.spec.partial);
    let message = visual_message(DIAGRAM_ARGS, StopReason::ToolUse);
    run(
        &mut projector,
        AgentEvent::MessageEnd {
            message: Message::Assistant(message.clone()).into(),
        },
    );
    let args = serde_json::from_str::<Value>(DIAGRAM_ARGS).expect("valid");
    run(&mut projector, start("v", VISUALIZE_TOOL_NAME, args));
    let text = "Shown to the user: diagram \"Turn\" (sequence diagram, 3 participants, 4 messages). The user can see it; do not repeat the data in prose.";
    let done = live_rows(run(
        &mut projector,
        end("v", VISUALIZE_TOOL_NAME, text, None),
    ));
    let settled = shown_visual(&done[0]);
    assert_eq!(settled.state, CallState::Done);
    assert_eq!(settled.spec, closed.spec);
    let logged = tool_result("v", VISUALIZE_TOOL_NAME, text, false, None);
    let replayed = replay(&[Message::Assistant(message).into(), logged]);
    assert_eq!(replayed, done);
    for width in [40, 60, 100] {
        assert_eq!(
            bake_tui_view::transcript::present(&done, 0, width, look),
            bake_tui_view::transcript::present(&replayed, 0, width, look)
        );
    }
    // The tool's error replaces the drawing.
    let refused = outcome(
        VISUALIZE_TOOL_NAME,
        true,
        &[UserContentBlock::Text(TextContent::new(
            "Mermaid line 3 \"subgraph api\" cannot be drawn: subgraphs are not drawn",
        ))],
        None,
    );
    let row = call_row(
        VISUALIZE_TOOL_NAME,
        &json!({ "kind": "diagram", "source": "flowchart TD\n    A --> B\n    subgraph api" }),
        &Labels::new(),
        refused,
    );
    let shown = shown_visual(&row);
    assert_eq!(shown.state, CallState::Failed);
    assert_eq!(
        shown.error,
        ["Mermaid line 3 \"subgraph api\" cannot be drawn: subgraphs are not drawn"]
    );
}

#[test]
fn an_aborted_diagram_keeps_the_longest_run_that_drew() {
    let at = DIAGRAM_ARGS.find("Tool-->>").expect("a message");
    let mut projector = Projector::default();
    projector.begin(None);
    let streamed = live_rows(run(&mut projector, visual_delta(&DIAGRAM_ARGS[..at])));
    let aborted = visual_message(&DIAGRAM_ARGS[..at], StopReason::Aborted);
    let rows = live_rows(run(
        &mut projector,
        AgentEvent::MessageEnd {
            message: Message::Assistant(aborted.clone()).into(),
        },
    ));
    let cut = shown_visual(&rows[0]);
    assert_eq!(
        (cut.state, cut.summary.as_deref(), cut.spec.partial),
        (CallState::Failed, Some(INTERRUPTED), true)
    );
    let look = bake_tui_view::transcript::Look::default();
    let lines = drawn_text(&cut.spec, 60, look);
    assert!(!shows_source(&lines));
    assert_eq!(
        lines,
        drawn_text(&shown_visual(&streamed[0]).spec, 60, look)
    );
    let Some(RuntimeUpdate::Commit(committed)) = projector.finish().pop() else {
        panic!("the turn commits");
    };
    assert_eq!(replay(&[Message::Assistant(aborted).into()]), committed);
}

mod visual_properties {
    use bake_tui_view::activity::Tones;
    use bake_tui_view::transcript::{Look, present};
    use proptest::prelude::*;

    use super::*;

    const LOOKS: [Look; 4] = [
        Look {
            tones: Tones::TrueColor,
            classic: false,
            lit: true,
            mermaid: bake_tui_view::mermaid::Mermaid::Streaming,
        },
        Look {
            tones: Tones::Ansi,
            classic: false,
            lit: true,
            mermaid: bake_tui_view::mermaid::Mermaid::Streaming,
        },
        Look {
            tones: Tones::None,
            classic: false,
            lit: true,
            mermaid: bake_tui_view::mermaid::Mermaid::Streaming,
        },
        Look {
            tones: Tones::Ansi,
            classic: true,
            lit: true,
            mermaid: bake_tui_view::mermaid::Mermaid::Streaming,
        },
    ];

    fn any_json() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::from),
            any::<f64>()
                .prop_map(|v| serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number)),
            any::<i64>().prop_map(Value::from),
            "\\PC{0,30}|[\\x00-\\x1f\\u{1b}a\\[m]{0,10}".prop_map(Value::from),
            prop_oneof![Just("bar"), Just("line"), Just("table"), Just("diagram")]
                .prop_map(Value::from),
            prop_oneof![
                Just("flowchart LR\n    A[Ask] --> B{ok?}\n    B --> C([Done])"),
                Just("sequenceDiagram\n    A->>B: hi\n    loop x\n        B-->>A: 请求\n    end\n"),
                Just("classDiagram\n    class C {\n        +int id\n    }\n    C <|-- D"),
                Just("stateDiagram-v2\n    [*] --> S\n    subgraph x\n"),
                Just("flowchart TD\n\t\u{1b}[31mA --> B\r\n"),
            ]
            .prop_map(Value::from),
        ];
        leaf.prop_recursive(4, 64, 8, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..8).prop_map(Value::from),
                prop::collection::vec(
                    (
                        prop_oneof![
                            Just("kind".to_owned()),
                            Just("title".to_owned()),
                            Just("columns".to_owned()),
                            Just("y".to_owned()),
                            Just("rows".to_owned()),
                            Just("min".to_owned()),
                            Just("max".to_owned()),
                            Just("unit".to_owned()),
                            Just("source".to_owned()),
                            "[a-z]{1,4}",
                        ],
                        inner,
                    ),
                    0..6
                )
                .prop_map(|members| Value::Object(members.into_iter().collect())),
            ]
        })
    }

    /// Valid bar arguments, as the model would write them.
    fn bar_args() -> impl Strategy<Value = String> {
        (
            "[a-z ]{0,12}",
            prop::collection::vec(
                (
                    "[a-z0-9]{1,8}",
                    prop_oneof![
                        (-1e4..1e4f64).prop_map(Value::from),
                        any::<i32>().prop_map(Value::from),
                        Just(Value::Null)
                    ],
                ),
                1..12,
            ),
        )
            .prop_map(|(title, rows)| {
                let rows: Vec<Value> = rows
                    .into_iter()
                    .map(|(label, value)| json!([label, value]))
                    .collect();
                serde_json::to_string(&json!({
                    "kind": "bar",
                    "title": title,
                    "columns": ["k", "v"],
                    "rows": rows,
                }))
                .expect("JSON")
            })
    }

    /// Valid diagram arguments, as the model would write them: random
    /// flowchart edges or sequence messages, some in a `loop` block.
    fn diagram_args() -> impl Strategy<Value = String> {
        let node = prop::sample::select(&["A", "B", "C", "D"][..]);
        let label = "[a-z]{1,8}";
        prop_oneof![
            prop::collection::vec((node.clone(), label, node.clone()), 1..8).prop_map(|edges| {
                let mut source = "flowchart LR\n".to_owned();
                for (from, label, to) in edges {
                    source.push_str(&format!("    {from} -->|{label}| {to}\n"));
                }
                source
            }),
            (
                prop::collection::vec((node.clone(), label, node), 1..8),
                0usize..8,
            )
                .prop_map(|(messages, block)| {
                    let mut source = "sequenceDiagram\n".to_owned();
                    let open = block.min(messages.len());
                    for (i, (from, label, to)) in messages.iter().enumerate() {
                        if i == open {
                            source.push_str("    loop again\n");
                        }
                        source.push_str(&format!("    {from}->>{to}: {label}\n"));
                    }
                    if open < messages.len() {
                        source.push_str("    end\n");
                    }
                    source
                }),
        ]
        .prop_map(|source| {
            serde_json::to_string(&json!({ "kind": "diagram", "source": source.trim_end() }))
                .expect("JSON")
        })
    }

    fn check_lines(row: &Row, width: usize) -> Result<(), TestCaseError> {
        for look in LOOKS {
            for line in present(std::slice::from_ref(row), 0, width, look) {
                prop_assert!(line.width() <= width, "{width}: {line:?}");
                for span in &line.spans {
                    prop_assert!(!span.content.chars().any(char::is_control), "{span:?}");
                }
            }
        }
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        // Any arguments draw a card inside its width, with no control
        // character, whether they are still arriving or complete.
        #[test]
        fn any_arguments_draw_inside_the_width(
            args in any_json(),
            width in 1usize..120,
            read in 0usize..3,
        ) {
            let read = [Args::Complete, Args::Streaming, Args::Cut][read];
            let row = visual::visual_row(&args, Outcome::running(Vec::new()), read);
            check_lines(&row, width)?;
        }

        // Every prefix of valid arguments draws without a panic, a bar
        // chart never loses a row as more arrive, and once the call
        // settles it reads as the replay draws the final arguments.
        #[test]
        fn streamed_arguments_grow_and_settle_as_the_final_drawing(
            text in bar_args(),
            width in 20usize..100,
        ) {
            let mut projector = Projector::default();
            projector.begin(None);
            let mut rows = 0;
            let mut lines = 0;
            let ends: Vec<usize> = text
                .char_indices()
                .map(|(at, c)| at + c.len_utf8())
                .collect();
            for &at in &ends {
                let live = live_rows(run(&mut projector, visual_delta(&text[..at])));
                let shown = shown_visual(&live[0]);
                prop_assert!(shown.streaming);
                prop_assert!(shown.spec.rows.len() >= rows, "{}", &text[..at]);
                rows = shown.spec.rows.len();
                let drawn = bake_tui_view::visual::draw(&shown.spec, width, LOOKS[0]).len();
                prop_assert!(drawn >= lines, "{}", &text[..at]);
                lines = drawn;
                check_lines(&live[0], width)?;
            }
            run(&mut projector, visual_closed(&text));
            let message = visual_message(&text, StopReason::ToolUse);
            run(
                &mut projector,
                AgentEvent::MessageEnd {
                    message: Message::Assistant(message.clone()).into(),
                },
            );
            let args = serde_json::from_str::<Value>(&text).expect("valid");
            run(&mut projector, start("v", VISUALIZE_TOOL_NAME, args));
            let live = live_rows(run(&mut projector, end("v", VISUALIZE_TOOL_NAME, "Shown.", None)));
            let replayed = replay(&[
                Message::Assistant(message).into(),
                tool_result("v", VISUALIZE_TOOL_NAME, "Shown.", false, None),
            ]);
            prop_assert_eq!(&live, &replayed);
            for look in LOOKS {
                prop_assert_eq!(
                    present(&live, 0, width, look),
                    present(&replayed, 0, width, look)
                );
            }
        }

        // Every prefix of a valid diagram's arguments draws without a
        // panic, once drawn never turns back into its source, and once the
        // call settles reads as the replay draws it.
        #[test]
        fn a_streamed_diagram_never_turns_back_and_settles_as_its_replay(
            text in diagram_args(),
            width in 16usize..100,
        ) {
            let mut projector = Projector::default();
            projector.begin(None);
            let mut drew = [false; 4];
            let ends: Vec<usize> = text
                .char_indices()
                .map(|(at, c)| at + c.len_utf8())
                .collect();
            for &at in &ends {
                let live = live_rows(run(&mut projector, visual_delta(&text[..at])));
                let shown = shown_visual(&live[0]);
                for (look, drew) in LOOKS.into_iter().zip(&mut drew) {
                    let lines = super::drawn_text(&shown.spec, width, look);
                    let source = super::shows_source(&lines);
                    prop_assert!(!(*drew && source), "{width}: {}", &text[..at]);
                    *drew |= !lines.is_empty() && !source;
                }
                check_lines(&live[0], width)?;
            }
            run(&mut projector, visual_closed(&text));
            let message = visual_message(&text, StopReason::ToolUse);
            run(
                &mut projector,
                AgentEvent::MessageEnd {
                    message: Message::Assistant(message.clone()).into(),
                },
            );
            let args = serde_json::from_str::<Value>(&text).expect("valid");
            run(&mut projector, start("v", VISUALIZE_TOOL_NAME, args));
            let live = live_rows(run(&mut projector, end("v", VISUALIZE_TOOL_NAME, "Shown.", None)));
            let replayed = replay(&[
                Message::Assistant(message).into(),
                tool_result("v", VISUALIZE_TOOL_NAME, "Shown.", false, None),
            ]);
            prop_assert_eq!(&live, &replayed);
            for look in LOOKS {
                prop_assert_eq!(
                    present(&live, 0, width, look),
                    present(&replayed, 0, width, look)
                );
            }
        }
    }
}
