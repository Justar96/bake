//! Pi `test/session-manager/build-context.test.ts`, and the
//! `buildSessionContext` cases of `test/compaction.test.ts`.

use bake_coding_agent::session::{
    LeafSelector, ModelRef, SessionEntry, build_context_entries, build_session_context,
};
use serde_json::{Value, json};

use crate::support::{entry, member, roles, text_of, usage};

const TS: &str = "2025-01-01T00:00:00Z";

fn msg(id: &str, parent: Option<&str>, role: &str, text: &str) -> SessionEntry {
    let message = if role == "user" {
        json!({"role": role, "content": text, "timestamp": 1})
    } else {
        json!({
            "role": role,
            "content": [{"type": "text", "text": text}],
            "api": "anthropic-messages",
            "provider": "anthropic",
            "model": "claude-test",
            "usage": usage(),
            "stopReason": "stop",
            "timestamp": 1,
        })
    };
    entry(
        json!({"type": "message", "id": id, "parentId": parent, "timestamp": TS, "message": message}),
    )
}

fn compaction(id: &str, parent: Option<&str>, summary: &str, first_kept: &str) -> SessionEntry {
    entry(json!({
        "type": "compaction", "id": id, "parentId": parent, "timestamp": TS,
        "summary": summary, "firstKeptEntryId": first_kept, "tokensBefore": 1000,
    }))
}

fn branch_summary(id: &str, parent: Option<&str>, summary: &str, from_id: &str) -> SessionEntry {
    entry(json!({
        "type": "branch_summary", "id": id, "parentId": parent, "timestamp": TS,
        "summary": summary, "fromId": from_id,
    }))
}

fn custom(id: &str, parent: Option<&str>, custom_type: &str, data: Value) -> SessionEntry {
    entry(json!({
        "type": "custom", "id": id, "parentId": parent, "timestamp": TS,
        "customType": custom_type, "data": data,
    }))
}

fn thinking_level(id: &str, parent: Option<&str>, level: &str) -> SessionEntry {
    entry(json!({
        "type": "thinking_level_change", "id": id, "parentId": parent, "timestamp": TS,
        "thinkingLevel": level,
    }))
}

fn model_change(id: &str, parent: Option<&str>, provider: &str, model_id: &str) -> SessionEntry {
    entry(json!({
        "type": "model_change", "id": id, "parentId": parent, "timestamp": TS,
        "provider": provider, "modelId": model_id,
    }))
}

fn summary_of(message: &bake_coding_agent::session::AgentMessage) -> String {
    member(message, "summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn claude() -> Option<ModelRef> {
    Some(ModelRef {
        provider: "anthropic".into(),
        model_id: "claude-test".into(),
    })
}

/// "trivial cases > empty entries returns empty context"
#[test]
fn empty_entries_returns_empty_context() {
    let ctx = build_session_context(&[], LeafSelector::Last);
    assert!(ctx.messages.is_empty());
    assert_eq!(ctx.thinking_level, "off");
    assert_eq!(ctx.model, None);
}

/// "trivial cases > single user message"
#[test]
fn single_user_message() {
    let ctx = build_session_context(&[msg("1", None, "user", "hello")], LeafSelector::Last);
    assert_eq!(roles(&ctx.messages), ["user"]);
}

/// "trivial cases > simple conversation"
#[test]
fn simple_conversation() {
    let entries = [
        msg("1", None, "user", "hello"),
        msg("2", Some("1"), "assistant", "hi there"),
        msg("3", Some("2"), "user", "how are you"),
        msg("4", Some("3"), "assistant", "great"),
    ];
    let ctx = build_session_context(&entries, LeafSelector::Last);
    assert_eq!(
        roles(&ctx.messages),
        ["user", "assistant", "user", "assistant"]
    );
}

/// "trivial cases > tracks thinking level changes"
#[test]
fn tracks_thinking_level_changes() {
    let entries = [
        msg("1", None, "user", "hello"),
        thinking_level("2", Some("1"), "high"),
        msg("3", Some("2"), "assistant", "thinking hard"),
    ];
    let ctx = build_session_context(&entries, LeafSelector::Last);
    assert_eq!(ctx.thinking_level, "high");
    assert_eq!(ctx.messages.len(), 2);
}

/// "trivial cases > tracks model from assistant message"
#[test]
fn tracks_model_from_assistant_message() {
    let entries = [
        msg("1", None, "user", "hello"),
        msg("2", Some("1"), "assistant", "hi"),
    ];
    assert_eq!(
        build_session_context(&entries, LeafSelector::Last).model,
        claude()
    );
}

/// "trivial cases > tracks model from model change entry"
#[test]
fn tracks_model_from_model_change_entry() {
    let entries = [
        msg("1", None, "user", "hello"),
        model_change("2", Some("1"), "openai", "gpt-4"),
        msg("3", Some("2"), "assistant", "hi"),
    ];
    // The assistant message overwrites the model change.
    assert_eq!(
        build_session_context(&entries, LeafSelector::Last).model,
        claude()
    );
    let changed = [
        msg("1", None, "user", "hello"),
        model_change("2", Some("1"), "openai", "gpt-4"),
    ];
    assert_eq!(
        build_session_context(&changed, LeafSelector::Last).model,
        Some(ModelRef {
            provider: "openai".into(),
            model_id: "gpt-4".into()
        })
    );
}

/// "with compaction > includes summary before kept messages"
#[test]
fn includes_summary_before_kept_messages() {
    let entries = [
        msg("1", None, "user", "first"),
        msg("2", Some("1"), "assistant", "response1"),
        msg("3", Some("2"), "user", "second"),
        msg("4", Some("3"), "assistant", "response2"),
        compaction("5", Some("4"), "Summary of first two turns", "3"),
        msg("6", Some("5"), "user", "third"),
        msg("7", Some("6"), "assistant", "response3"),
    ];
    let ctx = build_session_context(&entries, LeafSelector::Last);
    assert_eq!(ctx.messages.len(), 5);
    assert!(summary_of(&ctx.messages[0]).contains("Summary of first two turns"));
    assert_eq!(text_of(&ctx.messages[1]), "second");
    assert_eq!(text_of(&ctx.messages[2]), "response2");
    assert_eq!(text_of(&ctx.messages[3]), "third");
    assert_eq!(text_of(&ctx.messages[4]), "response3");
}

/// "with compaction > handles compaction keeping from first message"
#[test]
fn handles_compaction_keeping_from_first_message() {
    let entries = [
        msg("1", None, "user", "first"),
        msg("2", Some("1"), "assistant", "response"),
        compaction("3", Some("2"), "Empty summary", "1"),
        msg("4", Some("3"), "user", "second"),
    ];
    let ctx = build_session_context(&entries, LeafSelector::Last);
    assert_eq!(ctx.messages.len(), 4);
    assert!(summary_of(&ctx.messages[0]).contains("Empty summary"));
}

/// "with compaction > multiple compactions uses latest"
#[test]
fn multiple_compactions_uses_latest() {
    let entries = [
        msg("1", None, "user", "a"),
        msg("2", Some("1"), "assistant", "b"),
        compaction("3", Some("2"), "First summary", "1"),
        msg("4", Some("3"), "user", "c"),
        msg("5", Some("4"), "assistant", "d"),
        compaction("6", Some("5"), "Second summary", "4"),
        msg("7", Some("6"), "user", "e"),
    ];
    let ctx = build_session_context(&entries, LeafSelector::Last);
    assert_eq!(ctx.messages.len(), 4);
    assert!(summary_of(&ctx.messages[0]).contains("Second summary"));
}

/// "with compaction > buildContextEntries returns compaction-aware entries
/// including custom entries"
#[test]
fn build_context_entries_includes_custom_entries() {
    let entries = [
        msg("1", None, "user", "first"),
        custom("2", Some("1"), "old-state", json!({"hidden": true})),
        msg("3", Some("2"), "assistant", "response1"),
        custom("4", Some("3"), "kept-card", json!({"title": "Kept"})),
        msg("5", Some("4"), "user", "second"),
        compaction("6", Some("5"), "Summary", "4"),
        custom("7", Some("6"), "after-card", json!({"title": "After"})),
        msg("8", Some("7"), "assistant", "response2"),
    ];
    let context_ids: Vec<String> = build_context_entries(&entries, LeafSelector::Last)
        .iter()
        .map(|entry| entry.id().to_owned())
        .collect();
    assert_eq!(context_ids, ["6", "4", "5", "7", "8"]);
    let ctx = build_session_context(&entries, LeafSelector::Last);
    assert_eq!(
        roles(&ctx.messages),
        ["compactionSummary", "user", "assistant"]
    );
}

/// "with compaction > keeps settings from the full path after compaction"
#[test]
fn keeps_settings_from_the_full_path_after_compaction() {
    let entries = [
        msg("1", None, "user", "first"),
        thinking_level("2", Some("1"), "high"),
        msg("3", Some("2"), "assistant", "response1"),
        msg("4", Some("3"), "user", "second"),
        compaction("5", Some("4"), "Summary", "4"),
    ];
    let ctx = build_session_context(&entries, LeafSelector::Last);
    assert_eq!(ctx.thinking_level, "high");
    assert_eq!(roles(&ctx.messages), ["compactionSummary", "user"]);
}

/// "with branches > follows path to specified leaf"
#[test]
fn follows_path_to_specified_leaf() {
    let entries = [
        msg("1", None, "user", "start"),
        msg("2", Some("1"), "assistant", "response"),
        msg("3", Some("2"), "user", "branch A"),
        msg("4", Some("2"), "user", "branch B"),
    ];
    let a = build_session_context(&entries, LeafSelector::Id("3"));
    assert_eq!(a.messages.len(), 3);
    assert_eq!(text_of(&a.messages[2]), "branch A");
    let b = build_session_context(&entries, LeafSelector::Id("4"));
    assert_eq!(b.messages.len(), 3);
    assert_eq!(text_of(&b.messages[2]), "branch B");
}

/// "with branches > includes branch summary in path"
#[test]
fn includes_branch_summary_in_path() {
    let entries = [
        msg("1", None, "user", "start"),
        msg("2", Some("1"), "assistant", "response"),
        msg("3", Some("2"), "user", "abandoned path"),
        branch_summary("4", Some("2"), "Summary of abandoned work", "3"),
        msg("5", Some("4"), "user", "new direction"),
    ];
    let ctx = build_session_context(&entries, LeafSelector::Id("5"));
    assert_eq!(ctx.messages.len(), 4);
    assert!(summary_of(&ctx.messages[2]).contains("Summary of abandoned work"));
    assert_eq!(text_of(&ctx.messages[3]), "new direction");
}

/// "with branches > complex tree with multiple branches and compaction"
#[test]
fn complex_tree_with_multiple_branches_and_compaction() {
    let entries = [
        msg("1", None, "user", "start"),
        msg("2", Some("1"), "assistant", "r1"),
        msg("3", Some("2"), "user", "q2"),
        msg("4", Some("3"), "assistant", "r2"),
        compaction("5", Some("4"), "Compacted history", "3"),
        msg("6", Some("5"), "user", "q3"),
        msg("7", Some("6"), "assistant", "r3"),
        msg("8", Some("3"), "user", "wrong path"),
        msg("9", Some("8"), "assistant", "wrong response"),
        branch_summary("10", Some("3"), "Tried wrong approach", "9"),
        msg("11", Some("10"), "user", "better approach"),
    ];
    let main = build_session_context(&entries, LeafSelector::Id("7"));
    assert_eq!(main.messages.len(), 5);
    assert!(summary_of(&main.messages[0]).contains("Compacted history"));
    let texts: Vec<String> = main.messages[1..].iter().map(text_of).collect();
    assert_eq!(texts, ["q2", "r2", "q3", "r3"]);

    let branch = build_session_context(&entries, LeafSelector::Id("11"));
    assert_eq!(branch.messages.len(), 5);
    assert_eq!(text_of(&branch.messages[0]), "start");
    assert_eq!(text_of(&branch.messages[1]), "r1");
    assert_eq!(text_of(&branch.messages[2]), "q2");
    assert!(summary_of(&branch.messages[3]).contains("Tried wrong approach"));
    assert_eq!(text_of(&branch.messages[4]), "better approach");
}

/// "edge cases > uses last entry when leafId not found"
#[test]
fn uses_last_entry_when_leaf_id_not_found() {
    let entries = [
        msg("1", None, "user", "hello"),
        msg("2", Some("1"), "assistant", "hi"),
    ];
    let ctx = build_session_context(&entries, LeafSelector::Id("nonexistent"));
    assert_eq!(ctx.messages.len(), 2);
}

/// "edge cases > handles orphaned entries gracefully"
#[test]
fn handles_orphaned_entries_gracefully() {
    let entries = [
        msg("1", None, "user", "hello"),
        msg("2", Some("missing"), "assistant", "orphan"),
    ];
    let ctx = build_session_context(&entries, LeafSelector::Id("2"));
    assert_eq!(ctx.messages.len(), 1);
}

/// compaction.test.ts: "buildSessionContext > should track model and
/// thinking level changes"
#[test]
fn tracks_model_and_thinking_level_changes() {
    let entries = [
        msg("1", None, "user", "1"),
        model_change("2", Some("1"), "openai", "gpt-4"),
        msg("3", Some("2"), "assistant", "a"),
        thinking_level("4", Some("3"), "high"),
    ];
    let ctx = build_session_context(&entries, LeafSelector::Last);
    assert_eq!(ctx.model, claude());
    assert_eq!(ctx.thinking_level, "high");
}

/// compaction.test.ts: "buildSessionContext > should keep all messages when
/// firstKeptEntryId is first entry"
#[test]
fn keeps_all_messages_when_first_kept_is_first_entry() {
    let entries = [
        msg("1", None, "user", "1"),
        msg("2", Some("1"), "assistant", "a"),
        compaction("3", Some("2"), "First summary", "1"),
        msg("4", Some("3"), "user", "2"),
        msg("5", Some("4"), "assistant", "b"),
    ];
    assert_eq!(
        build_session_context(&entries, LeafSelector::Last)
            .messages
            .len(),
        5
    );
}

/// The summary messages Pi builds: members in Pi's order and the entry's
/// timestamp in milliseconds; `null` for an unparseable one.
#[test]
fn summary_messages_take_pi_shapes() {
    let entries = [
        msg("1", None, "user", "start"),
        entry(json!({
            "type": "branch_summary", "id": "2", "parentId": "1",
            "timestamp": "2025-01-01T00:00:01.500Z", "fromId": "x", "summary": "s",
        })),
        entry(json!({
            "type": "custom_message", "customType": "note", "content": "c", "display": false,
            "id": "3", "parentId": "2", "timestamp": "not a date",
        })),
        entry(json!({
            "type": "branch_summary", "id": "4", "parentId": "3", "timestamp": TS,
            "fromId": "y", "summary": "",
        })),
    ];
    let ctx = build_session_context(&entries, LeafSelector::Last);
    let lines: Vec<String> = ctx
        .messages
        .iter()
        .map(|message| serde_json::to_string(message).unwrap_or_default())
        .collect();
    assert_eq!(
        lines[1..],
        [
            r#"{"role":"branchSummary","summary":"s","fromId":"x","timestamp":1735689601500}"#,
            r#"{"role":"custom","customType":"note","content":"c","display":false,"timestamp":null}"#,
        ]
    );
}
