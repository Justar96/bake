//! Provider response bytes through `bake-ai`'s SSE decoder and the three
//! wire-protocol stream processors, as `api::*::stream` feeds them.
//!
//! Input: byte 0 picks the protocol (and Anthropic OAuth naming), byte 1 the
//! chunk size the stream arrives in (0 for one chunk), the rest is the HTTP
//! body. Oracles: decoding is independent of how the body is chunked, every
//! event's data parses as streamed JSON without panicking, and the assembled
//! message serializes and reads back unchanged.

#![no_main]

use bake_ai::api::anthropic_messages::{AnthropicStreamProcessor, decode_event};
use bake_ai::api::openai_completions::CompletionsStreamProcessor;
use bake_ai::api::openai_responses::apply_service_tier_pricing;
use bake_ai::api::openai_responses_shared::ResponsesStreamProcessor;
use bake_ai::providers::faux::faux_model;
use bake_ai::utils::json_parse::{parse_json_strict, parse_partial, parse_streaming_json};
use bake_ai::utils::sse::{ServerSentEvent, SseDecoder};
use bake_ai::{AssistantMessage, Tool, assistant_message_channel};
use libfuzzer_sys::fuzz_target;
use serde_json::{Value, json};

fn decode(body: &[u8], chunk: usize) -> Vec<ServerSentEvent> {
    let mut decoder = SseDecoder::new();
    let mut events = Vec::new();
    if chunk == 0 {
        events.extend(decoder.push(body));
    } else {
        for piece in body.chunks(chunk) {
            events.extend(decoder.push(piece));
        }
    }
    events.extend(decoder.finish());
    events
}

/// The OpenAI SDK's event handling (`parse_openai_sse`, crate-private):
/// `None` ends the stream, `Some(None)` skips the event.
fn openai_event(event: &ServerSentEvent) -> Option<Option<Value>> {
    if event.data == "[DONE]" {
        return None;
    }
    if event
        .event
        .as_deref()
        .is_some_and(|name| name.starts_with("thread."))
    {
        return Some(None);
    }
    let data = parse_json_strict(&event.data).ok()?;
    if event.event.as_deref() == Some("error") || data.get("error").is_some() {
        return None;
    }
    Some(Some(data))
}

fn check_round_trip(message: &AssistantMessage) {
    let text = serde_json::to_string(message).expect("an assistant message serializes");
    let back: AssistantMessage =
        serde_json::from_str(&text).expect("a serialized assistant message reads back");
    assert_eq!(
        &back, message,
        "an assistant message round-trips through JSON"
    );
}

fuzz_target!(|data: &[u8]| {
    let [mode, chunk, body @ ..] = data else {
        return;
    };
    let events = decode(body, usize::from(*chunk));
    assert_eq!(
        events,
        decode(body, 0),
        "SSE events do not depend on chunking"
    );
    for event in &events {
        let _ = parse_streaming_json(Some(&event.data));
        let _ = parse_partial(&event.data);
    }

    let (sender, _stream) = assistant_message_channel();
    match mode % 4 {
        0 | 1 => {
            let model = faux_model("claude-fuzz");
            let tools = vec![Tool {
                name: "read".into(),
                description: "Read a file".into(),
                parameters: json!({"type": "object"}),
                constrained_sampling: None,
            }];
            let mut processor = AnthropicStreamProcessor::new(
                &model,
                AssistantMessage::pending(&model),
                mode % 4 == 1,
                tools,
            )
            .with_max_tool_argument_whitespace(64);
            for event in &events {
                match decode_event(event) {
                    Ok(Some(value)) => {
                        if processor.handle_event(&value, &sender).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
            let _ = processor.finish();
            check_round_trip(&processor.output);
        }
        2 => {
            let model = faux_model("gpt-fuzz");
            let mut processor =
                CompletionsStreamProcessor::new(&model, AssistantMessage::pending(&model))
                    .with_max_tool_argument_whitespace(64);
            for event in &events {
                match openai_event(event) {
                    Some(Some(chunk)) => {
                        if processor.handle_chunk(&chunk, &sender).is_err() {
                            break;
                        }
                    }
                    Some(None) => {}
                    None => break,
                }
            }
            processor.finish_blocks(&sender);
            check_round_trip(&processor.output);
        }
        _ => {
            let model = faux_model("gpt-fuzz");
            let mut processor = ResponsesStreamProcessor::new(
                &model,
                AssistantMessage::pending(&model),
                Some("flex".into()),
                Some(Box::new(|usage, tier| {
                    apply_service_tier_pricing(usage, tier, "gpt-fuzz")
                })),
            )
            .with_max_tool_argument_whitespace(64);
            for event in &events {
                match openai_event(event) {
                    Some(Some(value)) => {
                        if processor.handle_event(&value, &sender).is_err() {
                            break;
                        }
                    }
                    Some(None) => {}
                    None => break,
                }
            }
            let _ = processor.finish();
            check_round_trip(&processor.output);
        }
    }
});
