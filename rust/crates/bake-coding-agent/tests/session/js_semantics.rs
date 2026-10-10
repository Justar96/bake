//! JavaScript value semantics Pi's session manager relies on when it reads
//! entries without validating them (Pi `core/session-manager.ts` v1.1.0:
//! `getSessionContextSettings`, `buildSessionPath`, `getBranch`,
//! `getLeafEntry`, `appendMessage`; `ai/src/utils/transcript.ts`:
//! `getCurrentSystemMessage`). No Pi test covers these hand-edited shapes.

use bake_coding_agent::session::{FileEntry, NewSessionOptions, SessionError, SessionManager};
use serde_json::{Value, json};

use crate::support::{message, object, roles, user_msg};

fn in_memory(values: Vec<Value>) -> SessionManager {
    let entries = values
        .into_iter()
        .map(|value| FileEntry::from_json(object(value)))
        .collect();
    SessionManager::in_memory(None, NewSessionOptions::default(), entries).expect("load")
}

fn assistant(id: &str, parent: &str, provider: Value) -> Value {
    json!({"type": "message", "id": id, "parentId": parent, "timestamp": "x", "message": {
        "role": "assistant", "content": [], "provider": provider, "model": "m", "timestamp": 1
    }})
}

/// `model = { provider: entry.message.provider, modelId: entry.message.model }`
/// replaces the model even when the members are not strings.
#[test]
fn an_assistant_without_a_string_provider_replaces_the_model() {
    let session = in_memory(vec![
        json!({"type": "model_change", "id": "a", "parentId": null, "timestamp": "x", "provider": "p", "modelId": "m"}),
        assistant("b", "a", json!(7)),
    ]);
    assert_eq!(session.build_session_context().model, None);
    let session = in_memory(vec![
        json!({"type": "model_change", "id": "a", "parentId": null, "timestamp": "x", "provider": "p", "modelId": "m"}),
        json!({"type": "model_change", "id": "b", "parentId": "a", "timestamp": "x", "provider": "q"}),
    ]);
    assert_eq!(session.build_session_context().model, None);
    let session = in_memory(vec![assistant("a", "", json!("p"))]);
    assert_eq!(
        session
            .build_session_context()
            .model
            .map(|model| model.provider),
        Some("p".to_owned())
    );
}

/// An empty `parentId` or leaf id is falsy: the walk stops, even when an
/// entry's id is the empty string.
#[test]
fn empty_ids_link_nowhere() {
    let session = in_memory(vec![
        json!({"type": "message", "id": "", "parentId": null, "timestamp": "x", "message": {"role": "user", "content": "root", "timestamp": 1}}),
        json!({"type": "message", "id": "b", "parentId": "", "timestamp": "x", "message": {"role": "user", "content": "b", "timestamp": 1}}),
        json!({"type": "message", "id": "c", "parentId": "b", "timestamp": "x", "message": {"role": "user", "content": "c", "timestamp": 1}}),
    ]);
    assert_eq!(session.build_session_context().messages.len(), 2);
    assert_eq!(session.branch_entries(None).len(), 2);
    assert!(session.branch_entries(Some("")).is_empty());

    // A leaf whose id is empty: no leaf entry, and the context starts from
    // the last entry, which is the leaf itself.
    let session = in_memory(vec![
        json!({"type": "message", "id": "a", "parentId": null, "timestamp": "x", "message": {"role": "user", "content": "a", "timestamp": 1}}),
        json!({"type": "message", "id": "", "parentId": "a", "timestamp": "x", "message": {"role": "user", "content": "leaf", "timestamp": 1}}),
    ]);
    assert_eq!(session.leaf_id(), Some(""));
    assert!(session.leaf_entry().is_none());
    assert!(session.branch_entries(None).is_empty());
    assert_eq!(
        roles(&session.build_session_context().messages),
        ["user", "user"]
    );
}

/// `timestamp ??= message.timestamp` keeps a `null`, which is not
/// `undefined`, so the system message is still recorded.
#[test]
fn a_null_system_timestamp_still_records_the_system_message() {
    let mut session = SessionManager::in_memory(None, NewSessionOptions::default(), Vec::new())
        .expect("in memory");
    session
        .append_message(message(
            json!({"role": "system", "content": "rules", "timestamp": null}),
        ))
        .expect("system");
    let user = session.append_message(user_msg("hi")).expect("user");
    let id = session
        .append_compaction("sum", Some(&user), 1, None, None, None)
        .expect("compaction");
    let system = session
        .entry(&id)
        .and_then(|entry| entry.get("systemMessage"));
    assert_eq!(
        system.and_then(|system| system.get("content")),
        Some(&json!("rules"))
    );
}

/// Pi's `appendMessage` type excludes summary messages; they are refused
/// before the session changes.
#[test]
fn summary_messages_are_not_appended_as_messages() {
    let mut session = SessionManager::in_memory(None, NewSessionOptions::default(), Vec::new())
        .expect("in memory");
    for role in ["branchSummary", "compactionSummary"] {
        let result = session.append_message(message(
            json!({"role": role, "summary": "s", "fromId": "x", "tokensBefore": 1, "timestamp": 1}),
        ));
        assert!(matches!(result, Err(SessionError::SummaryMessage(found)) if found == role));
    }
    assert_eq!(session.entry_count(), 0);
    assert_eq!(session.leaf_id(), None);
}
