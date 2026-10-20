//! The Wasm plugin host end to end: `bake-coding-agent`'s parent API
//! driving the shipped `bake` binary as its helper, with the committed
//! `guard` and hostile components and components built here from
//! core-module text.
//!
//! Every test that starts a helper ends by checking that the helper's
//! process is gone (reaped), whether the test passed through shutdown,
//! cancellation, a timeout, or a failure. Waits are bounded and keyed to
//! state (the host's status, frames, process exit), never to sleeps.

#[path = "wasm_plugin_host/fixtures.rs"]
mod fixtures;

use std::time::Duration;

use bake_ai::AbortController;
use bake_coding_agent::plugins::ToolCallVeto;
use bake_coding_agent::plugins::wasm::{
    CallError, CompileQueue, ExitReport, HELPER_ARG, HelperCommand, HostLimits, HostStatus,
    LimitMechanism, LoadFailure, LogLevel, MAX_ARGS_BYTES, StartError, TrapKind, WasmPackage,
    WasmPluginHost,
};
use fixtures::*;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;

/// Shuts `host` down and checks the helper was reaped and drained.
async fn finish(host: &WasmPluginHost) {
    let pid = host.pid().expect("the helper has a pid");
    let exit = host.shutdown().await;
    assert!(exit.is_complete(), "{exit:?}");
    assert!(process_gone(pid), "helper {pid} survived its shutdown");
    assert_eq!(*host.status().borrow(), HostStatus::Stopped);
}

fn veto(result: Result<bake_coding_agent::plugins::wasm::CheckedVeto, CallError>) -> ToolCallVeto {
    result.expect("the plugin answers").veto
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guard_blocks_protected_paths_through_the_shipped_binary() {
    let limits = limits();
    let host = start(&guard(), &limits).await;
    assert_eq!(host.manifest().id().as_str(), "guard");
    let limit = host.memory_limit().expect("initialized");
    assert_eq!(limit.budget, limits.memory_budget);
    if cfg!(windows) {
        assert_eq!(
            (limit.mechanism, limit.limit),
            (LimitMechanism::JobCommit, limit.budget)
        );
    } else {
        assert_eq!(limit.mechanism, LimitMechanism::AddressSpace);
        assert_eq!(limit.limit, limit.baseline + limit.budget);
        // Linux bounds the absolute limit as well.
        #[cfg(target_os = "linux")]
        assert!(
            limit.baseline <= bake_coding_agent::plugins::wasm::MAX_LINUX_BASELINE,
            "{limit:?}"
        );
    }
    // The kernel's view of the helper agrees with its report.
    #[cfg(target_os = "linux")]
    {
        let pid = host.pid().expect("pid");
        let limits_file = std::fs::read_to_string(format!("/proc/{pid}/limits")).expect("limits");
        let line = limits_file
            .lines()
            .find(|line| line.starts_with("Max address space"))
            .expect("address-space line");
        let fields: Vec<&str> = line.split_whitespace().collect();
        let expected = limit.limit.to_string();
        assert_eq!(
            (fields[3], fields[4]),
            (expected.as_str(), expected.as_str()),
            "{line}"
        );
        let core = limits_file
            .lines()
            .find(|line| line.starts_with("Max core file size"))
            .expect("core line");
        assert!(core.split_whitespace().nth(4) == Some("0"), "{core}");
    }

    let args = json!({ "path": "app/.env", "content": "x" });
    let answer = host
        .on_tool_call(call("write", &args), None)
        .await
        .expect("answer");
    assert_eq!(
        answer.veto,
        ToolCallVeto::Block {
            reason: "Path \"app/.env\" is protected".to_owned(),
            terminate: false
        }
    );
    assert_eq!(answer.logs.len(), 1);
    assert_eq!(answer.logs[0].level, LogLevel::Warn);
    assert_eq!(
        answer.logs[0].text,
        "Blocked write to protected path: app/.env"
    );
    assert_eq!(answer.instance, 1);

    let edit = json!({ "path": "node_modules/x/index.js", "edits": [] });
    assert!(matches!(
        veto(host.on_tool_call(call("edit", &edit), None).await),
        ToolCallVeto::Block { .. }
    ));
    let allowed = json!({ "path": "src/main.rs", "content": "fn main() {}" });
    let answer = host
        .on_tool_call(call("write", &allowed), None)
        .await
        .expect("answer");
    assert_eq!(
        (answer.veto, answer.logs.len()),
        (ToolCallVeto::Continue, 0)
    );
    // Pi's matching is case-sensitive, with no normalization.
    let upper = json!({ "path": ".ENV" });
    assert_eq!(
        veto(host.on_tool_call(call("write", &upper), None).await),
        ToolCallVeto::Continue
    );
    // An unlisted tool is never sent; a call without a path is the plugin's error.
    assert_eq!(
        host.on_tool_call(call("read", &allowed), None).await,
        Err(CallError::NotListed)
    );
    assert_eq!(
        host.on_tool_call(call("edit", &json!({})), None).await,
        Err(CallError::Guest("edit call has no string path".to_owned()))
    );
    // Too large for a plugin: refused before the helper.
    let huge = json!({ "path": "a", "content": "x".repeat(MAX_ARGS_BYTES) });
    assert!(matches!(
        host.on_tool_call(call("write", &huge), None).await,
        Err(CallError::ArgumentsTooLarge)
    ));
    // The helper still serves the same instance.
    let answer = host
        .on_tool_call(call("write", &args), None)
        .await
        .expect("answer");
    assert_eq!(answer.instance, 1);

    let pid = host.pid().expect("pid");
    let exit = host.shutdown().await;
    assert_eq!(
        (
            exit.spawned,
            exit.killed,
            exit.code,
            exit.reaped,
            exit.drained
        ),
        (true, false, Some(0), true, true),
        "{exit:?}"
    );
    assert!(process_gone(pid));
    assert_eq!(host.shutdown().await, exit, "shutdown is idempotent");
    assert_eq!(
        host.on_tool_call(call("write", &args), None).await,
        Err(CallError::Unavailable)
    );
    assert_eq!(*host.status().borrow(), HostStatus::Stopped);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_guest_has_no_ambient_authority() {
    let dir = TempDir::new();
    let sentinel = dir.path().join("credentials");
    std::fs::write(&sentinel, "secret-sentinel").expect("write sentinel");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    listener.set_nonblocking(true).expect("nonblocking");
    let addr = listener.local_addr().expect("addr");

    let host = start(&hostile(), &limits()).await;
    let args = json!({ "path": sentinel.to_str().expect("utf-8 path"), "addr": addr.to_string() });
    let ToolCallVeto::Block { reason, .. } =
        veto(host.on_tool_call(call("ambient", &args), None).await)
    else {
        panic!("the ambient probe blocks with its report");
    };
    let report: Value = serde_json::from_str(&reason).expect("JSON report");
    // The working directory is WASI's nominal `/`; nothing resolves there.
    for key in [
        "read_absolute",
        "read_relative",
        "list_root",
        "create_file",
        "connect",
        "spawn",
    ] {
        assert!(
            report[key].get("err").is_some(),
            "{key} succeeded: {report}"
        );
    }
    assert_eq!(
        (report["env"].as_u64(), report["args"].as_u64()),
        (Some(0), Some(0)),
        "{report}"
    );
    // Standard input is closed: an empty read.
    assert_eq!(report["stdin"], json!({ "ok": "0" }), "{report}");
    assert!(!reason.contains("secret-sentinel"));
    assert_eq!(
        listener.accept().map_err(|error| error.kind()).err(),
        Some(std::io::ErrorKind::WouldBlock),
        "the guest reached the listener"
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).expect("list").count(),
        1,
        "the guest created a file"
    );
    finish(&host).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guest_limits_trap_and_a_fresh_instance_answers_next() {
    let limits = HostLimits {
        call_fuel: 200_000_000,
        ..limits()
    };
    let host = start(&hostile(), &limits).await;
    let none = json!({});
    let trap = |result: Result<_, CallError>| match result {
        Err(CallError::Trap { kind, message }) => (kind, message),
        other => panic!("expected a trap, got {other:?}"),
    };

    // CPU: the fuel runs out.
    assert_eq!(
        trap(host.on_tool_call(call("spin", &none), None).await).0,
        TrapKind::OutOfFuel
    );
    let answer = host
        .on_tool_call(call("write", &none), None)
        .await
        .expect("fresh instance");
    assert_eq!((answer.veto, answer.instance), (ToolCallVeto::Continue, 2));

    // Memory: 100 MiB is over the 64 MiB store total; 16 MiB is not.
    let (kind, message) = trap(
        host.on_tool_call(call("grow", &json!({ "mib": 100 })), None)
            .await,
    );
    assert_eq!(kind, TrapKind::Memory, "{message}");
    assert_eq!(
        veto(
            host.on_tool_call(call("grow", &json!({ "mib": 16 })), None)
                .await
        ),
        ToolCallVeto::Block {
            reason: format!("grew {}", 16 << 20),
            terminate: false
        }
    );

    // Returns: a reason over the 128 KiB copy budget is refused before the
    // host allocates it; one over 16 KiB but within the budget is invalid.
    let (kind, message) = trap(
        host.on_tool_call(call("reason", &json!({ "bytes": 32 << 20 })), None)
            .await,
    );
    assert_eq!(kind, TrapKind::Copy, "{message}");
    assert!(
        message.contains("fuel allocated for hostcalls has been exhausted"),
        "{message}"
    );
    let (kind, _) = trap(
        host.on_tool_call(call("reason", &json!({ "bytes": 17 * 1024 })), None)
            .await,
    );
    assert_eq!(kind, TrapKind::InvalidReturn);
    let exact = host
        .on_tool_call(call("reason", &json!({ "bytes": 16 * 1024 })), None)
        .await
        .expect("a 16 KiB reason is valid");
    assert!(matches!(exact.veto, ToolCallVeto::Block { reason, .. } if reason.len() == 16 * 1024));

    // Host-call arguments: a 2 MiB log line is refused before `log` runs.
    let (kind, _) = trap(
        host.on_tool_call(call("log-huge", &json!({ "bytes": 2 << 20 })), None)
            .await,
    );
    assert_eq!(kind, TrapKind::Copy);

    // A panic traps; an error answer does not.
    assert_eq!(
        trap(host.on_tool_call(call("panic", &none), None).await).0,
        TrapKind::Other
    );
    assert_eq!(
        host.on_tool_call(call("error", &none), None).await,
        Err(CallError::Guest("hostile error".to_owned()))
    );
    finish(&host).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_helper_deadline_stops_a_spin_through_cooperative_yields() {
    // Effectively unlimited fuel: only the deadline can end the spin, and
    // only because fuel yields let the helper's timer run.
    let limits = HostLimits {
        call_fuel: u64::MAX / 4,
        call_timeout: Duration::from_millis(300),
        ..limits()
    };
    let host = start(&hostile(), &limits).await;
    let result = host.on_tool_call(call("spin", &json!({})), None).await;
    assert!(
        matches!(
            result,
            Err(CallError::Trap {
                kind: TrapKind::Deadline,
                ..
            })
        ),
        "the helper, not the parent's kill, ended the call: {result:?}"
    );
    // The same helper answers with a fresh instance.
    let answer = host
        .on_tool_call(call("write", &json!({})), None)
        .await
        .expect("answer");
    assert_eq!(answer.instance, 2);
    finish(&host).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_logs_and_output_stay_within_quotas() {
    let host = start(&hostile(), &limits()).await;
    // 1,000 lines lifted from one aliased 4 KiB string: the call keeps 64 KiB.
    let answer = host
        .on_tool_call(call("log-alias", &json!({ "times": 1000 })), None)
        .await
        .expect("answer");
    assert_eq!((answer.logs.len(), answer.dropped_logs), (16, 984));
    // The host keeps 1 MiB over all calls: 16 such calls fill it.
    for _ in 0..15 {
        let answer = host
            .on_tool_call(call("log-alias", &json!({ "times": 16 })), None)
            .await
            .expect("answer");
        assert_eq!(answer.logs.len(), 16);
    }
    let answer = host
        .on_tool_call(call("log-alias", &json!({ "times": 16 })), None)
        .await
        .expect("answer");
    assert_eq!((answer.logs.len(), answer.dropped_logs), (0, 16));

    // A 4 MiB flood on each stream: the capture keeps at most 64 KiB, then
    // writes fail.
    let ToolCallVeto::Block { reason, .. } = veto(
        host.on_tool_call(call("flood-output", &json!({})), None)
            .await,
    ) else {
        panic!("the flood reports");
    };
    let report: Value = serde_json::from_str(&reason).expect("JSON");
    for stream in ["stdout", "stderr"] {
        let written = report[stream]["written"].as_u64().expect("count");
        assert!((1..=65536).contains(&written), "{report}");
        assert!(report[stream]["error"].is_string(), "{report}");
    }
    finish(&host).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn table_and_multi_memory_growth_hit_the_store_totals() {
    // Table elements: `args_json`'s length is the growth.
    let table = package(
        "table",
        &["write"],
        wat_plugin("ref.null func\n local.get 5\n table.grow $t\n drop", ""),
    );
    let host = start(&table, &limits()).await;
    let small = json!({ "pad": "x".repeat(100) });
    assert_eq!(
        veto(host.on_tool_call(call("write", &small), None).await),
        ToolCallVeto::Continue
    );
    let large = json!({ "pad": "x".repeat(20_000) });
    let result = host.on_tool_call(call("write", &large), None).await;
    assert!(
        matches!(
            result,
            Err(CallError::Trap {
                kind: TrapKind::Table,
                ..
            })
        ),
        "{result:?}"
    );
    finish(&host).await;

    // Two memories of 40 MiB each: the second growth, made only for long
    // arguments, passes the 64 MiB sum.
    let memories = package(
        "memories",
        &["write"],
        wat_plugin(
            "i32.const 640\n memory.grow $m2\n drop\n local.get 5\n i32.const 1000\n i32.gt_u\n if\n i32.const 640\n memory.grow $mem\n drop\n end",
            "(memory $m2 1)",
        ),
    );
    let host = start(&memories, &limits()).await;
    assert_eq!(
        veto(host.on_tool_call(call("write", &small), None).await),
        ToolCallVeto::Continue
    );
    let result = host.on_tool_call(call("write", &large), None).await;
    assert!(
        matches!(
            result,
            Err(CallError::Trap {
                kind: TrapKind::Memory,
                ..
            })
        ),
        "{result:?}"
    );
    finish(&host).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn components_outside_the_contract_are_refused_before_compiling() {
    let refused = |result: Result<WasmPluginHost, StartError>| match result {
        Err(StartError::Refused {
            failure,
            message,
            exit,
        }) => {
            assert_eq!((exit.code, exit.reaped), (Some(0), true), "{exit:?}");
            (failure, message)
        }
        other => panic!("expected a refusal, got {other:?}"),
    };
    // Another API version, in both the export and the import.
    let v2 = WIT.replace("@0.1.0", "@0.2.0");
    let module = plugin_module(
        "i32.const 0\n i32.const 0\n i32.const 0\n call $log",
        r#"(import "bake:plugin/host@0.2.0" "log" (func $log (param i32 i32 i32)))"#,
        "bake:plugin/plugin@0.2.0#on-tool-call",
    );
    let (failure, message) = refused(
        try_start(
            &package("v2", &["write"], componentize(&module, &v2, "guest")),
            &limits(),
        )
        .await,
    );
    assert_eq!(failure, LoadFailure::Invalid);
    assert!(
        message.contains(
            "bake:plugin/host@0.2.0 is not supported: this Bake links bake:plugin/host@0.1.0"
        ),
        "{message}"
    );

    // An import outside the allowlist.
    let evil = WIT.replace("world guest {", "world guest {\n  import evil: func();");
    let module = plugin_module(
        "call $evil",
        r#"(import "$root" "evil" (func $evil))"#,
        ON_TOOL_CALL,
    );
    let (failure, message) = refused(
        try_start(
            &package("evil", &["write"], componentize(&module, &evil, "guest")),
            &limits(),
        )
        .await,
    );
    assert_eq!(
        (failure, message.as_str()),
        (LoadFailure::Invalid, "import evil is not allowed")
    );

    // A missing export, and an extra one.
    let bare = WIT.replace("  export plugin;\n", "");
    let module = wat::parse_str(r#"(module (memory (export "memory") 1))"#).expect("module");
    let (_, message) = refused(
        try_start(
            &package("bare", &["write"], componentize(&module, &bare, "guest")),
            &limits(),
        )
        .await,
    );
    assert_eq!(
        message,
        "the component does not export bake:plugin/plugin@0.1.0"
    );
    let extra = WIT.replace(
        "  export plugin;\n",
        "  export plugin;\n  export extra: func();\n",
    );
    let module = plugin_module("", r#"(func (export "extra"))"#, ON_TOOL_CALL);
    let (_, message) = refused(
        try_start(
            &package("extra", &["write"], componentize(&module, &extra, "guest")),
            &limits(),
        )
        .await,
    );
    assert_eq!(message, "export extra is not allowed");

    // A component header over garbage: the validating parse refuses it.
    let garbage = package("garbage", &["write"], b"\0asm\x0d\0\x01\0garbage".to_vec());
    let (failure, message) = refused(try_start(&garbage, &limits()).await);
    assert_eq!(failure, LoadFailure::Invalid);
    assert!(message.starts_with("invalid component"), "{message}");

    // A manifest whose API is not supported never starts a helper.
    let manifest = br#"{"api":"0.2.0","id":"g","version":"1","vetoTools":["write"]}"#;
    assert!(WasmPackage::new(manifest, guard().component().to_vec()).is_err());
    // Neither does a core module.
    assert!(
        WasmPackage::new(
            br#"{"api":"0.1.0","id":"g","version":"1","vetoTools":["write"]}"#,
            b"\0asm\x01\0\0\0".to_vec()
        )
        .is_err()
    );
    // Over the host's component limit.
    let small = HostLimits {
        max_component_bytes: 1024,
        ..limits()
    };
    assert!(matches!(
        try_start(&guard(), &small).await,
        Err(StartError::InvalidLimits(_))
    ));
}

/// A wall-clock bound that works while Tokio's clock is paused: a thread
/// this value owns completes `expired` after `timeout` of real time.
/// [`RealTimer::stop`] (or dropping it) cancels the thread and joins it.
struct RealTimer {
    expired: tokio::sync::oneshot::Receiver<()>,
    cancel: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RealTimer {
    fn start(timeout: Duration) -> Self {
        let (expire, expired) = tokio::sync::oneshot::channel();
        let (cancel, cancelled) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            // A dropped sender ends the wait at once.
            if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = cancelled.recv_timeout(timeout)
            {
                let _ = expire.send(());
            }
        });
        Self {
            expired,
            cancel: Some(cancel),
            thread: Some(thread),
        }
    }

    fn stop(&mut self) {
        drop(self.cancel.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for RealTimer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The start's task; a `JoinHandle` may be awaited only until it settles,
/// so this remembers that it has.
struct StartTask {
    handle: tokio::task::JoinHandle<Result<(), StartError>>,
    settled: bool,
}

impl StartTask {
    /// Awaits the task; cancel-safe, and pending forever once settled.
    async fn join(&mut self) -> Result<Result<(), StartError>, tokio::task::JoinError> {
        if self.settled {
            return std::future::pending().await;
        }
        let joined = (&mut self.handle).await;
        self.settled = true;
        joined
    }

    /// Cancels the task if it is still running and awaits it.
    async fn cancel(&mut self) {
        if !self.settled {
            self.handle.abort();
            let _ = (&mut self.handle).await;
            self.settled = true;
        }
    }
}

/// Drives the paused-clock start to its outcome: `Ok` once the start task
/// has settled after the advanced deadline, `Err` with the reason when the
/// test must fail. Expected failures return to the caller for cleanup.
async fn drive_past_compile_deadline(
    start: &mut StartTask,
    seen: &mut watch::Receiver<HostStatus>,
    trace: &mut Vec<HostStatus>,
    timer: &mut RealTimer,
    paused: &mut bool,
    deadline: Duration,
) -> Result<Result<(), StartError>, String> {
    loop {
        tokio::select! {
            changed = seen.changed() => {
                if changed.is_err() {
                    return Err(format!("the host dropped its status sender: {trace:?}"));
                }
                let state = *seen.borrow_and_update();
                trace.push(state);
                if matches!(state, HostStatus::Compiling(_)) {
                    break;
                }
                if !matches!(state, HostStatus::Starting | HostStatus::New | HostStatus::Spawned(_)) {
                    return Err(format!("the start ended before compiling: {trace:?}"));
                }
            }
            joined = start.join() => {
                return Err(format!("the start settled before compiling: {joined:?}; {trace:?}"));
            }
            _ = &mut timer.expired => {
                return Err(format!("no Compiling within 120 s of real time: {trace:?}"));
            }
        }
    }
    // Past the deadline: the host's next poll sees it expired.
    tokio::time::advance(deadline + Duration::from_secs(1)).await;
    tokio::time::resume();
    *paused = false;
    match tokio::time::timeout(Duration::from_secs(60), start.join()).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => Err(format!("the start task failed: {error:?}; {trace:?}")),
        Err(_) => Err(format!(
            "the start did not settle within 60 s of the deadline: {trace:?}"
        )),
    }
}

/// The compile deadline covers writing and validating the Load frame as
/// well as compiling, and how long those take depends on the machine:
/// on an Intel Mac under the full suite validation alone took 2.2 s. So
/// the deadline is driven, not estimated. Tokio's clock is paused (a
/// spinning task keeps the runtime from auto-advancing it), so the host's
/// deadline cannot expire on its own; once the host reports `Compiling`,
/// which the helper sends after validating and before compiling, the test
/// advances the clock past the deadline and resumes it. The remaining
/// assumption is that compiling this component (1.7 s on Linux, about
/// 4 s on that Mac) outlasts the time from the helper's `Compiling` frame
/// to the host acting on the expired deadline; the status trace in the
/// failure message shows that order if it is ever violated.
///
/// The test owns everything it starts until it has finished with it: on
/// each returned outcome it stops and awaits the spinner, resumes the clock,
/// cancels and awaits a start still running (a dropped `initialize` leaves
/// the helper with the host), attempts the host's `shutdown` with a bound,
/// and joins the real-time timer. It checks teardown before the outcome;
/// an incomplete teardown fails the test.
#[tokio::test(start_paused = true)]
async fn a_compile_past_its_deadline_is_killed_and_reaped() {
    let slow = package("slow", &["write"], slow_component(1400, 1000));
    let limits = HostLimits {
        compile_timeout: Duration::from_secs(30),
        max_component_bytes: 16 << 20,
        memory_budget: 512 << 20,
        ..limits()
    };
    let (status, mut seen) = watch::channel(HostStatus::Starting);
    let host = std::sync::Arc::new(
        new_host(
            &command(),
            &slow,
            &limits,
            CompileQueue::new(4),
            Some(status),
        )
        .expect("a host"),
    );
    let mut trace = vec![*seen.borrow_and_update()];
    let mut timer = RealTimer::start(Duration::from_secs(120));
    let mut paused = true;
    // Keeps the runtime busy so the paused clock never auto-advances, and
    // with it the startup and compile deadlines stay where they are.
    let spinner = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    let mut start = StartTask {
        handle: tokio::spawn({
            let host = host.clone();
            async move { host.initialize(None).await }
        }),
        settled: false,
    };

    let outcome = drive_past_compile_deadline(
        &mut start,
        &mut seen,
        &mut trace,
        &mut timer,
        &mut paused,
        limits.compile_timeout,
    )
    .await;

    // Cleanup, on every outcome, before any assertion.
    spinner.abort();
    let _ = spinner.await;
    if paused {
        tokio::time::resume();
    }
    start.cancel().await;
    let exit = tokio::time::timeout(Duration::from_secs(60), host.shutdown()).await;
    let pid = host.pid();
    timer.stop();
    let exit = exit.expect("the host's shutdown settles within 60 s");
    assert!(exit.is_complete(), "{exit:?}; statuses {trace:?}");
    if let Some(pid) = pid {
        assert!(process_gone(pid), "helper {pid} survived the test");
    }

    let result = outcome.unwrap_or_else(|reason| panic!("{reason}"));
    let Err(StartError::CompileTimeout { exit: reported }) = result else {
        panic!("expected a compile timeout, got {result:?}; statuses {trace:?}");
    };
    assert_eq!(reported, exit, "the failed start reported its teardown");
    assert!(exit.killed && exit.reaped && exit.drained, "{exit:?}");
    #[cfg(unix)]
    assert_eq!(exit.signal, Some(libc::SIGKILL));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_compile_queue_admits_one_start_and_bounds_the_wait() {
    let queue = CompileQueue::new(1);
    let limits = HostLimits {
        max_component_bytes: 16 << 20,
        memory_budget: 512 << 20,
        ..limits()
    };
    let slow = package("slow", &["write"], slow_component(1400, 1000));
    let guard = guard();
    let command = command();
    let first = AbortController::new();
    let first_signal = first.signal();
    let (status, mut seen) = watch::channel(HostStatus::New);
    let slow_host =
        new_host(&command, &slow, &limits, queue.clone(), Some(status)).expect("slow host");
    let waiting_host =
        new_host(&command, &guard, &limits, queue.clone(), None).expect("waiting host");
    let third_host = new_host(&command, &guard, &limits, queue.clone(), None).expect("third host");
    let slow_start = slow_host.initialize(Some(&first_signal));
    let waiting_start = waiting_host.initialize(None);
    let driver = async {
        // The first start compiles; the second waits; a third is refused.
        wait_status(&mut seen, |state| matches!(state, HostStatus::Compiling(_))).await;
        let third = third_host.initialize(None).await;
        assert!(matches!(third, Err(StartError::QueueFull)), "{third:?}");
        // A refused start spawned nothing.
        assert_eq!(third_host.shutdown().await, ExitReport::default());
        first.abort();
    };
    let (slow_result, waiting_result, ()) = tokio::join!(slow_start, waiting_start, driver);
    let Err(StartError::Cancelled { exit: Some(exit) }) = slow_result else {
        panic!("expected the cancelled start, got {slow_result:?}");
    };
    assert!(exit.killed && exit.is_complete(), "{exit:?}");
    assert_eq!(slow_host.shutdown().await, exit);
    waiting_result.expect("the waiting start runs once admitted");
    finish(&waiting_host).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_mid_host_call_kills_the_helper_and_returns_nothing() {
    let limits = HostLimits {
        call_timeout: Duration::from_secs(120),
        ..limits()
    };
    let host = start(&hostile(), &limits).await;
    let pid = host.pid().expect("pid");
    let controller = AbortController::new();
    let signal = controller.signal();
    let mut status = host.status();
    let args = json!({});
    let pending = host.on_tool_call(call("sleep", &args), Some(&signal));
    let cancel = async {
        wait_status(&mut status, |state| matches!(state, HostStatus::Calling(_))).await;
        // Linux: wait until the guest is parked in its WASI sleep, a host
        // future the helper awaits.
        #[cfg(target_os = "linux")]
        tokio::time::timeout(Duration::from_secs(30), async {
            while linux_state(pid) != Some('S') {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the helper sleeps in the host call");
        controller.abort();
    };
    let (result, ()) = tokio::join!(pending, cancel);
    let Err(CallError::Cancelled { exit: Some(exit) }) = result else {
        panic!("expected a cancelled call, got {result:?}");
    };
    assert!(exit.killed && exit.reaped && exit.drained, "{exit:?}");
    assert!(process_gone(pid));
    // Nothing the call staged ("before") came back, and the host is stopped.
    assert_eq!(
        host.on_tool_call(call("write", &args), None).await,
        Err(CallError::Unavailable)
    );
    assert_eq!(host.shutdown().await, exit);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_while_a_call_is_pending_interrupts_and_reaps_it() {
    let limits = HostLimits {
        call_timeout: Duration::from_secs(120),
        ..limits()
    };
    let host = start(&hostile(), &limits).await;
    let pid = host.pid().expect("pid");
    let mut status = host.status();
    let args = json!({});
    let pending = host.on_tool_call(call("sleep", &args), None);
    let stop = async {
        wait_status(&mut status, |state| matches!(state, HostStatus::Calling(_))).await;
        host.shutdown().await
    };
    let (result, exit) = tokio::join!(pending, stop);
    assert_eq!(result, Err(CallError::ShutDown { exit: exit.clone() }));
    assert!(exit.killed && exit.reaped, "{exit:?}");
    assert!(process_gone(pid));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_call_kills_at_once_and_shutdown_reaps() {
    let limits = HostLimits {
        call_timeout: Duration::from_secs(120),
        ..limits()
    };
    let host = start(&hostile(), &limits).await;
    let pid = host.pid().expect("pid");
    let mut status = host.status();
    let args = json!({});
    tokio::select! {
        result = host.on_tool_call(call("sleep", &args), None) => panic!("the call finished: {result:?}"),
        () = wait_status(&mut status, |state| matches!(state, HostStatus::Calling(_))) => {}
    }
    // The dropped call sent the kill; the helper is dead but unreaped.
    #[cfg(target_os = "linux")]
    tokio::time::timeout(Duration::from_secs(30), async {
        while linux_state(pid) != Some('Z') {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the helper died when the call was dropped");
    let exit = host.shutdown().await;
    assert!(exit.killed && exit.reaped, "{exit:?}");
    assert!(process_gone(pid));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_helper_that_dies_fails_the_call_with_its_exit() {
    let limits = HostLimits {
        call_timeout: Duration::from_secs(120),
        ..limits()
    };
    let host = start(&hostile(), &limits).await;
    let pid = host.pid().expect("pid");
    let mut status = host.status();
    let args = json!({});
    let pending = host.on_tool_call(call("sleep", &args), None);
    let kill = async {
        wait_status(&mut status, |state| matches!(state, HostStatus::Calling(_))).await;
        kill_externally(pid);
    };
    let (result, ()) = tokio::join!(pending, kill);
    let Err(CallError::Helper { exit, .. }) = result else {
        panic!("expected a helper failure, got {result:?}");
    };
    assert!(
        !exit.killed && exit.reaped,
        "the parent did not kill it: {exit:?}"
    );
    #[cfg(unix)]
    assert_eq!(exit.signal, Some(libc::SIGKILL));
    assert!(process_gone(pid));
    assert_eq!(
        host.on_tool_call(call("write", &args), None).await,
        Err(CallError::Unavailable)
    );
}

/// Kills `pid` from outside the host, as a crash would.
fn kill_externally(pid: u32) {
    #[cfg(unix)]
    // SAFETY: plain syscall on the helper, which the host has not reaped.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_TERMINATE, TerminateProcess,
        };
        // SAFETY: plain calls on a process handle closed below.
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            assert!(!handle.is_null(), "open the helper");
            TerminateProcess(handle, 9);
            CloseHandle(handle);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn helper_startup_failures_are_reported_and_reaped() {
    // No such program.
    let missing = HelperCommand::new("/nonexistent/bake", [HELPER_ARG]);
    assert!(matches!(
        try_start_with(&missing, &guard(), &limits(), None).await,
        Err(StartError::Spawn(_))
    ));
    // The helper refuses its arguments before any Hello.
    let usage = HelperCommand::new(BAKE_RS, [HELPER_ARG, "--bogus", "1"]);
    let Err(StartError::Handshake { message, exit }) =
        try_start_with(&usage, &guard(), &limits(), None).await
    else {
        panic!("expected a handshake failure");
    };
    assert_eq!(
        (exit.code, exit.killed, exit.reaped),
        (Some(64), false, true),
        "{message}: {exit:?}"
    );
    assert!(exit.stderr.contains("unknown argument --bogus"), "{exit:?}");
    // A program that is not the helper.
    let other = HelperCommand::new(BAKE_RS, ["--help"]);
    assert!(matches!(
        try_start_with(&other, &guard(), &limits(), None).await,
        Err(StartError::Handshake { .. })
    ));
}

/// On macOS the helper refuses to start without `MallocMediumZone=0` and
/// `MallocMaxMagazines=1`: on a Mac with 32 GiB or more, libmalloc's medium
/// allocator reserves 128 MiB of address space per region and CPU magazine,
/// and ordinary loads under the budget aborted at allocations of 36 to
/// 131 KiB; its tiny and small allocators map 1 MiB and 8 MiB regions per
/// CPU magazine, which left a 128 MiB guard too little room for a call
/// under the full suite. The host sets both; a shell removes or overrides
/// one here. This checks the
/// required setting, not allocator state; the positive guard and hostile
/// tests are the native evidence.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_helper_with_the_medium_allocator_refuses_to_start() {
    for (name, script, setting) in [
        (
            "unset",
            "unset MallocMediumZone; exec \"$0\" \"$@\"",
            "MallocMediumZone",
        ),
        (
            "on",
            "MallocMediumZone=1 exec \"$0\" \"$@\"",
            "MallocMediumZone",
        ),
        (
            "magazines unset",
            "unset MallocMaxMagazines; exec \"$0\" \"$@\"",
            "MallocMaxMagazines",
        ),
    ] {
        let command = HelperCommand::new("/bin/sh", ["-c", script, BAKE_RS, HELPER_ARG]);
        let Err(StartError::Handshake { exit, .. }) =
            try_start_with(&command, &guard(), &limits(), None).await
        else {
            panic!("{name}: a helper with the medium allocator on started");
        };
        assert_eq!(
            (exit.code, exit.reaped),
            (Some(70), true),
            "{name}: {exit:?}"
        );
        assert!(exit.stderr.contains(setting), "{name}: {exit:?}");
    }
    // The same shell path with the setting kept loads the guard.
    let kept = HelperCommand::new("/bin/sh", ["-c", "exec \"$0\" \"$@\"", BAKE_RS, HELPER_ARG]);
    let host = try_start_with(&kept, &guard(), &limits(), None)
        .await
        .expect("the guard loads with MallocMediumZone=0");
    assert!(host.shutdown().await.is_complete());
}

/// A hard address-space limit the helper cannot raise to its budget: it
/// must exit before its `Hello` rather than run with a smaller limit.
/// Linux only: macOS refuses an address-space limit below a process's
/// current size, which the shell cannot know.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_helper_without_its_memory_limit_refuses_to_start() {
    let capped = HelperCommand::new(
        "/bin/sh",
        [
            "-c",
            "ulimit -v 262144 && exec \"$0\" \"$@\"",
            BAKE_RS,
            HELPER_ARG,
        ],
    );
    let limits = HostLimits {
        memory_budget: 384 << 20,
        ..limits()
    };
    let Err(StartError::Handshake { exit, .. }) =
        try_start_with(&capped, &guard(), &limits, None).await
    else {
        panic!("expected a handshake failure");
    };
    assert_eq!(exit.code, Some(70), "{exit:?}");
    assert!(exit.stderr.contains("memory limit unavailable"), "{exit:?}");
}

/// The helper's own limit stops a hostile compile: one 300,000-operation
/// function needs far more than a 128 MiB budget, while the guard loads
/// and answers under the same budget, and on Linux the same function
/// compiles under the 512 MiB ceiling, so the budget is what refused it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_os_memory_limit_stops_an_oversized_compile() {
    let limits = HostLimits {
        memory_budget: 128 << 20,
        max_component_bytes: 16 << 20,
        ..limits()
    };
    let host = start(&guard(), &limits).await;
    let args = json!({ "path": ".env" });
    assert!(matches!(
        veto(host.on_tool_call(call("write", &args), None).await),
        ToolCallVeto::Block { .. }
    ));
    finish(&host).await;

    let heavy = package("heavy", &["write"], slow_component(1, 300_000));
    match try_start(&heavy, &limits).await {
        // Most allocations abort the helper...
        Err(StartError::Helper { message, exit }) => {
            assert!(!exit.killed && exit.reaped, "{message}: {exit:?}");
            assert!(exit.stderr.contains("memory allocation of"), "{exit:?}");
        }
        // ...and a refused code mapping fails the compile.
        Err(StartError::Refused {
            failure: LoadFailure::Compile,
            message,
            ..
        }) => {
            assert!(
                message.contains("mmap") || message.contains("memory"),
                "{message}"
            );
        }
        other => panic!("expected the limit to stop the compile, got {other:?}"),
    }
    // The roomy 512 MiB step runs on Linux only: how much address space
    // this function needs to compile on macOS has not been measured, so
    // there only the refusal is checked.
    #[cfg(target_os = "linux")]
    {
        let roomy = HostLimits {
            memory_budget: 512 << 20,
            ..limits
        };
        let host = try_start(&heavy, &roomy)
            .await
            .expect("the same function compiles with room");
        finish(&host).await;
    }
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_windows_helper_runs_in_its_job() {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::IsProcessInJob;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let host = start(&guard(), &limits()).await;
    let pid = host.pid().expect("pid");
    let mut in_job = 0;
    // SAFETY: plain calls on a handle closed below.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        assert!(!handle.is_null());
        assert_ne!(IsProcessInJob(handle, std::ptr::null_mut(), &mut in_job), 0);
        CloseHandle(handle);
    }
    assert_ne!(in_job, 0);
    finish(&host).await;
}

// ---------------------------------------------------------------------------
// The protocol, from both sides.

const GENERATION: u64 = 1;

fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![kind];
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn string(bytes: &mut Vec<u8>, text: &[u8]) {
    bytes.extend_from_slice(&(text.len() as u32).to_le_bytes());
    bytes.extend_from_slice(text);
}

fn call_payload(id: u64, generation: u64, name: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&id.to_le_bytes());
    bytes.extend_from_slice(&generation.to_le_bytes());
    string(&mut bytes, b"c");
    string(&mut bytes, name.as_bytes());
    string(&mut bytes, b"{}");
    bytes
}

fn load_payload(component: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    string(&mut bytes, b"0.1.0");
    string(&mut bytes, component);
    bytes
}

/// The host's helper launcher, compiled into this test from its private
/// source. On Windows the helper refuses to start outside the Job the host
/// creates it in (`os_limit::establish`), and only `CreateProcessW` with a
/// Job list puts it there from creation: stable `std` cannot pass that
/// attribute, and assigning a suspended process afterwards leaves a window
/// in which a dying test would orphan it unconfined. Including the module
/// keeps one launcher without widening the library's public API; a change
/// to it that this test crate cannot compile shows in the Windows lint.
#[cfg(windows)]
#[path = "../../bake-coding-agent/src/plugins/wasm/process.rs"]
// The host uses the kill switch; these tests need only the process.
#[allow(dead_code, unused_imports)]
mod process;

/// The helper's memory budget in the raw protocol tests.
const RAW_BUDGET: u64 = 512 << 20;

/// How much of a raw helper's standard error is kept, from its start,
/// where a startup refusal is reported. The rest is read and discarded so
/// the helper never blocks on a full pipe.
const RAW_STDERR_KEPT: usize = 16 * 1024;

type RawIn = Box<dyn tokio::io::AsyncWrite + Send + Unpin>;
type RawOut = Box<dyn tokio::io::AsyncRead + Send + Unpin>;

/// A raw helper's process: Tokio's child on Unix, the host's launcher's
/// process (its handle and Job) on Windows.
#[cfg(unix)]
struct RawProcess(tokio::process::Child);

#[cfg(unix)]
impl RawProcess {
    async fn spawn(args: &[String]) -> (Self, RawIn, RawOut, RawOut) {
        let mut child = tokio::process::Command::new(BAKE_RS)
            .args(args)
            .env_clear()
            // As the host starts it; see `a_helper_with_the_medium_allocator_refuses_to_start`.
            .envs(
                [("MallocMediumZone", "0"), ("MallocMaxMagazines", "1")]
                    .into_iter()
                    .filter(|_| cfg!(target_os = "macos")),
            )
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn the helper");
        // Owned from here: a missing pipe drops the child, which kills it.
        let pipes = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let (Some(stdin), Some(stdout), Some(stderr)) = pipes else {
            panic!("the helper's pipes are missing");
        };
        (
            Self(child),
            Box::new(stdin),
            Box::new(stdout),
            Box::new(stderr),
        )
    }

    /// The exit code (`None` for a signal), if the helper has exited;
    /// observing it reaps the helper.
    fn try_exit(&mut self) -> Option<Option<i32>> {
        let status = self.0.try_wait().expect("poll the helper");
        status.map(|status| status.code())
    }

    async fn wait(&mut self) -> Option<i32> {
        self.0.wait().await.expect("wait for the helper").code()
    }

    fn kill(&mut self) {
        // Fails only once the exit was observed, when nothing is left to kill.
        let _ = self.0.start_kill();
    }
}

#[cfg(windows)]
struct RawProcess(process::Process);

#[cfg(windows)]
impl RawProcess {
    async fn spawn(args: &[String]) -> (Self, RawIn, RawOut, RawOut) {
        let args: Vec<std::ffi::OsString> = args.iter().map(Into::into).collect();
        // The process exists only once this returns (after its last await),
        // and is owned at once.
        let spawned = process::spawn(std::path::Path::new(BAKE_RS), &args, RAW_BUDGET)
            .await
            .expect("spawn the helper");
        (
            Self(spawned.process),
            spawned.stdin,
            spawned.stdout,
            spawned.stderr,
        )
    }

    fn try_exit(&mut self) -> Option<Option<i32>> {
        self.0.try_exit().map(|exit| exit.code)
    }

    async fn wait(&mut self) -> Option<i32> {
        self.0.wait().await.expect("wait for the helper").code
    }

    fn kill(&mut self) {
        self.0.kill();
    }
}

/// The start of a raw helper's standard error, read whenever the test
/// awaits the helper.
struct RawStderr {
    pipe: Option<RawOut>,
    kept: Vec<u8>,
}

impl RawStderr {
    /// Reads one chunk; cancel-safe, and pending forever once closed.
    async fn pump(&mut self) {
        let Some(pipe) = self.pipe.as_mut() else {
            return std::future::pending().await;
        };
        let mut chunk = [0u8; 4096];
        match pipe.read(&mut chunk).await {
            Ok(count) if count > 0 => {
                let room = RAW_STDERR_KEPT.saturating_sub(self.kept.len());
                self.kept.extend_from_slice(&chunk[..count.min(room)]);
            }
            _ => self.pipe = None,
        }
    }
}

/// The production helper driven directly with raw frames, started as the
/// host starts it. Its owner from creation: a test that fails before
/// [`RawHelper::exit`] kills it on drop and waits, bounded, for its exit.
struct RawHelper {
    process: RawProcess,
    stdin: Option<RawIn>,
    stdout: RawOut,
    stderr: RawStderr,
    /// The exit code, once the exit was observed.
    exit: Option<Option<i32>>,
}

impl RawHelper {
    async fn start() -> Self {
        let args = [
            HELPER_ARG.to_owned(),
            "--generation".to_owned(),
            "1".to_owned(),
            "--memory-budget".to_owned(),
            RAW_BUDGET.to_string(),
            "--max-component".to_owned(),
            "16777216".to_owned(),
            "--call-fuel".to_owned(),
            "1000000000".to_owned(),
            "--call-timeout-ms".to_owned(),
            "10000".to_owned(),
        ];
        let (process, stdin, stdout, stderr) = RawProcess::spawn(&args).await;
        let mut helper = Self {
            process,
            stdin: Some(stdin),
            stdout,
            stderr: RawStderr {
                pipe: Some(stderr),
                kept: Vec::new(),
            },
            exit: None,
        };
        let first = helper.read().await;
        if let Some((0x81, _)) = first {
            return helper;
        }
        // No Hello: report how the helper ended and what it said. A helper
        // that closed its output is ending on its own; one that said
        // something else is killed.
        if first.is_some() {
            helper.process.kill();
        }
        let code = helper.wait().await;
        helper.drain_stderr().await;
        panic!(
            "Hello expected, got {:?}; exit code {code:?}; stderr: {}",
            first.map(|(kind, payload)| (kind, payload.len())),
            String::from_utf8_lossy(&helper.stderr.kept)
        );
    }

    async fn send(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        stdin.write_all(bytes).await.expect("write");
    }

    async fn read(&mut self) -> Option<(u8, Vec<u8>)> {
        let Self { stdout, stderr, .. } = self;
        tokio::time::timeout(Duration::from_secs(60), async {
            // Pinned outside the loop: reading stderr never cuts a frame.
            let frame = read_frame(stdout);
            tokio::pin!(frame);
            loop {
                tokio::select! {
                    frame = &mut frame => return frame,
                    () = stderr.pump() => {}
                }
            }
        })
        .await
        .expect("a frame or end of output within 60 s")
    }

    /// Waits, bounded, for the helper's exit; its code.
    async fn wait(&mut self) -> Option<i32> {
        if let Some(code) = self.exit {
            return code;
        }
        let Self {
            process, stderr, ..
        } = self;
        let code = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                tokio::select! {
                    code = process.wait() => return code,
                    () = stderr.pump() => {}
                }
            }
        })
        .await
        .expect("exit within 60 s");
        self.exit = Some(code);
        code
    }

    /// Reads standard error to its end, bounded.
    async fn drain_stderr(&mut self) {
        tokio::time::timeout(Duration::from_secs(60), async {
            while self.stderr.pipe.is_some() {
                self.stderr.pump().await;
            }
        })
        .await
        .expect("standard error closes within 60 s");
    }

    /// Reads to the helper's exit: the frame kinds seen, the last `Fatal`
    /// text, and the exit code.
    async fn exit(mut self) -> (Vec<u8>, String, Option<i32>) {
        let mut kinds = Vec::new();
        let mut fatal = String::new();
        while let Some((kind, payload)) = self.read().await {
            if kind == 0x86 {
                fatal = String::from_utf8_lossy(&payload[5..]).into_owned();
            }
            kinds.push(kind);
        }
        let code = self.wait().await;
        self.drain_stderr().await;
        (kinds, fatal, code)
    }
}

impl Drop for RawHelper {
    fn drop(&mut self) {
        if self.exit.is_some() {
            return;
        }
        // The test ended without observing the exit (it failed): kill the
        // helper and observe its exit here, bounded, so none outlives it.
        self.process.kill();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if let Some(code) = self.process.try_exit() {
                self.exit = Some(code);
                return;
            }
            std::thread::yield_now();
        }
        let message = "a raw helper did not exit within 10 s of its kill";
        if std::thread::panicking() {
            eprintln!("{message}");
        } else {
            panic!("{message}");
        }
    }
}

/// One frame, or `None` at the end of output.
async fn read_frame(stdout: &mut RawOut) -> Option<(u8, Vec<u8>)> {
    let mut header = [0u8; 5];
    if stdout.read_exact(&mut header).await.is_err() {
        return None;
    }
    let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
    // No helper reply exceeds the protocol's 256 KiB frame ceiling.
    assert!(len <= 256 * 1024, "oversized helper frame: {len} bytes");
    let mut payload = vec![0; len];
    stdout.read_exact(&mut payload).await.ok()?;
    Some((header[0], payload))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_helper_refuses_malformed_frames_before_allocating() {
    let cases: [(&str, Vec<u8>, &str); 6] = [
        (
            "oversized call",
            {
                let mut header = vec![0x02];
                header.extend_from_slice(&u32::MAX.to_le_bytes());
                header
            },
            "Call frame of 4294967295 bytes is over its",
        ),
        (
            "unknown kind",
            frame(0x7f, &[]),
            "unexpected frame kind 0x7f",
        ),
        (
            "helper-bound kind",
            frame(0x85, &[]),
            "unexpected frame kind 0x85",
        ),
        (
            "shutdown with payload",
            frame(0x03, &[0]),
            "Shutdown frame of 1 bytes",
        ),
        (
            "trailing bytes",
            {
                let mut payload = call_payload(1, GENERATION, "write");
                payload.push(0);
                frame(0x02, &payload)
            },
            "trailing bytes",
        ),
        (
            "call before load",
            frame(0x02, &call_payload(1, GENERATION, "write")),
            "without a loaded component",
        ),
    ];
    for (name, bytes, expected) in cases {
        let mut helper = RawHelper::start().await;
        helper.send(&bytes).await;
        let (kinds, fatal, code) = helper.exit().await;
        assert_eq!((kinds, code), (vec![0x86], Some(71)), "{name}");
        assert!(fatal.contains(expected), "{name}: {fatal}");
    }

    // A frame cut short by the end of input.
    let mut helper = RawHelper::start().await;
    helper.send(&[0x02, 10, 0, 0, 0, 1, 2, 3]).await;
    drop(helper.stdin.take());
    let (_, fatal, code) = helper.exit().await;
    assert_eq!(code, Some(71));
    assert!(fatal.contains("ended inside a frame"), "{fatal}");

    // End of input between frames: the parent is gone.
    let mut helper = RawHelper::start().await;
    drop(helper.stdin.take());
    assert_eq!(helper.exit().await, (Vec::new(), String::new(), Some(72)));

    // An orderly shutdown.
    let mut helper = RawHelper::start().await;
    helper.send(&frame(0x03, &[])).await;
    assert_eq!(helper.exit().await.2, Some(0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_helper_keeps_one_request_and_checks_ids_and_generations() {
    // A Call while a slow Load is still compiling.
    let mut helper = RawHelper::start().await;
    let slow = slow_component(1400, 1000);
    helper.send(&frame(0x01, &load_payload(&slow))).await;
    helper
        .send(&frame(0x02, &call_payload(1, GENERATION, "write")))
        .await;
    let (kinds, fatal, code) = helper.exit().await;
    assert_eq!(code, Some(71), "{kinds:?}");
    assert!(fatal.contains("in flight"), "{fatal}");

    let guard = guard();
    // Another generation.
    let mut helper = RawHelper::start().await;
    load_guard(&mut helper, &guard).await;
    helper
        .send(&frame(0x02, &call_payload(1, GENERATION + 1, "write")))
        .await;
    let (_, fatal, code) = helper.exit().await;
    assert_eq!(code, Some(71));
    assert!(fatal.contains("another generation"), "{fatal}");

    // An id that does not increase, and a second Load.
    let mut helper = RawHelper::start().await;
    load_guard(&mut helper, &guard).await;
    helper
        .send(&frame(0x02, &call_payload(5, GENERATION, "write")))
        .await;
    assert_eq!(helper.read().await.map(|(kind, _)| kind), Some(0x84));
    assert_eq!(helper.read().await.map(|(kind, _)| kind), Some(0x85));
    helper
        .send(&frame(0x02, &call_payload(5, GENERATION, "write")))
        .await;
    let (_, fatal, code) = helper.exit().await;
    assert_eq!(code, Some(71));
    assert!(fatal.contains("does not increase"), "{fatal}");

    let mut helper = RawHelper::start().await;
    load_guard(&mut helper, &guard).await;
    helper
        .send(&frame(0x01, &load_payload(guard.component())))
        .await;
    let (_, fatal, code) = helper.exit().await;
    assert_eq!(code, Some(71));
    assert!(fatal.contains("a second Load"), "{fatal}");
}

/// Loads the guard into a raw helper.
async fn load_guard(helper: &mut RawHelper, guard: &WasmPackage) {
    helper
        .send(&frame(0x01, &load_payload(guard.component())))
        .await;
    assert_eq!(
        helper.read().await.map(|(kind, _)| kind),
        Some(0x82),
        "Compiling"
    );
    let (kind, payload) = helper.read().await.expect("Loaded");
    assert_eq!((kind, payload[0]), (0x83, 0), "loaded");
}

/// A fake helper that writes `frames` and then waits, reading nothing:
/// the host must refuse what it says and still reap it.
#[cfg(unix)]
fn fake_helper(dir: &TempDir, frames: &[Vec<u8>]) -> HelperCommand {
    let path = dir.path().join("frames.bin");
    std::fs::write(&path, frames.concat()).expect("write frames");
    HelperCommand::new(
        "/bin/sh",
        [
            "-c".to_owned(),
            "cat \"$0\"; exec sleep 600".to_owned(),
            path.to_str().expect("utf-8").to_owned(),
        ],
    )
}

#[cfg(unix)]
fn hello_payload(generation: u64, budget: u64, build: &str) -> Vec<u8> {
    let mut bytes = 1u32.to_le_bytes().to_vec();
    string(&mut bytes, build.as_bytes());
    bytes.extend_from_slice(&generation.to_le_bytes());
    bytes.push(1);
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&budget.to_le_bytes());
    bytes.extend_from_slice(&budget.to_le_bytes());
    bytes
}

#[cfg(unix)]
fn reply_payload(id: u64, generation: u64) -> Vec<u8> {
    let mut bytes = id.to_le_bytes().to_vec();
    bytes.extend_from_slice(&generation.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_host_refuses_malformed_and_mismatched_helper_frames() {
    let limits = HostLimits {
        call_timeout: Duration::from_millis(100),
        ..limits()
    };
    let budget = limits.memory_budget;
    let build = concat!("bake ", env!("CARGO_PKG_VERSION"), " wasmtime 49.0.3");
    // A tiny component: the fake never compiles it, and its Load fits the pipe.
    let tiny = package("fake", &["write"], b"\0asm\x0d\0\x01\0".to_vec());
    let hello = frame(0x81, &hello_payload(GENERATION, budget, build));
    let loaded = frame(0x83, &[0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let started = frame(0x84, &1u64.to_le_bytes());

    let (tiny, limits) = (&tiny, &limits);
    let start_failure = |frames: Vec<Vec<u8>>| async move {
        let dir = TempDir::new();
        let result = try_start_with(&fake_helper(&dir, &frames), tiny, limits, None).await;
        match result {
            Err(StartError::Handshake { message, exit })
            | Err(StartError::Helper { message, exit }) => {
                assert!(exit.killed && exit.reaped, "{message}: {exit:?}");
                message
            }
            other => panic!("expected a refused helper, got {other:?}"),
        }
    };
    let oversized = {
        let mut header = vec![0x81];
        header.extend_from_slice(&u32::MAX.to_le_bytes());
        header
    };
    assert!(
        start_failure(vec![oversized])
            .await
            .contains("over its 1024-byte cap")
    );
    assert!(
        start_failure(vec![frame(0x85, &[])])
            .await
            .contains("Reply before Hello")
    );
    let mut trailing = hello_payload(GENERATION, budget, build);
    trailing.push(0);
    assert!(
        start_failure(vec![frame(0x81, &trailing)])
            .await
            .contains("trailing bytes")
    );
    assert!(
        start_failure(vec![frame(
            0x81,
            &hello_payload(GENERATION + 1, budget, build)
        )])
        .await
        .contains("generation")
    );
    assert!(
        start_failure(vec![frame(
            0x81,
            &hello_payload(GENERATION, budget, "bake 0.0.0")
        )])
        .await
        .contains("build")
    );
    assert!(
        start_failure(vec![frame(
            0x81,
            &hello_payload(GENERATION, budget / 2, build)
        )])
        .await
        .contains("memory limit")
    );
    assert!(
        start_failure(vec![hello.clone(), frame(0x83, &[7])])
            .await
            .contains("load result")
    );

    // Calls: replies for another request or generation, a reply before
    // Started, and silence past the deadline.
    let call_failure = |frames: Vec<Vec<u8>>| async move {
        let dir = TempDir::new();
        let host = try_start_with(&fake_helper(&dir, &frames), tiny, limits, None)
            .await
            .expect("the fake starts");
        let pid = host.pid().expect("pid");
        let error = host
            .on_tool_call(call("write", &json!({})), None)
            .await
            .expect_err("refused");
        assert!(process_gone(pid), "{error:?}");
        error
    };
    let wrong_id = call_failure(vec![
        hello.clone(),
        loaded.clone(),
        started.clone(),
        frame(0x85, &reply_payload(2, GENERATION)),
    ])
    .await;
    assert!(
        matches!(&wrong_id, CallError::Helper { message, .. } if message.contains("request 2")),
        "{wrong_id:?}"
    );
    let wrong_generation = call_failure(vec![
        hello.clone(),
        loaded.clone(),
        started.clone(),
        frame(0x85, &reply_payload(1, GENERATION + 1)),
    ])
    .await;
    assert!(
        matches!(&wrong_generation, CallError::Helper { message, .. } if message.contains("generation 2")),
        "{wrong_generation:?}"
    );
    let unstarted = call_failure(vec![
        hello.clone(),
        loaded.clone(),
        frame(0x85, &reply_payload(1, GENERATION)),
    ])
    .await;
    assert!(
        matches!(&unstarted, CallError::Helper { message, .. } if message.contains("unexpected Reply")),
        "{unstarted:?}"
    );
    let silent = call_failure(vec![hello, loaded]).await;
    assert!(
        matches!(&silent, CallError::Timeout { exit } if exit.killed && exit.reaped),
        "{silent:?}"
    );
}

/// `bake plugin check`, the product consumer: the guard blocks a
/// protected write, and an unlisted tool fails with exit 1.
#[test]
fn plugin_check_runs_the_guard_through_the_command_line() {
    let guard_dir = rust_dir().join("plugins/guard");
    let run = |args: &[&str]| {
        std::process::Command::new(BAKE_RS)
            .args(["plugin", "check"])
            .arg(&guard_dir)
            .args(args)
            .env_clear()
            .output()
            .expect("run bake")
    };
    let blocked = run(&["write", r#"{"path":"config/.env"}"#]);
    let stdout = String::from_utf8_lossy(&blocked.stdout);
    assert!(blocked.status.success(), "{blocked:?}");
    assert!(
        stdout.starts_with(
            "plugin guard 0.1.0 (bake:plugin@0.1.0), vetoes edit, write\nhelper ready: "
        ),
        "{stdout}"
    );
    assert!(stdout.ends_with("log warn: Blocked write to protected path: config/.env\nblock: Path \"config/.env\" is protected\n"), "{stdout}");
    assert!(blocked.stderr.is_empty(), "{blocked:?}");

    let unlisted = run(&["read", "{}"]);
    assert_eq!(unlisted.status.code(), Some(1), "{unlisted:?}");
    assert_eq!(
        String::from_utf8_lossy(&unlisted.stderr),
        "Error: the plugin does not veto this tool\n"
    );
}

/// A start dropped while the helper compiles leaves the helper with its
/// host: the drop sends the kill at once, the host still owns the unreaped
/// child, and `shutdown` awaits its exit and drains its pipes. On Windows,
/// which has no unreaped state, the killed helper exits before `shutdown`
/// while the host keeps its handle; the test's own handle, opened while the
/// host holds the helper, confirms that exit and its code.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_initialize_leaves_the_helper_with_its_host() {
    let slow = package("slow", &["write"], slow_component(1400, 1000));
    let limits = HostLimits {
        max_component_bytes: 16 << 20,
        memory_budget: 512 << 20,
        ..limits()
    };
    let host = new_host(&command(), &slow, &limits, CompileQueue::new(0), None).expect("host");
    assert_eq!(*host.status().borrow(), HostStatus::New);
    let mut status = host.status();
    tokio::select! {
        result = host.initialize(None) => panic!("the start finished: {result:?}"),
        () = wait_status(&mut status, |state| matches!(state, HostStatus::Compiling(_))) => {}
    }
    let pid = host.pid().expect("the host knows its helper");
    // The host holds its helper until `shutdown`, so the pid names it.
    #[cfg(windows)]
    let helper = ProcessHandle::open(pid);
    // Killed by the drop, and not reaped: the host still holds the child.
    #[cfg(target_os = "linux")]
    tokio::time::timeout(Duration::from_secs(30), async {
        while linux_state(pid) != Some('Z') {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the dropped start killed the helper");
    #[cfg(unix)]
    assert!(!process_gone(pid), "nobody but the host may reap it");
    // Killed by the drop: the Job's termination code.
    #[cfg(windows)]
    let killed = helper.exited().await;
    #[cfg(windows)]
    assert_eq!(killed, 1, "the dropped start killed the helper");
    let exit = host.shutdown().await;
    assert!(
        exit.spawned && exit.killed && exit.is_complete(),
        "{exit:?}"
    );
    #[cfg(unix)]
    assert_eq!(exit.signal, Some(libc::SIGKILL));
    // The host observed the exit the test's handle saw.
    #[cfg(windows)]
    assert_eq!((exit.code, exit.signal), (Some(killed as i32), None));
    assert!(process_gone(pid));
    assert_eq!(host.shutdown().await, exit, "a repeated shutdown is stable");
    assert_eq!(host.initialize(None).await, Err(StartError::AlreadyStarted));
    assert_eq!(*host.status().borrow(), HostStatus::Stopped);
}

/// A call cancelled mid-request starts its teardown; dropping the call
/// while that teardown waits (here, for the held pipes after the reap)
/// leaves the helper and the progress in the host. The next `shutdown`
/// continues, waits for the pipes, and reports the kill.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_call_teardown_is_continued_by_shutdown() {
    let dir = TempDir::new();
    let (held, mut holder) = held_command(&dir);
    let limits = HostLimits {
        call_timeout: Duration::from_secs(120),
        ..limits()
    };
    let host = try_start_with(&held, &hostile(), &limits, None)
        .await
        .expect("start");
    let pid = host.pid().expect("pid");
    let controller = AbortController::new();
    let signal = controller.signal();
    let mut status = host.status();
    let args = json!({});
    {
        let call_future = host.on_tool_call(call("sleep", &args), Some(&signal));
        tokio::pin!(call_future);
        tokio::select! {
            result = &mut call_future => panic!("the call finished: {result:?}"),
            () = wait_status(&mut status, |state| matches!(state, HostStatus::Calling(_))) => {}
        }
        controller.abort();
        // The cancelled call kills and reaps the helper, then waits on the
        // held pipes.
        tokio::select! {
            result = &mut call_future => panic!("the teardown finished: {result:?}"),
            () = until_gone(pid) => {}
        }
        // Dropped here, mid-drain.
    }
    assert_eq!(*host.status().borrow(), HostStatus::Stopping);
    let shutdown = host.shutdown();
    tokio::pin!(shutdown);
    tokio::select! {
        biased;
        exit = &mut shutdown => panic!("shutdown returned while the pipes are held: {exit:?}"),
        () = tokio::task::yield_now() => {}
    }
    holder.release();
    let exit = shutdown.await;
    assert_eq!(
        (exit.spawned, exit.killed, exit.reaped, exit.drained),
        (true, true, true, true),
        "{exit:?}"
    );
    assert_eq!(exit.signal, Some(libc::SIGKILL));
    assert_eq!(host.shutdown().await, exit);
    assert_eq!(*host.status().borrow(), HostStatus::Stopped);
}

/// A `shutdown` dropped while it waits for the held pipes of a helper that
/// exited by itself leaves the progress in the host: the next `shutdown`
/// finishes it and reports the helper's own exit, not a kill.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_shutdown_is_continued_by_the_next() {
    let dir = TempDir::new();
    let (held, mut holder) = held_command(&dir);
    let host = try_start_with(&held, &guard(), &limits(), None)
        .await
        .expect("start");
    let pid = host.pid().expect("pid");
    {
        let first = host.shutdown();
        tokio::pin!(first);
        tokio::select! {
            exit = &mut first => panic!("shutdown returned while the pipes are held: {exit:?}"),
            () = until_gone(pid) => {}
        }
    }
    assert_eq!(*host.status().borrow(), HostStatus::Stopping);
    let second = host.shutdown();
    tokio::pin!(second);
    tokio::select! {
        biased;
        exit = &mut second => panic!("shutdown returned while the pipes are held: {exit:?}"),
        () = tokio::task::yield_now() => {}
    }
    holder.release();
    let exit = second.await;
    assert_eq!(
        (exit.killed, exit.code, exit.reaped, exit.drained),
        (false, Some(0), true, true),
        "{exit:?}"
    );
    assert_eq!(host.shutdown().await, exit);
}

/// A teardown that runs past its bound returns an unfinished report and
/// keeps the helper: calls are refused (each continuing the teardown), and
/// a later `shutdown` finishes once the pipes close.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_teardown_past_its_bound_keeps_the_helper() {
    let dir = TempDir::new();
    let (held, mut holder) = held_command(&dir);
    let limits = HostLimits {
        teardown_timeout: Duration::from_millis(200),
        ..limits()
    };
    let host = try_start_with(&held, &guard(), &limits, None)
        .await
        .expect("start");
    let pid = host.pid().expect("pid");
    let exit = host.shutdown().await;
    assert_eq!(
        (exit.code, exit.reaped, exit.drained, exit.is_complete()),
        (Some(0), true, false, false),
        "{exit:?}"
    );
    assert!(process_gone(pid));
    assert_eq!(*host.status().borrow(), HostStatus::Stopping);
    assert_eq!(
        host.on_tool_call(call("write", &json!({ "path": "a" })), None)
            .await,
        Err(CallError::Unavailable)
    );
    assert_eq!(*host.status().borrow(), HostStatus::Stopping);
    holder.release();
    let exit = host.shutdown().await;
    assert!(exit.is_complete() && !exit.killed, "{exit:?}");
    assert_eq!(*host.status().borrow(), HostStatus::Stopped);
}

/// The host bounds what it sends before serializing: the call id's
/// length and the arguments' depth (without recursion), and the
/// arguments' length while serializing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn call_inputs_are_bounded_before_they_are_sent() {
    let host = start(&guard(), &limits()).await;
    let args = json!({ "path": "a" });
    let long_id = "i".repeat(bake_coding_agent::plugins::wasm::MAX_TOOL_CALL_ID_BYTES + 1);
    let view = bake_coding_agent::plugins::ToolCallView {
        tool_call_id: &long_id,
        tool_name: "write",
        args: &args,
    };
    assert!(matches!(
        host.on_tool_call(view, None).await,
        Err(CallError::InvalidCall(_))
    ));
    let mut deep = json!("leaf");
    for _ in 0..=bake_coding_agent::plugins::wasm::MAX_ARGS_DEPTH {
        deep = json!({ "path": deep });
    }
    assert!(matches!(
        host.on_tool_call(call("write", &deep), None).await,
        Err(CallError::InvalidCall(_))
    ));
    // The helper was asked nothing and still answers.
    assert_eq!(
        host.on_tool_call(call("write", &args), None)
            .await
            .expect("answer")
            .instance,
        1
    );
    finish(&host).await;
}

/// A package directory for `bake plugin check`.
fn check_dir(dir: &TempDir, id: &str, tools: &[&str], component: &[u8]) -> std::path::PathBuf {
    let path = dir.path().join(id);
    std::fs::create_dir(&path).expect("package dir");
    std::fs::write(path.join("plugin.wasm"), component).expect("component");
    let manifest = json!({ "api": "0.1.0", "id": id, "version": "0.0.0", "vetoTools": tools });
    std::fs::write(path.join("plugin.json"), manifest.to_string()).expect("manifest");
    path
}

/// `bake plugin check` escapes a guest's error text on standard error:
/// an error answer carrying ANSI sequences and a bell.
#[test]
fn plugin_check_escapes_guest_diagnostics() {
    let dir = TempDir::new();
    // `err("\x1b[31mred\x07")`: discriminant 1 at 0, the string at 4 and 8.
    let body = "i32.const 0\n i32.const 1\n i32.store8\n i32.const 4\n i32.const 2048\n i32.store\n i32.const 8\n i32.const 9\n i32.store";
    let component = wat_plugin(body, r#"(data (i32.const 2048) "\1b[31mred\07")"#);
    let package = check_dir(&dir, "ansi", &["write"], &component);
    let out = std::process::Command::new(BAKE_RS)
        .args(["plugin", "check"])
        .arg(&package)
        .args(["write", "{}"])
        .env_clear()
        .output()
        .expect("run bake");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        !out.stderr.contains(&0x1b) && !out.stderr.contains(&0x07),
        "{out:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "Error: the plugin failed: \\u{1b}[31mred\\u{7}\n"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("helper ready: "), "{stdout}");
}

/// Ctrl+C during `bake plugin check` cancels the call through its
/// signal: the helper is torn down, and the command exits 130 without a
/// teardown complaint.
///
/// The interrupt is sent only after the command prints `calling sleep`,
/// which it does once the helper is running the call (a host `Calling`
/// status), so the cancellation lands inside the call, not before it.
/// Every wait is bounded and names the transition it waited for, and
/// [`OwnedRun`] kills and awaits the command and its helper (through
/// pidfds) on every path, panics included. Linux only: it needs pidfds.
#[cfg(target_os = "linux")]
#[test]
fn plugin_check_handles_ctrl_c_during_a_call() {
    let dir = TempDir::new();
    let component = std::fs::read(rust_dir().join("plugins/fixtures/hostile/hostile.wasm"))
        .expect("hostile component");
    let package = check_dir(&dir, "hostile", &HOSTILE_TOOLS, &component);
    let mut run = OwnedRun::spawn(
        std::process::Command::new(BAKE_RS)
            .args(["plugin", "check"])
            .arg(&package)
            .args(["sleep", "{}"]),
    );
    run.expect_line("plugin hostile ", "the manifest line");
    let ready = run.expect_line("helper ready: ", "the helper-ready line");
    let helper: u32 = ready
        .rsplit_once("(pid ")
        .and_then(|(_, pid)| pid.trim_end_matches(')').parse().ok())
        .expect("the ready line names the helper's pid");
    // Opened while the command still holds its unreaped child, and checked
    // to be the helper, so the pidfd cannot name a reused pid.
    run.own_helper(helper);
    run.expect_line("calling sleep (request 1)", "the call-start line");
    run.signal_command(libc::SIGINT);
    let status = run.wait_command(Duration::from_secs(60), "the command's exit after SIGINT");
    assert_eq!(status.code(), Some(130), "{status:?}");
    let (rest, stderr) = run.finish_output(Duration::from_secs(60));
    assert_eq!(stderr, "Error: interrupted\n");
    assert!(rest.is_empty(), "no answer after the interrupt: {rest:?}");
    assert!(
        run.helper_exited(Duration::from_secs(60)),
        "helper {helper} outlived the command"
    );
    assert!(process_gone(helper), "helper {helper} was not reaped");
}

/// A test-owned command and, once known, its helper, each with a pidfd.
/// Its drop kills both with `SIGKILL` and waits, bounded, for both exits,
/// reaps the command, and joins the reader threads, so a failing test
/// leaves no process behind.
#[cfg(target_os = "linux")]
struct OwnedRun {
    child: std::process::Child,
    child_fd: std::os::fd::OwnedFd,
    helper_fd: Option<std::os::fd::OwnedFd>,
    lines: std::sync::mpsc::Receiver<String>,
    stdout_thread: Option<std::thread::JoinHandle<()>>,
    stderr_thread: Option<std::thread::JoinHandle<String>>,
    reaped: bool,
}

#[cfg(target_os = "linux")]
fn pidfd_open(pid: u32) -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    // SAFETY: plain syscall; a non-negative result is a new descriptor
    // this function exclusively owns.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    // SAFETY: as above.
    (fd >= 0).then(|| unsafe { std::os::fd::OwnedFd::from_raw_fd(fd as i32) })
}

/// Waits up to `timeout` for the process behind `fd` to exit.
#[cfg(target_os = "linux")]
fn pidfd_exited(fd: &std::os::fd::OwnedFd, timeout: Duration) -> bool {
    use std::os::fd::AsRawFd;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut poll, 1, left.as_millis().min(60_000) as i32) };
        if ready > 0 {
            return true;
        }
        if ready == 0 && left.is_zero() {
            return false;
        }
        if ready == 0 {
            continue;
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return false;
        }
    }
}

#[cfg(target_os = "linux")]
fn pidfd_kill(fd: &std::os::fd::OwnedFd, signal: i32) {
    use std::os::fd::AsRawFd;
    // SAFETY: plain syscall on a pidfd this test owns; an exited process
    // makes it fail harmlessly.
    unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        );
    }
}

#[cfg(target_os = "linux")]
impl OwnedRun {
    fn spawn(command: &mut std::process::Command) -> Self {
        use std::io::{BufRead, Read};
        let mut child = command
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("run bake");
        let child_fd = pidfd_open(child.id()).expect("a pidfd for our own child");
        let stdout = child.stdout.take().expect("stdout");
        let mut stderr = child.stderr.take().expect("stderr");
        let (sender, lines) = std::sync::mpsc::channel();
        let stdout_thread = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr_thread = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });
        Self {
            child,
            child_fd,
            helper_fd: None,
            lines,
            stdout_thread: Some(stdout_thread),
            stderr_thread: Some(stderr_thread),
            reaped: false,
        }
    }

    /// The next standard-output line, which must start with `prefix`,
    /// within 60 s.
    fn expect_line(&mut self, prefix: &str, transition: &str) -> String {
        match self.lines.recv_timeout(Duration::from_secs(60)) {
            Ok(line) if line.starts_with(prefix) => line,
            Ok(line) => panic!("expected {transition} ({prefix:?}), got {line:?}"),
            Err(error) => panic!("{transition}: none within 60 s ({error})"),
        }
    }

    fn own_helper(&mut self, pid: u32) {
        let fd = pidfd_open(pid).expect("a pidfd for the helper");
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).expect("the helper's cmdline");
        assert!(
            cmdline
                .split(|byte| *byte == 0)
                .any(|arg| arg == HELPER_ARG.as_bytes()),
            "pid {pid} is not the helper"
        );
        self.helper_fd = Some(fd);
    }

    fn signal_command(&self, signal: i32) {
        pidfd_kill(&self.child_fd, signal);
    }

    fn wait_command(&mut self, timeout: Duration, transition: &str) -> std::process::ExitStatus {
        assert!(
            pidfd_exited(&self.child_fd, timeout),
            "{transition}: none within {timeout:?}"
        );
        self.reaped = true;
        self.child.wait().expect("reap the exited command")
    }

    /// The standard-output lines left and all of standard error, once both
    /// pipes close.
    fn finish_output(&mut self, timeout: Duration) -> (Vec<String>, String) {
        let deadline = std::time::Instant::now() + timeout;
        let mut rest = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => rest.push(line),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    panic!("standard output did not close within {timeout:?}")
                }
            }
        }
        if let Some(thread) = self.stdout_thread.take() {
            thread.join().expect("stdout reader");
        }
        let stderr = self
            .stderr_thread
            .take()
            .map(|thread| thread.join().expect("stderr reader"))
            .unwrap_or_default();
        (rest, stderr)
    }

    fn helper_exited(&self, timeout: Duration) -> bool {
        self.helper_fd
            .as_ref()
            .is_some_and(|fd| pidfd_exited(fd, timeout))
    }
}

#[cfg(target_os = "linux")]
impl Drop for OwnedRun {
    fn drop(&mut self) {
        let mut stranded = Vec::new();
        if !self.reaped {
            pidfd_kill(&self.child_fd, libc::SIGKILL);
            if pidfd_exited(&self.child_fd, Duration::from_secs(10)) {
                let _ = self.child.wait();
            } else {
                stranded.push(format!("command {}", self.child.id()));
            }
        }
        if let Some(fd) = &self.helper_fd {
            pidfd_kill(fd, libc::SIGKILL);
            if !pidfd_exited(fd, Duration::from_secs(10)) {
                stranded.push("helper".to_owned());
            }
        }
        // The pipes close once both processes are gone; the readers end.
        if stranded.is_empty() {
            if let Some(thread) = self.stdout_thread.take() {
                let _ = thread.join();
            }
            if let Some(thread) = self.stderr_thread.take() {
                let _ = thread.join();
            }
        }
        if !stranded.is_empty() && !std::thread::panicking() {
            panic!("OwnedRun could not stop: {stranded:?}");
        }
        if !stranded.is_empty() {
            eprintln!("OwnedRun could not stop: {stranded:?}");
        }
    }
}

/// A start dropped while its helper compiles keeps its compile slot: the
/// killed helper is still the host's, unreaped, and another host on the
/// same queue (which admits no waiters) is refused until `shutdown` has
/// observed the first helper's exit. No timing is involved: the first
/// helper's exit is not observed by anyone until that `shutdown`. On
/// Windows the killed helper exits before `shutdown`; the test's own handle
/// waits for that exit before asking for the slot, so the refusal shows the
/// slot follows the host's observation, not the process's end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_compile_keeps_its_slot_until_its_exit_is_observed() {
    let slow = package("slow", &["write"], slow_component(1400, 1000));
    let limits = HostLimits {
        max_component_bytes: 16 << 20,
        memory_budget: 512 << 20,
        ..limits()
    };
    let queue = CompileQueue::new(0);
    let first = new_host(&command(), &slow, &limits, queue.clone(), None).expect("first host");
    let mut status = first.status();
    tokio::select! {
        result = first.initialize(None) => panic!("the start finished: {result:?}"),
        () = wait_status(&mut status, |state| matches!(state, HostStatus::Compiling(_))) => {}
    }
    let pid = first.pid().expect("pid");
    // The host holds its helper, and the slot with it, until `shutdown`;
    // the refusal below proves it still did after this handle was opened.
    #[cfg(windows)]
    let helper = ProcessHandle::open(pid);
    #[cfg(unix)]
    assert!(!process_gone(pid), "only the host may reap its helper");
    #[cfg(windows)]
    let killed = helper.exited().await;
    #[cfg(windows)]
    assert_eq!(killed, 1, "the dropped start killed the helper");
    let second = new_host(&command(), &guard(), &limits, queue.clone(), None).expect("second host");
    assert_eq!(
        second.initialize(None).await,
        Err(StartError::QueueFull),
        "the dropped compile still holds the slot"
    );
    assert_eq!(*second.status().borrow(), HostStatus::New);
    let exit = first.shutdown().await;
    assert!(exit.killed && exit.is_complete(), "{exit:?}");
    #[cfg(windows)]
    assert_eq!((exit.code, exit.signal), (Some(killed as i32), None));
    assert!(process_gone(pid));
    // The observed exit released the slot.
    second
        .initialize(None)
        .await
        .expect("admitted once the exit is observed");
    finish(&second).await;
}

/// The same, for a cancelled start whose teardown runs past its bound
/// with the pipes held open: once the exit is observed the slot is free,
/// even though the teardown, and the host's ownership, continue.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reaped_but_undrained_helper_frees_its_compile_slot() {
    let dir = TempDir::new();
    let (held, mut holder) = held_command(&dir);
    let slow = package("slow", &["write"], slow_component(1400, 1000));
    let limits = HostLimits {
        max_component_bytes: 16 << 20,
        memory_budget: 512 << 20,
        teardown_timeout: Duration::from_millis(200),
        ..limits()
    };
    let queue = CompileQueue::new(0);
    let first = new_host(&held, &slow, &limits, queue.clone(), None).expect("first host");
    let mut status = first.status();
    tokio::select! {
        result = first.initialize(None) => panic!("the start finished: {result:?}"),
        () = wait_status(&mut status, |state| matches!(state, HostStatus::Compiling(_))) => {}
    }
    let exit = first.shutdown().await;
    assert_eq!(
        (exit.reaped, exit.drained, exit.is_complete()),
        (true, false, false),
        "{exit:?}"
    );
    let second = new_host(
        &command(),
        &guard(),
        &fixtures::limits(),
        queue.clone(),
        None,
    )
    .expect("second");
    second
        .initialize(None)
        .await
        .expect("a reaped helper holds no slot");
    finish(&second).await;
    holder.release();
    assert!(first.shutdown().await.is_complete());
}
