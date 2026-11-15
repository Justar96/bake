//! The model-facing statement of a session's permission.
//!
//! The statement is two sentences, the sandbox policy and the approval
//! policy ([`ASK_SENTENCE`](super::ASK_SENTENCE)/[`NEVER_SENTENCE`](super::NEVER_SENTENCE)),
//! joined by a blank line, in one named system-prompt section,
//! [`PERMISSIONS_SECTION`], placed before the `environment` section
//! ([`crate::system_prompt`]). A session records its sections in a system
//! message and later sends only the sections that changed, as a new system
//! message after the history (Pi's `diffSystemPromptSections`), so a
//! permission change appends instead of rewriting the cached prompt
//! prefix.

use bake_sandbox::{SandboxMode, SandboxPolicy};

use super::approval::{ApprovalPolicy, approval_policy_sentence};

/// The system-prompt section holding the permission statement.
pub const PERMISSIONS_SECTION: &str = "permissions";

/// `renderPolicyContext`: the policy's file-effect contract, naming the
/// workspace root under `workspace-write` as `JSON.stringify` writes it.
pub fn sandbox_policy_sentence(policy: &SandboxPolicy) -> String {
    match policy.mode {
        SandboxMode::ReadOnly => "File sandbox: read-only. Sandboxed tools cannot modify files. Still attempt required changes normally, and follow the guidance any denial returns.".to_owned(),
        SandboxMode::WorkspaceWrite => {
            let root = policy.workspace_root.to_string_lossy();
            let quoted = serde_json::to_string(root.as_ref()).unwrap_or_default();
            format!(
                "File sandbox: workspace-write. Sandboxed tools can modify files under {quoted}, and possibly in some temporary directories."
            )
        }
        SandboxMode::DangerFullAccess => {
            "File sandbox: danger-full-access. File changes are not restricted.".to_owned()
        }
    }
}

/// The [`PERMISSIONS_SECTION`] text: the sandbox sentence, then the approval
/// sentence.
pub fn permissions_section_text(policy: &SandboxPolicy, approval: ApprovalPolicy) -> String {
    format!(
        "{}\n\n{}",
        sandbox_policy_sentence(policy),
        approval_policy_sentence(approval)
    )
}
