//! Pi v1.3.0's `packages/codemode/test/worker-lifetime.test.ts`, about the
//! run's thread instead of a worker: the outcome is decided without waiting
//! for anything but the thread, the thread's exit is observable through the
//! request's [`ExitHook`] (Pi's `acquireWorker` release), and a refused
//! request starts no thread. Cases about Node and Bun worker termination,
//! worker construction, wasm loading, and a throwing `onWorkerStarted` hook
//! have no counterpart; the crate README lists them.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bake_codemode::{
    Deterministic, Event, ExecuteRequest, ExitHook, Failure, GlobalDeclaration, OutputItem,
    StartError, ToolDeclaration,
};

use crate::harness::{BOUND, engine};

fn counted(mut request: ExecuteRequest, exits: &Arc<AtomicUsize>) -> ExecuteRequest {
    let exits = Arc::clone(exits);
    request.on_exit = Some(ExitHook::new(move || {
        exits.fetch_add(1, Ordering::SeqCst);
    }));
    request
}

async fn wait_for_exit(exits: &AtomicUsize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while exits.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "the script thread did not exit");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Pi's "does not start a worker when admission is denied": a refused request
/// starts no thread, and its hook is called once.
#[test]
fn a_refused_request_starts_no_thread_and_releases_its_hook() {
    let exits = Arc::new(AtomicUsize::new(0));
    let mut request = counted(ExecuteRequest::new("while (true) {}"), &exits);
    request.globals = vec![GlobalDeclaration::new("tools")];
    let refused = engine().start(request).map(|_| ());
    assert!(
        matches!(refused, Err(StartError::InvalidRequest(_))),
        "{refused:?}"
    );
    assert_eq!(exits.load(Ordering::SeqCst), 1);
}

/// The deadline of the hanging script's timeout arm. It outlasts the
/// script's start, which takes about 30 ms and has been seen to take 270 ms
/// on a loaded machine, so a slow shared runner reaches the script's call
/// before the deadline. The spinning script below needs no such margin: its
/// loop never has to reach anything.
const HANG_DEADLINE_MS: u64 = 2000;

#[derive(Clone, Copy, Debug)]
enum End {
    Abort,
    Timeout,
    Drop,
}

/// Pi's "settles abort/timeout/close without waiting for worker exit": the
/// outcome keeps the output so far, and the thread then exits.
#[tokio::test]
async fn settles_abort_timeout_and_drop_and_the_thread_exits() {
    for end in [End::Abort, End::Timeout, End::Drop] {
        let exits = Arc::new(AtomicUsize::new(0));
        let mut request = counted(
            ExecuteRequest::new("text(\"partial\"); await tools.hang()"),
            &exits,
        );
        request.tools = vec![ToolDeclaration::named("hang")];
        if matches!(end, End::Timeout) {
            request.source.options.timeout_ms = Some(HANG_DEADLINE_MS);
        }
        let mut run = match engine().start(request) {
            Ok(run) => run,
            Err(error) => panic!("{error}"),
        };
        let mut output = Vec::new();
        loop {
            match tokio::time::timeout(BOUND, run.next()).await {
                Ok(Event::Output(item)) => output.push(item),
                Ok(Event::Call(call)) => {
                    assert_eq!(call.name(), "hang");
                    break;
                }
                other => panic!("{end:?}: {other:?}"),
            }
        }
        assert_eq!(
            output,
            [OutputItem::Text {
                text: "partial".to_owned(),
                console: false
            }]
        );
        match end {
            End::Abort | End::Timeout => {
                if matches!(end, End::Abort) {
                    run.abort_with("cancelled");
                }
                let event = tokio::time::timeout(BOUND, run.next()).await;
                let Ok(Event::Done(outcome)) = event else {
                    panic!("{end:?}: {event:?}");
                };
                match end {
                    End::Abort => assert_eq!(
                        outcome.result,
                        Err(Failure::Aborted {
                            message: "cancelled".to_owned()
                        })
                    ),
                    _ => assert_eq!(
                        outcome.result,
                        Err(Failure::Timeout {
                            message: format!("Execution timed out after {HANG_DEADLINE_MS} ms")
                        })
                    ),
                }
                // Later calls return the decided outcome again.
                let again = tokio::time::timeout(BOUND, run.next()).await;
                assert_eq!(again.ok(), Some(Event::Done(outcome)));
                wait_for_exit(&exits).await;
                drop(run);
            }
            End::Drop => {
                drop(run);
                wait_for_exit(&exits).await;
            }
        }
        assert_eq!(exits.load(Ordering::SeqCst), 1, "{end:?}");
    }
}

/// Pi's "reports the actual exit of execute() and start() workers".
#[tokio::test]
async fn reports_the_exit_of_ordinary_and_deterministic_runs() {
    let exits = Arc::new(AtomicUsize::new(0));
    let mut ordinary = match engine().start(counted(ExecuteRequest::new("return 1"), &exits)) {
        Ok(run) => run,
        Err(error) => panic!("{error}"),
    };
    let event = tokio::time::timeout(BOUND, ordinary.next()).await;
    assert!(
        matches!(event, Ok(Event::Done(ref outcome)) if outcome.result.is_ok()),
        "{event:?}"
    );
    let mut request = counted(ExecuteRequest::new("return 2"), &exits);
    request.deterministic = Some(Deterministic {
        started_at_ms: 0.0,
        seed: 0,
    });
    let mut deterministic = match engine().start(request) {
        Ok(run) => run,
        Err(error) => panic!("{error}"),
    };
    loop {
        match tokio::time::timeout(BOUND, deterministic.next()).await {
            Ok(Event::Done(outcome)) => {
                assert!(outcome.result.is_ok(), "{outcome:?}");
                break;
            }
            Ok(_) => {}
            Err(_) => panic!("no outcome"),
        }
    }
    assert!(deterministic.finished());
    let deadline = Instant::now() + Duration::from_secs(5);
    while exits.load(Ordering::SeqCst) < 2 {
        assert!(Instant::now() < deadline, "a script thread did not exit");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    drop((ordinary, deterministic));
    assert_eq!(exits.load(Ordering::SeqCst), 2);
}

/// Pi's process-lifetime cases: a spinning script ends promptly after an
/// abort, a timeout, or a drop, and so does its thread.
#[tokio::test]
async fn a_spinning_script_and_its_thread_end_promptly() {
    for end in [End::Abort, End::Timeout, End::Drop] {
        let exits = Arc::new(AtomicUsize::new(0));
        let mut request = counted(ExecuteRequest::new("while (true) {}"), &exits);
        if matches!(end, End::Timeout) {
            request.source.options.timeout_ms = Some(100);
        }
        let mut run = match engine().start(request) {
            Ok(run) => run,
            Err(error) => panic!("{error}"),
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        let started = Instant::now();
        match end {
            End::Abort => {
                run.abort();
                let event = tokio::time::timeout(BOUND, run.next()).await;
                assert!(matches!(event, Ok(Event::Done(_))), "{event:?}");
            }
            End::Timeout => {
                let event = tokio::time::timeout(BOUND, run.next()).await;
                assert!(
                    matches!(&event, Ok(Event::Done(outcome)) if matches!(outcome.result, Err(Failure::Timeout { .. }))),
                    "{event:?}"
                );
            }
            End::Drop => drop(run),
        }
        wait_for_exit(&exits).await;
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{end:?}: {:?}",
            started.elapsed()
        );
    }
}
