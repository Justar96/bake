//! Context-size estimates from characters and reported usage.
//!
//! Ported from Pi `packages/ai/src/utils/estimate.ts` (v1.1.0). Lengths count
//! UTF-16 code units, as JavaScript's `String.length` does.

use crate::types::{
    AssistantContentBlock, Message, StopReason, Tool, ToolReference, Usage, UserContent,
    UserContentBlock,
};
use crate::utils::text::get_system_message_text;

const CHARS_PER_TOKEN: f64 = 3.5;
const ESTIMATED_IMAGE_CHARS: usize = 4800;

/// A context-size estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsageEstimate {
    /// Estimated total tokens.
    pub tokens: u64,
    /// Tokens the latest applicable usage reported.
    pub usage_tokens: u64,
    /// Estimated tokens after that usage.
    pub trailing_tokens: u64,
    /// Index of the message whose usage was used.
    pub last_usage_index: Option<usize>,
}

pub(crate) fn js_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn tokens_for_chars(chars: usize) -> u64 {
    (chars as f64 / CHARS_PER_TOKEN).ceil() as u64
}

/// Tokens a usage block reports for the context.
pub fn calculate_context_tokens(usage: &Usage) -> u64 {
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage.component_sum()
    }
}

/// Estimated tokens of `text`.
pub fn estimate_text_tokens(text: &str) -> u64 {
    tokens_for_chars(js_len(text))
}

fn blocks_chars(blocks: &[UserContentBlock]) -> usize {
    blocks
        .iter()
        .map(|block| match block {
            UserContentBlock::Text(text) => js_len(&text.text),
            UserContentBlock::Image(_) => ESTIMATED_IMAGE_CHARS,
        })
        .sum()
}

fn tools_tokens<T: serde::Serialize>(tools: Option<&Vec<T>>) -> u64 {
    match tools {
        Some(tools) if !tools.is_empty() => estimate_text_tokens(
            &serde_json::to_string(tools).unwrap_or_else(|_| "[unserializable]".into()),
        ),
        _ => 0,
    }
}

/// Estimated tokens of one message.
pub fn estimate_message_tokens(message: &Message) -> u64 {
    match message {
        Message::System(system) => {
            estimate_text_tokens(&get_system_message_text(system))
                + tools_tokens::<Tool>(system.tools_added.as_ref())
                + tools_tokens::<ToolReference>(system.tools_removed.as_ref())
        }
        Message::User(user) => match &user.content {
            UserContent::Text(text) => estimate_text_tokens(text),
            UserContent::Blocks(blocks) => tokens_for_chars(blocks_chars(blocks)),
        },
        Message::ToolResult(result) => tokens_for_chars(blocks_chars(&result.content)),
        Message::Assistant(assistant) => {
            let chars: usize = assistant
                .content
                .iter()
                .map(|block| match block {
                    AssistantContentBlock::Text(text) => js_len(&text.text),
                    AssistantContentBlock::Thinking(thinking) => js_len(&thinking.thinking),
                    AssistantContentBlock::ToolCall(call) => {
                        js_len(&call.name)
                            + js_len(&serde_json::to_string(&call.arguments).unwrap_or_default())
                    }
                })
                .sum();
            tokens_for_chars(chars)
        }
    }
}

fn last_assistant_usage(messages: &[Message]) -> Option<(Usage, usize)> {
    let mut latest_prefix_timestamp = i64::MIN;
    let mut found = None;
    for (index, message) in messages.iter().enumerate() {
        if let Message::Assistant(assistant) = message
            && assistant.timestamp >= latest_prefix_timestamp
            && assistant.stop_reason != StopReason::Aborted
            && assistant.stop_reason != StopReason::Error
            && calculate_context_tokens(&assistant.usage) > 0
        {
            found = Some((assistant.usage, index));
        }
        latest_prefix_timestamp = latest_prefix_timestamp.max(message.timestamp());
    }
    found
}

/// Estimated context tokens: the latest applicable reported usage plus
/// estimates for later messages, or estimates for all messages.
pub fn estimate_context_tokens(messages: &[Message]) -> ContextUsageEstimate {
    if let Some((usage, index)) = last_assistant_usage(messages) {
        let usage_tokens = calculate_context_tokens(&usage);
        let trailing_tokens = messages
            .iter()
            .skip(index + 1)
            .map(estimate_message_tokens)
            .sum();
        return ContextUsageEstimate {
            tokens: usage_tokens.saturating_add(trailing_tokens),
            usage_tokens,
            trailing_tokens,
            last_usage_index: Some(index),
        };
    }
    let tokens = messages.iter().map(estimate_message_tokens).sum();
    ContextUsageEstimate {
        tokens,
        usage_tokens: 0,
        trailing_tokens: tokens,
        last_usage_index: None,
    }
}

#[cfg(test)]
mod tests {
    //! Cases from Pi `packages/ai/test/context-estimate.test.ts`.

    use super::*;
    use crate::providers::faux::faux_assistant_message;
    use crate::types::UserMessage;

    fn user(text: &str, timestamp: i64) -> Message {
        Message::User(UserMessage {
            content: text.into(),
            timestamp,
        })
    }

    #[test]
    fn estimates_characters_and_uses_the_latest_usage() {
        assert_eq!(estimate_text_tokens("abcdefg"), 2);
        let mut assistant = faux_assistant_message("reply");
        assistant.timestamp = 2;
        assistant.usage.total_tokens = 100;
        let messages = vec![
            user("x".repeat(35).as_str(), 1),
            Message::Assistant(assistant.clone()),
            user("abcdefg", 3),
        ];
        let estimate = estimate_context_tokens(&messages);
        assert_eq!(
            estimate,
            ContextUsageEstimate {
                tokens: 102,
                usage_tokens: 100,
                trailing_tokens: 2,
                last_usage_index: Some(1)
            }
        );

        // A prefix message inserted after the response makes its usage stale.
        let stale = vec![user("abcdefg", 5), Message::Assistant(assistant)];
        assert_eq!(estimate_context_tokens(&stale).last_usage_index, None);
    }
}
