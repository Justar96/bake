//! Untrusted session bytes: torn, malformed, and hostile files open without
//! a panic or a hang, as Pi's reader skips what it cannot parse.

use std::fs;

use bake_coding_agent::session::{FileEntry, NewSessionOptions, SessionError, SessionManager};
use serde_json::json;

use crate::support::{TempDir, assistant_msg, object, roles, text_of, user_msg};

fn sample_session(dir: &TempDir) -> std::path::PathBuf {
    let mut session =
        SessionManager::create(dir.str(), Some(dir.path()), NewSessionOptions::default())
            .expect("create");
    let first = session
        .append_message(user_msg("héllo 😀"))
        .expect("append");
    session
        .append_message(assistant_msg("answer"))
        .expect("append");
    session
        .append_thinking_level_change("high")
        .expect("append");
    session
        .append_label_change(&first, Some("start"))
        .expect("label");
    session
        .append_compaction(
            "sum",
            Some(&first),
            9,
            Some(json!({"k": [1, 2.5]})),
            None,
            None,
        )
        .expect("compaction");
    session
        .branch_with_summary(Some(&first), "left", None, None, None)
        .expect("summary");
    session.append_message(user_msg("after")).expect("append");
    session.session_file().expect("file").to_path_buf()
}

/// Every prefix of a session file, as a writer killed mid-line leaves one,
/// opens to a context no longer than the whole file's, and the torn tail is
/// set apart by a newline so the next append starts its own line.
#[test]
fn every_truncation_opens_and_repairs() {
    let dir = TempDir::new("robust-truncate");
    let source = sample_session(&dir);
    let bytes = fs::read(&source).expect("read");
    let full = SessionManager::open(&source, Some(dir.path()), None)
        .expect("open")
        .build_session_context()
        .messages
        .len();
    let header_end = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .expect("header line");
    let work = dir.join("work");
    fs::create_dir_all(&work).expect("mkdir");
    let file = work.join("torn.jsonl");
    for cut in 0..=bytes.len() {
        fs::write(&file, &bytes[..cut]).expect("write");
        match SessionManager::open(&file, Some(&work), None) {
            Ok(mut session) => {
                assert!(session.build_session_context().messages.len() <= full);
                if cut > header_end {
                    let written = fs::read(&file).expect("read");
                    assert_eq!(written.last(), Some(&b'\n'), "cut at {cut}");
                    session.append_message(user_msg("resumed")).expect("append");
                    let reopened = SessionManager::open(&file, Some(&work), None).expect("reopen");
                    let messages = reopened.build_session_context().messages;
                    if let Some(last) = messages.last() {
                        assert_eq!(text_of(last), "resumed", "cut at {cut}");
                    }
                }
            }
            Err(SessionError::NotASession(_)) => assert!(cut <= header_end, "cut at {cut}"),
            Err(error) => panic!("cut at {cut}: {error}"),
        }
    }
}

/// Malformed lines anywhere are skipped; invalid UTF-8 reads as U+FFFD.
#[test]
fn malformed_lines_and_bytes_are_skipped() {
    let dir = TempDir::new("robust-malformed");
    let file = dir.join("mixed.jsonl");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(br#"{"type":"session","version":3,"id":"s","timestamp":"2025-01-01T00:00:00Z","cwd":"/tmp"}"#);
    bytes.extend_from_slice(
        b"\n\xff\xfe garbage\n[1,2]\n42\n\"text\"\nnull\n{\"type\":\"message\"}\n",
    );
    bytes.extend_from_slice(br#"{"type":"message","id":"a","parentId":null,"timestamp":"2025-01-01T00:00:01Z","message":{"role":"user","content":"caf"#);
    bytes.extend_from_slice(b"\xc3");
    bytes.extend_from_slice(br#"","timestamp":1}}"#);
    bytes.extend_from_slice(b"\n");
    bytes.extend_from_slice(
        br#"{"type":"message","id":"b","parentId":"a","timestamp":"x","message":"not an object"}"#,
    );
    bytes.extend_from_slice(b"\n");
    bytes.extend_from_slice(br#"{"type":"message","id":"c","parentId":"b","timestamp":"x","message":{"role":"assistant","content":null}}"#);
    bytes.extend_from_slice(b"\n");
    bytes.extend_from_slice(
        br#"{"type":"context_edit","id":"d","parentId":"c","timestamp":"x","targetId":"c"}"#,
    );
    bytes.extend_from_slice(b"\n");
    bytes.extend_from_slice(br#"{"type":"message","id":"e","parentId":"d","timestamp":"x","message":{"role":"user","content":"\ud800"}}"#);
    bytes.extend_from_slice(b"\n");
    fs::write(&file, &bytes).expect("write");
    let session = SessionManager::open(&file, Some(dir.path()), None).expect("open");
    // `{"type":"message"}` has no id; the lone surrogate line reads, as
    // `JSON.parse` reads it, with U+FFFD in memory.
    assert_eq!(session.entries().len(), 5);
    assert!(
        session
            .file_entries()
            .iter()
            .any(|entry| matches!(entry, FileEntry::Unindexed(_)))
    );
    let messages = session.build_session_context().messages;
    assert_eq!(roles(&messages), ["user", "assistant", "user"]);
    assert_eq!(text_of(&messages[0]), "caf\u{fffd}");
    assert_eq!(messages[1].as_json().get("content"), Some(&json!([])));
    assert_eq!(text_of(&messages[2]), "\u{fffd}");
}

/// A parent cycle stops at the repeated entry instead of looping, in the
/// path, the tree, and the context.
#[test]
fn parent_cycles_do_not_hang() {
    let entries: Vec<FileEntry> = [
        json!({"type": "message", "id": "a", "parentId": "b", "timestamp": "x", "message": {"role": "user", "content": "a", "timestamp": 1}}),
        json!({"type": "message", "id": "b", "parentId": "a", "timestamp": "x", "message": {"role": "user", "content": "b", "timestamp": 1}}),
    ]
    .into_iter()
    .map(|value| FileEntry::from_json(object(value)))
    .collect();
    let mut session =
        SessionManager::in_memory(None, NewSessionOptions::default(), entries).expect("load");
    assert_eq!(session.branch_entries(None).len(), 2);
    assert_eq!(session.build_session_context().messages.len(), 2);
    assert!(session.tree().roots.is_empty());
    session.create_branched_session("b").expect("branch");
    assert_eq!(session.entries().len(), 2);
}

/// A deep linear session builds its context, tree, and branch without
/// recursion.
#[test]
fn deep_sessions_need_no_recursion() {
    let mut session = SessionManager::in_memory(None, NewSessionOptions::default(), Vec::new())
        .expect("in memory");
    for index in 0..20_000 {
        session
            .append_thinking_level_change(if index % 2 == 0 { "low" } else { "high" })
            .expect("append");
    }
    let tree = session.tree();
    assert_eq!(tree.roots.len(), 1);
    drop(tree);
    assert_eq!(session.build_session_context().thinking_level, "high");
    assert_eq!(session.branch_entries(None).len(), 20_000);
}

/// A header that is not the first parsed line, or has no string id, is not
/// a session, and the file is left untouched.
#[test]
fn header_must_come_first() {
    let dir = TempDir::new("robust-header");
    for content in [
        "42\n{\"type\":\"session\",\"id\":\"s\"}\n",
        "{\"type\":\"session\",\"id\":7}\n",
        "{\"type\":\"session\"}",
    ] {
        let file = dir.join("bad.jsonl");
        fs::write(&file, content).expect("write");
        assert!(matches!(
            SessionManager::open(&file, Some(dir.path()), None),
            Err(SessionError::NotASession(_))
        ));
        assert_eq!(fs::read_to_string(&file).expect("read"), content);
    }
}
