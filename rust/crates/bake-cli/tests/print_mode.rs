//! End-to-end print mode: the built `bake-rs` against a loopback HTTP
//! server that answers `POST /v1/responses` with OpenAI Responses event
//! streams (`fixtures/openai-responses-text.sse`, and a `read` tool call in
//! `fixtures/openai-responses-read-call.sse`). Each test owns a temporary
//! Bake home, project directory, and server; nothing reaches the network.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const SSE: &str = include_str!("fixtures/openai-responses-text.sse");
const READ_CALL_SSE: &str = include_str!("fixtures/openai-responses-read-call.sse");

/// How long one `bake-rs` run may take before the test fails.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// A canned reply.
#[derive(Clone)]
struct Reply {
    status: u16,
    content_type: &'static str,
    body: String,
}

impl Reply {
    fn sse() -> Self {
        // The fixture ends with one newline; the stream's last event needs
        // its blank line.
        Self {
            status: 200,
            content_type: "text/event-stream",
            body: format!("{SSE}\n"),
        }
    }

    /// A tool call: the `read` call with its name and arguments replaced.
    fn tool_call(name: &str, arguments: &Value) -> Self {
        let arguments = serde_json::to_string(&arguments.to_string()).expect("arguments");
        let read = serde_json::to_string(&json!({"path": "notes.txt"}).to_string())
            .expect("read arguments");
        let body = READ_CALL_SSE
            .replace("\"name\":\"read\"", &format!("\"name\":\"{name}\""))
            .replace(
                &format!("\"delta\":{read}"),
                &format!("\"delta\":{arguments}"),
            )
            .replace(
                &format!("\"arguments\":{read}"),
                &format!("\"arguments\":{arguments}"),
            );
        Self {
            status: 200,
            content_type: "text/event-stream",
            body: format!("{body}\n"),
        }
    }

    /// A `read` call of `notes.txt`.
    fn read_call() -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream",
            body: format!("{READ_CALL_SSE}\n"),
        }
    }
}

/// One request the server received.
#[derive(Debug, Clone)]
struct Request {
    line: String,
    headers: Vec<(String, String)>,
    body: Value,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// A loopback server answering its connections with `replies` in turn,
/// repeating the last; stopped and joined on drop.
struct Server {
    port: u16,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn handle(mut socket: TcpStream, replies: &[Reply], requests: &Mutex<Vec<Request>>) {
    let _ = socket.set_read_timeout(Some(Duration::from_secs(10)));
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        match socket.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
        if let Some(end) = find(&buffer, b"\r\n\r\n") {
            break end;
        }
        if buffer.len() > 1 << 20 {
            return;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let line = lines
        .next()
        .unwrap_or("")
        .rsplit_once(' ')
        .map(|(line, _)| line.to_owned())
        .unwrap_or_default();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_lowercase(), value.trim().to_owned()))
        .collect();
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0)
        .min(16 << 20);
    while buffer.len() < head_end + 4 + length {
        match socket.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    }
    let body = buffer
        .get(head_end + 4..)
        .and_then(|body| serde_json::from_slice(body).ok())
        .unwrap_or(Value::Null);
    let seen = match requests.lock() {
        Ok(mut requests) => {
            requests.push(Request {
                line,
                headers,
                body,
            });
            requests.len() - 1
        }
        Err(_) => 0,
    };
    let reply = &replies[seen.min(replies.len() - 1)];
    let response = format!(
        "HTTP/1.1 {} X\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        reply.status,
        reply.content_type,
        reply.body.len(),
        reply.body
    );
    let _ = socket.write_all(response.as_bytes());
    let _ = socket.flush();
}

impl Server {
    fn start(reply: Reply) -> Self {
        Self::start_sequence(vec![reply])
    }

    fn start_sequence(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("address").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let requests = Arc::clone(&requests);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((socket, _)) => {
                            let _ = socket.set_nonblocking(false);
                            handle(socket, &replies, &requests);
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
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

    fn requests(&self) -> Vec<Request> {
        self.requests
            .lock()
            .map(|requests| requests.clone())
            .unwrap_or_default()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A temporary Bake home and project directory, removed on drop.
struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn new(name: &str, port: u16) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "bake-rs-print-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).expect("home");
        std::fs::create_dir_all(root.join("project")).expect("project");
        std::fs::create_dir_all(root.join("user")).expect("user home");
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

    fn project(&self) -> PathBuf {
        self.root.join("project")
    }

    /// Runs `bake-rs` in the project, failing the test if it takes longer
    /// than [`RUN_TIMEOUT`] rather than hanging it.
    fn run(&self, args: &[&str], stdin: Option<&str>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bake-rs"));
        command
            .args(args)
            .current_dir(self.project())
            .env("BAKE_HOME", self.home())
            .env_remove("DSH_HOME")
            .env_remove("CLIPROXYAPI_API_KEY")
            .env("HOME", self.root.join("user"))
            .env("USERPROFILE", self.root.join("user"))
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("bake-rs starts");
        let input = child.stdin.take();
        let text = stdin.unwrap_or_default().to_owned();
        let writer = std::thread::spawn(move || {
            if let Some(mut input) = input {
                // The child may exit without reading; that is its answer.
                let _ = input.write_all(text.as_bytes());
            }
        });
        let drain = |pipe: Option<Box<dyn Read + Send>>| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                if let Some(mut pipe) = pipe {
                    let _ = pipe.read_to_end(&mut bytes);
                }
                bytes
            })
        };
        let stdout = drain(child.stdout.take().map(|pipe| Box::new(pipe) as _));
        let stderr = drain(child.stderr.take().map(|pipe| Box::new(pipe) as _));
        let deadline = Instant::now() + RUN_TIMEOUT;
        let status = loop {
            match child.try_wait().expect("bake-rs status") {
                Some(status) => break status,
                None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("bake-rs {args:?} ran longer than {RUN_TIMEOUT:?}");
                }
            }
        };
        let _ = writer.join();
        Output {
            status,
            stdout: stdout.join().expect("stdout reader"),
            stderr: stderr.join().expect("stderr reader"),
        }
    }

    /// Every session file under the home.
    fn session_files(&self) -> Vec<PathBuf> {
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
        let mut out = Vec::new();
        walk(&self.home().join("sessions"), &mut out);
        out.sort();
        out
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .expect("session file")
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON line"))
        .collect()
}

/// `type` or `type:role` of each session line.
fn kinds(entries: &[Value]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| {
            let kind = entry["type"].as_str().unwrap_or_default();
            match entry["message"]["role"].as_str() {
                Some(role) => format!("{kind}:{role}"),
                None => kind.to_owned(),
            }
        })
        .collect()
}

/// The `input` items of a Responses request as `role: text` lines.
fn transcript(request: &Request) -> Vec<String> {
    request.body["input"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| {
            let role = item["role"]
                .as_str()
                .unwrap_or(item["type"].as_str().unwrap_or("?"));
            let text = match &item["content"] {
                Value::String(text) => text.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            };
            format!("{role}: {text}")
        })
        .collect()
}

/// The `tools` and `rules` sections Pi v1.1.0 builds for `selected`,
/// recorded by `bake-coding-agent/tests/fixtures/pi-tools/generate.ts`.
fn pi_prompt_sections(selected: &[&str]) -> (String, String) {
    let golden: Value = serde_json::from_str(include_str!(
        "../../bake-coding-agent/tests/fixtures/pi-tools/golden.json"
    ))
    .expect("golden.json");
    let case = golden["prompts"]
        .as_array()
        .expect("prompts")
        .iter()
        .find(|case| case["selected"] == json!(selected))
        .expect("a recorded selection");
    (
        case["tools"].as_str().expect("tools").to_owned(),
        case["rules"].as_str().expect("rules").to_owned(),
    )
}

#[test]
fn print_mode_answers_saves_and_continues() {
    let server = Server::start(Reply::sse());
    let workspace = Workspace::new("continue", server.port);

    let first = workspace.run(&["-p", "Say hi"], None);
    assert_eq!(text(&first.stderr), "");
    assert_eq!(first.status.code(), Some(0));
    assert_eq!(text(&first.stdout), "Hello from the loop.\n");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].line, "POST /v1/responses");
    assert_eq!(requests[0].header("authorization"), Some("Bearer test-key"));
    assert_eq!(requests[0].body["model"], "loop-model");
    let first_input = transcript(&requests[0]);
    assert_eq!(first_input.len(), 2, "{first_input:?}");
    assert!(
        first_input[0]
            .starts_with("system: You are an expert coding assistant operating inside pi")
    );
    // Pi's default tools.
    let (tools, rules) = pi_prompt_sections(&["read", "bash", "edit", "write"]);
    assert!(
        first_input[0].contains(&format!("\n{tools}\n")),
        "{}",
        first_input[0]
    );
    assert!(
        first_input[0].contains(&format!("\n{rules}\n")),
        "{}",
        first_input[0]
    );
    let declared: Vec<&str> = requests[0].body["tools"]
        .as_array()
        .expect("declared tools")
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(declared, ["read", "bash", "edit", "write"]);
    assert_eq!(first_input[1], "user: Say hi");

    let files = workspace.session_files();
    assert_eq!(files.len(), 1, "{files:?}");
    let entries = lines(&files[0]);
    assert_eq!(
        kinds(&entries),
        [
            "session",
            "custom",
            "model_change",
            "thinking_level_change",
            "message:system",
            "message:user",
            "message:assistant",
        ]
    );
    // D24: a new session pins the default preset, outside the model's view.
    assert_eq!(entries[1]["customType"], "bake.permission");
    assert_eq!(
        entries[1]["data"],
        json!({"preset": "workspace-write", "sandbox": "workspace-write", "approval": "ask"})
    );
    // The child reads its working directory with the links resolved, as
    // macOS's `/var` to `/private/var`.
    let header_cwd = entries[0]["cwd"].as_str().expect("header cwd");
    assert_eq!(
        std::fs::canonicalize(header_cwd).expect("header cwd exists"),
        std::fs::canonicalize(workspace.project()).expect("project exists")
    );
    assert_eq!(entries[2]["provider"], "loop");
    assert_eq!(entries[2]["modelId"], "loop-model");
    assert_eq!(entries[3]["thinkingLevel"], "off");
    assert_eq!(
        entries[6]["message"]["content"][0]["text"],
        "Hello from the loop."
    );
    assert_eq!(entries[6]["message"]["usage"]["totalTokens"], 48);

    // `--continue` sends the earlier turn and appends to the same file.
    let second = workspace.run(&["-p", "--continue", "Again"], None);
    assert_eq!(text(&second.stderr), "");
    assert_eq!(second.status.code(), Some(0));
    assert_eq!(text(&second.stdout), "Hello from the loop.\n");
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let second_input = transcript(&requests[1]);
    assert_eq!(second_input.len(), 4, "{second_input:?}");
    assert_eq!(second_input[0], first_input[0]);
    assert_eq!(second_input[1], "user: Say hi");
    assert_eq!(second_input[2], "assistant: Hello from the loop.");
    assert_eq!(second_input[3], "user: Again");
    assert_eq!(workspace.session_files(), files);
    let entries = lines(&files[0]);
    // The resumed session keeps its permission and writes no more of it.
    assert_eq!(kinds(&entries)[7..], ["message:user", "message:assistant"]);

    // `--session <path>` resumes the same file; `--no-session` writes none.
    let third = workspace.run(
        &["-p", "--session", &files[0].to_string_lossy(), "Third"],
        None,
    );
    assert_eq!(third.status.code(), Some(0), "{}", text(&third.stderr));
    assert_eq!(transcript(&server.requests()[2]).len(), 6);
    let fourth = workspace.run(&["-p", "--no-session", "Fresh"], None);
    assert_eq!(fourth.status.code(), Some(0), "{}", text(&fourth.stderr));
    assert_eq!(transcript(&server.requests()[3]).len(), 2);
    assert_eq!(workspace.session_files(), files);
    assert_eq!(lines(&files[0]).len(), 11);
}

#[test]
fn piped_stdin_is_prepended_to_the_first_message() {
    let server = Server::start(Reply::sse());
    let workspace = Workspace::new("stdin", server.port);
    let out = workspace.run(
        &["--no-session", "--model", "loop/loop-model", "tail"],
        Some("  piped text \n"),
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "Hello from the loop.\n");
    let input = transcript(&server.requests()[0]);
    assert_eq!(input[1], "user: piped texttail");
}

#[test]
fn json_mode_writes_the_header_and_every_event() {
    let server = Server::start(Reply::sse());
    let workspace = Workspace::new("json", server.port);
    let out = workspace.run(&["--mode", "json", "--no-session", "Say hi"], None);
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
    assert_eq!(
        types[1..5],
        ["agent_start", "turn_start", "message_start", "message_end"]
    );
    assert_eq!(events[3]["message"]["role"], "system");
    assert!(types.contains(&"message_update"));
    assert_eq!(
        types[types.len() - 4..],
        ["message_end", "turn_end", "agent_end", "agent_settled"]
    );
    let update = events
        .iter()
        .find(|event| event["type"] == "message_update")
        .expect("an update");
    assert!(update["assistantMessageEvent"].get("partial").is_none());
    assert!(update.get("usage").is_some());
    assert_eq!(events[events.len() - 2]["willRetry"], false);
    assert_eq!(events[events.len() - 1]["aborted"], false);
    assert!(workspace.session_files().is_empty());
}

#[test]
fn provider_errors_exit_one_with_the_message() {
    let server = Server::start(Reply {
        status: 401,
        content_type: "application/json",
        body: r#"{"error":{"message":"bad key"}}"#.into(),
    });
    let workspace = Workspace::new("error", server.port);
    let out = workspace.run(&["-p", "--no-session", "Say hi"], None);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "");
    assert_eq!(
        text(&out.stderr),
        "loop API error (401): {\"message\":\"bad key\"}\n"
    );
}

#[test]
fn startup_errors_follow_pi() {
    let server = Server::start(Reply::sse());
    let workspace = Workspace::new("startup", server.port);
    let out = workspace.run(&["-p", "--model", "nope/missing", "hi"], None);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        "Error: Model \"nope/missing\" not found. Use --list-models to see available models.\n"
    );
    let out = workspace.run(&["-p", "--provider", "loop", "hi"], None);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        "Error: --provider requires --model (for example: --provider loop --model <pattern>)\n"
    );
    let out = workspace.run(&["-p", "--session", "nothing-like-it", "hi"], None);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        // Pi prints this one without the `Error: ` prefix.
        "No session found matching 'nothing-like-it'\n"
    );
    std::fs::remove_file(workspace.home().join("models.json")).expect("remove models.json");
    let out = workspace.run(&["-p", "hi"], None);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).starts_with("No models available. "));
    assert!(server.requests().is_empty());
}

/// D25: with no `models.json` and no Pi settings, the 0.3 home's CLIProxyAPI
/// route, key, and default model run the turn, and neither 0.3 file changes.
#[test]
fn the_imported_cliproxyapi_route_runs_a_turn() {
    let server = Server::start(Reply::sse());
    let workspace = Workspace::new("cliproxyapi", server.port);
    std::fs::remove_file(workspace.home().join("models.json")).expect("remove models.json");
    let settings = format!(
        "llm-pi-ai:\n  providers:\n    cliproxyapi:\n      displayName: CLIProxyAPI\n      apiKeyEnv: CLIPROXYAPI_API_KEY\n      api: openai-responses\n      baseURL: http://127.0.0.1:{}/v1\n      models:\n        - id: proxy-model\n          name: Proxy Model\n          contextWindow: 200000\n          maxTokens: 32000\n          input: [text]\n          reasoningEfforts: {{low: low, medium: medium, high: high}}\nagent-default-model:\n  provider: cliproxyapi\n  model: proxy-model\n  reasoningEffort: high\n",
        server.port
    );
    let credentials = "version: 1\nrefs:\n  CLIPROXYAPI_API_KEY: proxy-test-key\n";
    std::fs::write(workspace.home().join("settings.yaml"), &settings).expect("settings.yaml");
    std::fs::write(workspace.home().join(".credentials.yaml"), credentials)
        .expect(".credentials.yaml");
    // The importer refuses a credentials file others can read, as 0.3 does.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            workspace.home().join(".credentials.yaml"),
            std::fs::Permissions::from_mode(0o600),
        )
        .expect("chmod 600");
    }

    let output = workspace.run(&["-p", "Say hi"], None);
    assert_eq!(text(&output.stderr), "");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(text(&output.stdout), "Hello from the loop.\n");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].line, "POST /v1/responses");
    assert_eq!(
        requests[0].header("authorization"),
        Some("Bearer proxy-test-key")
    );
    assert_eq!(requests[0].body["model"], "proxy-model");

    let entries = lines(&workspace.session_files()[0]);
    assert_eq!(entries[2]["provider"], "cliproxyapi");
    assert_eq!(entries[2]["modelId"], "proxy-model");
    assert_eq!(entries[3]["thinkingLevel"], "high");
    assert_eq!(
        std::fs::read_to_string(workspace.home().join("settings.yaml")).expect("settings"),
        settings
    );
    assert_eq!(
        std::fs::read_to_string(workspace.home().join(".credentials.yaml")).expect("creds"),
        credentials
    );
}

/// Pi's agent loop through `bake-rs -p`: the model calls `read` on a
/// project file, the tool result goes back to it, and the session file
/// records the call and its result.
#[test]
fn print_mode_runs_the_read_tool() {
    let server = Server::start_sequence(vec![Reply::read_call(), Reply::sse()]);
    let workspace = Workspace::new("read-tool", server.port);
    std::fs::write(
        workspace.project().join("notes.txt"),
        "first line\nsecond line\n",
    )
    .expect("notes.txt");

    let out = workspace.run(&["-p", "Read notes.txt"], None);
    assert_eq!(text(&out.stderr), "");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), "Hello from the loop.\n");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let input = requests[1].body["input"].as_array().expect("input");
    let call = input
        .iter()
        .find(|item| item["type"] == "function_call")
        .expect("the call is replayed");
    assert_eq!(call["name"], "read");
    assert_eq!(call["arguments"], "{\"path\":\"notes.txt\"}");
    let output = input
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .expect("the tool result is sent");
    assert_eq!(output["call_id"], call["call_id"]);
    let sent = match &output["output"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect(),
        other => panic!("unexpected output {other}"),
    };
    // Pi's read result for a short file is its text.
    assert_eq!(sent, "first line\nsecond line\n");

    let entries = lines(&workspace.session_files()[0]);
    assert_eq!(
        kinds(&entries)[4..],
        [
            "message:system",
            "message:user",
            "message:assistant",
            "message:toolResult",
            "message:assistant",
        ]
    );
    let assistant = &entries[6]["message"];
    assert_eq!(assistant["content"][0]["type"], "toolCall");
    assert_eq!(assistant["content"][0]["name"], "read");
    assert_eq!(
        assistant["content"][0]["arguments"],
        json!({"path": "notes.txt"})
    );
    let result = &entries[7]["message"];
    assert_eq!(result["toolName"], "read");
    assert_eq!(result["toolCallId"], assistant["content"][0]["id"]);
    assert_eq!(result["isError"], false);
    assert_eq!(
        result["content"],
        json!([{ "type": "text", "text": "first line\nsecond line\n" }])
    );
}

/// Instructions and skills come from the universal `.agents` locations:
/// `~/.agents/AGENTS.md` and `~/.agents/skills` under the user's home, and
/// the project's `AGENTS.md` and `.agents/skills`. `CLAUDE.md` and the Bake
/// home's `AGENTS.md` are not read, and `/skill:name` expands the skill.
#[test]
fn print_mode_reads_agents_instructions_and_skills() {
    let server = Server::start(Reply::sse());
    let workspace = Workspace::new("agents", server.port);
    let user = workspace.root.join("user");
    let project = workspace.project();
    // The project is a repository, so the skill search stops at it.
    std::fs::create_dir_all(project.join(".git")).expect(".git");
    let write = |path: PathBuf, text: &str| {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, text).expect("write");
    };
    write(user.join(".agents/AGENTS.md"), "Global agents rules.");
    write(workspace.home().join("AGENTS.md"), "Bake home rules.");
    write(project.join("AGENTS.md"), "Project agents rules.");
    write(project.join("CLAUDE.md"), "Claude rules.");
    write(
        user.join(".agents/skills/user-skill/SKILL.md"),
        "---\nname: user-skill\ndescription: From the user.\n---\nUser body.\n",
    );
    let project_skill = project.join(".agents/skills/project-skill/SKILL.md");
    write(
        project_skill.clone(),
        "---\nname: project-skill\ndescription: From the project.\n---\n\nProject body.\n",
    );

    // The session's working directory has its links resolved, as macOS's
    // temporary directory needs.
    let project_skill = std::fs::canonicalize(&project_skill).expect("project skill");
    let out = workspace.run(&["-p", "/skill:project-skill now"], None);
    assert_eq!(text(&out.stderr), "");
    assert_eq!(out.status.code(), Some(0));
    let requests = server.requests();
    let input = transcript(&requests[0]);
    let system = &input[0];
    assert!(system.contains("Global agents rules."), "{system}");
    assert!(system.contains("Project agents rules."), "{system}");
    assert!(!system.contains("Bake home rules."), "{system}");
    assert!(!system.contains("Claude rules."), "{system}");
    let skills = system
        .split("<skills>\n")
        .nth(1)
        .and_then(|rest| rest.split("\n</skills>").next())
        .expect("a skills section");
    let user_skill = user.join(".agents/skills/user-skill/SKILL.md");
    assert!(
        skills.contains(&format!(
            "<name>project-skill</name>\n    <description>From the project.</description>\n    <location>{}</location>",
            project_skill.display()
        )),
        "{skills}"
    );
    assert!(
        skills.contains(&format!("<location>{}</location>", user_skill.display())),
        "{skills}"
    );
    assert_eq!(
        input[1],
        format!(
            "user: <skill name=\"project-skill\" location=\"{}\">\nReferences are relative to {}.\n\nProject body.\n</skill>\n\nnow",
            project_skill.display(),
            project_skill.parent().expect("dir").display()
        )
    );

    // `--no-skills` leaves the section out; `--skill` adds a file back.
    let out = workspace.run(
        &[
            "-p",
            "--no-session",
            "--no-skills",
            "--skill",
            ".agents/skills/project-skill/SKILL.md",
            "Hi",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let requests = server.requests();
    let system = &transcript(&requests[1])[0];
    assert!(system.contains("<name>project-skill</name>"), "{system}");
    assert!(!system.contains("<name>user-skill</name>"), "{system}");
}

/// Pi's bash tool through `bake-rs -p`: the command runs in the project
/// with the session's `PI_*` variables, and its output goes back to the
/// model. `danger-full-access` runs it unconfined, which needs no sandbox
/// backend on the test host.
#[cfg(unix)]
#[test]
fn print_mode_runs_the_bash_tool() {
    let command = "printf '%s|%s|%s\\n' \"$PI_PROVIDER\" \"$PI_MODEL\" \"$(basename \"$PWD\")\"";
    let server = Server::start_sequence(vec![
        Reply::tool_call("bash", &json!({ "command": command })),
        Reply::sse(),
    ]);
    let workspace = Workspace::new("bash-tool", server.port);

    let out = workspace.run(
        &["-p", "--permission", "danger-full-access", "Run it"],
        None,
    );
    assert_eq!(text(&out.stderr), "");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), "Hello from the loop.\n");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let tools: Vec<&str> = requests[0].body["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(tools, ["read", "bash", "edit", "write"]);
    let input = requests[1].body["input"].as_array().expect("input");
    let output = input
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .expect("the tool result is sent");
    let sent = match &output["output"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect(),
        other => panic!("unexpected output {other}"),
    };
    assert_eq!(sent, "loop|loop-model|project\n");
    let entries = lines(&workspace.session_files()[0]);
    let result = &entries[7]["message"];
    assert_eq!(result["toolName"], "bash");
    assert_eq!(result["isError"], false);
}

/// The tool result sent back for the first tool call of `request`.
fn sent_tool_output(request: &Request) -> String {
    let input = request.body["input"].as_array().expect("input");
    let output = input
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .expect("the tool result is sent");
    match &output["output"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect(),
        other => panic!("unexpected output {other}"),
    }
}

/// D24 through `bake-rs -p --permission workspace-write`: the model's
/// `write` outside the project is refused before any effect, and the tool
/// result sent back is the denial marker and the escalation hint
/// (`packages/fs/tool-fs/src/sandbox.ts` `mapError`). The target lives under
/// Cargo's per-target temporary directory, outside the project and outside
/// the temporary directories `workspace-write` also allows.
/// Removes a test's directory on drop, so a failed assertion leaves nothing.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn print_mode_refuses_a_write_outside_the_project() {
    let outside_root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("bake-rs-print-outside-{}", std::process::id()));
    let _cleanup = RemoveOnDrop(outside_root.clone());
    let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
    let tmp = std::fs::canonicalize("/tmp").unwrap_or_else(|_| PathBuf::from("/tmp"));
    std::fs::create_dir_all(&outside_root).expect("outside dir");
    let outside_root = std::fs::canonicalize(&outside_root).expect("outside dir");
    if outside_root.starts_with(&temp) || outside_root.starts_with(&tmp) {
        eprintln!("skipped: the Cargo target directory is under a temporary directory");
        return;
    }
    let target = outside_root.join("escape.txt");
    let _ = std::fs::remove_file(&target);
    let path = target.to_string_lossy().into_owned();
    let server = Server::start_sequence(vec![
        Reply::tool_call("write", &json!({ "path": path, "content": "escaped" })),
        Reply::sse(),
    ]);
    let workspace = Workspace::new("write-outside", server.port);

    let out = workspace.run(&["-p", "--permission", "workspace-write", "Write it"], None);
    assert_eq!(text(&out.stderr), "");
    assert_eq!(out.status.code(), Some(0));
    assert!(
        !target.exists(),
        "the refused write reached {}",
        target.display()
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        sent_tool_output(&requests[1]),
        "[sandbox: file access denied under workspace-write mode]\n[sandbox: escalation available — retry this exact operation once with sandbox_permissions (the narrowest wider mode that suffices) + justification; the approval prompt asks the user]"
    );
    // The file tools advertise the escalation parameters.
    let write = requests[0].body["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == "write")
        .expect("write is declared");
    assert_eq!(
        write["parameters"]["properties"]["sandbox_permissions"],
        json!({
            "type": "string",
            "enum": ["workspace-write", "danger-full-access"],
            "description": "One retry after a sandbox denial; asks the user.",
        })
    );
    // The permission is stated in the system prompt.
    let system = &transcript(&requests[0])[0];
    let root = serde_json::to_string(
        lines(&workspace.session_files()[0])[0]["cwd"]
            .as_str()
            .expect("cwd"),
    )
    .expect("root");
    assert!(
        system.ends_with(&format!(
            "\n<permissions>\nFile sandbox: workspace-write. Sandboxed tools can modify files under {root}, and possibly in some temporary directories.\n\nApproval prompts: enabled. Actions that need approval request it before running, and fail if no one can answer.\n</permissions>"
        )),
        "{system}"
    );
    let entries = lines(&workspace.session_files()[0]);
    let result = &entries[7]["message"];
    assert_eq!(result["toolName"], "write");
    assert_eq!(result["isError"], true);
}

/// D24 through `bake-rs -p` on a real backend: under `workspace-write`, a
/// bash write outside the project and the temporary directories is denied by
/// the kernel, nothing is created, and the result carries the denial marker
/// and the escalation hint.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn workspace_write_confines_bash() {
    // Outside every writable root: Cargo's target tmpdir is not under the
    // system temporary directories.
    let outside = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("bake-rs-confined-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).expect("outside dir");
    let target = outside.join("escaped.txt");
    let command = format!("echo pwned > '{}'", target.display());
    let server = Server::start_sequence(vec![
        Reply::tool_call("bash", &json!({ "command": command })),
        Reply::sse(),
    ]);
    let workspace = Workspace::new("confined-bash", server.port);

    let out = workspace.run(&["-p", "--permission", "workspace-write", "Run it"], None);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let sent = sent_tool_output(&server.requests()[1]);
    if sent.contains("no sandbox backend is usable") && std::env::var_os("CI").is_none() {
        eprintln!("skipped: no sandbox backend on this host: {sent}");
        let _ = std::fs::remove_dir_all(&outside);
        return;
    }
    assert!(!target.exists(), "the write escaped: {sent}");
    assert!(
        sent.contains("[sandbox: file access denied under workspace-write mode]"),
        "{sent}"
    );
    assert!(sent.contains("[sandbox: escalation available"), "{sent}");
    let _ = std::fs::remove_dir_all(&outside);
}
