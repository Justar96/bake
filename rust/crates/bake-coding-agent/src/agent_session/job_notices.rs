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
//! Recovery is Bake's own, for the scope 11 rule that a job is lost with
//! its process and reported to the agent once. A `bash` result whose
//! details hold [`BACKGROUND_JOB_DETAIL`] records that a job started; a
//! notice entry records that it was reported. On resume every recorded id
//! is reserved, so an id is never issued twice in one session file, and
//! every job started on the current branch without a notice anywhere in
//! the file gets a lost notice, delivered with the next prompt. A notice
//! claimed but not yet written when the process ended is reported as lost
//! instead; no notice is written twice.

use std::collections::{BTreeMap, BTreeSet};

use bake_ai::{TextContent, UserContent, UserContentBlock};
use serde_json::{Value, json};

use crate::jobs::{JobRegistry, JobSnapshot};
use crate::session::SessionManager;
use crate::session::messages::CustomMessage;
use crate::tools::bash::BACKGROUND_JOB_DETAIL;
use crate::tools::job_tools::status_line;

/// The `customType` of a job notice.
pub const NOTICE_TYPE: &str = "bake.job.notice";

/// The notice for `snapshot`, a terminal job: `fitCompletionNotice`'s text
/// without a byte limit.
pub fn completion_notice(snapshot: &JobSnapshot) -> CustomMessage {
    let text = format!(
        "background job {} ({}: {}) finished {}. Read its output with job_output.",
        snapshot.id,
        snapshot.id.kind(),
        snapshot.label,
        status_line(snapshot)
    );
    notice(&snapshot.id.to_string(), text, snapshot.status.as_str())
}

/// The notice for a job an earlier process of the session started and did
/// not report.
pub fn lost_notice(id: &str, kind: &str, label: &str) -> CustomMessage {
    let text = format!(
        "background job {id} ({kind}: {label}) did not survive the end of the earlier process that ran it [status: killed, lost with its process]. Its output is gone; start it again if you still need it."
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

/// Reserves every job id `session` records in `jobs` and returns a lost
/// notice for each job started on the current branch and reported
/// nowhere in the file, in the order they were started. Ids of a kind the
/// registry cannot hold, or malformed details, are skipped.
pub fn recover(session: &SessionManager, jobs: &JobRegistry) -> Vec<CustomMessage> {
    let branch: BTreeSet<&str> = session
        .branch_entries(None)
        .iter()
        .map(|entry| entry.id())
        .collect();
    let mut started: BTreeMap<String, Started> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut reported: BTreeSet<String> = BTreeSet::new();
    for entry in session.entries() {
        match entry.entry_type() {
            "message" => {
                let Some(message) = entry.get("message") else {
                    continue;
                };
                if message.get("role").and_then(Value::as_str) != Some("toolResult")
                    || message.get("toolName").and_then(Value::as_str) != Some("bash")
                {
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
                if let Some(id) = entry
                    .get("details")
                    .and_then(|details| details.get("jobId"))
                    .and_then(Value::as_str)
                {
                    reported.insert(id.to_owned());
                }
            }
            _ => {}
        }
    }
    let mut highest: BTreeMap<&'static str, u64> = BTreeMap::new();
    for id in &order {
        if let Some((kind, ordinal)) = parse(id) {
            let slot = highest.entry(kind).or_default();
            *slot = (*slot).max(ordinal);
        }
    }
    for (kind, through) in highest {
        let _ = jobs.reserve_ordinals(kind, through);
    }
    order
        .iter()
        .filter(|id| !reported.contains(*id) && parse(id).is_some())
        .filter_map(|id| {
            let job = &started[id];
            job.on_branch
                .then(|| lost_notice(id, &job.kind, &job.label))
        })
        .collect()
}

/// `bash-<n>`: the only kind a session records today, as the registry's
/// static kind name, and its ordinal.
fn parse(id: &str) -> Option<(&'static str, u64)> {
    let (kind, number) = id.rsplit_once('-')?;
    if kind != "bash"
        || number.is_empty()
        || number.starts_with('0')
        || !number.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(("bash", number.parse().ok()?))
}

#[cfg(test)]
#[path = "job_notices_tests.rs"]
mod tests;
