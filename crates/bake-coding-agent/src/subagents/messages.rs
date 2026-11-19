//! The text a model sees from subagent orchestration, and the custom
//! messages that carry it.
//!
//! The texts: the `Agent {id} sent a message: ` prefix, the continuable return guidance
//! appended to a child's task, and the settlement summaries by stop reason
//! with `Its closing message:` or `It left no closing message.` An agent's
//! message and the runtime's account of a settled child are different
//! custom types ([`MESSAGE_TYPE`], [`NOTICE_TYPE`]), so a transcript never
//! credits one agent with another's words or with the runtime's account.
//! The lost-run notice, the lines on what a settled run left behind
//! ([`Leftovers`]), and the notice of a queued child that could not start
//! are Bake's own.

use bake_ai::{TextContent, UserContent, UserContentBlock};
use serde_json::json;

use crate::session::messages::CustomMessage;

/// The `customType` of a message one agent sent another.
pub const MESSAGE_TYPE: &str = "bake.agent.message";

/// The `customType` of the runtime's notice that a child's run ended.
pub const NOTICE_TYPE: &str = "bake.subagent.notice";

/// How a child's run ended, with `interrupted` for a run the process did
/// not survive.
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

/// What a settled run left behind: messages from the parent it did not
/// read, which wait for its next run, and its own background jobs still
/// running, whose notices start a new run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Leftovers {
    /// Parent messages the run did not read.
    pub unread: usize,
    /// The ids of the child's jobs still running.
    pub jobs: Vec<String>,
    /// Parent messages the run did not read, lost since, as Bake stopped.
    pub lost: usize,
}

impl Leftovers {
    /// One line for each kind of leftover, each ending in a newline.
    pub fn lines(&self) -> String {
        let mut text = String::new();
        match self.unread {
            0 => {}
            1 => text.push_str(
                "1 message you sent it is unread; its next run reads it first.
",
            ),
            n => text.push_str(&format!(
                "{n} messages you sent it are unread; its next run reads them first.
"
            )),
        }
        match self.lost {
            0 => {}
            1 => text.push_str("1 message you sent it was unread when Bake stopped and is lost.\n"),
            n => text.push_str(&format!(
                "{n} messages you sent it were unread when Bake stopped and are lost.\n"
            )),
        }
        if !self.jobs.is_empty() {
            text.push_str(&format!(
                "Its background jobs still run: {}. When one finishes it continues, and you are notified again.
",
                self.jobs.join(", ")
            ));
        }
        text
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
    settlement_notice_with(child_id, run_id, stop, closing, &Leftovers::default())
}

/// [`settlement_notice`], with what the run left behind after the summary.
pub fn settlement_notice_with(
    child_id: &str,
    run_id: u64,
    stop: StopReason,
    closing: Option<&str>,
    leftovers: &Leftovers,
) -> CustomMessage {
    let mut rest = leftovers.clone();
    // A finished turn whose jobs still run is not done, as Pi's summary
    // would say.
    let mut summary = if stop == StopReason::Completed && !rest.jobs.is_empty() {
        format!(
            "Background subagent {child_id} finished its turn, but its background jobs still run: {}. When one finishes it continues, and you are notified again.",
            std::mem::take(&mut rest.jobs).join(", ")
        )
    } else {
        settlement_summary(child_id, stop)
    };
    let lines = rest.lines();
    if !lines.is_empty() {
        summary.push('\n');
        summary.push_str(lines.trim_end());
    }
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

/// The notice for a queued child that could not `start` or `resume`:
/// its run never began, and `undelivered` messages it was sent are gone.
pub fn start_failure_notice(
    child_id: &str,
    run_id: u64,
    what: &str,
    error: &str,
    undelivered: usize,
) -> CustomMessage {
    let mut text = format!("Background subagent {child_id} could not {what}: {error}");
    match undelivered {
        0 => {}
        1 => text.push_str("\nThe message you sent it was not delivered."),
        n => text.push_str(&format!(
            "\nThe {n} messages you sent it were not delivered."
        )),
    }
    text_message(
        NOTICE_TYPE,
        text,
        json!({ "agentId": child_id, "runId": run_id, "stopReason": StopReason::Error.as_str() }),
    )
}

/// The foreground failure headline; `None` for a completed run.
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
