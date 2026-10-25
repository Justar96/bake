//! Pi `test/session-manager/labels.test.ts`.

use bake_coding_agent::session::{NewSessionOptions, SessionError, SessionManager};
use serde_json::json;

use crate::support::{message, roles, usage};

fn memory() -> SessionManager {
    SessionManager::in_memory(None, NewSessionOptions::default(), Vec::new()).expect("in memory")
}

fn user(session: &mut SessionManager, text: &str, timestamp: i64) -> String {
    session
        .append_message(message(
            json!({"role": "user", "content": text, "timestamp": timestamp}),
        ))
        .expect("append")
}

fn assistant(session: &mut SessionManager) -> String {
    session
        .append_message(message(json!({
            "role": "assistant", "content": [{"type": "text", "text": "hi"}],
            "api": "anthropic-messages", "provider": "anthropic", "model": "test",
            "usage": usage(), "stopReason": "stop", "timestamp": 2,
        })))
        .expect("append")
}

fn timestamp_of(session: &SessionManager, id: &str) -> Option<String> {
    session
        .entry(id)
        .and_then(|entry| entry.timestamp())
        .map(str::to_owned)
}

/// "sets and gets labels"
#[test]
fn sets_and_gets_labels() {
    let mut session = memory();
    let message_id = user(&mut session, "hello", 1);
    assert_eq!(session.label(&message_id), None);
    let label_id = session
        .append_label_change(&message_id, Some("checkpoint"))
        .expect("label");
    assert_eq!(session.label(&message_id), Some("checkpoint"));
    let entries = session.entries();
    let label = entries
        .iter()
        .find(|entry| entry.entry_type() == "label")
        .expect("label entry");
    assert_eq!(label.id(), label_id);
    assert_eq!(label.get("targetId"), Some(&json!(message_id)));
    assert_eq!(label.get("label"), Some(&json!("checkpoint")));
}

/// "clears labels with undefined"
#[test]
fn clears_labels_with_none() {
    let mut session = memory();
    let message_id = user(&mut session, "hello", 1);
    session
        .append_label_change(&message_id, Some("checkpoint"))
        .expect("label");
    session
        .append_label_change(&message_id, None)
        .expect("clear");
    assert_eq!(session.label(&message_id), None);
    let clear = session.leaf_entry().expect("leaf");
    assert!(!clear.as_json().contains_key("label"));
}

/// "last label wins"
#[test]
fn last_label_wins() {
    let mut session = memory();
    let message_id = user(&mut session, "hello", 1);
    session
        .append_label_change(&message_id, Some("first"))
        .expect("label");
    session
        .append_label_change(&message_id, Some("second"))
        .expect("label");
    let last = session
        .append_label_change(&message_id, Some("third"))
        .expect("label");
    assert_eq!(session.label(&message_id), Some("third"));
    let tree = session.tree();
    let node = tree
        .root_nodes()
        .find(|node| node.entry.id() == message_id)
        .expect("node");
    assert_eq!(node.label_timestamp, timestamp_of(&session, &last));
}

/// "labels are included in tree nodes"
#[test]
fn labels_are_included_in_tree_nodes() {
    let mut session = memory();
    let first = user(&mut session, "hello", 1);
    let second = assistant(&mut session);
    let first_label = session
        .append_label_change(&first, Some("start"))
        .expect("label");
    let second_label = session
        .append_label_change(&second, Some("response"))
        .expect("label");
    let tree = session.tree();
    let first_node = tree
        .root_nodes()
        .find(|node| node.entry.id() == first)
        .expect("node");
    assert_eq!(first_node.label.as_deref(), Some("start"));
    assert_eq!(
        first_node.label_timestamp,
        timestamp_of(&session, &first_label)
    );
    let second_node = tree
        .children(first_node)
        .find(|node| node.entry.id() == second)
        .expect("child");
    assert_eq!(second_node.label.as_deref(), Some("response"));
    assert_eq!(
        second_node.label_timestamp,
        timestamp_of(&session, &second_label)
    );
}

/// "labels are preserved in createBranchedSession"
#[test]
fn labels_are_preserved_in_create_branched_session() {
    let mut session = memory();
    let first = user(&mut session, "hello", 1);
    let second = assistant(&mut session);
    let first_label = session
        .append_label_change(&first, Some("important"))
        .expect("label");
    let second_label = session
        .append_label_change(&second, Some("also-important"))
        .expect("label");
    let first_time = timestamp_of(&session, &first_label);
    let second_time = timestamp_of(&session, &second_label);
    assert_eq!(
        session.create_branched_session(&second).expect("branch"),
        None
    );
    assert_eq!(session.label(&first), Some("important"));
    assert_eq!(session.label(&second), Some("also-important"));
    let labels = session
        .entries()
        .into_iter()
        .filter(|entry| entry.entry_type() == "label")
        .count();
    assert_eq!(labels, 2);
    let tree = session.tree();
    let first_node = tree
        .root_nodes()
        .find(|node| node.entry.id() == first)
        .expect("node");
    let second_node = tree
        .children(first_node)
        .find(|node| node.entry.id() == second)
        .expect("child");
    assert_eq!(first_node.label_timestamp, first_time);
    assert_eq!(second_node.label_timestamp, second_time);
}

/// "rewires children of removed labels when forking"
#[test]
fn rewires_children_of_removed_labels() {
    let mut session = memory();
    let first = user(&mut session, "hello", 1);
    session
        .append_label_change(&first, Some("checkpoint"))
        .expect("label");
    let model_change = session
        .append_model_change("anthropic", "claude-test")
        .expect("model");
    let second = user(&mut session, "followup", 2);
    session.create_branched_session(&second).expect("branch");
    assert_eq!(
        session
            .entry(&model_change)
            .and_then(|entry| entry.parent_id()),
        Some(first.as_str())
    );
}

/// "labels not on path are not preserved in createBranchedSession"
#[test]
fn labels_off_the_path_are_dropped() {
    let mut session = memory();
    let first = user(&mut session, "hello", 1);
    let second = assistant(&mut session);
    let third = user(&mut session, "followup", 3);
    session
        .append_label_change(&first, Some("first"))
        .expect("label");
    session
        .append_label_change(&second, Some("second"))
        .expect("label");
    session
        .append_label_change(&third, Some("third"))
        .expect("label");
    session.create_branched_session(&second).expect("branch");
    assert_eq!(session.label(&first), Some("first"));
    assert_eq!(session.label(&second), Some("second"));
    assert_eq!(session.label(&third), None);
}

/// "labels are not included in buildSessionContext"
#[test]
fn labels_are_not_in_context() {
    let mut session = memory();
    let message_id = user(&mut session, "hello", 1);
    session
        .append_label_change(&message_id, Some("checkpoint"))
        .expect("label");
    assert_eq!(roles(&session.build_session_context().messages), ["user"]);
}

/// "throws when labeling non-existent entry"
#[test]
fn rejects_labels_on_missing_entries() {
    let mut session = memory();
    let error = session
        .append_label_change("non-existent", Some("label"))
        .expect_err("missing");
    assert!(matches!(error, SessionError::EntryNotFound(_)));
    assert_eq!(error.to_string(), "Entry non-existent not found");
}
