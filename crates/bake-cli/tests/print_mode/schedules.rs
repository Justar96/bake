//! Schedules through the built binary over the loopback model server, with
//! `schedules` on. The other print-mode tests show the tools are absent by
//! default.

#[cfg(unix)]
use super::*;

/// The tool results a request carries, as their text.
#[cfg(unix)]
fn outputs(request: &Request) -> Vec<String> {
    request.body["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| match &item["output"] {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .collect()
}

/// The tool names a request declares.
#[cfg(unix)]
fn declared(request: &Request) -> Vec<String> {
    request.body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect()
}

// A reminder made in one run is listed by the next, which resumes the
// session.
#[cfg(unix)]
#[test]
fn a_reminder_is_kept_by_the_session_and_listed_after_resume() {
    let server = Server::start_sequence(vec![
        Reply::tool_call(
            "schedule_create",
            &json!({ "prompt": "check the deploy", "after_seconds": 86400 }),
        ),
        Reply::sse(),
        Reply::tool_call("schedule_list", &json!({})),
        Reply::sse(),
    ]);
    let workspace = Workspace::new("schedules-resume", server.port);
    std::fs::write(
        workspace.home().join("settings.json"),
        json!({ "schedules": true }).to_string(),
    )
    .expect("settings.json");
    let out = workspace.run(&["-p", "Remind me tomorrow"], None);
    assert_eq!(text(&out.stderr), "");
    assert_eq!(out.status.code(), Some(0));
    let out = workspace.run(&["-p", "--continue", "What is scheduled?"], None);
    assert_eq!(text(&out.stderr), "");
    assert_eq!(out.status.code(), Some(0));
    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    let tools = declared(&requests[0]);
    for name in ["schedule_create", "schedule_list", "schedule_delete"] {
        assert!(tools.iter().any(|tool| tool == name), "{tools:?}");
    }
    let created = outputs(&requests[1]);
    assert!(
        created
            .iter()
            .any(|line| line.contains("\"id\":\"schedule-1\"")
                && line.contains("\"state\":\"scheduled\"")),
        "{created:#?}"
    );
    let listed = outputs(&requests[3]);
    let last = listed.last().expect("the list result");
    assert!(
        last.contains("\"id\":\"schedule-1\"") && last.contains("check the deploy"),
        "{listed:#?}"
    );
}

/// The schedule changes of a session file, as their operations.
#[cfg(unix)]
fn operations(path: &Path) -> Vec<String> {
    lines(path)
        .into_iter()
        .filter(|entry| entry["customType"] == "bake.schedule.change")
        .filter_map(|entry| entry["data"]["operation"].as_str().map(str::to_owned))
        .collect()
}

// D39 through the binary: `bake -p` killed with SIGKILL after a reminder's
// durable dispatch and before any turn delivered it, while the server
// holds the turn's request. The next `--continue` run delivers nothing:
// the reminder is lost, never repeated.
#[cfg(unix)]
#[test]
fn a_reminder_dispatched_before_a_kill_is_never_delivered_again() {
    use super::subagents::{Answer, HoldServer, kill_when};
    let answer: Answer = Arc::new(|request| {
        let sent = transcript(request);
        if sent.iter().any(|line| line.ends_with("Anything due?")) {
            return Some(Reply::sse());
        }
        if sent
            .iter()
            .any(|line| line.starts_with("function_call_output"))
        {
            return None;
        }
        Some(Reply::tool_call(
            "schedule_create",
            &json!({ "prompt": "stretch", "after_seconds": 1 }),
        ))
    });
    let server = HoldServer::start(answer);
    let workspace = Workspace::new("schedules-kill", server.port);
    std::fs::write(
        workspace.home().join("settings.json"),
        json!({ "schedules": true }).to_string(),
    )
    .expect("settings.json");
    kill_when(&workspace, &["-p", "Remind me in a second"], || {
        workspace
            .session_files()
            .first()
            .is_some_and(|file| operations(file).contains(&"dispatch".to_owned()))
    });
    let file = workspace.session_files()[0].clone();
    assert_eq!(operations(&file), ["create", "dispatch"]);
    let before = server.requests().len();
    assert_eq!(before, 2, "the second request was held");

    let out = workspace.run(&["-p", "--continue", "Anything due?"], None);
    assert_eq!(text(&out.stderr), "");
    assert_eq!(out.status.code(), Some(0));
    let requests = server.requests();
    assert_eq!(requests.len(), before + 1, "one request, no reminder turn");
    for request in &requests[before..] {
        assert!(
            !transcript(request)
                .iter()
                .any(|line| line.contains("[SCHEDULE REMINDER")),
            "{:#?}",
            transcript(request)
        );
    }
    assert_eq!(operations(&file), ["create", "dispatch"]);
    assert!(
        !lines(&file)
            .iter()
            .any(|entry| entry["customType"] == "bake.schedule.reminder"),
        "the reminder was delivered after the kill"
    );
}
