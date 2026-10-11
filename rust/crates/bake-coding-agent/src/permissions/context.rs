//! The model-facing statement of a session's permission.
//!
//! Bake's TypeScript added two contributions to its runtime-context
//! snapshot, a message sent after retained history and replaced whole when
//! it changed: `sandbox:policy` (`renderPolicyContext` in
//! `packages/sandbox/sandbox-policy/src/index.ts`, order 110) and
//! `approval:policy` (`ASK_SENTENCE`/`NEVER_SENTENCE` in
//! `packages/interaction/user-approval/src/index.ts`, order 115), joined by
//! a blank line.
//!
//! Pi has no runtime-context snapshot. Its nearest place is a named
//! system-prompt section: a session records its sections in a system
//! message and later sends only the sections that changed, as a new system
//! message after the history (`diffSystemPromptSections`), so a permission
//! change appends instead of rewriting the cached prompt prefix, which is
//! what the TypeScript snapshot was for. The two sentences therefore form
//! one section, [`PERMISSIONS_SECTION`], placed after Pi's `cwd` section.
//! The TypeScript snapshot's `Runtime context (replaces any earlier
//! snapshot):` heading is not carried: Pi's section tag already says which
//! text replaces which.

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
/// sentence, in the TypeScript contributions' order.
pub fn permissions_section_text(policy: &SandboxPolicy, approval: ApprovalPolicy) -> String {
    format!(
        "{}\n\n{}",
        sandbox_policy_sentence(policy),
        approval_policy_sentence(approval)
    )
}
