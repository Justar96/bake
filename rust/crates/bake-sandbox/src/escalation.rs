//! The model-visible escalation vocabulary, from
//! `packages/sandbox/sandbox/src/escalation.ts` and the runner-failure notice
//! of `packages/shell/tool-bash/src/render.ts`. Every string here reaches the
//! model and is part of the retained D24 surface: keep it byte for byte.
//!
//! The approval itself (asking someone) lives above this crate; this module
//! decides what a request means and words each outcome.

use crate::mode::SandboxMode;

/// Every mode a call could escalate to (`ESCALATION_TARGETS`); `read-only` is
/// the floor and nothing escalates to it. Tools advertise the whole set
/// whenever they confine, since a session's effective mode may sit below the
/// composition default.
pub const ESCALATION_TARGETS: [SandboxMode; 2] =
    [SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess];

/// What an escalation request asks for, decided against the call's
/// effective mode before anyone is asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscalationCheck {
    /// The call already has the requested access (`isNoOpEscalation`): run
    /// under the effective mode, with no approval and no argument checks.
    NoOp,
    /// A strictly wider mode that needs approval.
    Widen(SandboxMode),
    /// Not a strict widening; the text is the tool error.
    Refused(String),
}

/// Classifies `requested` against `effective` (the first half of
/// `approveEscalation`, with `isNoOpEscalation`).
pub fn check_escalation(effective: SandboxMode, requested: &str) -> EscalationCheck {
    match requested.parse::<SandboxMode>() {
        Ok(mode) if effective.is_widened_by(mode) => EscalationCheck::Widen(mode),
        Ok(_) => EscalationCheck::NoOp,
        Err(_) => EscalationCheck::Refused(format!(
            "sandbox escalation to \"{requested}\" is not strictly wider than this call's current \"{effective}\" mode"
        )),
    }
}

/// `validateEscalationArgs`: `sandbox_permissions` and `justification` come
/// together, and the justification is not blank.
pub fn validate_escalation_args(
    sandbox_permissions: Option<&str>,
    justification: Option<&str>,
) -> Result<(), String> {
    match (sandbox_permissions, justification) {
        (Some(_), None) => {
            Err("invalid escalation: sandbox_permissions requires a justification".to_owned())
        }
        (None, Some(_)) => Err(
            "invalid escalation: justification is only valid together with sandbox_permissions"
                .to_owned(),
        ),
        (_, Some(text)) if is_js_blank(text) => {
            Err("invalid justification: expected a non-empty sentence".to_owned())
        }
        _ => Ok(()),
    }
}

/// JavaScript's `trim().length === 0`: blank under ECMAScript white space
/// and line terminators, which include U+FEFF.
fn is_js_blank(text: &str) -> bool {
    text.chars().all(|c| c.is_whitespace() || c == '\u{feff}')
}

/// The reason recorded with an approval request (`escalate sandbox to …`).
pub fn escalation_reason(mode: SandboxMode, justification: &str) -> String {
    format!("escalate sandbox to {mode}: {justification}")
}

/// The answer to an approval request (`EscalationOutcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EscalationOutcome {
    /// Granted for this one call.
    AllowedOnce,
    /// Refused by the user, or by the `never` policy.
    Rejected,
    /// The request was cancelled before an answer.
    Cancelled,
    /// No one can answer; fails closed.
    Unavailable,
}

impl EscalationOutcome {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            EscalationOutcome::AllowedOnce => "allowed-once",
            EscalationOutcome::Rejected => "rejected",
            EscalationOutcome::Cancelled => "cancelled",
            EscalationOutcome::Unavailable => "unavailable",
        }
    }

    /// The tool error for a refused escalation of a `subject` (`command` for
    /// bash, `operation` for the file tools) to `mode`; `None` when allowed.
    pub fn refusal(self, subject: &str, mode: SandboxMode) -> Option<String> {
        match self {
            EscalationOutcome::AllowedOnce => None,
            EscalationOutcome::Rejected => Some(format!(
                "the user rejected escalating this {subject} to \"{mode}\""
            )),
            EscalationOutcome::Cancelled => Some(format!(
                "approval for escalating to \"{mode}\" was cancelled"
            )),
            EscalationOutcome::Unavailable => Some(format!(
                "sandbox escalation to \"{mode}\" requires approval, but no approval channel is available"
            )),
        }
    }
}

/// The marker a denied file effect adds to a result (`sandboxDenialMarker`).
pub fn sandbox_denial_marker(mode: SandboxMode) -> String {
    format!("[sandbox: file access denied under {mode} mode]")
}

/// The hint that follows a denial when escalation is offered
/// (`escalationHintMarker`); `subject` is `command` or `operation`.
pub fn escalation_hint_marker(subject: &str) -> String {
    format!(
        "[sandbox: escalation available — retry this exact {subject} once with sandbox_permissions (the narrowest wider mode that suffices) + justification; the approval prompt asks the user]"
    )
}

/// The notice for a sandbox runner that failed before running the command
/// (`render.ts`).
pub fn runner_failed_notice(mode: SandboxMode) -> String {
    format!(
        "[sandbox: the sandbox runner itself failed under {mode} mode — the command did not run; this is a sandbox problem, not a command failure]"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use SandboxMode::*;

    #[test]
    fn checks_follow_typescript() {
        assert_eq!(
            check_escalation(ReadOnly, "workspace-write"),
            EscalationCheck::Widen(WorkspaceWrite)
        );
        assert_eq!(
            check_escalation(WorkspaceWrite, "danger-full-access"),
            EscalationCheck::Widen(DangerFullAccess)
        );
        assert_eq!(
            check_escalation(WorkspaceWrite, "workspace-write"),
            EscalationCheck::NoOp
        );
        assert_eq!(
            check_escalation(DangerFullAccess, "read-only"),
            EscalationCheck::NoOp
        );
        assert_eq!(
            check_escalation(WorkspaceWrite, "root"),
            EscalationCheck::Refused(
                "sandbox escalation to \"root\" is not strictly wider than this call's current \"workspace-write\" mode".into()
            )
        );
    }

    #[test]
    fn argument_pairing() {
        assert_eq!(validate_escalation_args(None, None), Ok(()));
        assert_eq!(
            validate_escalation_args(Some("workspace-write"), Some("needs it")),
            Ok(())
        );
        assert!(validate_escalation_args(Some("workspace-write"), None).is_err());
        assert!(validate_escalation_args(None, Some("x")).is_err());
        assert_eq!(
            validate_escalation_args(Some("workspace-write"), Some(" \u{feff}\n")),
            Err("invalid justification: expected a non-empty sentence".into())
        );
    }

    #[test]
    fn texts_are_the_typescript_ones() {
        assert_eq!(
            sandbox_denial_marker(WorkspaceWrite),
            "[sandbox: file access denied under workspace-write mode]"
        );
        assert_eq!(
            EscalationOutcome::Rejected
                .refusal("command", DangerFullAccess)
                .as_deref(),
            Some("the user rejected escalating this command to \"danger-full-access\"")
        );
        assert_eq!(
            EscalationOutcome::AllowedOnce.refusal("command", DangerFullAccess),
            None
        );
        assert_eq!(
            escalation_reason(WorkspaceWrite, "write the build dir"),
            "escalate sandbox to workspace-write: write the build dir"
        );
    }
}
