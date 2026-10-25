//! The text a model sees from subagent orchestration, and the custom
//! messages that carry it.
//!
//! Ported from Bake's TypeScript `packages/subagent/subagent/src/continuation-messages.ts`:
//! the `Agent {id} sent a message: ` prefix, the continuable return guidance
//! appended to a child's task, and the settlement summaries by stop reason
//! with `Its closing message:` or `It left no closing message.` An agent's
//! message and the runtime's account of a settled child are different
//! custom types ([`MESSAGE_TYPE`], [`NOTICE_TYPE`]), so a transcript never
//! credits one agent with another's words or with the runtime's account.
//! The lost-run notice is Bake's own.

use bake_ai::{TextContent, UserContent, UserContentBlock};
use serde_json::json;

use crate::session::messages::CustomMessage;

/// The `customType` of a message one agent sent another.
pub const MESSAGE_TYPE: &str = "bake.agent.message";

/// The `customType` of the runtime's notice that a child's run ended.
pub const NOTICE_TYPE: &str = "bake.subagent.notice";

/// How a child's run ended, 0.3's `SubagentResult['stopReason']` with
/// Bake's `interrupted` for a run the process did not survive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The child finished its turn.
    Completed,
    /// The parent, or the session's shutdown, stopped it.
    Aborted,
    /// The model's output limit ended it.
    MaxTokens,
    /// A request or the run failed.
    Error,
    /// The run was in flight when Bake stopped (recovery only).
    Interrupted,
}

impl StopReason {
    /// The word recorded in notice details and shown by `list_agents`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Aborted => "aborted",
            Self::MaxTokens => "max-tokens",
            Self::Error => "error",
            Self::Interrupted => "interrupted",
        }
    }

    /// The reason a recorded word names.
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "completed" => Self::Completed,
            "aborted" => Self::Aborted,
            "max-tokens" => Self::MaxTokens,
            "error" => Self::Error,
            "interrupted" => Self::Interrupted,
            _ => return None,
        })
    }
}

fn text_message(custom_type: &str, text: String, details: serde_json::Value) -> CustomMessage {
    CustomMessage {
        custom_type: custom_type.to_owned(),
        content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))]),
        display: true,
        details: Some(details),
        timestamp: bake_ai::now_ms(),
    }
}

/// `createAgentMessage`: what agent `from` sent agent `to`, as the
/// recipient's model reads it.
pub fn agent_message(message_id: &str, from: &str, to: &str, text: &str) -> CustomMessage {
    text_message(
        MESSAGE_TYPE,
        format!("Agent {from} sent a message: {text}"),
        json!({ "messageId": message_id, "from": from, "to": to }),
    )
}

/// `withContinuableReturnGuidance`: `prompt`, then the guidance naming the
/// parent and how to reach it.
pub fn task_with_return_guidance(parent_id: &str, prompt: &str) -> String {
    let parent = serde_json::Value::String(parent_id.to_owned()).to_string();
    format!(
        "{prompt}\n\nYour parent agent id is {parent}. Your final answer is delivered to the parent automatically; make it a self-contained result. The parent shares your workspace but does not receive your transcript, tool output, or reasoning. When an earlier finding changes what the parent should do next, use send_message({{ agent_id: {parent}, message: \"<actionable finding>\" }}); sending a message does not end your turn."
    )
}

/// `settlementSummary`: one line telling the parent the child is done and
/// why.
pub fn settlement_summary(child_id: &str, stop: StopReason) -> String {
    let subject = format!("Background subagent {child_id}");
    match stop {
        StopReason::Completed => {
            format!("{subject} finished and will do no further work unless you send it more.")
        }
        StopReason::Aborted => format!("{subject} was stopped before it finished."),
        StopReason::MaxTokens => format!("{subject} ran out of room before it finished."),
        StopReason::Error => format!("{subject} failed before it finished."),
        StopReason::Interrupted => format!(
            "{subject} was interrupted when Bake stopped and did not finish. Send it a message to continue it."
        ),
    }
}

/// `createSettlementMessage`: the summary, then the child's closing text
/// or that it left none.
pub fn settlement_notice(
    child_id: &str,
    run_id: u64,
    stop: StopReason,
    closing: Option<&str>,
) -> CustomMessage {
    let summary = settlement_summary(child_id, stop);
    let text = match closing.filter(|text| !text.is_empty()) {
        Some(closing) => format!("{summary}\nIts closing message:\n{closing}"),
        None if stop == StopReason::Interrupted => summary,
        None => format!("{summary}\nIt left no closing message."),
    };
    text_message(
        NOTICE_TYPE,
        text,
        json!({ "agentId": child_id, "runId": run_id, "stopReason": stop.as_str() }),
    )
}

/// The foreground failure headline, 0.3 `tool-subagent`'s
/// `stopReasonError`; `None` for a completed run.
pub fn foreground_error(stop: StopReason) -> Option<&'static str> {
    match stop {
        StopReason::Completed => None,
        StopReason::Aborted | StopReason::Interrupted => Some("subagent run was cancelled"),
        StopReason::Error => Some("subagent run failed"),
        StopReason::MaxTokens => Some("subagent run hit its token limit before finishing"),
    }
}

/// `withDiagnosticAndPartialText`: the headline, the failure's detail,
/// and the partial answer, each when present.
pub fn foreground_failure(
    headline: &str,
    diagnostic: Option<&str>,
    partial: Option<&str>,
) -> String {
    let mut text = headline.to_owned();
    if let Some(diagnostic) = diagnostic {
        text.push_str(&format!("\nDiagnostic: {diagnostic}"));
    }
    if let Some(partial) = partial.filter(|text| !text.is_empty()) {
        text.push_str(&format!(
            "\nPartial output before the run ended:\n{partial}"
        ));
    }
    text
}
