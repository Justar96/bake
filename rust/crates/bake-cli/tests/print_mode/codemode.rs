//! `codemode` through the built binary: `--tools read,bash,codemode`
//! selects it, the model's script calls `read` and `bash` in parallel, the
//! result the model receives is the script's output, and the session file
//! keeps one tool result with the nested calls in its details. With
//! `codemode.mode` `only`, the callable tools leave the request.

use super::*;

const SCRIPT: &str = r#"const [notes, listing] = await Promise.all([
    tools.read({ path: "notes.txt" }),
    tools.bash({ command: "wc -l < notes.txt" }),
]);
console.log("read", notes.split("\n").length - 1);
return { lines: Number(listing.output.trim()), exit: listing.exit_code };"#;

fn declared(request: &Request) -> Vec<String> {
    request.body["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect()
}

fn description<'a>(request: &'a Request, name: &str) -> &'a str {
    request.body["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == name)
        .and_then(|tool| tool["description"].as_str())
        .expect("declared")
}

fn workspace(name: &str, port: u16) -> Workspace {
    let workspace = Workspace::new(name, port);
    std::fs::write(workspace.project().join("notes.txt"), "one\ntwo\nthree\n").expect("notes.txt");
    workspace
}

#[test]
fn print_mode_runs_a_codemode_script_over_read_and_bash() {
    let server = Server::start_sequence(vec![
        Reply::tool_call("codemode", &json!({ "code": SCRIPT })),
        Reply::sse(),
    ]);
    let workspace = workspace("codemode", server.port);
    let out = workspace.run(
        &[
            "-p",
            "--permission",
            "danger-full-access",
            "--tools",
            "read,bash,codemode",
            "Count the lines",
        ],
        None,
    );
    assert_eq!(text(&out.stderr), "");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), "Hello from the loop.\n");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(declared(&requests[0]), ["read", "bash", "codemode"]);
    assert!(
        description(&requests[0], "bash").ends_with(
            "\n\nCodemode: `tools.bash(args)` resolves to `{ output, truncated, full_output_path?, exit_code, wall_time_seconds }`."
        ),
        "{}",
        description(&requests[0], "bash")
    );
    assert!(description(&requests[0], "codemode").starts_with("Run JavaScript"));
    let sent = sent_tool_output(&requests[1]);
    let (header, output) = sent.split_once("Output:\n\n").expect("a script header");
    assert!(header.starts_with("Script completed\nWall time "), "{sent}");
    assert_eq!(
        output,
        "{\"lines\":3,\"exit\":0}\n<console_output>\nread 3\n</console_output>"
    );

    let entries = lines(&workspace.session_files()[0]);
    let results: Vec<&Value> = entries
        .iter()
        .filter(|entry| entry["message"]["role"] == "toolResult")
        .map(|entry| &entry["message"])
        .collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["toolName"], "codemode");
    assert_eq!(results[0]["isError"], false);
    let calls: Vec<(&str, &str)> = results[0]["details"]["calls"]
        .as_array()
        .expect("calls")
        .iter()
        .map(|call| {
            (
                call["name"].as_str().unwrap_or_default(),
                call["status"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(calls, [("read", "ok"), ("bash", "ok")]);
}

#[test]
fn codemode_only_leaves_the_callable_tools_out_of_the_request() {
    let server = Server::start(Reply::sse());
    let workspace = workspace("codemode-only", server.port);
    std::fs::write(
        workspace.home().join("settings.json"),
        json!({ "codemode": { "mode": "only" } }).to_string(),
    )
    .expect("settings.json");
    let out = workspace.run(
        &[
            "-p",
            "--no-session",
            "--tools",
            "read,bash,codemode",
            "Count the lines",
        ],
        None,
    );
    assert_eq!(text(&out.stderr), "");
    assert_eq!(out.status.code(), Some(0));
    let requests = server.requests();
    assert_eq!(declared(&requests[0]), ["codemode"]);
    let codemode = description(&requests[0], "codemode");
    assert!(codemode.contains("### `read`"), "{codemode}");
    assert!(codemode.contains("### `bash`"), "{codemode}");
}
