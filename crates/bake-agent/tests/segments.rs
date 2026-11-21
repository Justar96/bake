//! Bake's segments: a response's calls run in the order the model wrote
//! them, adjacent `Read` calls together, and every other effect alone.

mod common;

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bake_agent::{
    AgentContext, AgentEvent, AgentLoopConfig, AgentTool, AgentToolResult, ToolEffect, agent_loop,
    hook,
};
use bake_ai::{AbortController, StopReason};
use common::*;
use serde_json::json;

/// What the run's tools and before hook saw, in the order they saw it.
#[derive(Default)]
struct Log {
    entries: Mutex<Vec<String>>,
    active: AtomicUsize,
    peak: AtomicUsize,
}

impl Log {
    fn push(&self, entry: String) {
        self.entries.lock().unwrap().push(entry);
    }

    fn entries(&self) -> Vec<String> {
        self.entries.lock().unwrap().clone()
    }
}

/// A tool that logs when each call enters and leaves, and sleeps between.
fn tool(name: &str, effect: Option<ToolEffect>, log: &Arc<Log>) -> Arc<AgentTool> {
    let log = Arc::clone(log);
    let mut tool = AgentTool::new(
        name,
        name,
        name,
        empty_schema(),
        move |id, _, _, _, _ctx| {
            let log = Arc::clone(&log);
            Box::pin(async move {
                let active = log.active.fetch_add(1, Ordering::SeqCst) + 1;
                log.peak.fetch_max(active, Ordering::SeqCst);
                log.push(format!("enter:{id}"));
                tokio::time::sleep(Duration::from_millis(10)).await;
                log.push(format!("leave:{id}"));
                log.active.fetch_sub(1, Ordering::SeqCst);
                Ok(AgentToolResult::text(id))
            })
        },
    );
    tool.effect = effect;
    Arc::new(tool)
}

fn tools(log: &Arc<Log>) -> Vec<Arc<AgentTool>> {
    vec![
        tool("read", Some(ToolEffect::Read), log),
        tool("write", Some(ToolEffect::Write), log),
        tool("exec", Some(ToolEffect::Exec), log),
        tool("undeclared", None, log),
    ]
}

/// Runs one response of `calls`, `(id, tool)`, with the cap, and returns
/// its events; the before hook logs each call's preparation.
async fn run(calls: &[(&str, &str)], cap: Option<usize>, log: &Arc<Log>) -> Vec<AgentEvent> {
    let mut config = AgentLoopConfig::new(model(), identity_converter());
    config.max_concurrent_read_tools = cap.and_then(NonZeroUsize::new);
    let seen = Arc::clone(log);
    config.before_tool_call = Some(hook::before_tool_call(move |call, _| {
        seen.push(format!("prepare:{}", call.tool_call.id));
        Box::pin(async { Ok(None) })
    }));
    let calls = calls
        .iter()
        .map(|(id, name)| call(id, name, json!({})))
        .collect();
    let stream = agent_loop(
        vec![user("go")],
        AgentContext {
            messages: Vec::new(),
            tools: tools(log),
        },
        config,
        None,
        Some(tools_then_done(calls)),
    );
    tokio::time::timeout(Duration::from_secs(10), drain(&stream))
        .await
        .expect("the run ends")
}

fn position(entries: &[String], entry: &str) -> usize {
    entries
        .iter()
        .position(|seen| seen == entry)
        .unwrap_or_else(|| panic!("{entry} in {entries:?}"))
}

/// Every entry of `first` comes before every entry of `then`.
fn before(entries: &[String], first: &[&str], then: &[&str]) {
    for a in first {
        for b in then {
            assert!(
                position(entries, a) < position(entries, b),
                "{a} before {b} in {entries:?}"
            );
        }
    }
}

/// The event kinds of the tool calls, as `kind:id`.
fn tool_events(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                Some(format!("start:{tool_call_id}"))
            }
            AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
                Some(format!("end:{tool_call_id}"))
            }
            AgentEvent::MessageStart { message } => message
                .as_tool_result()
                .map(|result| format!("result:{}", result.tool_call_id)),
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn reads_around_a_write_and_a_command_run_together_in_order() {
    let log = Arc::new(Log::default());
    let events = run(
        &[
            ("r1", "read"),
            ("r2", "read"),
            ("w", "write"),
            ("x", "exec"),
            ("r3", "read"),
            ("r4", "read"),
        ],
        None,
        &log,
    )
    .await;
    let entries = log.entries();
    // Each run of reads overlaps.
    before(
        &entries,
        &["enter:r1", "enter:r2"],
        &["leave:r1", "leave:r2"],
    );
    before(
        &entries,
        &["enter:r3", "enter:r4"],
        &["leave:r3", "leave:r4"],
    );
    // The write and the command run alone, in order, and nothing of a
    // segment, its preparation included, starts before the one before it
    // finished.
    before(&entries, &["leave:r1", "leave:r2"], &["prepare:w"]);
    before(&entries, &["leave:w"], &["prepare:x"]);
    before(&entries, &["leave:x"], &["prepare:r3", "prepare:r4"]);
    assert_eq!(log.peak.load(Ordering::SeqCst), 2);
    let events = tool_events(&events);
    // Inside a segment the calls are prepared in order, and the results
    // follow once the segment is done, in assistant order.
    before(&events, &["start:r1"], &["start:r2"]);
    before(&events, &["start:r2"], &["end:r1", "end:r2"]);
    before(&events, &["end:r1", "end:r2"], &["result:r1"]);
    before(&events, &["result:r1"], &["result:r2"]);
    assert_eq!(
        events[6..12],
        [
            "start:w", "end:w", "result:w", "start:x", "end:x", "result:x"
        ]
    );
    before(&events, &["result:x"], &["start:r3"]);
    assert_eq!(
        tool_result_ids_of(&events),
        ["r1", "r2", "w", "x", "r3", "r4"]
    );
}

#[tokio::test(start_paused = true)]
async fn the_cap_bounds_each_run_of_reads() {
    let log = Arc::new(Log::default());
    let events = run(
        &[
            ("r1", "read"),
            ("r2", "read"),
            ("r3", "read"),
            ("w", "write"),
            ("r4", "read"),
            ("r5", "read"),
            ("r6", "read"),
        ],
        Some(2),
        &log,
    )
    .await;
    let entries = log.entries();
    assert_eq!(log.peak.load(Ordering::SeqCst), 2);
    before(&entries, &["enter:r1", "enter:r2"], &["leave:r1"]);
    before(&entries, &["leave:r1", "leave:r2"], &["enter:r3"]);
    before(&entries, &["leave:r3"], &["prepare:w"]);
    before(&entries, &["leave:w"], &["prepare:r4"]);
    before(&entries, &["enter:r4", "enter:r5"], &["leave:r4"]);
    before(&entries, &["leave:r4", "leave:r5"], &["enter:r6"]);
    assert_eq!(
        tool_result_ids_of(&tool_events(&events)),
        ["r1", "r2", "r3", "w", "r4", "r5", "r6"]
    );
}

/// With the cap, an undeclared or unknown tool runs alone; the reads on
/// each side of it still run together. Without the cap, an undeclared tool
/// runs beside the reads, as in Pi.
#[tokio::test(start_paused = true)]
async fn under_the_cap_a_tool_that_is_not_read_runs_alone() {
    for name in ["undeclared", "missing"] {
        let log = Arc::new(Log::default());
        let events = run(
            &[
                ("r1", "read"),
                ("r2", "read"),
                ("u", name),
                ("r3", "read"),
                ("r4", "read"),
            ],
            Some(4),
            &log,
        )
        .await;
        let entries = log.entries();
        before(
            &entries,
            &["enter:r1", "enter:r2"],
            &["leave:r1", "leave:r2"],
        );
        before(
            &entries,
            &["enter:r3", "enter:r4"],
            &["leave:r3", "leave:r4"],
        );
        assert_eq!(log.peak.load(Ordering::SeqCst), 2, "{name}");
        let events = tool_events(&events);
        before(&events, &["result:r1", "result:r2"], &["start:u"]);
        before(&events, &["result:u"], &["start:r3", "start:r4"]);
        if name == "undeclared" {
            before(&entries, &["leave:r1", "leave:r2"], &["enter:u"]);
            before(&entries, &["leave:u"], &["prepare:r3"]);
        }
    }

    let log = Arc::new(Log::default());
    run(
        &[("r1", "read"), ("u", "undeclared"), ("r2", "read")],
        None,
        &log,
    )
    .await;
    assert_eq!(log.peak.load(Ordering::SeqCst), 3);
}

fn tool_result_ids_of(events: &[String]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| event.strip_prefix("result:"))
        .collect()
}

/// When a `Write` barrier aborts mid-batch, later `Exec` and `Read` segments
/// do not run or reach `before_tool_call`, and every remaining call is
/// answered with `Operation aborted` (never the failed-write skip message),
/// in assistant order.
#[tokio::test]
async fn aborted_mixed_write_then_exec_and_reads_answers_every_call_aborted() {
    let log = Arc::new(Log::default());
    let controller = AbortController::new();
    let abort = controller.clone();
    let write_log = Arc::clone(&log);
    let mut aborting_write = AgentTool::new(
        "write",
        "write",
        "write",
        empty_schema(),
        move |id, _, _, _, _ctx| {
            let write_log = Arc::clone(&write_log);
            let abort = abort.clone();
            Box::pin(async move {
                write_log.push(format!("enter:{id}"));
                abort.abort();
                Err("write interrupted".to_owned())
            })
        },
    );
    aborting_write.effect = Some(ToolEffect::Write);

    let mut config = AgentLoopConfig::new(model(), identity_converter());
    config.max_concurrent_read_tools = NonZeroUsize::new(2);
    let seen = Arc::clone(&log);
    config.before_tool_call = Some(hook::before_tool_call(move |call, _| {
        seen.push(format!("prepare:{}", call.tool_call.id));
        Box::pin(async { Ok(None) })
    }));

    let stream = agent_loop(
        vec![user("go")],
        AgentContext {
            messages: Vec::new(),
            tools: vec![
                tool("read", Some(ToolEffect::Read), &log),
                Arc::new(aborting_write),
                tool("exec", Some(ToolEffect::Exec), &log),
            ],
        },
        config,
        Some(controller.signal()),
        Some(scripted(|index, _, _| {
            if index == 0 {
                tool_use(vec![
                    call("r1", "read", json!({})),
                    call("w", "write", json!({})),
                    call("x", "exec", json!({})),
                    call("r2", "read", json!({})),
                ])
            } else {
                failed(StopReason::Aborted)
            }
        })),
    );

    let events = drain(&stream).await;
    let messages = stream.result().await.unwrap();
    let entries = log.entries();
    assert_eq!(
        entries,
        ["prepare:r1", "enter:r1", "leave:r1", "prepare:w", "enter:w"]
    );
    assert_eq!(
        tool_events(&events),
        [
            "start:r1",
            "end:r1",
            "result:r1",
            "start:w",
            "end:w",
            "result:w",
            "start:x",
            "end:x",
            "result:x",
            "start:r2",
            "end:r2",
            "result:r2",
        ]
    );
    assert_tool_results(
        &messages,
        &[
            ("r1", false, "r1"),
            ("w", true, "write interrupted"),
            ("x", true, "Operation aborted"),
            ("r2", true, "Operation aborted"),
        ],
    );
}
