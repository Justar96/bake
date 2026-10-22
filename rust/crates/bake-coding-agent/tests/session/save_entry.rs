//! Pi `test/session-manager/save-entry.test.ts`.

use bake_coding_agent::session::{NewSessionOptions, SessionManager};
use serde_json::json;

use crate::support::{ids, message, usage};

/// "saves custom entries and includes them in tree traversal"
#[test]
fn saves_custom_entries_in_the_tree() {
    let mut session = SessionManager::in_memory(None, NewSessionOptions::default(), Vec::new())
        .expect("in memory");
    let first = session
        .append_message(message(
            json!({"role": "user", "content": "hello", "timestamp": 1}),
        ))
        .expect("append");
    let custom = session
        .append_custom_entry("my_data", Some(json!({"foo": "bar"})))
        .expect("custom");
    let second = session
        .append_message(message(json!({
            "role": "assistant", "content": [{"type": "text", "text": "hi"}],
            "api": "anthropic-messages", "provider": "anthropic", "model": "test",
            "usage": usage(), "stopReason": "stop", "timestamp": 2,
        })))
        .expect("append");
    let entries = session.entries();
    assert_eq!(entries.len(), 3);
    let entry = entries
        .iter()
        .find(|entry| entry.entry_type() == "custom")
        .expect("custom entry");
    assert_eq!(entry.get("customType"), Some(&json!("my_data")));
    assert_eq!(entry.get("data"), Some(&json!({"foo": "bar"})));
    assert_eq!(entry.id(), custom);
    assert_eq!(entry.parent_id(), Some(first.as_str()));
    assert_eq!(ids(&session.branch_entries(None)), [first, custom, second]);
    assert_eq!(session.build_session_context().messages.len(), 2);
}
