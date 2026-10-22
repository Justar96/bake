//! Pi `test/session-manager/custom-session-id.test.ts`.

use bake_coding_agent::session::{FileEntry, NewSessionOptions, SessionError, SessionManager};
use serde_json::json;

use crate::support::{TempDir, message, now, object, usage};

fn is_uuid_v7(id: &str) -> bool {
    let parts: Vec<&str> = id.split('-').collect();
    parts.iter().map(|part| part.len()).collect::<Vec<_>>() == [8, 4, 4, 4, 12]
        && id
            .chars()
            .all(|ch| ch == '-' || ch.is_ascii_digit() || ('a'..='f').contains(&ch))
        && parts[2].starts_with('7')
        && parts[3].starts_with(['8', '9', 'a', 'b'])
}

fn file_name_matches(name: &str, id: &str) -> bool {
    // ^\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}-\d{3}Z_<id>\.jsonl$
    let Some(stamp) = name.strip_suffix(&format!("_{id}.jsonl")) else {
        return false;
    };
    let pattern = "dddd-dd-ddTdd-dd-dd-dddZ";
    stamp.len() == pattern.len()
        && stamp
            .chars()
            .zip(pattern.chars())
            .all(|(ch, want)| match want {
                'd' => ch.is_ascii_digit(),
                other => ch == other,
            })
}

fn memory() -> SessionManager {
    SessionManager::in_memory(None, NewSessionOptions::default(), Vec::new()).expect("in memory")
}

/// "uses the provided id instead of generating one"
#[test]
fn uses_the_provided_id() {
    let mut session = memory();
    session
        .new_session(NewSessionOptions::with_id("my-custom-id"))
        .expect("new session");
    assert_eq!(session.session_id(), "my-custom-id");
}

/// "uses the provided id when creating an in-memory session"
#[test]
fn uses_the_provided_id_in_memory() {
    let session = SessionManager::in_memory(
        None,
        NewSessionOptions::with_id("memory-session-id"),
        Vec::new(),
    )
    .expect("in memory");
    assert_eq!(session.session_id(), "memory-session-id");
    assert_eq!(
        session.header().map(|header| header.id()),
        Some("memory-session-id")
    );
    assert_eq!(session.session_file(), None);
}

/// "allows alphanumeric session ids with interior punctuation"
#[test]
fn allows_interior_punctuation() {
    let mut session = memory();
    session
        .new_session(NewSessionOptions::with_id("abc-123_def.456"))
        .expect("new session");
    assert_eq!(session.session_id(), "abc-123_def.456");
}

/// "rejects invalid custom session ids"
#[test]
fn rejects_invalid_custom_session_ids() {
    for id in [
        "", "-abc", "abc-", "_abc", "abc_", ".abc", "abc.", "abc/def", "abc\\def", "abc def",
    ] {
        let mut session = memory();
        let error = session.new_session(NewSessionOptions::with_id(id));
        assert!(matches!(error, Err(SessionError::InvalidSessionId)), "{id}");
        let text = error
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(
            text.starts_with("Session id must be non-empty, contain only alphanumeric characters")
        );
    }
}

/// "generates a UUIDv7 id when no id is provided", "... when options is
/// provided without id", and "... when constructed without an explicit id"
#[test]
fn generates_uuid_v7_ids() {
    let mut session = memory();
    assert!(is_uuid_v7(session.session_id()));
    assert_eq!(
        session.header().map(|header| header.id()),
        Some(session.session_id())
    );
    session
        .new_session(NewSessionOptions::default())
        .expect("new");
    assert!(is_uuid_v7(session.session_id()));
    session
        .new_session(NewSessionOptions {
            id: None,
            parent_session: Some("parent.jsonl".into()),
        })
        .expect("new");
    assert!(is_uuid_v7(session.session_id()));
    assert_eq!(
        session.header().and_then(|header| header.parent_session()),
        Some("parent.jsonl")
    );
}

/// "includes the custom id in the session header"
#[test]
fn includes_the_custom_id_in_the_header() {
    let mut session = memory();
    session
        .new_session(NewSessionOptions::with_id("header-test-id"))
        .expect("new");
    assert_eq!(
        session.header().map(|header| header.id()),
        Some("header-test-id")
    );
}

/// "uses the provided id when creating a persisted session"
#[test]
fn uses_the_provided_id_when_persisted() {
    let dir = TempDir::new("custom-id");
    let session = SessionManager::create(
        dir.str(),
        Some(dir.path()),
        NewSessionOptions::with_id("created-session-id"),
    )
    .expect("create");
    assert_eq!(session.session_id(), "created-session-id");
    let file = session.session_file().expect("file").to_path_buf();
    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    assert!(file_name_matches(name, "created-session-id"), "{name}");
    assert!(!file.exists());
}

/// "generates a UUIDv7 id when creating a branched session"
#[test]
fn generates_uuid_v7_for_branched_session() {
    let mut session = memory();
    let first = session
        .append_message(message(json!({"role": "user", "content": [{"type": "text", "text": "hello"}], "timestamp": now()})))
        .expect("append");
    session.create_branched_session(&first).expect("branch");
    assert!(is_uuid_v7(session.session_id()));
    assert_eq!(
        session.header().map(|header| header.id()),
        Some(session.session_id())
    );
}

fn write_source(dir: &TempDir, with_message: bool) -> std::path::PathBuf {
    let source = dir.join("source.jsonl");
    let mut lines = vec![
        json!({
            "type": "session", "version": 3, "id": "legacy-session-id",
            "timestamp": "2025-01-01T00:00:00.000Z", "cwd": dir.str(),
        })
        .to_string(),
    ];
    if with_message {
        lines.push(
            json!({
                "type": "message", "id": "entry-1", "parentId": null,
                "timestamp": "2025-01-01T00:00:01.000Z",
                "message": {
                    "role": "assistant", "content": [{"type": "text", "text": "hello"}],
                    "api": "openai-responses", "provider": "openai", "model": "gpt-5.4",
                    "usage": usage(), "stopReason": "stop", "timestamp": now(),
                },
            })
            .to_string(),
        );
    }
    std::fs::write(&source, format!("{}\n", lines.join("\n"))).expect("write source");
    source
}

/// "generates a UUIDv7 id when forking from another session file"
#[test]
fn generates_uuid_v7_when_forking() {
    let dir = TempDir::new("fork-id");
    let source = write_source(&dir, true);
    let forked = SessionManager::fork_from(
        &source,
        dir.str(),
        Some(dir.path()),
        NewSessionOptions::default(),
    )
    .expect("fork");
    let header = forked.header().expect("header");
    assert!(is_uuid_v7(header.id()));
    assert_eq!(header.parent_session(), source.to_str());
    assert_eq!(forked.entries().len(), 1);
}

/// "uses the provided id when forking from another session file"
#[test]
fn uses_the_provided_id_when_forking() {
    let dir = TempDir::new("fork-custom-id");
    let source = write_source(&dir, false);
    let forked = SessionManager::fork_from(
        &source,
        dir.str(),
        Some(dir.path()),
        NewSessionOptions::with_id("forked-session-id"),
    )
    .expect("fork");
    assert_eq!(
        forked.header().map(|header| header.id()),
        Some("forked-session-id")
    );
    assert_eq!(
        forked.header().and_then(|header| header.parent_session()),
        source.to_str()
    );
    let file = forked.session_file().expect("file");
    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    assert!(file_name_matches(name, "forked-session-id"), "{name}");
}

/// An in-memory session adopts a header among its entries.
#[test]
fn in_memory_entries_keep_their_header() {
    let header = FileEntry::from_json(object(json!({
        "type": "session", "version": 3, "id": "stored", "timestamp": "2026-01-01T00:00:00Z", "cwd": "/stored",
    })));
    let session =
        SessionManager::in_memory(Some("/project"), NewSessionOptions::default(), vec![header])
            .expect("in memory");
    assert_eq!(session.session_id(), "stored");
}
