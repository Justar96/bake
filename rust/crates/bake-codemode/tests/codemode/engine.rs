//! The engine's own cases: evaluation, exceptions, loops and built-ins
//! stopped by the deadline and by an abort, memory limits, recursion, and the
//! run's thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bake_codemode::{Event, ExecuteRequest, ExitHook, Failure, Limits, OutputItem};
use serde_json::json;

use crate::harness::{BOUND, Setup, engine, execute, run};

#[test]
fn runs_are_send() {
    fn send<T: Send>() {}
    send::<bake_codemode::Run>();
}

#[test]
fn compiles_once_and_reports_the_compile_time() {
    let first = engine();
    let again = engine();
    assert!(std::ptr::eq(first, again));
    eprintln!(
        "bake-codemode engine compile time: {:?}",
        first.compile_time()
    );
}

#[tokio::test]
async fn evaluates_a_script() {
    let execution = run("return 1 + 2").await;
    assert_eq!(execution.value(), Some(&json!(3)));
}

#[tokio::test]
async fn reports_an_exception() {
    let execution = run("throw new Error('x')").await;
    let error = execution.exception();
    assert_eq!(error.name.as_deref(), Some("Error"));
    assert_eq!(error.message, "x");
}

async fn timed(code: &str, timeout_ms: u64) -> (Failure, Duration) {
    let started = Instant::now();
    let execution = execute(
        code,
        Setup {
            timeout_ms: Some(timeout_ms),
            ..Setup::default()
        },
    )
    .await;
    (execution.failure().clone(), started.elapsed())
}

#[tokio::test]
async fn runs_ten_million_loop_iterations() {
    let execution = run("let n = 0; for (let i = 0; i < 1e7; i++) n += i; return n").await;
    assert_eq!(execution.value(), Some(&json!(49_999_995_000_000u64)));
}

#[tokio::test]
async fn stops_an_infinite_loop_at_the_deadline() {
    let (failure, elapsed) = timed("while (true) {}", 300).await;
    assert_eq!(
        failure,
        Failure::Timeout {
            message: "Execution timed out after 300 ms".to_owned()
        }
    );
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
}

/// Backtracking inside a built-in runs no script opcodes, so only the epoch
/// trap (or QuickJS's regex interrupt check) stops it.
#[tokio::test]
async fn stops_regex_backtracking_at_the_deadline() {
    let (failure, elapsed) = timed("return /(a+)+b/.test('a'.repeat(40))", 300).await;
    assert!(matches!(failure, Failure::Timeout { .. }), "{failure:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
}

/// One `JSON.stringify` of ten million elements runs for about a second
/// without a script opcode, so QuickJS's interrupt poll would let it finish;
/// the epoch trap stops it at the deadline.
#[tokio::test]
async fn stops_a_long_built_in_at_the_deadline() {
    let (failure, elapsed) = timed(
        "const a = new Array(1e7).fill(1); return JSON.stringify(a).length + JSON.stringify(a).length",
        300,
    )
    .await;
    assert!(matches!(failure, Failure::Timeout { .. }), "{failure:?}");
    assert!(elapsed < Duration::from_millis(1500), "{elapsed:?}");
}

#[tokio::test]
async fn aborts_a_spinning_script() {
    for code in [
        "while (true) {}",
        "while (true) await null",
        "return /(a+)+b/.test('a'.repeat(40))",
    ] {
        let mut run = crate::harness::start(code, &Setup::default());
        let started = Instant::now();
        tokio::time::sleep(Duration::from_millis(100)).await;
        run.abort();
        let event = tokio::time::timeout(BOUND, run.next()).await;
        let Ok(Event::Done(outcome)) = event else {
            panic!("{code}: {event:?}");
        };
        assert_eq!(
            outcome.result,
            Err(Failure::Aborted {
                message: "Execution aborted".to_owned()
            }),
            "{code}"
        );
        assert!(started.elapsed() < Duration::from_secs(5), "{code}");
    }
}

#[tokio::test]
async fn heap_exhaustion_is_a_catchable_out_of_memory_error() {
    let execution = execute(
        "const keep = []; try { for (;;) keep.push('x'.repeat(1 << 20) + keep.length); } catch (error) { keep.length = 0; return String(error); }",
        Setup {
            limits: Some(Limits {
                heap_bytes: Some(16 << 20),
                ..Limits::default()
            }),
            ..Setup::default()
        },
    )
    .await;
    assert_eq!(
        execution.value(),
        Some(&json!("InternalError: out of memory"))
    );
}

/// With no heap limit, the linear-memory limit ends the run, and the process
/// and the engine keep working.
#[tokio::test]
async fn linear_memory_exhaustion_ends_the_run() {
    let execution = execute(
        "const keep = []; for (;;) keep.push('x'.repeat(1 << 20) + keep.length);",
        Setup {
            limits: Some(Limits {
                heap_bytes: None,
                linear_memory_bytes: 48 << 20,
            }),
            ..Setup::default()
        },
    )
    .await;
    assert!(
        matches!(execution.failure(), Failure::OutOfMemory { .. }),
        "{:?}",
        execution.failure()
    );
    assert_eq!(
        run("return 'still working'").await.value(),
        Some(&json!("still working"))
    );
}

#[tokio::test]
async fn deep_recursion_is_a_catchable_range_error() {
    let execution = run(
        "let depth = 0; function dive() { depth++; dive(); } try { dive(); } catch (error) { return [error.name, depth > 1000]; }",
    )
    .await;
    assert_eq!(execution.value(), Some(&json!(["RangeError", true])));
}

#[tokio::test]
async fn reports_output_in_order() {
    let execution = run("console.log('a'); text('b'); return 1").await;
    assert_eq!(
        execution.output,
        [
            OutputItem::Text {
                text: "a".to_owned(),
                console: true
            },
            OutputItem::Text {
                text: "b".to_owned(),
                console: false
            },
        ]
    );
}

fn counting_request(code: &str, exits: &Arc<AtomicUsize>) -> ExecuteRequest {
    let mut request = ExecuteRequest::new(code);
    let exits = Arc::clone(exits);
    request.on_exit = Some(ExitHook::new(move || {
        exits.fetch_add(1, Ordering::SeqCst);
    }));
    request
}

/// Outside a runtime, dropping a run joins its thread before it returns.
#[test]
fn dropping_a_run_joins_its_thread() {
    let exits = Arc::new(AtomicUsize::new(0));
    let run = match engine().start(counting_request("while (true) {}", &exits)) {
        Ok(run) => run,
        Err(error) => panic!("{error}"),
    };
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(exits.load(Ordering::SeqCst), 0);
    let started = Instant::now();
    drop(run);
    assert_eq!(
        exits.load(Ordering::SeqCst),
        1,
        "the thread exited before drop returned"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// On a runtime, the join moves to a blocking task, and the thread still ends.
#[tokio::test]
async fn dropping_a_run_on_a_runtime_ends_its_thread() {
    let exits = Arc::new(AtomicUsize::new(0));
    let run = match engine().start(counting_request("while (true) await null", &exits)) {
        Ok(run) => run,
        Err(error) => panic!("{error}"),
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(run);
    let deadline = Instant::now() + Duration::from_secs(5);
    while exits.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "the script thread outlived its run"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(exits.load(Ordering::SeqCst), 1);
}
