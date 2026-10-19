//! End-to-end RPC mode: the built `bake-rs` with `--mode rpc`, driven by a
//! small test client over its pipes. Each test owns a temporary Bake home
//! and project directory; no model request is made, so nothing reaches the
//! network and no key is needed.
//!
//! The client stands in for Pi's `RpcClient`
//! (`packages/coding-agent/src/modes/rpc/rpc-client.ts`), which these
//! cases follow: `test/rpc-client-clear-queue.test.ts`,
//! `test/rpc-client-clone.test.ts`, and
//! `test/rpc-client-process-exit.test.ts`.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long one record may take before the test fails.
const TIMEOUT: Duration = Duration::from_secs(60);

/// A temporary Bake home and project directory, removed on drop.
struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "bake-rs-rpc-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).expect("home");
        std::fs::create_dir_all(root.join("project")).expect("project");
        std::fs::create_dir_all(root.join("user")).expect("user home");
        // A route that answers nothing: these tests send no prompt, and a
        // session needs a model with a key to start.
        let models = json!({
            "providers": {
                "loop": {
                    "baseUrl": "http://127.0.0.1:1/v1",
                    "api": "openai-responses",
                    "apiKey": "test-key",
                    "models": [{ "id": "loop-model", "name": "Loop" }],
                },
            },
        });
        std::fs::write(root.join("home/models.json"), models.to_string()).expect("models.json");
        Self { root }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bake-rs"));
        command
            .args(args)
            .current_dir(self.root.join("project"))
            .env("BAKE_HOME", self.root.join("home"))
            .env_remove("DSH_HOME")
            .env_remove("CLIPROXYAPI_API_KEY")
            .env("HOME", self.root.join("user"))
            .env("USERPROFILE", self.root.join("user"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("PI_") {
                command.env_remove(key);
            }
        }
        command
    }

    /// Starts `bake-rs --mode rpc` with a client on its pipes.
    fn rpc(&self, args: &[&str]) -> Client {
        let mut full = vec!["--mode", "rpc", "--model", "loop-model"];
        full.extend_from_slice(args);
        Client::start(self.command(&full))
    }

    /// Runs `bake-rs` to completion with nothing on standard input.
    fn run(&self, args: &[&str]) -> Output {
        let mut child = self.command(args).spawn().expect("bake-rs starts");
        drop(child.stdin.take());
        child.wait_with_output().expect("bake-rs output")
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A test client: it writes commands as JSONL records and reads the
/// records the agent writes, splitting on line feeds only.
struct Client {
    child: Child,
    input: Option<std::process::ChildStdin>,
    records: Arc<Mutex<Vec<Value>>>,
    stderr: Arc<Mutex<String>>,
    reader: Option<std::thread::JoinHandle<()>>,
    errors: Option<std::thread::JoinHandle<()>>,
}

impl Client {
    fn start(mut command: Command) -> Self {
        let mut child = command.spawn().expect("bake-rs starts");
        let input = child.stdin.take();
        let records = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(String::new()));
        let reader = child.stdout.take().map(|stdout| {
            let records = Arc::clone(&records);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                let mut line = Vec::new();
                while reader.read_until(b'\n', &mut line).unwrap_or(0) > 0 {
                    let text = String::from_utf8_lossy(&line).trim().to_owned();
                    line.clear();
                    if text.is_empty() {
                        continue;
                    }
                    let record: Value = serde_json::from_str(&text)
                        .unwrap_or_else(|error| panic!("record {text:?}: {error}"));
                    records.lock().expect("records").push(record);
                }
            })
        });
        let errors = child.stderr.take().map(|pipe| {
            let stderr = Arc::clone(&stderr);
            std::thread::spawn(move || {
                let mut text = String::new();
                let mut pipe = pipe;
                let _ = pipe.read_to_string(&mut text);
                stderr.lock().expect("stderr").push_str(&text);
            })
        });
        Self {
            child,
            input,
            records,
            stderr,
            reader,
            errors,
        }
    }

    fn send(&mut self, command: Value) {
        let line = format!("{command}\n");
        if let Some(input) = &mut self.input {
            input.write_all(line.as_bytes()).expect("write a command");
            input.flush().expect("flush the command");
        }
    }

    /// Writes a command, reporting a pipe whose reader is gone rather than
    /// failing the test. Used by the signal case, which is Unix-only.
    #[cfg(unix)]
    fn try_send(&mut self, command: Value) -> bool {
        let line = format!("{command}\n");
        match &mut self.input {
            Some(input) => input
                .write_all(line.as_bytes())
                .and_then(|()| input.flush())
                .is_ok(),
            None => false,
        }
    }

    fn records(&self) -> Vec<Value> {
        self.records.lock().expect("records").clone()
    }

    /// Whether a response to `id` arrives within `wait`.
    #[cfg(unix)]
    fn has_response(&self, id: &str, wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            if self
                .records()
                .iter()
                .any(|record| record["type"] == json!("response") && record["id"] == json!(id))
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// The response to `id`, waited for.
    fn response(&self, id: &str) -> Value {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let found = self
                .records()
                .into_iter()
                .find(|record| record["type"] == json!("response") && record["id"] == json!(id));
            if let Some(found) = found {
                return found;
            }
            assert!(
                Instant::now() < deadline,
                "no response to {id}; records: {:?}, stderr: {}",
                self.records(),
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn stderr(&self) -> String {
        self.stderr.lock().expect("stderr").clone()
    }

    /// Closes standard input, as a client asking for shutdown does, and
    /// returns the exit status.
    fn close(&mut self) -> i32 {
        drop(self.input.take());
        self.wait()
    }

    fn wait(&mut self) -> i32 {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.child.try_wait().expect("status") {
                Some(status) => {
                    if let Some(reader) = self.reader.take() {
                        let _ = reader.join();
                    }
                    if let Some(errors) = self.errors.take() {
                        let _ = errors.join();
                    }
                    return status_code(status);
                }
                None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                None => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    panic!(
                        "bake-rs did not exit; records: {:?}, stderr: {}",
                        self.records(),
                        self.stderr()
                    );
                }
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        drop(self.input.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(unix)]
fn status_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(1)
}

#[cfg(not(unix))]
fn status_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

// Pi `docs/rpc.md`, "Correlate commands and responses": a command's
// response repeats its id, and the session is the one the flags chose.
#[test]
fn answers_commands_on_its_pipes_and_shuts_down_on_end_of_input() {
    let workspace = Workspace::new("state");
    let mut client = workspace.rpc(&["--name", "rpc-session"]);
    client.send(json!({ "id": "1", "type": "get_state" }));
    let state = client.response("1");
    assert_eq!(state["success"], json!(true), "{state}");
    assert_eq!(state["data"]["model"]["id"], json!("loop-model"));
    assert_eq!(state["data"]["isStreaming"], json!(false));
    assert_eq!(state["data"]["sessionName"], json!("rpc-session"));
    assert!(
        state["data"]["sessionFile"]
            .as_str()
            .is_some_and(|file| file.ends_with(".jsonl")),
        "{state}"
    );
    // Pi `docs/rpc.md`, "Shutdown": closing stdin shuts the session down.
    assert_eq!(client.close(), 0);
}

// Pi `test/rpc-client-clear-queue.test.ts`: the `clear_queue` command
// answers with both queues.
#[test]
fn sends_the_clear_queue_rpc_command() {
    let workspace = Workspace::new("clear-queue");
    let mut client = workspace.rpc(&["--no-session"]);
    client.send(json!({ "id": "clear", "type": "clear_queue" }));
    assert_eq!(
        client.response("clear"),
        json!({
            "id": "clear",
            "type": "response",
            "command": "clear_queue",
            "success": true,
            "data": { "steering": [], "followUp": [] },
        })
    );
    assert_eq!(client.close(), 0);
}

// Pi `test/rpc-client-clone.test.ts`: the `clone` command is answered. A
// session whose file holds nothing yet cannot be cloned, which is the
// refusal `/clone` gives interactively.
#[test]
fn sends_the_clone_rpc_command() {
    let workspace = Workspace::new("clone");
    let mut client = workspace.rpc(&[]);
    client.send(json!({ "id": "clone", "type": "clone" }));
    assert_eq!(
        client.response("clone"),
        json!({
            "id": "clone",
            "type": "response",
            "command": "clone",
            "success": false,
            "error": "This session has not been saved yet. Send a message before cloning or forking it.",
        })
    );
    assert_eq!(client.close(), 0);
}

// Pi `docs/rpc.md`, "Errors": a malformed line and an unknown command
// each answer once, and the loop goes on.
#[test]
fn answers_a_malformed_record_and_an_unknown_command() {
    let workspace = Workspace::new("errors");
    let mut client = workspace.rpc(&["--no-session"]);
    if let Some(input) = &mut client.input {
        input.write_all(b"{ not json\n").expect("write");
        input.flush().expect("flush");
    }
    client.send(json!({ "id": "u", "type": "set_theme", "name": "dark" }));
    let unknown = client.response("u");
    assert_eq!(unknown["command"], json!("set_theme"));
    assert_eq!(unknown["error"], json!("Unknown command: set_theme"));
    let parse = client
        .records()
        .into_iter()
        .find(|record| record["command"] == json!("parse"))
        .expect("a parse failure");
    assert_eq!(parse["success"], json!(false));
    assert_eq!(parse.get("id"), None, "{parse}");
    client.send(json!({ "id": "s", "type": "get_state" }));
    assert_eq!(client.response("s")["success"], json!(true));
    assert_eq!(client.close(), 0);
}

// Pi `test/rpc-client-process-exit.test.ts`: a request in flight when the
// agent process exits gets no response, and the client sees the status.
// Here the stop request is `SIGTERM`, whose status is Pi's 143.
#[cfg(unix)]
#[test]
fn a_request_in_flight_when_the_process_exits_gets_no_response() {
    let workspace = Workspace::new("exit");
    let mut client = workspace.rpc(&["--no-session"]);
    client.send(json!({ "id": "first", "type": "get_state" }));
    client.response("first");
    // Pi's exit status for `SIGTERM`, after the session's shutdown.
    assert_eq!(
        unsafe { libc::kill(client.child.id() as i32, libc::SIGTERM) },
        0
    );
    assert_eq!(client.wait(), 143);
    // The protocol stream has ended with the process, so a request sent
    // afterwards is never answered; a client must report the exit instead
    // of waiting.
    client.try_send(json!({ "id": "second", "type": "get_commands" }));
    assert!(!client.has_response("second", Duration::from_millis(500)));
}

// Pi `src/main.ts`: RPC mode refuses `@file` arguments, since standard
// input carries the protocol.
#[test]
fn refuses_file_arguments_and_the_print_mode_run_limits() {
    let workspace = Workspace::new("flags");
    for (args, expected) in [
        (
            vec!["--mode", "rpc", "@notes.txt"],
            "@file arguments are not supported in RPC mode",
        ),
        (
            vec!["--mode", "rpc", "--max-model-calls", "1"],
            "--max-model-calls and --run-timeout-ms require print mode",
        ),
        (
            vec!["--mode", "rpc", "--recoverable"],
            "--recoverable and --resume-run require print mode",
        ),
    ] {
        let out = workspace.run(&args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(expected), "{args:?}: {stderr}");
        assert!(out.stdout.is_empty(), "{args:?}: {:?}", out.stdout);
    }
}

// Bake: `--mode` names rpc among its values.
#[test]
fn an_invalid_mode_lists_rpc() {
    let workspace = Workspace::new("mode");
    let out = workspace.run(&["--mode", "acp"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Valid values: text, json, rpc"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
