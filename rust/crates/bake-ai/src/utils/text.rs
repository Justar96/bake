//! Text extraction and system-message rendering.
//!
//! Ported from Pi `packages/ai/src/utils/text.ts` (v1.1.0).

use crate::types::{
    AssistantContentBlock, AssistantMessage, SystemContent, SystemMessage, SystemTextBlock,
    UserContent, UserContentBlock,
};

/// The text blocks of user or tool-result content joined by `separator`.
pub fn user_content_text(content: &UserContent, separator: &str) -> String {
    match content {
        UserContent::Text(text) => text.clone(),
        UserContent::Blocks(blocks) => blocks_text(blocks, separator),
    }
}

/// The text blocks of `blocks` joined by `separator`.
pub fn blocks_text(blocks: &[UserContentBlock], separator: &str) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join(separator)
}

/// The text blocks of an assistant message joined by `separator`.
pub fn assistant_content_text(content: &[AssistantContentBlock], separator: &str) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(separator)
}

/// The text of an assistant message, blocks joined by newlines.
pub fn assistant_text(message: &AssistantMessage) -> String {
    assistant_content_text(&message.content, "\n")
}

/// A system message's content as text, blocks joined by newlines.
pub fn system_content_text(content: &SystemContent) -> String {
    match content {
        SystemContent::Text(text) => text.clone(),
        SystemContent::Blocks(blocks) => blocks
            .iter()
            .map(|SystemTextBlock::Text(text)| text.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// A system message as a complete prompt: its content, then its sections.
pub fn get_system_message_text(message: &SystemMessage) -> String {
    let mut parts = vec![system_content_text(&message.content)];
    if let Some(sections) = &message.sections {
        parts.extend(sections.0.iter().filter_map(|(_, value)| value.clone()));
    }
    parts.retain(|part| !part.is_empty());
    parts.join("\n\n")
}

/// A later system message for APIs that accept system messages in place:
/// section changes are framed by name.
pub fn render_system_message_update(message: &SystemMessage) -> String {
    let mut parts = Vec::new();
    let text = system_content_text(&message.content);
    if !text.is_empty() {
        parts.push(text);
    }
    if let Some(sections) = &message.sections {
        for (name, value) in &sections.0 {
            parts.push(match value {
                None => format!("Removed system prompt section \"{name}\"."),
                Some(value) => format!("Updated system prompt section \"{name}\":\n\n{value}"),
            });
        }
    }
    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    //! Ports of Pi `packages/ai/test/text.test.ts`.

    use super::*;
    use crate::types::{ImageContent, TextContent, ThinkingContent, ToolCall};

    #[test]
    fn extracts_text() {
        let content = vec![
            AssistantContentBlock::Thinking(ThinkingContent::new("reasoning")),
            AssistantContentBlock::Text(TextContent::new("first")),
            AssistantContentBlock::ToolCall(ToolCall {
                id: "1".into(),
                name: "read".into(),
                ..ToolCall::default()
            }),
            AssistantContentBlock::Text(TextContent::new("second")),
        ];
        assert_eq!(assistant_content_text(&content, "\n"), "first\nsecond");
        assert_eq!(assistant_content_text(&content, ""), "firstsecond");
        assert_eq!(
            user_content_text(&UserContent::from("hello"), "\n"),
            "hello"
        );
        let tool_result = vec![
            UserContentBlock::Text(TextContent::new("first")),
            UserContentBlock::Image(ImageContent {
                data: "...".into(),
                mime_type: "image/png".into(),
            }),
            UserContentBlock::Text(TextContent::new("second")),
        ];
        assert_eq!(blocks_text(&tool_result, ""), "firstsecond");
    }
}
