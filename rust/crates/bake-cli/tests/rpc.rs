//! End-to-end RPC mode: the built `bake` with `--mode rpc`, driven by a
//! small test client over its pipes. Each test owns a temporary Bake home
//! and project directory. A case that runs a turn owns a loopback model
//! server answering `POST /v1/responses` with the OpenAI Responses streams
//! in `fixtures/`; nothing reaches the network and no real key is needed.
//!
//! The client stands in for Pi's `RpcClient`
//! (`packages/coding-agent/src/modes/rpc/rpc-client.ts`), which these
//! cases follow: `test/rpc-client-clear-queue.test.ts`,
//! `test/rpc-client-clone.test.ts`, and
//! `test/rpc-client-process-exit.test.ts`. The rest are scope 13's proofs
//! through the real binary: D24 approvals answered, refused, and left
//! unanswered, each against the session file; `abort` and read-only
//! commands answered while a turn runs; and shutdown on end of input and
//! on `SIGTERM`, `SIGINT`, and `SIGHUP` with no process left behind. The
//! approval and shutdown cases run on Linux, the platform 0.4.0 qualifies
//! (D47).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long one record may take before the test fails.
const TIMEOUT: Duration = Duration::from_secs(60);

/// The text answer the model server streams.
const TEXT_SSE: &str = include_str!("fixtures/openai-responses-text.sse");

/// What the model server answers one request with.
#[derive(Clone)]
enum Reply {
    /// An event stream, written whole.
    Stream(String),
    /// Nothing, with the connection kept open until the client hangs up or
    /// the server stops: a model that never answers.
    Hold,
}

impl Reply {
    fn text() -> Self {
        // The fixture ends with one newline; its last event needs its blank
        // line.
        Self::Stream(format!("{TEXT_SSE}\n"))
    }

    /// One function call of `name` with `arguments`.
    fn tool_call(name: &str, arguments: &Value) -> Self {
        let item = json!({"id": "fc_1", "type": "function_call", "status": "completed", "call_id": "call_1", "name": name, "arguments": arguments.to_string()});
        let events = [
            json!({"type": "response.created", "response": {"id": "resp_call", "object": "response", "created_at": 1_760_000_000, "status": "in_progress", "model": "loop-model", "output": []}}),
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"id": "fc_1", "type": "function_call", "status": "in_progress", "call_id": "call_1", "name": name, "arguments": ""}}),
            json!({"type": "response.function_call_arguments.done", "item_id": "fc_1", "output_index": 0, "arguments": arguments.to_string()}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": item}),
            json!({"type": "response.completed", "response": {"id": "resp_call", "object": "response", "created_at": 1_760_000_000, "status": "completed", "model": "loop-model", "output": [item], "usage": {"input_tokens": 40, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 8, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 48}}}),
        ];
        Self::Stream(
            events
                .into_iter()
                .enumerate()
                .map(|(sequence, mut event)| {
                    event["sequence_number"] = json!(sequence);
                    format!(
                        "event: {}\ndata: {event}\n\n",
                        event["type"].as_str().unwrap_or_default()
                    )
                })
                .collect(),
        )
    }
}

/// A loopback model server answering its requests with `replies` in turn,
/// repeating the last, each connection on its own thread; stopped and
/// joined on drop.
struct ModelServer {
    port: u16,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ModelServer {
    fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("address").port();
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (requests, stop) = (Arc::clone(&requests), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut connections = Vec::new();
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((socket, _)) => {
                            let (replies, requests, stop) =
                                (replies.clone(), Arc::clone(&requests), Arc::clone(&stop));
                            connections.push(std::thread::spawn(move || {
                                answer(socket, &replies, &requests, &stop);
                            }));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
                for connection in connections {
                    let _ = connection.join();
                }
            })
        };
        Self {
            port,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    /// How many model requests have arrived.
    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for ModelServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Reads one request's head and body, then answers it with the reply its
/// order selects.
fn answer(mut socket: TcpStream, replies: &[Reply], requests: &AtomicUsize, stop: &AtomicBool) {
    let _ = socket.set_nonblocking(false);
    let _ = socket.set_read_timeout(Some(Duration::from_millis(50)));
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let deadline = Instant::now() + TIMEOUT;
    let complete = |buffer: &[u8]| {
        let head_end = buffer.windows(4).position(|window| window == b"\r\n\r\n")?;
        let head = String::from_utf8_lossy(&buffer[..head_end]).to_lowercase();
        let length = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        (buffer.len() >= head_end + 4 + length).then_some(())
    };
    while complete(&buffer).is_none() {
        if stop.load(Ordering::SeqCst) || Instant::now() > deadline {
            return;
        }
        match socket.read(&mut chunk) {
            Ok(0) => return,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            Err(_) => {}
        }
    }
    let seen = requests.fetch_add(1, Ordering::SeqCst);
    match &replies[seen.min(replies.len() - 1)] {
        Reply::Stream(body) => {
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes());
            let _ = socket.flush();
        }
        Reply::Hold => loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            match socket.read(&mut chunk) {
                Ok(0) => return,
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => return,
            }
        },
    }
}

/// The custom entry type of a D24 approval record.
#[cfg(target_os = "linux")]
const APPROVAL: &str = bake_coding_agent::permissions::APPROVAL_CUSTOM_TYPE;

/// The session file's entries.
#[cfg(target_os = "linux")]
fn entries(file: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// The D24 approval records in `entries`, in order.
#[cfg(target_os = "linux")]
fn approvals(entries: &[Value]) -> Vec<Value> {
    entries
        .iter()
        .filter(|entry| entry["type"] == json!("custom") && entry["customType"] == json!(APPROVAL))
        .map(|entry| entry["data"].clone())
        .collect()
}

/// A temporary Bake home and project directory, removed on drop.
struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn new(name: &str) -> Self {
        Self::with_model(name, 1)
    }

    /// A workspace whose `loop` route is the model server on `port`.
    fn with_model(name: &str, port: u16) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "bake-rpc-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).expect("home");
        std::fs::create_dir_all(root.join("project")).expect("project");
        std::fs::create_dir_all(root.join("user")).expect("user home");
        // Port 1 answers nothing: a test that sends no prompt still needs
        // a model with a key to start a session.
        let models = json!({
            "providers": {
                "loop": {
                    "baseUrl": format!("http://127.0.0.1:{port}/v1"),
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_bake"));
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

    /// Starts `bake --mode rpc` with a client on its pipes.
    fn rpc(&self, args: &[&str]) -> Client {
        let mut full = vec!["--mode", "rpc", "--model", "loop-model"];
        full.extend_from_slice(args);
        Client::start(self.command(&full))
    }

    /// Runs `bake` to completion with nothing on standard input.
    fn run(&self, args: &[&str]) -> Output {
        let mut child = self.command(args).spawn().expect("bake starts");
        drop(child.stdin.take());
        child.wait_with_output().expect("bake output")
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
        let mut child = command.spawn().expect("bake starts");
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

    /// The first record that `found` accepts, waited for.
    fn wait_for(&self, what: &str, found: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(record) = self.records().into_iter().find(|record| found(record)) {
                return record;
            }
            assert!(
                Instant::now() < deadline,
                "no {what}; records: {:?}, stderr: {}",
                self.records(),
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The records of type `kind`.
    fn of_kind(&self, kind: &str) -> Vec<Value> {
        self.records()
            .into_iter()
            .filter(|record| record["type"] == json!(kind))
            .collect()
    }

    /// The session file, as `get_state` names it.
    fn session_file(&mut self) -> PathBuf {
        self.send(json!({ "id": "file", "type": "get_state" }));
        let state = self.response("file");
        PathBuf::from(
            state["data"]["sessionFile"]
                .as_str()
                .unwrap_or_else(|| panic!("no session file: {state}")),
        )
    }

    /// Closes standard input, as a client asking for shutdown does, and
    /// returns the exit code ([`exit_code`]).
    fn close(&mut self) -> Option<i32> {
        drop(self.input.take());
        self.wait()
    }

    fn wait(&mut self) -> Option<i32> {
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
                    return exit_code(status);
                }
                None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                None => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    panic!(
                        "bake did not exit; records: {:?}, stderr: {}",
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

/// The code `bake` exited with. Pi's signal handlers exit with 128 plus
/// the signal's number, so a process a signal killed must not read as one:
/// that fails here instead.
fn exit_code(status: std::process::ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), None, "a signal killed bake: {status}");
    }
    status.code()
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
    assert_eq!(client.close(), Some(0));
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
    assert_eq!(client.close(), Some(0));
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
    assert_eq!(client.close(), Some(0));
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
    assert_eq!(client.close(), Some(0));
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
    assert_eq!(client.wait(), Some(143));
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

/// Runs one prompt whose model answer is a `write` of `notes.txt` that
/// asks to escalate the read-only session to `workspace-write`, and waits
/// for the approval request. Returns the client, the request's id, the
/// file the write would create, and the session file.
#[cfg(target_os = "linux")]
fn ask_for_a_write(workspace: &Workspace) -> (Client, String, PathBuf, PathBuf) {
    let mut client = workspace.rpc(&["--sandbox", "read-only", "--approval", "ask"]);
    let file = client.session_file();
    client.send(json!({ "id": "p", "type": "prompt", "message": "write it" }));
    assert_eq!(
        client.response("p")["data"],
        json!({ "disposition": "started" })
    );
    let request = client.wait_for("the approval request", |record| {
        record["type"] == json!("extension_ui_request")
    });
    assert_eq!(request["method"], json!("confirm"), "{request}");
    assert_eq!(request["approval"]["toolName"], json!("write"), "{request}");
    let id = request["id"].as_str().expect("an id").to_owned();
    (client, id, workspace.root.join("project/notes.txt"), file)
}

/// The model's two answers for [`ask_for_a_write`]: the write, then text.
#[cfg(target_os = "linux")]
fn write_then_text() -> ModelServer {
    ModelServer::start(vec![
        Reply::tool_call(
            "write",
            &json!({
                "path": "notes.txt",
                "content": "approved",
                "sandbox_permissions": "workspace-write",
                "justification": "to write the notes",
            }),
        ),
        Reply::text(),
    ])
}

/// The asked and decided records of the one approval in `file`, and the
/// index in its entries of the `write` call's result, if it has one.
#[cfg(target_os = "linux")]
fn decision(file: &std::path::Path) -> (Vec<Value>, Vec<Value>, Option<usize>) {
    let entries = entries(file);
    let approvals = approvals(&entries);
    let decided = entries.iter().position(|entry| {
        entry["type"] == json!("custom")
            && entry["customType"] == json!(APPROVAL)
            && entry["data"]["kind"] == json!("decided")
    });
    let result = entries.iter().position(|entry| {
        entry["type"] == json!("message") && entry["message"]["role"] == json!("toolResult")
    });
    if let (Some(decided), Some(result)) = (decided, result) {
        assert!(
            decided < result,
            "the result was written before the decision"
        );
    }
    (approvals, entries, result)
}

// Scope 13 and D24 through the binary: `confirmed: true` lets the one call
// run, and the session file holds `asked` then `decided` with
// `allowed-once`, written before the call's result.
#[cfg(target_os = "linux")]
#[test]
fn an_approved_write_runs_after_its_decision_is_recorded() {
    let server = write_then_text();
    let workspace = Workspace::with_model("approve", server.port);
    let (mut client, id, notes, file) = ask_for_a_write(&workspace);
    assert!(!notes.exists(), "the write ran before its answer");
    client.send(json!({ "type": "extension_ui_response", "id": id, "confirmed": true }));
    client.wait_for("the settled run", |record| {
        record["type"] == json!("agent_settled")
    });
    assert_eq!(client.close(), Some(0));
    assert_eq!(
        std::fs::read_to_string(&notes).expect("the approved write"),
        "approved"
    );
    let (approvals, entries, result) = decision(&file);
    assert_eq!(approvals.len(), 2, "{approvals:?}");
    assert_eq!(approvals[0]["kind"], json!("asked"));
    assert_eq!(approvals[0]["toolName"], json!("write"));
    assert_eq!(approvals[1]["outcome"], json!("allowed-once"));
    assert_eq!(approvals[0]["id"], approvals[1]["id"]);
    let result = &entries[result.expect("a tool result")]["message"];
    assert_eq!(result["isError"], json!(false), "{result}");
    assert_eq!(server.requests(), 2);
}

// Scope 13 and D24: `confirmed: false` refuses the call, which writes
// nothing, and the refusal is recorded before the call's result.
#[cfg(target_os = "linux")]
#[test]
fn a_denied_write_is_refused_and_recorded() {
    let server = write_then_text();
    let workspace = Workspace::with_model("deny", server.port);
    let (mut client, id, notes, file) = ask_for_a_write(&workspace);
    client.send(json!({ "type": "extension_ui_response", "id": id, "confirmed": false }));
    client.wait_for("the settled run", |record| {
        record["type"] == json!("agent_settled")
    });
    assert_eq!(client.close(), Some(0));
    assert!(!notes.exists(), "the denied write ran");
    let (approvals, entries, result) = decision(&file);
    assert_eq!(approvals.len(), 2, "{approvals:?}");
    assert_eq!(approvals[1]["kind"], json!("decided"));
    assert_eq!(approvals[1]["outcome"], json!("rejected"));
    let result = &entries[result.expect("a tool result")]["message"];
    assert_eq!(result["isError"], json!(true), "{result}");
}

// Scope 13 and D24: a client that disconnects without answering leaves the
// call refused, never run, and the process exits 0 once the session has
// recorded the cancellation.
#[cfg(target_os = "linux")]
#[test]
fn a_client_that_disconnects_unanswered_leaves_the_write_refused() {
    let server = write_then_text();
    let workspace = Workspace::with_model("disconnect", server.port);
    let (mut client, _, notes, file) = ask_for_a_write(&workspace);
    assert_eq!(client.close(), Some(0));
    assert!(!notes.exists(), "the unanswered write ran");
    let (approvals, _, _) = decision(&file);
    assert_eq!(approvals.len(), 2, "{approvals:?}");
    assert_eq!(approvals[0]["kind"], json!("asked"));
    assert_eq!(approvals[1]["outcome"], json!("cancelled"));
}

// Scope 13 and Pi `rpc-mode.ts` `handleInputLine`: while a turn waits on a
// model that never answers, `get_state`, `get_messages`, and
// `get_session_stats` are answered, and `abort` ends the turn.
#[test]
fn abort_and_read_only_commands_are_answered_while_a_turn_runs() {
    let server = ModelServer::start(vec![Reply::Hold]);
    let workspace = Workspace::with_model("busy", server.port);
    let mut client = workspace.rpc(&[]);
    client.send(json!({ "id": "p", "type": "prompt", "message": "never answered" }));
    assert_eq!(
        client.response("p")["data"],
        json!({ "disposition": "started" })
    );
    let deadline = Instant::now() + TIMEOUT;
    while server.requests() == 0 {
        assert!(Instant::now() < deadline, "no model request");
        std::thread::sleep(Duration::from_millis(10));
    }
    client.send(json!({ "id": "s", "type": "get_state" }));
    let state = client.response("s");
    assert_eq!(state["data"]["isStreaming"], json!(true), "{state}");
    client.send(json!({ "id": "m", "type": "get_messages" }));
    assert_eq!(client.response("m")["success"], json!(true));
    client.send(json!({ "id": "st", "type": "get_session_stats" }));
    assert_eq!(client.response("st")["success"], json!(true));
    assert!(client.of_kind("agent_settled").is_empty(), "the turn ended");
    client.send(json!({ "id": "a", "type": "abort" }));
    assert_eq!(client.response("a")["success"], json!(true));
    client.wait_for("the settled run", |record| {
        record["type"] == json!("agent_settled")
    });
    client.send(json!({ "id": "s2", "type": "get_state" }));
    assert_eq!(client.response("s2")["data"]["isStreaming"], json!(false));
    assert_eq!(client.close(), Some(0));
}

/// A process's start time, field 22 of `/proc/<pid>/stat`, which tells a
/// process apart from a later one given the same id; `None` once it has
/// ended or been reaped. A zombie has ended.
#[cfg(target_os = "linux")]
fn started_at(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name, in parentheses, may hold spaces; fields follow it.
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    (fields.first() != Some(&"Z")).then(|| fields.get(19).map(|field| (*field).to_owned()))?
}

/// The processes whose command line holds `marker`, and every process
/// descended from them, each with its start time.
#[cfg(target_os = "linux")]
fn marked_tree(marker: &str) -> Vec<(u32, String)> {
    let mut parents = Vec::new();
    let mut tree = Vec::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let parent = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(1))
            .and_then(|field| field.parse::<u32>().ok());
        if String::from_utf8_lossy(&cmdline).contains(marker) {
            tree.push(pid);
        }
        if let Some(parent) = parent {
            parents.push((pid, parent));
        }
    }
    let mut index = 0;
    while index < tree.len() {
        let parent = tree[index];
        for (pid, of) in &parents {
            if *of == parent && !tree.contains(pid) {
                tree.push(*pid);
            }
        }
        index += 1;
    }
    tree.into_iter()
        .filter_map(|pid| started_at(pid).map(|start| (pid, start)))
        .collect()
}

/// How a client ends RPC mode while the model's `bash` call runs, and the
/// status that ending exits with: end of input exits 0, and each stop
/// signal exits with Pi's status for it.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug)]
enum Ending {
    EndOfInput,
    Signal(i32),
}

/// Starts a turn whose `bash` call runs a long `sleep`, ends RPC mode the
/// way `ending` says, and checks that no process of that call is left
/// once `bake` has exited.
#[cfg(target_os = "linux")]
fn ends_with_no_process_left(ending: Ending, expected: i32) {
    let tag = match ending {
        Ending::EndOfInput => "eof".to_owned(),
        Ending::Signal(signal) => format!("signal-{signal}"),
    };
    let marker = format!("bake-rpc-orphan-{}-{tag}", std::process::id());
    let server = ModelServer::start(vec![
        Reply::tool_call(
            "bash",
            &json!({ "command": format!("sleep 600; echo {marker}") }),
        ),
        Reply::text(),
    ]);
    let workspace = Workspace::with_model("orphan", server.port);
    let mut client = workspace.rpc(&["--sandbox", "danger-full-access", "--approval", "never"]);
    client.send(json!({ "id": "p", "type": "prompt", "message": "run it" }));
    assert_eq!(
        client.response("p")["data"],
        json!({ "disposition": "started" })
    );
    let deadline = Instant::now() + TIMEOUT;
    let tree = loop {
        let tree = marked_tree(&marker);
        // The shell and its `sleep`.
        if tree.len() >= 2 {
            break tree;
        }
        assert!(
            Instant::now() < deadline,
            "the bash call did not start; records: {:?}, stderr: {}",
            client.records(),
            client.stderr()
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let status = match ending {
        Ending::EndOfInput => client.close(),
        Ending::Signal(signal) => {
            assert_eq!(unsafe { libc::kill(client.child.id() as i32, signal) }, 0);
            client.wait()
        }
    };
    assert_eq!(status, Some(expected), "{ending:?}: {}", client.stderr());
    let left: Vec<u32> = tree
        .iter()
        .filter(|(pid, start)| started_at(*pid).as_ref() == Some(start))
        .map(|(pid, _)| *pid)
        .collect();
    assert!(
        left.is_empty(),
        "{ending:?} left {left:?} of {tree:?} running"
    );
    assert!(marked_tree(&marker).is_empty(), "{ending:?}");
}

// Scope 13 and Pi `docs/rpc.md`, "Shutdown": end of input while a tool
// runs shuts the session down, which stops the call, and exits 0.
#[cfg(target_os = "linux")]
#[test]
fn end_of_input_during_a_bash_call_exits_with_no_process_left() {
    ends_with_no_process_left(Ending::EndOfInput, 0);
}

// Scope 13 and Pi `rpc-mode.ts`'s signal handlers: `SIGTERM`, `SIGINT`,
// and `SIGHUP` during a `bash` call shut the session down, which stops the
// call, and exit with 143, 130, and 129.
#[cfg(target_os = "linux")]
#[test]
fn stop_signals_during_a_bash_call_exit_with_no_process_left() {
    for (signal, expected) in [
        (libc::SIGTERM, 143),
        (libc::SIGINT, 130),
        (libc::SIGHUP, 129),
    ] {
        ends_with_no_process_left(Ending::Signal(signal), expected);
    }
}
