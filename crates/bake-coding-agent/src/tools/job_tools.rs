//! The model's background-job tools, `job_output`, `job_list`, and
//! `job_kill`, over one session's [`JobRegistry`].
//!
//! Pi v1.3.0 (`71023aec`; MIT, see the crate `NOTICE`) has no job tools.
//! Ported from Bake's TypeScript `packages/jobs/tool-jobs/src/index.ts`:
//! the names, parameters, descriptions under `wakeup` delivery, the
//! `[status: ...]` line, the still-running guidance, the list and kill
//! texts, and the unknown-job explanation. Differences: the `job_id`
//! description and the unknown-job text mention subagent ids only for a
//! session that can start subagents
//! ([`create_job_tool_definitions_with_subagents`]); terminal observations
//! carry recovery metadata in result details; a `completing` job, which
//! the TypeScript did not have, reads as `running`; output a stream dropped
//! between reads is announced on its own line; and a read longer than
//! Pi's output limits keeps its tail, as Pi's `bash` does, with a marker
//! instead of a spill file.
//!
//! The completion notice the descriptions promise is delivered by the
//! session ([`crate::agent_session`]); a still-running read adds
//! [`STILL_RUNNING`] while that notice would open the next turn
//! ([`JobRegistry::notice_would_wake`]).

use std::time::Duration;

use bake_agent::{AgentTool, AgentToolResult, ToolReplay};
use bake_ai::{AbortSignal, TextContent, UserContentBlock};
use serde_json::{Value, json};

use crate::jobs::{JobError, JobId, JobRegistry, JobSnapshot, JobStatus, KillResult};
use crate::tools::definition::ToolDefinition;
use crate::tools::truncate::{TruncationOptions, truncate_tail};

/// The names of the job tools, in the order they are declared.
pub const JOB_TOOL_NAMES: [&str; 3] = ["job_output", "job_list", "job_kill"];

/// A terminal status delivered in a tool result, acknowledged by the
/// session only when that result has been written.
pub(crate) const JOB_OBSERVATION_DETAIL: &str = "backgroundJobObservation";

/// `waitTimeoutMs`'s default: a `wait` without `timeout_ms`.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(30);

/// Where the model finds the id both job-addressing tools take.
const JOB_ID_DESCRIPTION: &str = "A job id.";

/// [`JOB_ID_DESCRIPTION`] for a session that can start subagents.
const JOB_ID_DESCRIPTION_WITH_SUBAGENTS: &str = "A job id, not a continuable subagent's agent id.";

/// Told to the model when a read finds the job still running and its
/// completion would open the next turn.
pub const STILL_RUNNING: &str = "Still running. Its completion notice will start your next turn; if nothing else needs doing now, end your turn instead of waiting.";

/// `job_output`, `job_list`, and `job_kill` over `jobs`.
pub fn create_job_tool_definitions(jobs: &JobRegistry) -> Vec<ToolDefinition> {
    definitions(jobs, false)
}

/// The job tools of a session that can start subagents: the `job_id`
/// description and the unknown-job text say that a subagent's id is not a
/// job id, `tool-jobs`' wording.
pub fn create_job_tool_definitions_with_subagents(jobs: &JobRegistry) -> Vec<ToolDefinition> {
    definitions(jobs, true)
}

fn definitions(jobs: &JobRegistry, subagents: bool) -> Vec<ToolDefinition> {
    vec![
        job_output_tool(jobs.clone(), subagents),
        job_list_tool(jobs.clone()),
        job_kill_tool(jobs.clone(), subagents),
    ]
}

/// A job tool with its replay class: `job_list` only reads, while
/// `job_output` advances the job's read cursor and `job_kill` stops the job.
fn definition(mut tool: AgentTool, replay: ToolReplay) -> ToolDefinition {
    tool.replay = Some(replay);
    ToolDefinition {
        tool,
        prompt_snippet: None,
        prompt_guidelines: None,
    }
}

fn text_result(text: String) -> AgentToolResult {
    AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent::new(text))],
        ..AgentToolResult::default()
    }
}

fn observation(text: String, snapshot: &JobSnapshot) -> AgentToolResult {
    let mut result = text_result(text);
    if snapshot.status.is_terminal() {
        result.details = Some(json!({ JOB_OBSERVATION_DETAIL: {
            "id": snapshot.id.to_string(), "status": snapshot.status.as_str()
        } }));
    }
    result
}

/// The status word the model sees: the TypeScript's five, with a
/// `completing` job still `running`.
fn public_status(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Completing => "running",
        status => status.as_str(),
    }
}

/// `statusLine`: `[status: <status>]`, with the producer's detail.
pub fn status_line(snapshot: &JobSnapshot) -> String {
    match &snapshot.detail {
        Some(detail) => format!("[status: {}, {detail}]", public_status(snapshot.status)),
        None => format!("[status: {}]", public_status(snapshot.status)),
    }
}

/// `validateJobId` and the registry's lookup, with `explainUnknownJob`.
fn job_id(jobs: &JobRegistry, args: &Value, subagents: bool) -> Result<JobId, String> {
    let text = args.get("job_id").and_then(Value::as_str).unwrap_or("");
    if text.is_empty() {
        return Err(format!(
            "invalid job_id: expected a non-empty string, got {}",
            Value::String(text.to_owned())
        ));
    }
    jobs.id(text).map_err(|error| explain(error, subagents))
}

fn explain(error: JobError, subagents: bool) -> String {
    match error {
        // Models reach for job_output to wait on a subagent; say how that
        // wait already happens.
        JobError::Unknown(id) if subagents => format!(
            "unknown job {id}; use job_list to find a background job id. A continuable subagent's id is not a job id: its result arrives as a notice when it finishes."
        ),
        JobError::Unknown(id) => {
            format!("unknown job {id}; use job_list to find a background job id.")
        }
        error => error.to_string(),
    }
}

fn job_id_parameter(subagents: bool) -> Value {
    let description = if subagents {
        JOB_ID_DESCRIPTION_WITH_SUBAGENTS
    } else {
        JOB_ID_DESCRIPTION
    };
    json!({ "type": "string", "description": description })
}

fn job_output_tool(jobs: JobRegistry, subagents: bool) -> ToolDefinition {
    let max_wait = jobs.limits().max_wait;
    let description = "Read a background job's new output since your last read, or its result once done; ends with `[status: ...]`. You are notified when it finishes; do not poll or sleep. The notice starts a new turn if yours has ended, so end your turn rather than wait.";
    let parameters = json!({
        "type": "object",
        "required": ["job_id"],
        "properties": {
            "job_id": job_id_parameter(subagents),
            "wait": {
                "type": "boolean",
                "description": "Block until done or timeout; only for a job about to finish that you cannot continue without."
            },
            "timeout_ms": {
                "type": "number",
                "description": format!("Default {}, max {}.", DEFAULT_WAIT.as_millis(), max_wait.as_millis())
            }
        }
    });
    let tool = AgentTool::new(
        "job_output",
        "job_output",
        description,
        parameters,
        move |_id, args, signal, _update, _ctx| {
            let jobs = jobs.clone();
            Box::pin(async move { job_output(&jobs, &args, signal, subagents).await })
        },
    );
    definition(tool, ToolReplay::Never)
}

async fn job_output(
    jobs: &JobRegistry,
    args: &Value,
    signal: Option<AbortSignal>,
    subagents: bool,
) -> Result<AgentToolResult, String> {
    let explain = |error| explain(error, subagents);
    let id = job_id(jobs, args, subagents)?;
    if args.get("wait").and_then(Value::as_bool) == Some(true) {
        let max = jobs.limits().max_wait;
        let asked = args
            .get("timeout_ms")
            .and_then(Value::as_f64)
            .filter(|ms| ms.is_finite())
            .map_or(DEFAULT_WAIT, |ms| {
                Duration::from_millis(ms.clamp(1.0, max.as_millis() as f64) as u64)
            });
        jobs.wait(id, asked.min(max), signal.as_ref())
            .await
            .map_err(explain)?;
    }
    let read = jobs.read(id).map_err(explain)?;
    let mut body = String::new();
    if read.omitted_bytes > 0 {
        body.push_str(&format!(
            "[{} bytes of earlier output were dropped]\n",
            read.omitted_bytes
        ));
    }
    body.push_str(&read.text);
    let body = if body.is_empty() {
        "(no new output)".to_owned()
    } else {
        let kept = truncate_tail(&body, TruncationOptions::default());
        if kept.truncated {
            format!("[output truncated]\n{}", kept.content)
        } else {
            body
        }
    };
    let body = body.strip_suffix('\n').unwrap_or(&body);
    let live = !read.snapshot.status.is_terminal();
    let mut text = format!("{body}\n{}", status_line(&read.snapshot));
    if live && jobs.notice_would_wake() {
        text.push('\n');
        text.push_str(STILL_RUNNING);
    }
    Ok(observation(text, &read.snapshot))
}

fn job_list_tool(jobs: JobRegistry) -> ToolDefinition {
    let tool = AgentTool::new(
        "job_list",
        "job_list",
        "List your background jobs, running and finished.",
        json!({ "type": "object", "properties": {} }),
        move |_id, _args, _signal, _update, _ctx| {
            let listed = jobs.list();
            Box::pin(async move {
                let text = if listed.is_empty() {
                    "(no background jobs)".to_owned()
                } else {
                    listed
                        .iter()
                        .map(|job| {
                            format!(
                                "{} [{}] {} — {}",
                                job.id,
                                job.id.kind(),
                                public_status(job.status),
                                job.label
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                Ok(text_result(text))
            })
        },
    );
    definition(tool, ToolReplay::Safe)
}

fn job_kill_tool(jobs: JobRegistry, subagents: bool) -> ToolDefinition {
    let parameters = json!({
        "type": "object",
        "required": ["job_id"],
        "properties": {
            "job_id": job_id_parameter(subagents),
            "reason": { "type": "string" }
        }
    });
    let tool = AgentTool::new(
        "job_kill",
        "job_kill",
        "Stop a background job (jobs outlive your turn).",
        parameters,
        move |_id, args, _signal, _update, _ctx| {
            let result = job_kill(&jobs, &args, subagents);
            Box::pin(async move { result })
        },
    );
    definition(tool, ToolReplay::Never)
}

fn job_kill(jobs: &JobRegistry, args: &Value, subagents: bool) -> Result<AgentToolResult, String> {
    let explain = |error| explain(error, subagents);
    let id = job_id(jobs, args, subagents)?;
    let reason = args
        .get("reason")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let outcome = jobs.kill(id, reason).map_err(explain)?;
    let snapshot = jobs.get(id).map_err(explain)?;
    Ok(match outcome {
        KillResult::AlreadyFinished => observation(
            format!("job {id} had already finished {}", status_line(&snapshot)),
            &snapshot,
        ),
        KillResult::Requested | KillResult::AlreadyStopping => {
            text_result(format!("requested cancellation of job {id}"))
        }
    })
}

#[cfg(test)]
#[path = "job_tools_tests.rs"]
mod tests;
