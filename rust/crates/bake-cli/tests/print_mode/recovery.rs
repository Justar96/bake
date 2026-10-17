//! The real CLI selects a durable run without duplicating its prompt or budget.

use super::*;

fn events(out: &Output) -> Vec<Value> {
    text(&out.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON event"))
        .collect()
}

#[test]
fn completed_run_reopens_without_another_provider_request() {
    let server = Server::start(Reply::sse());
    let workspace = Workspace::new("recover-completed", server.port);
    let first = workspace.run(&["--mode", "json", "--recoverable", "Say hi"], None);
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr));
    let first_events = events(&first);
    let started = first_events
        .iter()
        .position(|event| event["type"] == "recoverable_run_started")
        .expect("durable run identity");
    let first_message = first_events
        .iter()
        .position(|event| event["type"] == "message_start")
        .expect("execution message");
    assert!(started < first_message);
    let run_id = first_events[started]["runId"].as_str().unwrap();
    assert_eq!(
        first_events.last().unwrap()["outcome"]["kind"],
        "finished_turn"
    );
    let files = workspace.session_files();
    assert_eq!(files.len(), 1);
    let resumed = workspace.run(
        &[
            "--mode",
            "json",
            "--session",
            files[0].to_str().unwrap(),
            "--resume-run",
            run_id,
        ],
        None,
    );
    assert_eq!(resumed.status.code(), Some(0), "{}", text(&resumed.stderr));
    let resumed_events = events(&resumed);
    let result = resumed_events.last().expect("recorded outcome");
    assert_eq!(result["type"], "recoverable_run_result");
    assert_eq!(result["runId"], run_id);
    assert_eq!(result["outcome"]["kind"], "finished_turn");
    assert_eq!(server.requests().len(), 1);
    let session = bake_coding_agent::session::SessionManager::open(&files[0], None, None).unwrap();
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| entry.message_role() == Some("user"))
            .count(),
        1
    );
}

#[test]
fn text_mode_reports_the_recoverable_identity_and_session() {
    let server = Server::start(Reply::sse());
    let workspace = Workspace::new("recover-text", server.port);
    let out = workspace.run(&["-p", "--recoverable", "Say hi"], None);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("Run ID: "));
    assert!(text(&out.stderr).contains("Session: "));
    assert!(!text(&out.stdout).is_empty());
}

#[test]
fn completed_selected_run_prints_its_own_answer_after_later_work() {
    let mut later = Reply::sse();
    later.body = later.body.replace("Hello from", "Later answer from");
    let server = Server::start_sequence(vec![Reply::sse(), later]);
    let workspace = Workspace::new("recover-selected-answer", server.port);
    let first = workspace.run(&["--mode", "json", "--recoverable", "First question"], None);
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr));
    let first_events = events(&first);
    let run_id = first_events.last().unwrap()["runId"].as_str().unwrap();
    let files = workspace.session_files();
    let path = files[0].to_str().unwrap();
    let later = workspace.run(&["-p", "--session", path, "Later question"], None);
    assert_eq!(later.status.code(), Some(0), "{}", text(&later.stderr));
    assert_eq!(text(&later.stdout), "Later answer from the loop.\n");
    let selected = workspace.run(&["-p", "--session", path, "--resume-run", run_id], None);
    assert_eq!(
        selected.status.code(),
        Some(0),
        "{}",
        text(&selected.stderr)
    );
    assert_eq!(text(&selected.stdout), "Hello from the loop.\n");
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn exhausted_run_cannot_refund_its_model_allowance() {
    let server = Server::start(Reply::read_call());
    let workspace = Workspace::new("recover-budget", server.port);
    std::fs::write(workspace.project().join("notes.txt"), "hello").unwrap();
    let first = workspace.run(
        &[
            "--mode",
            "json",
            "--recoverable",
            "--max-model-calls",
            "1",
            "Read notes",
        ],
        None,
    );
    assert_eq!(first.status.code(), Some(1), "{}", text(&first.stderr));
    let first_events = events(&first);
    let result = first_events.last().unwrap();
    assert_eq!(
        result["outcome"],
        json!({"kind":"budget_exhausted","limit":"model_calls"})
    );
    let run_id = result["runId"].as_str().unwrap();
    let files = workspace.session_files();
    let resumed = workspace.run(
        &[
            "--mode",
            "json",
            "--session",
            files[0].to_str().unwrap(),
            "--resume-run",
            run_id,
        ],
        None,
    );
    assert_eq!(resumed.status.code(), Some(1));
    assert_eq!(
        events(&resumed).last().unwrap()["outcome"],
        result["outcome"]
    );
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn invalid_recovery_input_starts_no_request_or_session() {
    let server = Server::start(Reply::sse());
    let workspace = Workspace::new("recover-invalid", server.port);
    for args in [
        vec!["-p", "--recoverable", "--no-session", "go"],
        vec!["-p", "--recoverable", "one", "two"],
        vec!["-p", "--recoverable"],
        vec!["-p", "--resume-run", "missing"],
        vec!["-p", "--resume-run", "missing", "--session", "absent.jsonl"],
    ] {
        let out = workspace.run(&args, None);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{args:?}: {}",
            text(&out.stderr)
        );
    }
    assert!(server.requests().is_empty());
    assert!(workspace.session_files().is_empty());
    assert!(!workspace.project().join("absent.jsonl").exists());
}

#[test]
fn resuming_rejects_piped_prompts_and_replacement_limits() {
    let server = Server::start(Reply::sse());
    let workspace = Workspace::new("recover-input", server.port);
    let first = workspace.run(&["--mode", "json", "--recoverable", "Say hi"], None);
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr));
    let first_events = events(&first);
    let run_id = first_events.last().unwrap()["runId"].as_str().unwrap();
    let files = workspace.session_files();
    let common = [
        "-p",
        "--session",
        files[0].to_str().unwrap(),
        "--resume-run",
        run_id,
    ];
    let piped = workspace.run(&common, Some("another prompt"));
    assert_eq!(piped.status.code(), Some(1));
    assert!(text(&piped.stderr).contains("takes no new prompt"));
    let mut with_limit = common.to_vec();
    with_limit.extend(["--max-model-calls", "100"]);
    let replacement = workspace.run(&with_limit, None);
    assert_eq!(replacement.status.code(), Some(1));
    assert!(text(&replacement.stderr).contains("restores the recorded budget"));
    assert_eq!(server.requests().len(), 1);
}
