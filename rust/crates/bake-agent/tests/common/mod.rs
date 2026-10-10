//! Fixtures shared by the ported Pi tests: Pi's `createModel`,
//! `createAssistantMessage`, `createUserMessage`, `identityConverter`, and
//! a scripted stream function standing in for Pi's
//! `createAssistantMessageEventStream` mocks.
#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bake_agent::{
    AgentEvent, AgentEventSink, AgentLoopStream, AgentMessage, AgentTool, AgentToolResult,
    ConvertToLlm, StreamFn, hook,
};
use bake_ai::{
    AssistantContentBlock, AssistantMessage, InputModality, JsonObject, Message, Model, ModelCost,
    SimpleStreamOptions, StopReason, TextContent, ToolCall, TranscriptContext, Usage, UserContent,
    UserMessage, assistant_message_channel,
};
use serde_json::{Value, json};

pub fn model() -> Model {
    Model {
        id: "mock".to_owned(),
        name: "mock".to_owned(),
        api: "openai-responses".to_owned(),
        provider: "openai".to_owned(),
        base_url: "https://example.invalid".to_owned(),
        input: vec![InputModality::Text],
        input_limits: None,
        cost: ModelCost::default(),
        headers: None,
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 8192,
        max_tokens: 2048,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    }
}

pub fn assistant(content: Vec<AssistantContentBlock>, stop_reason: StopReason) -> AssistantMessage {
    AssistantMessage {
        content,
        api: "openai-responses".to_owned(),
        provider: "openai".to_owned(),
        model: "mock".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: bake_ai::now_ms(),
        duration_ms: None,
    }
}

pub fn assistant_text(text: &str) -> AssistantMessage {
    assistant(vec![text_block(text)], StopReason::Stop)
}

pub fn failed(reason: StopReason) -> AssistantMessage {
    let mut message = assistant(Vec::new(), reason);
    message.error_message = Some(reason.as_str().to_owned());
    message
}

pub fn text_block(text: &str) -> AssistantContentBlock {
    AssistantContentBlock::Text(TextContent::new(text))
}

pub fn call(id: &str, name: &str, arguments: Value) -> AssistantContentBlock {
    AssistantContentBlock::ToolCall(tool_call(id, name, arguments))
}

pub fn tool_call(id: &str, name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: match arguments {
            Value::Object(object) => object,
            _ => JsonObject::new(),
        },
        thought_signature: None,
        namespace: None,
    }
}

pub fn tool_use(calls: Vec<AssistantContentBlock>) -> AssistantMessage {
    assistant(calls, StopReason::ToolUse)
}

pub fn user(text: &str) -> AgentMessage {
    UserMessage {
        content: UserContent::Text(text.to_owned()),
        timestamp: bake_ai::now_ms(),
    }
    .into()
}

/// The text of a string-content user message.
pub fn user_text(message: &Message) -> Option<&str> {
    match message {
        Message::User(UserMessage {
            content: UserContent::Text(text),
            ..
        }) => Some(text),
        _ => None,
    }
}

/// The string-content user messages of a provider request.
pub fn request_users(context: &TranscriptContext) -> Vec<String> {
    context
        .messages()
        .iter()
        .filter_map(user_text)
        .map(str::to_owned)
        .collect()
}

pub fn identity_converter() -> ConvertToLlm {
    hook::convert_to_llm(|messages| {
        let converted: Vec<Message> = messages
            .iter()
            .filter_map(|m| m.as_llm().cloned())
            .collect();
        Box::pin(async move { converted })
    })
}

/// A stream function that answers each request with `respond(call_index,
/// context, options)`, finished at once: `done` for completed stop reasons,
/// `error` for `error` and `aborted`.
pub fn scripted<F>(respond: F) -> StreamFn
where
    F: Fn(usize, &TranscriptContext, &SimpleStreamOptions) -> AssistantMessage
        + Send
        + Sync
        + 'static,
{
    let calls = AtomicUsize::new(0);
    Arc::new(move |_model, context, options| {
        let index = calls.fetch_add(1, Ordering::SeqCst);
        let message = respond(index, context, &options);
        let (sender, stream) = assistant_message_channel();
        sender.finish(message);
        Ok(stream)
    })
}

/// Tool calls on the first request, then `done`.
pub fn tools_then_done(calls: Vec<AssistantContentBlock>) -> StreamFn {
    scripted(move |index, _, _| {
        if index == 0 {
            tool_use(calls.clone())
        } else {
            assistant_text("done")
        }
    })
}

pub fn value_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "value": { "type": "string" } },
        "required": ["value"],
    })
}

pub fn empty_schema() -> Value {
    json!({ "type": "object", "properties": {} })
}

/// Pi's `echo` tool: records `value` and returns `echoed: <value>`.
pub fn echo_tool(executed: Arc<Mutex<Vec<String>>>) -> AgentTool {
    AgentTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        move |_id, params, _signal, _update| {
            let value = params["value"].as_str().unwrap_or_default().to_owned();
            executed.lock().unwrap().push(value.clone());
            Box::pin(async move {
                Ok(AgentToolResult {
                    details: Some(json!({ "value": value })),
                    ..AgentToolResult::text(format!("echoed: {value}"))
                })
            })
        },
    )
}

/// A sink that records events.
pub fn recorder() -> (AgentEventSink, Arc<Mutex<Vec<AgentEvent>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink: AgentEventSink = {
        let events = Arc::clone(&events);
        Arc::new(move |event| {
            events.lock().unwrap().push(event);
            Box::pin(async {})
        })
    };
    (sink, events)
}

pub async fn drain(stream: &AgentLoopStream) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

pub fn kinds(events: &[AgentEvent]) -> Vec<&'static str> {
    events.iter().map(AgentEvent::kind).collect()
}

pub fn roles(messages: &[AgentMessage]) -> Vec<String> {
    messages.iter().map(|m| m.role().to_owned()).collect()
}

pub fn tool_result_ids(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::MessageEnd { message } => message
                .as_tool_result()
                .map(|result| result.tool_call_id.clone()),
            _ => None,
        })
        .collect()
}

pub fn tool_end_ids(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolExecutionEnd { tool_call_id, .. } => Some(tool_call_id.clone()),
            _ => None,
        })
        .collect()
}

pub fn result_text(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            bake_ai::UserContentBlock::Text(text) => Some(text.text.as_str()),
            bake_ai::UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("")
}
