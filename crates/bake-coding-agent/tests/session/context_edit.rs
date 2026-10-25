//! Pi `test/session-context-edit.test.ts`: the cases that exercise the
//! manager's projection. The cases about `prepareCompaction` and
//! `estimateProjectedContextTokens` belong to compaction (scope 10).

use bake_coding_agent::session::{
    AgentMessage, EditContent, FileEntry, NewSessionOptions, SessionError, SessionManager,
};
use serde_json::{Value, json};

use crate::support::{member, message, now, object, roles, text_of};

fn memory() -> SessionManager {
    SessionManager::in_memory(None, NewSessionOptions::default(), Vec::new()).expect("in memory")
}

fn assistant(text: &str) -> AgentMessage {
    message(json!({
        "role": "assistant", "content": [{"type": "text", "text": text}],
        "api": "faux", "provider": "faux", "model": "faux",
        "usage": {"input": 10, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 11,
                  "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
        "stopReason": "stop", "timestamp": now(),
    }))
}

fn user(text: &str) -> AgentMessage {
    message(json!({"role": "user", "content": text, "timestamp": now()}))
}

fn texts(session: &SessionManager) -> Vec<String> {
    session
        .build_session_projection()
        .messages
        .iter()
        .map(|message| match member(message, "summary") {
            Some(Value::String(summary)) => summary.clone(),
            _ => text_of(message),
        })
        .collect()
}

fn blocks(text: &str) -> EditContent {
    EditContent::Blocks(vec![json!({"type": "text", "text": text})])
}

/// "omits a target only from model projection"
#[test]
fn omits_a_target_only_from_model_projection() {
    let mut session = memory();
    session.append_message(user("request")).expect("append");
    let assistant_id = session
        .append_message(assistant("partial"))
        .expect("append");
    let result = message(json!({
        "role": "toolResult", "toolCallId": "call-1", "toolName": "read",
        "content": [{"type": "text", "text": "raw output"}], "details": {"path": "large.txt"},
        "isError": true, "timestamp": now(),
    }));
    let result_id = session.append_message(result.clone()).expect("append");
    session
        .append_context_edit(&assistant_id, None)
        .expect("edit");
    session.append_context_edit(&result_id, None).expect("edit");
    let messages = session
        .branch_entries(None)
        .into_iter()
        .filter(|entry| entry.entry_type() == "message")
        .count();
    assert_eq!(messages, 3);
    assert_eq!(
        roles(&session.build_session_projection().messages),
        ["user"]
    );
    assert_eq!(
        session.entry(&result_id).and_then(|entry| entry.message()),
        Some(result)
    );
}

/// "replaces only content and lets the latest edit win"
#[test]
fn replaces_only_content_and_the_latest_edit_wins() {
    let mut session = memory();
    let target = session
        .append_message(assistant("original"))
        .expect("append");
    session
        .append_context_edit(&target, Some(blocks("first")))
        .expect("edit");
    session.append_context_edit(&target, None).expect("edit");
    session
        .append_context_edit(&target, Some(blocks("restored")))
        .expect("edit");
    let projected = &session.build_session_projection().messages[0];
    assert_eq!(projected.role(), Some("assistant"));
    assert_eq!(text_of(projected), "restored");
    assert_eq!(
        member(projected, "usage").and_then(|usage| usage.get("totalTokens")),
        Some(&json!(11))
    );
    let stored = session
        .entry(&target)
        .and_then(|entry| entry.message())
        .expect("stored");
    assert_eq!(text_of(&stored), "original");
}

/// "normalizes string replacements for array-only assistant and tool-result
/// roles"
#[test]
fn normalizes_string_replacements() {
    let mut session = memory();
    let assistant_id = session
        .append_message(assistant("original"))
        .expect("append");
    let result_id = session
        .append_message(message(json!({
            "role": "toolResult", "toolCallId": "call-1", "toolName": "read",
            "content": [{"type": "text", "text": "original result"}], "isError": false, "timestamp": now(),
        })))
        .expect("append");
    let assistant_edit = session
        .append_context_edit(
            &assistant_id,
            Some(EditContent::Text("assistant replacement".into())),
        )
        .expect("edit");
    let result_edit = session
        .append_context_edit(
            &result_id,
            Some(EditContent::Text("result replacement".into())),
        )
        .expect("edit");
    assert_eq!(
        session
            .entry(&assistant_edit)
            .and_then(|entry| entry.get("replacement")),
        Some(&json!({"content": [{"type": "text", "text": "assistant replacement"}]}))
    );
    assert_eq!(
        session
            .entry(&result_edit)
            .and_then(|entry| entry.get("replacement")),
        Some(&json!({"content": [{"type": "text", "text": "result replacement"}]}))
    );
    let projected = session.build_session_projection().messages;
    assert_eq!(
        member(&projected[0], "content"),
        Some(&json!([{"type": "text", "text": "assistant replacement"}]))
    );
    assert_eq!(
        member(&projected[1], "content"),
        Some(&json!([{"type": "text", "text": "result replacement"}]))
    );
}

/// "normalizes imported string replacements while projecting array-only
/// roles": an edit stored with string content, as another writer may.
#[test]
fn normalizes_imported_string_replacements() {
    let entries = vec![
        FileEntry::from_json(object(json!({
            "type": "message", "id": "a1", "parentId": null, "timestamp": "2025-01-01T00:00:00.000Z",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "original"}],
                        "api": "faux", "provider": "faux", "model": "faux", "stopReason": "stop", "timestamp": 1},
        }))),
        FileEntry::from_json(object(json!({
            "type": "context_edit", "id": "e1", "parentId": "a1", "timestamp": "2025-01-01T00:00:01.000Z",
            "targetId": "a1", "replacement": {"content": "imported replacement"},
        }))),
    ];
    let session =
        SessionManager::in_memory(None, NewSessionOptions::default(), entries).expect("load");
    assert_eq!(
        member(&session.build_session_projection().messages[0], "content"),
        Some(&json!([{"type": "text", "text": "imported replacement"}]))
    );
}

/// "keeps edits branch-relative"
#[test]
fn keeps_edits_branch_relative() {
    let mut session = memory();
    let target = session.append_message(user("original")).expect("append");
    session
        .append_context_edit(&target, Some(EditContent::Text("edited".into())))
        .expect("edit");
    assert_eq!(texts(&session), ["edited"]);
    session.branch(&target).expect("branch");
    assert_eq!(texts(&session), ["original"]);
}

/// "uses a self-referencing compaction to retain no preceding entries"
#[test]
fn self_referencing_compaction_retains_nothing() {
    let mut session = memory();
    session.append_message(user("discarded")).expect("append");
    let compaction = session
        .append_compaction("exact handoff", None, 100, None, None, None)
        .expect("compaction");
    session.append_message(user("after")).expect("append");
    assert_eq!(
        session
            .entry(&compaction)
            .and_then(|entry| entry.get("firstKeptEntryId")),
        Some(&json!(compaction))
    );
    assert_eq!(
        roles(&session.build_session_projection().messages),
        ["compactionSummary", "user"]
    );
    assert_eq!(texts(&session), ["exact handoff", "after"]);
}

/// "applies post-compaction edits to retained pre-compaction entries"
#[test]
fn applies_post_compaction_edits_to_retained_entries() {
    let mut session = memory();
    session.append_message(user("summarized")).expect("append");
    let retained = session
        .append_message(user("original retained"))
        .expect("append");
    session
        .append_compaction("summary", Some(&retained), 100, None, None, None)
        .expect("compaction");
    session
        .append_context_edit(&retained, Some(EditContent::Text("edited retained".into())))
        .expect("edit");
    assert_eq!(texts(&session), ["summary", "edited retained"]);
}

/// "uses only the newest summary when a repeated compaction retains entries
/// before the older compaction" (the projection half)
#[test]
fn uses_only_the_newest_summary() {
    let mut session = memory();
    session
        .append_message(user("summarized first"))
        .expect("append");
    let retained = session.append_message(user("retained")).expect("append");
    session
        .append_compaction("first summary", Some(&retained), 100, None, None, None)
        .expect("compaction");
    session
        .append_message(assistant("after first compaction"))
        .expect("append");
    session
        .append_compaction("second summary", Some(&retained), 80, None, None, None)
        .expect("compaction");
    session
        .append_message(user(&"new tail ".repeat(100)))
        .expect("append");
    let summaries: Vec<String> = session
        .build_session_projection()
        .messages
        .iter()
        .filter_map(|message| {
            member(message, "summary")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(summaries, ["second summary"]);
}

/// "supports repeated retain-none compactions"
#[test]
fn supports_repeated_retain_none_compactions() {
    let mut session = memory();
    session.append_message(user("discarded")).expect("append");
    session
        .append_compaction("first handoff", None, 100, None, None, None)
        .expect("compaction");
    session
        .append_message(user("also discarded"))
        .expect("append");
    let second = session
        .append_compaction("second handoff", None, 50, None, None, None)
        .expect("compaction");
    assert_eq!(
        session
            .entry(&second)
            .and_then(|entry| entry.get("firstKeptEntryId")),
        Some(&json!(second))
    );
    assert_eq!(texts(&session), ["second handoff"]);
}

/// Pi's `appendContextEdit` errors: a missing target, one off the active
/// branch, and one without editable model content.
#[test]
fn rejects_invalid_targets() {
    let mut session = memory();
    let first = session.append_message(user("a")).expect("append");
    let model = session.append_model_change("p", "m").expect("append");
    let off_branch = session.append_message(user("b")).expect("append");
    session.branch(&first).expect("branch");
    assert!(matches!(
        session.append_context_edit("missing", None),
        Err(SessionError::EntryNotFound(_))
    ));
    let error = session
        .append_context_edit(&off_branch, None)
        .expect_err("off branch");
    assert_eq!(
        error.to_string(),
        format!("Entry {off_branch} is not on the active branch")
    );
    session.branch(&model).expect("branch");
    let error = session
        .append_context_edit(&model, None)
        .expect_err("not editable");
    assert_eq!(
        error.to_string(),
        format!("Entry {model} does not contribute editable model content")
    );
}

/// A compaction records the prompt and tool state of the context it
/// replaces, and the projection puts it before the summary.
#[test]
fn compaction_records_the_system_message() {
    let mut session = memory();
    session
        .append_message(message(json!({
            "role": "system", "content": "base prompt",
            "toolsAdded": [{"name": "read", "description": "Read", "parameters": {"type": "object"}, "extra": 1}],
            "timestamp": 5,
        })))
        .expect("append");
    session
        .append_message(message(json!({
            "role": "system", "content": [{"type": "text", "text": "more"}],
            "sections": {"rules": "be brief"}, "toolsRemoved": [{"name": "read"}], "timestamp": 6,
        })))
        .expect("append");
    session.append_message(user("hi")).expect("append");
    let compaction = session
        .append_compaction("sum", None, 10, None, None, None)
        .expect("compaction");
    let entry = session.entry(&compaction).expect("entry");
    let system = entry.get("systemMessage").expect("system message");
    let expected_time =
        bake_coding_agent::session::time::parse_date_ms(entry.timestamp().unwrap_or(""));
    assert_eq!(
        system,
        &json!({"role": "system", "content": "base prompt\n\nmore", "sections": {"rules": "be brief"}, "timestamp": expected_time})
    );
    assert_eq!(
        roles(&session.build_session_context().messages),
        ["system", "compactionSummary"]
    );
}
