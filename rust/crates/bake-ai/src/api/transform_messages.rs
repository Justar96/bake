//! Cross-provider message normalization before a request.
//!
//! Ported from Pi `packages/ai/src/api/transform-messages.ts` (v1.1.0):
//! images become placeholders for text-only models, thinking from another
//! model becomes text, tool-call ids are normalized, failed assistant turns
//! are dropped, and orphaned tool calls get synthetic error results.

use std::collections::{HashMap, HashSet};

use crate::types::{
    AssistantContentBlock, AssistantMessage, Message, Model, StopReason, TextContent, ToolCall,
    ToolResultMessage, UserContent, UserContentBlock,
};

const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str =
    "(tool image omitted: model does not support images)";

fn replace_images_with_placeholder(
    content: &[UserContentBlock],
    placeholder: &str,
) -> Vec<UserContentBlock> {
    let mut result = Vec::with_capacity(content.len());
    let mut previous_was_placeholder = false;
    for block in content {
        match block {
            UserContentBlock::Image(_) => {
                if !previous_was_placeholder {
                    result.push(UserContentBlock::Text(TextContent::new(placeholder)));
                }
                previous_was_placeholder = true;
            }
            UserContentBlock::Text(text) => {
                previous_was_placeholder = text.text == placeholder;
                result.push(block.clone());
            }
        }
    }
    result
}

fn downgrade_unsupported_images(messages: &[Message], model: &Model) -> Vec<Message> {
    if model.accepts_images() {
        return messages.to_vec();
    }
    messages
        .iter()
        .map(|message| match message {
            Message::User(user) => match &user.content {
                UserContent::Blocks(blocks) => {
                    let mut user = user.clone();
                    user.content = UserContent::Blocks(replace_images_with_placeholder(
                        blocks,
                        NON_VISION_USER_IMAGE_PLACEHOLDER,
                    ));
                    Message::User(user)
                }
                UserContent::Text(_) => message.clone(),
            },
            Message::ToolResult(result) => {
                let mut result = result.clone();
                result.content = replace_images_with_placeholder(
                    &result.content,
                    NON_VISION_TOOL_IMAGE_PLACEHOLDER,
                );
                Message::ToolResult(result)
            }
            _ => message.clone(),
        })
        .collect()
}

/// Maps a tool-call id from a source message to one the target model accepts.
pub type NormalizeToolCallId<'a> = dyn Fn(&str, &Model, &AssistantMessage) -> String + 'a;

/// Normalizes `messages` for `model`. `normalize_tool_call_id` rewrites the
/// ids of tool calls from other models; their results follow.
pub fn transform_messages(
    messages: &[Message],
    model: &Model,
    normalize_tool_call_id: Option<&NormalizeToolCallId<'_>>,
) -> Vec<Message> {
    let mut id_map: HashMap<String, String> = HashMap::new();
    let image_aware = downgrade_unsupported_images(messages, model);

    let transformed: Vec<Message> = image_aware
        .into_iter()
        .map(|message| match message {
            Message::ToolResult(mut result) => {
                if let Some(normalized) = id_map.get(&result.tool_call_id)
                    && *normalized != result.tool_call_id
                {
                    result.tool_call_id = normalized.clone();
                }
                Message::ToolResult(result)
            }
            Message::Assistant(assistant) => {
                let same_model = assistant.provider == model.provider
                    && assistant.api == model.api
                    && assistant.model == model.id;
                let mut content = Vec::with_capacity(assistant.content.len());
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Thinking(thinking) => {
                            if thinking.is_redacted() {
                                if same_model {
                                    content.push(block.clone());
                                }
                                continue;
                            }
                            if same_model
                                && thinking
                                    .thinking_signature
                                    .as_deref()
                                    .is_some_and(|s| !s.is_empty())
                            {
                                content.push(block.clone());
                                continue;
                            }
                            if thinking.thinking.trim().is_empty() {
                                continue;
                            }
                            if same_model {
                                content.push(block.clone());
                            } else {
                                content.push(AssistantContentBlock::Text(TextContent::new(
                                    thinking.thinking.clone(),
                                )));
                            }
                        }
                        AssistantContentBlock::Text(text) => {
                            if same_model {
                                content.push(block.clone());
                            } else {
                                content.push(AssistantContentBlock::Text(TextContent::new(
                                    text.text.clone(),
                                )));
                            }
                        }
                        AssistantContentBlock::ToolCall(call) => {
                            let mut call: ToolCall = call.clone();
                            if !same_model {
                                call.thought_signature = None;
                                if let Some(normalize) = normalize_tool_call_id {
                                    let normalized = normalize(&call.id, model, &assistant);
                                    if normalized != call.id {
                                        id_map.insert(call.id.clone(), normalized.clone());
                                        call.id = normalized;
                                    }
                                }
                            }
                            content.push(AssistantContentBlock::ToolCall(call));
                        }
                    }
                }
                let mut assistant = assistant;
                assistant.content = content;
                Message::Assistant(assistant)
            }
            other => other,
        })
        .collect();

    let mut result: Vec<Message> = Vec::with_capacity(transformed.len());
    let mut pending: Vec<ToolCall> = Vec::new();
    let mut existing: HashSet<String> = HashSet::new();
    let mut held_system: Vec<Message> = Vec::new();
    let close_pending = |result: &mut Vec<Message>,
                         pending: &mut Vec<ToolCall>,
                         existing: &mut HashSet<String>,
                         held_system: &mut Vec<Message>| {
        if !pending.is_empty() {
            for call in pending.iter() {
                if !existing.contains(&call.id) {
                    result.push(Message::ToolResult(ToolResultMessage {
                        tool_call_id: call.id.clone(),
                        tool_name: call.name.clone(),
                        content: vec![UserContentBlock::Text(TextContent::new(
                            "No result provided",
                        ))],
                        is_error: true,
                        timestamp: crate::now_ms(),
                        ..ToolResultMessage::default()
                    }));
                }
            }
            pending.clear();
            existing.clear();
        }
        result.append(held_system);
    };

    for message in transformed {
        match message {
            Message::Assistant(assistant) => {
                close_pending(&mut result, &mut pending, &mut existing, &mut held_system);
                if matches!(
                    assistant.stop_reason,
                    StopReason::Error | StopReason::Aborted
                ) {
                    continue;
                }
                let calls: Vec<ToolCall> = assistant.tool_calls().cloned().collect();
                if !calls.is_empty() {
                    pending = calls;
                    existing.clear();
                }
                result.push(Message::Assistant(assistant));
            }
            Message::ToolResult(tool_result) => {
                existing.insert(tool_result.tool_call_id.clone());
                result.push(Message::ToolResult(tool_result));
            }
            Message::System(system) => {
                if pending.is_empty() {
                    result.push(Message::System(system));
                } else {
                    held_system.push(Message::System(system));
                }
            }
            Message::User(user) => {
                close_pending(&mut result, &mut pending, &mut existing, &mut held_system);
                result.push(Message::User(user));
            }
        }
    }
    close_pending(&mut result, &mut pending, &mut existing, &mut held_system);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::faux::{faux_assistant_message, faux_model};
    use crate::types::{ImageContent, SystemMessage, ThinkingContent, UserMessage};
    use serde_json::json;

    // Port of Pi `packages/ai/test/lax-message-content.test.ts`: null or
    // missing content reads as an empty list.
    #[test]
    fn lax_message_content_reads_as_empty() {
        let messages: Vec<Message> = serde_json::from_value(json!([
            { "role": "user", "content": null, "timestamp": 1 },
            {
                "role": "assistant", "content": null, "api": "openai-completions", "provider": "openai",
                "model": "test-model",
                "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                    "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } },
                "stopReason": "stop", "timestamp": 1
            },
            { "role": "toolResult", "toolCallId": "call_1", "toolName": "web_search", "isError": false, "timestamp": 1 }
        ]))
        .unwrap();
        let mut model = faux_model("test-model");
        model.input = vec![crate::types::InputModality::Text];
        let result = transform_messages(&messages, &model, None);
        assert_eq!(result.len(), 3);
        for message in result {
            let value = serde_json::to_value(&message).unwrap();
            assert_eq!(value["content"], json!([]), "{value}");
        }
    }

    fn tool_call(id: &str) -> AssistantContentBlock {
        AssistantContentBlock::ToolCall(ToolCall {
            id: id.into(),
            name: "read".into(),
            ..ToolCall::default()
        })
    }

    // From Pi `tool-call-without-result.test.ts` (its network-free part):
    // orphaned calls get a synthetic error result before the next user turn.
    #[test]
    fn synthesizes_results_for_orphaned_tool_calls_and_holds_system_messages() {
        let model = faux_model("faux-1");
        let mut assistant = faux_assistant_message("");
        assistant.content = vec![tool_call("a"), tool_call("b")];
        assistant.stop_reason = StopReason::ToolUse;
        let messages = vec![
            Message::Assistant(assistant),
            Message::System(SystemMessage {
                content: "update".into(),
                timestamp: 2,
                ..Default::default()
            }),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "a".into(),
                tool_name: "read".into(),
                timestamp: 3,
                ..Default::default()
            }),
            Message::User(UserMessage {
                content: "next".into(),
                timestamp: 4,
            }),
        ];
        let result = transform_messages(&messages, &model, None);
        let summary: Vec<String> = result
            .iter()
            .map(|message| match message {
                Message::ToolResult(result) => {
                    format!("toolResult:{}:{}", result.tool_call_id, result.is_error)
                }
                other => other.role().to_owned(),
            })
            .collect();
        assert_eq!(
            summary,
            [
                "assistant",
                "toolResult:a:false",
                "toolResult:b:true",
                "system",
                "user"
            ]
        );
    }

    #[test]
    fn drops_failed_turns_and_converts_foreign_thinking() {
        let mut model = faux_model("other");
        model.input = vec![crate::types::InputModality::Text];
        let mut failed = faux_assistant_message("partial");
        failed.stop_reason = StopReason::Error;
        let mut foreign = faux_assistant_message("");
        foreign.content = vec![
            AssistantContentBlock::Thinking(ThinkingContent {
                thinking: "hmm".into(),
                thinking_signature: Some("sig".into()),
                redacted: None,
            }),
            AssistantContentBlock::Thinking(ThinkingContent {
                thinking: "x".into(),
                thinking_signature: Some("opaque".into()),
                redacted: Some(true),
            }),
            AssistantContentBlock::Text(TextContent {
                text: "hi".into(),
                text_signature: Some("msg_1".into()),
            }),
            AssistantContentBlock::ToolCall(ToolCall {
                id: "call|item".into(),
                name: "read".into(),
                thought_signature: Some("t".into()),
                ..ToolCall::default()
            }),
        ];
        foreign.stop_reason = StopReason::ToolUse;
        let messages = vec![
            Message::User(UserMessage {
                content: UserContent::Blocks(vec![
                    UserContentBlock::Image(ImageContent {
                        data: "a".into(),
                        mime_type: "image/png".into(),
                    }),
                    UserContentBlock::Image(ImageContent {
                        data: "b".into(),
                        mime_type: "image/png".into(),
                    }),
                ]),
                timestamp: 1,
            }),
            Message::Assistant(failed),
            Message::Assistant(foreign),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "call|item".into(),
                tool_name: "read".into(),
                timestamp: 2,
                ..Default::default()
            }),
        ];
        let normalize = |id: &str, _: &Model, _: &AssistantMessage| id.replace('|', "_");
        let result =
            serde_json::to_value(transform_messages(&messages, &model, Some(&normalize))).unwrap();
        assert_eq!(
            result[0]["content"],
            json!([{ "type": "text", "text": NON_VISION_USER_IMAGE_PLACEHOLDER }])
        );
        assert_eq!(
            result[1]["content"],
            json!([
                { "type": "text", "text": "hmm" },
                { "type": "text", "text": "hi" },
                { "type": "toolCall", "id": "call_item", "name": "read", "arguments": {} }
            ])
        );
        assert_eq!(result[2]["toolCallId"], json!("call_item"));
        assert_eq!(result.as_array().unwrap().len(), 3);
    }
}
