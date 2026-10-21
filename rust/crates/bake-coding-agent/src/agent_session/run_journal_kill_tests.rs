//! The test kills a running fixture process with `SIGKILL` at each durable
//! checkpoint ([`super::kill_at`]): before a model request (a new session's
//! first prompt, a recoverable run's first request, and the requests a
//! retry, a follow-up, a steer, a compaction, or a job notice opens), after
//! an assistant's tool call is synced and before the tool's first effect,
//! during an effectful and a read-only tool, and before the next step. Each
//! case reopens the file and checks what it holds, what the next request
//! is, and which effects ran, counted in files across both processes.

use super::rerun::{NOTICE, conversation};
use super::*;
use bake_agent::ToolReplay;

/// The file's entries in order: run-journal records as their kind, call,
/// and class; messages as their role and call; other conversation entries
/// as their type.
fn on_disk(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let entry: Value = serde_json::from_str(line).unwrap();
            let event = &entry["data"]["event"];
            let parts = match entry["type"].as_str()? {
                "custom" if entry["customType"] == "bake.run.v1" => {
                    [&event["kind"], &event["tool_call_id"], &event["replay"]]
                }
                "message" => [
                    &entry["message"]["role"],
                    &entry["message"]["toolCallId"],
                    &Value::Null,
                ],
                "custom_message" => [&entry["customType"], &Value::Null, &Value::Null],
                "compaction" | "context_edit" => [&entry["type"], &Value::Null, &Value::Null],
                _ => return None,
            };
            Some(
                parts
                    .iter()
                    .filter_map(|part| part.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        })
        .collect()
}

/// The effects `name` ran, in both processes.
fn effects(killed: &Killed, name: &str) -> usize {
    std::fs::read_to_string(killed.dir.join(name)).map_or(0, |text| text.lines().count())
}

/// Answers the next request with a final message and keeps the request.
fn answering(h: &Harness) -> Arc<Mutex<Vec<TranscriptContext>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    h.faux
        .set_responses(vec![capture(&seen, faux_assistant_message("done"))]);
    seen
}

fn request(seen: &Mutex<Vec<TranscriptContext>>) -> Vec<String> {
    conversation(seen.lock().unwrap()[0].messages())
}

/// The killed session, reopened with the fixture's tools, which now run in
/// this process and count into the same files.
async fn reopen(h: &Harness, killed: &Killed) -> AgentSession {
    AgentSession::create(AgentSessionOptions {
        tools: vec![
            fixture_tool(
                "effect",
                ToolReplay::Never,
                killed.dir.path(),
                Hold::Never,
                None,
            ),
            fixture_tool(
                "probe",
                ToolReplay::Safe,
                killed.dir.path(),
                Hold::Never,
                None,
            ),
        ],
        active_tool_names: vec!["effect".into(), "probe".into()],
        ..options(
            h,
            SessionManager::open(&killed.path, None, None).unwrap(),
            Settings::default(),
        )
    })
    .await
    .unwrap()
}

/// Inspects and resumes the killed run: its disposition before, and the
/// resumption's outcome.
async fn resume(
    session: &AgentSession,
    killed: &Killed,
) -> (RunRecoveryDisposition, Result<RunOutcome, String>) {
    let run_id = killed.run_id.as_deref().unwrap();
    let before = session.inspect_run(run_id).unwrap().disposition;
    let resumed = session
        .resume_run(run_id)
        .await
        .map(|result| result.outcome);
    (before, resumed)
}

const INPUT: [&str; 6] = [
    "started", "system", "message", "user", "message", "admitted",
];

fn input_then(rest: &[&str]) -> Vec<String> {
    INPUT
        .iter()
        .chain(rest)
        .map(|entry| (*entry).to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_before_the_first_request_keeps_the_prompt() {
    // An ordinary prompt: its user message creates the file, as Pi's does.
    let killed = kill_at("first-prompt").await;
    assert_eq!(on_disk(&killed.path), ["system", "user"]);
    let listed = SessionManager::list(
        &killed.dir.path().to_string_lossy(),
        Some(&killed.dir.join("sessions")),
        None,
        None,
    )
    .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].first_message, "perform effect");
    let h = harness(200_000);
    let seen = answering(&h);
    let session = reopen(&h, &killed).await;
    session.prompt("continue", Vec::new()).await.unwrap();
    session.shutdown().await;
    assert_eq!(request(&seen), ["user: perform effect", "user: continue"]);
    assert_eq!(effects(&killed, "effect") + effects(&killed, "probe"), 0);

    // A recoverable run: its acknowledged input and debit precede it.
    let killed = kill_at("admission").await;
    assert_eq!(on_disk(&killed.path), INPUT);
    let h = harness(200_000);
    let seen = answering(&h);
    let session = reopen(&h, &killed).await;
    let (before, resumed) = resume(&session, &killed).await;
    session.shutdown().await;
    assert_eq!(before, RunRecoveryDisposition::Resumable);
    assert_eq!(resumed.unwrap(), RunOutcome::FinishedTurn);
    assert_eq!(request(&seen), ["user: perform effect", NOTICE]);
    assert_eq!(effects(&killed, "effect") + effects(&killed, "probe"), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_after_a_synced_tool_call_and_before_its_effect_runs_nothing() {
    // An ordinary prompt: the next one answers the call with Pi's
    // synthetic result, which every provider adapter sends.
    let killed = kill_at("plain-assistant").await;
    assert_eq!(on_disk(&killed.path), ["system", "user", "assistant"]);
    let h = harness(200_000);
    let seen = answering(&h);
    let session = reopen(&h, &killed).await;
    session.prompt("continue", Vec::new()).await.unwrap();
    session.shutdown().await;
    let sent = bake_ai::api::transform_messages::transform_messages(
        seen.lock().unwrap()[0].messages(),
        h.faux.model(),
        None,
    );
    assert_eq!(
        conversation(&sent),
        [
            "user: perform effect",
            "assistant: call",
            "result call (error): No result provided",
            "user: continue",
        ]
    );
    assert_eq!(effects(&killed, "effect"), 0);

    // A recoverable run, before and after the call's dispatch record: an
    // effectful call needs reconciliation, and nothing is requested or run.
    for (mode, rest, reason) in [
        (
            "assistant",
            &["assistant", "message"][..],
            "incomplete tool exchange",
        ),
        (
            "pre-effect",
            &["assistant", "message", "dispatch call"][..],
            "external effect",
        ),
    ] {
        let killed = kill_at(mode).await;
        assert_eq!(on_disk(&killed.path), input_then(rest), "{mode}");
        let h = harness(200_000);
        let _seen = answering(&h);
        let session = reopen(&h, &killed).await;
        let (before, resumed) = resume(&session, &killed).await;
        session.shutdown().await;
        assert!(
            matches!(&before, RunRecoveryDisposition::NeedsReconciliation { reason: why } if why.contains(reason)),
            "{mode}: {before:?}"
        );
        assert!(resumed.unwrap_err().contains(reason), "{mode}");
        assert_eq!(h.faux.call_count(), 0, "{mode}");
        assert_eq!(effects(&killed, "effect"), 0, "{mode}");
        assert_eq!(on_disk(&killed.path), input_then(rest), "{mode}");
    }

    // A `Safe` call runs again, once, before the resumed request.
    let killed = kill_at("pre-read").await;
    assert_eq!(
        on_disk(&killed.path),
        input_then(&["assistant", "message", "dispatch call safe"])
    );
    let h = harness(200_000);
    let seen = answering(&h);
    let session = reopen(&h, &killed).await;
    let (before, resumed) = resume(&session, &killed).await;
    session.shutdown().await;
    assert_eq!(before, RunRecoveryDisposition::Resumable);
    assert_eq!(resumed.unwrap(), RunOutcome::FinishedTurn);
    assert_eq!(
        request(&seen),
        [
            "user: perform effect",
            "assistant: call",
            "result call: probe done",
            NOTICE
        ]
    );
    assert_eq!(effects(&killed, "probe"), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_during_a_tool_keeps_its_effect_and_reruns_only_a_read() {
    let dispatched = input_then(&["assistant", "message", "dispatch call"]);
    let killed = kill_at("dispatch").await;
    assert_eq!(on_disk(&killed.path), dispatched);
    let h = harness(200_000);
    let _seen = answering(&h);
    let session = reopen(&h, &killed).await;
    let (before, resumed) = resume(&session, &killed).await;
    session.shutdown().await;
    assert!(matches!(
        before,
        RunRecoveryDisposition::NeedsReconciliation { .. }
    ));
    assert!(resumed.is_err());
    assert_eq!(h.faux.call_count(), 0);
    assert_eq!(effects(&killed, "effect"), 1);
    assert_eq!(on_disk(&killed.path), dispatched);

    let killed = kill_at("read").await;
    assert_eq!(
        on_disk(&killed.path),
        input_then(&["assistant", "message", "dispatch call safe"])
    );
    let h = harness(200_000);
    let seen = answering(&h);
    let session = reopen(&h, &killed).await;
    let (before, resumed) = resume(&session, &killed).await;
    session.shutdown().await;
    assert_eq!(before, RunRecoveryDisposition::Resumable);
    assert_eq!(resumed.unwrap(), RunOutcome::FinishedTurn);
    assert_eq!(
        request(&seen),
        [
            "user: perform effect",
            "assistant: call",
            "result call: probe done",
            NOTICE
        ]
    );
    assert_eq!(effects(&killed, "probe"), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_at_each_later_request_keeps_its_input() {
    // Retried, followed up, steered, compacted, and opened by a job notice.
    let tool_step = ["assistant", "message", "dispatch call", "toolResult call"];
    let cases: [(&str, Vec<&str>, Vec<&str>, usize); 5] = [
        (
            "retry",
            vec!["assistant", "message", "context_edit", "admitted"],
            vec!["user: perform effect", NOTICE],
            0,
        ),
        (
            "follow-up",
            vec!["assistant", "message", "user", "message", "admitted"],
            vec![
                "user: perform effect",
                "assistant: ",
                "user: then this",
                NOTICE,
            ],
            0,
        ),
        (
            "steer",
            [&tool_step[..], &["message", "user", "message", "admitted"]].concat(),
            vec![
                "user: perform effect",
                "assistant: call",
                "result call: effect done",
                "user: also this",
                NOTICE,
            ],
            1,
        ),
        (
            "compaction",
            [
                &tool_step[..],
                &["message", "admitted", "compaction", "admitted"],
            ]
            .concat(),
            vec![],
            1,
        ),
        (
            "job-notice",
            vec![
                "assistant",
                "message",
                "bake.job.notice",
                "message",
                "admitted",
            ],
            vec![
                "user: perform effect",
                "assistant: ",
                "user: background job bash-1 (bash: fixture job) finished \
                 [status: completed, fixture job done]. Read its output with job_output.",
                NOTICE,
            ],
            0,
        ),
    ];
    for (mode, rest, expected, ran) in cases {
        let killed = kill_at(mode).await;
        assert_eq!(on_disk(&killed.path), input_then(&rest), "{mode}");
        let h = harness(200_000);
        let seen = answering(&h);
        let session = reopen(&h, &killed).await;
        let (before, resumed) = resume(&session, &killed).await;
        session.shutdown().await;
        assert_eq!(before, RunRecoveryDisposition::Resumable, "{mode}");
        assert_eq!(resumed.unwrap(), RunOutcome::FinishedTurn, "{mode}");
        let sent = request(&seen);
        if mode == "compaction" {
            assert!(sent[0].contains("Compacted"), "{sent:?}");
            assert_eq!(sent.last().map(String::as_str), Some(NOTICE));
        } else {
            assert_eq!(sent, expected, "{mode}");
        }
        assert_eq!(effects(&killed, "effect"), ran, "{mode}");
    }
}

/// A loopback server answering one Chat Completions request with the
/// headers and first event of a stream, then holding the connection open,
/// sending nothing more, until it is dropped.
struct HalfStream {
    port: u16,
    sent: Arc<std::sync::atomic::AtomicBool>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl HalfStream {
    fn start() -> Self {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicBool, Ordering};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let sent = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (sent, stop) = (Arc::clone(&sent), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut socket = loop {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                // The request's head and its body, by its length.
                let mut request = Vec::new();
                let mut chunk = [0u8; 8192];
                let complete = |request: &[u8]| {
                    let text = String::from_utf8_lossy(request);
                    let Some(end) = text.find("\r\n\r\n") else {
                        return false;
                    };
                    let length = text[..end]
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    request.len() >= end + 4 + length
                };
                while !complete(&request) {
                    match socket.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(read) => request.extend_from_slice(&chunk[..read]),
                    }
                }
                let event = json!({"id": "chatcmpl-half", "object": "chat.completion.chunk",
                    "created": 1_760_000_000, "model": "faux-1",
                    "choices": [{"index": 0, "delta": {"role": "assistant",
                        "content": "Half of an answer"}}]});
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                    cache-control: no-cache\r\nconnection: close\r\n\r\n";
                socket
                    .write_all(format!("{head}data: {event}\n\n").as_bytes())
                    .unwrap();
                socket.flush().unwrap();
                sent.store(true, Ordering::SeqCst);
                while !stop.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        };
        Self {
            port,
            sent,
            stop,
            thread: Some(thread),
        }
    }

    fn sent(&self) -> bool {
        self.sent.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for HalfStream {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Every line of the file ends in a newline and parses: it ends at its
/// last finished entry.
fn ends_at_a_finished_entry(path: &Path) {
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.ends_with('\n'), "a torn last line");
    for line in text.lines() {
        serde_json::from_str::<Value>(line).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_mid_stream_keeps_the_admission_and_no_partial_message() {
    // A real provider streams the first text of an answer over loopback,
    // then stalls; the fixture is killed once it has received that text.
    let server = HalfStream::start();
    let port = server.port.to_string();
    let killed = kill_in(
        TempDir::new("run-journal-kill-mid-stream"),
        "mid-stream",
        &[(CHILD_PORT, port.as_ref())],
        |_| {},
    )
    .await;
    // The fixture saw the first event, so the server's write reached the
    // socket; its flag is set once that write returns, which on a loaded
    // host can come after the kill.
    let bound = std::time::Instant::now() + Duration::from_secs(10);
    while !server.sent() && std::time::Instant::now() < bound {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(server.sent(), "the stream's first event was sent");
    drop(server);
    // The file ends at the admission: no partial assistant was written.
    assert_eq!(on_disk(&killed.path), INPUT);
    ends_at_a_finished_entry(&killed.path);

    let h = harness(200_000);
    let seen = answering(&h);
    let session = reopen(&h, &killed).await;
    let run_id = killed.run_id.as_deref().unwrap();
    let before = session.inspect_run(run_id).unwrap();
    let resumed = session
        .resume_run(run_id)
        .await
        .map(|result| result.outcome);
    let after = session.inspect_run(run_id).unwrap();
    session.shutdown().await;
    // The admitted request stays spent: the resumed request is the second.
    assert_eq!(
        (before.model_calls, before.disposition),
        (1, RunRecoveryDisposition::Resumable)
    );
    assert_eq!(resumed.unwrap(), RunOutcome::FinishedTurn);
    assert_eq!(after.model_calls, 2);
    assert_eq!(request(&seen), ["user: perform effect", NOTICE]);
    assert_eq!(effects(&killed, "effect") + effects(&killed, "probe"), 0);
}

/// The admission kinds the file records, in order.
fn admissions(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let entry: Value = serde_json::from_str(line).unwrap();
            let event = &entry["data"]["event"];
            (event["kind"] == "admitted").then(|| event["admission"]["kind"].to_string())
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_inside_the_compaction_summary_request_leaves_no_compaction() {
    // Killed inside the provider call that writes the summary, after its
    // admission was synced: no compaction entry, half-applied or whole.
    let tool_step = ["assistant", "message", "dispatch call", "toolResult call"];
    let killed = kill_at("compaction-summary").await;
    let expected = input_then(&[&tool_step[..], &["message", "admitted"]].concat());
    assert_eq!(on_disk(&killed.path), expected);
    assert_eq!(admissions(&killed.path), ["\"agent\"", "\"summary\""]);
    ends_at_a_finished_entry(&killed.path);

    let h = harness(200_000);
    let seen = answering(&h);
    let session = reopen(&h, &killed).await;
    let run_id = killed.run_id.as_deref().unwrap();
    let before = session.inspect_run(run_id).unwrap();
    let resumed = session
        .resume_run(run_id)
        .await
        .map(|result| result.outcome);
    let after = session.inspect_run(run_id).unwrap();
    session.shutdown().await;
    // The summary's admission stays spent. Reopened under settings that
    // do not compact, the run continues from the whole, uncompacted
    // context, and the effect is not repeated.
    assert_eq!(
        (before.model_calls, before.disposition),
        (2, RunRecoveryDisposition::Resumable)
    );
    assert_eq!(resumed.unwrap(), RunOutcome::FinishedTurn);
    assert_eq!(after.model_calls, 3);
    assert_eq!(
        request(&seen),
        [
            "user: perform effect",
            "assistant: call",
            "result call: effect done",
            NOTICE
        ]
    );
    assert!(!on_disk(&killed.path).contains(&"compaction".to_owned()));
    assert_eq!(effects(&killed, "effect"), 1);
}

/// Whether a thread of process `pid` is inside a `read` system call, by
/// `/proc/<pid>/task/*/syscall`; `None` when `/proc` does not say.
#[cfg(target_os = "linux")]
fn in_read(pid: u32) -> Option<bool> {
    let tasks = std::fs::read_dir(format!("/proc/{pid}/task")).ok()?;
    let mut readable = false;
    for task in tasks.flatten() {
        let Ok(line) = std::fs::read_to_string(task.path().join("syscall")) else {
            continue;
        };
        readable = true;
        let number = line.split_whitespace().next().unwrap_or_default();
        if number.parse::<libc::c_long>().ok() == Some(libc::SYS_read) {
            return Some(true);
        }
    }
    readable.then_some(false)
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_inside_a_safe_tools_read_reruns_it_once() {
    use std::os::unix::ffi::OsStrExt;
    let dir = TempDir::new("run-journal-kill-fifo-read");
    let fifo = dir.join("fifo");
    let path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: `path` is a NUL-terminated string that outlives the call.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    // Held for writing, and never written, so the tool's read blocks.
    let writer = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo)
        .unwrap();
    let observed = std::cell::Cell::new(None);
    let killed = kill_in(dir, "fifo-read", &[], |pid| {
        let bound = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let state = in_read(pid);
            if state != Some(false) || std::time::Instant::now() >= bound {
                observed.set(state);
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    })
    .await;
    drop(writer);
    match observed.get() {
        Some(true) => {}
        None => {
            // Without `/proc`'s view the kill point is unproven: skip, or
            // fail where kernel evidence is required.
            let why = "/proc did not show the fixture's system call";
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            crate::session::manager::kernel_faults::skip_unavailable(
                "a view of the fixture's system call",
                why,
            );
            #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
            eprintln!("skipped: {why}");
            return;
        }
        Some(false) => panic!("the fixture was not inside a read when killed"),
    }
    assert_eq!(
        on_disk(&killed.path),
        input_then(&["assistant", "message", "dispatch call safe"])
    );
    ends_at_a_finished_entry(&killed.path);

    // Ordinary recovery: the next prompt answers the call with Pi's
    // synthetic result.
    let text = std::fs::read_to_string(&killed.path).unwrap();
    let entries: Vec<crate::session::SessionEntry> = crate::session::parse_session_entries(&text)
        .into_iter()
        .filter_map(|entry| entry.as_entry().cloned())
        .collect();
    let context =
        crate::session::build_session_context(&entries, crate::session::LeafSelector::Last);
    let live: Vec<_> = context.messages.iter().map(|m| m.to_agent()).collect();
    let h = harness(200_000);
    let sent = bake_ai::api::transform_messages::transform_messages(
        &crate::session::messages::convert_to_llm(&live),
        h.faux.model(),
        None,
    );
    assert_eq!(
        conversation(&sent),
        [
            "user: perform effect",
            "assistant: call",
            "result call (error): No result provided",
        ]
    );

    // Selected recovery reruns the `Safe` call once, before the resumed
    // request.
    let seen = answering(&h);
    let session = reopen(&h, &killed).await;
    let (before, resumed) = resume(&session, &killed).await;
    session.shutdown().await;
    assert_eq!(before, RunRecoveryDisposition::Resumable);
    assert_eq!(resumed.unwrap(), RunOutcome::FinishedTurn);
    assert_eq!(
        request(&seen),
        [
            "user: perform effect",
            "assistant: call",
            "result call: probe done",
            NOTICE
        ]
    );
    assert_eq!(effects(&killed, "probe"), 1);
}
