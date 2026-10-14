//! Ports of Pi `packages/agent/test/e2e.test.ts` (v1.1.0): the agent
//! against `bake-ai`'s faux provider. Each test names the Pi test it
//! follows.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bake_agent::{
    Agent, AgentEvent, AgentInitialState, AgentMessage, AgentOptions, AgentTool, AgentToolResult,
    registry_stream_fn,
};
use bake_ai::providers::faux::{
    FauxModelDefinition, FauxProvider, FauxProviderOptions, FauxResponseStep,
    faux_assistant_blocks, faux_assistant_message, faux_text, faux_thinking, faux_tool_call,
};
use bake_ai::{
    ApiProvider, ApiRegistry, AssistantMessage, Message, ModelThinkingLevel, StopReason,
    TextContent, ToolResultMessage, UserContent, UserContentBlock, UserMessage,
};
use common::*;
use serde_json::json;

fn faux(options: FauxProviderOptions) -> (Arc<FauxProvider>, Arc<ApiRegistry>) {
    let provider = Arc::new(FauxProvider::new(options));
    let registry = Arc::new(ApiRegistry::new());
    registry.register(Arc::clone(&provider) as Arc<dyn ApiProvider>);
    (provider, registry)
}

fn agent_with(
    provider: &FauxProvider,
    registry: Arc<ApiRegistry>,
    system_prompt: &str,
    tools: Vec<Arc<AgentTool>>,
) -> Agent {
    Agent::new(AgentOptions {
        stream_fn: Some(registry_stream_fn(registry)),
        initial_state: AgentInitialState {
            system_prompt: Some(system_prompt.to_owned()),
            model: Some(provider.model().clone()),
            thinking_level: Some(ModelThinkingLevel::Off),
            tools,
            messages: Vec::new(),
        },
        ..AgentOptions::default()
    })
    .unwrap()
}

/// Pi's `test/utils/calculate.ts` for `a <op> b` expressions; Pi evaluates
/// JavaScript.
fn calculate_tool() -> Arc<AgentTool> {
    let schema = json!({
        "type": "object",
        "properties": {
            "expression": { "type": "string", "description": "The mathematical expression to evaluate" },
        },
        "required": ["expression"],
    });
    Arc::new(AgentTool::new(
        "calculate",
        "Calculator",
        "Evaluate mathematical expressions",
        schema,
        |_, params, _, _| {
            let expression = params["expression"].as_str().unwrap_or_default().to_owned();
            Box::pin(async move {
                let parts: Vec<&str> = expression.split_whitespace().collect();
                let [a, op, b] = parts.as_slice() else {
                    return Err(format!("cannot evaluate {expression}"));
                };
                let (a, b): (i64, i64) = (
                    a.parse().map_err(|_| "bad number")?,
                    b.parse().map_err(|_| "bad number")?,
                );
                let value = match *op {
                    "+" => a + b,
                    "-" => a - b,
                    "*" => a * b,
                    _ => return Err(format!("unknown operator {op}")),
                };
                Ok(AgentToolResult::text(format!("{expression} = {value}")))
            })
        },
    ))
}

fn text_of(message: &AgentMessage) -> String {
    match message {
        AgentMessage::Llm(Message::Assistant(assistant)) => {
            bake_ai::utils::text::assistant_text(assistant)
        }
        AgentMessage::Llm(Message::ToolResult(result)) => result
            .content
            .iter()
            .filter_map(|block| match block {
                UserContentBlock::Text(text) => Some(text.text.as_str()),
                UserContentBlock::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Pi: "handles a basic text prompt".
#[tokio::test]
async fn handles_a_basic_text_prompt() {
    let (provider, registry) = faux(FauxProviderOptions::default());
    provider.set_responses(vec![FauxResponseStep::Message(faux_assistant_message("4"))]);
    let agent = agent_with(
        &provider,
        registry,
        "You are a helpful assistant. Keep your responses concise.",
        Vec::new(),
    );
    agent
        .prompt("What is 2+2? Answer with just the number.")
        .await
        .unwrap();
    assert!(!agent.is_streaming());
    let messages = agent.messages();
    assert_eq!(roles(&messages), ["system", "user", "assistant"]);
    assert!(text_of(&messages[2]).contains('4'));
}

/// Pi: "executes tools and tracks pending tool calls".
#[tokio::test]
async fn executes_tools_and_tracks_pending_tool_calls() {
    let (provider, registry) = faux(FauxProviderOptions::default());
    provider.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_blocks(
            vec![
                faux_text("Let me calculate that."),
                faux_tool_call(
                    "calculate",
                    json!({ "expression": "123 * 456" }),
                    Some("calc-1"),
                ),
            ],
            StopReason::ToolUse,
        )),
        FauxResponseStep::Message(faux_assistant_message("The result is 56088.")),
    ]);
    let agent = Arc::new(agent_with(
        &provider,
        registry,
        "You are a helpful assistant. Always use the calculator tool for math.",
        vec![calculate_tool()],
    ));
    let pending = Arc::new(Mutex::new(Vec::new()));
    let (p, weak) = (Arc::clone(&pending), Arc::downgrade(&agent));
    agent.subscribe_fn(move |event, _| {
        if matches!(
            event,
            AgentEvent::ToolExecutionStart { .. } | AgentEvent::ToolExecutionEnd { .. }
        ) && let Some(agent) = weak.upgrade()
        {
            p.lock().unwrap().push((
                event.kind(),
                agent.pending_tool_calls().into_iter().collect::<Vec<_>>(),
            ));
        }
    });
    agent
        .prompt("Calculate 123 * 456 using the calculator tool.")
        .await
        .unwrap();
    assert!(!agent.is_streaming());
    let messages = agent.messages();
    assert!(messages.len() >= 4);
    let result = messages.iter().find(|m| m.role() == "toolResult").unwrap();
    assert!(text_of(result).contains("123 * 456 = 56088"));
    assert!(text_of(messages.last().unwrap()).contains("56088"));
    assert!(agent.pending_tool_calls().is_empty());
    assert_eq!(
        *pending.lock().unwrap(),
        [
            ("tool_execution_start", vec!["calc-1".to_owned()]),
            ("tool_execution_end", Vec::new())
        ]
    );
}

/// Pi: "handles abort during streaming".
#[tokio::test(start_paused = true)]
async fn handles_abort_during_streaming() {
    let (provider, registry) = faux(FauxProviderOptions {
        tokens_per_second: Some(20.0),
        token_size_min: Some(2),
        token_size_max: Some(2),
        ..FauxProviderOptions::default()
    });
    provider.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen",
    ))]);
    let agent = Arc::new(agent_with(
        &provider,
        registry,
        "You are a helpful assistant.",
        Vec::new(),
    ));
    let aborter = Arc::clone(&agent);
    let abort = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        aborter.abort();
    });
    agent.prompt("Count slowly from 1 to 20.").await.unwrap();
    abort.await.unwrap();
    assert!(!agent.is_streaming());
    let messages = agent.messages();
    assert!(messages.len() >= 2);
    let last = messages
        .last()
        .and_then(AgentMessage::as_assistant)
        .unwrap();
    assert_eq!(last.stop_reason, StopReason::Aborted);
    assert!(last.error_message.is_some());
    assert_eq!(agent.error_message(), last.error_message);
}

/// Pi: "emits lifecycle updates while streaming".
#[tokio::test]
async fn emits_lifecycle_updates_while_streaming() {
    let (provider, registry) = faux(FauxProviderOptions {
        token_size_min: Some(1),
        token_size_max: Some(1),
        ..FauxProviderOptions::default()
    });
    provider.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        "1 2 3 4 5",
    ))]);
    let agent = agent_with(
        &provider,
        registry,
        "You are a helpful assistant.",
        Vec::new(),
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let e = Arc::clone(&events);
    agent.subscribe_fn(move |event, _| e.lock().unwrap().push(event.kind()));
    agent.prompt("Count from 1 to 5.").await.unwrap();
    let events = events.lock().unwrap();
    for kind in [
        "agent_start",
        "turn_start",
        "message_start",
        "message_update",
        "message_end",
        "turn_end",
        "agent_end",
    ] {
        assert!(events.contains(&kind), "{kind}");
    }
    let first = |kind| events.iter().position(|k| *k == kind).unwrap();
    assert!(first("agent_start") < first("message_start"));
    assert!(first("message_start") < first("message_end"));
    assert!(first("message_end") < events.iter().rposition(|k| *k == "agent_end").unwrap());
    assert!(!agent.is_streaming());
    assert_eq!(agent.messages().len(), 3);
}

/// Pi: "maintains context across multiple turns".
#[tokio::test]
async fn maintains_context_across_turns() {
    let (provider, registry) = faux(FauxProviderOptions::default());
    provider.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message("Nice to meet you, Alice.")),
        FauxResponseStep::from_fn(|context, _, _, _| {
            let has_alice = context.messages().iter().any(|message| match message {
                Message::User(UserMessage { content: UserContent::Text(text), .. }) => text.contains("Alice"),
                Message::User(UserMessage { content: UserContent::Blocks(blocks), .. }) => blocks
                    .iter()
                    .any(|block| matches!(block, UserContentBlock::Text(text) if text.text.contains("Alice"))),
                _ => false,
            });
            Ok(faux_assistant_message(if has_alice { "Your name is Alice." } else { "I do not know your name." }))
        }),
    ]);
    let agent = agent_with(
        &provider,
        registry,
        "You are a helpful assistant.",
        Vec::new(),
    );
    agent.prompt("My name is Alice.").await.unwrap();
    assert_eq!(agent.messages().len(), 3);
    agent.prompt("What is my name?").await.unwrap();
    let messages = agent.messages();
    assert_eq!(messages.len(), 5);
    assert!(text_of(&messages[4]).to_lowercase().contains("alice"));
}

/// Pi: "preserves thinking content blocks".
#[tokio::test]
async fn preserves_thinking_content_blocks() {
    let (provider, registry) = faux(FauxProviderOptions {
        models: vec![FauxModelDefinition {
            id: "faux-reasoning".to_owned(),
            reasoning: true,
            ..FauxModelDefinition::default()
        }],
        ..FauxProviderOptions::default()
    });
    provider.set_responses(vec![FauxResponseStep::Message(faux_assistant_blocks(
        vec![faux_thinking("step by step"), faux_text("4")],
        StopReason::Stop,
    ))]);
    let agent = agent_with(
        &provider,
        registry,
        "You are a helpful assistant.",
        Vec::new(),
    );
    agent.set_thinking_level(ModelThinkingLevel::Low);
    agent.prompt("What is 2+2?").await.unwrap();
    let messages = agent.messages();
    let assistant = messages[2].as_assistant().unwrap();
    assert_eq!(
        assistant.content,
        [faux_thinking("step by step"), faux_text("4")]
    );
}

/// Pi `Agent.continue()`: "throws when no messages in context" and "throws
/// when last message is assistant".
#[tokio::test]
async fn continue_validates_the_transcript() {
    let (provider, registry) = faux(FauxProviderOptions::default());
    let agent = agent_with(&provider, registry, "Test", Vec::new());
    assert_eq!(
        agent.continue_run().await.unwrap_err().to_string(),
        "No messages to continue from"
    );
    let mut hello: AssistantMessage = AssistantMessage::pending(provider.model());
    hello.content = vec![faux_text("Hello")];
    hello.stop_reason = StopReason::Stop;
    agent.set_messages(vec![hello.into()]);
    assert_eq!(
        agent.continue_run().await.unwrap_err().to_string(),
        "Cannot continue from message role: assistant"
    );
}

fn user_blocks(text: &str) -> AgentMessage {
    UserMessage {
        content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))]),
        timestamp: bake_ai::now_ms(),
    }
    .into()
}

/// Pi `Agent.continue()`: "continues and gets a response when last message
/// is user".
#[tokio::test]
async fn continue_from_a_user_message() {
    let (provider, registry) = faux(FauxProviderOptions::default());
    provider.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        "HELLO WORLD",
    ))]);
    let agent = agent_with(
        &provider,
        registry,
        "You are a helpful assistant. Follow instructions exactly.",
        Vec::new(),
    );
    agent.set_messages(vec![user_blocks("Say exactly: HELLO WORLD")]);
    agent.continue_run().await.unwrap();
    assert!(!agent.is_streaming());
    let messages = agent.messages();
    assert_eq!(roles(&messages), ["user", "assistant"]);
    assert!(text_of(&messages[1]).to_uppercase().contains("HELLO WORLD"));
}

/// Pi `Agent.continue()`: "continues and processes tool results".
#[tokio::test]
async fn continue_from_a_tool_result() {
    let (provider, registry) = faux(FauxProviderOptions::default());
    provider.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        "The answer is 8.",
    ))]);
    let agent = agent_with(
        &provider,
        registry,
        "You are a helpful assistant. After getting a calculation result, state the answer clearly.",
        vec![calculate_tool()],
    );
    let mut assistant = AssistantMessage::pending(provider.model());
    assistant.content = vec![
        faux_text("Let me calculate that."),
        faux_tool_call(
            "calculate",
            json!({ "expression": "5 + 3" }),
            Some("calc-1"),
        ),
    ];
    assistant.stop_reason = StopReason::ToolUse;
    let result: AgentMessage = ToolResultMessage {
        tool_call_id: "calc-1".to_owned(),
        tool_name: "calculate".to_owned(),
        content: vec![UserContentBlock::Text(TextContent::new("5 + 3 = 8"))],
        timestamp: bake_ai::now_ms(),
        ..ToolResultMessage::default()
    }
    .into();
    agent.set_messages(vec![
        user_blocks("What is 5 + 3?"),
        assistant.into(),
        result,
    ]);
    agent.continue_run().await.unwrap();
    assert!(!agent.is_streaming());
    let messages = agent.messages();
    assert!(messages.len() >= 4);
    assert_eq!(messages.last().unwrap().role(), "assistant");
    assert!(text_of(messages.last().unwrap()).contains('8'));
}
