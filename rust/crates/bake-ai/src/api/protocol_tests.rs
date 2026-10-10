//! Wire tests for the three protocols against a loopback server.
//!
//! Cases follow Pi's `anthropic-sse-parsing.test.ts`,
//! `openai-completions-*.test.ts`, and
//! `openai-responses-terminal-event.test.ts` (v1.1.0), with the SDK clients
//! replaced by real HTTP to a local server.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::api::{
    anthropic_messages, openai_completions, openai_responses, openai_responses_shared,
};
use crate::options::StreamOptions;
use crate::providers::faux::faux_model;
use crate::test_server::{Reply, serve, sse_data, sse_events};
use crate::transcript::normalize_context;
use crate::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, Message, Model,
    StopReason, Tool, TranscriptContext, UserMessage,
};
use crate::utils::abort::AbortController;
use crate::utils::event_stream::{AssistantMessageEventStream, assistant_message_channel};

fn model(api: &str, provider: &str, base_url: &str) -> Model {
    let mut model = faux_model("test-model");
    model.api = api.to_owned();
    model.provider = provider.to_owned();
    model.base_url = base_url.to_owned();
    model
}

fn context(with_tool: bool) -> TranscriptContext {
    normalize_context(Context {
        system_prompt: Some("Be brief.".into()),
        messages: vec![Message::User(UserMessage { content: "Hello".into(), timestamp: 1 })],
        tools: with_tool.then(|| {
            vec![Tool {
                name: "bash".into(),
                description: "Run a command".into(),
                parameters: json!({ "type": "object", "properties": { "command": { "type": "string" } }, "required": ["command"] }),
                constrained_sampling: None,
            }]
        }),
    })
}

fn options() -> StreamOptions {
    StreamOptions {
        api_key: Some("sk-test".into()),
        max_retries: Some(0),
        ..StreamOptions::default()
    }
}

async fn run(stream: AssistantMessageEventStream) -> (Vec<String>, AssistantMessage) {
    let events = stream.collect().await;
    let names = events
        .iter()
        .map(|event| {
            serde_json::to_value(event)
                .ok()
                .and_then(|value| value["type"].as_str().map(str::to_owned))
                .unwrap_or_default()
        })
        .collect();
    let result = stream.result().await.expect("a final message");
    (names, result)
}

fn tool_call(message: &AssistantMessage) -> Option<&crate::types::ToolCall> {
    message.content.iter().find_map(|block| match block {
        AssistantContentBlock::ToolCall(call) => Some(call),
        _ => None,
    })
}

// ---- openai-completions ----

#[tokio::test]
async fn completions_streams_text_reasoning_and_tool_calls() {
    let body = sse_data(&[
        json!({ "id": "chatcmpl-1", "model": "test-model", "choices": [{ "index": 0, "delta": { "reasoning_content": "Think" } }] }),
        json!({ "id": "chatcmpl-1", "choices": [{ "index": 0, "delta": { "content": "Hi" } }] }),
        json!({ "id": "chatcmpl-1", "choices": [{ "index": 0, "delta": { "tool_calls": [{ "index": 0, "id": "call_1", "function": { "name": "bash", "arguments": "{\"comm" } }] } }] }),
        json!({ "id": "chatcmpl-1", "choices": [{ "index": 0, "delta": { "tool_calls": [{ "index": 0, "function": { "arguments": "and\":\"ls\"}" } }] } }] }),
        json!({ "id": "chatcmpl-1", "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }] }),
        json!({ "id": "chatcmpl-1", "choices": [], "usage": { "prompt_tokens": 20, "completion_tokens": 7, "prompt_tokens_details": { "cached_tokens": 5 } } }),
    ]);
    let server = serve(vec![Reply::sse(body)]).await;
    let model = model(
        "openai-completions",
        "openai",
        &format!("{}/v1", server.base),
    );
    let observed = Arc::new(Mutex::new(0usize));
    let counter = Arc::clone(&observed);
    let mut options = options();
    options.on_provider_stream_event = Some(Arc::new(move |_, _| {
        if let Ok(mut count) = counter.lock() {
            *count += 1;
        }
    }));
    let stream = openai_completions::stream(
        &model,
        &context(true),
        openai_completions::OpenAICompletionsOptions {
            base: options,
            ..Default::default()
        },
    );
    let (names, result) = run(stream).await;
    assert_eq!(
        result.stop_reason,
        StopReason::ToolUse,
        "{:?}",
        result.error_message
    );
    assert_eq!(
        names,
        [
            "start",
            "thinking_start",
            "thinking_delta",
            "text_start",
            "text_delta",
            "toolcall_start",
            "toolcall_delta",
            "toolcall_delta",
            "thinking_end",
            "text_end",
            "toolcall_end",
            "done"
        ]
    );
    assert_eq!(*observed.lock().unwrap(), 6);
    assert_eq!(result.response_id.as_deref(), Some("chatcmpl-1"));
    let call = tool_call(&result).expect("a tool call");
    assert_eq!(call.id, "call_1");
    assert_eq!(
        Value::Object(call.arguments.clone()),
        json!({ "command": "ls" })
    );
    assert_eq!(
        (
            result.usage.input,
            result.usage.output,
            result.usage.cache_read,
            result.usage.total_tokens
        ),
        (15, 7, 5, 27)
    );
    let request = &server.requests()[0];
    assert_eq!(request.line, "POST /v1/chat/completions");
    assert_eq!(request.header("authorization"), Some("Bearer sk-test"));
    assert_eq!(request.body["stream"], json!(true));
    assert_eq!(
        request.body["stream_options"],
        json!({ "include_usage": true })
    );
    assert_eq!(request.body["store"], json!(false));
    assert_eq!(
        request.body["messages"][0],
        json!({ "role": "system", "content": "Be brief." })
    );
    assert_eq!(
        request.body["messages"][1],
        json!({ "role": "user", "content": "Hello" })
    );
    assert_eq!(request.body["tools"][0]["function"]["name"], json!("bash"));
}

#[tokio::test]
async fn completions_requires_a_finish_reason() {
    let body = sse_data(&[
        json!({ "id": "c", "choices": [{ "index": 0, "delta": { "content": "Hi" } }] }),
    ]);
    let server = serve(vec![Reply::sse(body)]).await;
    let model = model("openai-completions", "openai", &server.base);
    let stream = openai_completions::stream(
        &model,
        &context(false),
        openai_completions::OpenAICompletionsOptions {
            base: options(),
            ..Default::default()
        },
    );
    let (names, result) = run(stream).await;
    assert_eq!(names.last().map(String::as_str), Some("error"));
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Stream ended without finish_reason")
    );
}

#[tokio::test]
async fn completions_maps_unknown_finish_reasons_to_errors() {
    let body = sse_data(&[
        json!({ "id": "c", "choices": [{ "index": 0, "delta": {}, "finish_reason": "content_filter" }] }),
    ]);
    let server = serve(vec![Reply::sse(body)]).await;
    let model = model("openai-completions", "openai", &server.base);
    let stream = openai_completions::stream(
        &model,
        &context(false),
        openai_completions::OpenAICompletionsOptions {
            base: options(),
            ..Default::default()
        },
    );
    let (_, result) = run(stream).await;
    assert_eq!(result.raw_stop_reason.as_deref(), Some("content_filter"));
    assert_eq!(
        result.error_message.as_deref(),
        Some("Provider finish_reason: content_filter")
    );
}

#[tokio::test]
async fn completions_surfaces_http_errors_with_the_body() {
    let server = serve(vec![Reply::error(
        400,
        r#"{"error":{"message":"bad model","code":"invalid"}}"#,
    )])
    .await;
    let model = model("openai-completions", "openai", &server.base);
    let stream = openai_completions::stream(
        &model,
        &context(false),
        openai_completions::OpenAICompletionsOptions {
            base: options(),
            ..Default::default()
        },
    );
    let (names, result) = run(stream).await;
    assert_eq!(names, ["error"]);
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some(r#"400: {"message":"bad model","code":"invalid"}"#)
    );
}

#[tokio::test]
async fn completions_reports_stream_error_payloads() {
    let body = "data: {\"error\":{\"message\":\"upstream overloaded\"}}\n\n";
    let server = serve(vec![Reply::sse(body)]).await;
    let model = model("openai-completions", "openai", &server.base);
    let stream = openai_completions::stream(
        &model,
        &context(false),
        openai_completions::OpenAICompletionsOptions {
            base: options(),
            ..Default::default()
        },
    );
    let (_, result) = run(stream).await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(result.error_message.as_deref(), Some("upstream overloaded"));
}

// The OpenAI SDK parses each event with `JSON.parse`, so a raw control
// character that the Anthropic path would repair fails the stream here.
#[tokio::test]
async fn completions_reject_malformed_event_json() {
    let body =
        "data: {\"id\":\"c\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\tb\"}}]}\n\n";
    let server = serve(vec![Reply::sse(body)]).await;
    let model = model("openai-completions", "openai", &server.base);
    let stream = openai_completions::stream(
        &model,
        &context(false),
        openai_completions::OpenAICompletionsOptions {
            base: options(),
            ..Default::default()
        },
    );
    let (_, result) = run(stream).await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Error reading response: malformed server-sent event JSON.")
    );
}

#[tokio::test]
async fn completions_aborts_a_hanging_stream() {
    let mut reply = Reply::sse(
        "data: {\"id\":\"c\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"}}]}\n\n",
    );
    reply.hang = true;
    let server = serve(vec![reply]).await;
    let model = model("openai-completions", "openai", &server.base);
    let controller = AbortController::new();
    let mut base = options();
    base.signal = Some(controller.signal());
    let stream = openai_completions::stream(
        &model,
        &context(false),
        openai_completions::OpenAICompletionsOptions {
            base,
            ..Default::default()
        },
    );
    let first = stream.next().await.expect("start");
    assert!(matches!(first, AssistantMessageEvent::Start { .. }));
    tokio::time::sleep(Duration::from_millis(50)).await;
    controller.abort();
    // A missed abort leaves the body hanging; fail instead of waiting on it.
    let result = tokio::time::timeout(Duration::from_secs(10), stream.result())
        .await
        .expect("the abort ends the stream")
        .expect("a final message");
    assert_eq!(result.stop_reason, StopReason::Aborted);
    assert_eq!(result.error_message.as_deref(), Some("Request was aborted"));
}

#[test]
fn completions_detects_compat_from_the_route() {
    let mut deepseek = faux_model("deepseek-chat");
    deepseek.provider = "deepseek".into();
    deepseek.base_url = "https://api.deepseek.com".into();
    let compat = openai_completions::detect_compat(&deepseek);
    assert!(!compat.supports_store);
    assert_eq!(
        compat.max_tokens_field,
        crate::types::MaxTokensField::MaxTokens
    );
    assert!(compat.requires_reasoning_content_on_assistant_messages);
    let proxy = model(
        "openai-completions",
        "cliproxyapi",
        "http://127.0.0.1:8317/v1",
    );
    let compat = openai_completions::detect_compat(&proxy);
    assert!(compat.supports_store && compat.supports_developer_role);
}

// ---- openai-responses ----

fn responses_processor() -> (
    openai_responses_shared::ResponsesStreamProcessor,
    crate::AssistantMessageEventSender,
) {
    let model = model("openai-responses", "openai", "https://api.openai.com/v1");
    let output = AssistantMessage::pending(&model);
    let (sender, _stream) = assistant_message_channel();
    (
        openai_responses_shared::ResponsesStreamProcessor::new(&model, output, None, None),
        sender,
    )
}

fn feed(events: &[Value]) -> (Result<(), String>, AssistantMessage) {
    let (mut processor, sender) = responses_processor();
    for event in events {
        if let Err(error) = processor.handle_event(event, &sender) {
            return (Err(error), processor.output);
        }
    }
    (processor.finish(), processor.output)
}

#[test]
fn responses_require_a_terminal_event() {
    let (result, _) =
        feed(&[json!({ "type": "response.created", "response": { "id": "resp_eof" } })]);
    assert_eq!(
        result,
        Err("OpenAI Responses stream ended before a terminal response event".to_owned())
    );
}

#[test]
fn responses_reject_unfinished_tool_calls() {
    let (result, _) = feed(&[
        json!({ "type": "response.output_item.added", "output_index": 0, "item": { "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "bash", "arguments": "" } }),
        json!({ "type": "response.function_call_arguments.delta", "output_index": 0, "item_id": "fc_1", "delta": "{\"command\":\"rm -rf /tmp/build" }),
        json!({ "type": "response.completed", "response": { "id": "resp_unfinished", "status": "completed" } }),
    ]);
    assert_eq!(
        result,
        Err(
            "OpenAI Responses stream completed with an unfinished tool call: bash (call_1|fc_1)"
                .to_owned()
        )
    );
}

#[test]
fn responses_reject_parallel_calls_without_output_index() {
    let call = |n: &str, arguments: &str| json!({ "type": "function_call", "id": format!("fc_{n}"), "call_id": format!("call_{n}"), "name": "bash", "arguments": arguments });
    let (result, _) = feed(&[
        json!({ "type": "response.output_item.added", "item": call("a", "") }),
        json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_a", "delta": "{\"command\":\"echo a\"}" }),
        json!({ "type": "response.output_item.added", "item": call("b", "") }),
        json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_b", "delta": "{\"command\":\"echo b\"}" }),
        json!({ "type": "response.output_item.done", "item": call("a", "{\"command\":\"echo a\"}") }),
        json!({ "type": "response.output_item.done", "item": call("b", "{\"command\":\"echo b\"}") }),
        json!({ "type": "response.completed", "response": { "id": "resp_no_output_index", "status": "completed" } }),
    ]);
    assert_eq!(
        result,
        Err(
            "OpenAI Responses stream completed with an unfinished tool call: bash (call_a|fc_a)"
                .to_owned()
        )
    );
}

#[test]
fn responses_finalize_terminal_statuses() {
    let (result, output) = feed(&[json!({
        "type": "response.completed",
        "response": { "id": "resp_completed", "status": "completed", "usage": {
            "input_tokens": 20, "output_tokens": 7, "total_tokens": 27,
            "input_tokens_details": { "cached_tokens": 2, "cache_write_tokens": 3 }
        } }
    })]);
    assert_eq!(result, Ok(()));
    assert_eq!(output.response_id.as_deref(), Some("resp_completed"));
    assert_eq!(output.stop_reason, StopReason::Stop);
    assert_eq!(output.raw_stop_reason.as_deref(), Some("completed"));
    assert_eq!(
        (
            output.usage.input,
            output.usage.output,
            output.usage.cache_read,
            output.usage.cache_write,
            output.usage.total_tokens
        ),
        (15, 7, 2, 3, 27)
    );

    let incomplete = |reason: &str| {
        feed(&[json!({ "type": "response.incomplete", "response": { "id": "resp_incomplete", "status": "incomplete", "incomplete_details": { "reason": reason } } })]).1
    };
    let length = incomplete("max_output_tokens");
    assert_eq!(length.stop_reason, StopReason::Length);
    assert_eq!(
        length.raw_stop_reason.as_deref(),
        Some("incomplete.max_output_tokens")
    );
    let filtered = incomplete("content_filter");
    assert_eq!(filtered.stop_reason, StopReason::Error);
    assert_eq!(
        filtered.error_message.as_deref(),
        Some("Response incomplete: content_filter")
    );
    assert_eq!(
        incomplete("max_time_limit").error_message.as_deref(),
        Some("Response incomplete: max_time_limit")
    );

    let (result, output) = feed(&[
        json!({ "type": "response.failed", "response": { "id": "resp_failed", "status": "failed", "error": { "code": "server_error", "message": "boom" } } }),
    ]);
    assert_eq!(result, Err("server_error: boom".to_owned()));
    assert_eq!(output.raw_stop_reason.as_deref(), Some("failed"));
}

#[test]
fn responses_track_message_phases() {
    for (phases, terminal, expected_stop) in [
        (["commentary", "commentary"], "completed", StopReason::Stop),
        (
            ["final_answer", "final_answer"],
            "completed",
            StopReason::Stop,
        ),
        (
            ["final_answer", "final_answer"],
            "incomplete",
            StopReason::Length,
        ),
    ] {
        let (mut processor, sender) = responses_processor();
        let item = |phase: &str, status: &str| {
            json!({ "type": "message", "id": "msg_phase", "role": "assistant", "status": status,
                "content": [{ "type": "output_text", "text": "answer", "annotations": [] }], "phase": phase })
        };
        processor
            .handle_event(&json!({ "type": "response.output_item.added", "output_index": 0, "item": item(phases[0], "in_progress") }), &sender)
            .unwrap();
        let after_added = processor.output.stop_reason;
        processor
            .handle_event(&json!({ "type": "response.output_item.done", "output_index": 0, "item": item(phases[1], "completed") }), &sender)
            .unwrap();
        let terminal_event = if terminal == "completed" {
            json!({ "type": "response.completed", "response": { "id": "r", "status": "completed" } })
        } else {
            json!({ "type": "response.incomplete", "response": { "id": "r", "status": "incomplete", "incomplete_details": { "reason": "max_output_tokens" } } })
        };
        processor.handle_event(&terminal_event, &sender).unwrap();
        let expected_added = if phases[0] == "final_answer" {
            StopReason::Stop
        } else {
            StopReason::Pending
        };
        assert_eq!(after_added, expected_added);
        assert_eq!(processor.output.stop_reason, expected_stop);
        match processor.output.content.first() {
            Some(AssistantContentBlock::Text(text)) => {
                assert_eq!(text.text, "answer");
                assert_eq!(
                    text.text_signature.as_deref(),
                    Some(
                        format!(
                            "{{\"v\":1,\"id\":\"msg_phase\",\"phase\":\"{}\"}}",
                            phases[1]
                        )
                        .as_str()
                    )
                );
            }
            other => panic!("expected text, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn responses_stream_over_http() {
    let body = sse_events(&[
        (
            "response.created",
            json!({ "type": "response.created", "response": { "id": "resp_1" } }),
        ),
        (
            "response.output_item.added",
            json!({ "type": "response.output_item.added", "output_index": 0, "item": { "type": "reasoning", "id": "rs_1", "summary": [] } }),
        ),
        (
            "response.reasoning_summary_text.delta",
            json!({ "type": "response.reasoning_summary_text.delta", "output_index": 0, "delta": "Plan" }),
        ),
        (
            "response.output_item.done",
            json!({ "type": "response.output_item.done", "output_index": 0, "item": { "type": "reasoning", "id": "rs_1", "summary": [{ "type": "summary_text", "text": "Plan" }] } }),
        ),
        (
            "response.output_item.added",
            json!({ "type": "response.output_item.added", "output_index": 1, "item": { "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "bash", "arguments": "" } }),
        ),
        (
            "response.function_call_arguments.delta",
            json!({ "type": "response.function_call_arguments.delta", "output_index": 1, "delta": "{\"command\":" }),
        ),
        (
            "response.function_call_arguments.done",
            json!({ "type": "response.function_call_arguments.done", "output_index": 1, "arguments": "{\"command\":\"ls\"}" }),
        ),
        (
            "response.output_item.done",
            json!({ "type": "response.output_item.done", "output_index": 1, "item": { "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "bash", "arguments": "{\"command\":\"ls\"}" } }),
        ),
        (
            "response.completed",
            json!({ "type": "response.completed", "response": { "id": "resp_1", "status": "completed", "usage": { "input_tokens": 10, "output_tokens": 4, "total_tokens": 14 } } }),
        ),
    ]);
    let server = serve(vec![Reply::sse(body)]).await;
    let model = model(
        "openai-responses",
        "cliproxyapi",
        &format!("{}/v1", server.base),
    );
    let mut base = options();
    base.session_id = Some("session-1".into());
    let stream = openai_responses::stream(
        &model,
        &context(true),
        openai_responses::OpenAIResponsesOptions {
            base,
            ..Default::default()
        },
    );
    let (names, result) = run(stream).await;
    assert_eq!(
        result.stop_reason,
        StopReason::ToolUse,
        "{:?}",
        result.error_message
    );
    assert_eq!(
        names,
        [
            "start",
            "thinking_start",
            "thinking_delta",
            "thinking_end",
            "toolcall_start",
            "toolcall_delta",
            "toolcall_delta",
            "toolcall_end",
            "done"
        ]
    );
    let call = tool_call(&result).expect("a tool call");
    assert_eq!(call.id, "call_1|fc_1");
    assert_eq!(
        Value::Object(call.arguments.clone()),
        json!({ "command": "ls" })
    );
    assert_eq!(result.usage.total_tokens, 14);
    let request = &server.requests()[0];
    assert_eq!(request.line, "POST /v1/responses");
    assert_eq!(request.header("session_id"), Some("session-1"));
    assert_eq!(request.body["prompt_cache_key"], json!("session-1"));
    assert_eq!(request.body["store"], json!(false));
    assert_eq!(
        request.body["input"][0],
        json!({ "role": "system", "content": "Be brief." })
    );
    assert_eq!(
        request.body["input"][1],
        json!({ "role": "user", "content": [{ "type": "input_text", "text": "Hello" }] })
    );
    assert_eq!(request.body["tools"][0]["name"], json!("bash"));
}

#[tokio::test]
async fn responses_report_failed_and_truncated_streams() {
    let failed = sse_events(&[(
        "response.failed",
        json!({ "type": "response.failed", "response": { "status": "failed", "error": { "code": "server_error", "message": "boom" } } }),
    )]);
    let truncated = sse_events(&[(
        "response.created",
        json!({ "type": "response.created", "response": { "id": "resp_eof" } }),
    )]);
    let server = serve(vec![Reply::sse(failed), Reply::sse(truncated)]).await;
    let model = model("openai-responses", "cliproxyapi", &server.base);
    for expected in [
        "server_error: boom",
        "OpenAI Responses stream ended before a terminal response event",
    ] {
        let stream = openai_responses::stream(
            &model,
            &context(false),
            openai_responses::OpenAIResponsesOptions {
                base: options(),
                ..Default::default()
            },
        );
        let (names, result) = run(stream).await;
        assert_eq!(names.first().map(String::as_str), Some("start"));
        assert_eq!(names.last().map(String::as_str), Some("error"));
        assert_eq!(result.stop_reason, StopReason::Error);
        assert_eq!(result.error_message.as_deref(), Some(expected));
    }
}

#[tokio::test]
async fn responses_prefix_http_errors_with_the_provider() {
    let server = serve(vec![Reply::error(
        401,
        r#"{"error":{"message":"bad key"}}"#,
    )])
    .await;
    let model = model("openai-responses", "cliproxyapi", &server.base);
    let stream = openai_responses::stream(
        &model,
        &context(false),
        openai_responses::OpenAIResponsesOptions {
            base: options(),
            ..Default::default()
        },
    );
    let (_, result) = run(stream).await;
    assert_eq!(
        result.error_message.as_deref(),
        Some(r#"cliproxyapi API error (401): {"message":"bad key"}"#)
    );
}

// ---- anthropic-messages ----

fn minimal_anthropic_events() -> String {
    sse_events(&[
        (
            "message_start",
            json!({ "type": "message_start", "message": { "id": "msg_test", "usage": { "input_tokens": 12, "output_tokens": 0, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0 } } }),
        ),
        (
            "content_block_start",
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        ),
        ("ping", json!({ "type": "ping" })),
        (
            "content_block_delta",
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Hello" } }),
        ),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }),
        ),
        (
            "message_delta",
            json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "input_tokens": 12, "output_tokens": 5, "cache_read_input_tokens": null, "cache_creation_input_tokens": 0 } }),
        ),
        ("message_stop", json!({ "type": "message_stop" })),
    ])
}

#[tokio::test]
async fn anthropic_streams_and_forwards_provider_events() {
    let server = serve(vec![Reply::sse(minimal_anthropic_events())]).await;
    let model = model("anthropic-messages", "cliproxyapi", &server.base);
    let observed = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&observed);
    let mut base = options();
    base.on_provider_stream_event = Some(Arc::new(move |event: &Value, _: &Model| {
        if let Ok(mut events) = sink.lock() {
            events.push(event["type"].as_str().unwrap_or("").to_owned());
        }
    }));
    let stream = anthropic_messages::stream(
        &model,
        &context(false),
        anthropic_messages::AnthropicOptions {
            base,
            ..Default::default()
        },
    );
    let (names, result) = run(stream).await;
    assert_eq!(
        result.stop_reason,
        StopReason::Stop,
        "{:?}",
        result.error_message
    );
    assert_eq!(
        names,
        ["start", "text_start", "text_delta", "text_end", "done"]
    );
    assert_eq!(
        *observed.lock().unwrap(),
        [
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop"
        ]
    );
    assert_eq!(
        (
            result.usage.input,
            result.usage.output,
            result.usage.total_tokens
        ),
        (12, 5, 17)
    );
    assert_eq!(result.response_id.as_deref(), Some("msg_test"));
    let request = &server.requests()[0];
    assert_eq!(request.line, "POST /v1/messages?beta=true");
    assert_eq!(request.header("x-api-key"), Some("sk-test"));
    assert_eq!(request.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(request.header("anthropic-beta"), None);
    assert_eq!(
        request.body["system"],
        json!([{ "type": "text", "text": "Be brief.", "cache_control": { "type": "ephemeral" } }])
    );
    assert_eq!(
        request.body["messages"],
        json!([{ "role": "user", "content": [{ "type": "text", "text": "Hello", "cache_control": { "type": "ephemeral" } }] }])
    );
    assert_eq!(request.body["max_tokens"], json!(model.max_tokens));
}

#[tokio::test]
async fn anthropic_assembles_thinking_and_tool_use() {
    let body = sse_events(&[
        (
            "message_start",
            json!({ "type": "message_start", "message": { "id": "msg_1", "model": "proxy-model", "usage": { "input_tokens": 3, "output_tokens": 0 } } }),
        ),
        (
            "content_block_start",
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "thinking", "thinking": "" } }),
        ),
        (
            "content_block_delta",
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "thinking_delta", "thinking": "Hmm" } }),
        ),
        (
            "content_block_delta",
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "signature_delta", "signature": "sig" } }),
        ),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }),
        ),
        (
            "content_block_start",
            json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "tool_use", "id": "toolu_1", "name": "bash", "input": {} } }),
        ),
        (
            "content_block_delta",
            json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "input_json_delta", "partial_json": "{\"command\":" } }),
        ),
        (
            "content_block_delta",
            json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "input_json_delta", "partial_json": "\"ls\"}" } }),
        ),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 1 }),
        ),
        (
            "message_delta",
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" }, "usage": { "output_tokens": 9 } }),
        ),
        ("message_stop", json!({ "type": "message_stop" })),
    ]);
    let server = serve(vec![Reply::sse(body)]).await;
    let mut model = model("anthropic-messages", "cliproxyapi", &server.base);
    model.reasoning = true;
    let stream = anthropic_messages::stream(
        &model,
        &context(true),
        anthropic_messages::AnthropicOptions {
            base: options(),
            thinking_enabled: Some(true),
            ..Default::default()
        },
    );
    let (_, result) = run(stream).await;
    assert_eq!(
        result.stop_reason,
        StopReason::ToolUse,
        "{:?}",
        result.error_message
    );
    assert_eq!(result.response_model.as_deref(), Some("proxy-model"));
    match result.content.first() {
        Some(AssistantContentBlock::Thinking(thinking)) => {
            assert_eq!(thinking.thinking, "Hmm");
            assert_eq!(thinking.thinking_signature.as_deref(), Some("sig"));
        }
        other => panic!("expected thinking, got {other:?}"),
    }
    let call = tool_call(&result).expect("a tool call");
    assert_eq!(
        Value::Object(call.arguments.clone()),
        json!({ "command": "ls" })
    );
    assert_eq!(
        (
            result.usage.input,
            result.usage.output,
            result.usage.total_tokens
        ),
        (3, 9, 12)
    );
    let request = &server.requests()[0];
    assert_eq!(
        request.header("anthropic-beta"),
        Some("interleaved-thinking-2025-05-14")
    );
    assert_eq!(
        request.body["thinking"],
        json!({ "type": "enabled", "budget_tokens": 1024, "display": "summarized" })
    );
    assert_eq!(
        request.body["tools"][0]["eager_input_streaming"],
        json!(true)
    );
    assert_eq!(
        request.body["tools"][0]["cache_control"],
        json!({ "type": "ephemeral" })
    );
}

#[tokio::test]
async fn anthropic_reports_stream_failures() {
    let error_event = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
    let no_stop = sse_events(&[(
        "message_start",
        json!({ "type": "message_start", "message": { "id": "m", "usage": { "input_tokens": 1 } } }),
    )]);
    let no_reason = sse_events(&[
        (
            "message_start",
            json!({ "type": "message_start", "message": { "id": "m", "usage": { "input_tokens": 1 } } }),
        ),
        ("message_stop", json!({ "type": "message_stop" })),
    ]);
    let server = serve(vec![
        Reply::sse(error_event),
        Reply::sse(no_stop),
        Reply::sse(no_reason),
    ])
    .await;
    let model = model("anthropic-messages", "cliproxyapi", &server.base);
    for expected in [
        r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        "Anthropic stream ended before message_stop",
        "Anthropic stream ended without a stop reason",
    ] {
        let stream = anthropic_messages::stream(
            &model,
            &context(false),
            anthropic_messages::AnthropicOptions {
                base: options(),
                ..Default::default()
            },
        );
        let (names, result) = run(stream).await;
        assert_eq!(names.last().map(String::as_str), Some("error"));
        assert_eq!(result.stop_reason, StopReason::Error);
        assert_eq!(result.error_message.as_deref(), Some(expected));
    }
}

#[tokio::test]
async fn anthropic_http_errors_carry_the_whole_body() {
    let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#;
    let server = serve(vec![Reply::error(400, body)]).await;
    let model = model("anthropic-messages", "cliproxyapi", &server.base);
    let stream = anthropic_messages::stream(
        &model,
        &context(false),
        anthropic_messages::AnthropicOptions {
            base: options(),
            ..Default::default()
        },
    );
    let (names, result) = run(stream).await;
    assert_eq!(names, ["error"]);
    assert_eq!(
        result.error_message.as_deref(),
        Some(format!("400 {body}").as_str())
    );
}

#[tokio::test]
async fn anthropic_requires_credentials() {
    let model = model("anthropic-messages", "cliproxyapi", "http://127.0.0.1:9");
    let stream = anthropic_messages::stream(
        &model,
        &context(false),
        anthropic_messages::AnthropicOptions::default(),
    );
    let (_, result) = run(stream).await;
    assert_eq!(
        result.error_message.as_deref(),
        Some("No API key for provider: cliproxyapi")
    );
}

#[test]
fn anthropic_maps_stop_reasons() {
    use anthropic_messages::map_stop_reason;
    assert_eq!(
        map_stop_reason("end_turn", None),
        Ok((StopReason::Stop, None))
    );
    assert_eq!(
        map_stop_reason("max_tokens", None),
        Ok((StopReason::Length, None))
    );
    assert_eq!(
        map_stop_reason("pause_turn", None),
        Ok((StopReason::Stop, None))
    );
    assert_eq!(
        map_stop_reason("refusal", Some(&json!({ "explanation": "no" }))),
        Ok((StopReason::Error, Some("no".to_owned())))
    );
    assert_eq!(
        map_stop_reason("mystery", None),
        Err("Unhandled stop reason: mystery".to_owned())
    );
}
