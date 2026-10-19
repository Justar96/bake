//! Pi `packages/coding-agent/test/nested-tool-calls.test.ts` (v1.3.0), with
//! the runner over a pipeline without hooks in place of Pi's test host,
//! whose `runToolCall` runs the tool directly.

use std::sync::atomic::AtomicUsize;

use bake_ai::providers::faux::faux_assistant_message;
use bake_ai::{UsageCost, UserContentBlock};
use serde_json::json;

use super::*;
use crate::types::AgentContext;

fn usage(input: u64, cost: f64) -> Usage {
    Usage {
        input,
        total_tokens: input,
        cost: UsageCost {
            input: cost,
            total: cost,
            ..UsageCost::default()
        },
        ..Usage::default()
    }
}

fn tool(
    name: &str,
    execute: impl Fn(
        String,
        AgentToolUpdateCallback,
        ToolContext,
    ) -> BoxFuture<'static, Result<AgentToolResult, String>>
    + Send
    + Sync
    + 'static,
) -> AgentTool {
    AgentTool::new(
        name,
        name,
        name,
        json!({ "type": "object", "properties": {} }),
        move |id, _args, _signal, on_update, ctx| execute(id, on_update, ctx),
    )
}

fn empty() -> BoxFuture<'static, Result<AgentToolResult, String>> {
    Box::pin(async { Ok(AgentToolResult::default()) })
}

type Events = Arc<Mutex<Vec<AgentEvent>>>;

/// Pi's `createRunner`: a runner over `tools`, collecting its events.
fn create_runner(tools: Vec<AgentTool>, sequential: bool) -> (Arc<NestedToolCallRunner>, Events) {
    create_runner_with(tools, sequential, None)
}

/// [`create_runner`] whose pipeline runs `before`.
fn create_runner_with(
    tools: Vec<AgentTool>,
    sequential: bool,
    before: Option<BeforeToolCall>,
) -> (Arc<NestedToolCallRunner>, Events) {
    let events: Events = Arc::default();
    let sink: AgentEventSink = {
        let events = Arc::clone(&events);
        Arc::new(move |event| {
            events.lock().unwrap().push(event);
            Box::pin(async {})
        })
    };
    // Nothing answers: the site is ready before the first call.
    let (requests, _) = mpsc::unbounded_channel();
    let pipeline = NestedPipeline::new(
        requests,
        before,
        None,
        sink,
        if sequential {
            ToolExecutionMode::Sequential
        } else {
            ToolExecutionMode::Parallel
        },
        None,
    );
    let context = AgentContext {
        messages: Vec::new(),
        tools: tools.into_iter().map(Arc::new).collect(),
    };
    let _ = pipeline
        .site
        .set(CallSiteSnapshot::new(&context, &faux_assistant_message("")));
    (NestedToolCallRunner::new(pipeline), events)
}

fn record(id: &str, name: &str, arguments: Value) -> NestedToolCallRecord {
    NestedToolCallRecord {
        id: id.into(),
        name: name.into(),
        arguments: arguments.as_object().cloned(),
        arguments_bytes: None,
        status: "ok".into(),
        duration_ms: Some(0),
        error: None,
    }
}

#[tokio::test(start_paused = true)]
async fn assigns_ids_below_the_caller_emits_events_with_the_parent_id_and_records_the_calls() {
    let echo = tool("echo", |_id, on_update, _ctx| {
        on_update.update(AgentToolResult::text("partial"));
        Box::pin(async { Ok(AgentToolResult::text("ok")) })
    });
    let (runner, events) = create_runner(vec![echo], false);
    let updates = Arc::new(Mutex::new(Vec::new()));
    let call = runner.executor("call", None);

    let first = call
        .execute(
            "echo",
            json!({ "a": 1 }),
            NestedToolCallOptions {
                signal: None,
                on_update: Some(AgentToolUpdateCallback::new({
                    let updates = Arc::clone(&updates);
                    move |partial| updates.lock().unwrap().push(partial)
                })),
            },
        )
        .await;
    let missing = call
        .execute("missing", json!({}), NestedToolCallOptions::default())
        .await;

    assert_eq!(first.tool_call.id, "call/1");
    assert_eq!(missing.tool_call.id, "call/2");
    assert!(missing.is_error);
    assert_eq!(updates.lock().unwrap().len(), 1);
    let seen: Vec<_> = events
        .lock()
        .unwrap()
        .iter()
        .map(|event| match event {
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                parent_tool_call_id,
                ..
            }
            | AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                parent_tool_call_id,
                ..
            }
            | AgentEvent::ToolExecutionEnd {
                tool_call_id,
                parent_tool_call_id,
                ..
            } => (
                event.kind(),
                tool_call_id.clone(),
                parent_tool_call_id.clone(),
            ),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    let parent = Some("call".to_owned());
    assert_eq!(
        seen,
        [
            ("tool_execution_start", "call/1".into(), parent.clone()),
            ("tool_execution_update", "call/1".into(), parent.clone()),
            ("tool_execution_end", "call/1".into(), parent.clone()),
            ("tool_execution_start", "call/2".into(), parent.clone()),
            ("tool_execution_end", "call/2".into(), parent),
        ]
    );
    assert_eq!(
        call.take_summary().and_then(|summary| summary.calls),
        Some(NestedToolCalls {
            calls: vec![
                record("call/1", "echo", json!({ "a": 1 })),
                NestedToolCallRecord {
                    status: "error".into(),
                    error: Some("Tool missing not found".into()),
                    ..record("call/2", "missing", json!({}))
                },
            ],
            complete: true,
        })
    );
    // The record is taken once.
    assert_eq!(call.take_summary(), None);
    assert_eq!(runner.executor("other", None).take_summary(), None);
}

#[tokio::test]
async fn records_calls_of_nested_tools_on_the_model_issued_call() {
    let leaf = tool("leaf", |_id, _update, _ctx| empty());
    let middle = tool("middle", |_id, _update, ctx| {
        Box::pin(async move {
            ctx.nested()
                .execute("leaf", json!({}), NestedToolCallOptions::default())
                .await;
            Ok(AgentToolResult::default())
        })
    });
    let (runner, _) = create_runner(vec![leaf, middle], false);
    let call = runner.executor("call", None);

    call.execute("middle", json!({}), NestedToolCallOptions::default())
        .await;

    let ids: Vec<_> = call
        .take_summary()
        .and_then(|summary| summary.calls)
        .map(|calls| calls.calls.into_iter().map(|call| call.id).collect())
        .unwrap_or_default();
    assert_eq!(ids, ["call/1", "call/1/1"]);
}

#[tokio::test]
async fn sums_the_usage_of_nested_results_at_every_depth() {
    let leaf = tool("leaf", |_id, _update, _ctx| {
        Box::pin(async {
            Ok(AgentToolResult {
                usage: Some(usage(10, 0.01)),
                ..AgentToolResult::default()
            })
        })
    });
    let plain = tool("plain", |_id, _update, _ctx| empty());
    let middle = tool("middle", |_id, _update, ctx| {
        Box::pin(async move {
            ctx.nested()
                .execute("leaf", json!({}), NestedToolCallOptions::default())
                .await;
            // Its own usage only: the leaf's usage is counted once, by the
            // recorder.
            Ok(AgentToolResult {
                usage: Some(usage(5, 0.005)),
                ..AgentToolResult::default()
            })
        })
    });
    let (runner, _) = create_runner(vec![leaf, plain, middle], false);
    let call = runner.executor("call", None);
    let free = runner.executor("free", None);

    for name in ["middle", "leaf", "plain"] {
        call.execute(name, json!({}), NestedToolCallOptions::default())
            .await;
    }
    free.execute("plain", json!({}), NestedToolCallOptions::default())
        .await;

    let summary = call.take_summary().and_then(|summary| summary.usage);
    assert_eq!(summary.map(|usage| usage.input), Some(25));
    let cost = summary.map_or(0.0, |usage| usage.cost.total);
    assert!((cost - 0.025).abs() < 1e-10, "{cost}");
    let free = free.take_summary();
    assert_eq!(free.as_ref().map(|summary| summary.usage), Some(None));
    assert_eq!(
        free.and_then(|summary| summary.calls)
            .map(|calls| calls.complete),
        Some(true)
    );
}

#[tokio::test(start_paused = true)]
async fn serializes_concurrent_calls_to_sequential_tools() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active: Arc<Mutex<[usize; 2]>> = Arc::default();
    let make_tool = |name: &'static str, index: usize| {
        let active = Arc::clone(&active);
        let max_active = Arc::clone(&max_active);
        let mut tool = tool(name, move |_id, _update, _ctx| {
            let active = Arc::clone(&active);
            let max_active = Arc::clone(&max_active);
            Box::pin(async move {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                {
                    let mut max = max_active.lock().unwrap();
                    max[index] = max[index].max(now);
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(AgentToolResult::default())
            })
        });
        if name == "sequential" {
            tool.execution_mode = Some(ToolExecutionMode::Sequential);
        }
        tool
    };
    let (runner, _) = create_runner(
        vec![make_tool("sequential", 0), make_tool("parallel", 1)],
        false,
    );
    let call = runner.executor("call", None);
    let run = |name: &'static str| call.execute(name, json!({}), NestedToolCallOptions::default());

    tokio::join!(run("sequential"), run("sequential"), run("sequential"));
    tokio::join!(run("parallel"), run("parallel"), run("parallel"));

    assert_eq!(*max_active.lock().unwrap(), [1, 3]);
}

fn start(recorder: &mut NestedCallRecorder, id: &str, args: Value) -> Option<usize> {
    recorder.start(id, "t", &args)
}

#[tokio::test(start_paused = true)]
async fn recorder_omits_oversized_arguments_and_drops_calls_beyond_the_limit() {
    let mut recorder = NestedCallRecorder::new();
    assert_eq!(recorder.snapshot(), None);
    let small = start(&mut recorder, "a", json!({ "x": 1 }));
    recorder.finish(small, false, "");
    assert_eq!(
        recorder.snapshot(),
        Some(NestedToolCalls {
            calls: vec![record("a", "t", json!({ "x": 1 }))],
            complete: true,
        })
    );

    let big = start(
        &mut recorder,
        "b",
        json!({ "text": "x".repeat(NESTED_CALL_LIMITS.max_argument_bytes_per_call) }),
    );
    recorder.finish(big, true, &"e".repeat(1000));
    let snapshot = recorder.snapshot().unwrap();
    assert!(!snapshot.complete);
    assert_eq!(snapshot.calls[1].id, "b");
    assert_eq!(snapshot.calls[1].status, "error");
    assert_eq!(snapshot.calls[1].arguments, None);
    assert!(
        snapshot.calls[1].arguments_bytes.unwrap()
            > NESTED_CALL_LIMITS.max_argument_bytes_per_call as u64
    );
    assert_eq!(
        snapshot.calls[1].error.as_ref().map(|error| error.len()),
        Some(NESTED_CALL_LIMITS.max_error_chars)
    );

    for i in 0..NESTED_CALL_LIMITS.max_calls {
        let index = start(&mut recorder, &format!("c{i}"), json!({}));
        recorder.finish(index, false, "");
    }
    assert_eq!(
        recorder.snapshot().unwrap().calls.len(),
        NESTED_CALL_LIMITS.max_calls
    );
}

#[tokio::test]
async fn recorder_caps_the_total_argument_size_and_marks_unfinished_calls_incomplete() {
    let mut recorder = NestedCallRecorder::new();
    let chunk = json!({ "text": "x".repeat(7000) });
    let records: Vec<_> = (0..6)
        .map(|i| start(&mut recorder, &format!("c{i}"), chunk.clone()))
        .collect();
    let snapshot = recorder.snapshot().unwrap();
    // 32 KiB fits four 7000-byte argument objects.
    assert_eq!(
        snapshot
            .calls
            .iter()
            .filter(|call| call.arguments.is_some())
            .count(),
        4
    );
    assert!(
        snapshot
            .calls
            .iter()
            .all(|call| call.status == "unfinished")
    );
    assert!(!snapshot.complete);
    assert_eq!(records.len(), 6);
}

// Bake's cases.

#[tokio::test]
async fn non_object_arguments_fail_validation_and_keep_only_their_size() {
    let (runner, _) = create_runner(vec![tool("echo", |_id, _update, _ctx| empty())], false);
    let call = runner.executor("call", None);
    let outcome = call
        .execute("echo", json!("text"), NestedToolCallOptions::default())
        .await;
    assert!(outcome.is_error);
    assert!(
        matches!(&outcome.result.content[..], [UserContentBlock::Text(text)] if text.text.starts_with("Validation failed for tool \"echo\"")),
        "{:?}",
        outcome.result
    );
    let calls = call
        .take_summary()
        .and_then(|summary| summary.calls)
        .unwrap();
    assert_eq!(calls.calls[0].arguments, None);
    assert_eq!(calls.calls[0].arguments_bytes, Some(6));
    assert!(!calls.complete);
}

#[tokio::test]
async fn a_detached_executor_refuses_with_pis_text() {
    let outcome = ToolContext::detached()
        .nested()
        .execute("echo", json!({}), NestedToolCallOptions::default())
        .await;
    assert!(outcome.is_error);
    assert_eq!(outcome.tool_call.id, "/0");
    assert_eq!(
        outcome.result.content,
        [UserContentBlock::Text(bake_ai::TextContent::new(
            NESTED_CALLS_UNAVAILABLE
        ))]
    );
}

#[tokio::test(start_paused = true)]
async fn closing_a_caller_aborts_and_awaits_its_running_calls_and_refuses_new_ones() {
    let finished = Arc::new(AtomicUsize::new(0));
    // Honors its signal, as tools do, then cleans up, which the close must
    // wait for.
    let slow = AgentTool::new("slow", "slow", "slow", json!({ "type": "object" }), {
        let finished = Arc::clone(&finished);
        move |_id, _args, signal, _update, _ctx| {
            let finished = Arc::clone(&finished);
            Box::pin(async move {
                if let Some(signal) = signal {
                    signal.cancelled().await;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                finished.fetch_add(1, Ordering::SeqCst);
                Err("Operation aborted".to_owned())
            })
        }
    });
    let (runner, _) = create_runner(vec![slow], false);
    let call = runner.executor("call", None);
    // The caller let its call run on another task.
    let running = tokio::spawn({
        let call = call.clone();
        async move {
            call.execute("slow", json!({}), NestedToolCallOptions::default())
                .await
        }
    });
    tokio::task::yield_now().await;
    call.close().await;
    assert_eq!(finished.load(Ordering::SeqCst), 1);
    let outcome = running.await.unwrap();
    assert!(outcome.is_error);
    let later = call
        .execute("slow", json!({}), NestedToolCallOptions::default())
        .await;
    assert_eq!(later.tool_call.id, "call/0");
    assert_eq!(
        later.result.content,
        [UserContentBlock::Text(bake_ai::TextContent::new(
            NESTED_CALLER_FINISHED
        ))]
    );
}

/// A tool that waits for its signal, then ends as aborted.
fn waiting(name: &str) -> AgentTool {
    AgentTool::new(
        name,
        name,
        name,
        json!({ "type": "object" }),
        |_id, _args, signal, _update, _ctx| {
            Box::pin(async move {
                if let Some(signal) = signal {
                    signal.cancelled().await;
                }
                Err("Operation aborted".to_owned())
            })
        },
    )
}

/// Polls `future` once.
async fn poll_once<F: Future + Unpin>(future: &mut F) -> std::task::Poll<F::Output> {
    std::future::poll_fn(|cx| std::task::Poll::Ready(std::pin::Pin::new(&mut *future).poll(cx)))
        .await
}

fn text(outcome: &AgentToolCallOutcome) -> String {
    text_of(&outcome.result)
}

const WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// A tool may poll a call's future once and then hold it without polling,
/// as an interpreter's table of pending operations might. The call runs as
/// the scope's task all the same: closing the caller aborts it and returns,
/// the exclusive queue it held is free, and the held future resolves to the
/// aborted outcome.
#[tokio::test(start_paused = true)]
async fn a_call_held_unpolled_neither_blocks_close_nor_keeps_the_queue() {
    let (runner, _) = create_runner(
        vec![waiting("wait"), tool("quick", |_, _, _| empty())],
        true,
    );
    let call = runner.executor("call", None);
    let mut held = Box::pin(call.execute("wait", json!({}), NestedToolCallOptions::default()));
    assert!(poll_once(&mut held).await.is_pending());
    tokio::task::yield_now().await;

    tokio::time::timeout(WAIT, call.close())
        .await
        .expect("close returns");
    let other = runner.executor("other", None);
    let quick = tokio::time::timeout(
        WAIT,
        other.execute("quick", json!({}), NestedToolCallOptions::default()),
    )
    .await
    .expect("the queue is free");
    assert!(!quick.is_error, "{}", text(&quick));

    let held = held.await;
    assert_eq!(held.tool_call.id, "call/1");
    assert!(held.is_error);
    assert_eq!(text(&held), "Operation aborted");
}

/// A tool that ignores its signal is ended when the caller's run is dropped
/// mid-call, and later calls of that caller get no task.
#[tokio::test(start_paused = true)]
async fn dropping_the_abort_guard_ends_calls_that_ignore_their_signal() {
    let signals = Arc::new(Mutex::new(Vec::new()));
    let stuck = AgentTool::new("stuck", "stuck", "stuck", json!({ "type": "object" }), {
        let signals = Arc::clone(&signals);
        move |_id, _args, signal, _update, _ctx| {
            signals.lock().unwrap().extend(signal);
            Box::pin(std::future::pending())
        }
    });
    let (runner, _) = create_runner(vec![stuck], false);
    let call = runner.executor("call", None);
    let guard = call.abort_on_drop();
    let running = tokio::spawn({
        let call = call.clone();
        async move {
            call.execute("stuck", json!({}), NestedToolCallOptions::default())
                .await
        }
    });
    tokio::task::yield_now().await;
    drop(guard);
    let outcome = tokio::time::timeout(WAIT, running)
        .await
        .expect("the call ends")
        .unwrap();
    assert_eq!(outcome.tool_call.id, "call/1");
    assert_eq!(text(&outcome), "Operation aborted");
    // Work the tool handed off sees the abort.
    let signals = signals.lock().unwrap().clone();
    assert_eq!(signals.len(), 1);
    assert!(signals[0].aborted());
    let later = tokio::time::timeout(
        WAIT,
        call.execute("stuck", json!({}), NestedToolCallOptions::default()),
    )
    .await
    .expect("no task runs it");
    assert_eq!(later.tool_call.id, "call/2");
    assert_eq!(text(&later), "Operation aborted");
    // The record keeps both calls unfinished.
    let calls = call
        .take_summary()
        .and_then(|summary| summary.calls)
        .unwrap();
    assert!(!calls.complete);
}

/// An aborted call never reaches `before_tool_call`, where a session asks,
/// journals the dispatch, and runs hooks: not when its own signal or its
/// caller's aborted first, nor when its caller settles while it waits for
/// the exclusive queue.
#[tokio::test(start_paused = true)]
async fn an_aborted_call_does_not_reach_before_tool_call() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let before = crate::hook::before_tool_call({
        let seen = Arc::clone(&seen);
        move |ctx, _signal| {
            seen.lock().unwrap().push(ctx.tool_call.id.clone());
            Box::pin(async { Ok(None) })
        }
    });
    let (runner, _) = create_runner_with(
        vec![waiting("wait"), tool("quick", |_, _, _| empty())],
        true,
        Some(before),
    );
    let aborted = AbortController::new();
    aborted.abort();

    let own = runner.executor("own", None);
    let outcome = own
        .execute(
            "quick",
            json!({}),
            NestedToolCallOptions {
                signal: Some(aborted.signal()),
                on_update: None,
            },
        )
        .await;
    assert_eq!(text(&outcome), "Operation aborted");

    let caller = runner.executor("caller", Some(aborted.signal()));
    let outcome = caller
        .execute("quick", json!({}), NestedToolCallOptions::default())
        .await;
    assert_eq!(text(&outcome), "Operation aborted");

    // `holder` takes the queue; `queued` waits for it until its caller
    // settles.
    let holder = runner.executor("holder", None);
    let holding = tokio::spawn({
        let holder = holder.clone();
        async move {
            holder
                .execute("wait", json!({}), NestedToolCallOptions::default())
                .await
        }
    });
    tokio::task::yield_now().await;
    let queued = runner.executor("queued", None);
    let waiting = tokio::spawn({
        let queued = queued.clone();
        async move {
            queued
                .execute("quick", json!({}), NestedToolCallOptions::default())
                .await
        }
    });
    tokio::task::yield_now().await;
    tokio::time::timeout(WAIT, queued.close())
        .await
        .expect("close returns");
    assert_eq!(text(&waiting.await.unwrap()), "Operation aborted");
    holder.close().await;
    assert_eq!(text(&holding.await.unwrap()), "Operation aborted");

    assert_eq!(*seen.lock().unwrap(), ["holder/1"]);
}

/// A tool that calls itself goes no deeper than `MAX_NESTED_CALL_DEPTH`
/// levels; the call beyond that is refused.
#[tokio::test(start_paused = true)]
async fn nested_calls_stop_at_the_depth_limit() {
    let refusals = Arc::new(Mutex::new(Vec::new()));
    let recurse = tool("recurse", {
        let refusals = Arc::clone(&refusals);
        move |_id, _update, ctx| {
            let refusals = Arc::clone(&refusals);
            Box::pin(async move {
                let outcome = ctx
                    .nested()
                    .execute("recurse", json!({}), NestedToolCallOptions::default())
                    .await;
                if outcome.is_error {
                    refusals
                        .lock()
                        .unwrap()
                        .push((outcome.tool_call.id.clone(), text(&outcome)));
                }
                Ok(AgentToolResult::default())
            })
        }
    });
    let (runner, _) = create_runner(vec![recurse], false);
    let call = runner.executor("call", None);
    let outcome = call
        .execute("recurse", json!({}), NestedToolCallOptions::default())
        .await;
    assert!(!outcome.is_error);
    let deepest = format!("call{}", "/1".repeat(MAX_NESTED_CALL_DEPTH));
    assert_eq!(
        *refusals.lock().unwrap(),
        [(
            format!("{deepest}/0"),
            format!(
                "Nested tool calls are limited to {MAX_NESTED_CALL_DEPTH} levels below the model's tool call"
            )
        )]
    );
    let calls = call
        .take_summary()
        .and_then(|summary| summary.calls)
        .unwrap();
    assert_eq!(calls.calls.len(), MAX_NESTED_CALL_DEPTH);
    assert_eq!(
        calls.calls.last().map(|call| call.id.as_str()),
        Some(deepest.as_str())
    );
}

/// A tool whose `nested_callable` is cleared, as code mode's, cannot be
/// called by another tool, itself included, and is not listed.
#[tokio::test(start_paused = true)]
async fn a_tool_not_nested_callable_is_neither_listed_nor_run() {
    let ran = Arc::new(AtomicUsize::new(0));
    let mut script = tool("script", {
        let ran = Arc::clone(&ran);
        move |_id, _update, _ctx| {
            ran.fetch_add(1, Ordering::SeqCst);
            empty()
        }
    });
    script.nested_callable = false;
    let (runner, _) = create_runner(vec![script, tool("echo", |_, _, _| empty())], false);
    let call = runner.executor("call", None);
    let names: Vec<_> = call
        .tools()
        .await
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    assert_eq!(names, ["echo"]);
    let outcome = call
        .execute("script", json!({}), NestedToolCallOptions::default())
        .await;
    assert!(outcome.is_error);
    assert_eq!(text(&outcome), "Tool script not found");
    assert_eq!(ran.load(Ordering::SeqCst), 0);
}
