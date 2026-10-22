//! Bake: nested tool calls (`ToolContext::nested`) in the agent loop, which
//! hosts Pi's `NestedToolCallRunner` (`core/nested-tool-calls.ts`, v1.3.0).
//! Pi's own cases are in `src/nested_tests.rs`.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bake_agent::{
    AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, AgentTool, AgentToolResult,
    NestedToolCallOptions, ToolExecutionMode, agent_loop, hook,
};
use bake_ai::{AbortController, Message, Usage};
use common::*;
use serde_json::json;

fn config() -> AgentLoopConfig {
    AgentLoopConfig::new(model(), identity_converter())
}

fn context(tools: Vec<AgentTool>) -> AgentContext {
    AgentContext {
        messages: Vec::new(),
        tools: tools.into_iter().map(Arc::new).collect(),
    }
}

fn usage(input: u64) -> Usage {
    Usage {
        input,
        total_tokens: input,
        ..Usage::default()
    }
}

/// A tool whose execution is `run(ctx)`.
fn caller<F, Fut>(name: &str, run: F) -> AgentTool
where
    F: Fn(bake_agent::ToolContext, Option<bake_ai::AbortSignal>) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<AgentToolResult, String>> + Send + 'static,
{
    AgentTool::new(
        name,
        name,
        name,
        empty_schema(),
        move |_id, _args, signal, _update, ctx| Box::pin(run(ctx, signal)),
    )
}

fn parents(events: &[AgentEvent]) -> Vec<(&'static str, String, Option<String>)> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                parent_tool_call_id,
                ..
            }
            | AgentEvent::ToolExecutionEnd {
                tool_call_id,
                parent_tool_call_id,
                ..
            } => Some((
                event.kind(),
                tool_call_id.clone(),
                parent_tool_call_id.clone(),
            )),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn nested_calls_run_through_the_pipeline_and_are_recorded_on_the_caller() {
    let ran = Arc::new(Mutex::new(Vec::new()));
    let mut echo = echo_tool(Arc::clone(&ran));
    echo.execute = {
        let inner = Arc::clone(&echo.execute);
        Arc::new(move |id, args, signal, update, ctx| {
            let inner = Arc::clone(&inner);
            Box::pin(async move {
                let mut result = inner(id, args, signal, update, ctx).await?;
                result.usage = Some(usage(10));
                Ok(result)
            })
        })
    };
    let tool_names = Arc::new(Mutex::new(Vec::new()));
    let names = Arc::clone(&tool_names);
    let script = caller("script", move |ctx, _signal| {
        let names = Arc::clone(&names);
        async move {
            *names.lock().unwrap() = ctx
                .nested()
                .tools()
                .await
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>();
            let first = ctx
                .nested()
                .execute(
                    "echo",
                    json!({ "value": "a" }),
                    NestedToolCallOptions::default(),
                )
                .await;
            let invalid = ctx
                .nested()
                .execute("echo", json!({}), NestedToolCallOptions::default())
                .await;
            assert!(invalid.is_error);
            Ok(AgentToolResult {
                usage: Some(usage(1)),
                ..AgentToolResult::text(result_text(&first.result))
            })
        }
    });
    let hooks_seen = Arc::new(Mutex::new(Vec::new()));
    let mut config = config();
    let seen = Arc::clone(&hooks_seen);
    config.before_tool_call = Some(hook::before_tool_call(move |ctx, _| {
        seen.lock().unwrap().push((
            "before",
            ctx.tool_call.id.clone(),
            ctx.parent_tool_call_id.map(str::to_owned),
        ));
        Box::pin(async { Ok(None) })
    }));
    let seen = Arc::clone(&hooks_seen);
    config.after_tool_call = Some(hook::after_tool_call(move |ctx, _| {
        seen.lock().unwrap().push((
            "after",
            ctx.tool_call.id.clone(),
            ctx.parent_tool_call_id.map(str::to_owned),
        ));
        Box::pin(async { Ok(None) })
    }));
    let transcripts: Arc<Mutex<Vec<Vec<Message>>>> = Arc::default();
    let record = Arc::clone(&transcripts);
    let stream_fn = scripted(move |index, context, _| {
        record.lock().unwrap().push(context.messages().to_vec());
        if index == 0 {
            tool_use(vec![call("c1", "script", json!({}))])
        } else {
            assistant_text("done")
        }
    });
    let stream = agent_loop(
        vec![user("go")],
        context(vec![echo, script]),
        config,
        None,
        Some(stream_fn),
    );
    let events = drain(&stream).await;

    assert_eq!(*ran.lock().unwrap(), ["a"]);
    assert_eq!(*tool_names.lock().unwrap(), ["echo", "script"]);
    let c1 = Some("c1".to_owned());
    assert_eq!(
        parents(&events),
        [
            ("tool_execution_start", "c1".into(), None),
            ("tool_execution_start", "c1/1".into(), c1.clone()),
            ("tool_execution_end", "c1/1".into(), c1.clone()),
            ("tool_execution_start", "c1/2".into(), c1.clone()),
            ("tool_execution_end", "c1/2".into(), c1.clone()),
            ("tool_execution_end", "c1".into(), None),
        ]
    );
    // Invalid arguments fail before the hooks, as for a model-issued call.
    assert_eq!(
        *hooks_seen.lock().unwrap(),
        [
            ("before", "c1".into(), None),
            ("before", "c1/1".into(), c1.clone()),
            ("after", "c1/1".into(), c1.clone()),
            ("after", "c1".into(), None),
        ]
    );
    let messages = stream.result().await.unwrap();
    let results: Vec<_> = messages
        .iter()
        .filter_map(AgentMessage::as_tool_result)
        .collect();
    assert_eq!(
        results.len(),
        1,
        "nested results stay out of the transcript"
    );
    let result = results[0];
    assert_eq!(result_text_of(&result.content), "echoed: a");
    let nested = result.nested_calls.as_ref().unwrap();
    assert!(nested.complete);
    let ids: Vec<_> = nested
        .calls
        .iter()
        .map(|call| (call.id.as_str(), call.status.as_str()))
        .collect();
    assert_eq!(ids, [("c1/1", "ok"), ("c1/2", "error")]);
    assert!(
        nested.calls[1]
            .error
            .as_deref()
            .unwrap()
            .starts_with("Validation failed for tool \"echo\"")
    );
    // The caller's own usage plus its nested calls'.
    assert_eq!(result.usage.map(|usage| usage.input), Some(11));
    // The next request sees the caller's result only.
    let transcripts = transcripts.lock().unwrap();
    let tool_results: Vec<_> = transcripts[1]
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult(result) => Some(result.tool_call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(tool_results, ["c1"]);
}

fn result_text_of(content: &[bake_ai::UserContentBlock]) -> String {
    result_text(&AgentToolResult {
        content: content.to_vec(),
        ..AgentToolResult::default()
    })
}

/// Nothing changes for a tool that makes no nested call: its result
/// records none and keeps its own usage.
#[tokio::test]
async fn a_call_without_nested_calls_records_none() {
    let ran = Arc::new(Mutex::new(Vec::new()));
    let stream = agent_loop(
        vec![user("go")],
        context(vec![echo_tool(ran)]),
        config(),
        None,
        Some(tools_then_done(vec![call(
            "c1",
            "echo",
            json!({ "value": "a" }),
        )])),
    );
    drain(&stream).await;
    let messages = stream.result().await.unwrap();
    let result = messages
        .iter()
        .find_map(AgentMessage::as_tool_result)
        .unwrap();
    assert_eq!(result.nested_calls, None);
    assert_eq!(result.usage, None);
    let json = serde_json::to_value(result).unwrap();
    assert!(json.get("nestedCalls").is_none(), "{json}");
}

/// Records the most calls of `name` that ran at once.
#[derive(Default)]
struct Concurrency {
    active: AtomicUsize,
    max: AtomicUsize,
}

fn slow(name: &str, concurrency: Arc<Concurrency>) -> AgentTool {
    AgentTool::new(
        name,
        name,
        name,
        empty_schema(),
        move |_id, _args, _signal, _update, _ctx| {
            let concurrency = Arc::clone(&concurrency);
            Box::pin(async move {
                let now = concurrency.active.fetch_add(1, Ordering::SeqCst) + 1;
                concurrency.max.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(5)).await;
                concurrency.active.fetch_sub(1, Ordering::SeqCst);
                Ok(AgentToolResult::default())
            })
        },
    )
}

/// `script` runs `slow` twice and `middle` (which runs `slow` itself)
/// concurrently; returns the most `slow` calls that ran at once.
async fn concurrent_nested_calls(mode: ToolExecutionMode) -> (usize, Vec<String>) {
    let concurrency = Arc::new(Concurrency::default());
    let middle = caller("middle", |ctx, _| async move {
        let outcome = ctx
            .nested()
            .execute("slow", json!({}), NestedToolCallOptions::default())
            .await;
        Ok(AgentToolResult::text(outcome.tool_call.id))
    });
    let script = caller("script", |ctx, _| async move {
        let run = |name: &'static str| {
            let ctx = ctx.clone();
            async move {
                ctx.nested()
                    .execute(name, json!({}), NestedToolCallOptions::default())
                    .await
            }
        };
        let (a, b, c) = tokio::join!(run("slow"), run("slow"), run("middle"));
        assert!(!a.is_error && !b.is_error && !c.is_error);
        Ok(AgentToolResult::default())
    });
    let mut config = config();
    config.tool_execution = mode;
    let stream = agent_loop(
        vec![user("go")],
        context(vec![slow("slow", Arc::clone(&concurrency)), middle, script]),
        config,
        None,
        Some(tools_then_done(vec![call("c1", "script", json!({}))])),
    );
    let events = tokio::time::timeout(Duration::from_secs(10), drain(&stream))
        .await
        .expect("no deadlock");
    let messages = stream.result().await.unwrap();
    let mut ids: Vec<String> = messages
        .iter()
        .filter_map(AgentMessage::as_tool_result)
        .flat_map(|result| result.nested_calls.clone().unwrap().calls)
        .map(|call| call.id)
        .collect();
    ids.sort();
    assert!(tool_end_ids(&events).contains(&"c1".to_owned()));
    (concurrency.max.load(Ordering::SeqCst), ids)
}

/// Pi's exclusive queue: under sequential execution nested calls run one at
/// a time, and a call holding the queue (`middle`) does not wait on its own
/// nested call.
#[tokio::test(start_paused = true)]
async fn sequential_execution_runs_nested_calls_one_at_a_time_without_deadlock() {
    let (max, ids) = concurrent_nested_calls(ToolExecutionMode::Sequential).await;
    assert_eq!(max, 1);
    assert_eq!(ids, ["c1/1", "c1/2", "c1/3", "c1/3/1"]);
}

#[tokio::test(start_paused = true)]
async fn parallel_execution_runs_nested_calls_concurrently() {
    let (max, ids) = concurrent_nested_calls(ToolExecutionMode::Parallel).await;
    assert_eq!(max, 3);
    assert_eq!(ids, ["c1/1", "c1/2", "c1/3", "c1/3/1"]);
}

/// Aborting the run cancels a nested call the caller left running on
/// another task, and the caller's result waits for it to settle.
#[tokio::test(start_paused = true)]
async fn aborting_the_caller_cancels_and_awaits_its_nested_calls() {
    let settled = Arc::new(AtomicUsize::new(0));
    let wait = AgentTool::new("wait", "wait", "wait", empty_schema(), {
        let settled = Arc::clone(&settled);
        move |_id, _args, signal, _update, _ctx| {
            let settled = Arc::clone(&settled);
            Box::pin(async move {
                if let Some(signal) = signal {
                    signal.cancelled().await;
                }
                // Cleanup after the abort, which the caller's result waits for.
                tokio::time::sleep(Duration::from_millis(50)).await;
                settled.fetch_add(1, Ordering::SeqCst);
                Err("Operation aborted".to_owned())
            })
        }
    });
    let started = Arc::new(tokio::sync::Notify::new());
    let script = caller("script", {
        let started = Arc::clone(&started);
        move |ctx, signal| {
            let started = Arc::clone(&started);
            async move {
                // Left running on its own task, as a script engine's thread
                // might leave it.
                let nested = ctx.clone();
                drop(tokio::spawn(async move {
                    nested
                        .nested()
                        .execute("wait", json!({}), NestedToolCallOptions::default())
                        .await
                }));
                started.notify_one();
                if let Some(signal) = signal {
                    signal.cancelled().await;
                }
                Err("Operation aborted".to_owned())
            }
        }
    });
    let controller = AbortController::new();
    let observed = Arc::clone(&settled);
    let at_end = Arc::new(Mutex::new(None));
    let record = Arc::clone(&at_end);
    let stream = agent_loop(
        vec![user("go")],
        context(vec![wait, script]),
        config(),
        Some(controller.signal()),
        Some(tools_then_done(vec![call("c1", "script", json!({}))])),
    );
    started.notified().await;
    tokio::task::yield_now().await;
    controller.abort();
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        if matches!(&event, AgentEvent::ToolExecutionEnd { tool_call_id, .. } if tool_call_id == "c1")
        {
            *record.lock().unwrap() = Some(observed.load(Ordering::SeqCst));
        }
        events.push(event);
    }
    assert_eq!(*at_end.lock().unwrap(), Some(1));
    let c1 = Some("c1".to_owned());
    assert_eq!(
        parents(&events),
        [
            ("tool_execution_start", "c1".into(), None),
            ("tool_execution_start", "c1/1".into(), c1.clone()),
            ("tool_execution_end", "c1/1".into(), c1),
            ("tool_execution_end", "c1".into(), None),
        ]
    );
    let messages = stream.result().await.unwrap();
    let result = messages
        .iter()
        .find_map(AgentMessage::as_tool_result)
        .unwrap();
    let nested = result.nested_calls.as_ref().unwrap();
    assert_eq!(nested.calls[0].status, "error");
    assert!(nested.complete);
}

/// A sink that keeps its events in order as the agent's does, with a lock
/// held while each is handled, and that takes `delay` over a nested call's
/// start.
fn ordered_sink(delay: Duration) -> (bake_agent::AgentEventSink, Arc<Mutex<Vec<AgentEvent>>>) {
    let order = Arc::new(tokio::sync::Mutex::new(()));
    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::default();
    let record = Arc::clone(&events);
    let sink: bake_agent::AgentEventSink = Arc::new(move |event| {
        let order = Arc::clone(&order);
        let record = Arc::clone(&record);
        Box::pin(async move {
            let _order = order.lock().await;
            if matches!(
                &event,
                AgentEvent::ToolExecutionStart {
                    parent_tool_call_id: Some(_),
                    ..
                }
            ) {
                tokio::time::sleep(delay).await;
            }
            record.lock().unwrap().push(event);
        })
    });
    (sink, events)
}

/// The caller settles while its update waits behind a nested call's event
/// in the sink's order, and that nested call, left running on another
/// task, still has events to emit. The update is emitted before the nested
/// calls are closed, so neither waits on the other.
#[tokio::test(start_paused = true)]
async fn a_pending_caller_update_does_not_block_closing_its_nested_calls() {
    let script = AgentTool::new(
        "script",
        "script",
        "script",
        empty_schema(),
        |_id, _args, _signal, update, ctx| {
            Box::pin(async move {
                drop(tokio::spawn(async move {
                    ctx.nested()
                        .execute(
                            "echo",
                            json!({ "value": "a" }),
                            NestedToolCallOptions::default(),
                        )
                        .await
                }));
                // The nested call's start now holds the sink.
                tokio::time::sleep(Duration::from_millis(5)).await;
                update.update(AgentToolResult::text("partial"));
                // The loop starts emitting the update, which queues.
                tokio::time::sleep(Duration::from_millis(1)).await;
                Ok(AgentToolResult::text("done"))
            })
        },
    );
    let (sink, events) = ordered_sink(Duration::from_millis(20));
    let run = bake_agent::run_agent_loop(
        vec![user("go")],
        context(vec![echo_tool(Arc::default()), script]),
        config(),
        sink,
        None,
        Some(tools_then_done(vec![call("c1", "script", json!({}))])),
    );
    tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .expect("no deadlock")
        .unwrap();
    let events = events.lock().unwrap();
    let c1 = Some("c1".to_owned());
    assert_eq!(
        parents(&events),
        [
            ("tool_execution_start", "c1".into(), None),
            ("tool_execution_start", "c1/1".into(), c1.clone()),
            ("tool_execution_end", "c1/1".into(), c1),
            ("tool_execution_end", "c1".into(), None),
        ]
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolExecutionUpdate { tool_call_id, .. } if tool_call_id == "c1"))
    );
}

/// With `max_concurrent_read_tools`, a nested call to a tool that is not
/// read-only runs exclusively, one at a time, as the loop runs such a
/// batch; read-only nested calls run concurrently, beside them and without
/// the read limit, which caps the model's batches only.
#[tokio::test(start_paused = true)]
async fn nested_calls_under_max_concurrent_read_tools() {
    let reads = Arc::new(Concurrency::default());
    let writes = Arc::new(Concurrency::default());
    let mut read = slow("read", Arc::clone(&reads));
    read.read_only = true;
    let write = slow("write", Arc::clone(&writes));
    let script = caller("script", |ctx, _| async move {
        let run = |name: &'static str| {
            let ctx = ctx.clone();
            async move {
                ctx.nested()
                    .execute(name, json!({}), NestedToolCallOptions::default())
                    .await
            }
        };
        let outcomes = tokio::join!(
            run("read"),
            run("write"),
            run("read"),
            run("write"),
            run("read")
        );
        assert!(
            [outcomes.0, outcomes.1, outcomes.2, outcomes.3, outcomes.4]
                .iter()
                .all(|outcome| !outcome.is_error)
        );
        Ok(AgentToolResult::default())
    });
    let mut config = config();
    config.max_concurrent_read_tools = std::num::NonZeroUsize::new(1);
    let stream = agent_loop(
        vec![user("go")],
        context(vec![read, write, script]),
        config,
        None,
        Some(tools_then_done(vec![call("c1", "script", json!({}))])),
    );
    tokio::time::timeout(Duration::from_secs(10), drain(&stream))
        .await
        .expect("no deadlock");
    let messages = stream.result().await.unwrap();
    let nested = messages
        .iter()
        .find_map(AgentMessage::as_tool_result)
        .and_then(|result| result.nested_calls.clone())
        .unwrap();
    assert_eq!(nested.calls.len(), 5);
    assert!(nested.calls.iter().all(|call| call.status == "ok"));
    assert_eq!(writes.max.load(Ordering::SeqCst), 1);
    assert_eq!(reads.max.load(Ordering::SeqCst), 3);
}
