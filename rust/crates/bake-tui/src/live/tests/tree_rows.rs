//! The `/tree` and `/fork` sheet rows, adapted from Pi v1.2.0
//! `test/tree-selector.test.ts` (active branch first, default filter,
//! nearest visible current row, connectors) with Bake's bounds for
//! malformed and deep trees.

use super::*;
use bake_coding_agent::agent_session::ForkPoint;
use bake_coding_agent::session::{AgentMessage, NewSessionOptions, SessionEntry, SessionManager};
use serde_json::json;

fn message(value: Value) -> AgentMessage {
    let Value::Object(object) = value else {
        panic!("an object");
    };
    AgentMessage::from_json(object)
}

fn user(text: &str) -> AgentMessage {
    message(json!({"role": "user", "content": text, "timestamp": 1}))
}

fn assistant(text: &str) -> AgentMessage {
    message(json!({
        "role": "assistant",
        "content": [{"type": "text", "text": text}],
        "api": "anthropic-messages", "provider": "anthropic", "model": "test",
        "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2,
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
        "stopReason": "stop", "timestamp": 1,
    }))
}

fn tool_call_only(id: &str, path: &str) -> AgentMessage {
    message(json!({
        "role": "assistant",
        "content": [{"type": "toolCall", "id": id, "name": "read", "arguments": {"path": path}}],
        "api": "anthropic-messages", "provider": "anthropic", "model": "test",
        "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2,
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
        "stopReason": "toolUse", "timestamp": 1,
    }))
}

fn tool_result(id: &str) -> AgentMessage {
    message(json!({
        "role": "toolResult", "toolCallId": id, "toolName": "read",
        "content": [{"type": "text", "text": "contents"}], "isError": false, "timestamp": 1,
    }))
}

fn memory() -> SessionManager {
    SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
        .expect("session")
}

fn labels(rows: &TreeRows) -> Vec<&str> {
    rows.items.iter().map(|item| item.label.as_str()).collect()
}

fn rows_of(session: &SessionManager) -> TreeRows {
    tree_rows(&session.tree(), session.leaf_id())
}

#[test]
fn a_linear_session_lists_every_message_on_the_current_path() {
    let mut session = memory();
    session
        .append_model_change("anthropic", "test")
        .expect("model");
    session.append_thinking_level_change("off").expect("level");
    session.append_message(user("first\nline")).expect("u1");
    session.append_message(assistant("one")).expect("a1");
    session.append_message(user("second")).expect("u2");
    let leaf = session.append_message(assistant("two")).expect("a2");
    let rows = rows_of(&session);
    // Bookkeeping entries are hidden by Pi's default filter.
    assert_eq!(
        labels(&rows),
        [
            "• user: first line",
            "• assistant: one",
            "• user: second",
            "• assistant: two"
        ]
    );
    assert_eq!(rows.current, Some(3));
    assert_eq!(rows.selected, None);
    assert_eq!(rows.items[3].value, leaf);
    assert!(
        rows.items[0]
            .about
            .as_deref()
            .unwrap()
            .starts_with("User message")
    );
}

#[test]
fn the_branch_holding_the_leaf_comes_first_with_connectors() {
    let mut session = memory();
    session.append_message(user("root")).expect("u1");
    let a1 = session.append_message(assistant("reply")).expect("a1");
    session.append_message(user("old branch")).expect("u2");
    session.append_message(assistant("old answer")).expect("a2");
    session.branch(&a1).expect("branch");
    session.append_message(user("new branch")).expect("u3");
    let rows = rows_of(&session);
    assert_eq!(
        labels(&rows),
        [
            "• user: root",
            "• assistant: reply",
            "├─ • user: new branch",
            "└─ user: old branch",
            // Pi indents the first generation after a branch once more.
            "      assistant: old answer",
        ]
    );
    assert_eq!(rows.current, Some(2));
}

#[test]
fn tool_only_replies_hide_and_tool_results_name_their_call() {
    let mut session = memory();
    session.append_message(user("read it")).expect("u1");
    session
        .append_message(tool_call_only("call-1", "src/lib.rs"))
        .expect("a1");
    session.append_message(tool_result("call-1")).expect("tr");
    session.append_message(assistant("done")).expect("a2");
    let rows = rows_of(&session);
    assert_eq!(
        labels(&rows),
        [
            "• user: read it",
            "• [read: src/lib.rs]",
            "• assistant: done"
        ]
    );
}

#[test]
fn a_hidden_leaf_marks_its_nearest_shown_ancestor() {
    let mut session = memory();
    session.append_message(user("hello")).expect("u1");
    let a1 = session.append_message(assistant("hi")).expect("a1");
    session.append_thinking_level_change("high").expect("level");
    let rows = rows_of(&session);
    assert_eq!(rows.items.len(), 2);
    assert_eq!(rows.current, Some(1));
    assert_eq!(rows.items[1].value, a1);
}

#[test]
fn a_reset_leaf_selects_the_last_row_with_none_current() {
    let mut session = memory();
    session.append_message(user("hello")).expect("u1");
    session.append_message(assistant("hi")).expect("a1");
    session.reset_leaf();
    let rows = rows_of(&session);
    assert_eq!(rows.current, None);
    assert_eq!(rows.selected, Some(1));
    assert!(labels(&rows).iter().all(|label| !label.contains('•')));
}

fn raw_entry(id: &str, parent: Option<&str>, text: &str) -> SessionEntry {
    let Value::Object(object) = json!({
        "type": "message", "id": id, "parentId": parent, "timestamp": "2026-01-01T00:00:00.000Z",
        "message": {"role": "user", "content": text, "timestamp": 1},
    }) else {
        unreachable!()
    };
    SessionEntry::from_json(object).expect("entry")
}

fn tree_of(entries: Vec<SessionEntry>) -> SessionTree {
    let ids: Vec<String> = entries.iter().map(|e| e.id().to_owned()).collect();
    let mut nodes: Vec<SessionTreeNode> = entries
        .into_iter()
        .map(|entry| SessionTreeNode {
            entry,
            children: Vec::new(),
            label: None,
            label_timestamp: None,
        })
        .collect();
    let mut roots = Vec::new();
    for index in 0..nodes.len() {
        let parent = nodes[index]
            .entry
            .parent_id()
            .and_then(|parent| ids.iter().position(|id| id == parent));
        match parent {
            Some(parent) if parent != index => nodes[parent].children.push(index),
            _ => roots.push(index),
        }
    }
    SessionTree { nodes, roots }
}

#[test]
fn a_parent_cycle_lists_each_entry_once_without_looping() {
    // a <-> b form a cycle no root reaches; c is an ordinary root.
    let tree = tree_of(vec![
        raw_entry("a", Some("b"), "first"),
        raw_entry("b", Some("a"), "second"),
        raw_entry("c", None, "third"),
    ]);
    let rows = tree_rows(&tree, Some("a"));
    let mut values: Vec<&str> = rows.items.iter().map(|i| i.value.as_str()).collect();
    values.sort_unstable();
    assert_eq!(values, ["a", "b", "c"]);
    assert!(rows.current.is_some());
}

#[test]
fn a_deep_chain_and_deep_branching_stay_bounded() {
    // A long single chain: iterative, no stack overflow, no indentation.
    let mut entries = Vec::new();
    for index in 0..20_000 {
        let parent = (index > 0).then(|| format!("e{}", index - 1));
        entries.push(raw_entry(&format!("e{index}"), parent.as_deref(), "x"));
    }
    let rows = tree_rows(&tree_of(entries), Some("e19999"));
    assert_eq!(rows.items.len(), 20_000);
    assert_eq!(rows.current, Some(19_999));
    assert!(
        rows.items
            .iter()
            .all(|item| item.label.starts_with("• user"))
    );

    // Every node branches: indentation stops growing and prefixes are cut.
    let mut entries = Vec::new();
    for index in 0..300 {
        let parent = (index > 0).then(|| format!("m{}", index - 1));
        entries.push(raw_entry(&format!("m{index}"), parent.as_deref(), "main"));
        entries.push(raw_entry(&format!("s{index}"), parent.as_deref(), "side"));
    }
    let rows = tree_rows(&tree_of(entries), Some("m299"));
    assert_eq!(rows.items.len(), 600);
    for item in &rows.items {
        let prefix: String = item
            .label
            .chars()
            .take_while(|c| *c != 'u' && *c != '•')
            .collect();
        assert!(prefix.chars().count() <= MAX_PREFIX, "{prefix:?}");
    }
}

#[test]
fn fork_rows_list_user_messages_oldest_first() {
    let points = vec![
        ForkPoint {
            entry_id: "a".into(),
            text: "first\nmessage".into(),
        },
        ForkPoint {
            entry_id: "b".into(),
            text: "second".into(),
        },
    ];
    let rows = fork_rows(&points);
    assert_eq!(rows[0].label, "first message");
    assert_eq!(rows[1].detail.as_deref(), Some("Message 2 of 2"));
    assert_eq!(rows[1].value, "b");
}

// Bake: untrusted `read` arguments at the integer extremes are shown
// without overflow; Pi's `start + limit - 1` and its dropped end of 0.
#[test]
fn read_ranges_at_the_extremes_do_not_overflow() {
    let read = |args: Value| format_tool_call("read", &args);
    assert_eq!(
        read(json!({"path": "a", "offset": 3, "limit": 2})),
        "[read: a:3-4]"
    );
    assert_eq!(read(json!({"path": "a", "limit": 5})), "[read: a:1-5]");
    assert_eq!(
        read(json!({"path": "a", "offset": 1, "limit": 0})),
        "[read: a:1]"
    );
    assert_eq!(
        read(json!({"path": "a", "offset": i64::MAX, "limit": i64::MAX})),
        format!("[read: a:{}-{}]", i64::MAX, i128::from(i64::MAX) * 2 - 1)
    );
    assert_eq!(
        read(json!({"path": "a", "offset": i64::MIN, "limit": i64::MIN})),
        format!("[read: a:{}-{}]", i64::MIN, i128::from(i64::MIN) * 2 - 1)
    );
}

// Bake: a long hidden chain under many shown siblings resolves each
// node's shown ancestor once, and out-of-range child or root indexes in a
// hand-built tree are ignored.
#[test]
fn hidden_chains_resolve_once_and_bad_indexes_are_ignored() {
    let hidden = |id: &str, parent: Option<&str>| {
        let Value::Object(object) = json!({
            "type": "model_change", "id": id, "parentId": parent,
            "timestamp": "2026-01-01T00:00:00.000Z", "provider": "p", "modelId": "m",
        }) else {
            unreachable!()
        };
        SessionEntry::from_json(object).expect("entry")
    };
    let mut entries = vec![raw_entry("top", None, "top")];
    for index in 0..20_000 {
        let parent = if index == 0 {
            "top".to_owned()
        } else {
            format!("h{}", index - 1)
        };
        entries.push(hidden(&format!("h{index}"), Some(&parent)));
    }
    for index in 0..20_000 {
        entries.push(raw_entry(&format!("v{index}"), Some("h19999"), "leaf"));
    }
    let rows = tree_rows(&tree_of(entries), Some("v0"));
    assert_eq!(rows.items.len(), 20_001);
    assert_eq!(rows.current, Some(1));

    let mut tree = tree_of(vec![raw_entry("a", None, "a")]);
    tree.roots.push(99);
    tree.nodes[0].children.push(42);
    let rows = tree_rows(&tree, Some("a"));
    assert_eq!(rows.items.len(), 1);
}
