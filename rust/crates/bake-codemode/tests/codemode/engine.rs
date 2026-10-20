//! The engine's own cases: evaluation, exceptions, loops and built-ins
//! stopped by the deadline and by an abort, memory limits, recursion, and the
//! run's thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bake_codemode::{
    Event, ExecuteRequest, ExitHook, Failure, Limits, MAX_OUTPUT_CHARS, MAX_PENDING_ARGUMENT_BYTES,
    MAX_PENDING_CALLS, OutputItem, ToolDeclaration,
};
use serde_json::json;

use crate::harness::{BOUND, Setup, echo, engine, execute, run};

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
    // The first run in a process compiles the engine; that is not the run's time.
    engine();
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

/// The watchdog stops a run at its deadline while the host awaits something
/// other than `Run::next`, here inside three long built-ins that neither poll
/// QuickJS's interrupt handler nor return to the driver (about 3.5 s
/// without the watchdog's epoch increment).
#[tokio::test]
async fn stops_at_the_deadline_while_nothing_awaits_next() {
    let exits = Arc::new(AtomicUsize::new(0));
    let mut request = counting_request(
        "tools.echo(); const a = new Array(1e7).fill(1); return JSON.stringify(a).length + JSON.stringify(a).length + JSON.stringify(a).length",
        &exits,
    );
    request.tools = vec![ToolDeclaration::named("echo")];
    request.source.options.timeout_ms = Some(300);
    engine();
    let started = Instant::now();
    let mut run = match engine().start(request) {
        Ok(run) => run,
        Err(error) => panic!("{error}"),
    };
    while exits.load(Ordering::SeqCst) == 0 {
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "the script ran past its deadline"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let call = tokio::time::timeout(BOUND, run.next()).await;
    assert!(matches!(call, Ok(Event::Call(_))), "{call:?}");
    let event = tokio::time::timeout(BOUND, run.next()).await;
    let Ok(Event::Done(outcome)) = event else {
        panic!("{event:?}");
    };
    assert_eq!(
        outcome.result,
        Err(Failure::Timeout {
            message: "Execution timed out after 300 ms".to_owned()
        })
    );
}

/// `abort_with` reports its message, from the run or a cloned handle; only
/// the first stop's message counts.
#[tokio::test]
async fn abort_with_reports_its_message() {
    let run = crate::harness::start("while (true) {}", &Setup::default());
    let mut run = run;
    let handle = run.handle();
    tokio::time::sleep(Duration::from_millis(20)).await;
    handle.abort_with("user cancelled");
    run.abort_with("too late");
    run.abort();
    let event = tokio::time::timeout(BOUND, run.next()).await;
    let Ok(Event::Done(outcome)) = event else {
        panic!("{event:?}");
    };
    assert_eq!(
        outcome.result,
        Err(Failure::Aborted {
            message: "user cancelled".to_owned()
        })
    );
    // The outcome is reported again as the same shared value.
    let again = tokio::time::timeout(BOUND, run.next()).await;
    let Ok(Event::Done(again)) = again else {
        panic!("{again:?}");
    };
    assert!(Arc::ptr_eq(&outcome, &again));
    // A handle outliving its run answers nothing and does not panic.
    drop(run);
    assert_eq!(handle.answer(1, Ok(None)), Ok(()));
}

/// QuickJS escapes a lone surrogate in `JSON.stringify`, as JavaScript
/// requires; it reaches the host as U+FFFD instead of breaking the bridge,
/// in a return value, a call's arguments, and stored values and keys.
#[tokio::test]
async fn lone_surrogates_cross_the_bridge_as_replacement_characters() {
    let execution = run(r#"return "\ud83d""#).await;
    assert_eq!(execution.value(), Some(&json!("\u{fffd}")));

    let execution = execute(
        r#"const half = "😀".slice(0, 1); const echoed = await tools.echo({ s: half, low: "\udc00x", pair: "\ud83d\ude00" }); return [echoed.s, echoed.s === half]"#,
        Setup::tools(vec![echo()]),
    )
    .await;
    assert_eq!(execution.value(), Some(&json!(["\u{fffd}", false])));

    let execution =
        run(r#"store("k", "a\ud800"); store("\udfffkey", ["\ud83d\ude00", "\\ud800"]); return 1"#)
            .await;
    let writes = &execution.completion().store_writes;
    assert_eq!(writes.set.get("k"), Some(&json!("a\u{fffd}")));
    // A real pair is kept, and an escaped backslash before "ud800" is text.
    assert_eq!(
        writes.set.get("\u{fffd}key"),
        Some(&json!(["😀", "\\ud800"]))
    );
}

/// A lone surrogate is one UTF-16 unit for the prelude's output count and one
/// U+FFFD for the host's recount, so output the prelude accepts passes the
/// recount though it is three bytes per unit inside the VM.
#[tokio::test]
async fn the_output_recount_counts_a_lone_surrogate_once() {
    let execution = run(r#"text("\ud800".repeat(6e6)); return 1"#).await;
    assert_eq!(execution.value(), Some(&json!(1)));
    let texts = execution.texts();
    assert_eq!(texts.len(), 1);
    assert_eq!(texts[0].chars().count(), 6_000_000);
    assert!(texts[0].chars().all(|unit| unit == '\u{fffd}'));
    const { assert!(6_000_000 * 3 > MAX_OUTPUT_CHARS) };
}

/// Deeply nested data stops at QuickJS's stack guard with a catchable
/// `RangeError` in built-ins that recurse natively, rather than trapping on
/// the Wasm stack limit.
#[tokio::test]
async fn deep_nesting_in_built_ins_is_a_catchable_range_error() {
    for code in [
        r#"JSON.parse("[".repeat(1e5) + "]".repeat(1e5))"#,
        r#"JSON.parse('{"a":'.repeat(1e5) + "1" + "}".repeat(1e5))"#,
        "let o = []; for (let i = 0; i < 1e5; i++) o = [o]; JSON.stringify(o)",
        "let o = {}; for (let i = 0; i < 1e5; i++) o = { o }; JSON.stringify(o)",
        "let o = []; for (let i = 0; i < 1e5; i++) o = [o]; String(o)",
        r#"eval("[".repeat(1e5) + "]".repeat(1e5))"#,
    ] {
        let execution = run(&format!(
            "try {{ {code}; return 'completed'; }} catch (error) {{ return String(error); }}"
        ))
        .await;
        assert_eq!(
            execution.value(),
            Some(&json!("RangeError: Maximum call stack size exceeded")),
            "{code}"
        );
    }
}

#[tokio::test]
async fn an_oversized_string_is_a_catchable_error() {
    let execution =
        run("try { return 'x'.repeat(2 ** 30).length; } catch (error) { return String(error); }")
            .await;
    assert_eq!(
        execution.value(),
        Some(&json!("RangeError: invalid string length"))
    );
}

fn pending_limit_message() -> String {
    format!(
        "The script has {MAX_PENDING_CALLS} calls waiting for results, the most a run allows; await calls before making more, or batch them"
    )
}

fn pending_bytes_message() -> String {
    format!(
        "The arguments of the script's calls waiting for results would exceed {MAX_PENDING_ARGUMENT_BYTES} bytes, the most a run allows; await calls before making more, or batch them"
    )
}

/// A script that calls in a loop without awaiting fails at the pending-call
/// limit instead of filling the heap with calls the VM never settles, with a
/// deadline or without one.
#[tokio::test]
async fn calls_in_a_loop_without_awaiting_stop_at_the_pending_call_limit() {
    // Filling the queue must hit its limit before a deadline competes with
    // it under suite load. The completion bound below remains five seconds.
    for timeout_ms in [Some(30_000), None] {
        let started = Instant::now();
        let execution = execute(
            "for (;;) tools.echo({ a: 'y'.repeat(1000) })",
            Setup {
                tools: vec![echo()],
                timeout_ms,
                ..Setup::default()
            },
        )
        .await;
        assert_eq!(
            execution.failure(),
            &Failure::Protocol {
                message: pending_limit_message()
            },
            "{timeout_ms:?}"
        );
        assert_eq!(execution.calls.len(), MAX_PENDING_CALLS, "{timeout_ms:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "{timeout_ms:?}");
    }
}

/// Up to the limit, calls may wait together; answered calls no longer count.
#[tokio::test]
async fn the_pending_call_limit_counts_only_calls_that_wait() {
    let execution = execute(
        &format!(
            "let total = 0; for (let round = 0; round < 3; round++) {{ const results = await Promise.all(Array.from({{ length: {MAX_PENDING_CALLS} }}, (_, i) => tools.echo(i))); total += results.length; }} return total"
        ),
        Setup::tools(vec![echo()]),
    )
    .await;
    assert_eq!(execution.value(), Some(&json!(3 * MAX_PENDING_CALLS)));
    let execution = execute(
        &format!(
            "await Promise.all(Array.from({{ length: {} }}, (_, i) => tools.echo(i)))",
            MAX_PENDING_CALLS + 1
        ),
        Setup::tools(vec![echo()]),
    )
    .await;
    assert_eq!(
        execution.failure(),
        &Failure::Protocol {
            message: pending_limit_message()
        }
    );
}

/// A fan-out of a thousand calls fits the limit.
#[tokio::test]
async fn a_promise_all_of_a_thousand_calls_succeeds() {
    let execution = execute(
        "return (await Promise.all(Array.from({ length: 1000 }, (_, i) => tools.echo(i)))).length",
        Setup::tools(vec![echo()]),
    )
    .await;
    assert_eq!(execution.value(), Some(&json!(1000)));
    assert_eq!(MAX_PENDING_CALLS, 1024);
}

/// Waiting calls' arguments are bounded too: one call over the budget, or
/// several that pass it together, end the run before the host copies them.
#[tokio::test]
async fn waiting_calls_arguments_stop_at_the_byte_budget() {
    for code in [
        "await tools.echo('x'.repeat(70 * 2 ** 20))",
        "const big = 'x'.repeat(14 * 2 ** 20); await Promise.all([1, 2, 3, 4, 5].map(() => tools.echo(big)))",
    ] {
        let execution = execute(code, Setup::tools(vec![echo()])).await;
        assert_eq!(
            execution.failure(),
            &Failure::Protocol {
                message: pending_bytes_message()
            },
            "{code}"
        );
    }
    // Answered calls give their bytes back.
    let execution = execute(
        "const big = 'x'.repeat(14 * 2 ** 20); for (let i = 0; i < 6; i++) await tools.echo(big); return 6",
        Setup::tools(vec![echo()]),
    )
    .await;
    assert_eq!(execution.value(), Some(&json!(6)));
}

/// A script that keeps allocating at the heap limit, catching each
/// out-of-memory error, spends its time in QuickJS's allocator and garbage
/// collector; an abort still stops it within a second.
#[tokio::test]
async fn aborts_a_script_at_the_heap_limit() {
    let mut run = crate::harness::start(
        "const keep = []; for (;;) { try { keep.push('x'.repeat(1 << 16) + keep.length); } catch {} }",
        &Setup {
            limits: Some(Limits {
                heap_bytes: Some(32 << 20),
                ..Limits::default()
            }),
            ..Setup::default()
        },
    );
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let aborted = Instant::now();
    run.abort();
    let event = tokio::time::timeout(BOUND, run.next()).await;
    let Ok(Event::Done(outcome)) = event else {
        panic!("{event:?}");
    };
    assert!(
        matches!(outcome.result, Err(Failure::Aborted { .. })),
        "{outcome:?}"
    );
    assert!(
        aborted.elapsed() < Duration::from_secs(1),
        "{:?}",
        aborted.elapsed()
    );
}
