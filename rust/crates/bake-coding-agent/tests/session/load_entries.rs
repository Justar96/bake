//! Pi `test/session-manager/load-entries.test.ts`: in-memory sessions with
//! preloaded entries.

use bake_coding_agent::session::{FileEntry, NewSessionOptions, SessionManager};
use serde_json::json;

use crate::support::{message, now, object};

fn user_message(text: &str) -> bake_coding_agent::session::AgentMessage {
    message(
        json!({"role": "user", "content": [{"type": "text", "text": text}], "timestamp": now()}),
    )
}

fn stored_entries(build: impl FnOnce(&mut SessionManager)) -> Vec<FileEntry> {
    let mut source =
        SessionManager::in_memory(Some("/project"), NewSessionOptions::default(), Vec::new())
            .expect("source");
    build(&mut source);
    source
        .entries()
        .into_iter()
        .map(|entry| FileEntry::Entry(entry.clone()))
        .collect()
}

fn restore(options: NewSessionOptions, entries: Vec<FileEntry>) -> SessionManager {
    SessionManager::in_memory(Some("/project"), options, entries).expect("restore")
}

/// "adopts entries verbatim"
#[test]
fn adopts_entries_verbatim() {
    let entries = stored_entries(|source| {
        source
            .append_message(user_message("hello"))
            .expect("append");
        source
            .append_model_change("anthropic", "claude-opus-4-5")
            .expect("append");
        source
            .append_message(user_message("again"))
            .expect("append");
    });
    let session = restore(NewSessionOptions::default(), entries.clone());
    let adopted: Vec<FileEntry> = session
        .entries()
        .into_iter()
        .map(|entry| FileEntry::Entry(entry.clone()))
        .collect();
    assert_eq!(adopted, entries);
}

/// "keeps the loaded leaf so appends continue the conversation"
#[test]
fn keeps_the_loaded_leaf() {
    let entries = stored_entries(|source| {
        source
            .append_message(user_message("hello"))
            .expect("append");
        source
            .append_message(user_message("again"))
            .expect("append");
    });
    let last = entries
        .last()
        .and_then(FileEntry::as_entry)
        .expect("last")
        .id()
        .to_owned();
    let mut session = restore(NewSessionOptions::default(), entries);
    let appended = session
        .append_message(user_message("continued"))
        .expect("append");
    assert_eq!(session.leaf_id(), Some(appended.as_str()));
    assert_eq!(
        session.entry(&appended).and_then(|entry| entry.parent_id()),
        Some(last.as_str())
    );
}

/// "never mints an id that collides with a loaded entry"
#[test]
fn never_mints_a_colliding_id() {
    let entries = stored_entries(|source| {
        for index in 0..50 {
            source
                .append_message(user_message(&format!("message {index}")))
                .expect("append");
        }
    });
    let mut session = restore(NewSessionOptions::default(), entries.clone());
    let appended = session
        .append_message(user_message("continued"))
        .expect("append");
    assert!(
        !entries
            .iter()
            .any(|entry| entry.as_entry().is_some_and(|entry| entry.id() == appended))
    );
}

/// "rebuilds the branch structure rather than a flat chain"
#[test]
fn rebuilds_the_branch_structure() {
    let entries = stored_entries(|source| {
        let first = source
            .append_message(user_message("hello"))
            .expect("append");
        source
            .append_message(user_message("abandoned"))
            .expect("append");
        source.branch(&first).expect("branch");
        source.append_message(user_message("kept")).expect("append");
    });
    let session = restore(NewSessionOptions::default(), entries);
    let tree = session.tree();
    assert_eq!(tree.roots.len(), 1);
    let root = tree.root_nodes().next().expect("root");
    assert_eq!(root.children.len(), 2);
}

/// "rebuilds labels"
#[test]
fn rebuilds_labels() {
    let mut labelled = String::new();
    let entries = stored_entries(|source| {
        labelled = source
            .append_message(user_message("hello"))
            .expect("append");
        source
            .append_label_change(&labelled, Some("checkpoint"))
            .expect("label");
    });
    let session = restore(NewSessionOptions::default(), entries);
    assert_eq!(session.label(&labelled), Some("checkpoint"));
}

/// "resolves a compaction against the entry it was written against"
#[test]
fn resolves_a_compaction() {
    let mut kept = String::new();
    let entries = stored_entries(|source| {
        source
            .append_message(user_message("dropped"))
            .expect("append");
        kept = source.append_message(user_message("kept")).expect("append");
        source
            .append_compaction("summary so far", Some(&kept), 1000, None, None, None)
            .expect("compaction");
    });
    let session = restore(NewSessionOptions::default(), entries);
    assert!(
        session
            .build_context_entries()
            .iter()
            .any(|entry| entry.id() == kept)
    );
}

/// "creates a header from the options when the entries carry none"
#[test]
fn creates_a_header_from_the_options() {
    let entries = stored_entries(|source| {
        source
            .append_message(user_message("hello"))
            .expect("append");
    });
    let session = restore(NewSessionOptions::with_id("restored-session"), entries);
    assert_eq!(session.session_id(), "restored-session");
    let header = session.header().expect("header");
    assert_eq!(header.id(), "restored-session");
    assert_eq!(
        header.cwd(),
        Some(bake_coding_agent::session::paths::resolve_path_string("/project").as_str())
    );
}

/// "generates a session id when the options carry none"
#[test]
fn generates_a_session_id() {
    let entries = stored_entries(|source| {
        source
            .append_message(user_message("hello"))
            .expect("append");
    });
    let session = restore(NewSessionOptions::default(), entries);
    assert_eq!(session.session_id().len(), 36);
    assert_eq!(
        session.header().map(|header| header.id()),
        Some(session.session_id())
    );
}

/// "stays off the filesystem"
#[test]
fn stays_off_the_filesystem() {
    let entries = stored_entries(|source| {
        source
            .append_message(user_message("hello"))
            .expect("append");
    });
    let mut session = restore(NewSessionOptions::default(), entries);
    session
        .append_message(user_message("continued"))
        .expect("append");
    assert_eq!(session.session_file(), None);
    assert!(!session.is_persisted());
}

/// "starts an empty session when the entries are empty"
#[test]
fn starts_empty_with_empty_entries() {
    let session = restore(NewSessionOptions::with_id("empty-session"), Vec::new());
    assert_eq!(session.session_id(), "empty-session");
    assert!(session.entries().is_empty());
    assert_eq!(session.leaf_id(), None);
}

/// "takes the session identity from a header among the entries"
#[test]
fn takes_identity_from_a_header() {
    let mut entries = vec![FileEntry::from_json(object(json!({
        "type": "session", "version": 3, "id": "stored-session",
        "timestamp": "2026-01-01T00:00:00Z", "cwd": "/stored",
    })))];
    entries.extend(stored_entries(|source| {
        source
            .append_message(user_message("hello"))
            .expect("append");
    }));
    let session = restore(NewSessionOptions::with_id("ignored"), entries);
    assert_eq!(session.session_id(), "stored-session");
    assert_eq!(
        session.header().and_then(|header| header.cwd()),
        Some("/stored")
    );
}

fn hook_message_entry() -> FileEntry {
    FileEntry::from_json(object(json!({
        "type": "message", "id": "abc12345", "parentId": null,
        "timestamp": "2026-01-01T00:00:01Z",
        "message": {"role": "hookMessage", "content": "from a hook", "timestamp": 1},
    })))
}

/// "migrates entries restored with an older header"
#[test]
fn migrates_entries_with_an_older_header() {
    let entries = vec![
        FileEntry::from_json(object(json!({
            "type": "session", "version": 2, "id": "v2-session",
            "timestamp": "2026-01-01T00:00:00Z", "cwd": "/project",
        }))),
        hook_message_entry(),
    ];
    let session = restore(NewSessionOptions::default(), entries);
    assert_eq!(
        session
            .header()
            .and_then(|header| header.version().cloned()),
        Some(json!(3))
    );
    let restored = session.entries()[0];
    assert_eq!(restored.message_role(), Some("custom"));
    assert_eq!(restored.id(), "abc12345");
}

/// "adopts headerless entries as current-version without migrating them"
#[test]
fn adopts_headerless_entries_without_migrating() {
    let session = restore(NewSessionOptions::default(), vec![hook_message_entry()]);
    assert_eq!(session.entries()[0].message_role(), Some("hookMessage"));
}
