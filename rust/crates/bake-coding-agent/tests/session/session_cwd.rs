//! Pi `test/session-cwd.test.ts` (the manager-level cases; the runtime case
//! drives Pi's `createAgentSessionRuntime`).

use bake_coding_agent::session::SessionManager;
use bake_coding_agent::session::cwd::{
    MissingSessionCwdError, SessionCwdIssue, assert_session_cwd_exists,
    format_missing_session_cwd_prompt, get_missing_session_cwd_issue,
};
use serde_json::json;

use crate::support::TempDir;

fn write_session_file(path: &std::path::Path, cwd: &str) {
    let header = json!({
        "type": "session", "version": 3, "id": "session-id",
        "timestamp": "2025-01-01T00:00:00.000Z", "cwd": cwd,
    });
    std::fs::write(path, format!("{header}\n")).expect("write");
}

/// "detects missing session cwd from persisted sessions"
#[test]
fn detects_a_missing_session_cwd() {
    let fallback = TempDir::new("cwd-fallback");
    let missing = fallback.join("does-not-exist");
    let missing = missing.to_str().expect("UTF-8");
    let sessions = TempDir::new("cwd-sessions");
    let file = sessions.join("session.jsonl");
    write_session_file(&file, missing);
    let manager = SessionManager::open(&file, None, None).expect("open");
    let issue = get_missing_session_cwd_issue(&manager, fallback.str());
    assert_eq!(
        issue,
        Some(SessionCwdIssue {
            session_file: manager.session_file().map(std::path::Path::to_path_buf),
            session_cwd: missing.to_owned(),
            fallback_cwd: fallback.str().to_owned(),
        })
    );
    let error = assert_session_cwd_exists(&manager, fallback.str()).expect_err("missing");
    let MissingSessionCwdError { issue } = &error;
    assert_eq!(
        error.to_string(),
        format!(
            "Stored session working directory does not exist: {missing}\nSession file: {}\nCurrent working directory: {}",
            file.display(),
            fallback.str()
        )
    );
    assert_eq!(
        format_missing_session_cwd_prompt(issue),
        format!(
            "cwd from session file does not exist\n{missing}\n\ncontinue in current cwd\n{}",
            fallback.str()
        )
    );
}

/// "supports overriding the effective cwd when opening a session"
#[test]
fn supports_overriding_the_cwd() {
    let fallback = TempDir::new("cwd-override");
    let missing = fallback.join("does-not-exist");
    let sessions = TempDir::new("cwd-override-sessions");
    let file = sessions.join("session.jsonl");
    write_session_file(&file, missing.to_str().expect("UTF-8"));
    let manager = SessionManager::open(&file, None, Some(fallback.str())).expect("open");
    assert_eq!(manager.cwd(), fallback.str());
    assert_eq!(
        get_missing_session_cwd_issue(&manager, fallback.str()),
        None
    );
}
