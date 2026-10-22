//! Pi `test/suite/regressions/8989-fork-compaction-label-boundary.test.ts`
//! and `7497-session-discovery-symlink.test.ts`.

use bake_coding_agent::session::{NewSessionOptions, SessionManager};
use serde_json::{Value, json};

use crate::support::{member, roles, text_of, user_msg};

/// "regression #8989 > preserves compaction context when a fork removes the
/// boundary label"
#[test]
fn fork_keeps_compaction_context_past_a_removed_label() {
    let mut session = SessionManager::in_memory(None, NewSessionOptions::default(), Vec::new())
        .expect("in memory");
    let old = session.append_message(user_msg("old")).expect("append");
    let label = session
        .append_label_change(&old, Some("checkpoint"))
        .expect("label");
    let kept = session.append_message(user_msg("kept")).expect("append");
    let compaction = session
        .append_compaction("summary", Some(&label), 100, None, None, None)
        .expect("compaction");
    let leaf = session.append_message(user_msg("after")).expect("append");
    session.create_branched_session(&leaf).expect("branch");
    assert_eq!(
        session
            .entry(&compaction)
            .and_then(|entry| entry.get("firstKeptEntryId")),
        Some(&json!(kept))
    );
    let messages = session.build_session_context().messages;
    assert_eq!(roles(&messages), ["compactionSummary", "user", "user"]);
    assert_eq!(
        member(&messages[0], "summary"),
        Some(&Value::from("summary"))
    );
    assert_eq!(text_of(&messages[1]), "kept");
    assert_eq!(text_of(&messages[2]), "after");
}

#[cfg(unix)]
mod symlinks {
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::support::TempDir;

    fn write_session(dir: &std::path::Path, id: &str, cwd: &std::path::Path) {
        std::fs::create_dir_all(dir).expect("mkdir");
        let header = json!({
            "type": "session", "version": 3, "id": id,
            "timestamp": "2026-08-03T00:00:00.000Z", "cwd": cwd,
        });
        std::fs::write(dir.join(format!("{id}.jsonl")), format!("{header}\n")).expect("write");
    }

    fn ids(sessions: &[bake_coding_agent::session::SessionInfo]) -> Vec<String> {
        sessions.iter().map(|session| session.id.clone()).collect()
    }

    /// "discovers a session through a directory link and preserves the
    /// alias path"
    #[test]
    fn discovers_through_a_directory_link() {
        let temp = TempDir::new("symlink-discovery");
        let sessions_dir = temp.join("home").join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("mkdir");
        let target = temp.join("linked-sessions");
        write_session(&target, "linked", &temp.join("project"));
        let alias = sessions_dir.join("--linked--");
        symlink(&target, &alias).expect("symlink");
        let sessions = SessionManager::list_all_in(&sessions_dir, None, None).expect("list");
        assert_eq!(ids(&sessions), ["linked"]);
        assert_eq!(sessions[0].path, alias.join("linked.jsonl"));
    }

    /// "ignores a broken directory link without hiding valid sessions"
    #[test]
    fn ignores_a_broken_directory_link() {
        let temp = TempDir::new("symlink-broken");
        let sessions_dir = temp.join("home").join("sessions");
        write_session(
            &sessions_dir.join("--regular--"),
            "regular",
            &temp.join("project"),
        );
        let target = temp.join("removed-sessions");
        std::fs::create_dir_all(&target).expect("mkdir");
        symlink(&target, sessions_dir.join("--broken--")).expect("symlink");
        std::fs::remove_dir_all(&target).expect("remove");
        let sessions = SessionManager::list_all_in(&sessions_dir, None, None).expect("list");
        assert_eq!(ids(&sessions), ["regular"]);
    }

    /// "ignores links to files"
    #[test]
    fn ignores_links_to_files() {
        let temp = TempDir::new("symlink-file");
        let sessions_dir = temp.join("home").join("sessions");
        write_session(
            &sessions_dir.join("--regular--"),
            "regular",
            &temp.join("project"),
        );
        let file = temp.join("not-a-directory");
        std::fs::write(&file, "").expect("write");
        symlink(&file, sessions_dir.join("--file--")).expect("symlink");
        let sessions = SessionManager::list_all_in(&sessions_dir, None, None).expect("list");
        assert_eq!(ids(&sessions), ["regular"]);
    }
}
