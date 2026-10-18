//! The current branch as JSONL, adapted from Pi v1.3.0
//! `packages/coding-agent/src/core/session-export.ts`
//! (`serializeSessionBranch`) and `src/modes/interactive/session-share.ts`
//! (`createShareTrailingEntries`), with the case of
//! `test/export-jsonl-share.test.ts` that the tests name.
//!
//! `/bug` puts it into the bug-report archive when the transcript is
//! included. It is never written to the session.

use serde_json::{Map, Value};

use crate::session::SessionManager;
use crate::session::entry::CURRENT_SESSION_VERSION;

/// The custom type of the export-only entry that carries the system prompt
/// and tool definitions, Pi's `pi.share`, which Pi's session viewer reads.
pub const SHARE_ENTRY_TYPE: &str = "pi.share";

/// Pi's `createShareTrailingEntries`: one `pi.share` custom entry after the
/// branch, holding `system_prompt` and `tools` (each tool's `name`,
/// `description`, and `parameters`).
pub fn share_trailing_entry(
    parent_id: Option<&str>,
    timestamp: &str,
    system_prompt: &str,
    tools: Vec<Value>,
) -> Value {
    let mut data = Map::new();
    data.insert("systemPrompt".into(), system_prompt.into());
    data.insert("tools".into(), Value::Array(tools));
    let mut entry = Map::new();
    entry.insert("type".into(), "custom".into());
    entry.insert("customType".into(), SHARE_ENTRY_TYPE.into());
    let id = crate::session::id::uuid_v7();
    entry.insert("id".into(), id.chars().take(8).collect::<String>().into());
    entry.insert(
        "parentId".into(),
        parent_id.map_or(Value::Null, |id| Value::String(id.to_owned())),
    );
    entry.insert("timestamp".into(), timestamp.into());
    entry.insert("data".into(), Value::Object(data));
    Value::Object(entry)
}

/// Pi's `serializeSessionBranch`: a fresh header, the entries from the root
/// to the leaf relinked as one chain, then what `trailing` adds after the
/// last entry's id, one JSON object per line.
pub fn serialize_session_branch(
    session: &SessionManager,
    trailing: impl FnOnce(Option<&str>, &str) -> Vec<Value>,
) -> String {
    let timestamp = crate::session::time::now_iso();
    let mut header = Map::new();
    header.insert("type".into(), "session".into());
    header.insert("version".into(), CURRENT_SESSION_VERSION.into());
    header.insert("id".into(), session.session_id().into());
    header.insert("timestamp".into(), timestamp.clone().into());
    header.insert("cwd".into(), session.cwd().into());
    let mut lines = vec![Value::Object(header).to_string()];
    let mut parent: Option<String> = None;
    for entry in session.branch_entries(None) {
        let mut object = entry.as_json().clone();
        object.insert(
            "parentId".into(),
            parent.clone().map_or(Value::Null, Value::String),
        );
        lines.push(Value::Object(object).to_string());
        parent = Some(entry.id().to_owned());
    }
    for entry in trailing(parent.as_deref(), &timestamp) {
        lines.push(entry.to_string());
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn message(value: Value) -> crate::session::AgentMessage {
        crate::session::AgentMessage::from_json(value.as_object().expect("object").clone())
    }

    // export-jsonl-share.test.ts: the branch is relinked from the root, and
    // the `pi.share` entry follows the last entry with the prompt and tools.
    #[test]
    fn the_branch_is_one_chain_with_the_share_entry_last() {
        let mut session = SessionManager::in_memory(
            Some("/work"),
            crate::session::NewSessionOptions::default(),
            Vec::new(),
        )
        .expect("session");
        let first = session
            .append_message(message(
                json!({"role": "user", "content": "one", "timestamp": 1}),
            ))
            .expect("first");
        session
            .append_message(message(
                json!({"role": "user", "content": "abandoned", "timestamp": 2}),
            ))
            .expect("abandoned");
        session.branch(&first).expect("branch");
        let kept = session
            .append_message(message(
                json!({"role": "user", "content": "two", "timestamp": 3}),
            ))
            .expect("kept");
        let text = serialize_session_branch(&session, |parent, timestamp| {
            vec![share_trailing_entry(
                parent,
                timestamp,
                "prompt",
                vec![json!({"name": "read", "description": "Read", "parameters": {}})],
            )]
        });
        assert!(text.ends_with('\n'));
        let lines: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("json"))
            .collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0]["type"], "session");
        assert_eq!(lines[0]["version"], CURRENT_SESSION_VERSION);
        assert_eq!(lines[0]["cwd"], "/work");
        assert_eq!(lines[1]["parentId"], Value::Null);
        assert_eq!(lines[1]["message"]["content"], "one");
        assert_eq!(lines[2]["parentId"], first.as_str());
        assert_eq!(lines[2]["id"], kept.as_str());
        assert_eq!(lines[3]["customType"], SHARE_ENTRY_TYPE);
        assert_eq!(lines[3]["parentId"], kept.as_str());
        assert_eq!(lines[3]["timestamp"], lines[0]["timestamp"]);
        assert_eq!(lines[3]["data"]["systemPrompt"], "prompt");
        assert_eq!(lines[3]["data"]["tools"][0]["name"], "read");
        assert_eq!(lines[3]["id"].as_str().map(str::len), Some(8));
        assert!(!text.contains("abandoned"));
    }
}
