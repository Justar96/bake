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

/// Picks the reply to a request, or `None` to hold it open, unanswered.
#[cfg(unix)]
pub(super) type Answer = Arc<dyn Fn(&Request) -> Option<Reply> + Send + Sync>;

/// A loopback server like [`Server`] that answers each request on its own
/// thread with the reply [`Answer`] picks, or holds the connection open,
/// unanswered, while the server lives. Stopped and joined on drop.
#[cfg(unix)]
pub(super) struct HoldServer {
    pub(super) port: u16,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Reads one request from `socket`: its line, headers, and JSON body.
#[cfg(unix)]
fn read_request(socket: &mut TcpStream) -> Option<Request> {
    let _ = socket.set_read_timeout(Some(Duration::from_secs(10)));
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        match socket.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
        if let Some(end) = find(&buffer, b"\r\n\r\n") {
            break end;
        }
        if buffer.len() > 1 << 20 {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let line = lines.next().unwrap_or("").to_owned();
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
    Some(Request {
        line,
        headers,
        body,
    })
}

#[cfg(unix)]
impl HoldServer {
    pub(super) fn start(answer: Answer) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("address").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (requests, stop) = (Arc::clone(&requests), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut connections = Vec::new();
                while !stop.load(Ordering::SeqCst) {
                    let Ok((mut socket, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    let _ = socket.set_nonblocking(false);
                    let (requests, stop, answer) = (
                        Arc::clone(&requests),
                        Arc::clone(&stop),
                        Arc::clone(&answer),
                    );
                    connections.push(std::thread::spawn(move || {
                        let Some(request) = read_request(&mut socket) else {
                            return;
                        };
                        let reply = answer(&request);
                        if let Ok(mut requests) = requests.lock() {
                            requests.push(request);
                        }
                        let Some(reply) = reply else {
                            while !stop.load(Ordering::SeqCst) {
                                std::thread::sleep(Duration::from_millis(10));
                            }
                            return;
                        };
                        let response = format!(
                            "HTTP/1.1 {} X\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                            reply.status,
                            reply.content_type,
                            reply.body.len(),
                            reply.body
                        );
                        let _ = socket.write_all(response.as_bytes());
                        let _ = socket.flush();
                    }));
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

    pub(super) fn requests(&self) -> Vec<Request> {
        self.requests
            .lock()
            .map(|requests| requests.clone())
            .unwrap_or_default()
    }
}

#[cfg(unix)]
impl Drop for HoldServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts `bake` in `workspace` and kills it with `SIGKILL` once `ready`
/// holds, failing if it exits first or `ready` takes a minute.
#[cfg(unix)]
pub(super) fn kill_when(workspace: &Workspace, args: &[&str], ready: impl Fn() -> bool) {
    use std::os::unix::process::ExitStatusExt;
    let mut child = workspace.command(args, false).spawn().expect("bake starts");
    let deadline = Instant::now() + RUN_TIMEOUT;
    while !ready() {
        if let Some(status) = child.try_wait().expect("bake status") {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            panic!("bake {args:?} exited ({status}) before its kill point: {stderr}");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("bake {args:?} did not reach its kill point");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.kill().expect("SIGKILL bake");
    let status = child.wait().expect("reap bake");
    assert_eq!(status.signal(), Some(libc::SIGKILL), "{status}");
}

/// The parent's session file: the one outside `subagents/`.
#[cfg(unix)]
fn parent_file(workspace: &Workspace) -> Option<PathBuf> {
    workspace
        .session_files()
        .into_iter()
        .find(|file| !file.parent().is_some_and(|dir| dir.ends_with("subagents")))
}

/// Whether the file holds an assistant message that ended its turn.
#[cfg(unix)]
fn has_final_answer(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .any(|entry| {
            entry["message"]["role"] == "assistant" && entry["message"]["stopReason"] == "stop"
        })
}

// D38 through the binary: `bake -p` killed with SIGKILL while it waits
// for a background child whose request the server holds. Both files load;
// `--continue` sends one provider-valid request that carries one
// interrupted notice and never re-runs the child, and a second
// `--continue` reports nothing more.
#[cfg(unix)]
#[test]
fn a_killed_print_run_reports_its_running_subagent_once_after_continue() {
    let answer: Answer = Arc::new(|request| {
        if is_child(request) {
            return None;
        }
        let sent = transcript(request);
        if sent
            .iter()
            .any(|line| line.starts_with("function_call_output"))
        {
            return Some(Reply::sse());
        }
        Some(Reply::tool_call(
            "subagent",
            &json!({ "description": "say hello", "prompt": "Say hello." }),
        ))
    });
    let server = HoldServer::start(answer);
    let workspace = Workspace::new("subagent-kill", server.port);
    two_models(&workspace, server.port);
    kill_when(
        &workspace,
        &["-p", "--permission", "danger-full-access", "Delegate"],
        || {
            server.requests().iter().any(is_child)
                && parent_file(&workspace).is_some_and(|file| has_final_answer(&file))
        },
    );
    let parent = parent_file(&workspace).expect("parent file");
    let files = workspace.session_files();
    assert_eq!(files.len(), 2, "{files:?}");
    let child = files.iter().find(|file| **file != parent).expect("child");
    let child_entries = lines(child);
    assert_eq!(
        child_entries[0]["parentSession"],
        json!(parent.to_string_lossy())
    );
    let before = server.requests().len();

    let interrupted = |request: &Request| {
        transcript(request)
            .iter()
            .filter(|line| line.contains("was interrupted when Bake stopped and did not finish"))
            .count()
    };
    for prompt in ["Next", "Again"] {
        let out = workspace.run(&["-p", "--continue", prompt], None);
        assert_eq!(text(&out.stderr), "");
        assert_eq!(out.status.code(), Some(0));
        let requests = server.requests();
        let sent = &requests[before..];
        assert!(!sent.iter().any(is_child), "a child was re-run");
        let last = sent.last().expect("a request");
        assert_eq!(interrupted(last), 1, "{:#?}", transcript(last));
        let count = |kind: &str| {
            transcript(last)
                .iter()
                .filter(|line| line.starts_with(kind))
                .count()
        };
        assert_eq!(
            (count("function_call:"), count("function_call_output:")),
            (1, 1),
            "every call is answered: {:#?}",
            transcript(last)
        );
        let notices: Vec<Value> = lines(&parent)
            .into_iter()
            .filter(|entry| entry["customType"] == "bake.subagent.notice")
            .collect();
        assert_eq!(notices.len(), 1, "{prompt}: {notices:#?}");
        assert_eq!(notices[0]["details"]["stopReason"], "interrupted");
        assert_eq!(notices[0]["details"]["runId"], 1);
    }
}

// D48: goals are deferred, so no composition reaches a goal tool. With
// every opt-in on and goal-shaped settings present, neither the parent's
// nor a child's request declares one, nor does the default composition;
// RPC has no goal command. The task-document tool's `goal` document kind
// is D37's opt-in goal document, which D48 does not defer, and is not
// checked here.
#[cfg(unix)]
#[test]
fn no_goal_surface_is_reachable_from_any_composition() {
    let goal_tools = |request: &Request| -> Vec<String> {
        request.body["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tool| tool["name"].as_str())
            .filter(|name| name.to_lowercase().contains("goal"))
            .map(str::to_owned)
            .collect()
    };
    let server = Server::start_sequence(vec![
        Reply::tool_call(
            "subagent",
            &json!({ "description": "x", "prompt": "Say hello.", "run_in_background": false }),
        ),
        Reply::sse(),
    ]);
    let workspace = Workspace::new("subagent-no-goals", server.port);
    two_models(&workspace, server.port);
    std::fs::write(
        workspace.home().join("settings.json"),
        json!({
            "subagents": true,
            "schedules": true,
            "taskDocuments": true,
            "goals": true,
            "goal": { "enabled": true },
        })
        .to_string(),
    )
    .expect("settings.json");
    let out = workspace.run(
        &[
            "-p",
            "--permission",
            "danger-full-access",
            "--tools",
            "*",
            "Go",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let requests = server.requests();
    assert!(requests.iter().any(is_child), "a child's request was sent");
    for request in &requests {
        assert_eq!(goal_tools(request), Vec::<String>::new());
    }
    let declared = |request: &Request| request.body["tools"].as_array().map_or(0, Vec::len);
    assert!(declared(&requests[0]) > 0, "the parent declares its tools");

    let plain = Server::start(Reply::sse());
    let defaults = Workspace::new("subagent-no-goals-default", plain.port);
    let out = defaults.run(&["-p", "Go"], None);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(goal_tools(&plain.requests()[0]), Vec::<String>::new());

    let out = defaults.run(
        &["--mode", "rpc", "--no-session"],
        Some("{\"id\":\"g\",\"type\":\"goal\"}\n"),
    );
    let replies: Vec<Value> = text(&out.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let reply = replies
        .iter()
        .find(|reply| reply["id"] == "g")
        .unwrap_or_else(|| panic!("{replies:#?} {}", text(&out.stderr)));
    assert_eq!(reply["success"], false);
    assert_eq!(reply["error"], "Unknown command: goal");
}
