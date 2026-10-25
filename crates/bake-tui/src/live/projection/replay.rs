//! A resumed session's transcript, and the notices of background work as
//! the cards of the jobs and subagents they report.

use super::calls::{display_name, job_state, outcome};
use super::*;

/// The transcript a resumed session opens with, Pi's `renderSessionContext`
/// over the restored messages: prompts, responses with their calls settled
/// by their results, notices paired with the work they report, and
/// compaction summaries as a note.
pub fn replay(messages: &[AgentMessage]) -> Vec<Row> {
    let labels = Projector::with_history(messages).labels;
    let settled: HashMap<String, Outcome> = messages
        .iter()
        .filter_map(AgentMessage::as_tool_result)
        .map(|result| {
            (
                result.tool_call_id.clone(),
                outcome(
                    &result.tool_name,
                    result.is_error,
                    &result.content,
                    result.details.as_ref(),
                ),
            )
        })
        .collect();
    let mut rows = Vec::new();
    for message in messages {
        if let Some(user) = message.as_user() {
            rows.push(Row::User(user_text(&user.content)));
        } else if let Some(assistant) = message.as_assistant() {
            let failure = match assistant.stop_reason {
                StopReason::Aborted => Some(ABORTED.to_owned()),
                StopReason::Error => Some(
                    assistant
                        .error_message
                        .clone()
                        .filter(|message| !message.is_empty())
                        .unwrap_or_else(|| "Error".into()),
                ),
                _ => None,
            };
            let (mut assistant_rows, _) = assistant_rows(
                assistant,
                &settled,
                &labels,
                Some(failure.as_deref().unwrap_or(INTERRUPTED)),
            );
            for row in &mut assistant_rows {
                // A call with no result in the log never finished.
                let (output, summary) = match row {
                    Row::Call {
                        state: CallState::Failed,
                        output,
                        summary,
                        ..
                    } => (output, summary),
                    Row::Codemode(script) if script.state == CallState::Failed => {
                        (&mut script.output, &mut script.summary)
                    }
                    _ => continue,
                };
                if output.len() == 1 && output[0] == INTERRUPTED {
                    output.clear();
                    *summary = Some(INTERRUPTED.into());
                }
            }
            rows.extend(assistant_rows);
            if let Some(failure) = failure {
                rows.push(Row::Error(failure));
            }
        } else if message.role() == "compactionSummary" {
            rows.push(Row::Info("Earlier conversation was compacted".into()));
        } else if message.role() == "branchSummary" {
            rows.push(Row::branch_summary());
        } else if let Some(row) = notice_row(message, &labels) {
            rows.push(row);
        }
    }
    rows
}

/// The transcript after `/compact`, Pi's `compaction_end` rendering: the
/// kept history the compacted context holds, without the latest summary,
/// which the caller notes below it at its place in time. Without a summary
/// it is [`replay()`].
pub fn replay_compacted(messages: &[AgentMessage]) -> Vec<Row> {
    match messages
        .iter()
        .position(|message| message.role() == "compactionSummary")
    {
        Some(index) => {
            let mut kept = messages.to_vec();
            kept.remove(index);
            replay(&kept)
        }
        None => replay(messages),
    }
}

/// The row of a notice: a background job's notice ([`NOTICE_TYPE`]) and a
/// subagent's settlement as the cards of the work they report
/// ([`job_notice`], [`agent_notice`]), or as information when their text
/// cannot be read that way; a subagent's message as `← agent {id}: {first
/// line}`, its first line kept to 48 characters as 0.3's `presentation.ts`
/// keeps titles; and a schedule reminder as `reminder {id}: {first line}`,
/// or a batch as `reminders {id}, {id}`; and the note of a run its `Stop`
/// hooks kept going too often, as its text.
pub(super) fn notice_row(message: &AgentMessage, labels: &Labels) -> Option<Row> {
    let custom = message.as_custom()?;
    let custom = custom.as_any().downcast_ref::<CustomMessage>()?;
    let text = user_text(&custom.content);
    let details = custom.details.as_ref();
    let info = match custom.custom_type.as_str() {
        NOTICE_TYPE => return Some(job_notice(&text).unwrap_or(Row::Info(text))),
        subagents::NOTICE_TYPE => {
            return Some(
                agent_notice(&text, details, labels).unwrap_or_else(|| {
                    Row::Info(text.lines().next().unwrap_or_default().to_owned())
                }),
            );
        }
        subagents::MESSAGE_TYPE => {
            let from = details
                .and_then(|details| details.get("from"))
                .and_then(Value::as_str)?;
            let body = text
                .strip_prefix(&format!("Agent {from} sent a message: "))
                .unwrap_or(&text);
            format!("← agent {from}: {}", title(body))
        }
        schedules::REMINDER_TYPE => {
            let reminders = details
                .and_then(|details| details.get("reminders"))
                .and_then(Value::as_array)?;
            match reminders.as_slice() {
                [one] => format!(
                    "reminder {}: {}",
                    one.get("id").and_then(Value::as_str)?,
                    title(one.get("prompt").and_then(Value::as_str)?)
                ),
                many => format!(
                    "reminders {}",
                    many.iter()
                        .filter_map(|reminder| reminder.get("id").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        }
        // The note a run ends with when its `Stop` hooks reach their cap.
        hooks::CONTEXT_MESSAGE_TYPE
            if details.and_then(|details| details.get("capped")) == Some(&Value::Bool(true)) =>
        {
            text
        }
        _ => return None,
    };
    Some(Row::Info(info))
}

/// A background job's notice as the card of the job, 0.3's `jobDone`: the
/// title of its kind, its label, its id, and how it ended, read from
/// `background job {id} ({kind}: {label}) … [status: {status}]`. A job an
/// earlier process ran says so.
fn job_notice(text: &str) -> Option<Row> {
    let rest = text.strip_prefix("background job ")?;
    let (id, rest) = rest.split_once(" (")?;
    let (kind, rest) = rest.split_once(": ")?;
    let at = rest.rfind("[status: ")?;
    // A label can hold `) ` itself; the last one before the status ends it.
    let end = rest[..at].rfind(") ")?;
    let label = &rest[..end];
    let earlier = rest[end..at].contains("earlier process");
    let status = &rest[at..=at + rest[at..].rfind(']')?];
    let (state, word) = if status == "[status: lost]" {
        (CallState::Failed, copy::JOB_LOST.to_owned())
    } else {
        let job = job_state(status)?;
        let state = if job.failed() {
            CallState::Failed
        } else {
            CallState::Done
        };
        (state, jobs::word(&job))
    };
    let summary = if earlier {
        format!("{word}{}{}", copy::STATUS_SEP, copy::EARLIER_PROCESS)
    } else {
        word
    };
    Some(Row::Call {
        tool: display_name(kind),
        argument: label.to_owned(),
        id: Some(id.to_owned()),
        state,
        summary: Some(summary),
        output: Vec::new(),
    })
}

/// A subagent's settlement as the card of the agent: its label, its id,
/// how its run ended, and its closing message as the output.
fn agent_notice(text: &str, details: Option<&Value>, labels: &Labels) -> Option<Row> {
    let field = |name| details?.get(name)?.as_str();
    let id = field("agentId")?;
    let (state, word) = match subagents::StopReason::parse(field("stopReason")?)? {
        subagents::StopReason::Completed => (CallState::Done, copy::JOB_DONE),
        subagents::StopReason::Aborted => (CallState::Done, copy::JOB_STOPPED),
        subagents::StopReason::MaxTokens => (CallState::Failed, copy::TOKEN_LIMIT),
        subagents::StopReason::Error => (CallState::Failed, copy::JOB_FAILED),
        subagents::StopReason::Interrupted => (CallState::Failed, copy::INTERRUPTED_NOTE),
    };
    let output = text
        .split_once("\nIts closing message:\n")
        .map(|(_, closing)| closing.trim_end().lines().map(str::to_owned).collect())
        .unwrap_or_default();
    Some(Row::Call {
        tool: display_name("subagent"),
        argument: labels.get(id).cloned().unwrap_or_default(),
        id: Some(id.to_owned()),
        state,
        summary: Some(word.to_owned()),
        output,
    })
}

/// A notice's first line, kept to 48 characters.
fn title(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    let mut title: String = line.chars().take(48).collect();
    if title.len() < line.len() {
        title.push('…');
    }
    title
}
