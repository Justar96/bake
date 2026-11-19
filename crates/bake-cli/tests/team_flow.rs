//! The agent team end to end: real `bake --mode rpc` processes in one
//! project with telecom on, and the agents they spawn, against a loopback
//! model server whose replies are scripted from what each request holds.
//! Each test owns its Bake home, project, telecom directory, server, and
//! every process it starts; whatever an assertion leaves running is
//! killed when the test's [`Team`] is dropped.
//!
//! The proofs are about the flow, not its wording: crossed waiting asks
//! both end without their timeout and both get answered; a parent and
//! the agent it spawned can ask each other and settle, and stopping the
//! parent leaves no process behind; a spawn past the cap is queued and
//! starts when a slot frees; and a spawned agent that dies or cannot
//! start is reported to its parent's model. Each bound is the harness's
//! [`TIMEOUT`], far below the asks' own 600 s.
#![cfg(target_os = "linux")]

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long any one thing the tests wait for may take.
const TIMEOUT: Duration = Duration::from_secs(90);

/// How often a wait looks again.
const POLL: Duration = Duration::from_millis(20);

/// Polls `check` until it returns something, failing with `what` and
/// `context` after [`TIMEOUT`].
fn wait_for<T>(what: &str, context: impl Fn() -> String, check: impl Fn() -> Option<T>) -> T {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(found) = check() {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "{what} never happened: {}",
            context()
        );
        std::thread::sleep(POLL);
    }
}

// ---------------------------------------------------------------------
// The scripted model.

/// What one model request held, as the scripts read it.
#[derive(Debug, Clone)]
struct Seen {
    /// The text of each user message, in order.
    user: Vec<String>,
    /// The tool calls the model made: name, arguments, and call id.
    calls: Vec<(String, Value, String)>,
    /// The tool results: call id and text.
    outputs: Vec<(String, String)>,
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

impl Seen {
    fn read(body: &Value) -> Self {
        let mut seen = Self {
            user: Vec::new(),
            calls: Vec::new(),
            outputs: Vec::new(),
        };
        for item in body["input"].as_array().into_iter().flatten() {
            match (item["type"].as_str(), item["role"].as_str()) {
                (Some("function_call"), _) => seen.calls.push((
                    item["name"].as_str().unwrap_or_default().to_owned(),
                    item["arguments"]
                        .as_str()
                        .and_then(|arguments| serde_json::from_str(arguments).ok())
                        .unwrap_or(Value::Null),
                    item["call_id"].as_str().unwrap_or_default().to_owned(),
                )),
                (Some("function_call_output"), _) => seen.outputs.push((
                    item["call_id"].as_str().unwrap_or_default().to_owned(),
                    text_of(&item["output"]),
                )),
                (_, Some("user")) => seen.user.push(text_of(&item["content"])),
                _ => {}
            }
        }
        seen
    }

    /// Whether the first user message besides telecom's brief, the prompt
    /// or the spawned agent's task, contains `text`: who is asking. Later messages may quote
    /// another party's task, as a spawn notice does.
    fn is(&self, text: &str) -> bool {
        // A team brief may come before it.
        self.user
            .iter()
            .find(|message| !message.starts_with("[telecom]"))
            .is_some_and(|prompt| prompt.contains(text))
    }

    /// Whether a user message contains `text`.
    fn heard(&self, text: &str) -> bool {
        self.user.iter().any(|message| message.contains(text))
    }

    /// Whether a user message or a tool result contains `text`.
    fn has(&self, text: &str) -> bool {
        self.heard(text) || self.outputs.iter().any(|(_, output)| output.contains(text))
    }

    /// The calls of `name` with `action` (or any action when empty).
    fn called(&self, name: &str, action: &str) -> Vec<&Value> {
        self.calls
            .iter()
            .filter(|(called, arguments, _)| {
                called == name && (action.is_empty() || arguments["action"] == json!(action))
            })
            .map(|(_, arguments, _)| arguments)
            .collect()
    }

    /// The result of the call whose arguments match `matches`.
    fn output_of(&self, matches: impl Fn(&str, &Value) -> bool) -> Vec<(Value, String)> {
        self.calls
            .iter()
            .filter(|(name, arguments, _)| matches(name, arguments))
            .filter_map(|(_, arguments, id)| {
                self.outputs
                    .iter()
                    .find(|(output, _)| output == id)
                    .map(|(_, text)| (arguments.clone(), text.clone()))
            })
            .collect()
    }

    /// The first ask put to this session that it has not replied to.
    fn owed(&self) -> Option<String> {
        let replied: HashSet<String> = self
            .called("telecom", "reply")
            .into_iter()
            .filter_map(|arguments| arguments["askId"].as_str().map(str::to_owned))
            .collect();
        self.user.iter().find_map(|message| {
            let (_, rest) = message.split_once(" asks you (ask ")?;
            let (id, _) = rest.split_once(')')?;
            (!replied.contains(id)).then(|| id.to_owned())
        })
    }
}

/// A model response: a server-sent event stream.
#[derive(Clone)]
struct Reply(String);

fn stream(events: Vec<Value>) -> Reply {
    Reply(
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

const USAGE: &str = r#"{"input_tokens": 40, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 8, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 48}"#;

fn response(id: &str, status: &str, output: Value) -> Value {
    let mut response = json!({"id": id, "object": "response", "created_at": 1_760_000_000, "status": status, "model": "loop-model", "output": output});
    if status == "completed" {
        response["usage"] = serde_json::from_str(USAGE).expect("usage");
    }
    response
}

/// An answer of `text`.
fn say(text: &str) -> Reply {
    let item = json!({"id": "msg_1", "type": "message", "status": "completed", "role": "assistant", "content": [{"type": "output_text", "annotations": [], "text": text}]});
    stream(vec![
        json!({"type": "response.created", "response": response("resp_text", "in_progress", json!([]))}),
        json!({"type": "response.output_item.added", "output_index": 0, "item": {"id": "msg_1", "type": "message", "status": "in_progress", "role": "assistant", "content": []}}),
        json!({"type": "response.content_part.added", "item_id": "msg_1", "output_index": 0, "content_index": 0, "part": {"type": "output_text", "annotations": [], "text": ""}}),
        json!({"type": "response.output_text.delta", "item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": text}),
        json!({"type": "response.output_text.done", "item_id": "msg_1", "output_index": 0, "content_index": 0, "text": text}),
        json!({"type": "response.content_part.done", "item_id": "msg_1", "output_index": 0, "content_index": 0, "part": {"type": "output_text", "annotations": [], "text": text}}),
        json!({"type": "response.output_item.done", "output_index": 0, "item": item}),
        json!({"type": "response.completed", "response": response("resp_text", "completed", json!([item]))}),
    ])
}

/// One response holding `calls`, in order, each with a call id unique in
/// the test binary, so no conversation repeats one.
fn call_all(calls: &[(&str, Value)]) -> Reply {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    let mut events = vec![
        json!({"type": "response.created", "response": response("resp_call", "in_progress", json!([]))}),
    ];
    let mut items = Vec::new();
    for (index, (name, arguments)) in calls.iter().enumerate() {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let (item_id, call_id) = (format!("fc_{n}"), format!("call_{n}"));
        let item = json!({"id": item_id, "type": "function_call", "status": "completed", "call_id": call_id, "name": name, "arguments": arguments.to_string()});
        events.push(json!({"type": "response.output_item.added", "output_index": index, "item": {"id": item_id, "type": "function_call", "status": "in_progress", "call_id": call_id, "name": name, "arguments": ""}}));
        events.push(json!({"type": "response.function_call_arguments.done", "item_id": item_id, "output_index": index, "arguments": arguments.to_string()}));
        events.push(
            json!({"type": "response.output_item.done", "output_index": index, "item": item}),
        );
        items.push(item);
    }
    events.push(
        json!({"type": "response.completed", "response": response("resp_call", "completed", json!(items))}),
    );
    stream(events)
}

fn call(name: &str, arguments: Value) -> Reply {
    call_all(&[(name, arguments)])
}

/// Picks the reply to a request, or `None` to hold it unanswered until
/// the client hangs up or the server stops.
type Script = Arc<dyn Fn(&Seen) -> Option<Reply> + Send + Sync>;

/// A loopback model server answering each request on its own thread with
/// what its script picks; stopped and joined on drop.
struct ModelServer {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ModelServer {
    fn start(script: Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("address").port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (seen, stop) = (Arc::clone(&seen), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut connections = Vec::new();
                while !stop.load(Ordering::SeqCst) {
                    let Ok((socket, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    let (seen, stop, script) =
                        (Arc::clone(&seen), Arc::clone(&stop), Arc::clone(&script));
                    connections.push(std::thread::spawn(move || {
                        serve(socket, &script, &seen, &stop);
                    }));
                }
                for connection in connections {
                    let _ = connection.join();
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

    /// Every request so far, in arrival order.
    fn seen(&self) -> Vec<Seen> {
        self.seen
            .lock()
            .map(|seen| seen.clone())
            .unwrap_or_default()
    }

    /// The position of the first request `matches` picks, waited for.
    fn first(&self, what: &str, matches: impl Fn(&Seen) -> bool) -> usize {
        wait_for(
            what,
            || format!("{:#?}", self.seen()),
            || self.seen().iter().position(&matches),
        )
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

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn serve(mut socket: TcpStream, script: &Script, seen: &Mutex<Vec<Seen>>, stop: &AtomicBool) {
    let _ = socket.set_nonblocking(false);
    let _ = socket.set_read_timeout(Some(Duration::from_millis(100)));
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 16384];
    let deadline = Instant::now() + TIMEOUT;
    let body = loop {
        if stop.load(Ordering::SeqCst) || Instant::now() > deadline {
            return;
        }
        match socket.read(&mut chunk) {
            Ok(0) => return,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            Err(_) => {}
        }
        let Some(head_end) = find(&buffer, b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&buffer[..head_end]).to_lowercase();
        let length = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        if buffer.len() >= head_end + 4 + length {
            break serde_json::from_slice::<Value>(&buffer[head_end + 4..head_end + 4 + length])
                .unwrap_or(Value::Null);
        }
    };
    let request = Seen::read(&body);
    if let Ok(mut seen) = seen.lock() {
        seen.push(request.clone());
    }
    match script(&request) {
        Some(Reply(body)) => {
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes());
            let _ = socket.flush();
        }
        // Held: until the client hangs up or the server stops.
        None => loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            match socket.read(&mut chunk) {
                Ok(0) => return,
                Err(error)
                    if !matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return;
                }
                _ => {}
            }
        },
    }
}

/// Holds each party's first request until every party's has arrived, so
/// their tool calls start together.
struct Barrier {
    parties: usize,
    arrived: Mutex<HashSet<String>>,
    all: Condvar,
}

impl Barrier {
    fn new(parties: usize) -> Arc<Self> {
        Arc::new(Self {
            parties,
            arrived: Mutex::new(HashSet::new()),
            all: Condvar::new(),
        })
    }

    fn arrive(&self, who: &str) {
        let mut arrived = self.arrived.lock().expect("barrier");
        arrived.insert(who.to_owned());
        self.all.notify_all();
        let (arrived, _) = self
            .all
            .wait_timeout_while(arrived, TIMEOUT, |arrived| arrived.len() < self.parties)
            .expect("barrier");
        assert_eq!(arrived.len(), self.parties, "not every party arrived");
    }
}

// ---------------------------------------------------------------------
// The processes.

/// A temporary Bake home, project, and telecom directory, and the model
/// route to the server on `port`. Dropped, it kills every process still
/// running under its home, then removes it.
struct Team {
    root: PathBuf,
}

impl Team {
    fn new(name: &str, port: u16) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        // Short: the telecom sockets live under it.
        let root = std::env::temp_dir().join(format!(
            "btf-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["home", "project", "user", "t"] {
            std::fs::create_dir_all(root.join(dir)).expect("team directory");
        }
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

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    /// `bake --mode rpc` in the project, in the team as `callsign`.
    fn rpc(&self, callsign: &str) -> Client {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bake"));
        command
            .args(["--mode", "rpc", "--model", "loop-model"])
            .current_dir(self.root.join("project"))
            .env("BAKE_HOME", self.home())
            .env_remove("CLIPROXYAPI_API_KEY")
            .env("HOME", self.root.join("user"))
            .env("BAKE_TELECOM", "on")
            .env("BAKE_TELECOM_DIR", self.root.join("t"))
            .env("BAKE_TELECOM_CALLSIGN", callsign)
            .env_remove("BAKE_TELECOM_PARENT")
            .env_remove("BAKE_SPAWN_DEPTH")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("PI_") {
                command.env_remove(key);
            }
        }
        Client::start(command)
    }

    /// The processes running under this team's home, with their
    /// environment: its sessions and the agents they spawned.
    fn processes(&self) -> Vec<(i32, Vec<String>)> {
        let marker = format!("BAKE_HOME={}", self.home().display());
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
            .filter_map(|pid| {
                let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
                let vars: Vec<String> = environ
                    .split(|byte| *byte == 0)
                    .map(|var| String::from_utf8_lossy(var).into_owned())
                    .collect();
                // A zombie has an empty environment and is not running.
                vars.contains(&marker).then_some((pid, vars))
            })
            .collect()
    }

    /// The pid of the spawned agent that took `callsign`, waited for.
    fn agent(&self, callsign: &str) -> i32 {
        let var = format!("BAKE_TELECOM_CALLSIGN={callsign}");
        wait_for(
            &format!("a process for {callsign}"),
            || format!("{:?}", self.processes()),
            || {
                self.processes()
                    .into_iter()
                    .find(|(_, vars)| vars.contains(&var))
                    .map(|(pid, _)| pid)
            },
        )
    }

    /// Waits until no process runs under this team's home.
    fn all_gone(&self) {
        wait_for(
            "every process under the team's home exiting",
            || format!("{:?}", self.processes()),
            || self.processes().is_empty().then_some(()),
        );
    }
}

impl Drop for Team {
    fn drop(&mut self) {
        for (pid, _) in self.processes() {
            // SAFETY: `kill` only sends a signal to a process this test
            // started under its own home.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A test client of one `bake --mode rpc`: commands in as JSONL, records
/// out. Dropped, it kills the process if it still runs and reaps it.
struct Client {
    child: Child,
    input: Option<ChildStdin>,
    records: Arc<Mutex<Vec<Value>>>,
    stderr: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>,
}

impl Client {
    fn start(mut command: Command) -> Self {
        let mut child = command.spawn().expect("bake starts");
        let input = child.stdin.take();
        let records = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(String::new()));
        let mut readers = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            let records = Arc::clone(&records);
            readers.push(std::thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                let mut line = Vec::new();
                while reader.read_until(b'\n', &mut line).unwrap_or(0) > 0 {
                    if let Ok(record) = serde_json::from_slice::<Value>(&line) {
                        records.lock().expect("records").push(record);
                    }
                    line.clear();
                }
            }));
        }
        if let Some(mut pipe) = child.stderr.take() {
            let stderr = Arc::clone(&stderr);
            readers.push(std::thread::spawn(move || {
                let mut text = String::new();
                let _ = pipe.read_to_string(&mut text);
                stderr.lock().expect("stderr").push_str(&text);
            }));
        }
        Self {
            child,
            input,
            records,
            stderr,
            readers,
        }
    }

    fn prompt(&mut self, id: &str, message: &str) {
        let line = format!(
            "{}\n",
            json!({ "id": id, "type": "prompt", "message": message })
        );
        let input = self.input.as_mut().expect("input open");
        input.write_all(line.as_bytes()).expect("write a command");
        input.flush().expect("flush the command");
    }

    fn records(&self) -> Vec<Value> {
        self.records
            .lock()
            .map(|records| records.clone())
            .unwrap_or_default()
    }

    /// The records, cut to what tells a run's course, and standard error.
    fn context(&self) -> String {
        let records: Vec<String> = self
            .records()
            .iter()
            .filter(|record| record["type"] != json!("message_update"))
            .map(|record| match record["type"].as_str() {
                Some("tool_execution_end") => format!(
                    "tool_execution_end {} {}",
                    record["toolName"], record["result"]["details"]
                ),
                Some("message_start" | "message_end") => {
                    format!("{} {}", record["type"], record["message"]["role"])
                }
                _ => {
                    let text = record.to_string();
                    text.chars().take(300).collect()
                }
            })
            .collect();
        format!(
            "records: {records:#?}\nstderr: {}",
            self.stderr
                .lock()
                .map(|text| text.clone())
                .unwrap_or_default()
        )
    }

    /// Waits until a run has settled and none has started since. A notice
    /// turn may continue a run, so starts and settlements need not pair.
    fn settled(&self) {
        wait_for(
            "the session settling",
            || self.context(),
            || {
                self.records()
                    .iter()
                    .rev()
                    .find(|record| {
                        record["type"] == json!("agent_start")
                            || record["type"] == json!("agent_settled")
                    })
                    .is_some_and(|record| record["type"] == json!("agent_settled"))
                    .then_some(())
            },
        );
    }

    /// The results of the `telecom ask` calls that ended, as their
    /// details' status.
    fn ask_statuses(&self) -> Vec<String> {
        self.records()
            .iter()
            .filter(|record| {
                record["type"] == json!("tool_execution_end")
                    && record["result"]["details"]["action"] == json!("ask")
            })
            .map(|record| {
                record["result"]["details"]["status"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }

    /// Closes its input, RPC mode's orderly shutdown, and waits for it.
    fn close(&mut self) -> ExitStatus {
        drop(self.input.take());
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().expect("bake status") {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "bake never exited: {}",
                self.context()
            );
            std::thread::sleep(POLL);
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        drop(self.input.take());
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

/// The ask both parties put in the crossed-asks scripts.
fn ask(to: &str) -> Reply {
    call(
        "telecom",
        json!({ "action": "ask", "to": to, "message": format!("{to}, which port do you use?"), "wait": true, "timeoutSec": 600 }),
    )
}

/// After its own ask, a party answers what it owes, then says it is done.
fn answer_owed(me: &str, seen: &Seen) -> Reply {
    match seen.owed() {
        Some(id) => call(
            "telecom",
            json!({ "action": "reply", "askId": id, "message": format!("{me} uses port 7") }),
        ),
        None => say(&format!("{me} is done")),
    }
}

// ---------------------------------------------------------------------
// The scenarios.

// Two sessions ask each other at the same moment, each waiting up to ten
// minutes. Each wait ends as soon as the other's ask arrives, as
// interrupted, with that ask delivered next; each answers it, gets its own
// answer as a message, and settles.
#[test]
fn crossed_waiting_asks_both_end_early_and_both_get_answered() {
    let barrier = Barrier::new(2);
    let script: Script = Arc::new(move |seen: &Seen| {
        let (me, peer) = if seen.is("CROSS alpha") {
            ("alpha", "bravo")
        } else if seen.is("CROSS bravo") {
            ("bravo", "alpha")
        } else {
            return Some(say("unscripted"));
        };
        if seen.called("telecom", "ask").is_empty() {
            barrier.arrive(me);
            return Some(ask(peer));
        }
        Some(answer_owed(me, seen))
    });
    let server = ModelServer::start(script);
    let team = Team::new("cross", server.port);
    let mut alpha = team.rpc("alpha");
    let mut bravo = team.rpc("bravo");
    alpha.prompt("a", "CROSS alpha");
    bravo.prompt("b", "CROSS bravo");

    for (me, peer) in [("alpha", "bravo"), ("bravo", "alpha")] {
        server.first(&format!("{me} reading {peer}'s answer"), |seen| {
            seen.is(&format!("CROSS {me}")) && seen.has(&format!("{peer} uses port 7"))
        });
    }
    for (name, client) in [("alpha", &alpha), ("bravo", &bravo)] {
        client.settled();
        assert_eq!(
            client.ask_statuses(),
            ["interrupted"],
            "{name}'s ask: {}",
            client.context()
        );
    }
    assert!(alpha.close().success(), "{}", alpha.context());
    assert!(bravo.close().success(), "{}", bravo.context());
    team.all_gone();
}

// A parent spawns an agent and asks it, waiting, while the agent asks the
// parent, waiting. Neither wait holds the other: both are answered, the
// agent's run settles, and the parent is told. Closing the parent then
// ends the agent too: no process of the team is left.
#[test]
fn a_parent_and_its_spawned_agent_answer_each_other_and_stop_together() {
    let barrier = Barrier::new(2);
    let script: Script = Arc::new(move |seen: &Seen| {
        if seen.is("CHILD ask your parent") {
            if seen.called("telecom", "ask").is_empty() {
                barrier.arrive("child");
                return Some(ask("alpha"));
            }
            return Some(answer_owed("alpha-1", seen));
        }
        if seen.is("PARENT") {
            if seen.called("spawn_agent", "").is_empty() {
                return Some(call(
                    "spawn_agent",
                    json!({ "task": "CHILD ask your parent alpha which port it uses." }),
                ));
            }
            if seen.called("telecom", "ask").is_empty() {
                barrier.arrive("parent");
                return Some(ask("alpha-1"));
            }
            return Some(answer_owed("alpha", seen));
        }
        Some(say("unscripted"))
    });
    let server = ModelServer::start(script);
    let team = Team::new("parent", server.port);
    let mut parent = team.rpc("alpha");
    parent.prompt("p", "PARENT");

    server.first("the agent reading the parent's answer", |seen| {
        seen.is("CHILD ask your parent") && seen.has("alpha uses port 7")
    });
    server.first("the parent reading the agent's answer", |seen| {
        seen.is("PARENT") && seen.has("alpha-1 uses port 7")
    });
    server.first("the parent told the agent's run settled", |seen| {
        seen.is("PARENT") && seen.heard("[spawn] alpha-1, which you started on")
    });
    let child = team.agent("alpha-1");
    parent.settled();

    assert!(parent.close().success(), "{}", parent.context());
    team.all_gone();
    assert!(
        !Path::new(&format!("/proc/{child}")).exists()
            || std::fs::read(format!("/proc/{child}/environ")).is_ok_and(|env| env.is_empty()),
        "the spawned agent {child} outlived its parent"
    );
}

// With six agents running, a seventh spawn_agent is queued with a result
// that says so, and it starts only once the parent stops one of the six.
#[test]
fn a_spawn_past_the_cap_is_queued_and_starts_when_one_is_stopped() {
    const AGENTS: usize = 7;
    let script: Script = Arc::new(|seen: &Seen| {
        if seen.is("FLEET CHILD") {
            return Some(say("child idle"));
        }
        if !seen.is("FLEET") {
            return Some(say("unscripted"));
        }
        let spawns: Vec<(&str, Value)> = (1..=AGENTS)
            .map(|n| {
                (
                    "spawn_agent",
                    json!({ "task": format!("FLEET CHILD {n}.") }),
                )
            })
            .collect();
        if seen.called("spawn_agent", "").is_empty() {
            return Some(call_all(&spawns));
        }
        if seen.called("stop_agent", "").is_empty() {
            let started = seen
                .output_of(|name, _| name == "spawn_agent")
                .into_iter()
                .find_map(|(_, output)| {
                    output
                        .strip_prefix("started ")
                        .and_then(|rest| rest.split_once(';'))
                        .map(|(name, _)| name.to_owned())
                })
                .expect("a started agent");
            return Some(call("stop_agent", json!({ "agent": started })));
        }
        Some(say("fleet done"))
    });
    let server = ModelServer::start(script);
    let team = Team::new("fleet", server.port);
    let mut parent = team.rpc("alpha");
    parent.prompt("p", "FLEET");

    let stopping = server.first("the parent stopping one agent", |seen| {
        seen.is("FLEET")
            && !seen.is("FLEET CHILD")
            && seen.called("stop_agent", "").is_empty()
            && !seen.called("spawn_agent", "").is_empty()
    });
    let results = server.seen()[stopping].output_of(|name, _| name == "spawn_agent");
    assert_eq!(results.len(), AGENTS);
    let queued: Vec<&(Value, String)> = results
        .iter()
        .filter(|(_, output)| output.starts_with("queued "))
        .collect();
    assert_eq!(queued.len(), 1, "{results:#?}");
    let (arguments, output) = queued[0];
    let name = output
        .strip_prefix("queued ")
        .and_then(|rest| rest.split_once(':'))
        .map(|(name, _)| name)
        .expect("a queued name");
    assert_eq!(
        output,
        &format!(
            "queued {name}: 6 agents are running; it starts when one exits or is stopped (stop_agent)"
        )
    );
    let task = arguments["task"].as_str().expect("its task").to_owned();
    let started = server.first("the queued agent starting", |seen| seen.is(&task));
    assert!(
        started > stopping,
        "the queued agent started before one was stopped"
    );
    // Every agent but the one stopped, which may be stopped before it
    // reaches its model, runs.
    let stopped = server.seen()[stopping + 1..]
        .iter()
        .find_map(|seen| {
            seen.called("stop_agent", "")
                .first()
                .and_then(|arguments| arguments["agent"].as_str().map(str::to_owned))
        })
        .expect("the stop call");
    for (arguments, output) in &results {
        if output.starts_with(&format!("started {stopped};")) {
            continue;
        }
        let task = arguments["task"].as_str().expect("a task");
        server.first(&format!("{task} running"), |seen| seen.is(task));
    }
    assert!(parent.close().success(), "{}", parent.context());
    team.all_gone();
}

// A spawned agent killed in the middle of its run is reported to the
// parent's model in a following turn, with why it ended.
#[test]
fn a_spawned_agent_killed_mid_run_is_reported_to_its_parent() {
    let script: Script = Arc::new(|seen: &Seen| {
        if seen.is("DOOMED CHILD") {
            // Its model never answers: the agent is in its run when killed.
            return None;
        }
        if seen.called("spawn_agent", "").is_empty() {
            return Some(call(
                "spawn_agent",
                json!({ "task": "DOOMED CHILD: wait." }),
            ));
        }
        Some(say("noted"))
    });
    let server = ModelServer::start(script);
    let team = Team::new("doomed", server.port);
    let mut parent = team.rpc("alpha");
    parent.prompt("p", "DOOMED");
    server.first("the agent's run starting", |seen| seen.is("DOOMED CHILD"));
    let child = team.agent("alpha-1");
    // SAFETY: a signal to the agent this test's parent spawned.
    assert_eq!(unsafe { libc::kill(child, libc::SIGKILL) }, 0);

    let told = server.first("the parent told the agent exited", |seen| {
        seen.heard("[spawn] alpha-1, which you started on \"DOOMED CHILD: wait.\", exited: ")
    });
    let notice = server.seen()[told]
        .user
        .iter()
        .find(|message| message.contains("[spawn] alpha-1"))
        .cloned()
        .unwrap_or_default();
    assert!(
        notice.contains("SIGKILL") || notice.contains("signal"),
        "{notice}"
    );
    parent.settled();
    assert!(parent.close().success(), "{}", parent.context());
    team.all_gone();
}

// A spawned agent on a model its route does not list starts, and its run
// fails; the parent's model is told so, with the agent's own reason.
#[test]
fn a_spawned_agent_whose_run_fails_on_an_unknown_model_is_reported_to_its_parent() {
    let script: Script = Arc::new(|seen: &Seen| {
        if seen.is("UNKNOWN MODEL CHILD") {
            return Some(say("unreachable: this agent has no model"));
        }
        if seen.called("spawn_agent", "").is_empty() {
            return Some(call(
                "spawn_agent",
                json!({ "task": "UNKNOWN MODEL CHILD", "model": "loop/no-such-model" }),
            ));
        }
        Some(say("noted"))
    });
    let server = ModelServer::start(script);
    let team = Team::new("unknown", server.port);
    let mut parent = team.rpc("alpha");
    parent.prompt("p", "UNKNOWN MODEL");
    server.first("the parent told the agent's run failed", |seen| {
        seen.is("UNKNOWN MODEL")
            && seen.heard(
                "[spawn] alpha-1, which you started on \"UNKNOWN MODEL CHILD\", failed its run: ",
            )
    });
    assert!(
        !server
            .seen()
            .iter()
            .any(|seen| seen.is("UNKNOWN MODEL CHILD")),
        "the agent reached a model"
    );
    parent.settled();
    assert!(parent.close().success(), "{}", parent.context());
    team.all_gone();
}
