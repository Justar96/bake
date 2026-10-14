//! Write `tests/fixtures/pi-session/rust-written.jsonl`: a session with every
//! entry kind, written through this crate's API with `bake-ai` message
//! types, for `read-with-pi.mjs` to read in Pi.
//!
//! ```sh
//! cargo run -p bake-coding-agent --example write_golden_session -- <output.jsonl>
//! ```

use std::path::PathBuf;

use bake_ai::{
    AssistantContentBlock, AssistantMessage, ImageContent, Message, StopReason, SystemContent,
    SystemMessage, TextContent, ThinkingContent, Tool, ToolCall, ToolResultMessage, Usage,
    UsageCost, UserContent, UserContentBlock, UserMessage,
};
use bake_coding_agent::session::messages::{BashExecutionMessage, CustomMessage};
use bake_coding_agent::session::{
    AgentMessage, EditContent, NewSessionOptions, SessionError, SessionManager,
};
use serde_json::json;

fn usage() -> Usage {
    Usage {
        input: 1200,
        output: 345,
        cache_read: 100_000,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: Some(12),
        total_tokens: 101_545,
        cost: UsageCost {
            input: 0.0000015,
            output: 1e-7,
            cache_read: 0.30000000000000004,
            cache_write: 0.0,
            total: 1.5e-300,
        },
    }
}

fn assistant(
    content: Vec<AssistantContentBlock>,
    stop_reason: StopReason,
    timestamp: i64,
) -> AssistantMessage {
    AssistantMessage {
        content,
        api: "anthropic-messages".into(),
        provider: "anthropic".into(),
        model: "claude-test".into(),
        response_model: None,
        response_id: Some("msg_01".into()),
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: usage(),
        stop_reason,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp,
        duration_ms: Some(1500),
    }
}

fn write(output: PathBuf) -> Result<(), SessionError> {
    let dir = std::env::temp_dir().join(format!("bake-golden-{}", std::process::id()));
    let mut manager =
        SessionManager::create("/golden/project", Some(&dir), NewSessionOptions::default())?;
    manager.append_model_change("anthropic", "claude-test")?;
    manager.append_thinking_level_change("high")?;
    manager.append_message(Message::System(SystemMessage {
        content: SystemContent::Text("You are a coding agent.".into()),
        sections: None,
        tools_added: Some(vec![Tool {
            name: "read".into(),
            description: "Read a file".into(),
            parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            constrained_sampling: None,
        }]),
        tools_removed: None,
        timestamp: 1_767_225_600_000,
    }))?;
    let user = manager.append_message(UserMessage {
        content: UserContent::Blocks(vec![
            UserContentBlock::Text(TextContent::new("Read \"a.ts\" — émoji 😀 \u{1} control")),
            UserContentBlock::Image(ImageContent {
                data: "iVBORw0KGgo=".into(),
                mime_type: "image/png".into(),
            }),
        ]),
        timestamp: 1_767_225_601_000,
    })?;
    let mut arguments = serde_json::Map::new();
    arguments.insert("path".into(), json!("a.ts"));
    let answer = manager.append_message(assistant(
        vec![
            AssistantContentBlock::Thinking(ThinkingContent {
                thinking: "Need the file.".into(),
                thinking_signature: Some("sig==".into()),
                redacted: None,
            }),
            AssistantContentBlock::Text(TextContent::new("Reading it.")),
            AssistantContentBlock::ToolCall(ToolCall {
                id: "call_1".into(),
                name: "read".into(),
                arguments,
                thought_signature: None,
                namespace: None,
            }),
        ],
        StopReason::ToolUse,
        1_767_225_602_000,
    ))?;
    let result = manager.append_message(ToolResultMessage {
        tool_call_id: "call_1".into(),
        tool_name: "read".into(),
        content: vec![UserContentBlock::Text(TextContent::new(
            "export const a = 1;\n",
        ))],
        details: Some(json!({"path": "a.ts"})),
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 1_767_225_603_000,
        duration_ms: None,
    })?;
    manager.append_custom_entry("ext-state", Some(json!({"mode": "plan", "big": 1e21})))?;
    let note = manager.append_custom_message_entry(
        "note",
        UserContent::Text("A custom note.".into()),
        true,
        Some(json!({"source": "ext"})),
    )?;
    manager.append_message(BashExecutionMessage {
        command: "ls".into(),
        output: "a.ts\n".into(),
        exit_code: Some(0),
        cancelled: false,
        truncated: false,
        full_output_path: None,
        timestamp: 1_767_225_604_000,
        exclude_from_context: None,
    })?;
    manager.append_message(CustomMessage {
        custom_type: "inline".into(),
        content: UserContent::Text("inline custom".into()),
        display: false,
        details: None,
        timestamp: 1_767_225_605_000,
    })?;
    manager.append_message(AgentMessage::from_json(
        json!({"role": "futureRole", "payload": {"a": 1}, "timestamp": 1_767_225_606_000_i64})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    ))?;
    manager.append_label_change(&user, Some("checkpoint"))?;
    manager.append_session_info("Rust\nsession ")?;
    let usage_entry = manager.append_usage(
        "cache_warm",
        "anthropic",
        "claude-test",
        &usage(),
        Some("warm"),
    )?;
    manager.append_message(UserMessage {
        content: "go on".into(),
        timestamp: 1_767_225_607_000,
    })?;
    manager.branch_with_summary(
        Some(usage_entry.id()),
        "Tried another approach.",
        Some(json!({"files": ["a.ts"]})),
        Some(false),
        Some(&usage()),
    )?;
    manager.append_message(UserMessage {
        content: "new direction".into(),
        timestamp: 1_767_225_609_000,
    })?;
    manager.append_context_edit(
        &user,
        Some(EditContent::Text("edited first request".into())),
    )?;
    manager.append_context_edit(
        &result,
        Some(EditContent::Text("edited tool output".into())),
    )?;
    manager.append_compaction(
        "Compacted the start.",
        Some(&note),
        4321,
        Some(json!({"kind": "structured"})),
        Some(false),
        Some(&usage()),
    )?;
    manager.append_message(UserMessage {
        content: "after compaction".into(),
        timestamp: 1_767_225_610_000,
    })?;
    manager.append_label_change(&answer, Some("answer"))?;
    manager.append_label_change(&answer, None)?;
    manager.append_thinking_level_change("low")?;
    let file = manager.session_file().map(PathBuf::from);
    if let Some(file) = file {
        std::fs::copy(&file, &output).map_err(|error| SessionError::Io {
            path: output,
            source: error,
        })?;
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

fn main() {
    let Some(output) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: write_golden_session <output.jsonl>");
        std::process::exit(2);
    };
    if let Err(error) = write(output) {
        eprintln!("write_golden_session: {error}");
        std::process::exit(1);
    }
}
