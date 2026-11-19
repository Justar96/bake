//! The engine's own cases: evaluation, exceptions, loops and built-ins
//! stopped by the deadline and by an abort, memory limits, recursion, and the
//! run's thread.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bake_codemode::{
    Deterministic, Event, ExecuteRequest, ExitHook, Failure, HostCall, Limits, MAX_OUTPUT_CHARS,
    MAX_PENDING_ARGUMENT_BYTES, MAX_PENDING_CALLS, OutputItem, Run, ToolDeclaration,
};
use serde_json::json;

use crate::harness::{BOUND, Setup, async_tool, echo, engine, execute, run};

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
    let (failure, _) = timed("while (true) {}", 300).await;
    assert_eq!(
        failure,
        Failure::Timeout {
            message: "Execution timed out after 300 ms".to_owned()
        }
    );
}

/// Backtracking inside a built-in runs no script opcodes, so only the epoch
/// trap (or QuickJS's regex interrupt check) stops it.
#[tokio::test]
async fn stops_regex_backtracking_at_the_deadline() {
    let (failure, _) = timed("return /(a+)+b/.test('a'.repeat(40))", 300).await;
    assert!(matches!(failure, Failure::Timeout { .. }), "{failure:?}");
}

/// Eight `JSON.stringify` calls of ten million elements, which run for
/// about a second each with too few script opcodes between them for
/// QuickJS's interrupt poll; the epoch trap stops them at the deadline. The
/// bound is the property: well below the built-ins' uninterrupted runtime.
#[tokio::test]
async fn stops_a_long_built_in_at_the_deadline() {
    let (failure, elapsed) = timed(LONG_BUILT_INS, 300).await;
    assert!(matches!(failure, Failure::Timeout { .. }), "{failure:?}");
    assert!(elapsed < Duration::from_secs(4), "{elapsed:?}");
}

const LONG_BUILT_INS: &str = "const a = new Array(1e7).fill(1); let n = 0; for (let i = 0; i < 8; i++) n += JSON.stringify(a).length; return n";

/// Starts `code` after a `text("running")`, and returns once the script
/// reported it: the VM is running the rest.
async fn start_running(code: &str, setup: &Setup) -> Run {
    let mut run = crate::harness::start(&format!("text(\"running\"); {code}"), setup);
    let event = tokio::time::timeout(BOUND, run.next()).await;
    assert!(
        matches!(&event, Ok(Event::Output(OutputItem::Text { text, .. })) if text == "running"),
        "{code}: {event:?}"
    );
    run
}

#[tokio::test]
async fn aborts_a_spinning_script() {
    for code in [
        "while (true) {}",
        "while (true) await null",
        "return /(a+)+b/.test('a'.repeat(40))",
    ] {
        let mut run = start_running(code, &Setup::default()).await;
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
                ..Limits::default()
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
    let mut run = match engine().start(counting_request("text('running'); while (true) {}", &exits))
    {
        Ok(run) => run,
        Err(error) => panic!("{error}"),
    };
    // A runtime only to await the first event; the drop happens outside it.
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => panic!("{error}"),
    };
    let event = runtime.block_on(async { tokio::time::timeout(BOUND, run.next()).await });
    assert!(matches!(event, Ok(Event::Output(_))), "{event:?}");
    drop(runtime);
    assert_eq!(exits.load(Ordering::SeqCst), 0);
    drop(run);
    assert_eq!(
        exits.load(Ordering::SeqCst),
        1,
        "the thread exited before drop returned"
    );
}

/// On a runtime, the join moves to a blocking task, and the thread still ends.
#[tokio::test]
async fn dropping_a_run_on_a_runtime_ends_its_thread() {
    let exits = Arc::new(AtomicUsize::new(0));
    let mut run = match engine().start(counting_request(
        "text('running'); while (true) await null",
        &exits,
    )) {
        Ok(run) => run,
        Err(error) => panic!("{error}"),
    };
    let event = tokio::time::timeout(BOUND, run.next()).await;
    assert!(matches!(event, Ok(Event::Output(_))), "{event:?}");
    drop(run);
    let deadline = Instant::now() + BOUND;
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
/// other than `Run::next`, here inside long built-ins that neither poll
/// QuickJS's interrupt handler nor return to the driver (about 8 s without
/// the watchdog's epoch increment). The deadline allows the script to start
/// under contention; the exit bound, the property, remains well below the
/// built-ins' uninterrupted runtime.
#[tokio::test]
async fn stops_at_the_deadline_while_nothing_awaits_next() {
    let exits = Arc::new(AtomicUsize::new(0));
    let mut request = counting_request(&format!("tools.echo(); {LONG_BUILT_INS}"), &exits);
    request.tools = vec![ToolDeclaration::named("echo")];
    request.source.options.timeout_ms = Some(1000);
    engine();
    let started = Instant::now();
    let mut run = match engine().start(request) {
        Ok(run) => run,
        Err(error) => panic!("{error}"),
    };
    while exits.load(Ordering::SeqCst) == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(5),
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
            message: "Execution timed out after 1000 ms".to_owned()
        })
    );
}

/// `abort_with` reports its message, from the run or a cloned handle; only
/// the first stop's message counts.
#[tokio::test]
async fn abort_with_reports_its_message() {
    let mut run = start_running("while (true) {}", &Setup::default()).await;
    let handle = run.handle();
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

fn pending_bytes_message() -> String {
    format!(
        "The script's calls waiting for results would take more than {} MiB, the most a run allows; await calls before making more, or batch them",
        MAX_PENDING_ARGUMENT_BYTES >> 20
    )
}

/// A script that calls in a loop without awaiting ends at the byte budget
/// instead of filling the heap with calls the VM never settles, with a
/// deadline or without one, and with or without arguments; the host is
/// given only the window's calls.
#[tokio::test]
async fn calls_in_a_loop_without_awaiting_end_at_the_byte_budget() {
    let cases = [
        ("for (;;) tools.echo({ a: 'y'.repeat(1000) })", Some(60_000)),
        ("for (;;) tools.echo({ a: 'y'.repeat(1000) })", None),
        // About 260,000 calls: the busy budget must not end it first on a
        // loaded host.
        ("for (;;) tools.echo()", None),
    ];
    for (code, timeout_ms) in cases {
        let execution = execute(
            code,
            Setup {
                tools: vec![echo()],
                timeout_ms,
                limits: Some(Limits {
                    busy: Duration::from_secs(600),
                    ..Limits::default()
                }),
                ..Setup::default()
            },
        )
        .await;
        assert_eq!(
            execution.failure(),
            &Failure::Protocol {
                message: pending_bytes_message()
            },
            "{code} {timeout_ms:?}"
        );
        assert_eq!(execution.calls.len(), MAX_PENDING_CALLS, "{code}");
    }
}

/// A script that ends while calls past the window are still held reports
/// how many, on success and on failure: the host was given only the
/// window's calls, and the outcome counts the rest.
#[tokio::test]
async fn the_outcome_counts_the_calls_still_held_when_the_script_ends() {
    let never = || async_tool("wait", |_, _| std::future::pending());
    for (code, completes) in [
        (
            "for (let i = 0; i < 2000; i++) tools.wait();\nreturn 'ok';",
            true,
        ),
        (
            "for (let i = 0; i < 2000; i++) tools.wait();\nthrow new Error('x');",
            false,
        ),
    ] {
        let execution = execute(code, Setup::tools(vec![never()])).await;
        assert_eq!(execution.outcome.result.is_ok(), completes, "{code}");
        assert_eq!(execution.calls.len(), MAX_PENDING_CALLS, "{code}");
        assert_eq!(execution.outcome.held, 2000 - MAX_PENDING_CALLS, "{code}");
    }
    // Calls that all reached the host leave none held.
    let execution = execute(
        "await Promise.all([tools.echo(), tools.echo()]);",
        Setup::tools(vec![echo()]),
    )
    .await;
    assert_eq!(execution.outcome.held, 0);
}

/// Calls past the window are held, in order, and given to the host one by
/// one as earlier calls' answers are applied. Deterministic mode makes the
/// VM's waits observable: `Idle` reports the calls given to the host.
#[tokio::test]
async fn calls_past_the_window_wait_in_order_until_earlier_ones_settle() {
    let total = MAX_PENDING_CALLS + 500;
    let mut request = ExecuteRequest::new(format!(
        "return (await Promise.all(Array.from({{ length: {total} }}, (_, i) => tools.echo(i)))).reduce((a, b) => a + b, 0)"
    ));
    request.tools = vec![ToolDeclaration::named("echo")];
    request.deterministic = Some(Deterministic {
        started_at_ms: 0.0,
        seed: 0,
    });
    let mut run = engine().start(request).expect("the fixture starts");
    let mut waiting: VecDeque<(u32, serde_json::Value)> = VecDeque::new();
    let mut next_id = 1;
    let mut seq = 0;
    let outcome = tokio::time::timeout(BOUND, async {
        loop {
            match run.next().await {
                Event::Call(HostCall::Tool { id, args, .. }) => {
                    assert_eq!(id, next_id, "calls are given to the host in order");
                    next_id += 1;
                    waiting.push_back((id, args.unwrap_or_default()));
                }
                Event::Idle { issued, .. } => {
                    assert_eq!(issued as usize, waiting.len() + seq as usize);
                    assert!(waiting.len() <= MAX_PENDING_CALLS, "{}", waiting.len());
                    if seq == 0 {
                        assert_eq!(waiting.len(), MAX_PENDING_CALLS);
                    }
                    let (id, value) = waiting.pop_front().expect("a call waits");
                    seq += 1;
                    run.answer_in_order(seq, 0.0, id, Ok(Some(value)))
                        .expect("the call waits");
                }
                Event::Done(outcome) => break outcome,
                other => assert!(matches!(other, Event::Started { .. }), "{other:?}"),
            }
        }
    })
    .await
    .expect("the run ends");
    assert_eq!(
        outcome
            .result
            .as_ref()
            .map(|completion| completion.value.clone()),
        Ok(Some(json!(total * (total - 1) / 2)))
    );
    assert_eq!(seq as usize, total);
}

/// A fan-out over thousands of items succeeds; the window only paces it.
#[tokio::test]
async fn a_promise_all_of_five_thousand_calls_succeeds() {
    let execution = execute(
        "const paths = Array.from({ length: 5000 }, (_, i) => 'f' + i); const read = await Promise.all(paths.map((path) => tools.echo({ path }))); return [read.length, read[4999].path]",
        Setup::tools(vec![echo()]),
    )
    .await;
    assert_eq!(execution.value(), Some(&json!([5000, "f4999"])));
    assert_eq!(execution.calls.len(), 5000);
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
/// collector; an abort still stops it.
#[tokio::test]
async fn aborts_a_script_at_the_heap_limit() {
    let mut run = crate::harness::start(
        "const keep = []; let full = false; for (;;) { try { keep.push('x'.repeat(1 << 16) + keep.length); } catch { if (!full) { full = true; text('full'); } } }",
        &Setup {
            limits: Some(Limits {
                heap_bytes: Some(32 << 20),
                ..Limits::default()
            }),
            ..Setup::default()
        },
    );
    let event = tokio::time::timeout(BOUND, run.next()).await;
    assert!(matches!(event, Ok(Event::Output(_))), "{event:?}");
    run.abort();
    let event = tokio::time::timeout(BOUND, run.next()).await;
    let Ok(Event::Done(outcome)) = event else {
        panic!("{event:?}");
    };
    assert!(
        matches!(outcome.result, Err(Failure::Aborted { .. })),
        "{outcome:?}"
    );
}

fn busy_limits(busy: Duration) -> Option<Limits> {
    Some(Limits {
        busy,
        ..Limits::default()
    })
}

/// No script runs JavaScript forever: without a deadline, a loop with or
/// without `await`, and a promise ping-pong that never lets the VM wait, end
/// once they ran for the busy budget, keeping their output.
#[tokio::test]
async fn ends_a_script_that_runs_javascript_for_the_busy_budget_without_waiting() {
    for code in [
        "while (true) {}",
        "while (true) await null",
        "const ping = () => Promise.resolve().then(pong); const pong = () => Promise.resolve().then(ping); ping(); await tools.echo(1); await new Promise(() => {})",
    ] {
        let execution = execute(
            &format!("text('before'); {code}"),
            Setup {
                tools: vec![echo()],
                limits: busy_limits(Duration::from_millis(200)),
                ..Setup::default()
            },
        )
        .await;
        assert_eq!(
            execution.failure(),
            &Failure::Busy {
                message: "Script ran JavaScript for 200 ms without waiting on a tool call (an endless loop?). Await tool calls in the loop, or set timeout_ms to allow a longer computation.".to_owned()
            },
            "{code}"
        );
        assert_eq!(execution.texts(), ["before"], "{code}");
    }
}

/// Time spent waiting for answers does not count toward the busy budget,
/// which restarts each time the VM waits.
#[tokio::test]
async fn the_busy_budget_does_not_count_time_waiting_for_calls() {
    let slow = async_tool("slow", |_, _| async {
        tokio::time::sleep(Duration::from_millis(700)).await;
        Ok(Some(json!("late")))
    });
    let execution = execute(
        "const results = []; for (let i = 0; i < 2; i++) results.push(await tools.slow()); return results",
        Setup {
            tools: vec![slow],
            limits: busy_limits(Duration::from_millis(500)),
            ..Setup::default()
        },
    )
    .await;
    assert_eq!(execution.value(), Some(&json!(["late", "late"])));
}

/// An explicit `timeout_ms` is the script's own bound: it raises the busy
/// budget, so a computation longer than the budget completes within it.
#[tokio::test]
async fn timeout_ms_raises_the_busy_budget() {
    let code = "const end = Date.now() + 400; while (Date.now() < end) {} return 'computed'";
    let execution = execute(
        code,
        Setup {
            timeout_ms: Some(60_000),
            limits: busy_limits(Duration::from_millis(100)),
            ..Setup::default()
        },
    )
    .await;
    assert_eq!(execution.value(), Some(&json!("computed")));
    let execution = execute(
        code,
        Setup {
            limits: busy_limits(Duration::from_millis(100)),
            ..Setup::default()
        },
    )
    .await;
    assert!(
        matches!(execution.failure(), Failure::Busy { .. }),
        "{:?}",
        execution.failure()
    );
}
