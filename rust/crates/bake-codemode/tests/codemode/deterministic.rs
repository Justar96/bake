//! Pi v1.3.0's `packages/codemode/test/deterministic.test.ts`: a
//! deterministic run, with the caller answering its calls in `seq` order, so
//! a durable host can replay it. Pi runs each case on its worker and remote
//! sandboxes; Bake has one place a VM runs.

use std::time::{SystemTime, UNIX_EPOCH};

use bake_codemode::{
    AnswerError, CallError, CallResult, Deterministic, During, Event, ExecuteRequest, Failure,
    GlobalDeclaration, HostCall, Limits, Outcome, OutputItem, Run, StartError, ToolDeclaration,
};
use serde_json::{Value, json};

use crate::harness::{BOUND, engine, json as parse, run};

/// `Date.UTC(2026, 0, 15, 10, 0, 0)`.
const STARTED_AT: f64 = 1_768_471_200_000.0;
/// The first two `Math.random()` values with seed 0, pinned by Pi.
const PINNED_SEED_ZERO: [f64; 2] = [0.6711372530266764, 0.7258461452833667];
/// The first `Math.random()` with seed 1, pinned by Pi.
const PINNED_FIRST_RANDOM: f64 = 0.34227450605335275;

type Answer = Box<dyn Fn(&HostCall) -> CallResult + Send>;

struct Options {
    started_at: f64,
    seed: u32,
    tools: Vec<ToolDeclaration>,
    globals: Vec<GlobalDeclaration>,
    limits: Limits,
    answer: Answer,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            started_at: STARTED_AT,
            seed: 1,
            tools: vec![
                ToolDeclaration::named("echo"),
                ToolDeclaration::named("wait"),
            ],
            globals: Vec::new(),
            limits: Limits::default(),
            answer: Box::new(|call| Ok(call.args().cloned())),
        }
    }
}

/// A deterministic run of `code` whose calls other than `wait` are answered
/// at once as result 1, 2, ... with the clock at `STARTED_AT + 1000 * seq`;
/// `wait` is left for the test.
struct Recorded {
    run: Run,
    answer: Answer,
    seq: u32,
    calls: Vec<HostCall>,
    idles: Vec<(u32, u32)>,
    events: Vec<String>,
    fingerprint: Option<String>,
    output: Vec<OutputItem>,
    outcome: Option<Outcome>,
}

fn start(code: &str, options: Options) -> Recorded {
    let mut request = ExecuteRequest::new(code);
    request.tools = options.tools;
    request.globals = options.globals;
    request.limits = options.limits;
    request.source.options.timeout_ms = Some(20_000);
    request.deterministic = Some(Deterministic {
        started_at_ms: options.started_at,
        seed: options.seed,
    });
    let run = match engine().start(request) {
        Ok(run) => run,
        Err(error) => panic!("{error}"),
    };
    Recorded {
        run,
        answer: options.answer,
        seq: 0,
        calls: Vec::new(),
        idles: Vec::new(),
        events: Vec::new(),
        fingerprint: None,
        output: Vec::new(),
        outcome: None,
    }
}

impl Recorded {
    /// Handles events until `until` holds or the run ends.
    async fn pump(&mut self, until: impl Fn(&Self) -> bool) {
        while self.outcome.is_none() && !until(self) {
            let event = match tokio::time::timeout(BOUND, self.run.next()).await {
                Ok(event) => event,
                Err(_) => panic!("no event within {BOUND:?}"),
            };
            match event {
                Event::Started { fingerprint } => {
                    self.fingerprint = Some(fingerprint);
                    self.events.push("started".to_owned());
                }
                Event::Call(call) => {
                    self.events.push(format!("call {}", call.id()));
                    if call.name() != "wait" {
                        self.seq += 1;
                        let answered = self.run.answer_in_order(
                            self.seq,
                            STARTED_AT + 1000.0 * f64::from(self.seq),
                            call.id(),
                            (self.answer)(&call),
                        );
                        assert_eq!(answered, Ok(()));
                    }
                    self.calls.push(call);
                }
                Event::Idle { seq, issued } => {
                    self.idles.push((seq, issued));
                    self.events.push(format!("idle {seq}"));
                }
                Event::Output(item) => self.output.push(item),
                Event::Done(outcome) => self.outcome = Some(outcome),
            }
        }
    }

    async fn result(&mut self) -> Outcome {
        self.pump(|_| false).await;
        self.outcome.clone().unwrap_or_else(|| panic!("no outcome"))
    }

    fn deliver(&self, seq: u32, now: f64, id: u32, result: CallResult) -> Result<(), AnswerError> {
        self.run.answer_in_order(seq, now, id, result)
    }
}

fn value(outcome: &Outcome) -> Value {
    match &outcome.result {
        Ok(completion) => completion.value.clone().unwrap_or(Value::Null),
        Err(failure) => panic!("{failure:?}"),
    }
}

fn numbers(value: &Value) -> Vec<f64> {
    value
        .as_array()
        .map(|items| items.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn makes_the_same_calls_and_output_for_the_same_start_seed_and_results() {
    let code = r#"
        const values = [Date.now(), performance.now(), Math.random(), Math.random(), new Date().toISOString()];
        const first = await tools.echo(values);
        text(JSON.stringify([Date.now(), performance.now(), Math.random()]));
        return await tools.echo(first.concat(Math.random()));
    "#;
    let mut one = start(code, Options::default());
    let mut two = start(code, Options::default());
    let (a, b) = tokio::join!(one.result(), two.result());
    assert_eq!(a, b);
    assert_eq!(one.output, two.output);
    assert_eq!(one.calls, two.calls);
    // The clock: startedAt, then the time of the result being processed; performance.now() counts from startedAt.
    let args = one.calls[0].args().cloned().unwrap_or(Value::Null);
    let first = numbers(&args);
    assert_eq!(&first[..2], &[STARTED_AT, 0.0]);
    assert_eq!(args[4], json!("2026-01-15T10:00:00.000Z"));
    let OutputItem::Text { text, .. } = &one.output[0] else {
        panic!("{:?}", one.output);
    };
    let printed = numbers(&parse(text));
    assert_eq!(&printed[..2], &[STARTED_AT + 1000.0, 1000.0]);
    // Pinned: a change here changes what replayed scripts see, so it must change the fingerprint too.
    let random = first[2];
    assert_eq!(random, PINNED_FIRST_RANDOM);

    // The seed decides the random values, not the start time.
    let mut later = start(
        code,
        Options {
            started_at: STARTED_AT + 5000.0,
            ..Options::default()
        },
    );
    let mut reseeded = start(
        code,
        Options {
            seed: 2,
            ..Options::default()
        },
    );
    tokio::join!(later.result(), reseeded.result());
    assert_eq!(
        numbers(&later.calls[0].args().cloned().unwrap_or(Value::Null))[2],
        random
    );
    assert_ne!(
        numbers(&reseeded.calls[0].args().cloned().unwrap_or(Value::Null))[2],
        random
    );
    // Seed 0 has its own values: QuickJS turns a zero state into 1, which must not make it seed 1's.
    let mut zero = start(
        "return [Math.random(), Math.random()]",
        Options {
            seed: 0,
            ..Options::default()
        },
    );
    let zero = numbers(&value(&zero.result().await));
    assert_eq!(zero, PINNED_SEED_ZERO);
    assert!(!zero.contains(&random));
}

/// Pi also checks that `execute()` keeps the host's time zone; Bake runs every
/// script in UTC (`host_get_timezone_offset` returns 0), so only the
/// intrinsics differ.
#[tokio::test]
async fn runs_in_utc_and_without_weak_ref_or_finalization_registry() {
    let code = "return [new Date(0).getHours(), new Date(0).getTimezoneOffset(), typeof WeakRef, typeof FinalizationRegistry]";
    let mut recorded = start(code, Options::default());
    assert_eq!(
        value(&recorded.result().await),
        json!([0, 0, "undefined", "undefined"])
    );
    assert_eq!(
        run(code).await.value(),
        Some(&json!([0, 0, "function", "function"]))
    );
}

#[tokio::test]
async fn applies_results_in_seq_order_whatever_order_they_are_delivered_in() {
    let mut recorded = start(
        r#"const order = [];
        await Promise.all([tools.wait("a").then(() => order.push("a")), tools.wait("b").then(() => order.push("b"))]);
        return order;"#,
        Options::default(),
    );
    recorded
        .pump(|recorded| recorded.calls.len() == 2 && recorded.idles.len() == 1)
        .await;
    let (a, b) = (recorded.calls[0].id(), recorded.calls[1].id());
    // b is result 1 and a result 2, but a arrives first.
    assert_eq!(
        recorded.deliver(2, STARTED_AT + 2000.0, a, Ok(Some(Value::Null))),
        Ok(())
    );
    assert_eq!(
        recorded.deliver(1, STARTED_AT + 1000.0, b, Ok(Some(Value::Null))),
        Ok(())
    );
    assert_eq!(value(&recorded.result().await), json!(["b", "a"]));
    // No idle after the last result: the script finished in that drain.
    assert_eq!(recorded.idles, [(0, 2), (1, 2)]);
}

/// Pi's remote-transport case: results delivered in order are applied in order.
#[tokio::test]
async fn applies_results_in_seq_order_when_delivered_in_order() {
    let mut recorded = start(
        r#"const order = [];
        await Promise.all([tools.wait("a").then(() => order.push("a")), tools.wait("b").then(() => order.push("b"))]);
        return order;"#,
        Options::default(),
    );
    recorded.pump(|recorded| recorded.calls.len() == 2).await;
    let (a, b) = (recorded.calls[0].id(), recorded.calls[1].id());
    assert_eq!(
        recorded.deliver(1, STARTED_AT + 1000.0, a, Ok(Some(Value::Null))),
        Ok(())
    );
    assert_eq!(
        recorded.deliver(2, STARTED_AT + 2000.0, b, Ok(Some(Value::Null))),
        Ok(())
    );
    assert_eq!(value(&recorded.result().await), json!(["a", "b"]));
}

fn heap(bytes: u32) -> Options {
    Options {
        limits: Limits {
            heap_bytes: Some(bytes),
            ..Limits::default()
        },
        ..Options::default()
    }
}

#[tokio::test]
async fn reports_the_fingerprint_before_any_call_and_the_calls_issued_after_each_result() {
    let mut recorded = start(
        "await tools.echo(1); await Promise.all([tools.echo(2), tools.echo(3)]);",
        heap(64 << 20),
    );
    assert!(recorded.result().await.result.is_ok());
    assert_eq!(&recorded.events[..3], ["started", "call 1", "idle 0"]);
    assert_eq!(recorded.idles, [(0, 1), (1, 3), (2, 3)]);
    let calls: Vec<(u32, bool, &str, Option<&Value>)> = recorded
        .calls
        .iter()
        .map(|call| {
            (
                call.id(),
                matches!(call, HostCall::Tool { .. }),
                call.name(),
                call.args(),
            )
        })
        .collect();
    assert_eq!(
        calls,
        [
            (1, true, "echo", Some(&json!(1))),
            (2, true, "echo", Some(&json!(2))),
            (3, true, "echo", Some(&json!(3))),
        ]
    );
    let fingerprint = recorded.fingerprint.clone().unwrap_or_default();
    let parts: Vec<&str> = fingerprint.split(", ").collect();
    assert_eq!(parts.len(), 7, "{fingerprint}");
    assert_eq!(parts[0], "quickjs-wasi 3.6.2");
    // QuickJS-NG's version, as `engine/PROVENANCE` records it.
    assert_eq!(parts[1], "quickjs 0.17.0");
    assert_eq!(parts[2], "prelude 8c6c9242");
    assert_eq!(parts[3], "deterministic 1");
    assert_eq!(parts[4], "stack 524288");
    assert_eq!(parts[5], "heap 67108864");
    assert_eq!(parts[6], "budget none");

    let mut same = start("return 1", heap(64 << 20));
    let mut other = start("return 1", heap(32 << 20));
    tokio::join!(same.result(), other.result());
    assert_eq!(same.fingerprint.as_deref(), Some(fingerprint.as_str()));
    assert_ne!(other.fingerprint.as_deref(), Some(fingerprint.as_str()));
}

#[tokio::test]
async fn numbers_calls_to_globals_with_tools_and_answers_errors_as_rejections() {
    let mut recorded = start(
        r#"const found = await search("x", 2);
        try { await tools.echo("fail"); } catch (error) { text(error.message); }
        return found;"#,
        Options {
            globals: vec![GlobalDeclaration::spread("search")],
            answer: Box::new(|call| {
                if call.args() == Some(&json!("fail")) {
                    Err(CallError::new("no"))
                } else if call.name() == "search" {
                    Ok(Some(json!(["found"])))
                } else {
                    Ok(call.args().cloned())
                }
            }),
            ..Options::default()
        },
    );
    let outcome = recorded.result().await;
    assert_eq!(
        recorded.calls[0],
        HostCall::Global {
            id: 1,
            name: "search".to_owned(),
            args: Some(json!(["x", 2]))
        }
    );
    assert_eq!(value(&outcome), json!(["found"]));
    assert_eq!(
        recorded.output,
        [OutputItem::Text {
            text: "no".to_owned(),
            console: false
        }]
    );
    assert!(matches!(&recorded.calls[1], HostCall::Tool { id: 2, name, .. } if name == "echo"));
    // Calls to globals count as issued calls too.
    assert_eq!(&recorded.idles[..2], [(0, 1), (1, 2)]);
}

#[tokio::test]
async fn keeps_the_clock_monotonic_and_rejects_a_clock_value_that_is_not_finite() {
    let mut recorded = start(
        "const before = Date.now(); await tools.wait(1); return [before, Date.now(), performance.now()]",
        Options::default(),
    );
    recorded.pump(|recorded| recorded.calls.len() == 1).await;
    let refused = recorded.deliver(1, f64::NAN, 1, Ok(Some(Value::Null)));
    assert_eq!(refused, Err(AnswerError::ClockNotFinite(1)));
    assert!(
        refused
            .map_err(|error| error.to_string())
            .unwrap_err()
            .contains("not a finite number")
    );
    // A resumed host whose clock is behind the run's start.
    assert_eq!(
        recorded.deliver(1, STARTED_AT - 2000.0, 1, Ok(Some(Value::Null))),
        Ok(())
    );
    assert_eq!(
        numbers(&value(&recorded.result().await)),
        [STARTED_AT, STARTED_AT, 0.0]
    );
}

#[tokio::test]
async fn reports_the_result_the_vm_was_processing_when_the_script_ended() {
    let mut returned = start(
        "await tools.echo(1); return await tools.echo(2)",
        Options::default(),
    );
    let mut thrown = start(
        "await tools.echo(1); await Promise.all([tools.echo(2), tools.echo(3)]); throw new Error('x')",
        Options::default(),
    );
    let mut first = start("return 1", Options::default());
    let (returned, thrown, first) =
        tokio::join!(returned.result(), thrown.result(), first.result());
    assert_eq!(value(&returned), json!(2));
    assert_eq!(returned.during, Some(During { seq: 2, issued: 2 }));
    assert!(thrown.result.is_err());
    assert_eq!(thrown.during, Some(During { seq: 3, issued: 3 }));
    assert!(first.result.is_ok());
    assert_eq!(first.during, Some(During { seq: 0, issued: 0 }));
    assert_eq!(run("return 1").await.outcome.during, None);
}

#[tokio::test]
async fn rejects_with_an_error_carrying_the_answers_properties() {
    let mut recorded = start(
        "try { await tools.echo(1); } catch (e) { return [e instanceof Error, e.message, e.name, e.structuredOutput]; }",
        Options {
            answer: Box::new(|_| {
                Err(CallError {
                    message: "denied".to_owned(),
                    properties: parse(r#"{ "structuredOutput": { "code": 7 }, "name": "Ignored", "message": "ignored" }"#)
                        .as_object()
                        .cloned(),
                })
            }),
            ..Options::default()
        },
    );
    assert_eq!(
        value(&recorded.result().await),
        json!([true, "denied", "Error", { "code": 7 }])
    );
}

/// Pi also refuses rejection properties that are not an object, which
/// [`CallError`]'s type rules out.
#[test]
fn refuses_tools_sharing_a_script_name() {
    let mut request = ExecuteRequest::new("");
    request.tools = vec![ToolDeclaration::named("a-b"), ToolDeclaration::named("a_b")];
    request.deterministic = Some(Deterministic {
        started_at_ms: STARTED_AT,
        seed: 1,
    });
    let refused = engine().start(request).map(|_| ());
    assert_eq!(
        refused,
        Err(StartError::InvalidRequest(
            "Tool script names must be unique".to_owned()
        ))
    );
}

#[tokio::test]
async fn uses_the_script_names_the_run_passes() {
    let mut recorded = start(
        "return [typeof tools.renamed, Object.keys(tools), ALL_TOOLS.map((tool) => tool.name)]",
        Options {
            tools: vec![ToolDeclaration {
                name: "my-tool".to_owned(),
                js_name: Some("renamed".to_owned()),
                description: None,
            }],
            ..Options::default()
        },
    );
    assert_eq!(
        value(&recorded.result().await),
        json!(["function", ["renamed", "my-tool"], ["renamed"]])
    );
}

#[tokio::test]
async fn ends_with_an_error_the_script_cannot_catch_when_the_caller_aborts() {
    let mut recorded = start(
        r#"try { await tools.wait(1); } catch { text("caught"); }
        text("after");"#,
        Options::default(),
    );
    recorded.pump(|recorded| recorded.calls.len() == 1).await;
    recorded.run.abort();
    let outcome = recorded.result().await;
    assert!(
        matches!(outcome.result, Err(Failure::Aborted { .. })),
        "{outcome:?}"
    );
    assert!(recorded.output.is_empty());
    assert_eq!(
        recorded.deliver(1, STARTED_AT, 1, Ok(Some(Value::Null))),
        Ok(())
    );
}

#[tokio::test]
async fn is_finished_once_it_has_its_outcome() {
    let mut recorded = start("await tools.wait(1); return 2", Options::default());
    recorded.pump(|recorded| recorded.calls.len() == 1).await;
    assert!(!recorded.run.finished());
    assert_eq!(
        recorded.deliver(1, STARTED_AT, 1, Ok(Some(Value::Null))),
        Ok(())
    );
    recorded.result().await;
    assert!(recorded.run.finished());
}

#[tokio::test]
async fn rejects_a_call_that_is_not_waiting_and_a_result_number_used_twice() {
    let mut recorded = start(
        "await Promise.all([tools.wait(1), tools.wait(2)])",
        Options::default(),
    );
    recorded.pump(|recorded| recorded.calls.len() == 2).await;
    let not_waiting = recorded.deliver(1, STARTED_AT, 99, Ok(Some(Value::Null)));
    assert_eq!(not_waiting, Err(AnswerError::NotWaiting(99)));
    assert_eq!(
        not_waiting.map_err(|error| error.to_string()),
        Err("Call 99 is not waiting for a result".to_owned())
    );
    assert_eq!(
        recorded.deliver(1, STARTED_AT, 1, Ok(Some(Value::Null))),
        Ok(())
    );
    let reused = recorded.deliver(1, STARTED_AT, 2, Ok(Some(Value::Null)));
    assert!(
        reused
            .map_err(|error| error.to_string())
            .unwrap_err()
            .contains("used once")
    );
    let zero = recorded.deliver(0, STARTED_AT, 2, Ok(Some(Value::Null)));
    assert!(
        zero.map_err(|error| error.to_string())
            .unwrap_err()
            .contains("used once")
    );
    assert_eq!(
        recorded.deliver(2, STARTED_AT, 2, Ok(Some(Value::Null))),
        Ok(())
    );
    assert!(recorded.result().await.result.is_ok());
}

/// Answers take the run's own method: `answer_in_order` for deterministic
/// runs, `answer` otherwise.
#[tokio::test]
async fn refuses_an_answer_of_the_other_mode() {
    let mut recorded = start("return await tools.wait(1)", Options::default());
    recorded.pump(|recorded| recorded.calls.len() == 1).await;
    assert_eq!(
        recorded.run.answer(1, Ok(None)),
        Err(AnswerError::WrongMode)
    );
    assert_eq!(
        recorded.deliver(1, STARTED_AT, 1, Ok(Some(json!("fine")))),
        Ok(())
    );
    assert_eq!(value(&recorded.result().await), json!("fine"));
}

#[tokio::test]
async fn execute_reads_the_real_clock_and_real_randomness() {
    let now = || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |since| since.as_millis() as f64)
    };
    let before = now();
    let first = run("return [Date.now(), Math.random()]").await;
    let second = run("return [Date.now(), Math.random()]").await;
    let first = numbers(first.value().unwrap_or(&Value::Null));
    assert!(first[0] >= before && first[0] <= now() + 1.0, "{first:?}");
    assert!(!numbers(second.value().unwrap_or(&Value::Null)).contains(&first[1]));
}
