//! Shared helpers of compaction and branch summarization.
//!
//! Ported from Pi `packages/coding-agent/src/core/compaction/utils.ts`
//! (v1.4.0): file-operation tracking, conversation serialization, and the
//! summarization system prompt. Lengths count UTF-16 code units, as
//! JavaScript's `String.length` does.

use bake_ai::{AssistantContentBlock, Message, UserContent, UserContentBlock};
use indexmap::IndexSet;
use serde_json::Value;

use crate::session::AgentMessage;
use crate::session::json::{JsonObject, js_stringify};

/// The files a conversation read, wrote, and edited, Pi's `FileOperations`.
/// Each set keeps first-seen order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileOperations {
    /// Paths passed to `read`.
    pub read: IndexSet<String>,
    /// Paths passed to `write`.
    pub written: IndexSet<String>,
    /// Paths passed to `edit`.
    pub edited: IndexSet<String>,
}

/// Pi's `createFileOps`.
pub fn create_file_ops() -> FileOperations {
    FileOperations::default()
}

fn add_file_op(tool_name: Option<&str>, args: Option<&Value>, file_ops: &mut FileOperations) {
    let Some(path) = args
        .and_then(Value::as_object)
        .and_then(|args| args.get("path"))
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    else {
        return;
    };
    let set = match tool_name {
        Some("read") => &mut file_ops.read,
        Some("write") => &mut file_ops.written,
        Some("edit") => &mut file_ops.edited,
        _ => return,
    };
    set.insert(path.to_owned());
}

/// Pi's `extractFileOpsFromMessage`: the file paths of an assistant
/// message's `read`, `write`, and `edit` calls, or of the nested calls a
/// tool result records.
pub fn extract_file_ops_from_message(message: &AgentMessage, file_ops: &mut FileOperations) {
    let object = message.as_json();
    match message.role() {
        Some("toolResult") => {
            let calls = object
                .get("nestedCalls")
                .and_then(|nested| nested.get("calls"))
                .and_then(Value::as_array);
            for call in calls.into_iter().flatten() {
                add_file_op(
                    call.get("name").and_then(Value::as_str),
                    call.get("arguments"),
                    file_ops,
                );
            }
        }
        Some("assistant") => {
            let Some(Value::Array(content)) = object.get("content") else {
                return;
            };
            for block in content {
                let Some(block) = block.as_object() else {
                    continue;
                };
                if block.get("type").and_then(Value::as_str) != Some("toolCall")
                    || !block.contains_key("arguments")
                    || !block.contains_key("name")
                {
                    continue;
                }
                add_file_op(
                    block.get("name").and_then(Value::as_str),
                    block.get("arguments"),
                    file_ops,
                );
            }
        }
        _ => {}
    }
}

/// JavaScript's default `Array.prototype.sort` order: by UTF-16 code units.
pub(crate) fn js_sort(values: &mut [String]) {
    values.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
}

/// Pi's `computeFileLists`: the files only read, and the files written or
/// edited, each sorted.
pub fn compute_file_lists(file_ops: &FileOperations) -> (Vec<String>, Vec<String>) {
    let modified: IndexSet<&String> = file_ops.edited.iter().chain(&file_ops.written).collect();
    let mut read_files: Vec<String> = file_ops
        .read
        .iter()
        .filter(|path| !modified.contains(path))
        .cloned()
        .collect();
    js_sort(&mut read_files);
    let mut modified_files: Vec<String> = modified.into_iter().cloned().collect();
    js_sort(&mut modified_files);
    (read_files, modified_files)
}

/// Pi's `formatFileOperations`: the file lists as tags appended to a
/// summary.
pub fn format_file_operations(read_files: &[String], modified_files: &[String]) -> String {
    let mut sections = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!(
            "<read-files>\n{}\n</read-files>",
            read_files.join("\n")
        ));
    }
    if !modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified_files.join("\n")
        ));
    }
    if sections.is_empty() {
        return String::new();
    }
    format!("\n\n{}", sections.join("\n\n"))
}

/// The most characters of one tool result a serialized conversation keeps.
const TOOL_RESULT_MAX_CHARS: usize = 2000;

/// `String.length`.
pub(crate) fn js_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Pi's `truncateForSummary`. A surrogate pair split at the cut becomes
/// U+FFFD, where JavaScript keeps a lone surrogate.
fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() <= max_chars {
        return text.to_owned();
    }
    let kept = String::from_utf16_lossy(units.get(..max_chars).unwrap_or(&units));
    format!(
        "{kept}\n\n[... {} more characters truncated]",
        units.len() - max_chars
    )
}

fn blocks_text(blocks: &[UserContentBlock], separator: &str) -> String {
    bake_ai::utils::text::blocks_text(blocks, separator)
}

/// Whether `key` is an array index, which JavaScript enumerates first.
fn array_index(key: &str) -> Option<u32> {
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes.len() > 10 || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if bytes.len() > 1 && bytes.first() == Some(&b'0') {
        return None;
    }
    let value: u64 = key.parse().ok()?;
    u32::try_from(value).ok().filter(|value| *value < u32::MAX)
}

/// `Object.entries(object)`: array-index keys ascending, then the rest in
/// insertion order.
pub(crate) fn js_entries(object: &JsonObject) -> Vec<(&String, &Value)> {
    let mut indexed: Vec<(u32, (&String, &Value))> = object
        .iter()
        .filter_map(|entry| array_index(entry.0).map(|index| (index, entry)))
        .collect();
    indexed.sort_by_key(|(index, _)| *index);
    indexed
        .into_iter()
        .map(|(_, entry)| entry)
        .chain(object.iter().filter(|(key, _)| array_index(key).is_none()))
        .collect()
}

/// Pi's `serializeConversation`: model messages as labelled text, so the
/// summarizer does not continue the conversation. Tool results are cut at
/// 2000 characters; call [`crate::session::messages::convert_to_llm`]
/// first.
///
/// Thinking is omitted: providers return summaries of the real reasoning,
/// and replayed thinking is often not part of the model's live context, so
/// it could make the request larger than the context it summarizes.
pub fn serialize_conversation(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for message in messages {
        match message {
            Message::User(user) => {
                let content = match &user.content {
                    UserContent::Text(text) => text.clone(),
                    UserContent::Blocks(blocks) => blocks_text(blocks, ""),
                };
                if !content.is_empty() {
                    parts.push(format!("[User]: {content}"));
                }
            }
            Message::Assistant(assistant) => {
                let mut calls: Vec<String> = Vec::new();
                for block in &assistant.content {
                    if let AssistantContentBlock::ToolCall(call) = block {
                        let args: Vec<String> = js_entries(&call.arguments)
                            .into_iter()
                            .map(|(key, value)| format!("{key}={}", js_stringify(value)))
                            .collect();
                        calls.push(format!("{}({})", call.name, args.join(", ")));
                    }
                }
                if assistant
                    .content
                    .iter()
                    .any(|block| matches!(block, AssistantContentBlock::Text(_)))
                {
                    parts.push(format!(
                        "[Assistant]: {}",
                        bake_ai::utils::text::assistant_text(assistant)
                    ));
                }
                if !calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", calls.join("; ")));
                }
            }
            Message::ToolResult(result) => {
                let content = blocks_text(&result.content, "");
                if !content.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_for_summary(&content, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
            Message::System(_) => {}
        }
    }
    parts.join("\n\n")
}

/// Pi's `SUMMARIZATION_SYSTEM_PROMPT`.
pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

#[cfg(test)]
mod tests {
    use bake_ai::{NestedToolCallRecord, NestedToolCalls, ToolResultMessage, UserMessage};

    use super::*;

    fn tool_result(text: &str) -> Message {
        Message::ToolResult(ToolResultMessage {
            tool_call_id: "tc1".into(),
            tool_name: "read".into(),
            content: vec![UserContentBlock::Text(bake_ai::TextContent::new(text))],
            ..ToolResultMessage::default()
        })
    }

    // Pi `test/compaction-serialization.test.ts`: "should truncate long tool
    // results".
    #[test]
    fn truncates_long_tool_results() {
        let result = serialize_conversation(&[tool_result(&"x".repeat(5000))]);
        assert!(result.contains("[Tool result]:"));
        assert!(result.contains("[... 3000 more characters truncated]"));
        assert!(!result.contains(&"x".repeat(3000)));
        assert!(result.contains(&"x".repeat(2000)));
    }

    // Pi: "should not truncate short tool results".
    #[test]
    fn keeps_short_tool_results() {
        let short = "x".repeat(1500);
        let result = serialize_conversation(&[tool_result(&short)]);
        assert_eq!(result, format!("[Tool result]: {short}"));
    }

    // Pi: "should not truncate assistant or user messages".
    #[test]
    fn keeps_long_user_and_assistant_text() {
        let long = "y".repeat(5000);
        let result = serialize_conversation(&[
            Message::User(UserMessage {
                content: UserContent::Blocks(vec![UserContentBlock::Text(
                    bake_ai::TextContent::new(long.clone()),
                )]),
                timestamp: 0,
            }),
            Message::Assistant(bake_ai::providers::faux::faux_assistant_message(&long)),
        ]);
        assert!(!result.contains("truncated"));
        assert!(result.contains(&long));
    }

    // Pi v1.2.0: "should omit thinking".
    #[test]
    fn omits_thinking() {
        use bake_ai::providers::faux::{
            faux_assistant_blocks, faux_text, faux_thinking, faux_tool_call,
        };
        let assistant = faux_assistant_blocks(
            vec![
                faux_thinking("private reasoning"),
                faux_text("visible reply"),
                faux_tool_call("read", serde_json::json!({ "path": "a.ts" }), Some("tc1")),
            ],
            bake_ai::StopReason::ToolUse,
        );
        assert_eq!(
            serialize_conversation(&[Message::Assistant(assistant)]),
            "[Assistant]: visible reply\n\n[Assistant tool calls]: read(path=\"a.ts\")"
        );
    }

    // Pi `test/compaction-nested-calls.test.ts`: "include files touched by
    // nested calls recorded on tool results".
    #[test]
    fn nested_calls_count_as_file_operations() {
        let record = |id: &str, name: &str, arguments: Option<Value>| NestedToolCallRecord {
            id: id.into(),
            name: name.into(),
            arguments: arguments.and_then(|value| value.as_object().cloned()),
            arguments_bytes: None,
            status: "ok".into(),
            duration_ms: None,
            error: None,
        };
        let result = ToolResultMessage {
            tool_call_id: "codemode-1".into(),
            tool_name: "codemode".into(),
            nested_calls: Some(NestedToolCalls {
                calls: vec![
                    record(
                        "codemode-1/1",
                        "read",
                        Some(serde_json::json!({ "path": "a.ts" })),
                    ),
                    record(
                        "codemode-1/2",
                        "edit",
                        Some(serde_json::json!({ "path": "b.ts", "edits": [] })),
                    ),
                    NestedToolCallRecord {
                        arguments_bytes: Some(40_000),
                        ..record("codemode-1/3", "write", None)
                    },
                ],
                complete: false,
            }),
            ..ToolResultMessage::default()
        };
        let mut file_ops = create_file_ops();
        extract_file_ops_from_message(&AgentMessage::from(result), &mut file_ops);
        assert_eq!(
            compute_file_lists(&file_ops),
            (vec!["a.ts".to_owned()], vec!["b.ts".to_owned()])
        );
    }

    #[test]
    fn sorts_by_utf16_code_units_and_cuts_surrogates_safely() {
        let mut values = vec!["\u{ff61}".to_owned(), "\u{1f35e}".to_owned()];
        js_sort(&mut values);
        // U+1F35E is D83C DF5E in UTF-16, before U+FF61.
        assert_eq!(values, ["\u{1f35e}", "\u{ff61}"]);
        let cut = truncate_for_summary(&format!("{}\u{1f35e}", "x".repeat(1999)), 2000);
        assert!(cut.starts_with(&format!("{}\u{fffd}", "x".repeat(1999))));
        assert!(cut.ends_with("[... 1 more characters truncated]"));
    }
}
