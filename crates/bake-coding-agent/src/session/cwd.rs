//! A resumed session whose working directory is gone.
//!
//! Ported from Pi `packages/coding-agent/src/core/session-cwd.ts` (v1.1.0).

use std::fmt;
use std::path::{Path, PathBuf};

use crate::session::SessionManager;

/// A persisted session whose stored working directory does not exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCwdIssue {
    /// The session file.
    pub session_file: Option<PathBuf>,
    /// The stored working directory.
    pub session_cwd: String,
    /// Where the caller would continue instead.
    pub fallback_cwd: String,
}

/// Pi's `getMissingSessionCwdIssue`: an issue when the manager has a session
/// file and a non-empty working directory that does not exist.
pub fn get_missing_session_cwd_issue(
    manager: &SessionManager,
    fallback_cwd: &str,
) -> Option<SessionCwdIssue> {
    let session_file = manager.session_file()?;
    let session_cwd = manager.cwd();
    if session_cwd.is_empty() || Path::new(session_cwd).exists() {
        return None;
    }
    Some(SessionCwdIssue {
        session_file: Some(session_file.to_path_buf()),
        session_cwd: session_cwd.to_owned(),
        fallback_cwd: fallback_cwd.to_owned(),
    })
}

/// Pi's `formatMissingSessionCwdError`.
pub fn format_missing_session_cwd_error(issue: &SessionCwdIssue) -> String {
    let session_file = issue
        .session_file
        .as_ref()
        .map(|file| format!("\nSession file: {}", file.display()))
        .unwrap_or_default();
    format!(
        "Stored session working directory does not exist: {}{session_file}\nCurrent working directory: {}",
        issue.session_cwd, issue.fallback_cwd
    )
}

/// Pi's `formatMissingSessionCwdPrompt`.
pub fn format_missing_session_cwd_prompt(issue: &SessionCwdIssue) -> String {
    format!(
        "cwd from session file does not exist\n{}\n\ncontinue in current cwd\n{}",
        issue.session_cwd, issue.fallback_cwd
    )
}

/// Pi's `MissingSessionCwdError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingSessionCwdError {
    /// The issue.
    pub issue: SessionCwdIssue,
}

impl fmt::Display for MissingSessionCwdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&format_missing_session_cwd_error(&self.issue))
    }
}

impl std::error::Error for MissingSessionCwdError {}

/// Pi's `assertSessionCwdExists`.
pub fn assert_session_cwd_exists(
    manager: &SessionManager,
    fallback_cwd: &str,
) -> Result<(), MissingSessionCwdError> {
    match get_missing_session_cwd_issue(manager, fallback_cwd) {
        Some(issue) => Err(MissingSessionCwdError { issue }),
        None => Ok(()),
    }
}
