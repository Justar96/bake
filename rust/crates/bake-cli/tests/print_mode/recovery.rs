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

/// Make a FIFO at `path`, and hold it open for reading and writing, so a
/// reader's `open` returns at once and its `read` blocks until the holder
/// writes.
#[cfg(target_os = "linux")]
fn held_fifo(path: &Path) -> std::fs::File {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path");
    // SAFETY: `name` is a NUL-terminated string that outlives the call.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("hold the FIFO")
}

/// The run-journal records of the session file at `path`, as their kinds,
/// and the run's id.
#[cfg(target_os = "linux")]
fn journal(path: &Path) -> (Vec<String>, Option<String>) {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut kinds = Vec::new();
    let mut run = None;
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry["customType"] == "bake.run.v1" {
            kinds.push(
                entry["data"]["event"]["kind"]
                    .as_str()
                    .unwrap_or("?")
                    .to_owned(),
            );
            run = entry["data"]["runId"].as_str().map(str::to_owned);
        }
    }
    (kinds, run)
}

/// A `bake -p --recoverable` started in `workspace`, killed with `SIGKILL`
/// once its session file records the dispatch of the model's tool call and
/// `ready` holds; killed and reaped on drop whatever fails first.
#[cfg(target_os = "linux")]
struct KilledPrint {
    child: std::process::Child,
}

#[cfg(target_os = "linux")]
impl KilledPrint {
    fn run(workspace: &Workspace, args: &[&str], ready: impl Fn() -> bool) -> (PathBuf, String) {
        let mut command = workspace.command(args, false);
        command.stdout(Stdio::null()).stderr(Stdio::null());
        let mut killed = Self {
            child: command.spawn().expect("bake starts"),
        };
        let deadline = Instant::now() + RUN_TIMEOUT;
        let found = loop {
            if let Some(file) = workspace.session_files().first() {
                let (kinds, run) = journal(file);
                if kinds.iter().any(|kind| kind == "dispatch") && ready() {
                    break (file.clone(), run.expect("the run's id"));
                }
            }
            if let Some(status) = killed.child.try_wait().expect("bake status") {
                panic!("bake exited ({status}) before its dispatch");
            }
            assert!(Instant::now() < deadline, "bake never dispatched the call");
            std::thread::sleep(Duration::from_millis(10));
        };
        killed.child.kill().expect("kill bake");
        let status = killed.child.wait().expect("reap bake");
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{status}");
        found
    }
}

#[cfg(target_os = "linux")]
impl Drop for KilledPrint {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `bake -p --recoverable` killed while its `read` of a FIFO blocks, after
/// the call's dispatch is recorded: `--resume-run` reruns the `Safe` read
/// once, now of a regular file, and finishes with one more request.
#[cfg(target_os = "linux")]
#[test]
fn a_killed_print_run_reruns_its_safe_read_once() {
    let server = Server::start_sequence(vec![
        Reply::tool_call("read", &json!({"path": "pipe"})),
        Reply::sse(),
    ]);
    let workspace = Workspace::new("recover-killed-read", server.port);
    let pipe = workspace.project().join("pipe");
    let holder = held_fifo(&pipe);
    let (file, run_id) = KilledPrint::run(
        &workspace,
        &["-p", "--recoverable", "Read the pipe"],
        || true,
    );
    drop(holder);
    let (kinds, _) = journal(&file);
    assert_eq!(
        kinds.last().map(String::as_str),
        Some("dispatch"),
        "{kinds:?}"
    );
    assert_eq!(server.requests().len(), 1);

    std::fs::remove_file(&pipe).expect("remove the FIFO");
    std::fs::write(&pipe, "piped text").expect("a regular file");
    let resumed = workspace.run(
        &[
            "-p",
            "--session",
            file.to_str().unwrap(),
            "--resume-run",
            &run_id,
        ],
        None,
    );
    assert_eq!(resumed.status.code(), Some(0), "{}", text(&resumed.stderr));
    assert_eq!(text(&resumed.stdout), "Hello from the loop.\n");
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let sent = requests[1].body.to_string();
    assert_eq!(sent.matches("piped text").count(), 1, "{sent}");
    let session = bake_coding_agent::session::SessionManager::open(&file, None, None).unwrap();
    let results = session
        .entries()
        .iter()
        .filter(|entry| entry.message_role() == Some("toolResult"))
        .count();
    assert_eq!(results, 1, "the read ran once more");
}

/// `bake -p --recoverable` killed while its `bash` call holds after its
/// effect: `--resume-run` refuses to continue, needing reconciliation,
/// with no request, and the effect stays counted once.
#[cfg(target_os = "linux")]
#[test]
fn a_killed_print_run_refuses_to_rerun_its_effect() {
    let command = "echo $$ > shell.pid; echo effect >> effects.txt; \
        read line < gate; echo exited > exited.txt";
    let server = Server::start_sequence(vec![
        Reply::tool_call("bash", &json!({ "command": command })),
        Reply::sse(),
    ]);
    let workspace = Workspace::new("recover-killed-effect", server.port);
    let project = workspace.project();
    let mut gate = held_fifo(&project.join("gate"));
    let effects = project.join("effects.txt");
    let (file, run_id) = KilledPrint::run(
        &workspace,
        &[
            "-p",
            "--permission",
            "danger-full-access",
            "--recoverable",
            "Run it",
        ],
        || std::fs::read_to_string(&effects).is_ok_and(|text| text == "effect\n"),
    );
    // The shell outlives the killed `bake` in its own session; release it
    // and wait for it to end, or kill its group.
    gate.write_all(b"\n").expect("release the shell");
    let exited = project.join("exited.txt");
    let deadline = Instant::now() + RUN_TIMEOUT;
    while !exited.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if !exited.exists() {
        if let Some(pid) = std::fs::read_to_string(project.join("shell.pid"))
            .ok()
            .and_then(|pid| pid.trim().parse::<libc::pid_t>().ok())
        {
            // SAFETY: `kill` takes plain values.
            unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
        panic!("the released shell did not end");
    }
    drop(gate);
    assert_eq!(server.requests().len(), 1);

    let resumed = workspace.run(
        &[
            "-p",
            "--session",
            file.to_str().unwrap(),
            "--resume-run",
            &run_id,
        ],
        None,
    );
    assert_eq!(resumed.status.code(), Some(1), "{}", text(&resumed.stderr));
    let stderr = text(&resumed.stderr);
    assert!(stderr.contains("external effect"), "{stderr}");
    assert_eq!(server.requests().len(), 1, "no request after the refusal");
    assert_eq!(std::fs::read_to_string(&effects).unwrap(), "effect\n");
    let (kinds, _) = journal(&file);
    assert_eq!(
        kinds.last().map(String::as_str),
        Some("dispatch"),
        "{kinds:?}"
    );
}
