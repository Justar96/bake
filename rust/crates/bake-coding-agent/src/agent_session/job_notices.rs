//! Completion notices for a session's background jobs, and what a resumed
//! session learns from its file about jobs an earlier process ran.
//!
//! Ported from Bake's TypeScript `packages/jobs/tool-jobs/src/index.ts`
//! (`fitCompletionNotice`, `onJobDone`): a job that settles unreported
//! becomes a notice to its session. Where the TypeScript queued a user
//! message, the session queues Pi's custom message (`custom_message` entry,
//! [`NOTICE_TYPE`]), which reaches the model as a user message and records
//! the job in its details, so recovery can tell which jobs were reported.
//! When the notice is delivered is the session's ([`super::AgentSession`]):
//! inside a run at the next step, else by a turn it opens, within the
//! registry's wake budget.
//!
//! Recovery projects immutable `bake.job.state` custom entries: a durable
//! start precedes execution and a durable terminal fact precedes live
//! publication. Older `bash` results with [`BACKGROUND_JOB_DETAIL`] also
//! record starts. Terminal facts apply across branches because execution
//! cannot be undone; a notice or successful terminal job-tool result
//! acknowledges delivery only on the branch that received it.
//! On resume every valid recorded id is reserved. An unobserved start on
//! the selected branch gets its recorded terminal outcome, or an unknown
//! outcome when no terminal fact survived. Recovery never adopts or
//! restarts a process, and no output bytes are retained by this journal.

use std::collections::{BTreeMap, BTreeSet};

use bake_ai::{TextContent, UserContent, UserContentBlock};
use serde_json::{Value, json};

use super::job_journal::JOURNAL_TYPE;
use crate::jobs::{JobRegistry, JobSnapshot, StopCause, USER_STOP_REASON, parse_job_id};
use crate::session::SessionManager;
use crate::session::messages::CustomMessage;
use crate::tools::bash::BACKGROUND_JOB_DETAIL;
use crate::tools::job_tools::{JOB_OBSERVATION_DETAIL, status_line};

/// The `customType` of a job notice.
pub const NOTICE_TYPE: &str = "bake.job.notice";

/// The notice for `snapshot`, a terminal job: `fitCompletionNotice`'s text
/// without a byte limit.
///
/// A job the user stopped ([`crate::jobs::JobRegistry::stop`]) says so,
/// since the model did not ask for the stop.
pub fn completion_notice(snapshot: &JobSnapshot) -> CustomMessage {
    let by_user = matches!(
        &snapshot.stop,
        Some(StopCause::Kill { reason: Some(reason) }) if reason == USER_STOP_REASON
    );
    let text = format!(
        "background job {} ({}: {}) finished {}.{} Read its output with job_output.",
        snapshot.id,
        snapshot.id.kind(),
        snapshot.label,
        status_line(snapshot),
        if by_user { " The user stopped it." } else { "" }
    );
    notice(&snapshot.id.to_string(), text, snapshot.status.as_str())
}

/// Claims every notice due to the session: a completion notice for each
/// terminal job not yet reported, then the notices other agents posted
/// ([`JobRegistry::post`]), in arrival order.
pub fn claim(jobs: &JobRegistry) -> Vec<CustomMessage> {
    let mut notices: Vec<CustomMessage> = jobs
        .claim_unreported()
        .iter()
        .map(completion_notice)
        .collect();
    notices.extend(jobs.claim_mail());
    notices
}

/// The notice for a job an earlier process of the session started and did
/// not report.
pub fn lost_notice(id: &str, kind: &str, label: &str) -> CustomMessage {
    let text = format!(
        "background job {id} ({kind}: {label}) belongs to an earlier process and has no recorded final outcome [status: lost]. Its output is unavailable. Check whether its effects completed before starting it again."
    );
    notice(id, text, "lost")
}

/// `message` as the agent's message.
pub fn live(message: CustomMessage) -> bake_agent::AgentMessage {
    bake_agent::AgentMessage::Custom(std::sync::Arc::new(message))
}

fn notice(id: &str, text: String, status: &str) -> CustomMessage {
    CustomMessage {
        custom_type: NOTICE_TYPE.to_owned(),
        content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))]),
        display: true,
        details: Some(json!({ "jobId": id, "status": status })),
        timestamp: bake_ai::now_ms(),
    }
}

/// Every job the session file records as started: its id, kind, and label,
/// and whether it is on the current branch.
struct Started {
    kind: String,
    label: String,
    on_branch: bool,
}

fn recovered_completion(
    id: &str,
    job: &Started,
    status: &str,
    detail: Option<&str>,
) -> CustomMessage {
    let detail = detail
        .map(|detail| format!(", {detail}"))
        .unwrap_or_default();
    notice(
        id,
        format!(
            "background job {id} ({}: {}) finished in its earlier process [status: {status}{detail}]. Its output was not retained across processes and is unavailable.",
            job.kind, job.label
        ),
        status,
    )
}

/// Reserves every valid recorded id and returns an outcome notice for
/// each start on the selected branch without a durable observation there,
/// in start order. A missing terminal fact means an unknown outcome.
pub fn recover(session: &SessionManager, jobs: &JobRegistry) -> Vec<CustomMessage> {
    project(session, Some(jobs))
}

/// Durable starts without a selected-branch terminal observation. A running
/// job tool result is a start acknowledgement, never completion evidence.
pub(super) fn unobserved(session: &SessionManager) -> Vec<CustomMessage> {
    project(session, None)
}

fn project(session: &SessionManager, jobs: Option<&JobRegistry>) -> Vec<CustomMessage> {
    let branch: BTreeSet<&str> = session
        .branch_entries(None)
        .iter()
        .map(|entry| entry.id())
        .collect();
    let mut started: BTreeMap<String, Started> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut reported: BTreeSet<String> = BTreeSet::new();
    let mut recorded: BTreeSet<String> = BTreeSet::new();
    let mut terminal: BTreeMap<String, (String, Option<String>)> = BTreeMap::new();
    for entry in session.entries() {
        match entry.entry_type() {
            "message" => {
                let Some(message) = entry.get("message") else {
                    continue;
                };
                if message.get("role").and_then(Value::as_str) != Some("toolResult") {
                    continue;
                }
                if let Some(id) = terminal_observation(message) {
                    recorded.insert(id.to_owned());
                    if branch.contains(entry.id()) {
                        reported.insert(id.to_owned());
                    }
                }
                if message.get("toolName").and_then(Value::as_str) != Some("bash") {
                    continue;
                }
                let Some(job) = message
                    .get("details")
                    .and_then(|details| details.get(BACKGROUND_JOB_DETAIL))
                else {
                    continue;
                };
                let (Some(id), Some(label)) = (
                    job.get("id").and_then(Value::as_str),
                    job.get("label").and_then(Value::as_str),
                ) else {
                    continue;
                };
                let Some((kind, _)) = id.rsplit_once('-') else {
                    continue;
                };
                recorded.insert(id.to_owned());
                let on_branch = branch.contains(entry.id());
                let known = started.entry(id.to_owned()).or_insert_with(|| {
                    order.push(id.to_owned());
                    Started {
                        kind: kind.to_owned(),
                        label: label.to_owned(),
                        on_branch,
                    }
                });
                known.on_branch |= on_branch;
            }
            "custom_message" => {
                if entry.get("customType").and_then(Value::as_str) != Some(NOTICE_TYPE) {
                    continue;
                }
                if let Some(id) = notice_id(entry.get("details")) {
                    recorded.insert(id.to_owned());
                    if branch.contains(entry.id()) {
                        reported.insert(id.to_owned());
                    }
                }
            }
            "custom" if entry.get("customType").and_then(Value::as_str) == Some(JOURNAL_TYPE) => {
                let Some(data) = entry.get("data") else {
                    continue;
                };
                if data.get("version").and_then(Value::as_u64) != Some(1) {
                    continue;
                }
                let Some(id) = data.get("jobId").and_then(Value::as_str) else {
                    continue;
                };
                let Some((kind, _)) = parse(id) else {
                    continue;
                };
                if data.get("kind").and_then(Value::as_str) != Some(kind) {
                    continue;
                }
                let Some(label) = data.get("label").and_then(Value::as_str) else {
                    continue;
                };
                match data.get("phase").and_then(Value::as_str) {
                    Some("started") => {
                        recorded.insert(id.to_owned());
                        let on_branch = branch.contains(entry.id());
                        let known = started.entry(id.to_owned()).or_insert_with(|| {
                            order.push(id.to_owned());
                            Started {
                                kind: kind.into(),
                                label: label.into(),
                                on_branch,
                            }
                        });
                        known.on_branch |= on_branch;
                    }
                    Some("terminal") => {
                        let Some(status @ ("completed" | "killed" | "failed")) =
                            data.get("status").and_then(Value::as_str)
                        else {
                            continue;
                        };
                        recorded.insert(id.to_owned());
                        terminal.entry(id.to_owned()).or_insert_with(|| {
                            (
                                status.into(),
                                data.get("detail")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned),
                            )
                        });
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    let mut highest: BTreeMap<&'static str, u64> = BTreeMap::new();
    for id in &recorded {
        if let Some((kind, ordinal)) = parse(id) {
            let slot = highest.entry(kind).or_default();
            *slot = (*slot).max(ordinal);
        }
    }
    for (kind, through) in highest {
        if let Some(jobs) = jobs {
            let _ = jobs.reserve_ordinals(kind, through);
        }
    }
    order
        .iter()
        .filter(|id| !reported.contains(*id) && parse(id).is_some())
        .filter_map(|id| {
            let job = &started[id];
            job.on_branch.then(|| match terminal.get(id) {
                Some((status, detail)) => recovered_completion(id, job, status, detail.as_deref()),
                None => lost_notice(id, &job.kind, &job.label),
            })
        })
        .collect()
}

pub(super) fn observation_id(message: &bake_agent::AgentMessage) -> Option<&str> {
    if let Some(result) = message.as_tool_result() {
        if result.is_error || !matches!(result.tool_name.as_str(), "job_output" | "job_kill") {
            return None;
        }
        return terminal_id(result.details.as_ref()?.get(JOB_OBSERVATION_DETAIL)?);
    }
    let bake_agent::AgentMessage::Custom(custom) = message else {
        return None;
    };
    let message = custom.as_any().downcast_ref::<CustomMessage>()?;
    if message.custom_type != NOTICE_TYPE {
        return None;
    }
    notice_id(message.details.as_ref())
}

pub(super) fn notice_id(details: Option<&Value>) -> Option<&str> {
    let details = details?;
    let id = details.get("jobId")?.as_str()?;
    parse_job_id(id)?;
    matches!(
        details.get("status")?.as_str()?,
        "completed" | "killed" | "failed" | "lost"
    )
    .then_some(id)
}

/// Only a successful job-addressing tool can acknowledge a terminal
/// observation. Output text is untrusted and is never parsed as state.
fn terminal_observation(message: &Value) -> Option<&str> {
    if !matches!(
        message.get("toolName")?.as_str()?,
        "job_output" | "job_kill"
    ) || message.get("isError")?.as_bool()?
    {
        return None;
    }
    terminal_id(message.get("details")?.get(JOB_OBSERVATION_DETAIL)?)
}

fn terminal_id(observation: &Value) -> Option<&str> {
    let id = observation.get("id")?.as_str()?;
    parse_job_id(id)?;
    matches!(
        observation.get("status")?.as_str()?,
        "completed" | "failed" | "killed"
    )
    .then_some(id)
}

/// The shipped producer kinds as static registry names. New producer
/// kinds must extend this mapping so their earlier identities stay reserved.
fn parse(id: &str) -> Option<(&'static str, u64)> {
    let (kind, ordinal) = parse_job_id(id)?;
    (kind == "bash").then_some(("bash", ordinal))
}

#[cfg(test)]
#[path = "job_notices_tests.rs"]
mod tests;
