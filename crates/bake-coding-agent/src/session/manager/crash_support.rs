//! Fixtures the session writer's crash tests share: a written conversation
//! and the group that omits its tool turn, the file read as Pi's loader
//! reads it, the check a rebuilt request must pass, and [`TestChild`], a
//! child process re-running this test binary for one test, so a test can
//! stop a writer at a chosen point and kill it there.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bake_ai::Message;
use serde_json::{Value, json};

use super::*;
use crate::session::{
    AgentMessage, LeafSelector, SessionContext, SessionEntry, build_session_context,
    load_entries_from_file,
};
use crate::test_support::TempDir;

/// The longest a test waits on a child before it fails instead of hanging.
pub(crate) const PATIENCE: Duration = Duration::from_secs(30);

/// Exit codes a child ends with on its own, which an expected kill must
/// not be mistaken for: its watchdog, and a write that returned.
pub(super) const CHILD_WATCHDOG_EXIT: i32 = 3;
pub(super) const CHILD_RETURNED_EXIT: i32 = 4;

pub(super) fn message(value: Value) -> AgentMessage {
    match value {
        Value::Object(object) => AgentMessage::from_json(object),
        other => panic!("not an object: {other}"),
    }
}

pub(super) fn usage() -> Value {
    json!({"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}})
}

/// The ids a session's conversation gives its entries.
pub(super) struct Conversation {
    pub(super) assistant: String,
    pub(super) results: Vec<String>,
}

impl Conversation {
    /// The ids of the conversation a file written by [`conversation`]
    /// starts with, in a manager that opened it.
    pub(super) fn of(manager: &SessionManager) -> Self {
        let entries = manager.entries();
        let id = |index: usize| entries[index].id().to_owned();
        Self {
            assistant: id(1),
            results: vec![id(2), id(3)],
        }
    }
}

/// The tool result answering `call`.
pub(super) fn tool_result(call: &str) -> AgentMessage {
    message(json!({"role": "toolResult", "toolCallId": call,
        "toolName": "read", "content": [{"type": "text", "text": "x"}],
        "isError": false, "timestamp": 3}))
}

/// A prompt and an assistant that called two tools, without their results.
pub(super) fn tool_turn(manager: &mut SessionManager) -> String {
    manager
        .append_message(message(
            json!({"role": "user", "content": "go", "timestamp": 1}),
        ))
        .expect("user");
    manager
        .append_message(message(json!({"role": "assistant", "content": [
            {"type": "toolCall", "id": "c1", "name": "read", "arguments": {}},
            {"type": "toolCall", "id": "c2", "name": "read", "arguments": {}},
        ], "api": "faux", "provider": "faux", "model": "faux-1", "usage": usage(),
            "stopReason": "toolUse", "timestamp": 2})))
        .expect("assistant")
}

/// A written session: a prompt, an assistant that called two tools, and
/// both results, as an overflowing tool turn leaves it.
pub(super) fn conversation(manager: &mut SessionManager) -> Conversation {
    let assistant = tool_turn(manager);
    let results = ["c1", "c2"]
        .iter()
        .map(|call| manager.append_message(tool_result(call)).expect("result"))
        .collect();
    Conversation { assistant, results }
}

pub(super) fn file_session(dir: &TempDir) -> SessionManager {
    SessionManager::create(
        &dir.path().to_string_lossy(),
        Some(&dir.join("sessions")),
        NewSessionOptions::default(),
    )
    .expect("session")
}

/// A recovery omission in the order Bake writes it: the results, then
/// the attempt.
pub(super) fn omission(ids: &Conversation) -> Vec<GroupEntry> {
    ids.results
        .iter()
        .chain([&ids.assistant])
        .map(|id| GroupEntry::context_edit(id.clone(), None))
        .collect()
}

pub(super) fn lines(bytes: &[u8]) -> Vec<&[u8]> {
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .collect::<Vec<_>>()
}

pub(super) fn entries_of(path: &Path) -> Vec<SessionEntry> {
    load_entries_from_file(path)
        .expect("load")
        .into_iter()
        .filter_map(|entry| entry.as_entry().cloned())
        .collect()
}

pub(super) fn context_of(path: &Path) -> SessionContext {
    build_session_context(&entries_of(path), LeafSelector::Last)
}

/// What a provider would refuse in `messages`: a tool result that answers
/// no call of the assistant message it follows, or a call left without a
/// result before the next message.
pub(super) fn provider_problem(messages: &[Message]) -> Option<String> {
    let mut open: Vec<String> = Vec::new();
    let mut after_assistant = false;
    for message in messages {
        match message {
            Message::ToolResult(result) => {
                if !after_assistant || !open.contains(&result.tool_call_id) {
                    return Some(format!("{} answers no open call", result.tool_call_id));
                }
                open.retain(|id| id != &result.tool_call_id);
            }
            other => {
                if !open.is_empty() {
                    return Some(format!("calls {open:?} have no result"));
                }
                after_assistant = false;
                if let Message::Assistant(assistant) = other {
                    open = assistant.tool_calls().map(|call| call.id.clone()).collect();
                    after_assistant = true;
                }
            }
        }
    }
    (!open.is_empty()).then(|| format!("calls {open:?} have no result"))
}

/// The request a context rebuilds: Bake's `convert_to_llm`, then
/// `bake_ai`'s `transform_messages` as every provider runs it.
pub(super) fn request(context: &SessionContext) -> Vec<Message> {
    let live: Vec<_> = context
        .messages
        .iter()
        .map(AgentMessage::to_agent)
        .collect();
    let model = bake_ai::providers::faux::faux_model("faux-1");
    bake_ai::api::transform_messages::transform_messages(
        &crate::session::messages::convert_to_llm(&live),
        &model,
        None,
    )
}

pub(super) fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).expect("read")
}

/// The names in `dir`, sorted.
pub(super) fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read dir")
        .map(|entry| entry.expect("entry").file_name().to_string_lossy().into())
        .collect();
    names.sort();
    names
}

/// The temporary files in `dir`, by name, with their bytes.
pub(super) fn temps(dir: &Path) -> Vec<(String, Vec<u8>)> {
    names(dir)
        .into_iter()
        .filter(|name| name.ends_with(".tmp"))
        .map(|name| {
            let bytes = read(&dir.join(&name));
            (name, bytes)
        })
        .collect()
}

/// Whether `temp` is a temporary file of a rewrite of `target`.
pub(super) fn is_temp_of(temp: &str, target: &str) -> bool {
    temp.starts_with(&format!(".{target}.")) && temp.ends_with(".tmp")
}

/// The paths a listing of `sessions` for `cwd` and discovery find.
pub(super) fn listed(cwd: &str, sessions: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = SessionManager::list(cwd, Some(sessions), None, None)
        .expect("list")
        .into_iter()
        .map(|session| session.path)
        .collect();
    paths.sort();
    if let Some(recent) = find_most_recent_session(sessions, Some(cwd)) {
        assert!(paths.contains(&recent), "{recent:?} not in {paths:?}");
    }
    paths
}

/// A version 2 session in `sessions` whose `hookMessage` migration to
/// version 3 rewrites it on open, and its bytes.
pub(super) fn v2_session(sessions: &Path) -> (PathBuf, Vec<u8>) {
    std::fs::create_dir_all(sessions).expect("sessions");
    let cwd = sessions.to_string_lossy();
    let objects = [
        json!({"type": "session", "version": 2, "id": "crash-v2",
            "timestamp": "2026-01-01T00:00:00.000Z", "cwd": cwd}),
        json!({"type": "message", "id": "a1b2c3d4", "parentId": null,
            "timestamp": "2026-01-01T00:00:01.000Z",
            "message": {"role": "user", "content": "go", "timestamp": 1}}),
        json!({"type": "message", "id": "b2c3d4e5", "parentId": "a1b2c3d4",
            "timestamp": "2026-01-01T00:00:02.000Z",
            "message": {"role": "hookMessage", "customType": "x",
                "content": "from a hook", "display": true, "timestamp": 2}}),
        json!({"type": "message", "id": "c3d4e5f6", "parentId": "b2c3d4e5",
            "timestamp": "2026-01-01T00:00:03.000Z",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "done"}],
                "api": "faux", "provider": "faux", "model": "faux-1", "usage": usage(),
                "stopReason": "stop", "timestamp": 3}}),
    ];
    let mut bytes = Vec::new();
    for object in objects {
        bytes.extend_from_slice(object.to_string().as_bytes());
        bytes.push(b'\n');
    }
    let file = sessions.join("2026-01-01T00-00-00-000Z_crash-v2.jsonl");
    std::fs::write(&file, &bytes).expect("write v2 session");
    (file, bytes)
}

/// The bytes migrating `old` writes, and the context it rebuilds: a copy
/// opened in a directory of its own.
pub(super) fn migrated(old: &[u8]) -> (Vec<u8>, SessionContext) {
    let dir = TempDir::new("rewrite-expected-migration");
    let copy = dir.join("copy.jsonl");
    std::fs::write(&copy, old).expect("copy");
    let manager = SessionManager::open(&copy, None, None).expect("migrate copy");
    let context = manager.build_session_context();
    drop(manager);
    let new = read(&copy);
    assert_ne!(new, old, "the fixture must migrate");
    (new, context)
}

/// Open `file` once its writer lock is free: a killed writer's lock dies
/// with it, but Windows may release it a moment after the process exits.
pub(super) fn take_over(file: &Path) -> SessionManager {
    let deadline = Instant::now() + PATIENCE;
    loop {
        match SessionManager::open(file, None, None) {
            Err(SessionError::Locked(_)) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            result => return result.expect("taken over"),
        }
    }
}

/// A child process running one test of this test binary, and the threads
/// forwarding its stdout and stderr lines. Drop kills and reaps the child,
/// then joins the readers, which end at their pipes' end once the child is
/// gone, so none outlives the fixture, whichever assertion fails first.
pub(crate) struct TestChild {
    child: Child,
    output: mpsc::Receiver<String>,
    /// Every line received so far, for failure messages.
    seen: Vec<String>,
    readers: Vec<JoinHandle<()>>,
}

/// Forward each line of `pipe` to `lines` until the pipe ends.
fn forward(
    name: &str,
    pipe: impl Read + Send + 'static,
    lines: mpsc::Sender<String>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let mut reader = BufReader::new(pipe);
            let mut line = Vec::new();
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {
                        let text = String::from_utf8_lossy(&line).trim_end().to_owned();
                        if lines.send(text).is_err() {
                            return;
                        }
                    }
                }
            }
        })
        .expect("spawn output reader")
}

impl TestChild {
    /// Run the test at path `test` alone, with the environment variable
    /// `env` set to `spec`. libtest prints a marker on the line of the
    /// test's name, so markers are matched within lines; a name that
    /// matches no test still exits 0, so a parent always waits for one.
    pub(super) fn spawn(test: &str, env: &str, spec: &str) -> Self {
        Self::try_spawn(test, env, spec, |_| {}).expect("spawn child")
    }

    /// [`Self::spawn`], with `configure` adjusting the command first, such
    /// as to run a `pre_exec` hook; fails with the error starting it.
    pub(crate) fn try_spawn(
        test: &str,
        env: &str,
        spec: &str,
        configure: impl FnOnce(&mut Command),
    ) -> std::io::Result<Self> {
        let mut command = Command::new(std::env::current_exe().expect("test binary"));
        command
            .args(["--exact", test, "--nocapture", "--test-threads", "1"])
            .env(env, spec)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure(&mut command);
        let mut child = command.spawn()?;
        let pipes = (child.stdout.take(), child.stderr.take());
        let (lines, output) = mpsc::channel();
        // Owned before anything can fail, so Drop disposes of the child.
        let mut fixture = Self {
            child,
            output,
            seen: Vec::new(),
            readers: Vec::new(),
        };
        let (Some(stdout), Some(stderr)) = pipes else {
            panic!("the child has no output pipes");
        };
        fixture
            .readers
            .push(forward("child-stdout", stdout, lines.clone()));
        fixture.readers.push(forward("child-stderr", stderr, lines));
        Ok(fixture)
    }

    /// Wait for a line of output containing `mark`; fail when the child
    /// ends first or [`PATIENCE`] passes.
    pub(super) fn wait_for(&mut self, mark: &str) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.output.recv_timeout(left) {
                Ok(line) => {
                    let found = line.contains(mark);
                    self.seen.push(line);
                    if found {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    panic!("the child did not print {mark}: {:?}", self.seen)
                }
                Err(RecvTimeoutError::Disconnected) => panic!(
                    "the child ended ({:?}) without printing {mark}: {:?}",
                    self.child.try_wait(),
                    self.seen
                ),
            }
        }
    }

    /// Kill the child, reap it, and check that the parent's kill ended it:
    /// it was still running when killed, and it did not exit on its own
    /// through its watchdog, a returned write, or a panic.
    pub(super) fn kill(&mut self) {
        assert!(
            self.child.try_wait().expect("child status").is_none(),
            "the child exited before the parent killed it: {:?}",
            self.seen
        );
        self.child.kill().expect("kill child");
        let status = self.reap();
        assert_killed(status);
    }

    /// Wait for the child to end on its own, killing it and failing after
    /// [`PATIENCE`]; returns its status and every line it printed.
    #[cfg(target_os = "linux")]
    pub(crate) fn wait(&mut self) -> (ExitStatus, Vec<String>) {
        let status = self.reap();
        for reader in self.readers.drain(..) {
            assert!(reader.join().is_ok(), "an output reader panicked");
        }
        self.seen.extend(self.output.try_iter());
        (status, self.seen.clone())
    }

    /// The child's status once it ends, within [`PATIENCE`].
    fn reap(&mut self) -> ExitStatus {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(status) = self.child.try_wait().expect("child status") {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                self.seen.extend(self.output.try_iter());
                panic!("the child did not end: {:?}", self.seen);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// `status` is that of a child ended by [`Child::kill`]: SIGKILL on Unix;
/// on Windows, `TerminateProcess` with exit code 1, which the child never
/// exits with on its own (its own codes are 3, 4, and a panic's 101).
pub(super) fn assert_killed(status: ExitStatus) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            (status.signal(), status.code()),
            (Some(9), None),
            "the child was not ended by the parent's kill: {status}"
        );
    }
    #[cfg(windows)]
    assert_eq!(
        status.code(),
        Some(1),
        "the child was not ended by the parent's kill: {status}"
    );
    #[cfg(not(any(unix, windows)))]
    assert!(!status.success(), "{status}");
    assert_ne!(status.code(), Some(CHILD_WATCHDOG_EXIT), "{status}");
    assert_ne!(status.code(), Some(CHILD_RETURNED_EXIT), "{status}");
}

impl Drop for TestChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        for reader in self.readers.drain(..) {
            // The reader's own panic is reported here only when no other
            // failure is already unwinding.
            if reader.join().is_err() && !std::thread::panicking() {
                panic!("a child's output reader panicked");
            }
        }
    }
}
