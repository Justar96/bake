//! Pi `test/session-manager/migration.test.ts`, and migration on open.

use bake_coding_agent::session::{FileEntry, SessionManager, migrate_session_entries};
use serde_json::{Value, json};

use crate::support::{TempDir, object, roles};

fn assistant() -> Value {
    json!({
        "role": "assistant", "content": [{"type": "text", "text": "hello"}],
        "api": "test", "provider": "test", "model": "test",
        "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0},
        "stopReason": "stop", "timestamp": 2,
    })
}

fn entries(values: Vec<Value>) -> Vec<FileEntry> {
    values
        .into_iter()
        .map(|value| FileEntry::from_json(object(value)))
        .collect()
}

/// "should add id/parentId to v1 entries"
#[test]
fn adds_ids_to_v1_entries() {
    let mut entries = entries(vec![
        json!({"type": "session", "id": "sess-1", "timestamp": "2025-01-01T00:00:00Z", "cwd": "/tmp"}),
        json!({"type": "message", "timestamp": "2025-01-01T00:00:01Z", "message": {"role": "user", "content": "hi", "timestamp": 1}}),
        json!({"type": "message", "timestamp": "2025-01-01T00:00:02Z", "message": assistant()}),
    ]);
    migrate_session_entries(&mut entries);
    assert_eq!(entries[0].as_json().get("version"), Some(&json!(3)));
    let first = entries[1].as_entry().expect("entry");
    let second = entries[2].as_entry().expect("entry");
    assert_eq!(first.id().len(), 8);
    assert_eq!(first.get("parentId"), Some(&Value::Null));
    assert_eq!(second.id().len(), 8);
    assert_eq!(second.parent_id(), Some(first.id()));
}

/// "should be idempotent (skip already migrated)"
#[test]
fn is_idempotent() {
    let mut entries = entries(vec![
        json!({"type": "session", "id": "sess-1", "version": 2, "timestamp": "2025-01-01T00:00:00Z", "cwd": "/tmp"}),
        json!({"type": "message", "id": "abc12345", "parentId": null, "timestamp": "2025-01-01T00:00:01Z", "message": {"role": "user", "content": "hi", "timestamp": 1}}),
        json!({"type": "message", "id": "def67890", "parentId": "abc12345", "timestamp": "2025-01-01T00:00:02Z", "message": assistant()}),
    ]);
    migrate_session_entries(&mut entries);
    assert_eq!(
        entries[1].as_entry().map(|entry| entry.id()),
        Some("abc12345")
    );
    assert_eq!(
        entries[2].as_entry().map(|entry| entry.id()),
        Some("def67890")
    );
    assert_eq!(
        entries[2].as_entry().and_then(|entry| entry.parent_id()),
        Some("abc12345")
    );
}

/// A v1 compaction's `firstKeptEntryIndex` becomes `firstKeptEntryId`, and
/// opening a v1 file rewrites it at version 3, as Pi's `_loadEntries` does.
#[test]
fn opening_a_v1_file_migrates_and_rewrites_it() {
    let dir = TempDir::new("migrate-open");
    let file = dir.join("v1.jsonl");
    let lines = [
        json!({"type": "session", "id": "v1", "timestamp": "2025-01-01T00:00:00Z", "cwd": "/tmp"}),
        json!({"type": "message", "timestamp": "2025-01-01T00:00:01Z", "message": {"role": "user", "content": "old", "timestamp": 1}}),
        json!({"type": "message", "timestamp": "2025-01-01T00:00:02Z", "message": {"role": "user", "content": "kept", "timestamp": 2}}),
        json!({"type": "compaction", "timestamp": "2025-01-01T00:00:03Z", "summary": "s", "firstKeptEntryIndex": 2, "tokensBefore": 5}),
        json!({"type": "message", "timestamp": "2025-01-01T00:00:04Z", "message": {"role": "hookMessage", "customType": "x", "content": "hook", "display": true, "timestamp": 4}}),
    ];
    let content: String = lines.iter().map(|line| format!("{line}\n")).collect();
    std::fs::write(&file, content).expect("write");
    let session = SessionManager::open(&file, Some(dir.path()), None).expect("open");
    let entries = session.entries();
    let compaction = entries[2];
    assert_eq!(
        compaction.get("firstKeptEntryId"),
        Some(&json!(entries[1].id()))
    );
    assert!(compaction.get("firstKeptEntryIndex").is_none());
    assert_eq!(
        roles(&session.build_session_context().messages),
        ["compactionSummary", "user", "custom"]
    );
    let rewritten = std::fs::read_to_string(&file).expect("read");
    let header: Value =
        serde_json::from_str(rewritten.lines().next().unwrap_or("")).expect("header");
    assert_eq!(header["version"], 3);
    assert_eq!(rewritten.lines().count(), 5);
    let reopened = SessionManager::open(&file, Some(dir.path()), None).expect("reopen");
    assert_eq!(
        reopened
            .entries()
            .iter()
            .map(|entry| entry.id().to_owned())
            .collect::<Vec<_>>(),
        entries
            .iter()
            .map(|entry| entry.id().to_owned())
            .collect::<Vec<_>>()
    );
}
