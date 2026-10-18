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
