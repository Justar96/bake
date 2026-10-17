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
//! process a test starts is observed by a stable identity (`observe`), and
//! after each test, passing or failing, the runner kills and awaits whatever
//! of it is left before the next test starts, reporting a cleanup failure
//! apart from an assertion failure.
//!
//! `BAKE_MCP_STDIO_INJECT=<label>` makes the test holding that label fail at
//! that point, with its processes still running: the failure control for the
//! cleanup.
//!
//! Platform coverage: the escalation through `SIGTERM`, the group-escape,
//! stderr-drain, and reaped-leader cases are Unix only (Windows has no
//! `SIGTERM`, `setsid`, or reaping by another party); the subreaper case is
//! Linux only (`PR_SET_CHILD_SUBREAPER`).

#[path = "stdio/observe.rs"]
mod observe;

use std::future::Future;
use std::io::{BufRead, Write};
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use observe::Observed;

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

const INJECT: &str = "BAKE_MCP_STDIO_INJECT";

/// Fails here when `BAKE_MCP_STDIO_INJECT` names `label`.
fn inject(label: &str) {
    if std::env::var(INJECT).is_ok_and(|value| value == label) {
        panic!("injected failure at {label:?}");
    }
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

struct Spawned {
    process: StdioProcess,
    connecting: bake_mcp::Connecting,
    server: Arc<Observed>,
}

async fn spawn(command: StdioCommand, options: StdioOptions) -> Spawned {
    let (process, connecting) = spawn_stdio(command, options)
        .await
        .expect("spawn the fixture");
    let server = Observed::child(process.pid(), "the server");
    Spawned {
        process,
        connecting,
        server,
    }
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

/// The grandchild the server announced, observed while the server runs.
async fn grandchild(spawned: &Spawned) -> Arc<Observed> {
    let text = stderr_shows(&spawned.process, "grandchild ").await;
    let pid = text
        .lines()
        .find_map(|line| line.strip_prefix("grandchild "))
        .and_then(|pid| pid.trim().parse().ok())
        .expect("a grandchild pid");
    Observed::descendant(&spawned.server, pid, "the grandchild")
}

async fn initialize(connecting: &mut bake_mcp::Connecting) -> (McpClient, bake_mcp::ClientEvents) {
    bounded("initialize", connecting.initialize())
        .await
        .expect("initialize")
}

/// Wait until `observed` has exited and been reaped by its parent (for a
/// killed grandchild, whoever inherited it), bounded.
async fn gone(observed: &Observed, what: &str) {
    bounded(&format!("{what} ({}) to be gone", observed.pid()), async {
        while !observed.is_reaped() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
}

/// Wait until `observed` has exited (it may stay a zombie), bounded.
#[cfg(unix)]
async fn exited(observed: &Observed, what: &str) {
    bounded(&format!("{what} ({}) to exit", observed.pid()), async {
        while observed.is_running() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
}

/// Assert `observed` keeps running, not even a zombie, for a while: a
/// `SIGKILL` already sent takes effect asynchronously, so one probe right
/// after it proves nothing.
#[cfg(unix)]
async fn stays(observed: &Observed, failure: &str) {
    let until = tokio::time::Instant::now() + Duration::from_millis(500);
    while tokio::time::Instant::now() < until {
        assert!(observed.is_running(), "{failure}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Reap the server as a host's `waitpid(-1)` would, behind its owner's back:
/// kill it through its identity, wait (bounded) for its exit, then reap it
/// without blocking.
#[cfg(unix)]
async fn reap_behind_the_owner(server: &Observed) {
    server.kill();
    exited(server, "the server").await;
    assert!(
        server.is_zombie(),
        "the server must remain waitable until reaped"
    );
    assert!(
        server.reap_if_own_exited_child(),
        "the exited server could not be reaped"
    );
    assert!(server.is_reaped(), "the server was not observed reaped");
}

/// `observed`, a member of a tree a completed shutdown reported empty, must
/// already be gone: not running, and on Unix not even a zombie, since the
/// owner waits for the group's last member to be reaped before it reaps the
/// server. Checked once, with no wait: a later exit would not prove it.
fn ended(observed: &Observed, what: &str) {
    assert!(
        observed.is_reaped(),
        "{what} ({}) outlived a shutdown that reported its tree empty",
        observed.pid()
    );
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
    let Spawned {
        process,
        mut connecting,
        server,
    } = spawn(fixture("server"), options()).await;
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
    assert!(server.is_reaped(), "the server was not reaped");
}

async fn kills_a_server_that_ignores_shutdown_including_its_children() {
    // Pi `test/stdio.test.ts`: "kills a server that ignores shutdown,
    // including its children". The grandchild ignores SIGTERM and holds the
    // server's standard output and error.
    let spawned = spawn(fixture("stubborn"), stubborn_options()).await;
    let grandchild = grandchild(&spawned).await;
    let Spawned {
        process,
        mut connecting,
        server,
    } = spawned;
    let (client, _events) = initialize(&mut connecting).await;
    inject("stubborn-before-shutdown");
    assert_eq!(
        bounded("client shutdown", client.shutdown()).await,
        CloseReason::Shutdown
    );
    let report = shutdown(&process).await;
    assert_eq!(report.stop, Stop::Killed, "{report:?}");
    assert!(report.status.is_some_and(|status| !status.success()));
    assert_eq!(report.tree, TreeEnd::Empty, "{report:?}");
    // Standard error ended, so every holder of the pipe, the grandchild
    // included, has exited.
    assert!(report.stderr_ended, "{report:?}");
    assert!(server.is_reaped(), "the server was not reaped");
    ended(&grandchild, "the grandchild");
}

async fn a_cancelled_initialize_is_drained_by_its_owner() {
    // The server reads `initialize` and never answers. The initialize future
    // is cancelled; the owner still holds the protocol task and the process,
    // and its two shutdowns leave nothing behind.
    let spawned = spawn(fixture("stall"), stubborn_options()).await;
    let grandchild = grandchild(&spawned).await;
    let Spawned {
        process,
        mut connecting,
        server,
    } = spawned;
    {
        let mut initializing = Box::pin(connecting.initialize());
        tokio::select! {
            _ = &mut initializing => panic!("initialized without an answer"),
            _ = stderr_shows(&process, "got initialize") => {}
        }
        // Dropped here: the caller gave up on initialization.
    }
    // Nothing was torn down behind the owner's back.
    assert!(server.is_running() && grandchild.is_running());
    assert!(!process.stderr().ended);

    assert_eq!(
        bounded("connecting shutdown", connecting.shutdown()).await,
        Some(CloseReason::Shutdown)
    );
    let report = shutdown(&process).await;
    assert_eq!(report.stop, Stop::Killed, "{report:?}");
    assert_eq!(report.tree, TreeEnd::Empty, "{report:?}");
    assert!(report.stderr_ended, "{report:?}");
    assert!(server.is_reaped(), "the server was not reaped");
    ended(&grandchild, "the grandchild");
}

async fn a_stalled_initialize_times_out_and_the_owner_reaps() {
    let mut options = stubborn_options();
    options.client.request_timeout = Some(Duration::from_millis(300));
    let spawned = spawn(fixture("stall"), options).await;
    let grandchild = grandchild(&spawned).await;
    let Spawned {
        process,
        mut connecting,
        ..
    } = spawned;
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
    ended(&grandchild, "the grandchild");
}

async fn the_environment_is_explicit_and_scrubbed() {
    async fn names(command: StdioCommand) -> Vec<String> {
        let Spawned {
            process,
            mut connecting,
            ..
        } = spawn(command, options()).await;
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
    let Spawned {
        process,
        mut connecting,
        ..
    } = spawn(fixture("flood"), options).await;
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
    let Spawned {
        process,
        mut connecting,
        ..
    } = spawn(fixture("server"), options()).await;
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
    let Spawned {
        process,
        mut connecting,
        ..
    } = spawn(fixture("server"), options()).await;
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
    let Spawned {
        process,
        mut connecting,
        ..
    } = spawn(fixture("exit"), options()).await;
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
    match spawn_stdio(StdioCommand::new(&missing), options()).await {
        Err(SpawnError::Spawn(error)) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
        other => panic!("expected a spawn error, got {:?}", other.map(|_| ())),
    }
    let mut refused = options();
    refused.terminate_grace = Duration::MAX;
    assert!(matches!(
        spawn_stdio(fixture("server"), refused).await,
        Err(SpawnError::InvalidOptions(_))
    ));
    let mut refused = options();
    refused.client.limits.max_events = std::num::NonZeroUsize::MAX;
    assert!(matches!(
        spawn_stdio(fixture("server"), refused).await,
        Err(SpawnError::InvalidOptions(_))
    ));
    // Polled once on a thread with no runtime: refused at its first poll,
    // before anything is created.
    let outside = std::thread::spawn(|| {
        let mut spawning = std::pin::pin!(spawn_stdio(fixture("server"), options()));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        match spawning.as_mut().poll(&mut context) {
            std::task::Poll::Ready(result) => Some(result.map(|_| ())),
            std::task::Poll::Pending => None,
        }
    })
    .join()
    .unwrap();
    assert!(matches!(outside, Some(Err(SpawnError::NoRuntime))));
}

async fn repeated_and_concurrent_shutdowns_share_one_report() {
    let spawned = spawn(fixture("stubborn"), stubborn_options()).await;
    let grandchild = grandchild(&spawned).await;
    let Spawned {
        process,
        mut connecting,
        ..
    } = spawned;
    let (client, _events) = initialize(&mut connecting).await;
    client.shutdown().await;
    let (first, second) = tokio::join!(shutdown(&process), shutdown(&process));
    assert_eq!(first, second);
    assert_eq!(shutdown(&process).await, first);
    assert_eq!(first.stop, Stop::Killed);
    assert_eq!(first.tree, TreeEnd::Empty);
    ended(&grandchild, "the grandchild");
}

async fn dropping_the_owners_kills_the_tree_without_waiting() {
    // The drop fallback: nothing is awaited, but nothing is left either.
    let spawned = spawn(fixture("stubborn"), stubborn_options()).await;
    let grandchild = grandchild(&spawned).await;
    let Spawned {
        process,
        mut connecting,
        server,
    } = spawned;
    let (client, _events) = initialize(&mut connecting).await;
    drop(client);
    drop(connecting);
    drop(process);
    gone(&server, "the server").await;
    gone(&grandchild, "the grandchild").await;
}

#[cfg(unix)]
async fn a_descendant_that_leaves_the_group_is_not_waited_for() {
    // A grandchild that starts its own session escapes the group and keeps
    // standard error open. Shutdown stops waiting for it after
    // `stderr_drain` and says so; the test then removes it itself.
    let mut options = stubborn_options();
    options.stderr_drain = Duration::from_millis(300);
    let spawned = spawn(fixture("escape"), options).await;
    let escaped = grandchild(&spawned).await;
    let Spawned {
        process,
        mut connecting,
        server,
    } = spawned;
    stderr_shows(&process, "sleeper left the group").await;
    let (client, _events) = initialize(&mut connecting).await;
    client.shutdown().await;
    let started = std::time::Instant::now();
    let report = shutdown(&process).await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(report.stop, Stop::Killed);
    // The group's own members are gone; the escaped one is not in it.
    assert_eq!(report.tree, TreeEnd::Empty, "{report:?}");
    assert!(!report.stderr_ended, "{report:?}");
    assert!(server.is_reaped(), "the server was not reaped");
    assert!(
        escaped.is_running(),
        "the escaped grandchild was killed after all"
    );
    inject("escaped-survivor");
    escaped.kill();
    gone(&escaped, "the escaped grandchild").await;
}

async fn descendants_of_a_server_that_exits_by_itself_are_killed() {
    // The server exits when its input closes, as servers should, but leaves
    // a grandchild (ignoring SIGTERM) holding its standard output and error.
    // Pi signals the group once the server is gone; Bake kills it.
    let spawned = spawn(fixture("parent"), options()).await;
    let grandchild = grandchild(&spawned).await;
    let Spawned {
        process,
        mut connecting,
        ..
    } = spawned;
    let (client, _events) = initialize(&mut connecting).await;
    client.shutdown().await;
    let report = shutdown(&process).await;
    assert_eq!(report.stop, Stop::Exited, "{report:?}");
    assert!(report.status.is_some_and(|status| status.success()));
    assert!(
        report.stderr_ended,
        "the grandchild still held standard error: {report:?}"
    );
    // The group or Job was awaited until empty before the server was
    // reaped, so the grandchild has already exited.
    assert_eq!(report.tree, TreeEnd::Empty, "{report:?}");
    ended(&grandchild, "the grandchild");
}

#[cfg(unix)]
async fn a_server_that_ignores_end_of_input_stops_on_sigterm() {
    // Long enough after SIGTERM that only an ignored SIGTERM leads to SIGKILL.
    let mut options = stubborn_options();
    options.terminate_grace = Duration::from_secs(5);
    let spawned = spawn(fixture("term"), options).await;
    let grandchild = grandchild(&spawned).await;
    let Spawned {
        process,
        mut connecting,
        ..
    } = spawned;
    let (client, _events) = initialize(&mut connecting).await;
    client.shutdown().await;
    let report = shutdown(&process).await;
    assert_eq!(report.stop, Stop::Terminated, "{report:?}");
    assert_eq!(report.tree, TreeEnd::Empty, "{report:?}");
    assert!(report.stderr_ended);
    ended(&grandchild, "the grandchild");
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
    let spawned = spawn(fixture("ticker-parent"), options).await;
    let ticker = grandchild(&spawned).await;
    let Spawned {
        process,
        mut connecting,
        server,
    } = spawned;
    stderr_shows(&process, "ticker left the group").await;
    let (client, _events) = initialize(&mut connecting).await;
    client.shutdown().await;
    {
        let mut first = Box::pin(process.shutdown());
        tokio::select! {
            _ = &mut first => panic!("finished while an escaped grandchild holds standard error"),
            () = gone(&server, "the server") => {}
        }
        // Reaped, so the shutdown is now waiting in the drain; dropped here.
    }
    assert!(
        ticker.is_running(),
        "the ticker stopped while its reader was owned"
    );
    let report = shutdown(&process).await;
    assert_eq!(report.stop, Stop::Exited, "{report:?}");
    assert!(!report.stderr_ended, "{report:?}");
    gone(&ticker, "the ticker, once its reader was stopped").await;
}

#[cfg(unix)]
async fn a_leader_reaped_elsewhere_is_never_signalled_as_a_group() {
    // Review finding: once the leader is reaped its pid, and so the group
    // id, can be reused, and the owner signalled the group anyway. Here the
    // test reaps the leader itself (as a host `waitpid(-1)` would) while a
    // grandchild that ignores SIGTERM stays in the group. Neither shutdown
    // nor drop may signal the group: the grandchild must survive both.
    let spawned = spawn(fixture("stubborn"), stubborn_options()).await;
    let grandchild = grandchild(&spawned).await;
    let Spawned {
        process,
        mut connecting,
        server,
    } = spawned;
    let (client, _events) = initialize(&mut connecting).await;
    client.shutdown().await;
    reap_behind_the_owner(&server).await;
    let report = shutdown(&process).await;
    assert_eq!(report.status, None, "{report:?}");
    assert_eq!(report.tree, TreeEnd::NotSignalled, "{report:?}");
    stays(&grandchild, "shutdown signalled an obsolete process group").await;
    drop(process);
    drop(connecting);
    stays(&grandchild, "drop signalled an obsolete process group").await;
    inject("unowned-group-member");
    grandchild.kill();
    gone(&grandchild, "the grandchild").await;
}

#[cfg(unix)]
async fn a_dropped_owner_whose_leader_was_reaped_elsewhere_signals_nothing() {
    // The drop fallback checks the leader's identity before it signals: with
    // no shutdown first to notice the reap, the drop must still not signal a
    // group whose id may now be reused.
    let spawned = spawn(fixture("stubborn"), stubborn_options()).await;
    let grandchild = grandchild(&spawned).await;
    let Spawned {
        process,
        mut connecting,
        server,
    } = spawned;
    let (client, _events) = initialize(&mut connecting).await;
    client.shutdown().await;
    reap_behind_the_owner(&server).await;
    drop(process);
    drop(connecting);
    stays(&grandchild, "drop signalled an obsolete process group").await;
    grandchild.kill();
    gone(&grandchild, "the grandchild").await;
}

async fn an_incomplete_shutdown_keeps_everything_and_resumes() {
    // Review finding: a shutdown that gave up waiting after the kill stored
    // its report, so no later call waited for the process again. With no
    // wait after the kill, the first call usually returns before the kernel
    // has finished the kill; later calls must keep going until the server is
    // reaped and its tree gone.
    let mut options = stubborn_options();
    options.kill_wait = Duration::ZERO;
    let spawned = spawn(fixture("stubborn"), options).await;
    let grandchild = grandchild(&spawned).await;
    let Spawned {
        process,
        mut connecting,
        server,
    } = spawned;
    let (client, _events) = initialize(&mut connecting).await;
    client.shutdown().await;
    let (mut running, mut tree_left) = (0, 0);
    let report = bounded("a completed shutdown", async {
        loop {
            match process.shutdown().await {
                Ok(report) => return report,
                Err(error) => {
                    assert_eq!(error.stop, Stop::Killed);
                    if error.exited {
                        // Exited but held unreaped while its tree is awaited.
                        #[cfg(unix)]
                        assert!(server.is_zombie(), "reaped before its tree ended");
                        tree_left += 1;
                    } else {
                        running += 1;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
    })
    .await;
    println!(
        "    (incomplete shutdowns before completion: {running} before the exit, {tree_left} after)"
    );
    assert!(report.status.is_some(), "{report:?}");
    assert_eq!(report.stop, Stop::Killed);
    assert_eq!(report.tree, TreeEnd::Empty, "{report:?}");
    assert!(report.stderr_ended, "{report:?}");
    assert!(server.is_reaped(), "the server was not reaped");
    ended(&grandchild, "the grandchild");
}

/// Makes this process a child subreaper while it lives (Linux), so a killed
/// grandchild whose parent is gone becomes this process's zombie and stays
/// in its process group until it is reaped; restores the previous setting
/// on drop. The runner's cleanup reaps such a zombie through its pidfd if
/// the test fails first.
#[cfg(target_os = "linux")]
struct Subreaper {
    previous: libc::c_int,
}

#[cfg(target_os = "linux")]
impl Subreaper {
    fn get() -> libc::c_int {
        let mut value: libc::c_int = 0;
        // SAFETY: `prctl` writes one int through the valid pointer.
        let got = unsafe {
            libc::prctl(
                libc::PR_GET_CHILD_SUBREAPER,
                std::ptr::addr_of_mut!(value),
                0,
                0,
                0,
            )
        };
        assert_eq!(got, 0, "PR_GET_CHILD_SUBREAPER");
        value
    }

    fn set_to(value: libc::c_int) -> bool {
        // SAFETY: plain `prctl` with integer arguments.
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, value, 0, 0, 0) == 0 }
    }

    fn enable() -> Self {
        let previous = Self::get();
        assert!(Self::set_to(1), "PR_SET_CHILD_SUBREAPER");
        Self { previous }
    }
}

#[cfg(target_os = "linux")]
impl Drop for Subreaper {
    fn drop(&mut self) {
        let restored = Self::set_to(self.previous) && Self::get() == self.previous;
        println!(
            "    (child subreaper restored to {}: {})",
            self.previous,
            if restored { "ok" } else { "FAILED" }
        );
        if !restored && !std::thread::panicking() {
            panic!("the child subreaper setting was not restored");
        }
    }
}

#[cfg(target_os = "linux")]
async fn a_killed_member_left_in_the_group_holds_the_shutdown_and_the_server() {
    // A sent kill is not the tree's end. The server exits on end of input
    // and leaves a grandchild in its group; with this process a subreaper,
    // the killed grandchild stays a zombie member of the group until the
    // test reaps it. Until then every shutdown is incomplete, with the
    // server exited but held unreaped (its pid still pins the group's id);
    // once the member is reaped, the next shutdown completes.
    let _subreaper = Subreaper::enable();
    let mut options = options();
    options.kill_wait = Duration::from_millis(300);
    let spawned = spawn(fixture("parent"), options).await;
    let grandchild = grandchild(&spawned).await;
    let Spawned {
        process,
        mut connecting,
        server,
    } = spawned;
    let (client, _events) = initialize(&mut connecting).await;
    client.shutdown().await;
    for attempt in 0..2 {
        let error = bounded("an incomplete shutdown", process.shutdown())
            .await
            .expect_err("the shutdown completed with a member left in the group");
        assert!(error.exited, "attempt {attempt}: {error:?}");
        assert_eq!(error.stop, Stop::Exited, "attempt {attempt}");
        assert!(
            server.is_zombie(),
            "attempt {attempt}: the server was reaped before its tree ended"
        );
        assert!(
            grandchild.is_zombie(),
            "attempt {attempt}: the member is not a zombie"
        );
    }
    inject("subreaper-before-reap");
    assert!(
        grandchild.reap_if_own_exited_child(),
        "the zombie member could not be reaped"
    );
    let report = shutdown(&process).await;
    assert_eq!(report.stop, Stop::Exited, "{report:?}");
    assert!(report.status.is_some_and(|status| status.success()));
    assert_eq!(report.tree, TreeEnd::Empty, "{report:?}");
    assert!(server.is_reaped(), "the server was not reaped");
}

// ------------------------------------------------------------------ runner

type Test = fn() -> Pin<Box<dyn Future<Output = ()> + Send>>;

macro_rules! tests {
    ($($name:ident),* $(,)?) => {
        vec![$((stringify!($name), (|| Box::pin($name()) as Pin<Box<dyn Future<Output = ()> + Send>>) as Test)),*]
    };
}

/// How a test ended, before its cleanup.
enum Outcome {
    Passed,
    TimedOut,
    Panicked,
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
        a_dropped_owner_whose_leader_was_reaped_elsewhere_signals_nothing,
    ]);
    #[cfg(target_os = "linux")]
    tests.extend(tests![
        a_killed_member_left_in_the_group_holds_the_shutdown_and_the_server
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
        // The test runs on a thread and runtime of its own; a panic is
        // caught there, so the runtime is always dropped (running every
        // owner's drop fallback) before the cleanup, on that same thread.
        let joined = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("a runtime");
            let run = std::panic::catch_unwind(AssertUnwindSafe(|| {
                runtime
                    .block_on(async { tokio::time::timeout(BOUND, AssertUnwindSafe(test())).await })
            }));
            drop(runtime);
            let outcome = match run {
                Ok(Ok(())) => Outcome::Passed,
                Ok(Err(_)) => Outcome::TimedOut,
                Err(_) => Outcome::Panicked,
            };
            (outcome, observe::drain())
        })
        .join();
        let (outcome, (cleanup, clean)) = match joined {
            Ok(result) => result,
            // The cleanup itself panicked; nothing more can be done for it.
            Err(_) => (
                Outcome::Panicked,
                (vec!["LEFT: the cleanup panicked".into()], false),
            ),
        };
        let verdict = match (&outcome, clean) {
            (Outcome::Passed, true) => "ok".to_owned(),
            (Outcome::Passed, false) => "FAILED (cleanup)".to_owned(),
            (Outcome::TimedOut, clean) => format!(
                "FAILED (no result within {BOUND:?}){}",
                if clean { "" } else { "; cleanup FAILED" }
            ),
            (Outcome::Panicked, clean) => {
                format!("FAILED{}", if clean { "" } else { "; cleanup FAILED" })
            }
        };
        if !matches!(outcome, Outcome::Passed) || !clean {
            for line in &cleanup {
                println!("    cleanup: {line}");
            }
            failed.push(name);
        }
        println!("test {name} ... {verdict}");
    }
    if !failed.is_empty() {
        println!("\nfailures:\n    {}", failed.join("\n    "));
        println!("\ntest result: FAILED. {} failed", failed.len());
        std::process::exit(101);
    }
    println!("\ntest result: ok.");
}
