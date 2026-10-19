//! Cases from Pi v1.3.0 `test/rpc-prompt-response-semantics.test.ts` and
//! `test/rpc.test.ts`, named after the Pi test each follows, plus Bake's
//! approval cases (D24). They run the RPC loop over in-memory pipes
//! against `bake-ai`'s faux provider, as the session tests do.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use bake_ai::Model;
use bake_ai::providers::faux::{FauxFuture, FauxResponseStep, faux_assistant_message};
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::*;
use crate::agent_session::tests::{harness, new_file_session, options};
use crate::test_support::TempDir;

/// The input half of the loop's standard input.
pub(super) struct Input {
    buffer: Arc<Mutex<Vec<u8>>>,
    closed: Arc<Mutex<bool>>,
}

/// A pipe the test writes commands into; reads block until a line arrives
/// or the writer closes it.
pub(super) struct Pipe {
    buffer: Arc<Mutex<Vec<u8>>>,
    closed: Arc<Mutex<bool>>,
}

impl Read for Pipe {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        loop {
            {
                let mut buffer = self.buffer.lock().unwrap_or_else(PoisonError::into_inner);
                if !buffer.is_empty() {
                    let take = buffer.len().min(out.len());
                    out[..take].copy_from_slice(&buffer[..take]);
                    buffer.drain(..take);
                    return Ok(take);
                }
                if *self.closed.lock().unwrap_or_else(PoisonError::into_inner) {
                    return Ok(0);
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Input {
    pub(super) fn new() -> (Self, Pipe) {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(Mutex::new(false));
        (
            Self {
                buffer: Arc::clone(&buffer),
                closed: Arc::clone(&closed),
            },
            Pipe { buffer, closed },
        )
    }

    /// Writes one command line.
    pub(super) fn send(&self, command: Value) {
        let line = format!("{command}\n");
        self.buffer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(line.as_bytes());
    }

    /// Ends the input, as a client closing the child's stdin does.
    pub(super) fn close(&self) {
        *self.closed.lock().unwrap_or_else(PoisonError::into_inner) = true;
    }
}

/// The records the loop wrote.
#[derive(Clone)]
pub(super) struct Output(pub(super) Arc<Mutex<Vec<u8>>>);

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Output {
    pub(super) fn records(&self) -> Vec<Value> {
        let bytes = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        String::from_utf8_lossy(&bytes)
            .split('\n')
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("a JSON record"))
            .collect()
    }

    /// The records of one kind, such as `response`.
    pub(super) fn of_kind(&self, kind: &str) -> Vec<Value> {
        self.records()
            .into_iter()
            .filter(|record| record["type"] == json!(kind))
            .collect()
    }

    /// Pi's `getPromptResponses`: the responses to one command id.
    pub(super) fn responses(&self, id: &str) -> Vec<Value> {
        self.of_kind("response")
            .into_iter()
            .filter(|record| record["id"] == json!(id))
            .collect()
    }

    /// Waits until `ready` holds, as Pi's `vi.waitFor` does.
    pub(super) async fn wait_for(&self, what: &str, ready: impl Fn(&Output) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if ready(self) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("{what} did not arrive; records: {:?}", self.records());
    }

    pub(super) async fn wait_for_response(&self, id: &str) -> Value {
        self.wait_for(&format!("a response to {id}"), |out| {
            !out.responses(id).is_empty()
        })
        .await;
        self.responses(id).remove(0)
    }
}

pub(super) fn writer(output: &Output) -> SharedWriter {
    Arc::new(Mutex::new(output.clone()))
}

/// Writes `records` as test output on failure.
pub(super) fn sink() -> SharedWriter {
    Arc::new(Mutex::new(std::io::sink()))
}

async fn session(dir: &TempDir, harness: &crate::agent_session::tests::Harness) -> AgentSession {
    let cwd = dir.path().to_string_lossy().into_owned();
    AgentSession::create(options(harness, new_file_session(dir), &cwd))
        .await
        .expect("session")
}

/// Starts the loop over the pipes and returns its task.
pub(super) fn start(
    session: AgentSession,
    input: Pipe,
    out: &Output,
    approvals: Option<ApprovalBroker>,
) -> tokio::task::JoinHandle<i32> {
    let options = RpcModeOptions {
        stdin: Box::new(input),
        stdout: writer(out),
        stderr: sink(),
        approvals,
        factory: None,
        interrupt: crate::cli::no_interrupt(),
    };
    tokio::spawn(async move { run_rpc_mode(session, options).await })
}

// Pi `test/rpc-prompt-response-semantics.test.ts`: "emits one started
// response when prompt preflight succeeds".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn emits_one_started_response_when_prompt_preflight_succeeds() {
    let dir = TempDir::new("rpc-started");
    let h = harness();
    h.faux
        .set_responses(vec![faux_assistant_message("done").into()]);
    let session = session(&dir, &h).await;
    let (input, pipe) = Input::new();
    let out = Output(Arc::new(Mutex::new(Vec::new())));
    let mode = start(session, pipe, &out, None);
    input.send(json!({ "id": "b2", "type": "prompt", "message": "Hello" }));
    let response = out.wait_for_response("b2").await;
    assert_eq!(
        response,
        json!({
            "id": "b2",
            "type": "response",
            "command": "prompt",
            "success": true,
            "data": { "disposition": "started" },
        })
    );
    out.wait_for("agent_settled", |out| {
        !out.of_kind("agent_settled").is_empty()
    })
    .await;
    input.close();
    assert_eq!(mode.await.expect("the loop ends"), 0);
    assert_eq!(out.responses("b2").len(), 1);
}

// Pi `test/rpc-prompt-response-semantics.test.ts`: "emits one failure
// response when prompt preflight rejects". Bake's refusal before any run
// is the session's own: no model is selected.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn emits_one_failure_response_when_prompt_preflight_rejects() {
    let dir = TempDir::new("rpc-refused");
    let h = harness();
    let cwd = dir.path().to_string_lossy().into_owned();
    let mut unkeyed = options(&h, new_file_session(&dir), &cwd);
    // Pi's case is a provider with no key, whose message its test names.
    unkeyed.model = Some(Model {
        provider: "fake-provider".into(),
        ..bake_ai::providers::faux::faux_model("fake-model")
    });
    let session = AgentSession::create(unkeyed).await.expect("session");
    let (input, pipe) = Input::new();
    let out = Output(Arc::new(Mutex::new(Vec::new())));
    let mode = start(session, pipe, &out, None);
    input.send(json!({ "id": "b1", "type": "prompt", "message": "Hello" }));
    let response = out.wait_for_response("b1").await;
    assert_eq!(response["success"], json!(false), "{response}");
    assert_eq!(response["command"], json!("prompt"));
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.starts_with("No API key found for fake-provider.")),
        "{response}"
    );
    input.close();
    assert_eq!(mode.await.expect("the loop ends"), 0);
    // Pi: exactly one response, and no run started.
    assert_eq!(out.responses("b1").len(), 1);
    assert!(out.of_kind("agent_start").is_empty(), "{:?}", out.records());
}

// Pi `test/rpc-prompt-response-semantics.test.ts`: "emits one success
// response when prompt is queued during streaming" and "returns and clears
// queued steering and follow-up messages".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn returns_and_clears_queued_steering_and_follow_up_messages() {
    let dir = TempDir::new("rpc-queue");
    let h = harness();
    // The first response waits, so the queued prompts arrive while the
    // turn runs, as Pi's delayed stream does.
    let release = Arc::new(Notify::new());
    {
        let release = Arc::clone(&release);
        h.faux.set_responses(vec![
            FauxResponseStep::Factory(Arc::new(move |_, _, _, _| {
                let release = Arc::clone(&release);
                Box::pin(async move {
                    release.notified().await;
                    Ok(faux_assistant_message("first"))
                })
            })),
            faux_assistant_message("done").into(),
        ]);
    }
    let session = session(&dir, &h).await;
    let (input, pipe) = Input::new();
    let out = Output(Arc::new(Mutex::new(Vec::new())));
    let mode = start(session, pipe, &out, None);
    input.send(json!({ "id": "start", "type": "prompt", "message": "Start" }));
    out.wait_for_response("start").await;
    // Pi's `prompt`: while a turn runs, a prompt without a streaming
    // behavior is refused with the reason to pass one.
    input.send(json!({ "id": "busy", "type": "prompt", "message": "Now" }));
    let busy = out.wait_for_response("busy").await;
    assert_eq!(busy["success"], json!(false), "{busy}");
    assert!(
        busy["error"]
            .as_str()
            .is_some_and(|error| error.contains("streamingBehavior")),
        "{busy}"
    );
    input.send(json!({
        "id": "steering",
        "type": "prompt",
        "message": "Change direction",
        "streamingBehavior": "steer",
    }));
    let queued = out.wait_for_response("steering").await;
    assert_eq!(queued["data"], json!({ "disposition": "queued" }));
    input.send(json!({
        "id": "follow",
        "type": "follow_up",
        "message": "Summarize when finished",
    }));
    out.wait_for_response("follow").await;
    input.send(json!({ "id": "clear", "type": "clear_queue" }));
    let cleared = out.wait_for_response("clear").await;
    assert_eq!(
        cleared["data"],
        json!({
            "steering": ["Change direction"],
            "followUp": ["Summarize when finished"],
        })
    );
    release.notify_one();
    out.wait_for("agent_settled", |out| {
        !out.of_kind("agent_settled").is_empty()
    })
    .await;
    // Pi: the cleared prompts started no further run.
    assert_eq!(out.of_kind("agent_start").len(), 1);
    input.close();
    assert_eq!(mode.await.expect("the loop ends"), 0);
}

// Pi `test/rpc.test.ts`: "should get state", "should set and get thinking
// level", "should get available thinking levels", and "should get available
// models", over one session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reports_state_models_and_thinking_levels() {
    let dir = TempDir::new("rpc-state");
    let h = harness();
    let session = session(&dir, &h).await;
    let session_id = session.session_id();
    let (input, pipe) = Input::new();
    let out = Output(Arc::new(Mutex::new(Vec::new())));
    let mode = start(session, pipe, &out, None);

    input.send(json!({ "id": "s", "type": "get_state" }));
    let state = out.wait_for_response("s").await;
    assert_eq!(state["data"]["model"]["provider"], json!("faux"));
    assert_eq!(state["data"]["isStreaming"], json!(false));
    assert_eq!(state["data"]["messageCount"], json!(0));
    assert_eq!(state["data"]["sessionId"], json!(session_id));
    assert_eq!(state["data"]["pendingMessageCount"], json!(0));

    input.send(json!({ "id": "l", "type": "get_available_thinking_levels" }));
    let levels = out.wait_for_response("l").await;
    let levels = levels["data"]["levels"].as_array().expect("levels").clone();
    assert!(
        levels.contains(&state["data"]["thinkingLevel"]),
        "{levels:?}"
    );

    input.send(json!({ "id": "t", "type": "set_thinking_level", "level": "high" }));
    assert_eq!(out.wait_for_response("t").await["success"], json!(true));
    input.send(json!({ "id": "s2", "type": "get_state" }));
    let state = out.wait_for_response("s2").await;
    assert_eq!(state["data"]["thinkingLevel"], json!("high"));

    // Pi: "cycle_thinking_level must only ever land on levels from
    // get_available_thinking_levels".
    input.send(json!({ "id": "c", "type": "cycle_thinking_level" }));
    let cycled = out.wait_for_response("c").await;
    assert!(levels.contains(&cycled["data"]["level"]), "{cycled}");
    assert_ne!(cycled["data"]["level"], json!("high"));

    input.send(json!({ "id": "m", "type": "get_available_models" }));
    let models = out.wait_for_response("m").await;
    let models = models["data"]["models"].as_array().expect("models").clone();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["id"], json!("faux-1"));

    // Pi: "should get session stats".
    input.send(json!({ "id": "st", "type": "get_session_stats" }));
    let stats = out.wait_for_response("st").await;
    assert_eq!(stats["data"]["sessionId"], json!(session_id));
    assert_eq!(stats["data"]["tokens"]["total"], json!(0));

    input.close();
    assert_eq!(mode.await.expect("the loop ends"), 0);
}

// Pi `test/rpc.test.ts`: "should get session entries with since cursor",
// "should get session tree", and "should get last assistant text".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reports_entries_tree_and_last_assistant_text() {
    let dir = TempDir::new("rpc-entries");
    let h = harness();
    h.faux
        .set_responses(vec![faux_assistant_message("test123").into()]);
    let session = session(&dir, &h).await;
    let (input, pipe) = Input::new();
    let out = Output(Arc::new(Mutex::new(Vec::new())));
    let mode = start(session, pipe, &out, None);

    input.send(json!({ "id": "t0", "type": "get_last_assistant_text" }));
    assert_eq!(
        out.wait_for_response("t0").await["data"],
        json!({ "text": Value::Null })
    );

    input.send(json!({ "id": "p", "type": "prompt", "message": "Reply with just 'ok'" }));
    out.wait_for_response("p").await;
    out.wait_for("agent_settled", |out| {
        !out.of_kind("agent_settled").is_empty()
    })
    .await;

    input.send(json!({ "id": "e", "type": "get_entries" }));
    let entries = out.wait_for_response("e").await;
    let ids: Vec<Value> = entries["data"]["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| entry["id"].clone())
        .collect();
    assert!(ids.len() >= 2, "{entries}");
    assert_eq!(entries["data"]["leafId"], ids[ids.len() - 1]);

    // Pi: the since cursor returns only entries strictly after the id.
    input.send(json!({ "id": "e2", "type": "get_entries", "since": ids[0] }));
    let since = out.wait_for_response("e2").await;
    let since_ids: Vec<Value> = since["data"]["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| entry["id"].clone())
        .collect();
    assert_eq!(since_ids, ids[1..]);
    assert_eq!(since["data"]["leafId"], entries["data"]["leafId"]);

    // Pi: an unknown since id is an error response.
    input.send(json!({ "id": "e3", "type": "get_entries", "since": "nonexistent-id" }));
    let unknown = out.wait_for_response("e3").await;
    assert_eq!(unknown["success"], json!(false));
    assert_eq!(unknown["error"], json!("Entry not found: nonexistent-id"));

    // Pi: a single root whose chain matches the entries.
    input.send(json!({ "id": "tr", "type": "get_tree" }));
    let tree = out.wait_for_response("tr").await;
    assert_eq!(tree["data"]["leafId"], entries["data"]["leafId"]);
    let mut nodes = tree["data"]["tree"].as_array().expect("tree").clone();
    let mut chain = Vec::new();
    while nodes.len() == 1 {
        chain.push(nodes[0]["entry"]["id"].clone());
        nodes = nodes[0]["children"].as_array().expect("children").clone();
    }
    assert!(nodes.is_empty(), "{tree}");
    assert_eq!(chain, ids);

    input.send(json!({ "id": "t1", "type": "get_last_assistant_text" }));
    assert_eq!(
        out.wait_for_response("t1").await["data"],
        json!({ "text": "test123" })
    );

    input.close();
    assert_eq!(mode.await.expect("the loop ends"), 0);
}

// Pi `test/rpc.test.ts`: "should set and get session name", with the
// `session_info` entry its session file carries.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn should_set_and_get_session_name() {
    let dir = TempDir::new("rpc-name");
    let h = harness();
    h.faux
        .set_responses(vec![faux_assistant_message("ok").into()]);
    let session = session(&dir, &h).await;
    let file = session.session_file().expect("persisted");
    let (input, pipe) = Input::new();
    let out = Output(Arc::new(Mutex::new(Vec::new())));
    let mode = start(session, pipe, &out, None);

    input.send(json!({ "id": "s", "type": "get_state" }));
    let state = out.wait_for_response("s").await;
    assert_eq!(state["data"].get("sessionName"), None);

    // Pi: a session file is written once a prompt has run.
    input.send(json!({ "id": "p", "type": "prompt", "message": "Reply with just 'ok'" }));
    out.wait_for_response("p").await;
    out.wait_for("agent_settled", |out| {
        !out.of_kind("agent_settled").is_empty()
    })
    .await;

    input.send(json!({ "id": "n", "type": "set_session_name", "name": "my-test-session" }));
    assert_eq!(out.wait_for_response("n").await["success"], json!(true));
    input.send(json!({ "id": "s2", "type": "get_state" }));
    let state = out.wait_for_response("s2").await;
    assert_eq!(state["data"]["sessionName"], json!("my-test-session"));

    // Pi: an empty name is refused.
    input.send(json!({ "id": "n2", "type": "set_session_name", "name": "  " }));
    let refused = out.wait_for_response("n2").await;
    assert_eq!(refused["success"], json!(false));
    assert_eq!(refused["error"], json!("Session name cannot be empty"));

    input.close();
    assert_eq!(mode.await.expect("the loop ends"), 0);
    let written = std::fs::read_to_string(&file).expect("session file");
    let names: Vec<Value> = written
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| entry["type"] == json!("session_info"))
        .collect();
    assert_eq!(names.len(), 1);
    assert_eq!(names[0]["name"], json!("my-test-session"));
}

// Pi `docs/rpc.md`, "Errors": a malformed record and an unknown command
// each answer once, and the loop goes on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answers_malformed_and_unknown_records() {
    let dir = TempDir::new("rpc-errors");
    let h = harness();
    let session = session(&dir, &h).await;
    let (input, pipe) = Input::new();
    let out = Output(Arc::new(Mutex::new(Vec::new())));
    let mode = start(session, pipe, &out, None);
    input
        .buffer
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .extend_from_slice(b"{ not json\n");
    out.wait_for("the parse failure", |out| {
        out.of_kind("response")
            .iter()
            .any(|record| record["command"] == json!("parse"))
    })
    .await;
    input.send(json!({ "id": "u", "type": "set_theme", "name": "dark" }));
    let unknown = out.wait_for_response("u").await;
    assert_eq!(unknown["error"], json!("Unknown command: set_theme"));
    // The loop still answers.
    input.send(json!({ "id": "s", "type": "get_state" }));
    assert_eq!(out.wait_for_response("s").await["success"], json!(true));
    input.close();
    assert_eq!(mode.await.expect("the loop ends"), 0);
}

// Bake: the commands whose subsystem the Rust agent has no port of answer
// with Pi's failed-response shape, never a panic.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refuses_the_commands_bake_does_not_support() {
    let dir = TempDir::new("rpc-unsupported");
    let h = harness();
    let session = session(&dir, &h).await;
    let (input, pipe) = Input::new();
    let out = Output(Arc::new(Mutex::new(Vec::new())));
    let mode = start(session, pipe, &out, None);
    for (id, command) in [
        (
            "bash",
            json!({ "id": "bash", "type": "bash", "command": "echo hi" }),
        ),
        (
            "abort_bash",
            json!({ "id": "abort_bash", "type": "abort_bash" }),
        ),
        (
            "set_auto_retry",
            json!({ "id": "set_auto_retry", "type": "set_auto_retry", "enabled": false }),
        ),
        (
            "new_session",
            json!({ "id": "new_session", "type": "new_session" }),
        ),
    ] {
        input.send(command);
        let response = out.wait_for_response(id).await;
        assert_eq!(response["success"], json!(false), "{response}");
        assert_eq!(response["command"], json!(id), "{response}");
        assert!(
            response["error"]
                .as_str()
                .is_some_and(|error| !error.is_empty()),
            "{response}"
        );
    }
    input.close();
    assert_eq!(mode.await.expect("the loop ends"), 0);
}

// ----------------------------------------------------------------------------
// Pi `rpc-mode.ts` `handleInputLine`, which starts each line's handler
// without awaiting the one before it: a long command delays no other.
// ----------------------------------------------------------------------------

/// A summary request that reports it started and then waits, so the
/// compaction it serves is held until its own request is cancelled or
/// `release` is notified. Pi holds a compaction the same way in
/// `test/suite/regressions/7150-rpc-prompt-during-compaction.test.ts`,
/// there through its `session_before_compact` handler.
fn held_summary(
    started: tokio::sync::mpsc::UnboundedSender<()>,
    release: Arc<Notify>,
) -> FauxResponseStep {
    FauxResponseStep::Factory(Arc::new(move |_, options, _, _| {
        let signal = options.base.signal.clone();
        let started = started.clone();
        let release = Arc::clone(&release);
        let wait: FauxFuture = Box::pin(async move {
            let released = release.notified();
            tokio::pin!(released);
            // Registered before the request reports that it started, so a
            // release that follows at once is not missed.
            released.as_mut().enable();
            let _ = started.send(());
            match signal {
                Some(signal) => {
                    tokio::select! {
                        () = signal.cancelled() => {}
                        () = released => {}
                    }
                }
                None => released.await,
            }
            Ok(faux_assistant_message("## Goal\nHeld summary"))
        });
        wait
    }))
}

/// A session with history a manual compaction can summarize, keeping one
/// recent token as Pi's 7150 harness does with
/// `compaction.keepRecentTokens`. Its prompts run before the loop starts,
/// so the loop's records are the test's own.
async fn compactable_session(
    dir: &TempDir,
    h: &crate::agent_session::tests::Harness,
) -> AgentSession {
    let cwd = dir.path().to_string_lossy().into_owned();
    let mut created = options(h, new_file_session(dir), &cwd);
    created.settings = crate::settings::Settings::from_object(
        json!({ "compaction": { "keepRecentTokens": 1 } })
            .as_object()
            .expect("a settings object"),
    );
    h.faux.set_responses(vec![
        faux_assistant_message("one").into(),
        faux_assistant_message("two").into(),
        faux_assistant_message("three").into(),
    ]);
    let session = AgentSession::create(created).await.expect("session");
    for text in ["one", "two", "three"] {
        session.prompt(text, Vec::new()).await.expect("prompt");
    }
    session
}

/// Holds a compaction the `compact` command started: its summary request
/// has been sent and waits. Returns the release the request waits on.
async fn hold_a_compaction(
    h: &crate::agent_session::tests::Harness,
    input: &Input,
    out: &Output,
) -> Arc<Notify> {
    let (started, mut requested) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(Notify::new());
    h.faux.set_responses(vec![
        held_summary(started, Arc::clone(&release)),
        // A compaction that keeps one token summarizes the last turn's
        // prefix too, so a released hold needs a second summary.
        faux_assistant_message("## Original Request\nthree").into(),
    ]);
    input.send(json!({ "id": "c", "type": "compact" }));
    requested.recv().await.expect("the summary request started");
    assert!(
        out.responses("c").is_empty(),
        "the compaction is still running: {:?}",
        out.records()
    );
    release
}

// Pi `agent-session.ts` `abort`, which calls `abortCompaction()` and then
// waits for idle. It is reached while a compaction runs because Pi handles
// each command line as it arrives, and the aborted compaction answers with
// the message Pi's `compact` throws once its signal has aborted,
// `Compaction cancelled`. Serialized handling would never reach the abort:
// the held summary request is released by nothing else.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abort_reaches_a_held_compaction() {
    let dir = TempDir::new("rpc-abort-compaction");
    let h = harness();
    let session = compactable_session(&dir, &h).await;
    let (input, pipe) = Input::new();
    let out = Output(Arc::new(Mutex::new(Vec::new())));
    let mode = start(session, pipe, &out, None);
    let _release = hold_a_compaction(&h, &input, &out).await;
    input.send(json!({ "id": "a", "type": "abort" }));
    let aborted = out.wait_for_response("a").await;
    assert_eq!(aborted["success"], json!(true), "{aborted}");
    let compacted = out.wait_for_response("c").await;
    assert_eq!(compacted["success"], json!(false), "{compacted}");
    assert_eq!(compacted["error"], json!("Compaction cancelled"));
    // Pi's `compaction_end` for an aborted compaction: no error message.
    let ends = out.of_kind("compaction_end");
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0]["aborted"], json!(true), "{ends:?}");
    assert_eq!(ends[0].get("errorMessage"), None, "{ends:?}");
    input.close();
    assert_eq!(mode.await.expect("the loop ends"), 0);
}

// Pi `test/rpc.test.ts`, "should get state", answered while a compaction
// runs: Pi's `get_state` reads the session and needs nothing of the
// running command. `isCompacting` reports the compaction meanwhile, as
// Pi's `RpcSessionState` does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn get_state_answers_while_a_compaction_runs() {
    let dir = TempDir::new("rpc-state-compacting");
    let h = harness();
    let session = compactable_session(&dir, &h).await;
    let (input, pipe) = Input::new();
    let out = Output(Arc::new(Mutex::new(Vec::new())));
    let mode = start(session, pipe, &out, None);
    let release = hold_a_compaction(&h, &input, &out).await;
    input.send(json!({ "id": "s", "type": "get_state" }));
    let state = out.wait_for_response("s").await;
    assert_eq!(state["success"], json!(true), "{state}");
    assert_eq!(state["data"]["isCompacting"], json!(true), "{state}");
    assert!(
        out.responses("c").is_empty(),
        "the compaction answered first: {:?}",
        out.records()
    );
    release.notify_one();
    let compacted = out.wait_for_response("c").await;
    assert_eq!(compacted["success"], json!(true), "{compacted}");
    input.close();
    assert_eq!(mode.await.expect("the loop ends"), 0);
}

// Pi `test/suite/regressions/7150-rpc-prompt-during-compaction.test.ts`:
// an RPC prompt during a manual compaction is refused, with the session's
// own message, and starts no run. The refusal is the session's, so a
// mutation Pi admits concurrently is never interleaved unsafely.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejects_an_rpc_prompt_while_manual_compaction_is_in_progress() {
    let dir = TempDir::new("rpc-prompt-compacting");
    let h = harness();
    let session = compactable_session(&dir, &h).await;
    let (input, pipe) = Input::new();
    let out = Output(Arc::new(Mutex::new(Vec::new())));
    let mode = start(session, pipe, &out, None);
    let release = hold_a_compaction(&h, &input, &out).await;
    input.send(json!({ "id": "p", "type": "prompt", "message": "PROBE-7150" }));
    let refused = out.wait_for_response("p").await;
    assert_eq!(refused["success"], json!(false), "{refused}");
    assert_eq!(
        refused["error"],
        json!(
            "Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry."
        )
    );
    assert!(out.of_kind("agent_start").is_empty(), "{:?}", out.records());
    release.notify_one();
    let compacted = out.wait_for_response("c").await;
    assert_eq!(compacted["success"], json!(true), "{compacted}");
    input.close();
    assert_eq!(mode.await.expect("the loop ends"), 0);
}

// Pi `rpc-mode.ts` `shutdown`, reached from `process.stdin.on("end")`
// while a command still runs: the session is shut down, which stops the
// compaction, and the exit is 0. Bake's shutdown also awaits the command
// task, so the compaction's own response is written before the loop
// returns and nothing is left running.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn end_of_input_during_a_held_compaction_shuts_down() {
    let dir = TempDir::new("rpc-eof-compacting");
    let h = harness();
    let session = compactable_session(&dir, &h).await;
    let (input, pipe) = Input::new();
    let out = Output(Arc::new(Mutex::new(Vec::new())));
    let mode = start(session, pipe, &out, None);
    let _release = hold_a_compaction(&h, &input, &out).await;
    input.close();
    assert_eq!(mode.await.expect("the loop ends"), 0);
    let compacted = out.responses("c");
    assert_eq!(compacted.len(), 1, "{:?}", out.records());
    assert_eq!(compacted[0]["success"], json!(false), "{:?}", compacted[0]);
    assert_eq!(compacted[0]["error"], json!("Compaction cancelled"));
}
