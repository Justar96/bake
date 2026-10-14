//! Ports of Pi `packages/agent/test/agent-loop.test.ts` (v1.1.0). Each test
//! names the Pi test it follows; tests marked "Bake" have no Pi counterpart.

mod common;

use std::any::Any;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bake_agent::{
    AfterToolCallResult, AgentContext, AgentEvent, AgentLoopConfig, AgentLoopError,
    AgentLoopTurnUpdate, AgentMessage, AgentRequestUpdate, AgentTool, AgentToolResult,
    AgentToolUpdateCallback, AgentTurnDecision, BeforeToolCallResult, CustomAgentMessage,
    RunToolCallOptions, ToolExecutionMode, agent_loop, agent_loop_continue, hook, run_agent_loop,
    run_tool_call, set_default_stream_fn,
};
use bake_ai::providers::faux::{
    FauxProvider, FauxProviderOptions, FauxResponseStep, faux_assistant_blocks, faux_text,
    faux_tool_call,
};
use bake_ai::{
    AbortController, ApiRegistry, AssistantMessageEvent, Message, ModelThinkingLevel, StopReason,
    SystemContent, SystemMessage, TextContent, ThinkingLevel, Usage, UsageCost, UserContent,
    UserContentBlock, UserMessage,
};
use common::*;
use serde_json::{Value, json};
use tokio::sync::Notify;

fn config() -> AgentLoopConfig {
    AgentLoopConfig::new(model(), identity_converter())
}

fn context(tools: Vec<AgentTool>) -> AgentContext {
    AgentContext {
        messages: Vec::new(),
        tools: tools.into_iter().map(Arc::new).collect(),
    }
}

fn executed() -> Arc<Mutex<Vec<String>>> {
    Arc::new(Mutex::new(Vec::new()))
}

/// Pi: "uses the configured default when a legacy caller omits streamFn".
#[tokio::test]
async fn uses_the_configured_default_stream_fn() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    set_default_stream_fn(Some(scripted(move |_, _, _| {
        counter.fetch_add(1, Ordering::SeqCst);
        assistant_text("fallback")
    })));
    let stream = agent_loop(vec![user("Hello")], context(vec![]), config(), None, None);
    let messages = stream.result().await;
    set_default_stream_fn(None);
    assert!(messages.is_some());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Pi: "should emit events with AgentMessage types".
#[tokio::test]
async fn emits_events_with_agent_message_types() {
    let stream = agent_loop(
        vec![user("Hello")],
        context(vec![]),
        config(),
        None,
        Some(scripted(|_, _, _| assistant_text("Hi there!"))),
    );
    let events = drain(&stream).await;
    let messages = stream.result().await.unwrap();
    assert_eq!(roles(&messages), ["user", "assistant"]);
    assert_eq!(
        kinds(&events),
        [
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );
}

/// Pi: "should build provider context exclusively from transcript messages".
#[tokio::test]
async fn builds_provider_context_from_transcript_messages_only() {
    let initial = SystemMessage {
        content: SystemContent::Text("Transcript prompt".to_owned()),
        tools_added: Some(Vec::new()),
        timestamp: 1,
        ..SystemMessage::default()
    };
    let expected = Message::System(initial.clone());
    let seen = Arc::new(Mutex::new(None));
    let record = Arc::clone(&seen);
    let stream = agent_loop(
        vec![initial.into(), user("Hello")],
        context(vec![]),
        config(),
        None,
        Some(scripted(move |_, context, _| {
            *record.lock().unwrap() = context.messages().first().cloned();
            assistant_text("done")
        })),
    );
    stream.result().await.unwrap();
    assert_eq!(seen.lock().unwrap().clone(), Some(expected));
}

#[derive(Debug)]
struct Notification {
    text: String,
    timestamp: i64,
}

impl CustomAgentMessage for Notification {
    fn role(&self) -> &str {
        "notification"
    }
    fn timestamp(&self) -> i64 {
        self.timestamp
    }
    fn to_json(&self) -> Value {
        json!({ "role": "notification", "text": self.text, "timestamp": self.timestamp })
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Pi: "should handle custom message types via convertToLlm".
#[tokio::test]
async fn converts_custom_messages_through_convert_to_llm() {
    let notification: AgentMessage = AgentMessage::Custom(Arc::new(Notification {
        text: "This is a notification".to_owned(),
        timestamp: 1,
    }));
    let converted = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&converted);
    let mut config = config();
    config.convert_to_llm = hook::convert_to_llm(move |messages| {
        let out: Vec<Message> = messages
            .iter()
            .filter(|m| m.role() != "notification")
            .filter_map(|m| m.as_llm().cloned())
            .collect();
        *record.lock().unwrap() = out.clone();
        Box::pin(async move { out })
    });
    let context = AgentContext {
        messages: vec![notification],
        tools: Vec::new(),
    };
    let stream = agent_loop(
        vec![user("Hello")],
        context,
        config,
        None,
        Some(scripted(|_, _, _| assistant_text("Response"))),
    );
    drain(&stream).await;
    let converted = converted.lock().unwrap();
    assert_eq!(converted.len(), 1);
    assert_eq!(converted[0].role(), "user");
}

/// Pi: "should apply transformContext before convertToLlm".
#[tokio::test]
async fn applies_transform_context_before_convert_to_llm() {
    let transformed = Arc::new(AtomicUsize::new(0));
    let converted = Arc::new(AtomicUsize::new(0));
    let mut config = config();
    let t = Arc::clone(&transformed);
    config.transform_context = Some(hook::transform_context(move |messages, _signal| {
        let kept: Vec<AgentMessage> = messages[messages.len().saturating_sub(2)..].to_vec();
        t.store(kept.len(), Ordering::SeqCst);
        Box::pin(async move { kept })
    }));
    let c = Arc::clone(&converted);
    config.convert_to_llm = hook::convert_to_llm(move |messages| {
        let out: Vec<Message> = messages
            .iter()
            .filter(|m| matches!(m.role(), "user" | "assistant" | "toolResult"))
            .filter_map(|m| m.as_llm().cloned())
            .collect();
        c.store(out.len(), Ordering::SeqCst);
        Box::pin(async move { out })
    });
    let context = AgentContext {
        messages: vec![
            user("old message 1"),
            assistant_text("old response 1").into(),
            user("old message 2"),
            assistant_text("old response 2").into(),
        ],
        tools: Vec::new(),
    };
    let stream = agent_loop(
        vec![user("new message")],
        context,
        config,
        None,
        Some(scripted(|_, _, _| assistant_text("Response"))),
    );
    drain(&stream).await;
    assert_eq!(transformed.load(Ordering::SeqCst), 2);
    assert_eq!(converted.load(Ordering::SeqCst), 2);
}

fn usage(base: u64, total: f64) -> Usage {
    let b = base as f64 / 10.0;
    Usage {
        input: base,
        output: base + 1,
        cache_read: base + 2,
        cache_write: base + 3,
        total_tokens: 4 * base + 6,
        cost: UsageCost {
            input: b,
            output: b + 0.1,
            cache_read: b + 0.2,
            cache_write: b + 0.3,
            total,
        },
        ..Usage::default()
    }
}

/// Pi: "should handle tool calls and results".
#[tokio::test]
async fn handles_tool_calls_and_results() {
    let tool_usage = usage(1, 1.0);
    let patched_usage = usage(5, 2.6);
    let ran = executed();
    let record = Arc::clone(&ran);
    let tool = AgentTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        move |_, params, _, _| {
            let value = params["value"].as_str().unwrap_or_default().to_owned();
            record.lock().unwrap().push(value.clone());
            Box::pin(async move {
                Ok(AgentToolResult {
                    details: Some(json!({ "value": value })),
                    usage: Some(tool_usage),
                    ..AgentToolResult::text(format!("echoed: {value}"))
                })
            })
        },
    );
    let observed = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&observed);
    let mut config = config();
    config.after_tool_call = Some(hook::after_tool_call(move |ctx, _signal| {
        *seen.lock().unwrap() = ctx.result.usage;
        Box::pin(async move {
            Ok(Some(AfterToolCallResult {
                usage: Some(patched_usage),
                ..AfterToolCallResult::default()
            }))
        })
    }));
    let stream = agent_loop(
        vec![user("echo something")],
        context(vec![tool]),
        config,
        None,
        Some(tools_then_done(vec![call(
            "tool-1",
            "echo",
            json!({ "value": "hello" }),
        )])),
    );
    let events = drain(&stream).await;
    assert_eq!(*ran.lock().unwrap(), ["hello"]);
    assert!(events.iter().any(|e| e.kind() == "tool_execution_start"));
    let end = events.iter().find_map(|e| match e {
        AgentEvent::ToolExecutionEnd { is_error, .. } => Some(*is_error),
        _ => None,
    });
    assert_eq!(end, Some(false));
    assert_eq!(*observed.lock().unwrap(), Some(tool_usage));
    let messages = stream.result().await.unwrap();
    let result = messages
        .iter()
        .find_map(AgentMessage::as_tool_result)
        .unwrap();
    assert_eq!(result.usage, Some(patched_usage));
}

/// Pi: "records how long execute() took on the tool result, excluding
/// hooks" (#10549).
#[tokio::test(start_paused = true)]
async fn records_execute_duration_excluding_hooks() {
    let tool = AgentTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        |_, params, _, _| {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(30)).await;
                Ok(AgentToolResult::text(
                    params["value"].as_str().unwrap_or_default(),
                ))
            })
        },
    );
    let mut config = config();
    config.before_tool_call = Some(hook::before_tool_call(|ctx, _signal| {
        let blocked = ctx.tool_call.id == "blocked";
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(blocked.then(|| BeforeToolCallResult {
                block: true,
                reason: Some("no".to_owned()),
                terminate: false,
            }))
        })
    }));
    let stream = agent_loop(
        vec![user("go")],
        context(vec![tool]),
        config,
        None,
        Some(tools_then_done(vec![
            call("ran", "echo", json!({ "value": "a" })),
            call("blocked", "echo", json!({ "value": "b" })),
        ])),
    );
    drain(&stream).await;
    let messages = stream.result().await.unwrap();
    let results: Vec<_> = messages
        .iter()
        .filter_map(AgentMessage::as_tool_result)
        .collect();
    assert_eq!(results[0].duration_ms, Some(30));
    assert!(results[1].is_error);
    assert_eq!(results[1].duration_ms, None);
    assert!(
        !serde_json::to_value(results[1])
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("durationMs")
    );
}

/// Pi: "should not execute tool calls from a length-truncated assistant
/// message".
#[tokio::test]
async fn fails_tool_calls_of_a_length_truncated_message() {
    let ran = executed();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let stream = agent_loop(
        vec![user("echo something")],
        context(vec![echo_tool(Arc::clone(&ran))]),
        config(),
        None,
        Some(scripted(move |index, _, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            if index == 0 {
                assistant(
                    vec![call("tool-1", "echo", json!({ "value": "hel" }))],
                    StopReason::Length,
                )
            } else {
                assistant_text("done")
            }
        })),
    );
    let events = drain(&stream).await;
    assert!(ran.lock().unwrap().is_empty());
    let (is_error, text) = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolExecutionEnd {
                is_error, result, ..
            } => Some((*is_error, result_text(result))),
            _ => None,
        })
        .unwrap();
    assert!(is_error);
    assert!(text.contains("output token limit"), "{text}");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let messages = stream.result().await.unwrap();
    assert_eq!(messages.last().unwrap().role(), "assistant");
}

/// Pi: "should execute mutated beforeToolCall args without revalidation".
#[tokio::test]
async fn executes_mutated_before_tool_call_args_without_revalidation() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let tool = AgentTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        move |_, params, _, _| {
            record.lock().unwrap().push(params["value"].clone());
            Box::pin(async { Ok(AgentToolResult::text("ok")) })
        },
    );
    let mut config = config();
    config.before_tool_call = Some(hook::before_tool_call(|ctx, _signal| {
        ctx.args["value"] = json!(123);
        Box::pin(async { Ok(None) })
    }));
    let stream = agent_loop(
        vec![user("echo something")],
        context(vec![tool]),
        config,
        None,
        Some(tools_then_done(vec![call(
            "tool-1",
            "echo",
            json!({ "value": "hello" }),
        )])),
    );
    drain(&stream).await;
    assert_eq!(*seen.lock().unwrap(), [json!(123)]);
}

/// Pi: "should prepare tool arguments for validation".
#[tokio::test]
async fn prepares_tool_arguments_for_validation() {
    let schema = json!({
        "type": "object",
        "properties": {
            "edits": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "oldText": { "type": "string" }, "newText": { "type": "string" } },
                    "required": ["oldText", "newText"],
                },
            },
        },
        "required": ["edits"],
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let mut tool = AgentTool::new(
        "edit",
        "Edit",
        "Edit tool",
        schema,
        move |_, params, _, _| {
            record.lock().unwrap().push(params["edits"].clone());
            Box::pin(async { Ok(AgentToolResult::text("edited")) })
        },
    );
    tool.prepare_arguments = Some(Arc::new(|args: Value| {
        let (Some(old), Some(new)) = (args["oldText"].as_str(), args["newText"].as_str()) else {
            return Ok(args);
        };
        let mut edits = args["edits"].as_array().cloned().unwrap_or_default();
        edits.push(json!({ "oldText": old, "newText": new }));
        Ok(json!({ "edits": edits }))
    }));
    let stream = agent_loop(
        vec![user("edit something")],
        context(vec![tool]),
        config(),
        None,
        Some(tools_then_done(vec![call(
            "tool-1",
            "edit",
            json!({ "oldText": "before", "newText": "after" }),
        )])),
    );
    drain(&stream).await;
    assert_eq!(
        *seen.lock().unwrap(),
        [json!([{ "oldText": "before", "newText": "after" }])]
    );
}

/// A tool whose `first` call waits for `release` and records whether
/// `second` ran meanwhile.
fn gated_tool(name: &str, release: Arc<Notify>, overlap: Arc<AtomicBool>) -> AgentTool {
    let first_done = Arc::new(AtomicBool::new(false));
    AgentTool::new(
        name,
        name,
        "Gated tool",
        value_schema(),
        move |_, params, _, _| {
            let value = params["value"].as_str().unwrap_or_default().to_owned();
            let release = Arc::clone(&release);
            let overlap = Arc::clone(&overlap);
            let first_done = Arc::clone(&first_done);
            Box::pin(async move {
                if value == "first" {
                    release.notified().await;
                    first_done.store(true, Ordering::SeqCst);
                }
                if value == "second" && !first_done.load(Ordering::SeqCst) {
                    overlap.store(true, Ordering::SeqCst);
                }
                Ok(AgentToolResult::text(format!("echoed: {value}")))
            })
        },
    )
}

/// Releases `notify` 20 ms after the first request, as Pi's
/// `setTimeout(releaseFirst, 20)`.
fn release_later(notify: &Arc<Notify>) -> impl Fn() + Send + Sync + 'static {
    let notify = Arc::clone(notify);
    move || {
        let notify = Arc::clone(&notify);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            notify.notify_one();
        });
    }
}

fn first_second_then_done(
    release: impl Fn() + Send + Sync + 'static,
    name: &'static str,
) -> bake_agent::StreamFn {
    scripted(move |index, _, _| {
        if index == 0 {
            release();
            tool_use(vec![
                call("tool-1", name, json!({ "value": "first" })),
                call("tool-2", name, json!({ "value": "second" })),
            ])
        } else {
            assistant_text("done")
        }
    })
}

/// Pi: "should emit tool_execution_end in completion order but persist tool
/// results in source order". The parallel-tool ordering test.
#[tokio::test(start_paused = true)]
async fn parallel_tools_end_in_completion_order_and_persist_in_source_order() {
    let release = Arc::new(Notify::new());
    let overlap = Arc::new(AtomicBool::new(false));
    let tool = gated_tool("echo", Arc::clone(&release), Arc::clone(&overlap));
    let mut config = config();
    config.tool_execution = ToolExecutionMode::Parallel;
    let stream = agent_loop(
        vec![user("echo both")],
        context(vec![tool]),
        config,
        None,
        Some(first_second_then_done(release_later(&release), "echo")),
    );
    let events = drain(&stream).await;
    let turn_results: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TurnEnd { tool_results, .. } if !tool_results.is_empty() => Some(
                tool_results
                    .iter()
                    .map(|r| r.tool_call_id.clone())
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(overlap.load(Ordering::SeqCst));
    assert_eq!(tool_end_ids(&events), ["tool-2", "tool-1"]);
    assert_eq!(tool_result_ids(&events), ["tool-1", "tool-2"]);
    assert_eq!(turn_results, ["tool-1", "tool-2"]);
}

/// Pi: "should inject queued messages after all tool calls complete".
#[tokio::test]
async fn injects_steering_after_all_tool_calls_complete() {
    let ran = executed();
    let delivered = Arc::new(AtomicBool::new(false));
    let saw_interrupt = Arc::new(AtomicBool::new(false));
    let mut config = config();
    config.tool_execution = ToolExecutionMode::Sequential;
    let (r, d) = (Arc::clone(&ran), Arc::clone(&delivered));
    config.get_steering_messages = Some(hook::get_messages(move || {
        let messages = if !r.lock().unwrap().is_empty() && !d.swap(true, Ordering::SeqCst) {
            vec![user("interrupt")]
        } else {
            Vec::new()
        };
        Box::pin(async move { messages })
    }));
    let saw = Arc::clone(&saw_interrupt);
    let stream = agent_loop(
        vec![user("start")],
        context(vec![echo_tool(Arc::clone(&ran))]),
        config,
        None,
        Some(scripted(move |index, context, _| {
            if index == 1 {
                saw.store(
                    request_users(context).contains(&"interrupt".to_owned()),
                    Ordering::SeqCst,
                );
            }
            if index == 0 {
                tool_use(vec![
                    call("tool-1", "echo", json!({ "value": "first" })),
                    call("tool-2", "echo", json!({ "value": "second" })),
                ])
            } else {
                assistant_text("done")
            }
        })),
    );
    let events = drain(&stream).await;
    assert_eq!(*ran.lock().unwrap(), ["first", "second"]);
    let ends: Vec<bool> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolExecutionEnd { is_error, .. } => Some(*is_error),
            _ => None,
        })
        .collect();
    assert_eq!(ends, [false, false]);
    let sequence: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageStart { message } => match message {
                AgentMessage::Llm(Message::ToolResult(result)) => {
                    Some(format!("tool:{}", result.tool_call_id))
                }
                AgentMessage::Llm(message) => user_text(message).map(str::to_owned),
                AgentMessage::Custom(_) => None,
            },
            _ => None,
        })
        .collect();
    let position = |item: &str| sequence.iter().position(|s| s == item).unwrap();
    assert!(position("tool:tool-1") < position("interrupt"));
    assert!(position("tool:tool-2") < position("interrupt"));
    assert!(saw_interrupt.load(Ordering::SeqCst));
}

/// Pi: "should force sequential execution when a tool has
/// executionMode=sequential even with default parallel config".
#[tokio::test(start_paused = true)]
async fn a_sequential_tool_forces_sequential_execution() {
    let release = Arc::new(Notify::new());
    let overlap = Arc::new(AtomicBool::new(false));
    let mut tool = gated_tool("slow", Arc::clone(&release), Arc::clone(&overlap));
    tool.execution_mode = Some(ToolExecutionMode::Sequential);
    let stream = agent_loop(
        vec![user("run both")],
        context(vec![tool]),
        config(),
        None,
        Some(first_second_then_done(release_later(&release), "slow")),
    );
    let events = drain(&stream).await;
    assert!(!overlap.load(Ordering::SeqCst));
    assert_eq!(tool_result_ids(&events), ["tool-1", "tool-2"]);
}

/// Pi: "should force sequential execution when one of multiple tools has
/// executionMode=sequential".
#[tokio::test(start_paused = true)]
async fn one_sequential_tool_among_several_forces_sequential_execution() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let release = Arc::new(Notify::new());
    let (o, r) = (Arc::clone(&order), Arc::clone(&release));
    let mut slow = AgentTool::new(
        "slow",
        "Slow",
        "Slow tool",
        value_schema(),
        move |_, params, _, _| {
            let value = params["value"].as_str().unwrap_or_default().to_owned();
            o.lock().unwrap().push(format!("slow:{value}"));
            let r = Arc::clone(&r);
            Box::pin(async move {
                if value == "a" {
                    r.notified().await;
                }
                Ok(AgentToolResult::text("slow"))
            })
        },
    );
    slow.execution_mode = Some(ToolExecutionMode::Sequential);
    let o = Arc::clone(&order);
    let fast = AgentTool::new(
        "fast",
        "Fast",
        "Fast tool",
        value_schema(),
        move |_, params, _, _| {
            o.lock().unwrap().push(format!(
                "fast:{}",
                params["value"].as_str().unwrap_or_default()
            ));
            Box::pin(async { Ok(AgentToolResult::text("fast")) })
        },
    );
    let releaser = release_later(&release);
    let stream = agent_loop(
        vec![user("run both")],
        context(vec![slow, fast]),
        config(),
        None,
        Some(scripted(move |index, _, _| {
            if index == 0 {
                releaser();
                tool_use(vec![
                    call("tool-1", "slow", json!({ "value": "a" })),
                    call("tool-2", "fast", json!({ "value": "b" })),
                ])
            } else {
                assistant_text("done")
            }
        })),
    );
    drain(&stream).await;
    assert_eq!(*order.lock().unwrap(), ["slow:a", "fast:b"]);
}

/// Pi: "should allow parallel execution when all tools have
/// executionMode=parallel".
#[tokio::test(start_paused = true)]
async fn parallel_tools_overlap() {
    let release = Arc::new(Notify::new());
    let overlap = Arc::new(AtomicBool::new(false));
    let mut tool = gated_tool("echo", Arc::clone(&release), Arc::clone(&overlap));
    tool.execution_mode = Some(ToolExecutionMode::Parallel);
    let stream = agent_loop(
        vec![user("echo both")],
        context(vec![tool]),
        config(),
        None,
        Some(first_second_then_done(release_later(&release), "echo")),
    );
    drain(&stream).await;
    assert!(overlap.load(Ordering::SeqCst));
}

fn terminating_echo() -> AgentTool {
    AgentTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        |_, params, _, _| {
            let value = params["value"].as_str().unwrap_or_default().to_owned();
            Box::pin(async move {
                Ok(AgentToolResult {
                    terminate: true,
                    ..AgentToolResult::text(value)
                })
            })
        },
    )
}

fn noop_tool() -> AgentTool {
    AgentTool::new("noop", "Noop", "Noop tool", empty_schema(), |_, _, _, _| {
        Box::pin(async { Ok(AgentToolResult::text("done")) })
    })
}

/// Pi: "runs finishTurn after tool-result messages and before turn_end".
#[tokio::test]
async fn finish_turn_runs_after_tool_results_and_before_turn_end() {
    let ordering = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut config = config();
    let o = Arc::clone(&ordering);
    config.finish_turn = Some(hook::finish_turn(move |turn, _signal| {
        assert_eq!(turn.tool_results.len(), 1);
        assert_eq!(turn.context.messages.last().unwrap().role(), "toolResult");
        o.lock().unwrap().push("finishTurn".to_owned());
        Box::pin(async { None })
    }));
    let o = Arc::clone(&ordering);
    let sink: bake_agent::AgentEventSink = Arc::new(move |event| {
        match &event {
            AgentEvent::MessageEnd { message } => o
                .lock()
                .unwrap()
                .push(format!("message_end:{}", message.role())),
            AgentEvent::TurnEnd { .. } => o.lock().unwrap().push("turn_end".to_owned()),
            _ => {}
        }
        Box::pin(async {})
    });
    run_agent_loop(
        vec![user("echo")],
        context(vec![terminating_echo()]),
        config,
        sink,
        None,
        Some(scripted(|_, _, _| {
            tool_use(vec![call("tool-1", "echo", json!({ "value": "hello" }))])
        })),
    )
    .await
    .unwrap();
    let ordering = ordering.lock().unwrap();
    assert_eq!(
        ordering[ordering.len() - 3..],
        ["message_end:toolResult", "finishTurn", "turn_end"]
    );
}

/// Pi: "runs finishTurn for a %s assistant before turn_end without changing
/// the hard exit", for `error` and `aborted`.
#[tokio::test]
async fn finish_turn_runs_for_error_and_aborted_without_changing_the_hard_exit() {
    for reason in [StopReason::Error, StopReason::Aborted] {
        let ordering = Arc::new(Mutex::new(Vec::<&str>::new()));
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let steering_polls = Arc::new(AtomicUsize::new(0));
        let follow_up_polls = Arc::new(AtomicUsize::new(0));
        let mut config = config();
        let o = Arc::clone(&ordering);
        config.finish_turn = Some(hook::finish_turn(move |turn, _signal| {
            assert_eq!(turn.message.stop_reason, reason);
            o.lock().unwrap().push("finishTurn");
            Box::pin(async { Some(AgentTurnDecision::Continue) })
        }));
        let s = Arc::clone(&steering_polls);
        config.get_steering_messages = Some(hook::get_messages(move || {
            s.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Vec::new() })
        }));
        let f = Arc::clone(&follow_up_polls);
        config.get_follow_up_messages = Some(hook::get_messages(move || {
            f.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { vec![user("queued")] })
        }));
        let o = Arc::clone(&ordering);
        let sink: bake_agent::AgentEventSink = Arc::new(move |event| {
            if event.kind() == "turn_end" {
                o.lock().unwrap().push("turn_end");
            }
            Box::pin(async {})
        });
        let p = Arc::clone(&provider_calls);
        run_agent_loop(
            vec![user("run")],
            context(vec![]),
            config,
            sink,
            None,
            Some(scripted(move |_, _, _| {
                p.fetch_add(1, Ordering::SeqCst);
                failed(reason)
            })),
        )
        .await
        .unwrap();
        assert_eq!(*ordering.lock().unwrap(), ["finishTurn", "turn_end"]);
        assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
        assert_eq!(steering_polls.load(Ordering::SeqCst), 1);
        assert_eq!(follow_up_polls.load(Ordering::SeqCst), 0);
    }
}

struct Counters {
    provider: Arc<AtomicUsize>,
    steering: Arc<AtomicUsize>,
    follow_up: Arc<AtomicUsize>,
    prepare_next: Arc<AtomicUsize>,
}

fn counting(config: &mut AgentLoopConfig, follow_up: bool) -> Counters {
    let counters = Counters {
        provider: Arc::new(AtomicUsize::new(0)),
        steering: Arc::new(AtomicUsize::new(0)),
        follow_up: Arc::new(AtomicUsize::new(0)),
        prepare_next: Arc::new(AtomicUsize::new(0)),
    };
    let s = Arc::clone(&counters.steering);
    config.get_steering_messages = Some(hook::get_messages(move || {
        s.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Vec::new() })
    }));
    let f = Arc::clone(&counters.follow_up);
    config.get_follow_up_messages = Some(hook::get_messages(move || {
        f.fetch_add(1, Ordering::SeqCst);
        let queued = if follow_up {
            vec![user("queued")]
        } else {
            Vec::new()
        };
        Box::pin(async move { queued })
    }));
    let p = Arc::clone(&counters.prepare_next);
    config.prepare_next_turn = Some(hook::prepare_next_turn(move |_turn| {
        p.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { None })
    }));
    counters
}

/// Pi: "action:end skips queue polling and next-turn preparation".
#[tokio::test]
async fn end_skips_queue_polling_and_next_turn_preparation() {
    let mut config = config();
    let counters = counting(&mut config, true);
    config.finish_turn = Some(hook::finish_turn(|_, _| {
        Box::pin(async { Some(AgentTurnDecision::End) })
    }));
    let provider = Arc::clone(&counters.provider);
    let stream = agent_loop(
        vec![user("run")],
        context(vec![noop_tool()]),
        config,
        None,
        Some(scripted(move |_, _, _| {
            provider.fetch_add(1, Ordering::SeqCst);
            tool_use(vec![call("tool-1", "noop", json!({}))])
        })),
    );
    stream.result().await.unwrap();
    assert_eq!(counters.provider.load(Ordering::SeqCst), 1);
    assert_eq!(counters.steering.load(Ordering::SeqCst), 1);
    assert_eq!(counters.follow_up.load(Ordering::SeqCst), 0);
    assert_eq!(counters.prepare_next.load(Ordering::SeqCst), 0);
}

fn continue_once(finish_calls: &Arc<AtomicUsize>) -> bake_agent::FinishTurn {
    let calls = Arc::clone(finish_calls);
    hook::finish_turn(move |_, _| {
        let first = calls.fetch_add(1, Ordering::SeqCst) == 0;
        Box::pin(async move { first.then_some(AgentTurnDecision::Continue) })
    })
}

/// Pi: "makes exactly one context-only request when no natural request
/// satisfies continuation".
#[tokio::test]
async fn continue_makes_exactly_one_context_only_request() {
    let provider = Arc::new(AtomicUsize::new(0));
    let finish = Arc::new(AtomicUsize::new(0));
    let mut config = config();
    config.finish_turn = Some(continue_once(&finish));
    let p = Arc::clone(&provider);
    let stream = agent_loop(
        vec![user("run")],
        context(vec![]),
        config,
        None,
        Some(scripted(move |index, _, _| {
            p.fetch_add(1, Ordering::SeqCst);
            assistant_text(&format!("response {}", index + 1))
        })),
    );
    stream.result().await.unwrap();
    assert_eq!(provider.load(Ordering::SeqCst), 2);
    assert_eq!(finish.load(Ordering::SeqCst), 2);
}

/// Pi: "lets a natural tool-result request satisfy continuation".
#[tokio::test]
async fn a_tool_result_request_satisfies_continue() {
    let provider = Arc::new(AtomicUsize::new(0));
    let finish = Arc::new(AtomicUsize::new(0));
    let mut config = config();
    config.finish_turn = Some(continue_once(&finish));
    let p = Arc::clone(&provider);
    let stream = agent_loop(
        vec![user("run")],
        context(vec![noop_tool()]),
        config,
        None,
        Some(scripted(move |index, _, _| {
            p.fetch_add(1, Ordering::SeqCst);
            if index == 0 {
                tool_use(vec![call("tool-1", "noop", json!({}))])
            } else {
                assistant_text("done")
            }
        })),
    );
    stream.result().await.unwrap();
    assert_eq!(provider.load(Ordering::SeqCst), 2);
    assert_eq!(finish.load(Ordering::SeqCst), 2);
}

/// Pi: "lets a natural %s request satisfy continuation", for steering and
/// follow-up.
#[tokio::test]
async fn a_steering_or_follow_up_request_satisfies_continue() {
    for steering in [true, false] {
        let kind = if steering { "steering" } else { "follow-up" };
        let provider = Arc::new(AtomicUsize::new(0));
        let finish = Arc::new(AtomicUsize::new(0));
        let polls = Arc::new(AtomicUsize::new(0));
        let delivered = Arc::new(AtomicBool::new(false));
        let second_users = Arc::new(Mutex::new(Vec::new()));
        let mut config = config();
        config.finish_turn = Some(continue_once(&finish));
        let p = Arc::clone(&polls);
        config.get_steering_messages = Some(hook::get_messages(move || {
            let n = p.fetch_add(1, Ordering::SeqCst) + 1;
            let messages = if steering && n == 2 {
                vec![user(kind)]
            } else {
                Vec::new()
            };
            Box::pin(async move { messages })
        }));
        let d = Arc::clone(&delivered);
        config.get_follow_up_messages = Some(hook::get_messages(move || {
            let messages = if !steering && !d.swap(true, Ordering::SeqCst) {
                vec![user(kind)]
            } else {
                Vec::new()
            };
            Box::pin(async move { messages })
        }));
        let (pr, users) = (Arc::clone(&provider), Arc::clone(&second_users));
        let stream = agent_loop(
            vec![user("run")],
            context(vec![]),
            config,
            None,
            Some(scripted(move |_, context, _| {
                if pr.fetch_add(1, Ordering::SeqCst) == 1 {
                    users.lock().unwrap().extend(request_users(context));
                }
                assistant_text("done")
            })),
        );
        stream.result().await.unwrap();
        assert_eq!(provider.load(Ordering::SeqCst), 2, "{kind}");
        assert_eq!(finish.load(Ordering::SeqCst), 2, "{kind}");
        assert!(
            second_users.lock().unwrap().contains(&kind.to_owned()),
            "{kind}"
        );
    }
}

/// Pi: "prepares the initial request after pending messages and can replace
/// request state".
#[tokio::test]
async fn prepare_request_runs_after_pending_messages_and_replaces_state() {
    let mut replacement = model();
    replacement.id = "replacement".to_owned();
    replacement.name = "replacement".to_owned();
    let canonical = user("canonical projection");
    let steering = user("steering");
    let completed = Arc::new(Mutex::new(Vec::<AgentMessage>::new()));
    let prepare_calls = Arc::new(AtomicUsize::new(0));
    let delivered = Arc::new(AtomicBool::new(false));
    let mut config = config();
    let (d, s) = (Arc::clone(&delivered), steering.clone());
    config.get_steering_messages = Some(hook::get_messages(move || {
        let messages = if d.swap(true, Ordering::SeqCst) {
            Vec::new()
        } else {
            vec![s.clone()]
        };
        Box::pin(async move { messages })
    }));
    let (c, pc, s, can, rep) = (
        Arc::clone(&completed),
        Arc::clone(&prepare_calls),
        steering.clone(),
        canonical.clone(),
        replacement.clone(),
    );
    config.prepare_request = Some(hook::prepare_request(move |request, _signal| {
        pc.fetch_add(1, Ordering::SeqCst);
        assert!(c.lock().unwrap().contains(&s));
        assert!(request.context.messages.contains(&s));
        let update = AgentRequestUpdate {
            context: Some(AgentContext {
                messages: vec![can.clone()],
                tools: request.context.tools.clone(),
            }),
            model: Some(rep.clone()),
            thinking_level: Some(ModelThinkingLevel::High),
        };
        Box::pin(async move { Some(update) })
    }));
    let c = Arc::clone(&completed);
    let sink: bake_agent::AgentEventSink = Arc::new(move |event| {
        if let AgentEvent::MessageEnd { message } = event {
            c.lock().unwrap().push(message);
        }
        Box::pin(async {})
    });
    let expected = canonical.as_llm().cloned().unwrap();
    let seen_model = Arc::new(Mutex::new(None));
    let sm = Arc::clone(&seen_model);
    let stream_fn: bake_agent::StreamFn = Arc::new(move |model, context, options| {
        *sm.lock().unwrap() = Some(model.id.clone());
        assert_eq!(context.messages(), std::slice::from_ref(&expected));
        assert_eq!(options.reasoning, Some(ThinkingLevel::High));
        let (sender, stream) = bake_ai::assistant_message_channel();
        sender.finish(assistant_text("done"));
        Ok(stream)
    });
    run_agent_loop(
        vec![user("prompt")],
        context(vec![]),
        config,
        sink,
        None,
        Some(stream_fn),
    )
    .await
    .unwrap();
    assert_eq!(prepare_calls.load(Ordering::SeqCst), 1);
    assert_eq!(seen_model.lock().unwrap().as_deref(), Some("replacement"));
}

/// Pi: "does not poll steering after prepareRequest".
#[tokio::test]
async fn does_not_poll_steering_after_prepare_request() {
    let queued = Arc::new(Mutex::new(Vec::<AgentMessage>::new()));
    let late = user("late steering");
    let included = Arc::new(Mutex::new(Vec::new()));
    let preparations = Arc::new(AtomicUsize::new(0));
    let polls = Arc::new(AtomicUsize::new(0));
    let mut config = config();
    let (q, p) = (Arc::clone(&queued), Arc::clone(&polls));
    config.get_steering_messages = Some(hook::get_messages(move || {
        p.fetch_add(1, Ordering::SeqCst);
        let messages: Vec<AgentMessage> = q.lock().unwrap().drain(..).collect();
        Box::pin(async move { messages })
    }));
    let (q, pr, l) = (Arc::clone(&queued), Arc::clone(&preparations), late.clone());
    config.prepare_request = Some(hook::prepare_request(move |_, _| {
        if pr.fetch_add(1, Ordering::SeqCst) == 0 {
            q.lock().unwrap().push(l.clone());
        }
        Box::pin(async { None })
    }));
    let (inc, late_llm) = (Arc::clone(&included), late.as_llm().cloned().unwrap());
    let stream = agent_loop(
        vec![user("run")],
        context(vec![]),
        config,
        None,
        Some(scripted(move |_, context, _| {
            inc.lock()
                .unwrap()
                .push(context.messages().contains(&late_llm));
            assistant_text("done")
        })),
    );
    stream.result().await.unwrap();
    assert_eq!(*included.lock().unwrap(), [false, true]);
    assert_eq!(preparations.load(Ordering::SeqCst), 2);
    // Startup, post-turn delivery, then the final natural-stop check.
    assert_eq!(polls.load(Ordering::SeqCst), 3);
}

/// Pi: "should use prepareNextTurn snapshot before continuing".
#[tokio::test]
async fn uses_the_prepare_next_turn_snapshot() {
    let prepare_calls = Arc::new(AtomicUsize::new(0));
    let has_update = Arc::new(AtomicBool::new(false));
    let mut config = config();
    let pc = Arc::clone(&prepare_calls);
    config.prepare_next_turn = Some(hook::prepare_next_turn(move |turn| {
        let first = pc.fetch_add(1, Ordering::SeqCst) == 0;
        let update = first.then(|| AgentLoopTurnUpdate {
            context: Some(AgentContext {
                messages: turn.context.messages.clone(),
                tools: turn.context.tools.clone(),
            }),
            messages: vec![
                SystemMessage {
                    content: SystemContent::Text("updated guidance".to_owned()),
                    timestamp: 1,
                    ..SystemMessage::default()
                }
                .into(),
            ],
            ..AgentLoopTurnUpdate::default()
        });
        Box::pin(async move { update })
    }));
    let calls = Arc::new(AtomicUsize::new(0));
    let (c, h) = (Arc::clone(&calls), Arc::clone(&has_update));
    let stream = agent_loop(
        vec![user("echo something")],
        context(vec![echo_tool(executed())]),
        config,
        None,
        Some(scripted(move |index, context, _| {
            c.fetch_add(1, Ordering::SeqCst);
            if index == 1 {
                h.store(
                    context.messages().iter().any(|m| matches!(m,
                        Message::System(SystemMessage { content: SystemContent::Text(text), .. }) if text == "updated guidance")),
                    Ordering::SeqCst,
                );
            }
            if index == 0 {
                tool_use(vec![call("tool-1", "echo", json!({ "value": "hello" }))])
            } else {
                assistant_text("done")
            }
        })),
    );
    drain(&stream).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(prepare_calls.load(Ordering::SeqCst), 1);
    assert!(has_update.load(Ordering::SeqCst));
}

/// Pi: "picks up steering queued during prepareNextTurn before the next
/// request".
#[tokio::test]
async fn picks_up_steering_queued_during_prepare_next_turn() {
    let queued = Arc::new(Mutex::new(Vec::<AgentMessage>::new()));
    let late = user("late steering");
    let mut config = config();
    let (q, l) = (Arc::clone(&queued), late.clone());
    config.prepare_next_turn = Some(hook::prepare_next_turn(move |_| {
        q.lock().unwrap().push(l.clone());
        Box::pin(async { None })
    }));
    let q = Arc::clone(&queued);
    config.get_steering_messages = Some(hook::get_messages(move || {
        let messages: Vec<AgentMessage> = q.lock().unwrap().drain(..).collect();
        Box::pin(async move { messages })
    }));
    let included = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let (inc, c, late_llm) = (
        Arc::clone(&included),
        Arc::clone(&calls),
        late.as_llm().cloned().unwrap(),
    );
    let stream = agent_loop(
        vec![user("run")],
        context(vec![noop_tool()]),
        config,
        None,
        Some(scripted(move |index, context, _| {
            c.fetch_add(1, Ordering::SeqCst);
            if index == 1 {
                inc.store(context.messages().contains(&late_llm), Ordering::SeqCst);
            }
            if index == 0 {
                tool_use(vec![call("tool-1", "noop", json!({}))])
            } else {
                assistant_text("done")
            }
        })),
    );
    stream.result().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(included.load(Ordering::SeqCst));
}

/// Pi: "action:end receives finalized turn context and stops before queue
/// polling". Also checks Pi's full event order for a tool turn.
#[tokio::test]
async fn end_receives_the_finalized_turn_and_stops_before_queue_polling() {
    let ran = executed();
    let mut config = config();
    let counters = counting(&mut config, true);
    config.prepare_next_turn = None;
    let seen = Arc::new(Mutex::new((Vec::new(), Vec::new())));
    let s = Arc::clone(&seen);
    config.finish_turn = Some(hook::finish_turn(move |turn, _| {
        assert_eq!(turn.message.stop_reason, StopReason::ToolUse);
        *s.lock().unwrap() = (
            turn.tool_results
                .iter()
                .map(|r| r.tool_call_id.clone())
                .collect::<Vec<_>>(),
            roles(&turn.context.messages),
        );
        Box::pin(async { Some(AgentTurnDecision::End) })
    }));
    let provider = Arc::clone(&counters.provider);
    let stream = agent_loop(
        vec![user("echo something")],
        context(vec![echo_tool(Arc::clone(&ran))]),
        config,
        None,
        Some(scripted(move |index, _, _| {
            provider.fetch_add(1, Ordering::SeqCst);
            if index == 0 {
                tool_use(vec![call("tool-1", "echo", json!({ "value": "hello" }))])
            } else {
                assistant_text("should not run")
            }
        })),
    );
    let events = drain(&stream).await;
    let messages = stream.result().await.unwrap();
    assert_eq!(counters.provider.load(Ordering::SeqCst), 1);
    assert_eq!(*ran.lock().unwrap(), ["hello"]);
    assert_eq!(counters.steering.load(Ordering::SeqCst), 1);
    assert_eq!(counters.follow_up.load(Ordering::SeqCst), 0);
    let (ids, context_roles) = seen.lock().unwrap().clone();
    assert_eq!(ids, ["tool-1"]);
    assert_eq!(context_roles, ["system", "user", "assistant", "toolResult"]);
    // The context declares no tools, so the loop announces the loadout.
    assert_eq!(
        roles(&messages),
        ["system", "user", "assistant", "toolResult"]
    );
    assert_eq!(
        kinds(&events),
        [
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "tool_execution_start",
            "tool_execution_end",
            "message_start",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );
}

/// Pi: "should stop after a tool batch when every tool result sets
/// terminate=true".
#[tokio::test]
async fn stops_when_every_tool_result_terminates() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    let stream = agent_loop(
        vec![user("echo something")],
        context(vec![terminating_echo()]),
        config(),
        None,
        Some(scripted(move |_, _, _| {
            c.fetch_add(1, Ordering::SeqCst);
            tool_use(vec![call("tool-1", "echo", json!({ "value": "hello" }))])
        })),
    );
    let events = drain(&stream).await;
    let messages = stream.result().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        roles(&messages),
        ["system", "user", "assistant", "toolResult"]
    );
    assert_eq!(events.iter().filter(|e| e.kind() == "turn_end").count(), 1);
}

/// Pi: "should stop after a blocked tool call when beforeToolCall sets
/// terminate=true".
#[tokio::test]
async fn stops_after_a_blocked_call_that_terminates() {
    let ran = executed();
    let mut config = config();
    config.before_tool_call = Some(hook::before_tool_call(|_, _| {
        Box::pin(async {
            Ok(Some(BeforeToolCallResult {
                block: true,
                reason: Some("Blocked by policy".to_owned()),
                terminate: true,
            }))
        })
    }));
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    let stream = agent_loop(
        vec![user("echo something")],
        context(vec![echo_tool(Arc::clone(&ran))]),
        config,
        None,
        Some(scripted(move |index, _, _| {
            c.fetch_add(1, Ordering::SeqCst);
            if index == 0 {
                tool_use(vec![call("tool-1", "echo", json!({ "value": "hello" }))])
            } else {
                assistant_text("should not run")
            }
        })),
    );
    drain(&stream).await;
    let messages = stream.result().await.unwrap();
    let result = messages
        .iter()
        .find_map(AgentMessage::as_tool_result)
        .unwrap();
    assert!(ran.lock().unwrap().is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(result.is_error);
    assert_eq!(
        result.content,
        [UserContentBlock::Text(TextContent::new(
            "Blocked by policy"
        ))]
    );
}

/// Pi: "should continue after a mixed batch with one terminating blocked
/// call".
#[tokio::test]
async fn continues_after_a_mixed_batch() {
    let ran = executed();
    let mut config = config();
    config.tool_execution = ToolExecutionMode::Parallel;
    config.before_tool_call = Some(hook::before_tool_call(|ctx, _| {
        let first = ctx.args["value"] == "first";
        Box::pin(async move {
            Ok(first.then(|| BeforeToolCallResult {
                block: true,
                reason: Some("Blocked first".to_owned()),
                terminate: true,
            }))
        })
    }));
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    let stream = agent_loop(
        vec![user("echo both")],
        context(vec![echo_tool(Arc::clone(&ran))]),
        config,
        None,
        Some(scripted(move |index, _, _| {
            c.fetch_add(1, Ordering::SeqCst);
            if index == 0 {
                tool_use(vec![
                    call("tool-1", "echo", json!({ "value": "first" })),
                    call("tool-2", "echo", json!({ "value": "second" })),
                ])
            } else {
                assistant_text("done")
            }
        })),
    );
    drain(&stream).await;
    assert_eq!(*ran.lock().unwrap(), ["second"]);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// Pi: "should continue after parallel tool calls when not all tool results
/// terminate".
#[tokio::test]
async fn continues_when_not_every_parallel_result_terminates() {
    let tool = AgentTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        |_, params, _, _| {
            let value = params["value"].as_str().unwrap_or_default().to_owned();
            Box::pin(async move {
                Ok(AgentToolResult {
                    terminate: value == "first",
                    ..AgentToolResult::text(value)
                })
            })
        },
    );
    let mut config = config();
    config.tool_execution = ToolExecutionMode::Parallel;
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    let stream = agent_loop(
        vec![user("echo both")],
        context(vec![tool]),
        config,
        None,
        Some(scripted(move |index, _, _| {
            c.fetch_add(1, Ordering::SeqCst);
            if index == 0 {
                tool_use(vec![
                    call("tool-1", "echo", json!({ "value": "first" })),
                    call("tool-2", "echo", json!({ "value": "second" })),
                ])
            } else {
                assistant_text("done")
            }
        })),
    );
    drain(&stream).await;
    let messages = stream.result().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        roles(&messages),
        [
            "system",
            "user",
            "assistant",
            "toolResult",
            "toolResult",
            "assistant"
        ]
    );
}

/// Pi: "should allow afterToolCall to mark a tool batch as terminating".
#[tokio::test]
async fn after_tool_call_can_terminate_the_batch() {
    let mut config = config();
    config.after_tool_call = Some(hook::after_tool_call(|_, _| {
        Box::pin(async {
            Ok(Some(AfterToolCallResult {
                terminate: Some(true),
                ..AfterToolCallResult::default()
            }))
        })
    }));
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    let stream = agent_loop(
        vec![user("echo something")],
        context(vec![echo_tool(executed())]),
        config,
        None,
        Some(scripted(move |_, _, _| {
            c.fetch_add(1, Ordering::SeqCst);
            tool_use(vec![call("tool-1", "echo", json!({ "value": "hello" }))])
        })),
    );
    drain(&stream).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Pi: "should throw when context has no messages", and the assistant-tail
/// check of `agentLoopContinue`.
#[tokio::test]
async fn continue_rejects_an_empty_context_and_an_assistant_tail() {
    let never = scripted(|_, _, _| panic!("Unexpected stream call"));
    let error =
        agent_loop_continue(context(vec![]), config(), None, Some(Arc::clone(&never))).unwrap_err();
    assert_eq!(error, AgentLoopError::NoMessages);
    assert_eq!(error.to_string(), "Cannot continue: no messages in context");
    let tail = AgentContext {
        messages: vec![assistant_text("done").into()],
        tools: Vec::new(),
    };
    let error = agent_loop_continue(tail, config(), None, Some(never)).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Cannot continue from message role: assistant"
    );
}

/// Pi: "should continue from existing context without emitting user message
/// events".
#[tokio::test]
async fn continue_emits_no_events_for_existing_messages() {
    let context = AgentContext {
        messages: vec![user("Hello")],
        tools: Vec::new(),
    };
    let stream = agent_loop_continue(
        context,
        config(),
        None,
        Some(scripted(|_, _, _| assistant_text("Response"))),
    )
    .unwrap();
    let events = drain(&stream).await;
    let messages = stream.result().await.unwrap();
    assert_eq!(roles(&messages), ["assistant"]);
    let ends: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageEnd { message } => Some(message.role().to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(ends, ["assistant"]);
}

/// Pi: "should allow custom message types as last message (caller
/// responsibility)".
#[tokio::test]
async fn continue_accepts_a_custom_tail_that_converts_to_user() {
    let custom = AgentMessage::Custom(Arc::new(Notification {
        text: "Hook content".to_owned(),
        timestamp: 3,
    }));
    let mut config = config();
    config.convert_to_llm = hook::convert_to_llm(|messages| {
        let out: Vec<Message> = messages
            .iter()
            .filter_map(|m| match m {
                AgentMessage::Custom(custom) => {
                    let text = custom.as_any().downcast_ref::<Notification>()?.text.clone();
                    Some(Message::User(UserMessage {
                        content: UserContent::Text(text),
                        timestamp: custom.timestamp(),
                    }))
                }
                AgentMessage::Llm(message) => Some(message.clone()),
            })
            .collect();
        Box::pin(async move { out })
    });
    let saw = Arc::new(Mutex::new(Vec::new()));
    let s = Arc::clone(&saw);
    let stream = agent_loop_continue(
        AgentContext {
            messages: vec![custom],
            tools: Vec::new(),
        },
        config,
        None,
        Some(scripted(move |_, context, _| {
            *s.lock().unwrap() = request_users(context);
            assistant_text("Response to custom message")
        })),
    )
    .unwrap();
    drain(&stream).await;
    let messages = stream.result().await.unwrap();
    assert_eq!(roles(&messages), ["assistant"]);
    assert_eq!(*saw.lock().unwrap(), ["Hook content"]);
}

fn echo_with_update() -> AgentTool {
    let mut tool = AgentTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        |_, params, _, update| {
            update.update(AgentToolResult {
                details: Some(json!({})),
                ..AgentToolResult::text("partial")
            });
            let value = params["value"].clone();
            Box::pin(async move {
                Ok(AgentToolResult {
                    details: Some(json!({})),
                    structured_content: Some(json!({ "value": value })),
                    ..AgentToolResult::text(value.as_str().unwrap_or_default())
                })
            })
        },
    );
    tool.output_schema = Some(value_schema());
    tool
}

fn failing_tool() -> AgentTool {
    AgentTool::new(
        "failing",
        "Failing",
        "Returns an error result",
        empty_schema(),
        |_, _, _, _| {
            Box::pin(async {
                Ok(AgentToolResult {
                    details: Some(json!({ "partial": true })),
                    is_error: true,
                    ..AgentToolResult::text("bad")
                })
            })
        },
    )
}

/// Pi `runToolCall`: "validates, runs the hooks, and reports failures as
/// error outcomes".
#[tokio::test]
async fn run_tool_call_validates_runs_hooks_and_reports_failures() {
    let tools = vec![Arc::new(echo_with_update()), Arc::new(failing_tool())];
    let assistant = assistant(Vec::new(), StopReason::Stop);
    let context = AgentContext::default();
    let hook_calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let updates = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&hook_calls);
    let before = hook::before_tool_call(move |ctx, _| {
        h.lock()
            .unwrap()
            .push(format!("before {}", ctx.tool_call.id));
        let blocked = ctx.args["value"] == "blocked";
        Box::pin(async move {
            Ok(blocked.then(|| BeforeToolCallResult {
                block: true,
                reason: Some("nope".to_owned()),
                terminate: false,
            }))
        })
    });
    let h = Arc::clone(&hook_calls);
    let after = hook::after_tool_call(move |ctx, _| {
        h.lock()
            .unwrap()
            .push(format!("after {}", ctx.tool_call.id));
        Box::pin(async { Ok(None) })
    });
    let u = Arc::clone(&updates);
    let options = RunToolCallOptions {
        tools: &tools,
        assistant_message: &assistant,
        context: &context,
        before_tool_call: Some(before),
        after_tool_call: Some(after),
        signal: None,
        on_update: Some(AgentToolUpdateCallback::new(move |partial| {
            u.lock().unwrap().push(partial)
        })),
    };
    let run = |id: &str, name: &str, args: Value| {
        let call = tool_call(id, name, args);
        let options = options.clone();
        async move { run_tool_call(&call, options).await }
    };

    let a = run("a", "echo", json!({ "value": "a" })).await;
    assert_eq!(a.tool_call.id, "a");
    assert_eq!(a.result.structured_content, Some(json!({ "value": "a" })));
    assert!(!a.is_error);
    assert!(
        run("b", "echo", json!({ "value": { "nested": true } }))
            .await
            .is_error
    );
    let c = run("c", "echo", json!({ "value": "blocked" })).await;
    assert!(c.is_error);
    assert_eq!(result_text(&c.result), "nope");
    let d = run("d", "missing", json!({})).await;
    assert!(d.is_error);
    assert_eq!(result_text(&d.result), "Tool missing not found");
    let e = run("e", "failing", json!({})).await;
    assert!(e.is_error);
    assert_eq!(e.result.details, Some(json!({ "partial": true })));
    assert_eq!(
        *updates.lock().unwrap(),
        [AgentToolResult {
            details: Some(json!({})),
            ..AgentToolResult::text("partial")
        }]
    );
    assert_eq!(
        *hook_calls.lock().unwrap(),
        ["before a", "after a", "before c", "before e", "after e"]
    );
}

/// Pi `runToolCall`: "lets afterToolCall replace structured content and
/// drops it when only content is replaced".
#[tokio::test]
async fn run_tool_call_after_hook_replaces_or_drops_structured_content() {
    let tools = vec![Arc::new(echo_with_update())];
    let assistant = assistant(Vec::new(), StopReason::Stop);
    let context = AgentContext::default();
    let redacted = vec![UserContentBlock::Text(TextContent::new("redacted"))];
    let overrides = [
        AfterToolCallResult {
            content: Some(redacted.clone()),
            ..AfterToolCallResult::default()
        },
        AfterToolCallResult {
            structured_content: Some(json!({ "value": "replaced" })),
            ..AfterToolCallResult::default()
        },
        AfterToolCallResult {
            content: Some(redacted),
            structured_content: Some(json!({ "value": "both" })),
            ..AfterToolCallResult::default()
        },
        AfterToolCallResult {
            details: Some(json!({ "note": "kept" })),
            ..AfterToolCallResult::default()
        },
    ];
    let mut seen = Vec::new();
    for after in overrides {
        let outcome = run_tool_call(
            &tool_call("x", "echo", json!({ "value": "original" })),
            RunToolCallOptions {
                tools: &tools,
                assistant_message: &assistant,
                context: &context,
                before_tool_call: None,
                after_tool_call: Some(hook::after_tool_call(move |_, _| {
                    let after = after.clone();
                    Box::pin(async move { Ok(Some(after)) })
                })),
                signal: None,
                on_update: None,
            },
        )
        .await;
        seen.push(outcome.result.structured_content);
    }
    assert_eq!(
        seen,
        [
            None,
            Some(json!({ "value": "replaced" })),
            Some(json!({ "value": "both" })),
            Some(json!({ "value": "original" }))
        ]
    );
}

/// Bake: an abort while a tool runs. The tool honors the signal, the batch
/// ends, and the next request comes back aborted, so the run ends with
/// `aborted` and makes no further request. Uses `bake-ai`'s faux provider.
#[tokio::test]
async fn abort_during_a_tool_ends_the_run_aborted() {
    let registry = Arc::new(ApiRegistry::new());
    let faux = Arc::new(FauxProvider::new(FauxProviderOptions::default()));
    registry.register(Arc::clone(&faux) as Arc<dyn bake_ai::ApiProvider>);
    faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_blocks(
            vec![
                faux_tool_call("wait", json!({}), Some("call-1")),
                faux_tool_call("wait", json!({}), Some("call-2")),
            ],
            StopReason::ToolUse,
        )),
        FauxResponseStep::Message(faux_assistant_blocks(
            vec![faux_text("unreachable")],
            StopReason::Stop,
        )),
    ]);
    let started = Arc::new(Notify::new());
    let s = Arc::clone(&started);
    let tool = AgentTool::new(
        "wait",
        "Wait",
        "Waits for abort",
        empty_schema(),
        move |_, _, signal, _| {
            let s = Arc::clone(&s);
            Box::pin(async move {
                s.notify_one();
                match signal {
                    Some(signal) => signal.cancelled().await,
                    None => std::future::pending().await,
                }
                Err("Operation aborted".to_owned())
            })
        },
    );
    let controller = AbortController::new();
    let mut config = AgentLoopConfig::new(faux.model().clone(), identity_converter());
    config.tool_execution = ToolExecutionMode::Sequential;
    let stream = agent_loop(
        vec![user("wait")],
        context(vec![tool]),
        config,
        Some(controller.signal()),
        Some(bake_agent::registry_stream_fn(registry)),
    );
    started.notified().await;
    controller.abort();
    let events = drain(&stream).await;
    let messages = stream.result().await.unwrap();
    // The sequential batch stops after the aborted call; the second never runs.
    assert_eq!(tool_end_ids(&events), ["call-1"]);
    let result = messages
        .iter()
        .find_map(AgentMessage::as_tool_result)
        .unwrap();
    assert!(result.is_error);
    assert_eq!(
        result.content,
        [UserContentBlock::Text(TextContent::new(
            "Operation aborted"
        ))]
    );
    let last = messages
        .last()
        .and_then(AgentMessage::as_assistant)
        .unwrap();
    assert_eq!(last.stop_reason, StopReason::Aborted);
    assert_eq!(faux.call_count(), 2);
    assert_eq!(events.last().map(AgentEvent::kind), Some("agent_end"));
}

/// Bake: in parallel mode, calls not yet started when the run aborts end as
/// `Operation aborted` without running.
#[tokio::test]
async fn parallel_calls_after_an_abort_do_not_run() {
    let controller = AbortController::new();
    let ran = executed();
    let mut config = config();
    let abort = controller.clone();
    config.before_tool_call = Some(hook::before_tool_call(move |_, _| {
        abort.abort();
        Box::pin(async { Ok(None) })
    }));
    let stream = agent_loop(
        vec![user("go")],
        context(vec![echo_tool(Arc::clone(&ran))]),
        config,
        Some(controller.signal()),
        Some(scripted(|index, _, options| {
            if options
                .base
                .signal
                .as_ref()
                .is_some_and(bake_ai::AbortSignal::aborted)
            {
                return failed(StopReason::Aborted);
            }
            assert_eq!(index, 0);
            tool_use(vec![
                call("tool-1", "echo", json!({ "value": "a" })),
                call("tool-2", "echo", json!({ "value": "b" })),
            ])
        })),
    );
    let events = drain(&stream).await;
    assert!(ran.lock().unwrap().is_empty());
    // Preparation stops at the first aborted call.
    assert_eq!(tool_end_ids(&events), ["tool-1"]);
}

/// Bake: streamed deltas from the faux provider become `message_update`
/// events between `message_start` and `message_end`, and the final message
/// records the requested thinking level.
#[tokio::test]
async fn streams_faux_deltas_as_message_updates() {
    let registry = Arc::new(ApiRegistry::new());
    let faux = Arc::new(FauxProvider::new(FauxProviderOptions::default()));
    registry.register(Arc::clone(&faux) as Arc<dyn bake_ai::ApiProvider>);
    faux.set_responses(vec![FauxResponseStep::Message(
        bake_ai::providers::faux::faux_assistant_message(
            "a reply long enough to stream in several chunks",
        ),
    )]);
    let mut config = AgentLoopConfig::new(faux.model().clone(), identity_converter());
    config.stream_options.reasoning = Some(ThinkingLevel::Low);
    let stream = agent_loop(
        vec![user("hi")],
        context(vec![]),
        config,
        None,
        Some(bake_agent::registry_stream_fn(registry)),
    );
    let events = drain(&stream).await;
    let assistant_events: Vec<&str> = events
        .iter()
        .skip_while(
            |e| !matches!(e, AgentEvent::MessageStart { message } if message.role() == "assistant"),
        )
        .map(AgentEvent::kind)
        .take_while(|kind| *kind != "turn_end")
        .collect();
    assert_eq!(assistant_events.first(), Some(&"message_start"));
    assert_eq!(assistant_events.last(), Some(&"message_end"));
    assert!(
        assistant_events
            .iter()
            .filter(|k| **k == "message_update")
            .count()
            >= 3
    );
    let deltas: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageUpdate {
                assistant_message_event: AssistantMessageEvent::TextDelta { delta, .. },
                ..
            } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, "a reply long enough to stream in several chunks");
    let messages = stream.result().await.unwrap();
    let last = messages
        .last()
        .and_then(AgentMessage::as_assistant)
        .unwrap();
    assert_eq!(last.thinking_level, Some(ModelThinkingLevel::Low));
}

/// Bake: a tool that panics, and one that returns `Err`, become error
/// results as Pi's thrown errors do; the run goes on.
#[tokio::test]
async fn tool_panics_and_errors_become_error_results() {
    let panicking = AgentTool::new("boom", "Boom", "Panics", empty_schema(), |_, _, _, _| {
        Box::pin(async { panic!("tool exploded") })
    });
    let erring = AgentTool::new("err", "Err", "Fails", empty_schema(), |_, _, _, _| {
        Box::pin(async { Err("tool failed".to_owned()) })
    });
    let stream = agent_loop(
        vec![user("go")],
        context(vec![panicking, erring]),
        config(),
        None,
        Some(tools_then_done(vec![
            call("p", "boom", json!({})),
            call("e", "err", json!({})),
        ])),
    );
    drain(&stream).await;
    let messages = stream.result().await.unwrap();
    let results: Vec<_> = messages
        .iter()
        .filter_map(AgentMessage::as_tool_result)
        .collect();
    assert!(results.iter().all(|r| r.is_error));
    let texts: Vec<String> = results
        .iter()
        .map(|r| match &r.content[0] {
            UserContentBlock::Text(text) => text.text.clone(),
            UserContentBlock::Image(_) => String::new(),
        })
        .collect();
    assert_eq!(texts, ["tool exploded", "tool failed"]);
    assert_eq!(messages.last().unwrap().role(), "assistant");
}

/// Bake: tool updates arrive as `tool_execution_update` events between the
/// call's start and end, in the order sent.
#[tokio::test]
async fn tool_updates_are_emitted_between_start_and_end() {
    let tool = AgentTool::new(
        "progress",
        "Progress",
        "Reports",
        empty_schema(),
        |_, _, _, update| {
            Box::pin(async move {
                for step in ["one", "two"] {
                    update.update(AgentToolResult::text(step));
                    tokio::task::yield_now().await;
                }
                Ok(AgentToolResult::text("done"))
            })
        },
    );
    let stream = agent_loop(
        vec![user("go")],
        context(vec![tool]),
        config(),
        None,
        Some(tools_then_done(vec![call("t", "progress", json!({}))])),
    );
    let events = drain(&stream).await;
    let tool_events: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolExecutionStart { .. } => Some("start".to_owned()),
            AgentEvent::ToolExecutionUpdate { partial_result, .. } => {
                Some(result_text(partial_result))
            }
            AgentEvent::ToolExecutionEnd { .. } => Some("end".to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(tool_events, ["start", "one", "two", "end"]);
}

/// Bake: dropping an `AgentLoopStream` cancels its task, and with it the
/// running tool, which is never left running.
#[tokio::test]
async fn dropping_the_loop_stream_cancels_the_running_tool() {
    let started = Arc::new(Notify::new());
    let dropped = Arc::new(Notify::new());
    struct OnDrop(Arc<Notify>);
    impl Drop for OnDrop {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }
    let (s, d) = (Arc::clone(&started), Arc::clone(&dropped));
    let tool = AgentTool::new(
        "hang",
        "Hang",
        "Never ends",
        empty_schema(),
        move |_, _, _, _| {
            let (s, guard) = (Arc::clone(&s), OnDrop(Arc::clone(&d)));
            Box::pin(async move {
                let _guard = guard;
                s.notify_one();
                std::future::pending::<()>().await;
                Ok(AgentToolResult::text("never"))
            })
        },
    );
    let stream = agent_loop(
        vec![user("go")],
        context(vec![tool]),
        config(),
        None,
        Some(tools_then_done(vec![call("t", "hang", json!({}))])),
    );
    started.notified().await;
    drop(stream);
    tokio::time::timeout(Duration::from_secs(5), dropped.notified())
        .await
        .expect("the tool future was dropped");
}

/// Bake: an invalid argument is reported with Pi's validation message and
/// the tool does not run.
#[tokio::test]
async fn invalid_arguments_fail_validation_without_running() {
    let ran = executed();
    let stream = agent_loop(
        vec![user("go")],
        context(vec![echo_tool(Arc::clone(&ran))]),
        config(),
        None,
        Some(tools_then_done(vec![call(
            "t",
            "echo",
            json!({ "other": 1 }),
        )])),
    );
    let events = drain(&stream).await;
    assert!(ran.lock().unwrap().is_empty());
    let text = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolExecutionEnd { result, .. } => Some(result_text(result)),
            _ => None,
        })
        .unwrap();
    assert!(
        text.starts_with(
            "Validation failed for tool \"echo\":\n  - value: must have required properties value"
        ),
        "{text}"
    );
}

/// Bake: an update sent in the same poll that completes the tool is still
/// emitted before `tool_execution_end`, as Pi awaits pending update events
/// before it finalizes.
#[tokio::test]
async fn an_update_sent_as_the_tool_completes_is_emitted() {
    let tool = AgentTool::new(
        "last",
        "Last",
        "Updates then ends",
        empty_schema(),
        |_, _, _, update| {
            Box::pin(async move {
                update.update(AgentToolResult::text("final progress"));
                Ok(AgentToolResult::text("done"))
            })
        },
    );
    let stream = agent_loop(
        vec![user("go")],
        context(vec![tool]),
        config(),
        None,
        Some(tools_then_done(vec![call("t", "last", json!({}))])),
    );
    let events = drain(&stream).await;
    let tool_events: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolExecutionUpdate { partial_result, .. } => {
                Some(result_text(partial_result))
            }
            AgentEvent::ToolExecutionEnd { .. } => Some("end".to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(tool_events, ["final progress", "end"]);
}

/// Bake: a tool keeps running while a slow listener handles its update,
/// and its duration, as in Pi, ends when the tool settles rather than when
/// the listener does.
#[tokio::test(start_paused = true)]
async fn slow_update_listeners_neither_stall_the_tool_nor_lengthen_its_duration() {
    let tool = AgentTool::new(
        "progress",
        "Progress",
        "Reports, then works",
        empty_schema(),
        |_, _, _, update| {
            Box::pin(async move {
                update.update(AgentToolResult::text("started"));
                tokio::time::sleep(Duration::from_millis(30)).await;
                Ok(AgentToolResult::text("done"))
            })
        },
    );
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let s = Arc::clone(&seen);
    let sink: bake_agent::AgentEventSink = Arc::new(move |event| {
        let s = Arc::clone(&s);
        Box::pin(async move {
            match &event {
                AgentEvent::ToolExecutionUpdate { partial_result, .. } => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    s.lock().unwrap().push(result_text(partial_result));
                }
                AgentEvent::ToolExecutionEnd { .. } => s.lock().unwrap().push("end".to_owned()),
                _ => {}
            }
        })
    });
    let started = tokio::time::Instant::now();
    let messages = run_agent_loop(
        vec![user("go")],
        context(vec![tool]),
        config(),
        sink,
        None,
        Some(tools_then_done(vec![call("t", "progress", json!({}))])),
    )
    .await
    .unwrap();
    let result = messages
        .iter()
        .find_map(AgentMessage::as_tool_result)
        .unwrap();
    assert_eq!(result.duration_ms, Some(30));
    // The tool's 30 ms overlapped the listener's 100 ms.
    assert!(started.elapsed() < Duration::from_millis(130));
    assert_eq!(*seen.lock().unwrap(), ["started", "end"]);
}

/// Pi `prepareToolCall`: a `prepareArguments` that throws, and a
/// `beforeToolCall` or `afterToolCall` that throws before it returns its
/// promise, end the call with an error result carrying the message. Here a
/// shim returns `Err` and the hooks panic synchronously.
#[tokio::test]
async fn failing_preparation_and_synchronously_panicking_hooks_become_error_results() {
    let tools = vec![Arc::new({
        let mut tool = echo_with_update();
        tool.prepare_arguments = Some(Arc::new(|args: Value| {
            if args["value"] == "reject" {
                Err("cannot prepare".to_owned())
            } else {
                Ok(args)
            }
        }));
        tool
    })];
    let assistant = assistant(Vec::new(), StopReason::Stop);
    let context = AgentContext::default();
    let before = hook::before_tool_call(|ctx, _| {
        assert!(ctx.args["value"] != "panic-before", "before exploded");
        Box::pin(async { Ok(None) })
    });
    let after = hook::after_tool_call(|ctx, _| {
        assert!(ctx.args["value"] != "panic-after", "after exploded");
        Box::pin(async { Ok(None) })
    });
    let options = RunToolCallOptions {
        tools: &tools,
        assistant_message: &assistant,
        context: &context,
        before_tool_call: Some(before),
        after_tool_call: Some(after),
        signal: None,
        on_update: None,
    };
    for (value, message) in [
        ("reject", "cannot prepare"),
        ("panic-before", "before exploded"),
        ("panic-after", "after exploded"),
    ] {
        let call = tool_call(value, "echo", json!({ "value": value }));
        let outcome = run_tool_call(&call, options.clone()).await;
        assert!(outcome.is_error, "{value}");
        assert_eq!(result_text(&outcome.result), message);
    }
    let call = tool_call("ok", "echo", json!({ "value": "fine" }));
    assert!(!run_tool_call(&call, options).await.is_error);
}
