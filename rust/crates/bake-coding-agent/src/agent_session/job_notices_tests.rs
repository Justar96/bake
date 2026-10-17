//! What a resumed session learns from its file about background jobs:
//! every recorded id is reserved, and a job started on the current branch
//! and reported nowhere gets one lost notice.

use bake_ai::{TextContent, ToolResultMessage, UserContent, UserContentBlock};
use serde_json::{Value, json};

use super::*;
use crate::jobs::JobLimits;
use crate::owned_work::WorkGate;
use crate::session::{NewSessionOptions, SessionManager};

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
        text.starts_with(
            "background job bash-1 (bash: npm test) did not survive the end of the earlier process"
        ),
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
