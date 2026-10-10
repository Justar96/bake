//! Pi `test/session-info-modified-timestamp.test.ts`, and the listing's
//! other `SessionInfo` members.

use bake_coding_agent::session::{NewSessionOptions, SessionManager};
use serde_json::json;

use crate::support::{TempDir, message, now, usage};

fn assistant(text: &str, timestamp: i64) -> bake_coding_agent::session::AgentMessage {
    message(json!({
        "role": "assistant", "content": [{"type": "text", "text": text}],
        "api": "openai-completions", "provider": "openai", "model": "test",
        "usage": usage(), "stopReason": "stop", "timestamp": timestamp,
    }))
}

/// "uses last user/assistant message timestamp instead of file mtime"
#[test]
fn modified_uses_the_last_message_timestamp() {
    let dir = TempDir::new("info-modified");
    let file = dir.join("session-modified.jsonl");
    let header = json!({
        "type": "session", "id": "test-session", "version": 3,
        "timestamp": "1970-01-01T00:00:00.000Z", "cwd": dir.str(),
    });
    std::fs::write(&file, format!("{header}\n")).expect("write");
    let mut first = SessionManager::open(&file, None, None).expect("open");
    first
        .append_message(assistant("hi", now()))
        .expect("append");
    let before = std::fs::metadata(&file)
        .and_then(|meta| meta.modified())
        .expect("mtime");

    let mut manager = SessionManager::open(&file, None, None).expect("open");
    let message_time = now() + 60_000;
    manager
        .append_message(assistant("later", message_time))
        .expect("append");

    let sessions = SessionManager::list(dir.str(), Some(dir.path()), None, None).expect("list");
    let info = sessions
        .iter()
        .find(|info| info.path == file)
        .expect("listed");
    assert_eq!(info.modified_ms, message_time);
    let before_ms = before
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0);
    assert_ne!(info.modified_ms, before_ms);
}

/// The other members `buildSessionInfo` fills: names, counts, first
/// message, all text, the parent, and progress.
#[test]
fn lists_names_counts_and_text() {
    let dir = TempDir::new("info-members");
    let mut session =
        SessionManager::create(dir.str(), Some(dir.path()), NewSessionOptions::default())
            .expect("create");
    session
        .append_message(message(
            json!({"role": "user", "content": "first ask", "timestamp": 10}),
        ))
        .expect("append");
    session
        .append_message(assistant("an answer", 20))
        .expect("append");
    session
        .append_session_info("  My\r\nsession  ")
        .expect("name");
    session
        .append_message(message(json!({"role": "toolResult", "toolCallId": "c", "toolName": "t", "content": [{"type": "text", "text": "skip"}], "isError": false, "timestamp": 30})))
        .expect("append");
    let mut updates = Vec::new();
    let mut progress =
        |loaded: usize,
         total: usize,
         partial: Option<&[bake_coding_agent::session::SessionInfo]>| {
            updates.push((loaded, total, partial.map(<[_]>::len)));
        };
    let sessions =
        SessionManager::list(dir.str(), Some(dir.path()), Some(&mut progress), None).expect("list");
    assert_eq!(updates, [(1, 1, Some(1))]);
    let info = &sessions[0];
    assert_eq!(info.name.as_deref(), Some("My session"));
    assert_eq!(session.session_name().as_deref(), Some("My session"));
    assert_eq!(info.message_count, 3);
    assert_eq!(info.first_message, "first ask");
    assert_eq!(info.all_messages_text, "first ask an answer");
    assert_eq!(info.modified_ms, 20);
    assert_eq!(info.id, session.session_id());
    assert_eq!(info.cwd, session.cwd());
    assert_eq!(info.parent_session_path, None);
}
