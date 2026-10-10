//! Ports of Pi `packages/agent/test/agent.test.ts` (v1.1.0). Each test
//! names the Pi test it follows; tests marked "Bake" cover the Tokio
//! ownership rules that have no Pi counterpart.

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bake_agent::{
    Agent, AgentError, AgentEvent, AgentInitialState, AgentMessage, AgentOptions, AgentTool,
    AgentToolResult, AgentToolUpdateCallback, AgentTurnDecision, QueueMode, StreamFn, hook,
    set_default_stream_fn,
};
use bake_ai::transcript::get_current_system_message;
use bake_ai::{
    AbortSignal, AssistantMessageEvent, Message, ModelThinkingLevel, Sections, StopReason,
    SystemContent, SystemMessage, ToolReference, ToolResultMessage, UserContent, UserContentBlock,
    assistant_message_channel,
};
use common::*;
use serde_json::{Value, json};
use tokio::sync::Notify;

fn done() -> StreamFn {
    scripted(|_, _, _| assistant_text("done"))
}

fn unused() -> StreamFn {
    Arc::new(|_, _, _| Err("Unexpected stream call".to_owned()))
}

fn agent(stream_fn: StreamFn) -> Agent {
    Agent::new(AgentOptions {
        stream_fn: Some(stream_fn),
        ..AgentOptions::default()
    })
    .unwrap()
}

fn tool(name: &str) -> Arc<AgentTool> {
    let text = name.to_owned();
    Arc::new(AgentTool::new(
        name,
        name,
        format!("{name} tool"),
        empty_schema(),
        move |_, _, _, _| {
            let text = text.clone();
            Box::pin(async move { Ok(AgentToolResult::text(text)) })
        },
    ))
}

fn system(text: &str) -> SystemMessage {
    SystemMessage {
        content: SystemContent::Text(text.to_owned()),
        ..SystemMessage::default()
    }
}

fn systems(messages: &[AgentMessage]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(|m| m.as_system().cloned().map(Message::System))
        .collect()
}

/// A stream that starts, then ends `aborted` once the request's signal
/// aborts, like Pi's `checkAbort` polling mocks. Each request's task ends
/// with its signal.
fn abortable() -> StreamFn {
    Arc::new(|_, _, options| {
        let (sender, stream) = assistant_message_channel();
        let signal = options.base.signal.clone();
        tokio::spawn(async move {
            sender.push(AssistantMessageEvent::Start {
                partial: assistant_text(""),
            });
            if let Some(signal) = signal {
                signal.cancelled().await;
            }
            let mut message = assistant_text("Aborted");
            message.stop_reason = StopReason::Aborted;
            sender.finish(message);
        });
        Ok(stream)
    })
}

/// Pi: "uses the configured default when a legacy caller omits streamFn".
/// Without a default, construction fails as Pi's constructor throws.
#[tokio::test]
async fn uses_the_configured_default_stream_fn() {
    assert_eq!(
        Agent::new(AgentOptions::default()).unwrap_err(),
        AgentError::NoStreamFn
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    set_default_stream_fn(Some(scripted(move |_, _, _| {
        c.fetch_add(1, Ordering::SeqCst);
        assistant_text("fallback")
    })));
    let agent = Agent::new(AgentOptions::default());
    set_default_stream_fn(None);
    agent.unwrap().prompt("Hello").await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Pi: "should create an agent instance with default state".
#[test]
fn starts_with_the_default_state() {
    let state = agent(unused()).state();
    assert_eq!(state.model.id, "unknown");
    assert_eq!(state.thinking_level, ModelThinkingLevel::Off);
    assert!(state.tools.is_empty());
    assert!(state.messages.is_empty());
    assert!(!state.is_streaming);
    assert_eq!(state.streaming_message, None);
    assert!(state.pending_tool_calls.is_empty());
    assert_eq!(state.error_message, None);
}

/// Pi: "should create an agent instance with custom initial state".
#[test]
fn starts_with_a_custom_initial_state() {
    let mut custom = model();
    custom.id = "gpt-4o-mini".to_owned();
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(unused()),
        initial_state: AgentInitialState {
            system_prompt: Some("You are a helpful assistant.".to_owned()),
            model: Some(custom.clone()),
            thinking_level: Some(ModelThinkingLevel::Low),
            ..AgentInitialState::default()
        },
        ..AgentOptions::default()
    })
    .unwrap();
    let initial: AgentMessage = SystemMessage {
        timestamp: 0,
        ..system("You are a helpful assistant.")
    }
    .into();
    assert_eq!(agent.messages(), [initial]);
    assert_eq!(agent.model(), custom);
    assert_eq!(agent.thinking_level(), ModelThinkingLevel::Low);
    assert_eq!(agent.system_prompt(), "You are a helpful assistant.");
}

fn helpful_with(
    tools: Vec<Arc<AgentTool>>,
    messages: Vec<AgentMessage>,
    stream_fn: StreamFn,
) -> Agent {
    Agent::new(AgentOptions {
        stream_fn: Some(stream_fn),
        initial_state: AgentInitialState {
            system_prompt: Some("You are helpful.".to_owned()),
            tools,
            messages,
            ..AgentInitialState::default()
        },
        ..AgentOptions::default()
    })
    .unwrap()
}

/// Pi: "converts initial prompt and tools into transcript state".
#[test]
fn converts_the_initial_prompt_and_tools_into_the_transcript() {
    let agent = helpful_with(vec![tool("echo")], Vec::new(), unused());
    let messages = agent.messages();
    let initial = messages[0].as_system().unwrap();
    assert_eq!(
        initial.content,
        SystemContent::Text("You are helpful.".to_owned())
    );
    let names: Vec<_> = initial
        .tools_added
        .iter()
        .flatten()
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(names, ["echo"]);
}

fn tool_names(message: &SystemMessage) -> (String, String) {
    (
        format!(
            "+{}",
            message
                .tools_added
                .iter()
                .flatten()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ),
        format!(
            "-{}",
            message
                .tools_removed
                .iter()
                .flatten()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ),
    )
}

/// Pi: "declares tool loadout changes to the model before the next
/// request".
#[tokio::test]
async fn declares_tool_loadout_changes_before_the_next_request() {
    let requests = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let r = Arc::clone(&requests);
    let agent = helpful_with(
        vec![tool("first")],
        Vec::new(),
        scripted(move |_, context, _| {
            r.lock().unwrap().push(
                context
                    .messages()
                    .iter()
                    .filter_map(|m| match m {
                        Message::System(system) => {
                            let (added, removed) = tool_names(system);
                            Some([added, removed])
                        }
                        _ => None,
                    })
                    .flatten()
                    .collect(),
            );
            assistant_text("done")
        }),
    );
    agent.prompt("one").await.unwrap();
    agent.set_tools(vec![tool("second")]);
    agent.prompt("two").await.unwrap();
    agent.prompt("three").await.unwrap();
    let both = ["+first", "-", "+second", "-first"];
    assert_eq!(
        *requests.lock().unwrap(),
        [vec!["+first", "-"], both.to_vec(), both.to_vec()]
    );
    let messages = agent.messages();
    let update = messages
        .iter()
        .filter_map(AgentMessage::as_system)
        .find(|m| m.tools_removed.is_some())
        .unwrap();
    assert_eq!(update.content, SystemContent::Text(String::new()));
    assert_eq!(update.tools_added, Some(vec![tool("second").declaration()]));
    assert_eq!(
        update.tools_removed,
        Some(vec![ToolReference {
            name: "first".to_owned()
        }])
    );
    assert!(update.timestamp > 0);
    // The declaration carries no executable members.
    let initial = serde_json::to_value(messages[0].as_system().unwrap()).unwrap();
    assert_eq!(
        initial["toolsAdded"][0],
        json!({ "name": "first", "description": "first tool", "parameters": empty_schema() })
    );
}

/// Pi: "merges tool changes into a pending system message".
#[tokio::test]
async fn merges_tool_changes_into_a_pending_system_message() {
    let agent = helpful_with(
        Vec::new(),
        Vec::new(),
        scripted(|_, context, _| {
            assert_eq!(
                context
                    .messages()
                    .iter()
                    .filter(|m| m.role() == "system")
                    .count(),
                2
            );
            assistant_text("done")
        }),
    );
    agent.set_tools(vec![tool("echo")]);
    let sections = Sections(vec![(
        "skills".to_owned(),
        Some("<skills>x</skills>".to_owned()),
    )]);
    agent
        .prompt(vec![
            SystemMessage {
                sections: Some(sections.clone()),
                timestamp: 1,
                ..system("")
            }
            .into(),
            user("hi"),
        ])
        .await
        .unwrap();
    let expected: AgentMessage = SystemMessage {
        sections: Some(sections),
        tools_added: Some(vec![tool("echo").declaration()]),
        timestamp: 1,
        ..system("")
    }
    .into();
    assert_eq!(agent.messages()[1], expected);
}

/// Pi: "rewrites pending tool declarations to match the executable set".
#[tokio::test]
async fn rewrites_pending_tool_declarations_to_the_executable_set() {
    let agent = helpful_with(vec![tool("first")], Vec::new(), done());
    let sections = Sections(vec![("note".to_owned(), Some("<note>x</note>".to_owned()))]);
    agent
        .prompt(vec![
            SystemMessage {
                sections: Some(sections.clone()),
                tools_added: Some(vec![tool("second").declaration()]),
                tools_removed: Some(vec![ToolReference {
                    name: "first".to_owned(),
                }]),
                timestamp: 1,
                ..system("")
            }
            .into(),
            user("hi"),
        ])
        .await
        .unwrap();
    let expected: AgentMessage = SystemMessage {
        sections: Some(sections),
        timestamp: 1,
        ..system("")
    }
    .into();
    let messages = agent.messages();
    assert_eq!(messages[1], expected);
    let current = get_current_system_message(&systems(&messages)).unwrap();
    let names: Vec<_> = current
        .tools_added
        .iter()
        .flatten()
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(names, ["first"]);
}

/// Pi: "restores the transcript baseline when reset".
#[test]
fn reset_restores_the_transcript_baseline() {
    let mut old = user("old");
    if let AgentMessage::Llm(Message::User(message)) = &mut old {
        message.timestamp = 1;
    }
    let agent = helpful_with(vec![tool("echo")], vec![old], unused());
    agent.steer(user("queued"));
    agent.reset().unwrap();
    let messages = agent.messages();
    assert_eq!(messages.len(), 1);
    let initial = messages[0].as_system().unwrap();
    assert_eq!(
        initial.content,
        SystemContent::Text("You are helpful.".to_owned())
    );
    let names: Vec<_> = initial
        .tools_added
        .iter()
        .flatten()
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(names, ["echo"]);
    assert!(!agent.has_queued_messages());
}

/// Pi: "should subscribe to events".
#[tokio::test]
async fn subscribing_and_state_mutators_emit_nothing() {
    let agent = agent(done());
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);
    let subscription = agent.subscribe_fn(move |_, _| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(count.load(Ordering::SeqCst), 0);
    agent.set_thinking_level(ModelThinkingLevel::Low);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(agent.thinking_level(), ModelThinkingLevel::Low);
    subscription.unsubscribe();
    agent.set_thinking_level(ModelThinkingLevel::High);
    agent.prompt("hello").await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

fn record_kinds(agent: &Agent) -> Arc<Mutex<Vec<&'static str>>> {
    let kinds = Arc::new(Mutex::new(Vec::new()));
    let k = Arc::clone(&kinds);
    agent.subscribe_fn(move |event, _| k.lock().unwrap().push(event.kind()));
    kinds
}

/// Pi: "emits full lifecycle events for thrown run failures".
#[tokio::test]
async fn reports_a_failed_run_with_full_lifecycle_events() {
    let agent = agent(Arc::new(|_, _, _| Err("provider exploded".to_owned())));
    let kinds = record_kinds(&agent);
    agent.prompt("hello").await.unwrap();
    assert_eq!(
        *kinds.lock().unwrap(),
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
    let messages = agent.messages();
    let last = messages
        .last()
        .and_then(AgentMessage::as_assistant)
        .unwrap();
    assert_eq!(last.stop_reason, StopReason::Error);
    assert_eq!(last.error_message.as_deref(), Some("provider exploded"));
    assert_eq!(agent.error_message().as_deref(), Some("provider exploded"));
}

/// Pi: "should await async subscribers before prompt resolves".
#[tokio::test(start_paused = true)]
async fn prompt_waits_for_async_subscribers() {
    let barrier = Arc::new(Notify::new());
    let finished = Arc::new(AtomicBool::new(false));
    let agent = Arc::new(agent(done()));
    let (b, f) = (Arc::clone(&barrier), Arc::clone(&finished));
    agent.subscribe(move |event, _| {
        let (b, f) = (Arc::clone(&b), Arc::clone(&f));
        let is_end = event.kind() == "agent_end";
        Box::pin(async move {
            if is_end {
                b.notified().await;
                f.store(true, Ordering::SeqCst);
            }
        })
    });
    let resolved = Arc::new(AtomicBool::new(false));
    let (a, r) = (Arc::clone(&agent), Arc::clone(&resolved));
    let prompt = tokio::spawn(async move {
        a.prompt("hello").await.unwrap();
        r.store(true, Ordering::SeqCst);
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!resolved.load(Ordering::SeqCst));
    assert!(!finished.load(Ordering::SeqCst));
    assert!(agent.is_streaming());
    barrier.notify_one();
    prompt.await.unwrap();
    assert!(finished.load(Ordering::SeqCst));
    assert!(resolved.load(Ordering::SeqCst));
    assert!(!agent.is_streaming());
}

/// Pi: "waitForIdle should wait for async subscribers".
#[tokio::test(start_paused = true)]
async fn wait_for_idle_waits_for_async_subscribers() {
    let barrier = Arc::new(Notify::new());
    let agent = Arc::new(agent(done()));
    let b = Arc::clone(&barrier);
    agent.subscribe(move |event, _| {
        let b = Arc::clone(&b);
        let assistant_end =
            matches!(event, AgentEvent::MessageEnd { message } if message.role() == "assistant");
        Box::pin(async move {
            if assistant_end {
                b.notified().await;
            }
        })
    });
    let a = Arc::clone(&agent);
    let prompt = tokio::spawn(async move { a.prompt("hello").await.unwrap() });
    tokio::task::yield_now().await;
    let idle = Arc::new(AtomicBool::new(false));
    let (a, i) = (Arc::clone(&agent), Arc::clone(&idle));
    let waiter = tokio::spawn(async move {
        a.wait_for_idle().await;
        i.store(true, Ordering::SeqCst);
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!idle.load(Ordering::SeqCst));
    assert!(agent.is_streaming());
    barrier.notify_one();
    prompt.await.unwrap();
    waiter.await.unwrap();
    assert!(idle.load(Ordering::SeqCst));
    assert!(!agent.is_streaming());
}

/// Pi: "should pass the active abort signal to subscribers".
#[tokio::test(start_paused = true)]
async fn passes_the_run_signal_to_subscribers() {
    let received = Arc::new(Mutex::new(None::<AbortSignal>));
    let agent = Arc::new(agent(abortable()));
    let r = Arc::clone(&received);
    agent.subscribe_fn(move |event, signal| {
        if event.kind() == "agent_start" {
            *r.lock().unwrap() = Some(signal.clone());
        }
    });
    let a = Arc::clone(&agent);
    let prompt = tokio::spawn(async move { a.prompt("hello").await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    let signal = received.lock().unwrap().clone().unwrap();
    assert!(!signal.aborted());
    agent.abort();
    prompt.await.unwrap().unwrap();
    assert!(signal.aborted());
}

/// Pi: "should ignore tool updates after the tool execution settles".
#[tokio::test]
async fn ignores_tool_updates_after_the_tool_settles() {
    let late = Arc::new(Mutex::new(None::<AgentToolUpdateCallback>));
    let l = Arc::clone(&late);
    let delayed = Arc::new(AgentTool::new(
        "delayed_tool",
        "Delayed Tool",
        "Captures progress callbacks",
        empty_schema(),
        move |_, _, _, update| {
            update.update(AgentToolResult {
                details: Some(json!({ "status": "running" })),
                ..AgentToolResult::text("running")
            });
            *l.lock().unwrap() = Some(update);
            Box::pin(async {
                Ok(AgentToolResult {
                    terminate: true,
                    details: Some(json!({ "status": "done" })),
                    ..AgentToolResult::text("ok")
                })
            })
        },
    ));
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(scripted(|_, _, _| {
            tool_use(vec![call("call-1", "delayed_tool", json!({}))])
        })),
        initial_state: AgentInitialState {
            tools: vec![delayed],
            ..AgentInitialState::default()
        },
        ..AgentOptions::default()
    })
    .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let e = Arc::clone(&events);
    agent.subscribe_fn(move |event, _| e.lock().unwrap().push(event.clone()));
    agent.prompt("run tool").await.unwrap();
    let count = events.lock().unwrap().len();
    let callback = late.lock().unwrap().clone().unwrap();
    callback.update(AgentToolResult::text("late"));
    tokio::task::yield_now().await;
    let events = events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind() == "tool_execution_update")
            .count(),
        1
    );
    assert_eq!(events.len(), count);
}

/// Pi: "should ignore a settled parallel tool update while another tool is
/// still running".
#[tokio::test]
async fn ignores_a_settled_parallel_tool_update_while_another_runs() {
    let late = Arc::new(Mutex::new(None::<AgentToolUpdateCallback>));
    let slow_started = Arc::new(Notify::new());
    let settled_ended = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let l = Arc::clone(&late);
    let settled = Arc::new(AgentTool::new(
        "settled_tool",
        "Settled Tool",
        "Captures progress callbacks",
        empty_schema(),
        move |_, _, _, update| {
            *l.lock().unwrap() = Some(update);
            Box::pin(async {
                Ok(AgentToolResult {
                    terminate: true,
                    ..AgentToolResult::text("done")
                })
            })
        },
    ));
    let (s, r) = (Arc::clone(&slow_started), Arc::clone(&release));
    let slow = Arc::new(AgentTool::new(
        "slow_tool",
        "Slow Tool",
        "Keeps the agent run active",
        empty_schema(),
        move |_, _, _, _| {
            let (s, r) = (Arc::clone(&s), Arc::clone(&r));
            Box::pin(async move {
                s.notify_one();
                r.notified().await;
                Ok(AgentToolResult {
                    terminate: true,
                    ..AgentToolResult::text("done")
                })
            })
        },
    ));
    let agent = Arc::new(
        Agent::new(AgentOptions {
            stream_fn: Some(scripted(|_, _, _| {
                tool_use(vec![
                    call("call-1", "settled_tool", json!({})),
                    call("call-2", "slow_tool", json!({})),
                ])
            })),
            initial_state: AgentInitialState {
                tools: vec![settled, slow],
                ..AgentInitialState::default()
            },
            ..AgentOptions::default()
        })
        .unwrap(),
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let (e, se) = (Arc::clone(&events), Arc::clone(&settled_ended));
    agent.subscribe_fn(move |event, _| {
        e.lock().unwrap().push(event.clone());
        if matches!(event, AgentEvent::ToolExecutionEnd { tool_call_id, .. } if tool_call_id == "call-1") {
            se.notify_one();
        }
    });
    let a = Arc::clone(&agent);
    let prompt = tokio::spawn(async move { a.prompt("run tools").await.unwrap() });
    slow_started.notified().await;
    settled_ended.notified().await;
    let before = events.lock().unwrap().len();
    let callback = late.lock().unwrap().clone().unwrap();
    callback.update(AgentToolResult::text("late"));
    tokio::task::yield_now().await;
    assert_eq!(events.lock().unwrap().len(), before);
    release.notify_one();
    prompt.await.unwrap();
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.kind() == "tool_execution_update")
            .count(),
        0
    );
}

/// Pi: "should update state with mutators".
#[test]
fn updates_state_with_mutators() {
    let agent = agent(unused());
    let mut next = model();
    next.id = "gemini-2.5-flash".to_owned();
    agent.set_model(next.clone());
    assert_eq!(agent.model(), next);
    agent.set_thinking_level(ModelThinkingLevel::High);
    assert_eq!(agent.thinking_level(), ModelThinkingLevel::High);
    let tools = vec![tool("test")];
    agent.set_tools(tools.clone());
    assert!(Arc::ptr_eq(&agent.tools()[0], &tools[0]));
    let messages = vec![user("Hello")];
    agent.set_messages(messages.clone());
    assert_eq!(agent.messages(), messages);
    agent.update_messages(|messages| messages.push(assistant_text("Hi").into()));
    assert_eq!(agent.messages().len(), 2);
    agent.set_messages(Vec::new());
    assert!(agent.messages().is_empty());
}

/// Pi: "should support steering message queue" and "should support
/// follow-up message queue".
#[test]
fn queues_steering_and_follow_ups_outside_the_transcript() {
    let agent = agent(unused());
    agent.steer(user("Steering message"));
    agent.follow_up(user("Follow-up message"));
    assert!(agent.messages().is_empty());
    assert!(agent.has_queued_messages());
}

/// Pi: "should handle abort controller".
#[test]
fn abort_without_a_run_does_nothing() {
    let agent = agent(unused());
    agent.abort();
    assert!(agent.signal().is_none());
}

/// A stream that starts and waits for `release` before finishing.
fn held(started: Arc<Notify>, release: Arc<Notify>) -> StreamFn {
    Arc::new(move |_, _, _| {
        let (sender, stream) = assistant_message_channel();
        let (started, release) = (Arc::clone(&started), Arc::clone(&release));
        tokio::spawn(async move {
            sender.push(AssistantMessageEvent::Start {
                partial: assistant_text(""),
            });
            started.notify_one();
            release.notified().await;
            sender.finish(assistant_text("Done"));
        });
        Ok(stream)
    })
}

/// Pi: "should reject reset while processing without corrupting the
/// transcript".
#[tokio::test]
async fn reset_is_refused_while_processing() {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let agent = Arc::new(agent(held(Arc::clone(&started), Arc::clone(&release))));
    let a = Arc::clone(&agent);
    let prompt = tokio::spawn(async move { a.prompt("Hello").await.unwrap() });
    started.notified().await;
    // The partial assistant message is streaming but not in the transcript.
    assert!(agent.is_streaming());
    assert_eq!(roles(&agent.messages()), ["user"]);
    let error = agent.reset().unwrap_err();
    assert_eq!(
        error.to_string(),
        "Agent is already processing. Wait for completion before resetting."
    );
    assert!(agent.is_streaming());
    assert_eq!(roles(&agent.messages()), ["user"]);
    release.notify_one();
    prompt.await.unwrap();
    assert!(!agent.is_streaming());
    assert_eq!(roles(&agent.messages()), ["user", "assistant"]);
}

/// Pi: "should throw when prompt() called while streaming" and "should
/// throw when continue() called while streaming".
#[tokio::test(start_paused = true)]
async fn prompt_and_continue_are_refused_while_streaming() {
    let agent = Arc::new(agent(abortable()));
    let a = Arc::clone(&agent);
    let first = tokio::spawn(async move { a.prompt("First message").await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(agent.is_streaming());
    assert_eq!(
        agent
            .prompt("Second message")
            .await
            .unwrap_err()
            .to_string(),
        "Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion."
    );
    assert_eq!(
        agent.continue_run().await.unwrap_err().to_string(),
        "Agent is already processing. Wait for completion before continuing."
    );
    agent.abort();
    first.await.unwrap().unwrap();
    let messages = agent.messages();
    assert_eq!(
        messages
            .last()
            .and_then(AgentMessage::as_assistant)
            .unwrap()
            .stop_reason,
        StopReason::Aborted
    );
}

fn user_blocks(text: &str) -> AgentMessage {
    bake_ai::UserMessage {
        content: UserContent::Blocks(vec![UserContentBlock::Text(bake_ai::TextContent::new(
            text,
        ))]),
        timestamp: bake_ai::now_ms(),
    }
    .into()
}

/// Pi: "continue() should process queued follow-up messages after an
/// assistant turn".
#[tokio::test]
async fn continue_processes_a_queued_follow_up_after_an_assistant_turn() {
    let agent = agent(scripted(|_, _, _| assistant_text("Processed")));
    agent.set_messages(vec![
        user_blocks("Initial"),
        assistant_text("Initial response").into(),
    ]);
    agent.follow_up(user_blocks("Queued follow-up"));
    agent.continue_run().await.unwrap();
    let messages = agent.messages();
    assert!(messages.contains(&messages[2]));
    assert!(messages.iter().any(|m| matches!(m.as_user(), Some(u) if u.content == UserContent::Blocks(vec![UserContentBlock::Text(bake_ai::TextContent::new("Queued follow-up"))]))));
    assert_eq!(messages.last().unwrap().role(), "assistant");
}

fn recording_users() -> (StreamFn, Arc<Mutex<Vec<Vec<String>>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let r = Arc::clone(&requests);
    let stream_fn = scripted(move |_, context, _| {
        r.lock().unwrap().push(request_users(context));
        assistant_text("Processed")
    });
    (stream_fn, requests)
}

/// Pi: "continue() keeps $mode steering semantics for assistant-tail
/// fallback", for both modes.
#[tokio::test]
async fn continue_keeps_steering_mode_from_an_assistant_tail() {
    for (mode, expected) in [(QueueMode::OneAtATime, 2), (QueueMode::All, 1)] {
        let (stream_fn, requests) = recording_users();
        let agent = Agent::new(AgentOptions {
            steering_mode: mode,
            stream_fn: Some(stream_fn),
            ..AgentOptions::default()
        })
        .unwrap();
        agent.set_messages(vec![
            user("Initial"),
            assistant_text("Initial response").into(),
        ]);
        agent.steer(user("Steering 1"));
        agent.steer(user("Steering 2"));
        agent.continue_run().await.unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), expected, "{mode:?}");
        assert!(requests[0].contains(&"Steering 1".to_owned()));
        if mode == QueueMode::OneAtATime {
            assert!(!requests[0].contains(&"Steering 2".to_owned()));
            assert!(requests[1].contains(&"Steering 2".to_owned()));
        } else {
            assert!(requests[0].contains(&"Steering 2".to_owned()));
        }
    }
}

/// Pi: "keeps legacy prepareNextTurn signal callback behavior".
#[tokio::test]
async fn legacy_prepare_next_turn_receives_the_run_signal() {
    let saw_signal = Arc::new(AtomicBool::new(false));
    let s = Arc::clone(&saw_signal);
    let requests = Arc::new(AtomicUsize::new(0));
    let r = Arc::clone(&requests);
    let agent = Agent::new(AgentOptions {
        initial_state: AgentInitialState {
            tools: vec![tool("noop")],
            ..AgentInitialState::default()
        },
        prepare_next_turn: Some(Arc::new(move |signal| {
            s.store(signal.is_some(), Ordering::SeqCst);
            Box::pin(async { None })
        })),
        stream_fn: Some(scripted(move |index, _, _| {
            r.fetch_add(1, Ordering::SeqCst);
            if index == 0 {
                tool_use(vec![call("tool-1", "noop", json!({}))])
            } else {
                assistant_text("done")
            }
        })),
        ..AgentOptions::default()
    })
    .unwrap();
    agent.prompt("start").await.unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    assert!(saw_signal.load(Ordering::SeqCst));
}

/// Pi: "forwards finishTurn through AgentOptions with the active abort
/// signal".
#[tokio::test]
async fn forwards_finish_turn_with_the_run_signal() {
    let seen = Arc::new(Mutex::new((false, Vec::new())));
    let s = Arc::clone(&seen);
    let requests = Arc::new(AtomicUsize::new(0));
    let r = Arc::clone(&requests);
    let agent = Agent::new(AgentOptions {
        initial_state: AgentInitialState {
            tools: vec![tool("noop")],
            ..AgentInitialState::default()
        },
        finish_turn: Some(hook::finish_turn(move |turn, signal| {
            *s.lock().unwrap() = (signal.is_some(), roles(&turn.context.messages));
            Box::pin(async { Some(AgentTurnDecision::End) })
        })),
        stream_fn: Some(scripted(move |index, _, _| {
            r.fetch_add(1, Ordering::SeqCst);
            if index == 0 {
                tool_use(vec![call("tool-1", "noop", json!({}))])
            } else {
                assistant_text("should not run")
            }
        })),
        ..AgentOptions::default()
    })
    .unwrap();
    agent.prompt("start").await.unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    let (saw_signal, context_roles) = seen.lock().unwrap().clone();
    assert!(saw_signal);
    assert_eq!(context_roles, ["system", "user", "assistant", "toolResult"]);
}

/// Pi: "rejects a queued continuation from $name context without draining
/// queues", for an empty and a system-only transcript.
#[tokio::test]
async fn continue_from_an_empty_or_system_only_transcript_keeps_the_queues() {
    for messages in [
        Vec::new(),
        vec![AgentMessage::from(SystemMessage {
            timestamp: 1,
            ..system("system only")
        })],
    ] {
        let agent = Agent::new(AgentOptions {
            stream_fn: Some(unused()),
            initial_state: AgentInitialState {
                messages,
                ..AgentInitialState::default()
            },
            ..AgentOptions::default()
        })
        .unwrap();
        let (steering, follow_up) = (user("steering"), user("follow-up"));
        agent.steer(steering.clone());
        agent.follow_up(follow_up.clone());
        assert_eq!(
            agent.continue_run().await.unwrap_err().to_string(),
            "No messages to continue from"
        );
        assert_eq!(agent.peek_queued_messages(), [steering]);
        agent.clear_steering_queue();
        assert_eq!(agent.peek_queued_messages(), [follow_up]);
    }
}

/// Pi: "defers follow-up input on the first continuation request from a
/// $name tail", for a user and a tool-result tail.
#[tokio::test]
async fn continue_defers_follow_ups_past_the_first_request() {
    let tool_result: AgentMessage = ToolResultMessage {
        tool_call_id: "call-1".to_owned(),
        tool_name: "noop".to_owned(),
        content: vec![UserContentBlock::Text(bake_ai::TextContent::new("done"))],
        timestamp: 1,
        ..ToolResultMessage::default()
    }
    .into();
    let tails = [
        vec![user("existing user")],
        vec![
            user("existing user"),
            tool_use(vec![call("call-1", "noop", json!({}))]).into(),
            tool_result,
        ],
    ];
    for messages in tails {
        let (stream_fn, requests) = recording_users();
        let agent = Agent::new(AgentOptions {
            stream_fn: Some(stream_fn),
            initial_state: AgentInitialState {
                messages,
                ..AgentInitialState::default()
            },
            ..AgentOptions::default()
        })
        .unwrap();
        agent.follow_up(user("follow-up"));
        agent.continue_run().await.unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(!requests[0].contains(&"follow-up".to_owned()));
        assert!(requests[1].contains(&"follow-up".to_owned()));
    }
}

/// Pi: "polls $mode steering at continuation startup", for both modes.
#[tokio::test]
async fn continue_polls_steering_at_startup() {
    for (mode, expected) in [(QueueMode::OneAtATime, 2), (QueueMode::All, 1)] {
        let (stream_fn, requests) = recording_users();
        let agent = Agent::new(AgentOptions {
            steering_mode: mode,
            stream_fn: Some(stream_fn),
            initial_state: AgentInitialState {
                messages: vec![user("existing")],
                ..AgentInitialState::default()
            },
            ..AgentOptions::default()
        })
        .unwrap();
        agent.steer(user("first"));
        agent.steer(user("second"));
        agent.continue_run().await.unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), expected, "{mode:?}");
        assert!(requests[0].contains(&"first".to_owned()));
        assert_eq!(
            requests[0].contains(&"second".to_owned()),
            mode == QueueMode::All
        );
        if mode == QueueMode::OneAtATime {
            assert!(requests[1].contains(&"second".to_owned()));
        }
    }
}

/// Pi: "keeps steering ahead of follow-up from a non-assistant continuation
/// tail".
#[tokio::test]
async fn continue_keeps_steering_ahead_of_follow_ups() {
    let (stream_fn, requests) = recording_users();
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn),
        initial_state: AgentInitialState {
            messages: vec![user("existing")],
            ..AgentInitialState::default()
        },
        ..AgentOptions::default()
    })
    .unwrap();
    agent.steer(user("steering"));
    agent.follow_up(user("follow-up"));
    agent.continue_run().await.unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains(&"steering".to_owned()));
    assert!(!requests[0].contains(&"follow-up".to_owned()));
    assert!(requests[1].contains(&"follow-up".to_owned()));
}

fn steer_on_assistant_end(agent: &Arc<Agent>, message: AgentMessage) {
    let weak = Arc::downgrade(agent);
    agent.subscribe_fn(move |event, _| {
        if matches!(event, AgentEvent::MessageEnd { message } if message.role() == "assistant")
            && let Some(agent) = weak.upgrade()
        {
            agent.steer(message.clone());
        }
    });
}

/// Pi: "keeps queues on a %s response even when finishTurn requests
/// continuation", for `error` and `aborted`.
#[tokio::test]
async fn keeps_queues_on_a_failed_response_even_when_finish_turn_continues() {
    for reason in [StopReason::Error, StopReason::Aborted] {
        let agent = Arc::new(
            Agent::new(AgentOptions {
                finish_turn: Some(hook::finish_turn(|_, _| {
                    Box::pin(async { Some(AgentTurnDecision::Continue) })
                })),
                stream_fn: Some(scripted(move |_, _, _| failed(reason))),
                ..AgentOptions::default()
            })
            .unwrap(),
        );
        let (steering, follow_up) = (user("steering"), user("follow-up"));
        agent.follow_up(follow_up.clone());
        steer_on_assistant_end(&agent, steering.clone());
        agent.prompt("start").await.unwrap();
        assert_eq!(agent.peek_queued_messages(), [steering]);
        agent.clear_steering_queue();
        assert_eq!(agent.peek_queued_messages(), [follow_up]);
    }
}

/// Pi: "keeps queues when finishTurn ends the run".
#[tokio::test]
async fn keeps_queues_when_finish_turn_ends_the_run() {
    let agent = Arc::new(
        Agent::new(AgentOptions {
            finish_turn: Some(hook::finish_turn(|_, _| {
                Box::pin(async { Some(AgentTurnDecision::End) })
            })),
            stream_fn: Some(done()),
            ..AgentOptions::default()
        })
        .unwrap(),
    );
    let (steering, follow_up) = (user("steering"), user("follow-up"));
    agent.follow_up(follow_up.clone());
    steer_on_assistant_end(&agent, steering.clone());
    agent.prompt("start").await.unwrap();
    assert_eq!(agent.peek_queued_messages(), [steering]);
    agent.clear_steering_queue();
    assert_eq!(agent.peek_queued_messages(), [follow_up]);
}

/// Pi: "previews the next selected queued messages without consuming them".
#[test]
fn peeks_the_next_queued_messages_without_consuming_them() {
    let agent = Agent::new(AgentOptions {
        steering_mode: QueueMode::OneAtATime,
        follow_up_mode: QueueMode::All,
        stream_fn: Some(unused()),
        ..AgentOptions::default()
    })
    .unwrap();
    let (first, second, follow_up) = (
        user("first steering"),
        user("second steering"),
        user("follow-up"),
    );
    agent.steer(first.clone());
    agent.steer(second);
    agent.follow_up(follow_up.clone());
    assert_eq!(agent.peek_queued_messages(), std::slice::from_ref(&first));
    assert_eq!(agent.peek_queued_messages(), [first]);
    agent.clear_steering_queue();
    assert_eq!(agent.peek_queued_messages(), [follow_up]);
}

/// Pi: "forwards provider stream event observers through AgentOptions".
#[tokio::test]
async fn forwards_provider_stream_event_observers() {
    let events = Arc::new(Mutex::new(Vec::<Value>::new()));
    let e = Arc::clone(&events);
    let agent = Agent::new(AgentOptions {
        on_provider_stream_event: Some(Arc::new(move |data, _model| {
            e.lock().unwrap().push(data.clone())
        })),
        stream_fn: Some(Arc::new(|model, _, options| {
            if let Some(observe) = &options.base.on_provider_stream_event {
                observe(&json!({ "request_cost": 0.01 }), model);
            }
            let (sender, stream) = assistant_message_channel();
            sender.finish(assistant_text("ok"));
            Ok(stream)
        })),
        ..AgentOptions::default()
    })
    .unwrap();
    agent.prompt("hello").await.unwrap();
    assert_eq!(*events.lock().unwrap(), [json!({ "request_cost": 0.01 })]);
}

/// Pi: "forwards sessionId to streamFunction options".
#[tokio::test]
async fn forwards_the_session_id() {
    let received = Arc::new(Mutex::new(None));
    let r = Arc::clone(&received);
    let agent = Agent::new(AgentOptions {
        session_id: Some("session-abc".to_owned()),
        stream_fn: Some(scripted(move |_, _, options| {
            *r.lock().unwrap() = options.base.session_id.clone();
            assistant_text("ok")
        })),
        ..AgentOptions::default()
    })
    .unwrap();
    agent.prompt("hello").await.unwrap();
    assert_eq!(received.lock().unwrap().as_deref(), Some("session-abc"));
    agent.set_session_id(Some("session-def".to_owned()));
    assert_eq!(agent.session_id().as_deref(), Some("session-def"));
    agent.prompt("hello again").await.unwrap();
    assert_eq!(received.lock().unwrap().as_deref(), Some("session-def"));
}

/// Bake: an abort during a tool, through the agent. The tool honors the
/// signal, pending calls and listeners settle, and the agent is idle with
/// the aborted turn recorded.
#[tokio::test]
async fn abort_during_a_tool_settles_the_run() {
    let started = Arc::new(Notify::new());
    let s = Arc::clone(&started);
    let waiting = Arc::new(AgentTool::new(
        "wait",
        "Wait",
        "Waits for abort",
        empty_schema(),
        move |_, _, signal, _| {
            let s = Arc::clone(&s);
            Box::pin(async move {
                s.notify_one();
                if let Some(signal) = signal {
                    signal.cancelled().await;
                }
                Err("Operation aborted".to_owned())
            })
        },
    ));
    let agent = Arc::new(
        Agent::new(AgentOptions {
            initial_state: AgentInitialState {
                tools: vec![waiting],
                ..AgentInitialState::default()
            },
            stream_fn: Some(scripted(|_, _, options| {
                if options
                    .base
                    .signal
                    .as_ref()
                    .is_some_and(AbortSignal::aborted)
                {
                    failed(StopReason::Aborted)
                } else {
                    tool_use(vec![call("call-1", "wait", json!({}))])
                }
            })),
            ..AgentOptions::default()
        })
        .unwrap(),
    );
    let a = Arc::clone(&agent);
    let prompt = tokio::spawn(async move { a.prompt("wait").await });
    started.notified().await;
    assert_eq!(
        agent.pending_tool_calls().into_iter().collect::<Vec<_>>(),
        ["call-1"]
    );
    agent.abort();
    prompt.await.unwrap().unwrap();
    assert!(!agent.is_streaming());
    assert!(agent.pending_tool_calls().is_empty());
    assert_eq!(agent.error_message().as_deref(), Some("aborted"));
    assert_eq!(
        roles(&agent.messages()),
        ["system", "user", "assistant", "toolResult", "assistant"]
    );
}

/// Bake: `shutdown` cancels a run whose tool ignores the signal, waits for
/// its task, calls no listener afterwards, and refuses later runs.
#[tokio::test]
async fn shutdown_cancels_the_run_and_silences_listeners() {
    let started = Arc::new(Notify::new());
    let s = Arc::clone(&started);
    let stubborn = Arc::new(AgentTool::new(
        "stubborn",
        "Stubborn",
        "Ignores abort",
        empty_schema(),
        move |_, _, _, _| {
            let s = Arc::clone(&s);
            Box::pin(async move {
                s.notify_one();
                std::future::pending::<()>().await;
                Ok(AgentToolResult::text("never"))
            })
        },
    ));
    let agent = Arc::new(
        Agent::new(AgentOptions {
            initial_state: AgentInitialState {
                tools: vec![stubborn],
                ..AgentInitialState::default()
            },
            stream_fn: Some(scripted(|_, _, _| {
                tool_use(vec![call("c", "stubborn", json!({}))])
            })),
            ..AgentOptions::default()
        })
        .unwrap(),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    agent.subscribe_fn(move |_, _| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    let a = Arc::clone(&agent);
    let prompt = tokio::spawn(async move { a.prompt("go").await });
    started.notified().await;
    tokio::time::timeout(Duration::from_secs(5), agent.shutdown())
        .await
        .expect("shutdown returns once the run's task is gone");
    prompt.await.unwrap().unwrap();
    let after = calls.load(Ordering::SeqCst);
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), after);
    assert!(!agent.is_streaming());
    assert!(agent.signal().is_none());
    assert_eq!(agent.prompt("again").await.unwrap_err(), AgentError::Closed);
}

/// Bake: dropping the agent cancels a detached run: the tool future is
/// dropped and no listener runs afterwards.
#[tokio::test]
async fn dropping_the_agent_cancels_a_detached_run() {
    struct OnDrop(Arc<Notify>);
    impl Drop for OnDrop {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }
    let started = Arc::new(Notify::new());
    let dropped = Arc::new(Notify::new());
    let (s, d) = (Arc::clone(&started), Arc::clone(&dropped));
    let hang = Arc::new(AgentTool::new(
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
    ));
    let agent = Agent::new(AgentOptions {
        initial_state: AgentInitialState {
            tools: vec![hang],
            ..AgentInitialState::default()
        },
        stream_fn: Some(scripted(|_, _, _| {
            tool_use(vec![call("c", "hang", json!({}))])
        })),
        ..AgentOptions::default()
    })
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    agent.subscribe_fn(move |_, _| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    // Start the run and drop the waiting future: the run is detached.
    let detached = agent.prompt("go");
    tokio::select! {
        _ = detached => panic!("the run cannot finish"),
        () = started.notified() => {}
    }
    assert!(agent.is_streaming());
    let before = calls.load(Ordering::SeqCst);
    drop(agent);
    tokio::time::timeout(Duration::from_secs(5), dropped.notified())
        .await
        .expect("the tool future was dropped");
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), before);
}

/// Bake: once `wait_for_idle` returns, no listener or hook runs; the last
/// listener call is `agent_end`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_runs_after_the_agent_is_idle() {
    let after_idle = Arc::new(AtomicBool::new(false));
    let idle = Arc::new(AtomicBool::new(false));
    let (a, i) = (Arc::clone(&after_idle), Arc::clone(&idle));
    let agent = Agent::new(AgentOptions {
        initial_state: AgentInitialState {
            tools: vec![tool("noop")],
            ..AgentInitialState::default()
        },
        stream_fn: Some(tools_then_done(vec![call("c", "noop", json!({}))])),
        ..AgentOptions::default()
    })
    .unwrap();
    let last = Arc::new(Mutex::new(""));
    let l = Arc::clone(&last);
    agent.subscribe(move |event, _| {
        let (a, i, l) = (Arc::clone(&a), Arc::clone(&i), Arc::clone(&l));
        let kind = event.kind();
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(1)).await;
            if i.load(Ordering::SeqCst) {
                a.store(true, Ordering::SeqCst);
            }
            *l.lock().unwrap() = kind;
        })
    });
    agent.prompt("go").await.unwrap();
    idle.store(true, Ordering::SeqCst);
    agent.wait_for_idle().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!after_idle.load(Ordering::SeqCst));
    assert_eq!(*last.lock().unwrap(), "agent_end");
}

/// Bake: a panicking listener ends the run as a failed turn and leaves the
/// agent idle and usable.
#[tokio::test]
async fn a_panicking_listener_fails_the_run_without_wedging_the_agent() {
    let agent = agent(done());
    let armed = Arc::new(AtomicBool::new(true));
    let a = Arc::clone(&armed);
    agent.subscribe_fn(move |event, _| {
        if event.kind() == "agent_start" && a.swap(false, Ordering::SeqCst) {
            panic!("listener exploded");
        }
    });
    agent.prompt("one").await.unwrap();
    assert!(!agent.is_streaming());
    assert_eq!(agent.error_message().as_deref(), Some("listener exploded"));
    agent.prompt("two").await.unwrap();
    assert_eq!(agent.messages().last().unwrap().role(), "assistant");
}

/// Bake: the default converter drops custom messages, as Pi's does.
#[tokio::test]
async fn the_default_converter_drops_custom_messages() {
    #[derive(Debug)]
    struct Note;
    impl bake_agent::CustomAgentMessage for Note {
        fn role(&self) -> &str {
            "note"
        }
        fn timestamp(&self) -> i64 {
            0
        }
        fn to_json(&self) -> Value {
            json!({ "role": "note" })
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }
    let roles_seen = Arc::new(Mutex::new(Vec::new()));
    let r = Arc::clone(&roles_seen);
    let agent = agent(scripted(move |_, context, _| {
        *r.lock().unwrap() = context
            .messages()
            .iter()
            .map(|m| m.role())
            .collect::<Vec<_>>();
        assistant_text("done")
    }));
    agent.set_messages(vec![AgentMessage::Custom(Arc::new(Note))]);
    agent.prompt("hi").await.unwrap();
    assert_eq!(*roles_seen.lock().unwrap(), ["user"]);
    assert_eq!(roles(&agent.messages()), ["note", "user", "assistant"]);
    assert_eq!(
        serde_json::to_value(&agent.messages()[0]).unwrap(),
        json!({ "role": "note" })
    );
}

/// A stream function whose provider task, which the agent does not own,
/// calls the run's observers once at the start and again when `later` is
/// notified, as a provider does for chunks it is still processing.
fn observing_provider(later: Arc<Notify>) -> StreamFn {
    Arc::new(move |model, _, options| {
        let (sender, stream) = assistant_message_channel();
        let later = Arc::clone(&later);
        let model = model.clone();
        let base = options.base;
        let observe = move || {
            if let Some(observe) = &base.on_provider_stream_event {
                observe(&json!({ "chunk": true }), &model);
            }
            if let Some(observe) = &base.on_payload {
                let _ = observe(&json!({}), &model);
            }
        };
        tokio::spawn(async move {
            observe();
            sender.finish(assistant_text("done"));
            later.notified().await;
            observe();
        });
        Ok(stream)
    })
}

fn counting_observers(stream_fn: StreamFn) -> (Agent, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let (a, b) = (Arc::clone(&calls), Arc::clone(&calls));
    let agent = Agent::new(AgentOptions {
        on_provider_stream_event: Some(Arc::new(move |_, _| {
            a.fetch_add(1, Ordering::SeqCst);
        })),
        on_payload: Some(Arc::new(move |_, _| {
            b.fetch_add(1, Ordering::SeqCst);
            None
        })),
        stream_fn: Some(stream_fn),
        ..AgentOptions::default()
    })
    .unwrap();
    (agent, calls)
}

/// Bake: a provider task that outlives its run cannot call the run's
/// observers once the agent is idle or shut down.
#[tokio::test]
async fn provider_observers_do_not_run_after_the_run_ends() {
    for shut_down in [false, true] {
        let later = Arc::new(Notify::new());
        let (agent, calls) = counting_observers(observing_provider(Arc::clone(&later)));
        agent.prompt("go").await.unwrap();
        if shut_down {
            agent.shutdown().await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        later.notify_one();
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2, "shut down: {shut_down}");
    }
}

/// Bake: `shutdown` returns only after an observer call already running on
/// a provider thread has returned.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_waits_for_a_running_provider_observer() {
    let entered = Arc::new(Notify::new());
    let returned = Arc::new(AtomicBool::new(false));
    let (e, r) = (Arc::clone(&entered), Arc::clone(&returned));
    let agent = Agent::new(AgentOptions {
        on_provider_stream_event: Some(Arc::new(move |_, _| {
            e.notify_one();
            std::thread::sleep(Duration::from_millis(100));
            r.store(true, Ordering::SeqCst);
        })),
        stream_fn: Some(Arc::new(|model, _, options| {
            let (sender, stream) = assistant_message_channel();
            let model = model.clone();
            let observe = options.base.on_provider_stream_event.clone();
            std::thread::spawn(move || {
                if let Some(observe) = observe {
                    observe(&json!({}), &model);
                }
                drop(sender);
            });
            Ok(stream)
        })),
        ..AgentOptions::default()
    })
    .unwrap();
    let agent = Arc::new(agent);
    let a = Arc::clone(&agent);
    let prompt = tokio::spawn(async move { a.prompt("go").await });
    entered.notified().await;
    agent.shutdown().await;
    assert!(returned.load(Ordering::SeqCst));
    prompt.await.unwrap().unwrap();
}
