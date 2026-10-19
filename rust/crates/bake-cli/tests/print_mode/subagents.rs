//! Subagents through the built binary over the loopback model server, with
//! `subagents` on and two models so each request's route can be seen. The
//! other print-mode tests show the tools are absent by default.

#[cfg(unix)]
use super::*;

/// Turns subagents on, with the workspace's models: the parent's and a
/// second one for children.
#[cfg(unix)]
fn two_models(workspace: &Workspace, port: u16) {
    std::fs::write(
        workspace.home().join("settings.json"),
        json!({ "subagents": true }).to_string(),
    )
    .expect("settings.json");
    let models = json!({
        "providers": {
            "loop": {
                "baseUrl": format!("http://127.0.0.1:{port}/v1"),
                "api": "openai-responses",
                "apiKey": "test-key",
                "models": [
                    { "id": "loop-model", "name": "Loop" },
                    { "id": "loop-child", "name": "Loop child" },
                ],
            },
        },
    });
    std::fs::write(workspace.home().join("models.json"), models.to_string()).expect("models.json");
}

/// Whether a request is a child's: its task carries the return guidance.
#[cfg(unix)]
fn is_child(request: &Request) -> bool {
    transcript(request)
        .iter()
        .any(|line| line.contains("Your parent agent id is"))
}

// A foreground child runs on the route the parent chose, in its own file
// under the sessions directory's `subagents`, and its answer is the
// parent's tool result; `--continue` resumes the parent, never the child.
#[cfg(unix)]
#[test]
fn a_foreground_subagent_runs_on_its_route_and_answers_the_call() {
    let server = Server::start_sequence(vec![
        Reply::tool_call(
            "subagent",
            &json!({
                "description": "say hello",
                "prompt": "Say hello.",
                "provider": "loop",
                "model": "loop-child",
                "run_in_background": false,
            }),
        ),
        Reply::sse(),
        Reply::sse(),
    ]);
    let workspace = Workspace::new("subagent-foreground", server.port);
    two_models(&workspace, server.port);
    let out = workspace.run(
        &["-p", "--permission", "danger-full-access", "Delegate"],
        None,
    );
    assert_eq!(text(&out.stderr), "");
    assert_eq!(out.status.code(), Some(0));
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].body["model"], "loop-model");
    let declared: Vec<&str> = requests[0].body["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(declared.contains(&"list_subagent_models"), "{declared:?}");
    assert!(is_child(&requests[1]));
    assert_eq!(requests[1].body["model"], "loop-child");
    let child_tools: Vec<&str> = requests[1].body["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(child_tools.contains(&"send_message"), "{child_tools:?}");
    // A child starts with its parent's default tools, `codemode` included.
    assert!(child_tools.contains(&"codemode"), "{child_tools:?}");
    assert!(
        !child_tools.contains(&"subagent"),
        "a child cannot delegate"
    );
    assert_eq!(sent_tool_output(&requests[2]), "Hello from the loop.");

    let files = workspace.session_files();
    assert_eq!(files.len(), 2, "{files:?}");
    let (child, parent): (Vec<&PathBuf>, Vec<&PathBuf>) = files
        .iter()
        .partition(|file| file.parent().is_some_and(|dir| dir.ends_with("subagents")));
    let child_entries = lines(child[0]);
    assert_eq!(
        child_entries[0]["parentSession"],
        json!(parent[0].to_string_lossy())
    );
    assert_eq!(child_entries[1]["customType"], "bake.subagent.child");
    assert!(
        child_entries
            .iter()
            .any(|entry| entry["type"] == "model_change" && entry["modelId"] == "loop-child")
    );

    let again = workspace.run(&["-p", "--continue", "Again"], None);
    assert_eq!(again.status.code(), Some(0), "{}", text(&again.stderr));
    let requests = server.requests();
    assert!(!is_child(&requests[3]), "--continue resumed the parent");
    assert_eq!(requests[3].body["model"], "loop-model");
}

// A background child's settlement reaches the parent once, in its run or
// in the turn print mode waits for, and is logged once.
#[cfg(unix)]
#[test]
fn print_mode_waits_for_a_background_subagent_and_reports_it_once() {
    let server = Server::start_sequence(vec![
        Reply::tool_call(
            "subagent",
            &json!({ "description": "say hello", "prompt": "Say hello." }),
        ),
        Reply::sse(),
    ]);
    let workspace = Workspace::new("subagent-background", server.port);
    two_models(&workspace, server.port);
    let out = workspace.run(
        &["-p", "--permission", "danger-full-access", "Delegate"],
        None,
    );
    assert_eq!(text(&out.stderr), "");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), "Hello from the loop.\n");
    let requests = server.requests();
    let child: Vec<&Request> = requests
        .iter()
        .filter(|request| is_child(request))
        .collect();
    assert_eq!(child.len(), 1);
    assert_eq!(
        child[0].body["model"], "loop-model",
        "the child inherits the route"
    );
    let with_notice = requests
        .iter()
        .filter(|request| {
            transcript(request)
                .iter()
                .any(|line| line.starts_with("user: Background subagent "))
        })
        .count();
    assert!(with_notice >= 1, "the parent read the notice");
    let parent = workspace
        .session_files()
        .into_iter()
        .find(|file| !file.parent().is_some_and(|dir| dir.ends_with("subagents")))
        .expect("parent file");
    let notices: Vec<Value> = lines(&parent)
        .into_iter()
        .filter(|entry| entry["customType"] == "bake.subagent.notice")
        .collect();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0]["details"]["stopReason"], "completed");
    assert_eq!(notices[0]["details"]["runId"], 1);
}
