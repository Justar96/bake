//! Approval requests as the view shows them, and the view's answers as
//! the broker takes them.

use bake_coding_agent::permissions::{self, ApprovalPrompt};
use bake_sandbox::SandboxMode;
use bake_tui_view::runtime::{Approval, ApprovalAnswer};

use super::visible;

/// A queued approval request as the view shows it: what the call would
/// run or change first, the access it wants in plain words, and the
/// model's justification, with the call's other arguments after them.
pub(super) fn approval(prompt: ApprovalPrompt) -> Approval {
    let target_key = target_key(&prompt.tool_name);
    let text = |key: &str| {
        prompt
            .arguments
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let target = target_key.and_then(text);
    // Shown on its own only when it is the string the panel expects.
    let shown = target.as_ref().and(target_key);
    Approval {
        generation: prompt.generation,
        access: prompt
            .sandbox
            .map(|mode| access(&prompt.tool_name, mode).to_owned()),
        target,
        justification: text("justification")
            .filter(|_| prompt.sandbox.is_some())
            .map(|justification| justification.trim().to_owned()),
        session: prompt.offers.session,
        full_access: prompt.offers.full_access,
        arguments: arguments_text(&prompt.arguments, shown),
        tool: prompt.tool_name,
        call_id: prompt.call_id,
        parent_call_id: prompt.parent_tool_call_id,
        reason: prompt.reason,
    }
}

/// The argument a tool's request shows first: `bash`'s command, the path
/// `write` or `edit` changes.
fn target_key(tool: &str) -> Option<&'static str> {
    match tool {
        "bash" => Some("command"),
        "write" | "edit" => Some("path"),
        _ => None,
    }
}

/// What `tool` wants to do with `mode`, after "wants to".
fn access(tool: &str, mode: SandboxMode) -> &'static str {
    match (mode, tool) {
        (SandboxMode::DangerFullAccess, "bash") => "run outside the sandbox",
        (SandboxMode::DangerFullAccess, "write" | "edit") => "write outside the workspace",
        (SandboxMode::DangerFullAccess, _) => "run with full access",
        (SandboxMode::WorkspaceWrite, _) => "change files in the workspace",
        (SandboxMode::ReadOnly, _) => "run read-only",
    }
}

/// The escalation arguments, which the panel's title and quote already
/// state.
const ESCALATION_ARGUMENTS: [&str; 2] = ["sandbox_permissions", "justification"];

/// A call's arguments for review, one argument per line as `name: value`,
/// apart from the escalation pair and `shown`, the one the panel shows on
/// its own. A string shows as written, a string of several lines on the
/// lines under its name, each indented; any other value as compact JSON.
/// Control characters other than tab show as `\u{…}` escapes, so nothing
/// a call would run is hidden and nothing reaches the terminal as an
/// escape sequence.
pub(super) fn arguments_text(arguments: &serde_json::Value, shown: Option<&str>) -> String {
    let Some(object) = arguments.as_object() else {
        return visible(&arguments.to_string());
    };
    let mut lines = Vec::new();
    for (name, value) in object {
        if ESCALATION_ARGUMENTS.contains(&name.as_str()) || Some(name.as_str()) == shown {
            continue;
        }
        let name = visible(name);
        match value.as_str() {
            Some(text) if text.contains('\n') => {
                lines.push(format!("{name}:"));
                lines.extend(text.split('\n').map(|line| format!("  {}", visible(line))));
            }
            Some(text) => lines.push(format!("{name}: {}", visible(text))),
            None => lines.push(format!("{name}: {}", visible(&value.to_string()))),
        }
    }
    lines.join("\n")
}

/// The view's answer as the broker takes it.
pub(super) fn broker_answer(answer: ApprovalAnswer) -> permissions::ApprovalAnswer {
    match answer {
        ApprovalAnswer::AllowOnce => permissions::ApprovalAnswer::AllowOnce,
        ApprovalAnswer::AllowSession => permissions::ApprovalAnswer::AllowSession,
        ApprovalAnswer::AllowFullAccess => permissions::ApprovalAnswer::AllowFullAccess,
        ApprovalAnswer::Reject => permissions::ApprovalAnswer::Reject,
        ApprovalAnswer::RejectWithNote(note) => permissions::ApprovalAnswer::RejectWithNote(note),
        ApprovalAnswer::Cancel => permissions::ApprovalAnswer::Cancel,
    }
}
