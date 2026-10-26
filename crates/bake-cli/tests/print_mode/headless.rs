//! Running `bake` from a script or another agent: `--output-format json`
//! and `stream-json`, the result object a caller reads and continues the
//! session from, and piped standard input that stays open and silent.
//! The standard-input tests wait on the clock on purpose: the first-byte
//! wait they check is a duration.

use std::process::ChildStdin;

use super::*;

/// The one JSON line a `--output-format json` run prints.
fn result_of(out: &Output) -> Value {
    let stdout = text(&out.stdout);
    let mut lines = stdout.lines();
    let result = serde_json::from_str(lines.next().expect("a result line")).expect("JSON");
    assert_eq!(lines.next(), None, "only the result: {stdout}");
    result
}

// An agent runs a prompt, reads the answer and the session id from one
// object, and continues the same session with that id.
#[test]
fn the_json_output_format_prints_one_result_to_continue_from() {
    let server = Server::start(Reply::sse());
    let workspace = Workspace::new("result", server.port);
    let first = workspace.run(&["-p", "--output-format", "json", "Say hi"], None);
    assert_eq!(text(&first.stderr), "");
    assert_eq!(first.status.code(), Some(0));
    let result = result_of(&first);
    assert_eq!(result["type"], "result");
    assert_eq!(result["subtype"], "success");
    assert_eq!(result["isError"], false);
    assert_eq!(result["result"], "Hello from the loop.");
    assert_eq!(result["error"], Value::Null);
    assert_eq!(result["stopReason"], "stop");
    assert_eq!(result["model"], "loop/loop-model");
    assert_eq!(result["numTurns"], 1);
    assert_eq!(result["modelCalls"], 1);
    assert_eq!(result["usage"]["totalTokens"], 48);
    let files = workspace.session_files();
    assert_eq!(files.len(), 1);
    let header = &lines(&files[0])[0];
    let id = result["sessionId"].as_str().expect("a session id");
    assert_eq!(header["id"], id);
    assert_eq!(
        std::fs::canonicalize(result["sessionFile"].as_str().expect("a file")).expect("file"),
        std::fs::canonicalize(&files[0]).expect("file")
    );

    // `--output-format` alone chooses print mode, as `--mode` does.
    let second = workspace.run(&["--output-format", "json", "--session", id, "Again"], None);
    assert_eq!(second.status.code(), Some(0), "{}", text(&second.stderr));
    let result = result_of(&second);
    assert_eq!(result["sessionId"], id);
    // This invocation's usage, not the session's.
    assert_eq!(result["usage"]["totalTokens"], 48);
    assert_eq!(transcript(&server.requests()[1]).len(), 4);
    assert_eq!(workspace.session_files(), files);
}

#[test]
fn the_stream_json_output_format_ends_with_the_result() {
    let server = Server::start(Reply::sse());
    let workspace = Workspace::new("stream-json", server.port);
    let out = workspace.run(
        &["--output-format", "stream-json", "--no-session", "Say hi"],
        None,
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let events: Vec<Value> = text(&out.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON line"))
        .collect();
    let types: Vec<&str> = events
        .iter()
        .map(|event| event["type"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(types[0], "session");
    assert_eq!(types[types.len() - 2..], ["agent_settled", "result"]);
    let result = &events[events.len() - 1];
    assert_eq!(result["result"], "Hello from the loop.");
    assert_eq!(result["sessionFile"], Value::Null);
    assert_eq!(result["sessionId"], events[0]["id"]);
    assert!(
        text(
            &workspace
                .run(&["--output-format", "json", "--mode", "json", "hi"], None)
                .stderr
        )
        .contains("--output-format and --mode choose the same thing")
    );
}

// A failure, at startup or from the provider, is a result object too, and
// standard error still says what happened.
#[test]
fn failures_are_results_with_their_errors() {
    let server = Server::start(Reply {
        status: 401,
        content_type: "application/json",
        body: r#"{"error":{"message":"bad key"}}"#.into(),
    });
    let workspace = Workspace::new("result-failure", server.port);
    let out = workspace.run(&["--output-format", "json", "--no-session", "Say hi"], None);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        "loop API error (401): {\"message\":\"bad key\"}\n"
    );
    let result = result_of(&out);
    assert_eq!(result["subtype"], "error");
    assert_eq!(result["isError"], true);
    assert_eq!(
        result["error"],
        "loop API error (401): {\"message\":\"bad key\"}"
    );

    let out = workspace.run(
        &["--output-format", "json", "--model", "nope/missing", "hi"],
        None,
    );
    assert_eq!(out.status.code(), Some(1));
    let expected = format!(
        "Model \"nope/missing\" not found. See:\n  {} models",
        docs_command()
    );
    assert_eq!(text(&out.stderr), format!("Error: {expected}\n"));
    let result = result_of(&out);
    assert_eq!(result["subtype"], "error");
    assert_eq!(result["error"], expected);
    assert_eq!(result["sessionId"], Value::Null);
    assert_eq!(server.requests().len(), 1);
}

/// Starts `bake` with a piped standard input the test owns.
fn spawn_piped(workspace: &Workspace, args: &[&str]) -> (std::process::Child, ChildStdin) {
    let mut child = workspace.command(args, true).spawn().expect("bake starts");
    let stdin = child.stdin.take().expect("stdin");
    (child, stdin)
}

/// Writes `text` after `delay` and closes the pipe.
fn write_late(mut stdin: ChildStdin, delay: Duration, text: &'static str) -> JoinHandle<()> {
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        let _ = stdin.write_all(text.as_bytes());
    })
}

// A caller that leaves standard input open and never writes it, as a
// harness's inherited pipe does, still gets its answer, with a warning.
#[test]
fn a_silent_open_pipe_beside_a_prompt_is_not_waited_for() {
    let server = Server::start(Reply::sse());
    let workspace = Workspace::new("silent-stdin", server.port);
    let args = ["-p", "--no-session", "Say hi"];
    let (child, held) = spawn_piped(&workspace, &args);
    let out = finish(child, &args);
    drop(held);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "Hello from the loop.\n");
    assert_eq!(
        text(&out.stderr),
        "Warning: Nothing arrived on standard input within 1000 ms, so the prompt is sent without it. Pass - to wait for piped input, or redirect standard input from /dev/null to skip the wait.\n"
    );
    assert_eq!(transcript(&server.requests()[0])[1], "user: Say hi");
}

// Input that comes later than the wait is read when it is the only
// prompt, or when `-` asks for it.
#[test]
fn late_input_is_read_without_a_prompt_or_with_a_dash() {
    let server = Server::start(Reply::sse());
    let workspace = Workspace::new("late-stdin", server.port);
    let late = Duration::from_millis(1500);
    for (args, expected) in [
        (&["-p", "--no-session"][..], "user: late text"),
        (
            &["-p", "-", "--no-session", " tail"][..],
            "user: late text tail",
        ),
    ] {
        let (child, stdin) = spawn_piped(&workspace, args);
        let writer = write_late(stdin, late, "late text");
        let out = finish(child, args);
        writer.join().expect("writer");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&out.stderr)
        );
        assert_eq!(text(&out.stderr), "", "{args:?}");
        let requests = server.requests();
        assert_eq!(
            transcript(requests.last().expect("a request"))[1],
            expected,
            "{args:?}"
        );
    }
}
