//! Real stdio servers, spawned and owned through `bake_mcp::stdio`.
//!
//! This binary is its own fixture: with `BAKE_MCP_FIXTURE` set it runs as a
//! stdio MCP server (or as a server's grandchild) instead of running tests,
//! in place of Pi's `test/fixtures/stdio-server.mjs` and
//! `stubborn-server.mjs` (v1.3.0). The harness is its own (`harness = false`)
//! because libtest writes to standard output, which is the server's protocol
//! stream. Tests run one at a time, each on a runtime of its own under a
//! 60 s bound, and report as libtest does (`test NAME ... ok|FAILED`).
//!
//! Ordering comes from barriers the fixture writes to standard error ("got
//! initialize", "grandchild PID"), awaited through the owner's
//! `stderr_updates`; waits are bounded, never used as the condition. Every
//! process a test starts is recorded, and if any test fails the runner kills
//! what is left before it exits.

use std::future::Future;
use std::io::{BufRead, Write};
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::Duration;

use bake_mcp::stdio::{
    ProcessReport, SpawnError, StdioCommand, StdioOptions, StdioProcess, Stop, TreeEnd,
    scrubbed_parent_environment, spawn_stdio,
};
use bake_mcp::{
    ClientEvent, ClientOptions, CloseReason, ConnectError, Diagnostic, Implementation, McpClient,
    RequestError,
};
use serde_json::{Value, json};

const FIXTURE: &str = "BAKE_MCP_FIXTURE";
const BOUND: Duration = Duration::from_secs(60);
/// Set in this process before any thread starts; the scrub must drop it.
const PARENT_SECRET: &str = "BAKE_MCP_TEST_API_KEY";
/// Set likewise; the scrub must keep it.
const PARENT_VISIBLE: &str = "BAKE_MCP_TEST_VISIBLE";

// ------------------------------------------------------------------ fixture

fn fixture_main(mode: &str) {
    if mode == "ticker" {
        // Leaves the group and writes to the inherited standard error until
        // a write fails: once its only reader is gone (Rust ignores SIGPIPE,
        // so the write returns EPIPE), it exits by itself.
        leave_process_group();
        // Announced only once out of the group, so the test does not
        // start the shutdown while a group kill could still reach it.
        let mut ticks = 0_u64;
        loop {
            let mut err = std::io::stderr().lock();
            if ticks == 0 && writeln!(err, "ticker left the group").is_err() {
                std::process::exit(0);
            }
            ticks += 1;
            if writeln!(err, "tick").and_then(|()| err.flush()).is_err() {
                std::process::exit(0);
            }
            drop(err);
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    if mode == "sleeper" || mode == "escaped-sleeper" {
        ignore_sigterm();
        if mode == "escaped-sleeper" {
            leave_process_group();
            let mut err = std::io::stderr().lock();
            let _ = writeln!(err, "sleeper left the group");
            let _ = err.flush();
        }
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    let stderr = |line: &str| {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "{line}");
        let _ = err.flush();
    };
    match mode {
        "exit" => {
            stderr("exiting");
            std::process::exit(3);
        }
        // `parent` exits on end of input but leaves a grandchild holding its
        // pipes; `term` ignores end of input but not SIGTERM.
        // `ticker-parent` exits on end of input and leaves an escaped
        // grandchild writing to standard error.
        "stubborn" | "stall" | "escape" | "parent" | "term" | "ticker-parent" => {
            if mode != "term" {
                ignore_sigterm();
            }
            let sleeper = match mode {
                "escape" => "escaped-sleeper",
                "ticker-parent" => "ticker",
                _ => "sleeper",
            };
            // Inherits standard output and error, so it holds both pipes.
            let child = std::process::Command::new(std::env::current_exe().expect("own path"))
                .env(FIXTURE, sleeper)
                .stdin(std::process::Stdio::null())
                .spawn()
                .expect("spawn the grandchild");
            stderr(&format!("grandchild {}", child.id()));
            // Not waited for: it outlives this process unless killed.
            std::mem::forget(child);
        }
        "flood" => {
            let chunk = vec![b'e'; 64 * 1024];
            let mut err = std::io::stderr().lock();
            for _ in 0..64 {
                err.write_all(&chunk).expect("stderr");
            }
            err.flush().expect("stderr");
        }
        _ => stderr("stdio fixture ready"),
    }
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(method) = message["method"].as_str() else {
            continue;
        };
        stderr(&format!("got {method}"));
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        if mode == "stall" {
            continue;
        }
        let result = match method {
            "initialize" => json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "stdio-fixture", "version": "1.0.0"},
            }),
            "tools/list" => json!({"tools": [{"name": "echo", "inputSchema": {"type": "object"}}]}),
            "tools/call" => match message["params"]["name"].as_str() {
                Some("env") => {
                    let mut names: Vec<String> = std::env::vars_os()
                        .map(|(name, _)| name.to_string_lossy().into_owned())
                        .collect();
                    names.sort();
                    json!({"content": [], "structuredContent": names})
                }
                Some("partial") => {
                    // A response cut off mid-frame, then end of output.
                    let _ = write!(out, "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"res");
                    let _ = out.flush();
                    std::process::exit(0);
                }
                Some("malformed") => {
                    let _ = writeln!(out, "this is not JSON");
                    json!({"content": [{"type": "text", "text": "after noise"}]})
                }
                _ => {
                    json!({"content": [{"type": "text", "text": message["params"]["arguments"]["text"]}]})
                }
            },
            _ => json!({}),
        };
        let _ = writeln!(
            out,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        );
        let _ = out.flush();
    }
    if ["stubborn", "stall", "escape", "term"].contains(&mode) {
        // Ignore end of input as Pi's stubborn fixture does.
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
}

#[cfg(unix)]
fn ignore_sigterm() {
    // SAFETY: installs the predefined ignore disposition; no handler runs.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
}

#[cfg(not(unix))]
fn ignore_sigterm() {}

#[cfg(unix)]
fn leave_process_group() {
    // SAFETY: a plain syscall; this process is not a group leader.
    unsafe {
        libc::setsid();
    }
}

#[cfg(not(unix))]
fn leave_process_group() {}

// ------------------------------------------------------------------ helpers

/// Every pid a test started, killed by the runner if a test fails.
static STARTED: Mutex<Vec<u32>> = Mutex::new(Vec::new());

fn record(pid: u32) {
    STARTED.lock().unwrap_or_else(|e| e.into_inner()).push(pid);
}

fn fixture(mode: &str) -> StdioCommand {
    StdioCommand::new(std::env::current_exe().expect("own path")).env(FIXTURE, mode)
}

fn client_options() -> ClientOptions {
    let mut options = ClientOptions::new(Implementation::new("stdio-test", "1.0.0"));
    options.request_timeout = Some(Duration::from_secs(20));
    options
}

/// For servers expected to exit by themselves once their input closes: a
/// grace long enough that a loaded host does not turn that into `SIGTERM`.
fn options() -> StdioOptions {
    let mut options = stubborn_options();
    options.stdin_close_grace = Duration::from_secs(5);
    options
}

/// For servers that never exit by themselves: short graces, as only the
/// escalation is under test.
fn stubborn_options() -> StdioOptions {
    let mut options = StdioOptions::new(client_options());
    options.stdin_close_grace = Duration::from_millis(300);
    options.terminate_grace = Duration::from_millis(300);
    options.kill_wait = Duration::from_secs(5);
    options.stderr_drain = Duration::from_secs(5);
    options
}

fn spawn(command: StdioCommand, options: StdioOptions) -> (StdioProcess, bake_mcp::Connecting) {
    let (process, connecting) = spawn_stdio(command, options).expect("spawn the fixture");
    record(process.pid());
    (process, connecting)
}

async fn bounded<F: Future>(what: &str, work: F) -> F::Output {
    match tokio::time::timeout(Duration::from_secs(20), work).await {
        Ok(output) => output,
        Err(_) => panic!("timed out waiting for {what}"),
    }
}

/// Wait until the server's standard error contains `needle`.
async fn stderr_shows(process: &StdioProcess, needle: &str) -> String {
    let mut updates = process.stderr_updates();
    bounded(&format!("stderr to show {needle:?}"), async {
        loop {
            let text = process.stderr().tail_text();
            if text.contains(needle) {
                return text;
            }
            if updates.changed().await.is_err() {
                panic!("stderr ended without {needle:?}: {text}");
            }
        }
    })
    .await
}

async fn grandchild(process: &StdioProcess) -> u32 {
    let text = stderr_shows(process, "grandchild ").await;
    let pid = text
        .lines()
        .find_map(|line| line.strip_prefix("grandchild "))
        .and_then(|pid| pid.trim().parse().ok())
        .expect("a grandchild pid");
    record(pid);
    pid
}

async fn initialize(connecting: &mut bake_mcp::Connecting) -> (McpClient, bake_mcp::ClientEvents) {
    bounded("initialize", connecting.initialize())
        .await
        .expect("initialize")
}

/// Whether `pid` names a live or unreaped process.
#[cfg(unix)]
fn exists(pid: u32) -> bool {
    // SAFETY: signal 0 only checks; the pid fits as it came from the OS.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Whether `pid` names a running process. A pid can be reused once every
/// handle to its process closes; the tests probe only processes they started
/// moments before.
#[cfg(windows)]
fn exists(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: plain calls; the handle is closed before returning.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0_u32;
        let running = GetExitCodeProcess(handle, &mut code) != 0 && code == STILL_ACTIVE as u32;
        CloseHandle(handle);
        running
    }
}

/// Wait until `pid` is gone. A killed grandchild is reaped by whoever
/// inherits it (init or a subreaper), not by this process, so its
/// disappearance is polled, bounded.
async fn gone(pid: u32, what: &str) {
    bounded(&format!("{what} ({pid}) to be gone"), async {
        while exists(pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
}

/// Assert `pid` stays alive for a while: a `SIGKILL` already sent takes
/// effect asynchronously, so one probe right after it proves nothing.
#[cfg(unix)]
async fn stays(pid: u32, failure: &str) {
    let until = tokio::time::Instant::now() + Duration::from_millis(500);
    while tokio::time::Instant::now() < until {
        // A killed process is a zombie until its new parent reaps it.
        let zombie = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                stat.rsplit_once(") ")
                    .map(|(_, rest)| rest.starts_with('Z'))
            })
            .unwrap_or(false);
        assert!(exists(pid) && !zombie, "{failure}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(unix)]
fn kill_now(pid: u32) {
    // SAFETY: plain syscall on a pid this test recorded.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(windows)]
fn kill_now(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    // SAFETY: plain calls on a pid this test recorded; the handle is closed.
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !handle.is_null() {
            TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }
}

async fn shutdown(process: &StdioProcess) -> ProcessReport {
    bounded("the process shutdown", process.shutdown())
        .await
        .expect("the shutdown completes")
}

// ------------------------------------------------------------------ tests

async fn connects_lists_calls_and_captures_stderr() {
    // Pi `test/stdio.test.ts`: "connects to a newline-delimited MCP server
    // and captures stderr".
    let (process, mut connecting) = spawn(fixture("server"), options());
    let (client, _events) = initialize(&mut connecting).await;
    let tools = client.list_tools().await.unwrap();
    assert_eq!(
        tools.iter().map(|tool| tool.name()).collect::<Vec<_>>(),
        ["echo"]
    );
    let mut arguments = serde_json::Map::new();
    arguments.insert("text".into(), "hello".into());
    let result = client.call_tool("echo", Some(arguments)).await.unwrap();
    assert_eq!(result.content, [json!({"type": "text", "text": "hello"})]);
    // The server logs before it answers, but its standard error is read
    // independently of its output, so wait for the line rather than expect it.
    assert!(
        stderr_shows(&process, "got tools/call")
            .await
            .starts_with("stdio fixture ready\n")
    );

    assert_eq!(
        bounded("client shutdown", client.shutdown()).await,
        CloseReason::Shutdown
    );
    let report = shutdown(&process).await;
    // Closing standard input was enough: the server exited by itself.
    assert_eq!(report.stop, Stop::Exited);
    assert!(
        report.status.is_some_and(|status| status.success()),
        "{report:?}"
    );
    assert!(report.stderr_ended);
    assert!(!exists(process.pid()), "the server was not reaped");
}

async fn kills_a_server_that_ignores_shutdown_including_its_children() {
    // Pi `test/stdio.test.ts`: "kills a server that ignores shutdown,
    // including its children". The grandchild ignores SIGTERM and holds the
    // server's standard output and error.
    let (process, mut connecting) = spawn(fixture("stubborn"), stubborn_options());
    let grandchild = grandchild(&process).await;
    let (client, _events) = initialize(&mut connecting).await;
    assert_eq!(
        bounded("client shutdown", client.shutdown()).await,
        CloseReason::Shutdown
    );
    let report = shutdown(&process).await;
    assert_eq!(report.stop, Stop::Killed, "{report:?}");
    assert!(report.status.is_some_and(|status| !status.success()));
    // Standard error ended, so every holder of the pipe, the grandchild
    // included, has exited.
    assert!(report.stderr_ended, "{report:?}");
    {
        assert!(!exists(process.pid()), "the server was not reaped");
        gone(grandchild, "the grandchild").await;
    }
    let _ = grandchild;
}

async fn a_cancelled_initialize_is_drained_by_its_owner() {
    // The server reads `initialize` and never answers. The initialize future
    // is cancelled; the owner still holds the protocol task and the process,
    // and its two shutdowns leave nothing behind.
    let (process, mut connecting) = spawn(fixture("stall"), stubborn_options());
    let grandchild = grandchild(&process).await;
    {
        let mut initializing = Box::pin(connecting.initialize());
        tokio::select! {
            _ = &mut initializing => panic!("initialized without an answer"),
            _ = stderr_shows(&process, "got initialize") => {}
        }
        // Dropped here: the caller gave up on initialization.
    }
    // Nothing was torn down behind the owner's back.
    assert!(exists(process.pid()) && exists(grandchild));
    assert!(!process.stderr().ended);

    assert_eq!(
        bounded("connecting shutdown", connecting.shutdown()).await,
        Some(CloseReason::Shutdown)
    );
    let report = shutdown(&process).await;
    assert_eq!(report.stop, Stop::Killed, "{report:?}");
    assert!(report.stderr_ended, "{report:?}");
    {
        assert!(!exists(process.pid()), "the server was not reaped");
        gone(grandchild, "the grandchild").await;
    }
    let _ = grandchild;
}

async fn a_stalled_initialize_times_out_and_the_owner_reaps() {
    let mut options = stubborn_options();
    options.client.request_timeout = Some(Duration::from_millis(300));
    let (process, mut connecting) = spawn(fixture("stall"), options);
    let grandchild = grandchild(&process).await;
    let error = bounded("initialize", connecting.initialize())
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            ConnectError::Initialize(RequestError::Timeout {
                method: "initialize",
                ..
            })
        ),
        "{error}"
    );
    // A failed initialize has already shut the protocol task down.
    assert_eq!(connecting.shutdown().await, Some(CloseReason::Shutdown));
    let report = shutdown(&process).await;
    assert_eq!(report.stop, Stop::Killed);
    assert!(report.stderr_ended);
    gone(grandchild, "the grandchild").await;
    let _ = grandchild;
}

async fn the_environment_is_explicit_and_scrubbed() {
    async fn names(command: StdioCommand) -> Vec<String> {
        let (process, mut connecting) = spawn(command, options());
        let (client, _events) = initialize(&mut connecting).await;
        let result = client.call_tool("env", None).await.unwrap();
        client.shutdown().await;
        shutdown(&process).await;
        serde_json::from_value(result.structured_content.unwrap()).unwrap()
    }
    // Nothing is inherited unless added.
    let bare = names(fixture("server")).await;
    assert!(
        !bare
            .iter()
            .any(|name| name == PARENT_VISIBLE || name == PARENT_SECRET),
        "{bare:?}"
    );
    // Nothing of this process's: the fixture's own variable, and at most what
    // the platform adds to every process.
    assert!(bare.iter().any(|name| name == FIXTURE));
    assert!(
        !bare
            .iter()
            .any(|name| ["PATH", "HOME", "Path"].contains(&name.as_str())),
        "{bare:?}"
    );
    // The scrubbed base drops credential-shaped names; an explicit value
    // set after it reaches the server.
    let scrubbed = names(
        fixture("server")
            .envs(scrubbed_parent_environment())
            .env("SERVER_TOKEN", "configured"),
    )
    .await;
    assert!(
        scrubbed.iter().any(|name| name == PARENT_VISIBLE),
        "{scrubbed:?}"
    );
    assert!(
        !scrubbed.iter().any(|name| name == PARENT_SECRET),
        "{scrubbed:?}"
    );
    assert!(scrubbed.iter().any(|name| name == "SERVER_TOKEN"));
}

async fn stderr_is_bounded_while_a_server_floods_it() {
    // 4 MiB of standard error before the server reads anything: the reader
    // keeps up, so the server is not blocked, and keeps only the tail.
    let mut options = options();
    options.max_stderr_bytes = std::num::NonZeroUsize::new(4096).unwrap();
    let (process, mut connecting) = spawn(fixture("flood"), options);
    let (client, _events) = initialize(&mut connecting).await;
    client.shutdown().await;
    let report = shutdown(&process).await;
    assert!(
        report.stderr.total_bytes >= 4 * 1024 * 1024,
        "{}",
        report.stderr.total_bytes
    );
    assert!(report.stderr.tail.len() <= 4096);
    assert!(report.stderr.truncated());
    let tail = report.stderr.tail_text();
    assert!(
        tail.ends_with("got initialize\ngot notifications/initialized\n"),
        "{:?}",
        &tail[tail.len().saturating_sub(80)..]
    );
}

async fn a_partial_frame_then_end_of_output_fails_the_call() {
    let (process, mut connecting) = spawn(fixture("server"), options());
    let (client, _events) = initialize(&mut connecting).await;
    let error = client.call_tool("partial", None).await.unwrap_err();
    assert!(
        matches!(
            error,
            RequestError::Closed(CloseReason::IncompleteMessage { .. })
        ),
        "{error}"
    );
    client.shutdown().await;
    let report = shutdown(&process).await;
    assert_eq!(report.stop, Stop::Exited);
    assert!(report.status.is_some_and(|status| status.success()));
}

async fn malformed_output_is_reported_and_the_call_completes() {
    let (process, mut connecting) = spawn(fixture("server"), options());
    let (client, mut events) = initialize(&mut connecting).await;
    let result = client.call_tool("malformed", None).await.unwrap();
    assert_eq!(result.content[0]["text"], "after noise");
    assert!(matches!(
        events.try_recv(),
        Some(ClientEvent::Diagnostic(Diagnostic::MalformedMessage { .. }))
    ));
    client.shutdown().await;
    shutdown(&process).await;
}

async fn a_server_that_exits_before_initialize_reports_its_status() {
    let (process, mut connecting) = spawn(fixture("exit"), options());
    let error = bounded("initialize", connecting.initialize())
        .await
        .unwrap_err();
    // Which side notices first is the OS's: the read of end of output, or
    // the write of `initialize` into a pipe nobody reads (`EPIPE`; Rust
    // programs ignore `SIGPIPE`). Both close the connection; nothing else may.
    assert!(
        matches!(
            &error,
            ConnectError::Initialize(RequestError::Closed(
                CloseReason::EndOfStream
                    | CloseReason::Send {
                        kind: std::io::ErrorKind::BrokenPipe,
                        ..
                    }
            ))
        ),
        "{error}"
    );
    let report = shutdown(&process).await;
    assert_eq!(report.stop, Stop::Exited);
    assert_eq!(report.status.and_then(|status| status.code()), Some(3));
    assert_eq!(report.stderr.tail_text(), "exiting\n");
    assert!(report.stderr_ended);
}

async fn spawn_failures_start_nothing() {
    let missing = std::env::temp_dir().join("bake-mcp-no-such-server-program");
    match spawn_stdio(StdioCommand::new(&missing), options()) {
        Err(SpawnError::Spawn(error)) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
        other => panic!("expected a spawn error, got {:?}", other.map(|_| ())),
    }
    let mut refused = options();
    refused.terminate_grace = Duration::MAX;
    assert!(matches!(
        spawn_stdio(fixture("server"), refused),
        Err(SpawnError::InvalidOptions(_))
    ));
    let mut refused = options();
    refused.client.limits.max_events = std::num::NonZeroUsize::MAX;
    assert!(matches!(
        spawn_stdio(fixture("server"), refused),
        Err(SpawnError::InvalidOptions(_))
    ));
    let outside = std::thread::spawn(|| spawn_stdio(fixture("server"), options()).map(|_| ()))
        .join()
        .unwrap();
    assert!(matches!(outside, Err(SpawnError::NoRuntime)));
}

async fn repeated_and_concurrent_shutdowns_share_one_report() {
    let (process, mut connecting) = spawn(fixture("stubborn"), stubborn_options());
    // Recorded, so a failed run still kills it.
    let grandchild = grandchild(&process).await;
    let (client, _events) = initialize(&mut connecting).await;
    client.shutdown().await;
    let (first, second) = tokio::join!(shutdown(&process), shutdown(&process));
    assert_eq!(first, second);
    assert_eq!(shutdown(&process).await, first);
    assert_eq!(first.stop, Stop::Killed);
    gone(grandchild, "the grandchild").await;
    let _ = grandchild;
}

async fn dropping_the_owners_kills_the_tree_without_waiting() {
    // The drop fallback: nothing is awaited, but nothing is left either.
    let (process, mut connecting) = spawn(fixture("stubborn"), stubborn_options());
    let grandchild = grandchild(&process).await;
    let (client, _events) = initialize(&mut connecting).await;
    let pid = process.pid();
    drop(client);
    drop(connecting);
    drop(process);
    {
        gone(pid, "the server").await;
        gone(grandchild, "the grandchild").await;
    }
    let _ = (pid, grandchild);
}

#[cfg(unix)]
async fn a_descendant_that_leaves_the_group_is_not_waited_for() {
    // A grandchild that starts its own session escapes the group and keeps
    // standard error open. Shutdown stops waiting for it after
    // `stderr_drain` and says so; the test then removes it itself.
    let mut options = stubborn_options();
    options.stderr_drain = Duration::from_millis(300);
    let (process, mut connecting) = spawn(fixture("escape"), options);
    let escaped = grandchild(&process).await;
    stderr_shows(&process, "sleeper left the group").await;
    let (client, _events) = initialize(&mut connecting).await;
    client.shutdown().await;
    let started = std::time::Instant::now();
    let report = shutdown(&process).await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(report.stop, Stop::Killed);
    assert!(!report.stderr_ended, "{report:?}");
    assert!(!exists(process.pid()), "the server was not reaped");
    assert!(
        exists(escaped),
        "the escaped grandchild was killed after all"
    );
    kill_now(escaped);
    gone(escaped, "the escaped grandchild").await;
}

async fn descendants_of_a_server_that_exits_by_itself_are_killed() {
    // The server exits when its input closes, as servers should, but leaves
    // a grandchild (ignoring SIGTERM) holding its standard output and error.
    // Pi signals the group once the server is gone; Bake kills it.
    let (process, mut connecting) = spawn(fixture("parent"), options());
    let grandchild = grandchild(&process).await;
    let (client, _events) = initialize(&mut connecting).await;
    client.shutdown().await;
    let report = shutdown(&process).await;
    assert_eq!(report.stop, Stop::Exited, "{report:?}");
    assert!(report.status.is_some_and(|status| status.success()));
    assert!(
        report.stderr_ended,
        "the grandchild still held standard error: {report:?}"
    );
    // Windows waits for the Job to be empty before reporting, so the
    // grandchild has already exited; Unix reports the kill it sent.
    #[cfg(windows)]
    {
        assert_eq!(report.tree, TreeEnd::Empty, "{report:?}");
        assert!(!exists(grandchild), "the Job was reported empty too early");
    }
    #[cfg(unix)]
    assert_eq!(report.tree, TreeEnd::Killed, "{report:?}");
    gone(grandchild, "the grandchild").await;
}

#[cfg(unix)]
async fn a_server_that_ignores_end_of_input_stops_on_sigterm() {
    // Long enough after SIGTERM that only an ignored SIGTERM leads to SIGKILL.
    let mut options = stubborn_options();
    options.terminate_grace = Duration::from_secs(5);
    let (process, mut connecting) = spawn(fixture("term"), options);
    let grandchild = grandchild(&process).await;
    let (client, _events) = initialize(&mut connecting).await;
    client.shutdown().await;
    let report = shutdown(&process).await;
    assert_eq!(report.stop, Stop::Terminated, "{report:?}");
    assert!(report.stderr_ended);
    gone(grandchild, "the grandchild").await;
}

#[cfg(unix)]
async fn a_dropped_shutdown_during_the_stderr_drain_keeps_the_reader_owned() {
    // Review finding: the reader's handle was taken before it was awaited,
    // so a shutdown dropped during the drain detached the reader and a later
    // shutdown returned without awaiting it. Here an escaped grandchild keeps
    // standard error open and writes to it; the first shutdown is dropped
    // during the drain, and the second must stop and await the reader, after
    // which the grandchild's next write fails and it exits by itself.
    let mut options = options();
    options.stderr_drain = Duration::from_secs(3);
    let (process, mut connecting) = spawn(fixture("ticker-parent"), options);
    let ticker = grandchild(&process).await;
    stderr_shows(&process, "ticker left the group").await;
    let (client, _events) = initialize(&mut connecting).await;
    client.shutdown().await;
    let pid = process.pid();
    {
        let mut first = Box::pin(process.shutdown());
        tokio::select! {
            _ = &mut first => panic!("finished while an escaped grandchild holds standard error"),
            () = gone(pid, "the server") => {}
        }
        // Reaped, so the shutdown is now waiting in the drain; dropped here.
    }
    assert!(
        exists(ticker),
        "the ticker stopped while its reader was owned"
    );
    let report = shutdown(&process).await;
    assert_eq!(report.stop, Stop::Exited, "{report:?}");
    assert!(!report.stderr_ended, "{report:?}");
    gone(ticker, "the ticker, once its reader was stopped").await;
}

#[cfg(unix)]
async fn a_leader_reaped_elsewhere_is_never_signalled_as_a_group() {
    // Review finding: once the leader is reaped its pid, and so the group
    // id, can be reused, and the owner signalled the group anyway. Here the
    // test reaps the leader itself (as a host `waitpid(-1)` would) while a
    // grandchild that ignores SIGTERM stays in the group. Neither shutdown
    // nor drop may signal the group: the grandchild must survive both.
    let (process, mut connecting) = spawn(fixture("stubborn"), stubborn_options());
    let grandchild = grandchild(&process).await;
    let (client, _events) = initialize(&mut connecting).await;
    client.shutdown().await;
    let pid = process.pid() as libc::pid_t;
    let mut status = 0;
    // SAFETY: plain syscalls on the test's own child.
    let reaped = unsafe {
        libc::kill(pid, libc::SIGKILL);
        libc::waitpid(pid, &mut status, 0)
    };
    assert_eq!(reaped, pid);
    let report = shutdown(&process).await;
    assert_eq!(report.status, None, "{report:?}");
    assert_eq!(report.tree, TreeEnd::NotSignalled, "{report:?}");
    stays(grandchild, "shutdown signalled an obsolete process group").await;
    drop(process);
    drop(connecting);
    stays(grandchild, "drop signalled an obsolete process group").await;
    kill_now(grandchild);
    gone(grandchild, "the grandchild").await;
}

async fn an_incomplete_shutdown_keeps_everything_and_resumes() {
    // Review finding: a shutdown that gave up waiting after the kill stored
    // its report, so no later call waited for the process again. With no
    // wait after the kill, the first call usually returns before the kernel
    // has finished the kill; later calls must keep going until the server is
    // reaped and its tree gone.
    let mut options = stubborn_options();
    options.kill_wait = Duration::ZERO;
    let (process, mut connecting) = spawn(fixture("stubborn"), options);
    let grandchild = grandchild(&process).await;
    let (client, _events) = initialize(&mut connecting).await;
    client.shutdown().await;
    let mut incomplete = 0;
    let report = bounded("a completed shutdown", async {
        loop {
            match process.shutdown().await {
                Ok(report) => return report,
                Err(error) => {
                    assert_eq!(error.stop, Stop::Killed);
                    incomplete += 1;
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
    })
    .await;
    println!("    (incomplete shutdowns before completion: {incomplete})");
    assert!(report.status.is_some(), "{report:?}");
    assert_eq!(report.stop, Stop::Killed);
    assert!(report.stderr_ended, "{report:?}");
    {
        assert!(!exists(process.pid()), "the server was not reaped");
        gone(grandchild, "the grandchild").await;
    }
    let _ = grandchild;
}

// ------------------------------------------------------------------ runner

type Test = fn() -> Pin<Box<dyn Future<Output = ()> + Send>>;

macro_rules! tests {
    ($($name:ident),* $(,)?) => {
        vec![$((stringify!($name), (|| Box::pin($name()) as Pin<Box<dyn Future<Output = ()> + Send>>) as Test)),*]
    };
}

fn main() {
    if let Ok(mode) = std::env::var(FIXTURE) {
        fixture_main(&mode);
        return;
    }
    // SAFETY: no other thread exists yet.
    unsafe {
        std::env::set_var(PARENT_SECRET, "synthetic-secret");
        std::env::set_var(PARENT_VISIBLE, "visible");
    }
    #[allow(unused_mut)]
    let mut tests: Vec<(&str, Test)> = tests![
        connects_lists_calls_and_captures_stderr,
        kills_a_server_that_ignores_shutdown_including_its_children,
        a_cancelled_initialize_is_drained_by_its_owner,
        a_stalled_initialize_times_out_and_the_owner_reaps,
        the_environment_is_explicit_and_scrubbed,
        stderr_is_bounded_while_a_server_floods_it,
        a_partial_frame_then_end_of_output_fails_the_call,
        malformed_output_is_reported_and_the_call_completes,
        a_server_that_exits_before_initialize_reports_its_status,
        spawn_failures_start_nothing,
        repeated_and_concurrent_shutdowns_share_one_report,
        dropping_the_owners_kills_the_tree_without_waiting,
        descendants_of_a_server_that_exits_by_itself_are_killed,
        an_incomplete_shutdown_keeps_everything_and_resumes,
    ];
    #[cfg(unix)]
    tests.extend(tests![
        a_descendant_that_leaves_the_group_is_not_waited_for,
        a_server_that_ignores_end_of_input_stops_on_sigterm,
        a_dropped_shutdown_during_the_stderr_drain_keeps_the_reader_owned,
        a_leader_reaped_elsewhere_is_never_signalled_as_a_group,
    ]);
    let filters: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    let selected: Vec<_> = tests
        .into_iter()
        .filter(|(name, _)| {
            filters.is_empty() || filters.iter().any(|filter| name.contains(filter.as_str()))
        })
        .collect();
    println!("\nrunning {} tests", selected.len());
    let mut failed = Vec::new();
    for (name, test) in selected {
        let outcome = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("a runtime");
            let result = runtime
                .block_on(async { tokio::time::timeout(BOUND, AssertUnwindSafe(test())).await });
            // Dropping the runtime here also runs every owner's drop fallback.
            drop(runtime);
            result.is_ok()
        })
        .join();
        match outcome {
            Ok(true) => println!("test {name} ... ok"),
            Ok(false) => {
                println!("test {name} ... FAILED (no result within {BOUND:?})");
                failed.push(name);
            }
            Err(_) => {
                println!("test {name} ... FAILED");
                failed.push(name);
            }
        }
    }
    if !failed.is_empty() {
        // A failed test may have left processes; kill everything recorded.
        for pid in STARTED.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            kill_now(*pid);
        }
        println!("\nfailures:\n    {}", failed.join("\n    "));
        println!("\ntest result: FAILED. {} failed", failed.len());
        std::process::exit(101);
    }
    println!("\ntest result: ok.");
}
