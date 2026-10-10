//! Pi `test/session-manager/tree-traversal.test.ts`.

use std::fs;

use bake_coding_agent::session::json::js_stringify;
use bake_coding_agent::session::{NewSessionOptions, SessionError, SessionManager};
use serde_json::{Value, json};

use crate::support::{
    TempDir, assistant_msg, ids, message, now, read_session_file_roles, text_of, user_msg,
};

fn memory() -> SessionManager {
    SessionManager::in_memory(None, NewSessionOptions::default(), Vec::new()).expect("in memory")
}

fn persisted(dir: &TempDir) -> SessionManager {
    SessionManager::create(dir.str(), Some(dir.path()), NewSessionOptions::default())
        .expect("create")
}

fn usage_value() -> Value {
    json!({
        "input": 10, "output": 20, "cacheRead": 30, "cacheWrite": 40, "totalTokens": 100,
        "cost": {"input": 0.1, "output": 0.2, "cacheRead": 0.3, "cacheWrite": 0.4, "total": 1},
    })
}

fn usage() -> bake_ai::Usage {
    serde_json::from_value(usage_value()).expect("usage")
}

mod append_operations {
    use super::*;

    /// "appendMessage creates entry with correct parentId chain"
    #[test]
    fn append_message_chains_parents() {
        let mut session = memory();
        let first = session.append_message(user_msg("first")).expect("append");
        let second = session
            .append_message(assistant_msg("second"))
            .expect("append");
        let third = session.append_message(user_msg("third")).expect("append");
        let entries = session.entries();
        assert_eq!(ids(&entries), [first.clone(), second.clone(), third]);
        assert_eq!(entries[0].get("parentId"), Some(&Value::Null));
        assert_eq!(entries[0].entry_type(), "message");
        assert_eq!(entries[1].parent_id(), Some(first.as_str()));
        assert_eq!(entries[2].parent_id(), Some(second.as_str()));
    }

    /// "appendThinkingLevelChange integrates into tree"
    #[test]
    fn thinking_level_change_integrates() {
        let mut session = memory();
        let message_id = session.append_message(user_msg("hello")).expect("append");
        let thinking = session
            .append_thinking_level_change("high")
            .expect("append");
        session
            .append_message(assistant_msg("response"))
            .expect("append");
        let entries = session.entries();
        let entry = entries
            .iter()
            .find(|entry| entry.entry_type() == "thinking_level_change")
            .expect("entry");
        assert_eq!(entry.id(), thinking);
        assert_eq!(entry.parent_id(), Some(message_id.as_str()));
        assert_eq!(entries[2].parent_id(), Some(thinking.as_str()));
    }

    /// "appendModelChange integrates into tree"
    #[test]
    fn model_change_integrates() {
        let mut session = memory();
        let message_id = session.append_message(user_msg("hello")).expect("append");
        let model = session
            .append_model_change("openai", "gpt-4")
            .expect("append");
        session
            .append_message(assistant_msg("response"))
            .expect("append");
        let entries = session.entries();
        let entry = entries
            .iter()
            .find(|entry| entry.entry_type() == "model_change")
            .expect("entry");
        assert_eq!(entry.id(), model);
        assert_eq!(entry.parent_id(), Some(message_id.as_str()));
        assert_eq!(entry.get("provider"), Some(&json!("openai")));
        assert_eq!(entry.get("modelId"), Some(&json!("gpt-4")));
        assert_eq!(entries[2].parent_id(), Some(model.as_str()));
    }

    /// "appendCompaction integrates into tree"
    #[test]
    fn compaction_integrates() {
        let mut session = memory();
        let first = session.append_message(user_msg("1")).expect("append");
        let second = session.append_message(assistant_msg("2")).expect("append");
        let compaction = session
            .append_compaction(
                "summary",
                Some(&first),
                1000,
                None,
                Some(false),
                Some(&usage()),
            )
            .expect("compaction");
        session.append_message(user_msg("3")).expect("append");
        let entries = session.entries();
        let entry = entries
            .iter()
            .find(|entry| entry.entry_type() == "compaction")
            .expect("entry");
        assert_eq!(entry.id(), compaction);
        assert_eq!(entry.parent_id(), Some(second.as_str()));
        assert_eq!(entry.get("summary"), Some(&json!("summary")));
        assert_eq!(entry.get("firstKeptEntryId"), Some(&json!(first)));
        assert_eq!(entry.get("tokensBefore"), Some(&json!(1000)));
        assert_eq!(
            entry.get("usage").map(js_stringify),
            Some(js_stringify(&usage_value()))
        );
        assert_eq!(entries[3].parent_id(), Some(compaction.as_str()));
    }

    /// "appendCustomEntry integrates into tree"
    #[test]
    fn custom_entry_integrates() {
        let mut session = memory();
        let message_id = session.append_message(user_msg("hello")).expect("append");
        let custom = session
            .append_custom_entry("my_data", Some(json!({"key": "value"})))
            .expect("append");
        session
            .append_message(assistant_msg("response"))
            .expect("append");
        let entries = session.entries();
        let entry = entries
            .iter()
            .find(|entry| entry.entry_type() == "custom")
            .expect("entry");
        assert_eq!(entry.id(), custom);
        assert_eq!(entry.parent_id(), Some(message_id.as_str()));
        assert_eq!(entry.get("customType"), Some(&json!("my_data")));
        assert_eq!(entry.get("data"), Some(&json!({"key": "value"})));
        assert_eq!(entries[2].parent_id(), Some(custom.as_str()));
    }

    /// "leaf pointer advances after each append"
    #[test]
    fn leaf_advances() {
        let mut session = memory();
        assert_eq!(session.leaf_id(), None);
        let first = session.append_message(user_msg("1")).expect("append");
        assert_eq!(session.leaf_id(), Some(first.as_str()));
        let second = session.append_message(assistant_msg("2")).expect("append");
        assert_eq!(session.leaf_id(), Some(second.as_str()));
        let third = session
            .append_thinking_level_change("high")
            .expect("append");
        assert_eq!(session.leaf_id(), Some(third.as_str()));
    }
}

mod get_path {
    use super::*;

    /// "returns empty array for empty session"
    #[test]
    fn empty_for_empty_session() {
        assert!(memory().branch_entries(None).is_empty());
    }

    /// "returns single entry path"
    #[test]
    fn single_entry_path() {
        let mut session = memory();
        let id = session.append_message(user_msg("hello")).expect("append");
        assert_eq!(ids(&session.branch_entries(None)), [id]);
    }

    /// "returns full path from root to leaf"
    #[test]
    fn full_path_from_root_to_leaf() {
        let mut session = memory();
        let a = session.append_message(user_msg("1")).expect("append");
        let b = session.append_message(assistant_msg("2")).expect("append");
        let c = session
            .append_thinking_level_change("high")
            .expect("append");
        let d = session.append_message(user_msg("3")).expect("append");
        assert_eq!(ids(&session.branch_entries(None)), [a, b, c, d]);
    }

    /// "returns path from specified entry to root"
    #[test]
    fn path_from_specified_entry() {
        let mut session = memory();
        let a = session.append_message(user_msg("1")).expect("append");
        let b = session.append_message(assistant_msg("2")).expect("append");
        session.append_message(user_msg("3")).expect("append");
        session.append_message(assistant_msg("4")).expect("append");
        assert_eq!(ids(&session.branch_entries(Some(&b))), [a, b.clone()]);
    }
}

mod get_tree {
    use super::*;

    /// "returns empty array for empty session"
    #[test]
    fn empty_for_empty_session() {
        assert!(memory().tree().roots.is_empty());
    }

    /// "returns single root for linear session"
    #[test]
    fn single_root_for_linear_session() {
        let mut session = memory();
        let a = session.append_message(user_msg("1")).expect("append");
        let b = session.append_message(assistant_msg("2")).expect("append");
        let c = session.append_message(user_msg("3")).expect("append");
        let tree = session.tree();
        assert_eq!(tree.roots.len(), 1);
        let root = tree.root_nodes().next().expect("root");
        assert_eq!(root.entry.id(), a);
        let children: Vec<_> = tree.children(root).collect();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].entry.id(), b);
        let grandchildren: Vec<_> = tree.children(children[0]).collect();
        assert_eq!(grandchildren.len(), 1);
        assert_eq!(grandchildren[0].entry.id(), c);
        assert!(grandchildren[0].children.is_empty());
    }

    /// "returns tree with branches after branch"
    #[test]
    fn branches_after_branch() {
        let mut session = memory();
        let a = session.append_message(user_msg("1")).expect("append");
        let b = session.append_message(assistant_msg("2")).expect("append");
        let c = session.append_message(user_msg("3")).expect("append");
        session.branch(&b).expect("branch");
        let d = session
            .append_message(user_msg("4-branch"))
            .expect("append");
        let tree = session.tree();
        assert_eq!(tree.roots.len(), 1);
        let root = tree.root_nodes().next().expect("root");
        assert_eq!(root.entry.id(), a);
        let node_b = tree.children(root).next().expect("b");
        assert_eq!(node_b.entry.id(), b);
        let mut children: Vec<String> = tree
            .children(node_b)
            .map(|node| node.entry.id().to_owned())
            .collect();
        children.sort();
        let mut expected = vec![c, d];
        expected.sort();
        assert_eq!(children, expected);
    }

    /// "handles multiple branches at same point"
    #[test]
    fn multiple_branches_at_same_point() {
        let mut session = memory();
        session.append_message(user_msg("root")).expect("append");
        let b = session
            .append_message(assistant_msg("response"))
            .expect("append");
        let mut branches = Vec::new();
        for name in ["branch-A", "branch-B", "branch-C"] {
            session.branch(&b).expect("branch");
            branches.push(session.append_message(user_msg(name)).expect("append"));
        }
        let tree = session.tree();
        let root = tree.root_nodes().next().expect("root");
        let node_b = tree.children(root).next().expect("b");
        assert_eq!(node_b.entry.id(), b);
        let mut children: Vec<String> = tree
            .children(node_b)
            .map(|node| node.entry.id().to_owned())
            .collect();
        children.sort();
        branches.sort();
        assert_eq!(children, branches);
    }

    /// "handles deep branching"
    #[test]
    fn deep_branching() {
        let mut session = memory();
        session.append_message(user_msg("1")).expect("append");
        let b = session.append_message(assistant_msg("2")).expect("append");
        let c = session.append_message(user_msg("3")).expect("append");
        session.append_message(assistant_msg("4")).expect("append");
        session.branch(&b).expect("branch");
        let e = session.append_message(user_msg("5")).expect("append");
        session.append_message(assistant_msg("6")).expect("append");
        session.branch(&e).expect("branch");
        session.append_message(user_msg("7")).expect("append");
        let tree = session.tree();
        let root = tree.root_nodes().next().expect("root");
        let node_b = tree.children(root).next().expect("b");
        assert_eq!(node_b.children.len(), 2);
        let node_e = tree
            .children(node_b)
            .find(|node| node.entry.id() == e)
            .expect("e");
        assert_eq!(node_e.children.len(), 2);
        let node_c = tree
            .children(node_b)
            .find(|node| node.entry.id() == c)
            .expect("c");
        assert_eq!(node_c.children.len(), 1);
    }

    /// Children are ordered by timestamp, oldest first, whatever the file
    /// order; Pi's `getTree` sorts them the same way.
    #[test]
    fn children_are_ordered_by_timestamp() {
        let entries: Vec<_> = [
            json!({"type": "message", "id": "r", "parentId": null, "timestamp": "2025-01-01T00:00:00.000Z", "message": {"role": "user", "content": "r", "timestamp": 1}}),
            json!({"type": "message", "id": "late", "parentId": "r", "timestamp": "2025-01-01T00:00:03.000Z", "message": {"role": "user", "content": "x", "timestamp": 1}}),
            json!({"type": "message", "id": "early", "parentId": "r", "timestamp": "2025-01-01T00:00:01.000Z", "message": {"role": "user", "content": "x", "timestamp": 1}}),
            json!({"type": "message", "id": "self", "parentId": "self", "timestamp": "2025-01-01T00:00:02.000Z", "message": {"role": "user", "content": "x", "timestamp": 1}}),
        ]
        .into_iter()
        .map(|value| bake_coding_agent::session::FileEntry::from_json(crate::support::object(value)))
        .collect();
        let session =
            SessionManager::in_memory(None, NewSessionOptions::default(), entries).expect("load");
        let tree = session.tree();
        let roots: Vec<&str> = tree.root_nodes().map(|node| node.entry.id()).collect();
        assert_eq!(roots, ["r", "self"]);
        let root = tree.root_nodes().next().expect("root");
        let children: Vec<&str> = tree.children(root).map(|node| node.entry.id()).collect();
        assert_eq!(children, ["early", "late"]);
    }
}

mod branch {
    use super::*;

    /// "moves leaf pointer to specified entry"
    #[test]
    fn moves_the_leaf() {
        let mut session = memory();
        let a = session.append_message(user_msg("1")).expect("append");
        session.append_message(assistant_msg("2")).expect("append");
        let c = session.append_message(user_msg("3")).expect("append");
        assert_eq!(session.leaf_id(), Some(c.as_str()));
        session.branch(&a).expect("branch");
        assert_eq!(session.leaf_id(), Some(a.as_str()));
    }

    /// "throws for non-existent entry"
    #[test]
    fn rejects_missing_entries() {
        let mut session = memory();
        session.append_message(user_msg("hello")).expect("append");
        let error = session.branch("nonexistent").expect_err("missing");
        assert_eq!(error.to_string(), "Entry nonexistent not found");
    }

    /// "new appends become children of branch point"
    #[test]
    fn appends_become_children_of_the_branch_point() {
        let mut session = memory();
        let a = session.append_message(user_msg("1")).expect("append");
        session.append_message(assistant_msg("2")).expect("append");
        session.branch(&a).expect("branch");
        let c = session
            .append_message(user_msg("branched"))
            .expect("append");
        assert_eq!(
            session.entry(&c).and_then(|entry| entry.parent_id()),
            Some(a.as_str())
        );
    }

    /// `resetLeaf()`: the next append is a new root, and the context is
    /// empty until then.
    #[test]
    fn reset_leaf_starts_a_new_root() {
        let mut session = memory();
        session.append_message(user_msg("1")).expect("append");
        session.reset_leaf();
        assert!(session.build_session_context().messages.is_empty());
        let root = session.append_message(user_msg("again")).expect("append");
        assert_eq!(
            session.entry(&root).and_then(|entry| entry.get("parentId")),
            Some(&Value::Null)
        );
        assert_eq!(session.tree().roots.len(), 2);
    }
}

mod branch_with_summary {
    use super::*;

    /// "inserts branch summary with the source and destination and advances
    /// leaf"
    #[test]
    fn inserts_a_branch_summary() {
        let mut session = memory();
        let a = session.append_message(user_msg("1")).expect("append");
        session.append_message(assistant_msg("2")).expect("append");
        let c = session.append_message(user_msg("3")).expect("append");
        let summary = session
            .branch_with_summary(
                Some(&a),
                "Summary of abandoned work",
                None,
                Some(false),
                Some(&usage()),
            )
            .expect("summary");
        assert_eq!(session.leaf_id(), Some(summary.as_str()));
        let entries = session.entries();
        let entry = entries
            .iter()
            .find(|entry| entry.entry_type() == "branch_summary")
            .expect("entry");
        assert_eq!(entry.parent_id(), Some(a.as_str()));
        assert_eq!(entry.get("fromId"), Some(&json!(c)));
        assert_eq!(
            entry.get("summary"),
            Some(&json!("Summary of abandoned work"))
        );
        assert_eq!(
            entry.get("usage").map(js_stringify),
            Some(js_stringify(&usage_value()))
        );
    }

    /// "throws for non-existent entry"
    #[test]
    fn rejects_missing_entries() {
        let mut session = memory();
        session.append_message(user_msg("hello")).expect("append");
        let error = session
            .branch_with_summary(Some("nonexistent"), "summary", None, None, None)
            .expect_err("missing");
        assert!(matches!(error, SessionError::EntryNotFound(_)));
        assert_eq!(error.to_string(), "Entry nonexistent not found");
    }
}

mod get_leaf_entry_and_entry {
    use super::*;

    /// "getLeafEntry > returns undefined for empty session"
    #[test]
    fn no_leaf_for_empty_session() {
        assert!(memory().leaf_entry().is_none());
    }

    /// "getLeafEntry > returns current leaf entry"
    #[test]
    fn returns_current_leaf_entry() {
        let mut session = memory();
        session.append_message(user_msg("1")).expect("append");
        let b = session.append_message(assistant_msg("2")).expect("append");
        assert_eq!(
            session.leaf_entry().map(|entry| entry.id()),
            Some(b.as_str())
        );
    }

    /// "getEntry > returns undefined for non-existent id"
    #[test]
    fn no_entry_for_missing_id() {
        assert!(memory().entry("nonexistent").is_none());
    }

    /// "getEntry > returns entry by id"
    #[test]
    fn returns_entry_by_id() {
        let mut session = memory();
        let a = session.append_message(user_msg("first")).expect("append");
        let b = session
            .append_message(assistant_msg("second"))
            .expect("append");
        let first = session
            .entry(&a)
            .and_then(|entry| entry.message())
            .expect("first");
        assert_eq!(text_of(&first), "first");
        let second = session
            .entry(&b)
            .and_then(|entry| entry.message())
            .expect("second");
        assert_eq!(text_of(&second), "second");
    }

    /// `getChildren()`
    #[test]
    fn children_of_an_entry() {
        let mut session = memory();
        let a = session.append_message(user_msg("1")).expect("append");
        let b = session.append_message(user_msg("2")).expect("append");
        session.branch(&a).expect("branch");
        let c = session.append_message(user_msg("3")).expect("append");
        assert_eq!(ids(&session.children(&a)), [b, c]);
    }
}

/// "buildSessionContext with branches > returns messages from current
/// branch only"
#[test]
fn context_follows_the_current_branch() {
    let mut session = memory();
    session.append_message(user_msg("msg1")).expect("append");
    let b = session
        .append_message(assistant_msg("msg2"))
        .expect("append");
    session.append_message(user_msg("msg3")).expect("append");
    session.branch(&b).expect("branch");
    session
        .append_message(assistant_msg("msg4-branch"))
        .expect("append");
    let texts: Vec<String> = session
        .build_session_context()
        .messages
        .iter()
        .map(text_of)
        .collect();
    assert_eq!(texts, ["msg1", "msg2", "msg4-branch"]);
}

mod create_branched_session {
    use super::*;

    /// "throws for non-existent entry"
    #[test]
    fn rejects_missing_entries() {
        let mut session = memory();
        session.append_message(user_msg("hello")).expect("append");
        let error = session
            .create_branched_session("nonexistent")
            .expect_err("missing");
        assert_eq!(error.to_string(), "Entry nonexistent not found");
    }

    /// "creates new session with path to specified leaf (in-memory)"
    #[test]
    fn keeps_the_path_in_memory() {
        let mut session = memory();
        let a = session.append_message(user_msg("1")).expect("append");
        let b = session.append_message(assistant_msg("2")).expect("append");
        let c = session.append_message(user_msg("3")).expect("append");
        session.append_message(assistant_msg("4")).expect("append");
        session.branch(&c).expect("branch");
        session.append_message(user_msg("5")).expect("append");
        assert_eq!(session.create_branched_session(&b).expect("branch"), None);
        assert_eq!(ids(&session.entries()), [a, b]);
    }

    /// "extracts correct path from branched tree"
    #[test]
    fn extracts_the_branched_path() {
        let mut session = memory();
        let a = session.append_message(user_msg("1")).expect("append");
        let b = session.append_message(assistant_msg("2")).expect("append");
        session.append_message(user_msg("3")).expect("append");
        session.branch(&b).expect("branch");
        let d = session.append_message(user_msg("4")).expect("append");
        let e = session.append_message(assistant_msg("5")).expect("append");
        session.create_branched_session(&e).expect("branch");
        assert_eq!(ids(&session.entries()), [a, b, d, e]);
    }

    /// "does not duplicate entries when forking from before the first user
    /// message"
    #[test]
    fn does_not_duplicate_entries() {
        let dir = TempDir::new("fork-dedup");
        let mut session = persisted(&dir);
        let model_change = session
            .append_model_change("anthropic", "claude-sonnet-4-5")
            .expect("append");
        session
            .append_message(user_msg("first question"))
            .expect("append");
        session
            .append_message(assistant_msg("first answer"))
            .expect("append");
        let new_file = session
            .create_branched_session(&model_change)
            .expect("branch")
            .expect("file");
        assert!(!new_file.exists());
        session
            .append_message(user_msg("new question"))
            .expect("append");
        assert!(new_file.exists());
        session
            .append_custom_entry("preset-state", Some(json!({"name": "plan"})))
            .expect("append");
        session
            .append_message(assistant_msg("new answer"))
            .expect("append");
        assert_eq!(
            read_session_file_roles(&new_file),
            ["session", "model_change", "user", "custom", "assistant"]
        );
    }

    /// "preserves tool and summary usage across a file-backed reload"
    #[test]
    fn preserves_usage_across_reload() {
        let dir = TempDir::new("usage-roundtrip");
        let mut session = persisted(&dir);
        let root = session
            .append_message(user_msg("question"))
            .expect("append");
        session
            .append_message(assistant_msg("answer"))
            .expect("append");
        session
            .append_message(message(json!({
                "role": "toolResult", "toolCallId": "call-1", "toolName": "nested-model",
                "content": [{"type": "text", "text": "result"}], "isError": false,
                "usage": usage_value(), "timestamp": now(),
            })))
            .expect("append");
        session
            .append_compaction(
                "summary",
                Some(&root),
                100,
                None,
                Some(false),
                Some(&usage()),
            )
            .expect("compaction");
        session
            .branch_with_summary(
                Some(&root),
                "branch summary",
                None,
                Some(false),
                Some(&usage()),
            )
            .expect("summary");
        let file = session.session_file().expect("file").to_path_buf();
        let reopened = SessionManager::open(&file, Some(dir.path()), None).expect("open");
        let entries = reopened.entries();
        let usage_of = |kind: &str| {
            entries
                .iter()
                .find(|entry| entry.entry_type() == kind)
                .and_then(|entry| entry.get("usage").cloned())
        };
        let expected = Some(js_stringify(&usage_value()));
        assert_eq!(usage_of("compaction").as_ref().map(js_stringify), expected);
        assert_eq!(
            usage_of("branch_summary").as_ref().map(js_stringify),
            expected
        );
        let tool = entries
            .iter()
            .find(|entry| entry.message_role() == Some("toolResult"))
            .and_then(|entry| entry.message())
            .expect("tool result");
        assert_eq!(tool.as_json().get("usage"), Some(&usage_value()));
        let bytes = fs::read_to_string(&file).expect("read");
        assert!(bytes.contains(
            r#""cost":{"input":0.1,"output":0.2,"cacheRead":0.3,"cacheWrite":0.4,"total":1}"#
        ));
    }

    /// "writes file immediately when forking at a user message"
    #[test]
    fn writes_immediately_at_a_user_message() {
        let dir = TempDir::new("fork-user");
        let mut session = persisted(&dir);
        let a = session
            .append_message(user_msg("first question"))
            .expect("append");
        session
            .append_message(assistant_msg("first answer"))
            .expect("append");
        let previous = session.session_file().expect("file").to_path_buf();
        let new_file = session
            .create_branched_session(&a)
            .expect("branch")
            .expect("file");
        assert!(new_file.exists());
        assert_eq!(
            session.header().and_then(|header| header.parent_session()),
            previous.to_str()
        );
        session
            .append_message(assistant_msg("new answer"))
            .expect("append");
        assert_eq!(
            read_session_file_roles(&new_file),
            ["session", "user", "assistant"]
        );
    }
}
