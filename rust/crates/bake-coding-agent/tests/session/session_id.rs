//! Pi `test/session-id-readonly.test.ts`: the cases that call
//! `SessionManager.findById` directly (the others drive Pi's
//! `createSessionManager` in `main.ts`).

use bake_coding_agent::session::{NewSessionOptions, SessionManager};

use crate::support::{TempDir, assistant_msg, user_msg};

fn persist_session(session: &mut SessionManager, content: &str) {
    session.append_message(user_msg(content)).expect("append");
    session
        .append_message(assistant_msg("persisted"))
        .expect("append");
}

/// "filters exact IDs by cwd in a custom session directory"
#[test]
fn filters_exact_ids_by_cwd() {
    let root = TempDir::new("find-by-id");
    let project_a = root.join("project-a");
    let project_b = root.join("project-b");
    let sessions = root.join("sessions");
    std::fs::create_dir_all(&project_a).expect("mkdir");
    std::fs::create_dir_all(&project_b).expect("mkdir");
    let mut foreign = SessionManager::create(
        project_b.to_str().expect("UTF-8"),
        Some(&sessions),
        NewSessionOptions::with_id("foreign-id"),
    )
    .expect("create");
    persist_session(&mut foreign, "foreign session");
    assert_eq!(
        SessionManager::find_by_id(
            project_a.to_str().expect("UTF-8"),
            "foreign-id",
            Some(&sessions)
        )
        .expect("find"),
        None
    );
    assert_eq!(
        SessionManager::find_by_id(
            project_b.to_str().expect("UTF-8"),
            "foreign-id",
            Some(&sessions)
        )
        .expect("find")
        .as_deref(),
        foreign.session_file()
    );
}

/// "reopens an exact ID from a renamed session file", at the manager level:
/// the id is read from the header, not the file name.
#[test]
fn finds_a_renamed_session_file() {
    let root = TempDir::new("find-renamed");
    let project = root.join("project");
    let sessions = root.join("sessions");
    let project = project.to_str().expect("UTF-8");
    let mut original = SessionManager::create(
        project,
        Some(&sessions),
        NewSessionOptions::with_id("renamed-id"),
    )
    .expect("create");
    persist_session(&mut original, "persist me");
    let renamed = sessions.join("imported-session.jsonl");
    std::fs::rename(original.session_file().expect("file"), &renamed).expect("rename");
    assert_eq!(
        SessionManager::find_by_id(project, "renamed-id", Some(&sessions)).expect("find"),
        Some(renamed)
    );
}
