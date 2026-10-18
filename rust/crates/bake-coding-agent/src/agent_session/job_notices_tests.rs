//! What a resumed session learns from its file about background jobs:
//! every recorded id is reserved, and a job started on the current branch
//! and reported nowhere gets one lost notice.

use bake_agent::{AgentToolResult, AgentToolUpdateCallback};
use bake_ai::{TextContent, ToolResultMessage, UserContent, UserContentBlock};
use serde_json::{Value, json};

use super::*;
use crate::jobs::JobLimits;
use crate::owned_work::WorkGate;
use crate::session::{NewSessionOptions, SessionManager};
use crate::test_support::TempDir;
use crate::tools::bash::{BashToolOptions, create_bash_tool};
use crate::tools::job_tools::create_job_tool_definitions;

fn tool_result(session: &mut SessionManager, name: &str, result: AgentToolResult) -> String {
    session
        .append_message(ToolResultMessage {
            tool_call_id: format!("call-{name}"),
            tool_name: name.into(),
            content: result.content,
            details: result.details,
            usage: None,
            nested_calls: None,
            is_error: result.is_error,
            timestamp: 0,
            duration_ms: None,
        })
        .expect("tool result appended")
}

#[tokio::test]
async fn a_completed_bash_job_observed_by_its_output_is_not_lost_after_reopen() {
    if crate::tools::bash::shell::get_shell_config(None).is_err() {
        eprintln!("skipping job recovery shell fixture: bash unavailable");
        return;
    }
    for wait in [false, true] {
        let dir = TempDir::new("job-observation");
        let mut file = SessionManager::create(
            &dir.path().to_string_lossy(),
            Some(dir.path()),
            NewSessionOptions::default(),
        )
        .expect("persisted session");
        file.append_message(bake_ai::UserMessage {
            content: UserContent::Text("run the job".into()),
            timestamp: 0,
        })
        .expect("conversation persisted");
        let path = file.session_file().expect("file").to_owned();
        let jobs = JobRegistry::new(WorkGate::default(), JobLimits::default());
        let bash = create_bash_tool(
            dir.path(),
            BashToolOptions {
                jobs: Some(jobs.clone()),
                ..BashToolOptions::default()
            },
        );
        let started = (bash.execute)(
            "start".into(),
            json!({
                "command": "printf done", "run_in_background": true
            }),
            None,
            AgentToolUpdateCallback::ignore(),
        )
        .await
        .expect("bash started");
        tool_result(&mut file, "bash", started);
        let id = jobs.id("bash-1").expect("id");
        if !wait {
            jobs.wait(id, std::time::Duration::from_secs(10), None)
                .await
                .expect("job ended");
        }
        let output = create_job_tool_definitions(&jobs)
            .into_iter()
            .find(|definition| definition.name() == "job_output")
            .expect("output tool")
            .tool;
        let result = (output.execute)(
            "output".into(),
            json!({
                "job_id": "bash-1", "wait": wait, "timeout_ms": 10_000
            }),
            None,
            AgentToolUpdateCallback::ignore(),
        )
        .await
        .expect("job read");
        assert_eq!(
            result.content,
            vec![UserContentBlock::Text(TextContent::new(
                "done\n[status: completed, exit code: 0]"
            ))]
        );
        tool_result(&mut file, "job_output", result);
        jobs.close().await;
        file.close_writer();
        let reopened = SessionManager::open(&path, None, None).expect("reopened");
        let resumed = JobRegistry::new(WorkGate::default(), JobLimits::default());
        let recovered = recover(&reopened, &resumed);
        let reserved = resumed.id("bash-1");
        resumed.close().await;
        assert!(
            recovered.is_empty(),
            "observed terminal job recovered as lost: {recovered:?}"
        );
        assert!(reserved.is_ok(), "the completed id must remain reserved");
    }
}

fn session() -> SessionManager {
    SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
        .expect("session")
}

fn started(session: &mut SessionManager, id: &str, label: &str) -> String {
    session
        .append_message(ToolResultMessage {
            tool_call_id: format!("call-{id}"),
            tool_name: "bash".into(),
            content: vec![UserContentBlock::Text(TextContent::new(format!(
                "started background job {id}"
            )))],
            details: Some(json!({ BACKGROUND_JOB_DETAIL: { "id": id, "label": label } })),
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 0,
            duration_ms: None,
        })
        .expect("appended")
}

fn reported(session: &mut SessionManager, id: &str) {
    session
        .append_custom_message_entry(
            NOTICE_TYPE,
            UserContent::Text("done".into()),
            true,
            Some(json!({ "jobId": id, "status": "completed" })),
        )
        .expect("appended");
}

fn ids(notices: &[CustomMessage]) -> Vec<Value> {
    notices
        .iter()
        .map(|notice| notice.details.as_ref().expect("details")["jobId"].clone())
        .collect()
}

fn observed(session: &mut SessionManager, name: &str, details: Value, is_error: bool) {
    tool_result(
        session,
        name,
        AgentToolResult {
            details: Some(json!({ "backgroundJobObservation": details })),
            is_error,
            ..AgentToolResult::default()
        },
    );
}

#[tokio::test]
async fn only_valid_terminal_observations_on_the_selected_branch_acknowledge_jobs() {
    let mut file = session();
    for number in 1..=6 {
        started(&mut file, &format!("bash-{number}"), "job");
    }
    observed(
        &mut file,
        "job_output",
        json!({ "id": "bash-1", "status": "completed" }),
        false,
    );
    observed(
        &mut file,
        "job_output",
        json!({ "id": "bash-2", "status": "running" }),
        false,
    );
    observed(
        &mut file,
        "job_list",
        json!({ "id": "bash-3", "status": "completed" }),
        false,
    );
    observed(
        &mut file,
        "job_output",
        json!({ "id": "bash-4", "status": "completed" }),
        true,
    );
    observed(
        &mut file,
        "job_output",
        json!({ "id": "bash-5", "status": 1 }),
        false,
    );
    observed(
        &mut file,
        "job_kill",
        json!({ "id": "bash-6", "status": "killed" }),
        false,
    );
    // An observation can remain after an imported or compacted prefix;
    // its id must not be reissued even without the original start.
    observed(
        &mut file,
        "job_output",
        json!({ "id": "bash-9", "status": "failed" }),
        false,
    );
    let jobs = JobRegistry::new(WorkGate::default(), JobLimits::default());
    let recovered = ids(&recover(&file, &jobs));
    let reserved = jobs.id("bash-9");
    jobs.close().await;
    assert_eq!(
        recovered,
        [
            json!("bash-2"),
            json!("bash-3"),
            json!("bash-4"),
            json!("bash-5")
        ]
    );
    assert!(reserved.is_ok());

    let mut file = session();
    let branch = started(&mut file, "bash-1", "job");
    observed(
        &mut file,
        "job_output",
        json!({ "id": "bash-1", "status": "completed" }),
        false,
    );
    file.branch(&branch).expect("earlier branch");
    let jobs = JobRegistry::new(WorkGate::default(), JobLimits::default());
    let recovered = ids(&recover(&file, &jobs));
    jobs.close().await;
    assert_eq!(
        recovered,
        [json!("bash-1")],
        "another branch has not delivered this branch's result"
    );
}

#[tokio::test]
async fn unreported_jobs_on_the_branch_are_lost_once_and_their_ids_reserved() {
    let mut file = session();
    let fork = started(&mut file, "bash-1", "npm test");
    started(&mut file, "bash-2", "cargo build");
    reported(&mut file, "bash-2");
    started(&mut file, "bash-3", "sleep 60");
    // A job on another branch is not this branch's to report, but its id
    // is still never issued again.
    file.branch(&fork).expect("branched");
    started(&mut file, "bash-4", "make");

    let jobs = JobRegistry::new(WorkGate::default(), JobLimits::default());
    let notices = recover(&file, &jobs);
    assert_eq!(ids(&notices), [json!("bash-1"), json!("bash-4")]);
    let text = match &notices[0].content {
        UserContent::Blocks(blocks) => match &blocks[0] {
            UserContentBlock::Text(text) => text.text.clone(),
            _ => String::new(),
        },
        UserContent::Text(text) => text.clone(),
    };
    assert!(
        text.starts_with("background job bash-1 (bash: npm test) belongs to an earlier process"),
        "{text}"
    );
    assert_eq!(notices[0].details.as_ref().unwrap()["status"], "lost");
    let reserved = jobs.id("bash-4").expect("reserved");
    assert!(matches!(
        jobs.get(reserved),
        Err(crate::jobs::JobError::EarlierProcess(_))
    ));
    assert!(jobs.id("bash-5").is_err());
    jobs.close().await;
}

#[tokio::test]
async fn malformed_and_foreign_records_are_skipped() {
    let mut file = session();
    started(&mut file, "bash-01", "padded");
    started(&mut file, "deploy-1", "other kind");
    file.append_custom_message_entry(
        "other.notice",
        UserContent::Text("x".into()),
        true,
        Some(json!({ "jobId": "bash-2" })),
    )
    .expect("appended");
    started(&mut file, "bash-2", "kept");
    let jobs = JobRegistry::new(WorkGate::default(), JobLimits::default());
    assert_eq!(ids(&recover(&file, &jobs)), [json!("bash-2")]);
    jobs.close().await;
}

fn fact(file: &mut SessionManager, data: Value) -> String {
    file.append_custom_entry(JOURNAL_TYPE, Some(data)).unwrap()
}

#[tokio::test]
async fn terminal_facts_survive_branching_but_observations_belong_to_their_branch() {
    let mut file = session();
    let fork = fact(
        &mut file,
        json!({
            "version": 1, "phase": "started", "jobId": "bash-1",
            "kind": "bash", "label": "one effect", "status": "running"
        }),
    );
    fact(
        &mut file,
        json!({
            "version": 1, "phase": "terminal", "jobId": "bash-1",
            "kind": "bash", "label": "one effect", "status": "completed",
            "detail": "exit code: 0"
        }),
    );
    // The immutable first outcome wins over a contradictory later entry.
    fact(
        &mut file,
        json!({
            "version": 1, "phase": "terminal", "jobId": "bash-1",
            "kind": "bash", "label": "one effect", "status": "failed"
        }),
    );
    reported(&mut file, "bash-1");
    let jobs = JobRegistry::new(WorkGate::default(), JobLimits::default());
    assert!(recover(&file, &jobs).is_empty());
    jobs.close().await;
    file.branch(&fork).unwrap();
    let jobs = JobRegistry::new(WorkGate::default(), JobLimits::default());
    let recovered = recover(&file, &jobs);
    jobs.close().await;
    assert_eq!(ids(&recovered), [json!("bash-1")]);
    assert_eq!(
        recovered[0].details.as_ref().unwrap()["status"],
        "completed"
    );
    let text = bake_ai::utils::text::user_content_text(&recovered[0].content, "");
    assert!(text.contains("exit code: 0"));
    assert!(text.contains("unavailable"));
}

#[tokio::test]
async fn malformed_facts_and_notices_do_not_acknowledge_or_invent_an_outcome() {
    let mut file = session();
    for ordinal in 1..=5 {
        let id = format!("bash-{ordinal}");
        fact(
            &mut file,
            json!({
                "version": 1, "phase": "started", "jobId": id,
                "kind": "bash", "label": "job", "status": "running"
            }),
        );
        let mut broken = json!({
            "version": 1, "phase": "terminal", "jobId": id,
            "kind": "bash", "label": "job", "status": "completed"
        });
        match ordinal {
            1 => broken["version"] = json!(2),
            2 => broken["kind"] = json!("deploy"),
            3 => broken["status"] = json!("running"),
            4 => broken["label"] = json!(null),
            _ => broken["jobId"] = json!("bash-05"),
        }
        fact(&mut file, broken);
        file.append_custom_message_entry(
            NOTICE_TYPE,
            UserContent::Text("untrusted text claims completion".into()),
            true,
            Some(json!({ "jobId": id, "status": "running" })),
        )
        .unwrap();
    }
    // A valid orphan terminal fact reserves its id even without a start.
    fact(
        &mut file,
        json!({
            "version": 1, "phase": "terminal", "jobId": "bash-9",
            "kind": "bash", "label": "orphan", "status": "failed"
        }),
    );
    let jobs = JobRegistry::new(WorkGate::default(), JobLimits::default());
    let recovered = recover(&file, &jobs);
    assert!(jobs.id("bash-9").is_ok());
    jobs.close().await;
    assert_eq!(recovered.len(), 5);
    assert!(
        recovered
            .iter()
            .all(|notice| notice.details.as_ref().unwrap()["status"] == "lost")
    );
}
