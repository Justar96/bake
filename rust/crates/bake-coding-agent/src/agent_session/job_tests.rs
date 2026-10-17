//! Background job notices through a session: delivered once, inside a
//! run or by a turn they open while the wake budget lasts, then kept for
//! the next prompt. Bake's TypeScript `packages/jobs/tool-jobs/src/index.ts`
//! `onJobDone` and `maxConsecutiveWakes`; waits are on job state, never on
//! time.

use std::time::Duration;

use bake_ai::providers::faux::faux_assistant_message;
use serde_json::Value;

use super::control_tests::{harness, in_memory};
use super::*;
use crate::jobs::{JobId, JobOutcome, JobSpec, MAX_CONSECUTIVE_WAKES, OutputMode};
use crate::print_mode::{PrintModeOptions, PrintOutput, SharedWriter, run_print_mode};

fn print_options(job_wait: Duration) -> PrintModeOptions {
    PrintModeOptions {
        control: RunControl::default(),
        mode: PrintOutput::Text,
        initial_message: None,
        initial_images: Vec::new(),
        messages: Vec::new(),
        job_wait,
    }
}

fn sink() -> SharedWriter {
    Arc::new(Mutex::new(Vec::<u8>::new()))
}

#[tokio::test]
async fn zero_print_job_budget_admits_no_notice_request() {
    let h = harness();
    let session = h
        .session(in_memory(), Settings::default(), None, Vec::new())
        .await;
    finished(&session, "already complete").await;
    h.faux.append_responses(vec![
        faux_assistant_message("unexpected notice turn").into(),
    ]);
    let code = run_print_mode(&session, print_options(Duration::ZERO), sink(), sink()).await;
    assert_eq!(code, 0);
    assert_eq!(h.faux.call_count(), 0, "zero wait admitted a model request");
}

#[tokio::test]
async fn the_print_job_deadline_aborts_and_awaits_a_notice_turn() {
    assert_print_notice_deadline(false).await;
}

#[tokio::test]
async fn the_shared_run_deadline_still_bounds_a_longer_print_job_wait() {
    assert_print_notice_deadline(true).await;
}

async fn assert_print_notice_deadline(shared_deadline: bool) {
    let h = harness();
    let session = Arc::new(
        h.session(in_memory(), Settings::default(), None, Vec::new())
            .await,
    );
    finished(&session, "already complete").await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let aborted = Arc::new(tokio::sync::Notify::new());
    let cleanup = Arc::new(tokio::sync::Notify::new());
    let cleaned = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let settled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let settled_events = Arc::clone(&settled);
    let _subscription = session.subscribe(move |event| {
        if matches!(event, SessionEvent::AgentSettled { aborted: true, .. }) {
            settled_events.fetch_add(1, Ordering::SeqCst);
        }
    });
    h.faux
        .append_responses(vec![bake_ai::providers::faux::FauxResponseStep::Factory(
            Arc::new({
                let entered = Arc::clone(&entered);
                let aborted = Arc::clone(&aborted);
                let cleanup = Arc::clone(&cleanup);
                let cleaned = Arc::clone(&cleaned);
                move |_, options, _, _| {
                    let signal = options.base.signal.clone().expect("request signal");
                    let entered = Arc::clone(&entered);
                    let aborted = Arc::clone(&aborted);
                    let cleanup = Arc::clone(&cleanup);
                    let cleaned = Arc::clone(&cleaned);
                    Box::pin(async move {
                        entered.notify_one();
                        signal.cancelled().await;
                        aborted.notify_one();
                        cleanup.notified().await;
                        cleaned.store(true, Ordering::SeqCst);
                        Err("request cancelled".into())
                    })
                }
            }),
        )]);
    let owner = Arc::clone(&session);
    let mut run = tokio::spawn(async move {
        let mut options =
            print_options(Duration::from_secs(if shared_deadline { 600 } else { 60 }));
        if shared_deadline {
            options.control = RunControl::new(bake_agent::RunLimits {
                deadline: Some(tokio::time::Instant::now() + Duration::from_secs(60)),
                ..bake_agent::RunLimits::default()
            });
        }
        run_print_mode(&owner, options, sink(), sink()).await
    });
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .expect("notice request started");
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::time::resume();
    let cancelled_at_deadline = tokio::time::timeout(Duration::from_secs(1), aborted.notified())
        .await
        .is_ok();
    let returned_before_cleanup = run.is_finished();
    cleanup.notify_one();
    if !cancelled_at_deadline {
        // A failing negative control still owns and drains its request.
        session.abort().await;
    }
    let result = tokio::time::timeout(Duration::from_secs(10), &mut run).await;
    session.shutdown().await;
    assert!(
        cancelled_at_deadline,
        "the notice turn escaped the print job deadline"
    );
    assert!(
        !returned_before_cleanup,
        "print returned before request cleanup"
    );
    assert!(result.expect("print settled").is_ok());
    assert!(
        cleaned.load(Ordering::SeqCst),
        "owned request was not drained"
    );
    assert_eq!(
        settled.load(Ordering::SeqCst),
        1,
        "the admitted notice delivery must settle, not be dropped"
    );
}

#[tokio::test]
async fn idle_notices_share_the_prompt_call_allowance() {
    let h = harness();
    let session = h
        .session(in_memory(), Settings::default(), None, Vec::new())
        .await;
    let control = RunControl::new(bake_agent::RunLimits {
        max_model_calls: Some(1),
        ..bake_agent::RunLimits::default()
    });
    let empty = session
        .deliver_job_notices_with_control(control.clone())
        .await
        .unwrap();
    assert_eq!(empty, RunOutcome::FinishedTurn);
    assert_eq!(h.faux.call_count(), 0);
    h.faux.append_responses(vec![
        faux_assistant_message("first answer").into(),
        faux_assistant_message("unexpected fresh allowance").into(),
    ]);
    let first = session
        .prompt_with_control("first", Vec::new(), control.clone())
        .await
        .unwrap();
    finished(&session, "completed after the prompt").await;
    let notice = session
        .deliver_job_notices_with_control(control.clone())
        .await
        .unwrap();
    session.shutdown().await;
    assert_eq!(first, RunOutcome::FinishedTurn);
    assert!(
        matches!(notice, RunOutcome::BudgetExhausted { .. }),
        "{notice:?}"
    );
    assert_eq!(
        h.faux.call_count(),
        1,
        "notice delivery refunded the shared allowance"
    );
    assert_eq!(control.model_calls(), 1);
}

#[tokio::test]
async fn a_parked_notice_waiter_does_not_hold_abort_or_release_its_owned_cleanup() {
    let h = harness();
    let session = Arc::new(
        h.session(in_memory(), Settings::default(), None, Vec::new())
            .await,
    );
    finished(&session, "completed").await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let aborted = Arc::new(tokio::sync::Notify::new());
    let cleanup = Arc::new(tokio::sync::Notify::new());
    h.faux
        .append_responses(vec![bake_ai::providers::faux::FauxResponseStep::Factory(
            Arc::new({
                let entered = Arc::clone(&entered);
                let aborted = Arc::clone(&aborted);
                let cleanup = Arc::clone(&cleanup);
                move |_, options, _, _| {
                    let signal = options.base.signal.clone().unwrap();
                    let entered = Arc::clone(&entered);
                    let aborted = Arc::clone(&aborted);
                    let cleanup = Arc::clone(&cleanup);
                    Box::pin(async move {
                        entered.notify_one();
                        signal.cancelled().await;
                        aborted.notify_one();
                        cleanup.notified().await;
                        Err("request cancelled".into())
                    })
                }
            }),
        )]);
    let delivery = session.deliver_job_notices_with_control(RunControl::default());
    tokio::pin!(delivery);
    tokio::select! {
        () = entered.notified() => {},
        result = &mut delivery => panic!("notice request ended before the barrier: {result:?}"),
        () = tokio::time::sleep(Duration::from_secs(10)) => panic!("notice request did not start"),
    }
    let owner = Arc::clone(&session);
    let stop = tokio::spawn(async move { owner.abort().await });
    tokio::time::timeout(Duration::from_secs(10), aborted.notified())
        .await
        .unwrap();
    let premature_abort = stop.is_finished();
    cleanup.notify_one();
    let independent_abort = tokio::time::timeout(Duration::from_secs(10), async {
        while !stop.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    // Drain even a failing borrowed-driver control before asserting.
    let (result, stopped) = tokio::join!(&mut delivery, stop);
    session.shutdown().await;
    stopped.unwrap();
    assert!(
        !premature_abort,
        "abort released the request before owned cleanup"
    );
    assert!(
        independent_abort,
        "abort needed the parked notice caller to be polled"
    );
    assert_eq!(result.unwrap(), RunOutcome::Cancelled);
    assert_eq!(notices(&session), ["bash-1"]);
    assert_eq!(h.faux.call_count(), 1);
}

/// Starts a job that completes at once and waits until it has settled.
async fn finished(session: &AgentSession, label: &str) -> JobId {
    let jobs = session.jobs();
    let id = jobs
        .start(
            JobSpec {
                kind: "bash",
                label: label.into(),
                output: OutputMode::Stream,
            },
            |_| async { JobOutcome::completed("exit code: 0") },
        )
        .expect("admitted");
    tokio::time::timeout(Duration::from_secs(10), async {
        while !jobs.get(id).expect("kept").status.is_terminal() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("settled");
    id
}

/// The job ids of the notice entries, in log order.
fn notices(session: &AgentSession) -> Vec<String> {
    lock(&session.session)
        .entries()
        .iter()
        .filter(|entry| entry.entry_type() == "custom_message")
        .filter_map(|entry| Some(entry.as_json()["details"]["jobId"].as_str()?.to_owned()))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_notices_wake_the_agent_within_the_budget_then_wait_for_a_prompt() {
    let h = harness();
    let session = h
        .session(in_memory(), Settings::default(), None, Vec::new())
        .await;
    // Nothing settled: no turn.
    assert!(!session.deliver_job_notices().await.expect("delivered"));
    for turn in 1..=MAX_CONSECUTIVE_WAKES {
        h.faux
            .append_responses(vec![faux_assistant_message("noted").into()]);
        finished(&session, &format!("job {turn}")).await;
        assert!(session.deliver_job_notices().await.expect("delivered"));
        assert_eq!(h.faux.call_count(), turn as u64);
    }
    // Past the budget the notice is claimed and kept; no turn opens, and
    // a second delivery finds nothing new.
    finished(&session, "job 4").await;
    assert!(!session.deliver_job_notices().await.expect("delivered"));
    assert!(!session.deliver_job_notices().await.expect("delivered"));
    assert_eq!(h.faux.call_count(), MAX_CONSECUTIVE_WAKES as u64);
    assert_eq!(notices(&session), ["bash-1", "bash-2", "bash-3"]);

    // The next prompt carries it ahead of the user's text and restores
    // the budget.
    h.faux
        .append_responses(vec![faux_assistant_message("ok").into()]);
    session.prompt("next", Vec::new()).await.expect("prompt");
    assert_eq!(notices(&session), ["bash-1", "bash-2", "bash-3", "bash-4"]);
    let roles: Vec<String> = lock(&session.session)
        .entries()
        .iter()
        .rev()
        .take(3)
        .map(|entry| {
            let json = entry.as_json();
            json.get("message")
                .and_then(|message| message.get("role"))
                .and_then(Value::as_str)
                .unwrap_or_else(|| entry.entry_type())
                .to_owned()
        })
        .collect();
    assert_eq!(roles, ["assistant", "user", "custom_message"]);
    assert!(session.jobs().notice_would_wake());
    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_that_settles_before_a_prompt_is_reported_with_it_once() {
    let h = harness();
    let session = h
        .session(in_memory(), Settings::default(), None, Vec::new())
        .await;
    finished(&session, "early").await;
    h.faux
        .append_responses(vec![faux_assistant_message("ok").into()]);
    session.prompt("go", Vec::new()).await.expect("prompt");
    assert_eq!(notices(&session), ["bash-1"]);
    assert!(!session.deliver_job_notices().await.expect("delivered"));
    assert_eq!(h.faux.call_count(), 1);
    session.shutdown().await;
}
