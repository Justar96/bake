//! The shipped `bake-rs` binary as the consumer of the Bake home's
//! `mcp.json`: print mode against a loopback model server that answers
//! `POST /v1/responses` with OpenAI Responses event streams, and real stdio
//! MCP servers. A configured server's tools are declared, called, and
//! logged; `--no-mcp` and a missing file start nothing; refused entries and
//! files are reported, terminal-safe and without their values, while valid
//! entries still start; the scrub and an entry's explicit `env`; D24
//! admission under the session's sandbox; and no server process outlives a
//! normal exit, a setup that fails after the servers started, or a stop
//! request during setup. Cases that start a server run on Unix: elsewhere
//! an existing `mcp.json` is refused unread until its owner can be checked,
//! which the last case asserts.
//!
//! The servers are `bake-coding-agent`'s MCP session fixture
//! (`tests/mcp_session/fixture.rs`), run by this test binary re-executed, so
//! the binary has its own harness (`harness = false`): libtest would write
//! to the standard output the protocol uses. Processes are observed through
//! the same crate's stable identities (`tests/mcp_session/probe.rs`: a Linux
//! pidfd, a Windows process handle, or a macOS pid with its start time). A
//! server is observed while `bake-rs`, which holds it unreaped until it
//! settles it, is waiting on the model or on standard input, and the
//! server's later events show it was still that process; a case that
//! expects a process fails if it cannot observe one. Each check that a
//! process is gone is made at once after `bake-rs` exits, with no wait.
//!
//! Each case owns a temporary directory, a model server, and every process
//! it starts; whatever its outcome, a `bake-rs` still running is killed and
//! awaited, then each observed process still running is killed and awaited
//! within a bound, descendants first, which fails the case.

// Most helpers serve the cases that start a server, which run on Unix.
#![cfg_attr(not(unix), allow(dead_code))]

#[allow(dead_code)]
#[path = "../../bake-coding-agent/tests/mcp_session/fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "../../bake-coding-agent/tests/mcp_session/probe.rs"]
mod probe;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long one `bake-rs` run may take before the case fails.
const RUN_BOUND: Duration = Duration::from_secs(60);
/// How long a case waits for something it already caused.
const BOUND: Duration = Duration::from_secs(20);
/// A variable `bake-rs` holds that the scrub must keep from servers.
const SECRET: &str = "BAKE_MCP_CLI_TEST_SECRET_TOKEN";
const SECRET_VALUE: &str = "leaked-secret-value";
/// A variable no process sets.
const UNSET: &str = "BAKE_MCP_CLI_TEST_UNSET";
/// The answer the text reply prints.
const ANSWER: &str = "Hello from the loop.";
const TEXT_SSE: &str = include_str!("fixtures/openai-responses-text.sse");

type CaseBody = fn(&Arc<Case>);

fn main() {
    if fixture::run_if_fixture() {
        return;
    }
    let filter: Option<String> = std::env::args().skip(1).find(|arg| !arg.starts_with('-'));
    let cases: Vec<(&str, CaseBody)> = vec![
        #[cfg(unix)]
        (
            "a_configured_tool_is_declared_called_and_logged",
            a_configured_tool_is_declared_called_and_logged,
        ),
        #[cfg(unix)]
        (
            "the_scrub_drops_credentials_an_entry_does_not_name",
            the_scrub_drops_credentials_an_entry_does_not_name,
        ),
        #[cfg(unix)]
        (
            "a_url_servers_tool_is_declared_called_and_closed",
            a_url_servers_tool_is_declared_called_and_closed,
        ),
        ("no_mcp_starts_nothing", no_mcp_starts_nothing),
        (
            "a_missing_configuration_is_an_ordinary_startup",
            a_missing_configuration_is_an_ordinary_startup,
        ),
        #[cfg(unix)]
        (
            "refused_entries_are_reported_and_valid_servers_start",
            refused_entries_are_reported_and_valid_servers_start,
        ),
        #[cfg(unix)]
        (
            "admission_follows_the_sessions_sandbox",
            admission_follows_the_sessions_sandbox,
        ),
        #[cfg(unix)]
        (
            "an_unsafe_configuration_starts_nothing",
            an_unsafe_configuration_starts_nothing,
        ),
        #[cfg(unix)]
        (
            "a_setup_failure_after_servers_start_leaves_no_process",
            a_setup_failure_after_servers_start_leaves_no_process,
        ),
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        (
            "an_enabled_plugin_vetoes_an_mcp_call_and_both_owners_end",
            an_enabled_plugin_vetoes_an_mcp_call_and_both_owners_end,
        ),
        #[cfg(unix)]
        (
            "tool_restrictions_cover_mcp_tools",
            tool_restrictions_cover_mcp_tools,
        ),
        #[cfg(unix)]
        (
            "restrictions_cover_a_later_listing",
            restrictions_cover_a_later_listing,
        ),
        #[cfg(unix)]
        (
            "ctrl_c_while_setup_waits_for_input_stops_every_server",
            ctrl_c_while_setup_waits_for_input_stops_every_server,
        ),
        #[cfg(unix)]
        (
            "ctrl_c_during_the_startup_wait_stops_every_server",
            ctrl_c_during_the_startup_wait_stops_every_server,
        ),
        #[cfg(unix)]
        (
            "ctrl_c_during_a_prompt_stops_every_server",
            ctrl_c_during_a_prompt_stops_every_server,
        ),
        #[cfg(unix)]
        (
            "ctrl_c_after_a_finished_prompt_stops_every_server",
            ctrl_c_after_a_finished_prompt_stops_every_server,
        ),
        #[cfg(not(unix))]
        (
            "an_existing_configuration_is_refused_where_its_owner_cannot_be_checked",
            an_existing_configuration_is_refused_where_its_owner_cannot_be_checked,
        ),
        #[cfg(unix)]
        (
            "a_stop_during_setup_shuts_the_servers_down",
            a_stop_during_setup_shuts_the_servers_down,
        ),
    ];
    let mut failed = Vec::new();
    let mut ran = 0;
    for (name, body) in cases {
        if filter
            .as_deref()
            .is_some_and(|filter| !name.contains(filter))
        {
            continue;
        }
        ran += 1;
        match run_case(name, body) {
            Ok(()) => println!("test {name} ... ok"),
            Err(error) => {
                println!("test {name} ... FAILED\n{error}");
                failed.push(name);
            }
        }
    }
    println!(
        "\ntest result: {}. {} passed; {} failed",
        if failed.is_empty() { "ok" } else { "FAILED" },
        ran - failed.len(),
        failed.len()
    );
    if !failed.is_empty() {
        std::process::exit(1);
    }
}

fn run_case(name: &str, body: CaseBody) -> Result<(), String> {
    let case = Arc::new(Case::new(name));
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| body(&case))).map_err(|panic| {
        panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_else(|| "panicked".into())
    });
    let cleanup = case.clean_up();
    match (outcome, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(cleanup)) => Err(format!("cleanup: {cleanup}")),
        (Err(error), Err(cleanup)) => Err(format!("{error}\ncleanup: {cleanup}")),
    }
}

/// One case's directory, its running `bake-rs`, if any, and the processes
/// it observed, each marked whether it is a descendant of a server.
struct Case {
    root: PathBuf,
    child: Mutex<Option<Child>>,
    /// The pid of the `bake-rs` last started, while it is unreaped.
    pid: Mutex<Option<u32>>,
    processes: Mutex<Vec<(bool, probe::Process)>>,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "bake-rs-mcp-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["home", "project", "user"] {
            std::fs::create_dir_all(root.join(dir)).expect("case directory");
            chmod(&root.join(dir), 0o755);
        }
        chmod(&root, 0o755);
        Self {
            // Canonical, so paths the servers report compare equal.
            root: std::fs::canonicalize(&root).expect("canonical case directory"),
            child: Mutex::new(None),
            pid: Mutex::new(None),
            processes: Mutex::default(),
        }
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn project(&self) -> PathBuf {
        self.root.join("project")
    }

    fn server_dir(&self, name: &str) -> PathBuf {
        self.root.join(format!("server-{name}"))
    }

    /// `models.json` naming the model server on `port`.
    fn write_models(&self, port: u16) {
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
        std::fs::write(self.home().join("models.json"), models.to_string()).expect("models.json");
    }

    /// Writes `mcp.json` in `home`, writable by its owner only.
    fn write_config(&self, home: &Path, config: &Value) -> PathBuf {
        let path = home.join("mcp.json");
        std::fs::write(&path, config.to_string()).expect("mcp.json");
        chmod(&path, 0o644);
        path
    }

    /// The fixture server's entry, with its own events directory and
    /// `extra` environment.
    fn server(&self, name: &str, mode: &str, extra: Value) -> Value {
        let dir = self.server_dir(name);
        std::fs::create_dir_all(&dir).expect("server directory");
        let mut env = json!({
            fixture::ROLE: "server",
            "FIXTURE_DIR": dir.to_string_lossy(),
            "FIXTURE_MODE": mode,
        });
        if let (Some(env), Some(extra)) = (env.as_object_mut(), extra.as_object()) {
            env.extend(extra.clone());
        }
        json!({
            "command": std::env::current_exe().expect("test binary").to_string_lossy(),
            "env": env,
        })
    }

    fn events(&self, server: &str) -> Vec<String> {
        std::fs::read_to_string(self.server_dir(server).join("events"))
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// Waits until `server` has logged an event starting with `prefix`.
    fn event(&self, server: &str, prefix: &str) -> String {
        let deadline = Instant::now() + BOUND;
        loop {
            if let Some(found) = self
                .events(server)
                .into_iter()
                .find(|event| event.starts_with(prefix))
            {
                return found;
            }
            assert!(
                Instant::now() < deadline,
                "server {server} never logged {prefix:?}: {:?}",
                self.events(server)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Starts `bake-rs` with `args` in the project; standard input is a
    /// pipe the caller holds when `hold_stdin`, else empty.
    fn spawn(&self, args: &[&str], hold_stdin: bool, env: &[(&str, &str)]) -> Option<ChildStdin> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bake-rs"));
        command
            .args(args)
            .current_dir(self.project())
            .env("BAKE_HOME", self.home())
            .env_remove("DSH_HOME")
            .env_remove("CLIPROXYAPI_API_KEY")
            .env("HOME", self.root.join("user"))
            .env("USERPROFILE", self.root.join("user"))
            .env(SECRET, SECRET_VALUE)
            .env_remove(UNSET)
            .stdin(if hold_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("PI_") {
                command.env_remove(key);
            }
        }
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("bake-rs starts");
        let stdin = child.stdin.take();
        *lock(&self.pid) = Some(child.id());
        *lock(&self.child) = Some(child);
        stdin
    }

    /// The running `bake-rs`'s pid; [`Self::wait`] may be waiting for it.
    fn child_pid(&self) -> u32 {
        lock(&self.pid).expect("bake-rs runs")
    }

    /// Waits, bounded, for the running `bake-rs` to exit and reads its
    /// output.
    fn wait(&self) -> Run {
        let mut child = lock(&self.child).take().expect("bake-rs runs");
        let drain = |pipe: Option<Box<dyn Read + Send>>| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                if let Some(mut pipe) = pipe {
                    let _ = pipe.read_to_end(&mut bytes);
                }
                String::from_utf8_lossy(&bytes).into_owned()
            })
        };
        let stdout = drain(child.stdout.take().map(|pipe| Box::new(pipe) as _));
        let stderr = drain(child.stderr.take().map(|pipe| Box::new(pipe) as _));
        let deadline = Instant::now() + RUN_BOUND;
        let status = loop {
            match child.try_wait().expect("bake-rs status") {
                Some(status) => {
                    *lock(&self.pid) = None;
                    break status;
                }
                None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                None => {
                    // Put back for the cleanup, which kills and reaps it.
                    *lock(&self.child) = Some(child);
                    panic!("bake-rs ran longer than {RUN_BOUND:?}");
                }
            }
        };
        Run {
            status,
            stdout: stdout.join().expect("stdout reader"),
            stderr: stderr.join().expect("stderr reader"),
        }
    }

    /// Runs `bake-rs -p` to its end with `args` before the prompt.
    fn run(&self, args: &[&str]) -> Run {
        let mut all = vec!["-p"];
        all.extend_from_slice(args);
        all.push("Go");
        self.spawn(&all, false, &[]);
        self.wait()
    }

    /// Observes the server whose events name its pid, as a descendant of
    /// the running `bake-rs`, and its `grandchild` when it logged one.
    /// Fails the case when a process cannot be observed.
    fn observe_server(&self, server: &str) -> Vec<u32> {
        let pid = |event: &str| -> u32 {
            event
                .rsplit(' ')
                .next()
                .and_then(|pid| pid.parse().ok())
                .unwrap_or_else(|| panic!("no pid in {event:?}"))
        };
        let bake =
            probe::Process::observe(self.child_pid()).unwrap_or_else(|error| panic!("{error}"));
        let leader = pid(&self.event(server, "pid "));
        let server_process = probe::Process::observe_descendant(&bake, leader)
            .unwrap_or_else(|error| panic!("{error}"));
        let mut observed = vec![(false, server_process)];
        if let Some(event) = self
            .events(server)
            .into_iter()
            .find(|event| event.starts_with("grandchild "))
        {
            let descendant = probe::Process::observe_descendant(&observed[0].1, pid(&event))
                .unwrap_or_else(|error| panic!("{error}"));
            observed.push((true, descendant));
        }
        let pids: Vec<u32> = observed.iter().map(|(_, process)| process.pid).collect();
        assert!(
            observed.iter().all(|(_, process)| !process.exited()),
            "observed processes of {server} had already exited: {pids:?}"
        );
        lock(&self.processes).extend(observed);
        pids
    }

    /// Whether each observed process with one of `pids` has exited, at once.
    fn all_exited(&self, pids: &[u32]) -> bool {
        let processes = lock(&self.processes);
        let found: Vec<bool> = processes
            .iter()
            .filter(|(_, process)| pids.contains(&process.pid))
            .map(|(_, process)| process.exited())
            .collect();
        assert_eq!(found.len(), pids.len(), "unobserved pids among {pids:?}");
        found.iter().all(|exited| *exited)
    }

    /// Kills and reaps a `bake-rs` still running, then kills and awaits,
    /// bounded, each observed process still running, descendants first;
    /// either fails the case. Removes the directory.
    fn clean_up(&self) -> Result<(), String> {
        let mut problems = Vec::new();
        if let Some(mut child) = lock(&self.child).take() {
            if matches!(child.try_wait(), Ok(None)) {
                problems.push("bake-rs was still running".to_owned());
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        let mut processes = std::mem::take(&mut *lock(&self.processes));
        processes.sort_by_key(|(descendant, _)| !*descendant);
        for (_, process) in &processes {
            match process.clean_up() {
                Ok(false) => {}
                Ok(true) => problems.push(format!("process {} had to be killed", process.pid)),
                Err(error) => problems.push(error),
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(unix)]
fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

#[cfg(not(unix))]
fn chmod(_path: &Path, _mode: u32) {}

/// How one `bake-rs` run ended.
struct Run {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

/// A reply the model server sends: an event stream body.
#[derive(Clone)]
struct Reply(String);

impl Reply {
    fn text() -> Self {
        Self(format!("{TEXT_SSE}\n"))
    }

    /// One response holding these tool calls, in order.
    fn calls(calls: &[(&str, Value)]) -> Self {
        let response = |status: &str, output: Value| json!({"id": "resp_calls", "object": "response", "created_at": 1_760_000_000, "status": status, "model": "loop-model", "output": output});
        let mut events = vec![
            json!({"type": "response.created", "response": response("in_progress", json!([]))}),
        ];
        let mut items = Vec::new();
        for (index, (name, arguments)) in calls.iter().enumerate() {
            let item = |status: &str, arguments: &str| json!({"id": format!("fc_{index}"), "type": "function_call", "status": status, "call_id": format!("call_{index}"), "name": name, "arguments": arguments});
            let arguments = arguments.to_string();
            events.push(json!({"type": "response.output_item.added", "output_index": index, "item": item("in_progress", "")}));
            events.push(json!({"type": "response.function_call_arguments.done", "item_id": format!("fc_{index}"), "output_index": index, "arguments": arguments}));
            events.push(json!({"type": "response.output_item.done", "output_index": index, "item": item("completed", &arguments)}));
            items.push(item("completed", &arguments));
        }
        let mut done = response("completed", Value::Array(items));
        done["usage"] = json!({"input_tokens": 40, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 8, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 48});
        events.push(json!({"type": "response.completed", "response": done}));
        Self(
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

/// Called with each request's index and body before its reply is sent.
type Hook = Box<dyn Fn(usize, &Value) + Send + Sync>;

/// A loopback model server answering with `replies` in turn, repeating the
/// last; stopped and joined on drop.
struct ModelServer {
    port: u16,
    requests: Arc<Mutex<Vec<Value>>>,
    /// Failures of the hook, which runs on the server's thread.
    failures: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ModelServer {
    fn start(replies: Vec<Reply>) -> Self {
        Self::with_hook(replies, Box::new(|_, _| {}))
    }

    fn with_hook(replies: Vec<Reply>, hook: Hook) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("address").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let failures = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (requests, failures, stop) = (
                Arc::clone(&requests),
                Arc::clone(&failures),
                Arc::clone(&stop),
            );
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((socket, _)) => {
                            let _ = socket.set_nonblocking(false);
                            serve(socket, &replies, &requests, &failures, &hook);
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            })
        };
        Self {
            port,
            requests,
            failures,
            stop,
            thread: Some(thread),
        }
    }

    fn requests(&self) -> Vec<Value> {
        lock(&self.requests).clone()
    }

    fn assert_hook_passed(&self) {
        let failures = lock(&self.failures);
        assert!(failures.is_empty(), "{failures:?}");
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

fn serve(
    mut socket: TcpStream,
    replies: &[Reply],
    requests: &Mutex<Vec<Value>>,
    failures: &Mutex<Vec<String>>,
    hook: &Hook,
) {
    let _ = socket.set_read_timeout(Some(Duration::from_secs(10)));
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        match socket.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
        if buffer.len() > 1 << 20 {
            return;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).to_lowercase();
    let length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0)
        .min(16 << 20);
    while buffer.len() < head_end + 4 + length {
        match socket.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    }
    let body: Value = buffer
        .get(head_end + 4..)
        .and_then(|body| serde_json::from_slice(body).ok())
        .unwrap_or(Value::Null);
    let index = {
        let mut requests = lock(requests);
        requests.push(body.clone());
        requests.len() - 1
    };
    if let Err(panic) = std::panic::catch_unwind(AssertUnwindSafe(|| hook(index, &body))) {
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_else(|| "the hook panicked".into());
        lock(failures).push(message);
    }
    let Reply(body) = &replies[index.min(replies.len() - 1)];
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(response.as_bytes());
    let _ = socket.flush();
}

/// The tool names a request declares.
fn declared(request: &Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect()
}

/// Each tool output a request sends back, in order.
fn tool_outputs(request: &Value) -> Vec<String> {
    request["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| match &item["output"] {
            Value::String(text) => text.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect(),
            other => other.to_string(),
        })
        .collect()
}

/// Every line of the one session file under the home.
fn session_lines(case: &Case) -> Vec<Value> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "jsonl") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&case.home().join("sessions"), &mut files);
    assert_eq!(files.len(), 1, "{files:?}");
    std::fs::read_to_string(&files[0])
        .expect("session file")
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON line"))
        .collect()
}

const BUILTIN: [&str; 10] = [
    "read",
    "bash",
    "edit",
    "write",
    "job_output",
    "job_list",
    "job_kill",
    "web_search",
    "web_fetch",
    "codemode",
];

const FIXTURE_TOOLS: [&str; 9] = [
    "mcp__fx__echo",
    "mcp__fx__env",
    "mcp__fx__slow",
    "mcp__fx__change",
    "mcp__fx__break_list",
    "mcp__fx__spawn",
    "mcp__fx__fail",
    "mcp__fx__exit",
    "mcp__fx__write",
];

/// The pids of `root`'s descendants, from `ps`, whose arguments include
/// `marker`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn descendants_running(root: u32, marker: &str) -> Vec<u32> {
    let out = Command::new("ps")
        .args(["-A", "-o", "pid=", "-o", "ppid=", "-o", "args="])
        .output()
        .expect("ps runs");
    let table: Vec<(u32, u32, String)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid = fields.next()?.parse().ok()?;
            Some((pid, ppid, fields.collect::<Vec<_>>().join(" ")))
        })
        .collect();
    let mut tree = vec![root];
    let mut index = 0;
    while let Some(&parent) = tree.get(index) {
        tree.extend(
            table
                .iter()
                .filter(|(_, ppid, _)| *ppid == parent)
                .map(|(pid, _, _)| *pid),
        );
        index += 1;
    }
    table
        .iter()
        .filter(|(pid, _, args)| tree[1..].contains(pid) && args.contains(marker))
        .map(|(pid, _, _)| *pid)
        .collect()
}

// Bake's installed-plugin veto over an MCP tool, through the shipped
// binary: a home enables a plugin whose manifest lists the fixture
// server's `mcp__fx__write`, and the hostile component blocks a call
// carrying a `block` reason. That call never reaches the server and its
// file is not written; the decision precedes the denial in the session
// file; the same tool without a reason runs. The plugin's helper and the
// server are observed while `bake-rs` waits on the model, and both are
// gone at once when it exits, so its shutdown awaited both owners. Only
// Linux and macOS check who can change plugin files, so only they enable
// one.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn an_enabled_plugin_vetoes_an_mcp_call_and_both_owners_end(case: &Arc<Case>) {
    let package = case.root.join("package");
    std::fs::create_dir_all(&package).expect("package directory");
    std::fs::write(
        package.join("plugin.json"),
        r#"{"api":"0.1.0","id":"mcp-guard","version":"0.1.0","vetoTools":["mcp__fx__write"]}"#,
    )
    .expect("plugin.json");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/fixtures/hostile/hostile.wasm"),
        package.join("plugin.wasm"),
    )
    .expect("plugin.wasm");
    let manage = |args: &[&str]| {
        case.spawn(args, false, &[]);
        let run = case.wait();
        assert_eq!(run.status.code(), Some(0), "{args:?}: {}", run.stderr);
        run.stdout
    };
    let installed = manage(&["plugin", "install", &package.to_string_lossy()]);
    let hash = installed
        .lines()
        .find_map(|line| line.strip_prefix("installed mcp-guard 0.1.0 as sha256:"))
        .unwrap_or_else(|| panic!("{installed}"))
        .to_owned();
    manage(&["plugin", "enable", &hash]);

    let (vetoed, allowed) = (
        case.project().join("vetoed.txt"),
        case.project().join("allowed.txt"),
    );
    let observed: Arc<Mutex<Vec<u32>>> = Arc::default();
    let (hook_case, observed_hook) = (Arc::clone(case), Arc::clone(&observed));
    let model = ModelServer::with_hook(
        vec![
            Reply::calls(&[
                (
                    "mcp__fx__write",
                    json!({"path": vetoed.to_string_lossy(), "block": "No MCP writes here"}),
                ),
                ("mcp__fx__write", json!({"path": allowed.to_string_lossy()})),
            ]),
            Reply::text(),
        ],
        Box::new(move |index, _| {
            if index != 1 {
                return;
            }
            // `bake-rs` waits for this reply: the vetoing helper and the
            // server it asked about both still run.
            let mut pids = hook_case.observe_server("fx");
            let bake = probe::Process::observe(hook_case.child_pid())
                .unwrap_or_else(|error| panic!("{error}"));
            let helpers = descendants_running(bake.pid, "__bake-wasm-plugin-host");
            assert_eq!(helpers.len(), 1, "one plugin helper: {helpers:?}");
            let helper = probe::Process::observe_descendant(&bake, helpers[0])
                .unwrap_or_else(|error| panic!("{error}"));
            assert!(!helper.exited(), "the helper had already exited");
            pids.push(helper.pid);
            lock(&hook_case.processes).push((false, helper));
            *lock(&observed_hook) = pids;
        }),
    );
    case.write_models(model.port);
    case.write_config(
        &case.home(),
        &json!({ "mcpServers": { "fx": case.server("fx", "", json!({})) } }),
    );
    let run = case.run(&["--sandbox", "danger-full-access"]);
    let pids = lock(&observed).clone();
    // At once: the binary's shutdown returned only once they were gone.
    assert!(
        case.all_exited(&pids),
        "the server or the plugin helper outlived bake-rs"
    );
    assert_eq!(pids.len(), 2, "the server and the helper: {pids:?}");
    model.assert_hook_passed();
    assert_eq!(run.status.code(), Some(0), "{}", run.stderr);
    assert_eq!(run.stdout, format!("{ANSWER}\n"));
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(tool_outputs(&requests[1]), ["No MCP writes here", "wrote"]);
    assert!(!vetoed.exists(), "the vetoed call wrote its file");
    assert!(allowed.exists(), "the allowed call did not write");
    let calls: Vec<String> = case
        .events("fx")
        .into_iter()
        .filter(|event| event.starts_with("call "))
        .collect();
    assert_eq!(
        calls.len(),
        1,
        "only the allowed call reached the server: {calls:?}"
    );
    let lines = session_lines(case);
    let decision = lines
        .iter()
        .position(|line| line["customType"] == "bake.plugin.decision")
        .expect("a logged decision");
    let denial = lines
        .iter()
        .position(|line| {
            line["message"]["role"] == "toolResult"
                && line["message"]["content"][0]["text"] == "No MCP writes here"
        })
        .expect("a logged denial");
    assert!(decision < denial, "the decision follows its denial");
    assert_eq!(lines[decision]["data"]["sha256"], hash.as_str());
    assert_eq!(lines[denial]["message"]["toolName"], "mcp__fx__write");
}

// Pi v1.3.0 `test/suite/agent-session-mcp.test.ts`, "declares directly
// exposed MCP tools to the model", through the shipped binary: the first
// request declares the server's tools after the built-in ones, the call
// reaches the server, its result reaches the model and the session file,
// and the server and its descendant are gone when `bake-rs` exits.
#[cfg(unix)]
fn a_configured_tool_is_declared_called_and_logged(case: &Arc<Case>) {
    let observed: Arc<Mutex<Vec<u32>>> = Arc::default();
    let (hook_case, observed_hook) = (Arc::clone(case), Arc::clone(&observed));
    let model = ModelServer::with_hook(
        vec![
            Reply::calls(&[("mcp__fx__echo", json!({"text": "hi"}))]),
            Reply::text(),
        ],
        Box::new(move |index, body| {
            if index != 0 {
                return;
            }
            let mut expected: Vec<&str> = BUILTIN.to_vec();
            expected.extend(FIXTURE_TOOLS);
            assert_eq!(declared(body), expected);
            // `bake-rs` waits for this reply, so its server is serving.
            *lock(&observed_hook) = hook_case.observe_server("fx");
        }),
    );
    case.write_models(model.port);
    case.write_config(
        &case.home(),
        &json!({ "mcpServers": { "fx": case.server("fx", "grandchild", json!({})) } }),
    );
    let run = case.run(&["--sandbox", "danger-full-access"]);
    let pids = lock(&observed).clone();
    // At once: the binary's shutdown returned only once they were gone.
    assert!(case.all_exited(&pids), "a server process outlived bake-rs");
    assert_eq!(pids.len(), 2, "the server and its descendant: {pids:?}");
    model.assert_hook_passed();
    assert_eq!(run.stderr, "");
    assert_eq!(run.status.code(), Some(0));
    assert_eq!(run.stdout, format!("{ANSWER}\n"));
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(tool_outputs(&requests[1]), ["echo: hi"]);
    // The call came after the reply the hook held, so the observed server,
    // whose events these are, still ran then: its identity was never
    // another process's.
    let events = case.events("fx");
    assert!(
        events.iter().any(|event| event.starts_with("call echo ")),
        "{events:?}"
    );
    let lines = session_lines(case);
    let result = lines
        .iter()
        .find(|line| line["message"]["role"] == "toolResult")
        .expect("a logged tool result");
    assert_eq!(result["message"]["toolName"], "mcp__fx__echo");
    assert_eq!(result["message"]["content"][0]["text"], "echo: hi");
    // The declaration is logged, so a resumed session rebuilds the request.
    let added: Vec<&str> = lines
        .iter()
        .filter(|line| line["message"]["role"] == "system")
        .flat_map(|line| {
            line["message"]["toolsAdded"]
                .as_array()
                .into_iter()
                .flatten()
        })
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(
        FIXTURE_TOOLS.iter().all(|tool| added.contains(tool)),
        "{added:?}"
    );
}

// Bake's `transport.ts` `buildChildEnv` over `scrubbedParentEnv`: a
// credential-shaped variable of `bake-rs` never reaches a server unless the
// entry names it, and an entry's template passes it.
#[cfg(unix)]
fn the_scrub_drops_credentials_an_entry_does_not_name(case: &Arc<Case>) {
    let model = ModelServer::start(vec![
        Reply::calls(&[
            ("mcp__fx__env", json!({"name": SECRET})),
            ("mcp__fx__env", json!({"name": "PASSED"})),
            ("mcp__fx__env", json!({"name": "BAKE_HOME"})),
        ]),
        Reply::text(),
    ]);
    case.write_models(model.port);
    let passed = format!("${{{SECRET}}}");
    case.write_config(
        &case.home(),
        &json!({ "mcpServers": { "fx": case.server("fx", "", json!({ "PASSED": passed })) } }),
    );
    let run = case.run(&["--sandbox", "danger-full-access"]);
    assert_eq!(run.stderr, "");
    assert_eq!(run.status.code(), Some(0));
    assert_eq!(
        tool_outputs(&model.requests()[1]),
        ["<unset>", SECRET_VALUE, "<unset>"]
    );
}

// Pi v1.3.0 `--no-mcp`: no file is read and no server starts. The first
// case is the control: the same configuration starts the server.
fn no_mcp_starts_nothing(case: &Arc<Case>) {
    let model = ModelServer::start(vec![Reply::text()]);
    case.write_models(model.port);
    case.write_config(
        &case.home(),
        &json!({ "mcpServers": { "fx": case.server("fx", "", json!({})) } }),
    );
    let run = case.run(&["--no-mcp", "--sandbox", "danger-full-access"]);
    assert_eq!(run.stderr, "");
    assert_eq!(run.status.code(), Some(0));
    assert_eq!(declared(&model.requests()[0]), BUILTIN);
    assert!(case.events("fx").is_empty(), "{:?}", case.events("fx"));
}

fn a_missing_configuration_is_an_ordinary_startup(case: &Arc<Case>) {
    let model = ModelServer::start(vec![Reply::text()]);
    case.write_models(model.port);
    let run = case.run(&[]);
    assert_eq!(run.stderr, "");
    assert_eq!(run.status.code(), Some(0));
    assert_eq!(run.stdout, format!("{ANSWER}\n"));
    assert_eq!(declared(&model.requests()[0]), BUILTIN);
}

// Pi's `readConfigFile` keeps valid entries beside refused ones, and Pi
// reports both; Bake's print mode reports them on standard error, without
// the values of `env`, and with nothing a file supplies reaching the
// terminal as an escape sequence. Refused servers spawn nothing.
#[cfg(unix)]
fn refused_entries_are_reported_and_valid_servers_start(case: &Arc<Case>) {
    let model = ModelServer::start(vec![Reply::text()]);
    case.write_models(model.port);
    let mut unset = case.server("unset", "", json!({ "TOKEN": format!("${{{UNSET}}}") }));
    unset["description"] = json!("refused at its start");
    let config = json!({ "mcpServers": {
        "fx": case.server("fx", "", json!({})),
        "bad": { "args": ["no command"] },
        "legacy": { "type": "sse", "url": "https://example.com/sse" },
        "remote": { "url": "http://93.184.216.34/mcp" },
        "code": { "command": "x", "exposure": "codemode" },
        "cmd": { "command": "x", "env": { "TOKEN": "!print-the-secret-value" } },
        "esc\u{1b}[31mname": { "command": "x" },
        "unset": unset,
    } });
    let path = case.write_config(&case.home(), &config);
    let run = case.run(&["--sandbox", "danger-full-access"]);
    assert_eq!(run.status.code(), Some(0), "{}", run.stderr);
    let shown = path.display();
    let expected = [
        format!("Warning: MCP: {shown}: server \"bad\" needs either \"command\""),
        format!("Warning: MCP: {shown}: server \"legacy\": legacy SSE transport is not supported"),
        format!(
            "Warning: MCP: {shown}: server \"code\": exposure \"codemode\" is not supported yet; use \"direct\" or \"hidden\""
        ),
        format!("Warning: MCP: {shown}: server \"cmd\": env \"TOKEN\" runs a command"),
        format!("Warning: MCP: {shown}: invalid server name \"esc\\u{{1b}}[31mname\""),
        "Warning: MCP server \"remote\" uses http to a public address".to_owned(),
        format!("Warning: MCP server \"unset\" env \"TOKEN\" references unset variables: {UNSET}"),
    ];
    for line in &expected {
        assert!(
            run.stderr.contains(line),
            "missing {line:?} in:\n{}",
            run.stderr
        );
    }
    assert_eq!(run.stderr.lines().count(), expected.len(), "{}", run.stderr);
    assert!(!run.stderr.contains('\u{1b}'), "{}", run.stderr);
    assert!(
        !run.stderr.contains("print-the-secret-value"),
        "{}",
        run.stderr
    );
    let mut want: Vec<&str> = BUILTIN.to_vec();
    want.extend(FIXTURE_TOOLS);
    assert_eq!(declared(&model.requests()[0]), want);
    assert!(!case.events("fx").is_empty());
    assert!(
        case.events("unset").is_empty(),
        "{:?}",
        case.events("unset")
    );

    // A file that is not JSON starts nothing and is one warning.
    std::fs::write(&path, "{").expect("mcp.json");
    let run = case.run(&["--sandbox", "danger-full-access"]);
    assert_eq!(run.status.code(), Some(0), "{}", run.stderr);
    assert!(
        run.stderr.starts_with(&format!("Warning: MCP: {shown}: ")),
        "{}",
        run.stderr
    );
    assert_eq!(run.stderr.lines().count(), 1, "{}", run.stderr);
    assert_eq!(declared(&model.requests()[1]), BUILTIN);
}

// D24: a server starts under the session's sandbox mode. Where a backend
// enforces `workspace-write`, the server cannot write outside the project
// and the temporary directories; on a local host without one, it is
// refused before anything runs and the model gets none of its tools (CI
// hosts have a backend, so there the refusal fails the case).
#[cfg(unix)]
fn admission_follows_the_sessions_sandbox(case: &Arc<Case>) {
    // Outside every writable root: Cargo's target tmpdir is not under the
    // system temporary directories.
    let outside = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("bake-rs-mcp-confined-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).expect("outside dir");
    let target = outside.join("escaped.txt");
    let model = ModelServer::start(vec![
        Reply::calls(&[("mcp__fx__write", json!({"path": target.to_string_lossy()}))]),
        Reply::text(),
    ]);
    case.write_models(model.port);
    case.write_config(
        &case.home(),
        &json!({ "mcpServers": { "fx": case.server("fx", "", json!({})) } }),
    );
    let run = case.run(&["--sandbox", "workspace-write"]);
    let escaped = target.exists();
    let _ = std::fs::remove_dir_all(&outside);
    assert_eq!(run.status.code(), Some(0), "{}", run.stderr);
    let requests = model.requests();
    if run.stderr.contains("MCP server \"fx\" was not started") {
        assert!(
            std::env::var_os("CI").is_none(),
            "no sandbox backend on a CI host that has one: {}",
            run.stderr
        );
        assert_eq!(declared(&requests[0]), BUILTIN);
        assert!(case.events("fx").is_empty(), "{:?}", case.events("fx"));
        eprintln!(
            "note: no sandbox backend; checked the refusal: {}",
            run.stderr.trim()
        );
        return;
    }
    assert_eq!(run.stderr, "");
    let outputs = tool_outputs(&requests[1]);
    assert!(!escaped, "the server's write escaped: {outputs:?}");
    assert!(
        outputs.len() == 1 && outputs[0].starts_with("error: "),
        "{outputs:?}"
    );
}

// The owner rules through the binary: a file other users can change, and a
// Bake home inside the working directory, start nothing, with the reason.
#[cfg(unix)]
fn an_unsafe_configuration_starts_nothing(case: &Arc<Case>) {
    let model = ModelServer::start(vec![Reply::text()]);
    case.write_models(model.port);
    let config = json!({ "mcpServers": { "fx": case.server("fx", "", json!({})) } });
    #[cfg(unix)]
    {
        let path = case.write_config(&case.home(), &config);
        chmod(&path, 0o666);
        let run = case.run(&["--sandbox", "danger-full-access"]);
        assert_eq!(run.status.code(), Some(0), "{}", run.stderr);
        assert_eq!(
            run.stderr,
            format!(
                "Warning: MCP: {}: can be changed by other users (mode 0666); remove their write access with chmod go-w. No MCP server was started from it\n",
                path.display()
            )
        );
        assert!(case.events("fx").is_empty());
        std::fs::remove_file(&path).expect("remove");
    }
    // `BAKE_HOME` may point anywhere; inside the project the agent's tools
    // could rewrite the servers.
    let inside = case.project().join(".bake");
    std::fs::create_dir_all(&inside).expect("inner home");
    chmod(&inside, 0o700);
    std::fs::copy(case.home().join("models.json"), inside.join("models.json")).expect("models");
    case.write_config(&inside, &config);
    let inside_text = inside.to_string_lossy().into_owned();
    case.spawn(
        &["-p", "--sandbox", "danger-full-access", "Go"],
        false,
        &[("BAKE_HOME", &inside_text)],
    );
    let run = case.wait();
    assert_eq!(run.status.code(), Some(0), "{}", run.stderr);
    assert!(
        run.stderr
            .contains("is inside the session's working directory")
            && run.stderr.contains("No MCP server was started from it"),
        "{}",
        run.stderr
    );
    assert_eq!(run.stderr.lines().count(), 1, "{}", run.stderr);
    assert!(case.events("fx").is_empty());
    assert!(
        model
            .requests()
            .iter()
            .all(|request| declared(request) == BUILTIN)
    );
}

/// Starts `bake-rs -p` with its standard input held open, so its setup
/// waits for the input after the session, and its servers, started, and
/// observes the server once it is connected.
fn start_held(case: &Case) -> (ChildStdin, Vec<u32>) {
    case.write_config(
        &case.home(),
        &json!({ "mcpServers": { "fx": case.server("fx", "grandchild", json!({})) } }),
    );
    let stdin = case
        .spawn(&["-p", "--sandbox", "danger-full-access", "Go"], true, &[])
        .expect("piped input");
    case.event("fx", "list ");
    let pids = case.observe_server("fx");
    (stdin, pids)
}

// Print mode with no model fails after reading its input, when the
// session's servers already run: the failure shuts the session down, and
// with it the servers.
#[cfg(unix)]
fn a_setup_failure_after_servers_start_leaves_no_process(case: &Arc<Case>) {
    let (stdin, pids) = start_held(case);
    drop(stdin);
    let run = case.wait();
    assert!(case.all_exited(&pids), "a server process outlived bake-rs");
    assert_eq!(pids.len(), 2, "{pids:?}");
    assert_eq!(run.status.code(), Some(1), "{}", run.stderr);
    assert!(run.stderr.contains("No models available"), "{}", run.stderr);
}

// A stop request (SIGTERM) while setup waits for the input shuts the
// session and its servers down, with Pi's exit status.
#[cfg(unix)]
fn a_stop_during_setup_shuts_the_servers_down(case: &Arc<Case>) {
    let model = ModelServer::start(vec![Reply::text()]);
    case.write_models(model.port);
    let (stdin, pids) = start_held(case);
    let pid = libc::pid_t::try_from(case.child_pid()).expect("pid");
    // SAFETY: this process's own unreaped child, so the pid is still it.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    let run = case.wait();
    assert!(case.all_exited(&pids), "a server process outlived bake-rs");
    drop(stdin);
    assert_eq!(run.status.code(), Some(143), "{}", run.stderr);
    assert!(model.requests().is_empty());
}

// Windows ACLs are not inspected yet, so an existing `mcp.json` is refused
// unread there and nothing starts; `--no-mcp` reads nothing.
/// A loopback Streamable HTTP MCP server for the `url` case: one request
/// per connection, answered with JSON, recording each request's JSON-RPC
/// method, HTTP method, and `Authorization` header. Stopped and joined on
/// drop. It is test code with its own HTTP reader, independent of the
/// transport under test.
#[cfg(unix)]
struct McpHttpServer {
    port: u16,
    seen: Arc<Mutex<Vec<(String, String, String)>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

#[cfg(unix)]
impl McpHttpServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("address").port();
        let seen: Arc<Mutex<Vec<(String, String, String)>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (seen, stop) = (Arc::clone(&seen), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((socket, _)) => {
                            let _ = socket.set_nonblocking(false);
                            serve_mcp(socket, &seen);
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            })
        };
        Self {
            port,
            seen,
            stop,
            thread: Some(thread),
        }
    }

    /// Every request, as (HTTP method, JSON-RPC method, `Authorization`).
    fn seen(&self) -> Vec<(String, String, String)> {
        lock(&self.seen).clone()
    }
}

#[cfg(unix)]
impl Drop for McpHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(unix)]
fn serve_mcp(mut socket: TcpStream, seen: &Mutex<Vec<(String, String, String)>>) {
    let _ = socket.set_read_timeout(Some(Duration::from_secs(10)));
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        match socket.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
        if buffer.len() > 1 << 20 {
            return;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let method = head.split_whitespace().next().unwrap_or("").to_owned();
    let header = |name: &str| {
        head.lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(key, _)| key.trim().eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim().to_owned())
    };
    let length: usize = header("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
        .min(1 << 20);
    while buffer.len() < head_end + 4 + length {
        match socket.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    }
    let message: Value = buffer
        .get(head_end + 4..head_end + 4 + length)
        .and_then(|body| serde_json::from_slice(body).ok())
        .unwrap_or(Value::Null);
    let rpc = message["method"].as_str().unwrap_or_default().to_owned();
    lock(seen).push((
        method.clone(),
        rpc.clone(),
        header("authorization").unwrap_or_default(),
    ));
    let id = message["id"].clone();
    let (status, headers, body) = match (method.as_str(), rpc.as_str()) {
        // No server-to-client stream, and the client may end the session.
        ("GET", _) => (405, Vec::new(), Value::Null),
        ("DELETE", _) => (200, Vec::new(), Value::Null),
        (_, "initialize") => (
            200,
            vec![("mcp-session-id", "cli-session-1".to_owned())],
            json!({"jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "cli-http-fixture", "version": "1.0.0"},
            }}),
        ),
        (_, "tools/list") => (
            200,
            Vec::new(),
            json!({"jsonrpc": "2.0", "id": id, "result": {"tools": [
                {"name": "echo", "description": "Echo text", "inputSchema": {
                    "type": "object", "properties": {"text": {"type": "string"}},
                }},
            ]}}),
        ),
        (_, "tools/call") => (
            200,
            Vec::new(),
            json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{
                "type": "text",
                "text": format!("http echo: {}", message["params"]["arguments"]["text"].as_str().unwrap_or_default()),
            }]}}),
        ),
        // A notification is accepted without an answer.
        _ if id.is_null() => (202, Vec::new(), Value::Null),
        _ => (
            200,
            Vec::new(),
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "no such method"}}),
        ),
    };
    let body = if body.is_null() {
        String::new()
    } else {
        body.to_string()
    };
    let mut response = format!("HTTP/1.1 {status} X\r\ncontent-length: {}\r\n", body.len());
    if !body.is_empty() {
        response.push_str("content-type: application/json\r\n");
    }
    for (name, value) in &headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str("connection: close\r\n\r\n");
    response.push_str(&body);
    let _ = socket.write_all(response.as_bytes());
    let _ = socket.flush();
}

// A `url` server in the home's `mcp.json`: `bake-rs -p` admits its origin,
// declares its tool beside the built-ins, calls it, logs the result, and
// ends its HTTP session with a `DELETE` carrying the configured header
// before it exits. The connection is the agent process's own, so no
// sandbox backend takes part.
#[cfg(unix)]
fn a_url_servers_tool_is_declared_called_and_closed(case: &Arc<Case>) {
    let server = McpHttpServer::start();
    let model = ModelServer::with_hook(
        vec![
            Reply::calls(&[("mcp__web__echo", json!({"text": "hi"}))]),
            Reply::text(),
        ],
        Box::new(|index, body| {
            if index != 0 {
                return;
            }
            let mut expected: Vec<&str> = BUILTIN.to_vec();
            expected.push("mcp__web__echo");
            assert_eq!(declared(body), expected);
        }),
    );
    case.write_models(model.port);
    case.write_config(
        &case.home(),
        &json!({ "mcpServers": { "web": {
            "url": format!("http://127.0.0.1:{}/mcp", server.port),
            "headers": { "Authorization": "Bearer cli-test-token" },
        } } }),
    );
    let run = case.run(&["--sandbox", "danger-full-access"]);
    model.assert_hook_passed();
    assert_eq!(run.stderr, "");
    assert_eq!(run.status.code(), Some(0));
    assert_eq!(run.stdout, format!("{ANSWER}\n"));
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(tool_outputs(&requests[1]), ["http echo: hi"]);
    let seen = server.seen();
    let rpc: Vec<&str> = seen.iter().map(|(_, rpc, _)| rpc.as_str()).collect();
    assert!(
        rpc.contains(&"initialize") && rpc.contains(&"tools/list") && rpc.contains(&"tools/call"),
        "{seen:?}"
    );
    assert!(
        seen.iter()
            .all(|(_, _, authorization)| authorization == "Bearer cli-test-token"),
        "every request carried the configured header: {seen:?}"
    );
    // The session was ended before the process exited.
    assert!(
        seen.iter().any(|(method, _, _)| method == "DELETE"),
        "{seen:?}"
    );
    let lines = session_lines(case);
    let result = lines
        .iter()
        .find(|line| line["message"]["role"] == "toolResult")
        .expect("a logged tool result");
    assert_eq!(result["message"]["toolName"], "mcp__web__echo");
    assert_eq!(result["message"]["content"][0]["text"], "http echo: hi");
}

#[cfg(not(unix))]
fn an_existing_configuration_is_refused_where_its_owner_cannot_be_checked(case: &Arc<Case>) {
    let model = ModelServer::start(vec![Reply::text()]);
    case.write_models(model.port);
    let path = case.write_config(
        &case.home(),
        &json!({ "mcpServers": { "fx": case.server("fx", "", json!({})) } }),
    );
    let run = case.run(&["--sandbox", "danger-full-access"]);
    assert_eq!(run.status.code(), Some(0), "{}", run.stderr);
    assert_eq!(
        run.stderr,
        format!(
            "Warning: MCP: {}: {}. No MCP server was started from it\n",
            path.display(),
            bake_coding_agent::mcp::home::UNCHECKED_PLATFORM
        )
    );
    assert!(case.events("fx").is_empty());
    assert_eq!(declared(&model.requests()[0]), BUILTIN);
}

/// Sends `signal` to the running `bake-rs`, this process's own unreaped
/// child, so the pid is still it.
#[cfg(unix)]
fn send_signal(case: &Case, signal: libc::c_int) {
    let pid = libc::pid_t::try_from(case.child_pid()).expect("pid");
    // SAFETY: a plain syscall to our unreaped child.
    assert_eq!(unsafe { libc::kill(pid, signal) }, 0);
}

/// Waits, bounded, until the process `pid` has exited.
fn wait_exited(pid: u32) {
    let process = probe::Process::observe(pid).unwrap_or_else(|error| panic!("{error}"));
    let deadline = Instant::now() + BOUND;
    while !process.exited() {
        assert!(Instant::now() < deadline, "bake-rs {pid} did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Runs `bake-rs -p` with `args` and one server, answering the model with
/// `replies`; returns the run and the requests.
#[cfg(unix)]
fn restricted(case: &Arc<Case>, args: &[&str], replies: Vec<Reply>) -> (Run, Vec<Value>) {
    let model = ModelServer::start(replies);
    case.write_models(model.port);
    let mut all = vec!["--sandbox", "danger-full-access"];
    all.extend_from_slice(args);
    let run = case.run(&all);
    (run, model.requests())
}

// Pi v1.3.0 `_isAllowedTool` and `_isActivatable` (`core/agent-session.ts`)
// without tool search, and `sdk.ts`: an allowlist must match an MCP tool
// for it to be declared, `--no-tools` alone declares none, `+name`/`-name`
// modifiers are no allowlist, and `--exclude-tools` always applies. A tool
// left out is not callable either: the model's call never reaches the
// server.
#[cfg(unix)]
fn tool_restrictions_cover_mcp_tools(case: &Arc<Case>) {
    case.write_config(
        &case.home(),
        &json!({ "mcpServers": { "fx": case.server("fx", "", json!({})) } }),
    );
    let all_mcp: Vec<&str> = FIXTURE_TOOLS.to_vec();
    let cases: Vec<(Vec<&str>, Vec<&str>)> = vec![
        (
            vec!["--tools", "read,mcp__fx__e*"],
            vec!["read", "mcp__fx__echo", "mcp__fx__env", "mcp__fx__exit"],
        ),
        (vec!["--tools", "read,grep"], vec!["read", "grep"]),
        (
            vec!["-nt", "--tools", "mcp__fx__echo"],
            vec!["mcp__fx__echo"],
        ),
        // The job tools leave with `bash`; the web tools and `codemode`
        // stay.
        (vec!["--tools", "+grep,-bash"], {
            let mut want = vec![
                "read",
                "edit",
                "write",
                "web_search",
                "web_fetch",
                "codemode",
                "grep",
            ];
            want.extend(all_mcp.iter().copied());
            want
        }),
        (vec!["--exclude-tools", "bash,mcp__fx__s*,mcp__fx__env"], {
            let mut want = vec![
                "read",
                "edit",
                "write",
                "web_search",
                "web_fetch",
                "codemode",
            ];
            want.extend(
                all_mcp
                    .iter()
                    .copied()
                    .filter(|name| !name.starts_with("mcp__fx__s") && *name != "mcp__fx__env"),
            );
            want
        }),
    ];
    for (args, want) in cases {
        let (run, requests) = restricted(case, &args, vec![Reply::text()]);
        assert_eq!(run.status.code(), Some(0), "{args:?}: {}", run.stderr);
        assert_eq!(run.stderr, "", "{args:?}");
        assert_eq!(declared(&requests[0]), want, "{args:?}");
    }
    // `--no-tools` alone: nothing could be declared, so no server starts.
    let before = case
        .events("fx")
        .iter()
        .filter(|event| event.starts_with("pid "))
        .count();
    let (run, requests) = restricted(case, &["-nt"], vec![Reply::text()]);
    assert_eq!(run.status.code(), Some(0), "{}", run.stderr);
    assert!(declared(&requests[0]).is_empty());
    let after = case
        .events("fx")
        .iter()
        .filter(|event| event.starts_with("pid "))
        .count();
    assert_eq!(before, after, "a server started under --no-tools");

    // An excluded tool the model calls anyway is not found, and the server
    // never sees the call.
    let (run, requests) = restricted(
        case,
        &["--exclude-tools", "mcp__fx__env"],
        vec![
            Reply::calls(&[("mcp__fx__env", json!({"name": "HOME"}))]),
            Reply::text(),
        ],
    );
    assert_eq!(run.status.code(), Some(0), "{}", run.stderr);
    let outputs = tool_outputs(&requests[1]);
    assert!(
        outputs.len() == 1
            && outputs[0].contains("mcp__fx__env")
            && outputs[0].contains("not found"),
        "{outputs:?}"
    );
    assert!(
        !case
            .events("fx")
            .iter()
            .any(|event| event.starts_with("call env ")),
        "{:?}",
        case.events("fx")
    );
}

/// The prompts of a run whose server changes its tool list during the
/// first prompt: `change` makes the fixture list variant B (no `echo`, a
/// new `extra`), and `slow` is only answered after the server has answered
/// that new listing, since it reads its input in order; the second prompt
/// declares the generation published then.
#[cfg(unix)]
fn later_listing(case: &Arc<Case>, args: &[&str]) -> Vec<Value> {
    let release = case.root.join("release-slow");
    let _ = std::fs::remove_file(&release);
    let model = ModelServer::start(vec![
        Reply::calls(&[("mcp__fx__change", json!({}))]),
        Reply::calls(&[(
            "mcp__fx__slow",
            json!({"release": release.to_string_lossy()}),
        )]),
        Reply::text(),
        Reply::text(),
    ]);
    case.write_models(model.port);
    let mut all = vec!["-p", "--sandbox", "danger-full-access"];
    all.extend_from_slice(args);
    all.extend(["first", "second"]);
    let before = case.events("fx").len();
    case.spawn(&all, false, &[]);
    // The listing request, then the call that follows it in the server's
    // input; the call is held until then.
    let deadline = Instant::now() + BOUND;
    loop {
        let events = case.events("fx");
        let fresh = &events[before.min(events.len())..];
        let listed = fresh.iter().position(|event| event == "list B");
        let called = fresh
            .iter()
            .position(|event| event.starts_with("call slow "));
        if let (Some(listed), Some(called)) = (listed, called) {
            assert!(listed < called, "{fresh:?}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no new listing and call: {fresh:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::write(&release, "go").expect("release");
    let run = case.wait();
    assert_eq!(run.status.code(), Some(0), "{}", run.stderr);
    let requests = model.requests();
    assert_eq!(requests.len(), 4);
    requests
}

// Restrictions apply to every generation a prompt declares, not only the
// first: the second prompt declares the new listing without the excluded
// tool, while the same run without the exclusion declares it.
#[cfg(unix)]
fn restrictions_cover_a_later_listing(case: &Arc<Case>) {
    case.write_config(
        &case.home(),
        &json!({ "mcpServers": { "fx": case.server("fx", "", json!({})) } }),
    );
    let control = later_listing(case, &[]);
    let second = declared(&control[3]);
    assert!(
        second.iter().any(|name| name == "mcp__fx__extra"),
        "{second:?}"
    );
    assert!(
        !second.iter().any(|name| name == "mcp__fx__echo"),
        "{second:?}"
    );

    let restricted = later_listing(case, &["--exclude-tools", "mcp__fx__extra"]);
    let first = declared(&restricted[0]);
    assert!(
        first.iter().any(|name| name == "mcp__fx__echo"),
        "{first:?}"
    );
    let second = declared(&restricted[3]);
    // The new generation was published (no `echo`), and its excluded tool
    // is not declared.
    assert!(
        !second.iter().any(|name| name == "mcp__fx__echo"),
        "{second:?}"
    );
    assert!(
        !second.iter().any(|name| name == "mcp__fx__extra"),
        "{second:?}"
    );
    assert!(
        second.iter().any(|name| name == "mcp__fx__env"),
        "{second:?}"
    );

    let allowlisted = later_listing(
        case,
        &["--tools", "mcp__fx__change,mcp__fx__slow,mcp__fx__env"],
    );
    assert_eq!(
        declared(&allowlisted[3]),
        ["mcp__fx__env", "mcp__fx__slow", "mcp__fx__change"]
    );
}

// Ctrl+C (`SIGINT`) in print mode shuts the session down, awaited, and
// with it every server and its descendants, exiting 130: while setup waits
// for piped input.
#[cfg(unix)]
fn ctrl_c_while_setup_waits_for_input_stops_every_server(case: &Arc<Case>) {
    let model = ModelServer::start(vec![Reply::text()]);
    case.write_models(model.port);
    let (stdin, pids) = start_held(case);
    send_signal(case, libc::SIGINT);
    let run = case.wait();
    assert!(case.all_exited(&pids), "a server process outlived bake-rs");
    drop(stdin);
    assert_eq!(pids.len(), 2, "{pids:?}");
    assert_eq!(run.status.code(), Some(130), "{}", run.stderr);
    assert!(model.requests().is_empty());
}

// While the first prompt waits for a server that never answers
// `initialize`.
#[cfg(unix)]
fn ctrl_c_during_the_startup_wait_stops_every_server(case: &Arc<Case>) {
    let model = ModelServer::start(vec![Reply::text()]);
    case.write_models(model.port);
    case.write_config(
        &case.home(),
        &json!({ "mcpServers": { "fx": case.server("fx", "stall grandchild", json!({})) } }),
    );
    case.spawn(&["-p", "--sandbox", "danger-full-access", "Go"], false, &[]);
    case.event("fx", "initialize");
    let pids = case.observe_server("fx");
    send_signal(case, libc::SIGINT);
    let run = case.wait();
    assert!(case.all_exited(&pids), "a server process outlived bake-rs");
    assert_eq!(pids.len(), 2, "{pids:?}");
    assert_eq!(run.status.code(), Some(130), "{}", run.stderr);
    assert!(model.requests().is_empty());
}

/// Runs `bake-rs -p` with `prompts`, and on model request `at` observes the
/// server, sends `SIGINT`, and holds the reply until `bake-rs` has exited.
#[cfg(unix)]
fn ctrl_c_at_request(case: &Arc<Case>, prompts: &[&str], at: usize) -> (Run, Vec<u32>, usize) {
    let observed: Arc<Mutex<Vec<u32>>> = Arc::default();
    let (hook_case, observed_hook) = (Arc::clone(case), Arc::clone(&observed));
    let model = ModelServer::with_hook(
        vec![Reply::text()],
        Box::new(move |index, _| {
            if index != at {
                return;
            }
            *lock(&observed_hook) = hook_case.observe_server("fx");
            let pid = hook_case.child_pid();
            send_signal(&hook_case, libc::SIGINT);
            wait_exited(pid);
        }),
    );
    case.write_models(model.port);
    case.write_config(
        &case.home(),
        &json!({ "mcpServers": { "fx": case.server("fx", "grandchild", json!({})) } }),
    );
    let mut args = vec!["-p", "--sandbox", "danger-full-access"];
    args.extend_from_slice(prompts);
    case.spawn(&args, false, &[]);
    let run = case.wait();
    let pids = lock(&observed).clone();
    assert!(case.all_exited(&pids), "a server process outlived bake-rs");
    model.assert_hook_passed();
    (run, pids, model.requests().len())
}

// While a prompt's model request is in flight.
#[cfg(unix)]
fn ctrl_c_during_a_prompt_stops_every_server(case: &Arc<Case>) {
    let (run, pids, requests) = ctrl_c_at_request(case, &["Go"], 0);
    assert_eq!(pids.len(), 2, "{pids:?}");
    assert_eq!(run.status.code(), Some(130), "{}", run.stderr);
    assert_eq!(requests, 1);
    assert_eq!(run.stdout, "");
}

// After the first prompt finished, while the next one runs.
#[cfg(unix)]
fn ctrl_c_after_a_finished_prompt_stops_every_server(case: &Arc<Case>) {
    let (run, pids, requests) = ctrl_c_at_request(case, &["first", "second"], 1);
    assert_eq!(pids.len(), 2, "{pids:?}");
    assert_eq!(run.status.code(), Some(130), "{}", run.stderr);
    assert_eq!(requests, 2);
    let lines = session_lines(case);
    assert!(
        lines
            .iter()
            .any(|line| line["message"]["role"] == "assistant"),
        "the first prompt finished and was logged"
    );
}
