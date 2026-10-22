//! Decoding a hook process's outcome, ported from Bake 0.3's
//! `packages/hooks/hook-protocol/src/codec.ts`.
//!
//! Exit 0 may carry structured JSON on standard output, or plain text;
//! exit 2 blocks with standard error as the reason; every other exit, and a
//! hook that could not run, is a non-blocking error. Decoding is total:
//! malformed JSON stays plain output. The dialects decide which fields
//! apply (`super::session`).

use serde_json::{Map, Value};

/// The exit code a hook blocks with; its standard error is the reason.
const BLOCKING_EXIT_CODE: i32 = 2;

/// A decision a hook expressed, from the legacy top-level `decision`
/// (`approve` or `block` only) or `hookSpecificOutput.permissionDecision`
/// (`allow`, `deny`, or `ask` only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookDecision {
    Approve,
    Allow,
    Block,
    Deny,
    Ask,
}

impl HookDecision {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Allow => "allow",
            Self::Block => "block",
            Self::Deny => "deny",
            Self::Ask => "ask",
        }
    }
}

/// The dialect-neutral outcome of one hook process, 0.3's `HookOutput`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookOutput {
    /// The exit code; `None` when the hook could not run, timed out, or
    /// died by a signal.
    pub exit_code: Option<i32>,
    /// Standard error, trimmed: the reason of an exit-2 block.
    pub stderr: String,
    /// Standard output, trimmed.
    pub stdout: String,
    /// `continue`: `Some(false)` asks the run to stop.
    pub continue_: Option<bool>,
    /// `stopReason`.
    pub stop_reason: Option<String>,
    /// The decision, from either channel.
    pub decision: Option<HookDecision>,
    /// The reason accompanying the decision.
    pub reason: Option<String>,
    /// `hookSpecificOutput.hookEventName`, recorded even on a mismatch.
    pub hook_event_name: Option<String>,
    /// `hookSpecificOutput.additionalContext`.
    pub additional_context: Option<String>,
    /// `systemMessage`, which is neither shown nor recorded.
    pub system_message: Option<String>,
    /// Whether `hookSpecificOutput.updatedInput` was given; it is not
    /// honored.
    pub updated_input: bool,
}

fn string(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Decodes a hook's exit code and output. When `expected_event` is given, a
/// `hookSpecificOutput` block whose `hookEventName` is missing or names
/// another event has its event-scoped fields discarded; the top-level
/// fields and the claimed name still apply.
pub fn parse_hook_output(
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
    expected_event: Option<&str>,
) -> HookOutput {
    let mut output = HookOutput {
        exit_code,
        stderr: stderr.trim().to_owned(),
        stdout: stdout.trim().to_owned(),
        ..HookOutput::default()
    };
    if exit_code == Some(BLOCKING_EXIT_CODE) {
        output.decision = Some(HookDecision::Block);
        if !output.stderr.is_empty() {
            output.reason = Some(output.stderr.clone());
        }
    }
    // Structured output only on a clean exit, and only when it looks like
    // an object; anything else is plain text.
    if exit_code == Some(0)
        && output.stdout.starts_with('{')
        && let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(&output.stdout)
    {
        apply_structured(&mut output, &parsed, expected_event);
    }
    output
}

fn apply_structured(
    output: &mut HookOutput,
    parsed: &Map<String, Value>,
    expected_event: Option<&str>,
) {
    if let Some(value) = parsed.get("continue").and_then(Value::as_bool) {
        output.continue_ = Some(value);
    }
    if let Some(value) = string(parsed, "stopReason") {
        output.stop_reason = Some(value);
    }
    if let Some(value) = string(parsed, "systemMessage") {
        output.system_message = Some(value);
    }
    // The top-level `decision` is `approve` or `block` in both schemas; an
    // out-of-band `deny` there is invalid and ignored.
    match parsed.get("decision").and_then(Value::as_str) {
        Some("approve") => output.decision = Some(HookDecision::Approve),
        Some("block") => output.decision = Some(HookDecision::Block),
        _ => {}
    }
    if let Some(value) = string(parsed, "reason") {
        output.reason = Some(value);
    }
    let Some(specific) = parsed.get("hookSpecificOutput").and_then(Value::as_object) else {
        return;
    };
    let event = string(specific, "hookEventName");
    if event.is_some() {
        output.hook_event_name.clone_from(&event);
    }
    if expected_event.is_some_and(|expected| event.as_deref() != Some(expected)) {
        return;
    }
    match specific.get("permissionDecision").and_then(Value::as_str) {
        Some("allow") => output.decision = Some(HookDecision::Allow),
        Some("deny") => output.decision = Some(HookDecision::Deny),
        Some("ask") => output.decision = Some(HookDecision::Ask),
        _ => {}
    }
    if let Some(value) = string(specific, "permissionDecisionReason") {
        output.reason = Some(value);
    }
    if let Some(value) = string(specific, "additionalContext") {
        output.additional_context = Some(value);
    }
    if specific.get("updatedInput").is_some_and(Value::is_object) {
        output.updated_input = true;
    }
}

#[cfg(test)]
mod tests {
    //! Ported from 0.3's `hook-protocol/tests/codec.spec.ts`.
    use serde_json::json;

    use super::*;

    fn clean(stdout: Value, expected: Option<&str>) -> HookOutput {
        parse_hook_output(Some(0), &stdout.to_string(), "", expected)
    }

    #[test]
    fn exit_0_without_output_is_neutral() {
        let out = parse_hook_output(Some(0), "", "", None);
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(out.decision, None);
        assert_eq!(out.continue_, None);
    }

    #[test]
    fn exit_2_blocks_with_stderr_as_the_reason() {
        let out = parse_hook_output(Some(2), "", "this command is not allowed", None);
        assert_eq!(out.decision, Some(HookDecision::Block));
        assert_eq!(out.reason.as_deref(), Some("this command is not allowed"));
        assert_eq!(out.stderr, "this command is not allowed");
        let out = parse_hook_output(Some(2), "", "   ", None);
        assert_eq!(out.decision, Some(HookDecision::Block));
        assert_eq!(out.reason, None);
    }

    #[test]
    fn other_exits_and_failures_carry_no_decision() {
        let out = parse_hook_output(Some(1), "", "some warning", None);
        assert_eq!((out.decision, out.exit_code), (None, Some(1)));
        assert_eq!(out.stderr, "some warning");
        let out = parse_hook_output(None, "", "spawn failed: ENOENT", None);
        assert_eq!((out.decision, out.exit_code), (None, None));
        assert_eq!(out.stderr, "spawn failed: ENOENT");
    }

    #[test]
    fn top_level_fields_are_parsed() {
        let out = clean(
            json!({ "continue": false, "stopReason": "budget exceeded", "systemMessage": "heads up" }),
            None,
        );
        assert_eq!(out.continue_, Some(false));
        assert_eq!(out.stop_reason.as_deref(), Some("budget exceeded"));
        assert_eq!(out.system_message.as_deref(), Some("heads up"));
    }

    #[test]
    fn only_approve_and_block_are_top_level_decisions() {
        let decision = |value: &str| clean(json!({ "decision": value }), None).decision;
        assert_eq!(decision("block"), Some(HookDecision::Block));
        assert_eq!(decision("approve"), Some(HookDecision::Approve));
        for invalid in ["deny", "allow", "ask", "maybe"] {
            assert_eq!(decision(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn the_permission_decision_overrides_the_top_level_one() {
        let out = clean(
            json!({
                "decision": "approve",
                "hookSpecificOutput": { "permissionDecision": "deny", "permissionDecisionReason": "denied by policy" },
            }),
            None,
        );
        assert_eq!(out.decision, Some(HookDecision::Deny));
        assert_eq!(out.reason.as_deref(), Some("denied by policy"));
        let permission = |value: &str| {
            clean(
                json!({ "hookSpecificOutput": { "permissionDecision": value } }),
                None,
            )
            .decision
        };
        assert_eq!(permission("allow"), Some(HookDecision::Allow));
        assert_eq!(permission("ask"), Some(HookDecision::Ask));
    }

    #[test]
    fn hook_specific_context_and_updated_input_are_read() {
        let out = clean(
            json!({ "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": "remember X",
                "updatedInput": { "command": "safe" },
            } }),
            None,
        );
        assert_eq!(out.hook_event_name.as_deref(), Some("PreToolUse"));
        assert_eq!(out.additional_context.as_deref(), Some("remember X"));
        assert!(out.updated_input);
    }

    #[test]
    fn a_block_for_another_event_or_none_is_discarded() {
        let mismatched = json!({ "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": "no",
            "additionalContext": "x",
            "updatedInput": { "command": "y" },
        } });
        let out = clean(mismatched.clone(), Some("Stop"));
        assert_eq!(out.hook_event_name.as_deref(), Some("PreToolUse"));
        assert_eq!(out.decision, None);
        assert_eq!(out.reason, None);
        assert_eq!(out.additional_context, None);
        assert!(!out.updated_input);
        let out = clean(mismatched.clone(), Some("PreToolUse"));
        assert_eq!(out.decision, Some(HookDecision::Deny));
        assert_eq!(out.additional_context.as_deref(), Some("x"));
        // Without an expected event the block applies as is.
        assert_eq!(clean(mismatched, None).decision, Some(HookDecision::Deny));
        let unnamed = json!({ "hookSpecificOutput": { "permissionDecision": "deny", "additionalContext": "x" } });
        let out = clean(unnamed.clone(), Some("Stop"));
        assert_eq!((out.hook_event_name, out.decision), (None, None));
        assert_eq!(out.additional_context, None);
        assert_eq!(clean(unnamed, None).decision, Some(HookDecision::Deny));
    }

    #[test]
    fn a_discarded_block_keeps_the_top_level_fields() {
        let out = clean(
            json!({
                "decision": "block", "reason": "top", "continue": false, "stopReason": "halt",
                "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "allow" },
            }),
            Some("Stop"),
        );
        assert_eq!(out.decision, Some(HookDecision::Block));
        assert_eq!(out.reason.as_deref(), Some("top"));
        assert_eq!(out.continue_, Some(false));
        assert_eq!(out.stop_reason.as_deref(), Some("halt"));
    }

    #[test]
    fn malformed_or_plain_output_stays_plain() {
        let out = parse_hook_output(Some(0), "{ not valid json", "", None);
        assert_eq!((out.decision, out.continue_), (None, None));
        let out = parse_hook_output(Some(0), "just some text output", "", None);
        assert_eq!(out.stdout, "just some text output");
        assert_eq!(out.decision, None);
        assert_eq!(
            parse_hook_output(Some(0), "[1,2,3]", "", None).decision,
            None
        );
        assert_eq!(parse_hook_output(Some(0), "", "", None).stdout, "");
    }

    #[test]
    fn stdout_is_kept_trimmed_beside_its_fields() {
        let text = json!({ "decision": "block" }).to_string();
        let out = parse_hook_output(Some(0), &format!("  {text}  \n"), "", None);
        assert_eq!(out.stdout, text);
        assert_eq!(out.decision, Some(HookDecision::Block));
    }

    #[test]
    fn structured_output_is_ignored_on_exit_2() {
        let out = parse_hook_output(
            Some(2),
            &json!({ "decision": "approve" }).to_string(),
            "blocked",
            None,
        );
        assert_eq!(out.decision, Some(HookDecision::Block));
        assert_eq!(out.reason.as_deref(), Some("blocked"));
    }
}
