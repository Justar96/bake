//! Approval requests as the view shows them, and the view's answers as
//! the broker takes them.

use bake_coding_agent::permissions::{self, ApprovalPrompt};
use bake_tui_view::runtime::{Approval, ApprovalAnswer};

use super::visible;

/// A queued approval request as the view shows it.
pub(super) fn approval(prompt: ApprovalPrompt) -> Approval {
    Approval {
        generation: prompt.generation,
        tool: prompt.tool_name,
        call_id: prompt.call_id,
        parent_call_id: prompt.parent_tool_call_id,
        reason: prompt.reason,
        arguments: arguments_text(&prompt.arguments),
    }
}

/// The escalation arguments, which the reason already states.
const ESCALATION_ARGUMENTS: [&str; 2] = ["sandbox_permissions", "justification"];

/// A call's arguments for review, one argument per line as `name: value`,
/// apart from the escalation pair. A string shows as written, a string of
/// several lines on the lines under its name, each indented; any other
/// value as compact JSON. Control characters other than tab show as
/// `\u{…}` escapes, so nothing a call would run is hidden and nothing
/// reaches the terminal as an escape sequence.
pub(super) fn arguments_text(arguments: &serde_json::Value) -> String {
    let Some(object) = arguments.as_object() else {
        return visible(&arguments.to_string());
    };
    let mut lines = Vec::new();
    for (name, value) in object {
        if ESCALATION_ARGUMENTS.contains(&name.as_str()) {
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
        ApprovalAnswer::Reject => permissions::ApprovalAnswer::Reject,
        ApprovalAnswer::Cancel => permissions::ApprovalAnswer::Cancel,
    }
}
