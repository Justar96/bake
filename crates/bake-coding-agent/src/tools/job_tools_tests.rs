//! The job tools' texts over a real registry, against Bake's TypeScript
//! `packages/jobs/tool-jobs/tests/` cases for the list, read, wait, and
//! kill results and the unknown-job explanation.

use std::time::Duration;

use bake_agent::AgentToolUpdateCallback;
use tokio::sync::oneshot;

use super::*;
use crate::jobs::{JobLimits, JobOutcome, JobSpec, OutputMode};
use crate::owned_work::WorkGate;

fn registry() -> JobRegistry {
    JobRegistry::new(WorkGate::default(), JobLimits::default())
}

fn tool(jobs: &JobRegistry, name: &str) -> AgentTool {
    create_job_tool_definitions(jobs)
        .into_iter()
        .find(|definition| definition.name() == name)
        .expect("a job tool")
        .tool
}

async fn call(jobs: &JobRegistry, name: &str, args: Value) -> Result<String, String> {
    let tool = tool(jobs, name);
    let result = (tool.execute)(
        "call".into(),
        args,
        None,
        AgentToolUpdateCallback::ignore(),
        bake_agent::ToolContext::detached(),
    )
    .await?;
    Ok(result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect())
}

/// A stream job that writes `first`, then waits for its release and
/// completes with `exit code: 0`.
fn held(jobs: &JobRegistry, label: &str, first: &'static str) -> (JobId, oneshot::Sender<()>) {
    let (release, released) = oneshot::channel::<()>();
    let id = jobs
        .start(
            JobSpec {
                kind: "bash",
                label: label.into(),
                output: OutputMode::Stream,
            },
            move |context| async move {
                if let Some(output) = context.output() {
                    output.append(first.as_bytes());
                }
                tokio::select! {
                    _ = released => JobOutcome::completed("exit code: 0"),
                    () = context.signal().cancelled() => JobOutcome::killed("signal: SIGKILL"),
                }
            },
        )
        .expect("admitted");
    (id, release)
}

async fn settle(jobs: &JobRegistry, id: JobId) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !jobs.get(id).expect("kept").status.is_terminal() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the job settled");
}

#[tokio::test]
async fn the_list_names_each_job_and_says_when_there_is_none() {
    let jobs = registry();
    assert_eq!(
        call(&jobs, "job_list", json!({})).await.unwrap(),
        "(no background jobs)"
    );
    let (id, release) = held(&jobs, "npm test", "");
    assert_eq!(
        call(&jobs, "job_list", json!({})).await.unwrap(),
        format!("{id} [bash] running — npm test")
    );
    release.send(()).unwrap();
    settle(&jobs, id).await;
    assert_eq!(
        call(&jobs, "job_list", json!({})).await.unwrap(),
        "bash-1 [bash] completed — npm test"
    );
    jobs.close().await;
}

#[tokio::test]
async fn a_read_consumes_new_output_and_ends_with_the_status() {
    let jobs = registry();
    let (id, release) = held(&jobs, "build", "compiling\n");
    // Until the producer's first poll, the read is empty.
    let first = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let text = call(&jobs, "job_output", json!({ "job_id": "bash-1" }))
                .await
                .unwrap();
            if text.starts_with("compiling") {
                return text;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("output");
    assert_eq!(
        first,
        format!("compiling\n[status: running]\n{STILL_RUNNING}")
    );
    assert_eq!(
        call(&jobs, "job_output", json!({ "job_id": "bash-1" }))
            .await
            .unwrap(),
        format!("(no new output)\n[status: running]\n{STILL_RUNNING}")
    );
    // Past the wake budget, a running job's read gives no guidance.
    for _ in 0..crate::jobs::MAX_CONSECUTIVE_WAKES {
        assert!(jobs.spend_wake());
    }
    assert!(!jobs.spend_wake());
    assert_eq!(
        call(&jobs, "job_output", json!({ "job_id": "bash-1" }))
            .await
            .unwrap(),
        "(no new output)\n[status: running]"
    );
    release.send(()).unwrap();
    // A wait returns the settled job; its read claims the report.
    let text = call(
        &jobs,
        "job_output",
        json!({ "job_id": "bash-1", "wait": true, "timeout_ms": 10_000 }),
    )
    .await
    .unwrap();
    assert_eq!(text, "(no new output)\n[status: completed, exit code: 0]");
    assert!(jobs.get(id).unwrap().reported);
    jobs.close().await;
}

#[tokio::test]
async fn kill_stops_a_running_job_and_reports_a_finished_one() {
    let jobs = registry();
    let (id, _release) = held(&jobs, "sleep 30", "started");
    // The producer runs before the kill, so the stop reaches it.
    tokio::time::timeout(Duration::from_secs(10), async {
        while !call(&jobs, "job_output", json!({ "job_id": "bash-1" }))
            .await
            .unwrap()
            .starts_with("started")
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("started");
    assert_eq!(
        call(
            &jobs,
            "job_kill",
            json!({ "job_id": "bash-1", "reason": "done" })
        )
        .await
        .unwrap(),
        "requested cancellation of job bash-1"
    );
    settle(&jobs, id).await;
    assert_eq!(
        call(&jobs, "job_kill", json!({ "job_id": "bash-1" }))
            .await
            .unwrap(),
        "job bash-1 had already finished [status: killed, signal: SIGKILL]"
    );
    jobs.close().await;
}

#[tokio::test]
async fn unknown_and_blank_ids_explain_themselves() {
    let jobs = registry();
    for name in ["job_output", "job_kill"] {
        assert_eq!(
            call(&jobs, name, json!({ "job_id": "bash-9" })).await,
            Err("unknown job bash-9; use job_list to find a background job id.".into())
        );
        assert_eq!(
            call(&jobs, name, json!({ "job_id": "" })).await,
            Err("invalid job_id: expected a non-empty string, got \"\"".into())
        );
    }
    jobs.reserve_ordinals("bash", 2).unwrap();
    assert_eq!(
        call(&jobs, "job_output", json!({ "job_id": "bash-2" })).await,
        Err("job bash-2 ran in an earlier process of this session and did not survive it".into())
    );
    jobs.close().await;
}

#[test]
fn a_completing_job_reads_as_running() {
    assert_eq!(public_status(JobStatus::Completing), "running");
    assert_eq!(public_status(JobStatus::Killed), "killed");
}
