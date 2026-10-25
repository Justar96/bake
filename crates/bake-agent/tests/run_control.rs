//! Caller-owned limits and outcomes are independent of transcript tails.

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{future::Future, hash::BuildHasher, io::Write, pin::Pin, task::Poll};

use bake_agent::{
    AdmissionObserver, Agent, AgentEvent, AgentOptions, AgentTool, AgentToolResult, BudgetLimit,
    ModelCallKind, RunControl, RunLimits, RunOutcome, StreamFn,
};
use bake_ai::{StopReason, assistant_message_channel};
use common::*;
use serde_json::json;
use tokio::sync::Notify;
use tokio::time::Instant;

fn agent(stream: StreamFn) -> Agent {
    Agent::new(AgentOptions {
        stream_fn: Some(stream),
        ..AgentOptions::default()
    })
    .unwrap()
}

fn limited(calls: u64) -> RunControl {
    RunControl::new(RunLimits {
        max_model_calls: Some(calls),
        ..RunLimits::default()
    })
}

#[tokio::test]
async fn restored_debits_keep_the_original_allowance_and_ordinals() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let writes = Arc::clone(&observed);
    let control = RunControl::restore(
        RunLimits {
            deadline: None,
            max_model_calls: Some(3),
        },
        2,
        Some(Arc::new(move |admission| {
            writes.lock().unwrap().push(admission.ordinal);
            Box::pin(async { Ok(()) })
        })),
    );
    let provider = bake_ai::providers::faux::FauxProvider::new(Default::default());
    let model = provider.model();
    assert_eq!(
        control
            .admit_model_call(ModelCallKind::Agent, model)
            .await
            .unwrap()
            .ordinal,
        3
    );
    assert_eq!(
        control
            .admit_model_call(ModelCallKind::Summary, model)
            .await,
        Err(RunOutcome::BudgetExhausted {
            limit: BudgetLimit::ModelCalls
        })
    );
    assert_eq!(control.model_calls(), 3);
    assert_eq!(*observed.lock().unwrap(), [3]);
    let exceeded = RunControl::restore(
        RunLimits {
            deadline: None,
            max_model_calls: Some(1),
        },
        2,
        None,
    );
    assert!(
        exceeded
            .admit_model_call(ModelCallKind::Agent, model)
            .await
            .is_err()
    );
    assert_eq!(exceeded.model_calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn dedicated_interval_completion_closes_admission_without_rewriting_shared_settlement() {
    let shared = RunControl::default();
    assert_eq!(
        shared.settle(RunOutcome::FinishedTurn),
        RunOutcome::FinishedTurn
    );
    assert_eq!(shared.stopped(), None);
    let control = RunControl::new(RunLimits {
        deadline: Some(Instant::now() + Duration::from_secs(5)),
        max_model_calls: None,
    });
    assert_eq!(
        control.finish_interval(RunOutcome::FinishedTurn),
        RunOutcome::FinishedTurn
    );
    tokio::time::advance(Duration::from_secs(10)).await;
    control.cancel();
    assert_eq!(control.stopped(), Some(RunOutcome::FinishedTurn));
    let provider = bake_ai::providers::faux::FauxProvider::new(Default::default());
    assert_eq!(
        control
            .admit_model_call(ModelCallKind::Agent, provider.model())
            .await,
        Err(RunOutcome::FinishedTurn)
    );
    assert_eq!(control.model_calls(), 0);
    assert_eq!(
        control.fail("terminal sync failed".into()),
        RunOutcome::Failed {
            message: "terminal sync failed".into()
        }
    );
    let expired = RunControl::new(RunLimits {
        deadline: Some(Instant::now()),
        max_model_calls: None,
    });
    assert_eq!(
        expired.finish_interval(RunOutcome::FinishedTurn),
        RunOutcome::BudgetExhausted {
            limit: BudgetLimit::Deadline
        }
    );
}

struct AdmissionJournal(std::path::PathBuf);

impl AdmissionJournal {
    fn new() -> Arc<Self> {
        loop {
            let nonce = std::hash::RandomState::new().hash_one(());
            let path = std::env::temp_dir()
                .join(format!("bake-admission-{}-{nonce:x}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Arc::new(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("create owned journal: {error}"),
            }
        }
    }

    fn sync(&self, ordinal: u64) -> Result<(), String> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.0.join(format!("{ordinal}.debit")))
            .map_err(|error| error.to_string())?;
        file.write_all(b"reserved\n")
            .and_then(|()| file.sync_data())
            .map_err(|error| error.to_string())
    }
}

impl Drop for AdmissionJournal {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct InterruptedWrite {
    completed: bool,
    dropped_early: Arc<AtomicBool>,
}

impl Drop for InterruptedWrite {
    fn drop(&mut self) {
        if !self.completed {
            self.dropped_early.store(true, Ordering::SeqCst);
        }
    }
}

struct RequiredWrite {
    journal: Arc<AdmissionJournal>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    dropped_early: Arc<AtomicBool>,
}

impl RequiredWrite {
    fn new() -> Self {
        Self {
            journal: AdmissionJournal::new(),
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            dropped_early: Arc::new(AtomicBool::new(false)),
        }
    }

    fn observer(&self, fail: bool) -> AdmissionObserver {
        let (journal, entered, release, dropped_early) = (
            Arc::clone(&self.journal),
            Arc::clone(&self.entered),
            Arc::clone(&self.release),
            Arc::clone(&self.dropped_early),
        );
        Arc::new(move |admission| {
            let (journal, entered, release, dropped_early) = (
                Arc::clone(&journal),
                Arc::clone(&entered),
                Arc::clone(&release),
                Arc::clone(&dropped_early),
            );
            Box::pin(async move {
                if admission.ordinal != 1 {
                    return Ok(());
                }
                let mut write = InterruptedWrite {
                    completed: false,
                    dropped_early,
                };
                entered.notify_one();
                release.notified().await;
                journal.sync(admission.ordinal)?;
                write.completed = true;
                if fail {
                    Err("debit sync failed".into())
                } else {
                    Ok(())
                }
            })
        })
    }
}

impl Drop for RequiredWrite {
    fn drop(&mut self) {
        self.release.notify_one();
    }
}

async fn poll_once<F: Future + ?Sized>(mut future: Pin<&mut F>) -> Poll<F::Output> {
    std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
}

#[tokio::test]
async fn dropped_admission_waiter_retains_the_required_write_and_blocks_reuse() {
    let write = RequiredWrite::new();
    let control =
        RunControl::with_admission_observer(RunLimits::default(), Some(write.observer(true)));
    let model = model();
    let mut first = Box::pin(control.admit_model_call(ModelCallKind::Agent, &model));
    tokio::select! {
        result = &mut first => panic!("write completed before release: {result:?}"),
        () = write.entered.notified() => {}
    }
    drop(first);
    let dropped_early = write.dropped_early.load(Ordering::SeqCst);
    write.release.notify_one();
    let second = control.admit_model_call(ModelCallKind::Agent, &model).await;
    assert!(
        !dropped_early,
        "dropping a waiter dropped the mandatory write"
    );
    assert_eq!(
        second,
        Err(RunOutcome::Failed {
            message: "debit sync failed".into()
        })
    );
    assert_eq!(control.model_calls(), 1);
    assert_eq!(
        std::fs::read(write.journal.0.join("1.debit")).unwrap(),
        b"reserved\n"
    );
}

#[tokio::test]
async fn shutdown_drains_required_admission_before_idle_and_sibling_dispatch() {
    let write = RequiredWrite::new();
    let control =
        RunControl::with_admission_observer(RunLimits::default(), Some(write.observer(true)));
    let dispatches = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&dispatches);
    let stream = scripted(move |_, _, _| {
        count.fetch_add(1, Ordering::SeqCst);
        assistant_text("done")
    });
    let owner = agent(Arc::clone(&stream));
    let sibling = agent(stream);
    let mut prompt = Box::pin(owner.prompt_with_control("first", control.clone()));
    tokio::select! {
        result = &mut prompt => panic!("write completed before release: {result:?}"),
        () = write.entered.notified() => {}
    }
    let mut shutdown = Box::pin(owner.shutdown());
    let shutdown_waited = poll_once(shutdown.as_mut()).await.is_pending();
    let stopped_before_release = control.stopped();
    let mut next = Box::pin(sibling.prompt_with_control("second", control.clone()));
    let sibling_waited = poll_once(next.as_mut()).await.is_pending();
    write.release.notify_one();
    let ((), first, second) = tokio::join!(shutdown, prompt, next);
    assert!(shutdown_waited && sibling_waited);
    assert_eq!(stopped_before_release, Some(RunOutcome::Cancelled));
    let failed = RunOutcome::Failed {
        message: "debit sync failed".into(),
    };
    assert_eq!(first.unwrap(), failed);
    assert_eq!(second.unwrap(), failed);
    assert_eq!(control.stopped(), Some(failed));
    assert!(!write.dropped_early.load(Ordering::SeqCst));
    assert_eq!(dispatches.load(Ordering::SeqCst), 0);
    assert_eq!(control.model_calls(), 1);
    assert_eq!(
        std::fs::read(write.journal.0.join("1.debit")).unwrap(),
        b"reserved\n"
    );
}

#[tokio::test]
async fn drain_takes_over_a_parked_admission_waiter() {
    for fail in [false, true] {
        let write = RequiredWrite::new();
        let control =
            RunControl::with_admission_observer(RunLimits::default(), Some(write.observer(fail)));
        let model = model();
        let mut first = Box::pin(control.admit_model_call(ModelCallKind::Summary, &model));
        tokio::select! {
            result = &mut first => panic!("write completed before release: {result:?}"),
            () = write.entered.notified() => {}
        }
        control.cancel();
        let mut drain = Box::pin(control.drain_admissions());
        assert!(poll_once(drain.as_mut()).await.is_pending());
        write.release.notify_one();
        let expected = if fail {
            RunOutcome::Failed {
                message: "debit sync failed".into(),
            }
        } else {
            RunOutcome::Cancelled
        };
        // `first` remains alive but unpolled throughout the drain.
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(30), drain)
                .await
                .unwrap(),
            Some(expected.clone())
        );
        assert_eq!(first.await, Err(expected));
        assert!(!write.dropped_early.load(Ordering::SeqCst));
        assert_eq!(
            std::fs::read(write.journal.0.join("1.debit")).unwrap(),
            b"reserved\n"
        );
    }
}

#[tokio::test]
async fn dropping_the_last_driver_does_not_lose_another_waiters_wakeup() {
    let write = RequiredWrite::new();
    let control =
        RunControl::with_admission_observer(RunLimits::default(), Some(write.observer(false)));
    let first_control = control.clone();
    let mut first = tokio::spawn(async move {
        first_control
            .admit_model_call(ModelCallKind::Agent, &model())
            .await
    });
    tokio::select! {
        result = &mut first => panic!("write completed before release: {result:?}"),
        () = write.entered.notified() => {}
    }
    let mut drain = Box::pin(control.drain_admissions());
    let waiting = poll_once(drain.as_mut()).await.is_pending();
    drop(drain);
    write.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(30), &mut first).await;
    if result.is_err() {
        first.abort();
        let _ = first.await;
        panic!("dropping the most recent driver stranded the original waiter");
    }
    assert!(waiting);
    assert_eq!(result.unwrap().unwrap().unwrap().ordinal, 1);
    assert_eq!(control.stopped(), None);
}

#[tokio::test]
async fn observer_construction_and_poll_panics_are_required_failures() {
    for construct in [false, true] {
        let observer: AdmissionObserver = Arc::new(move |_| {
            assert!(!construct, "admission construction panic");
            Box::pin(async { panic!("admission poll panic") })
        });
        let control = RunControl::with_admission_observer(RunLimits::default(), Some(observer));
        let failure = control
            .admit_model_call(ModelCallKind::Agent, &model())
            .await
            .unwrap_err();
        let expected = if construct {
            "admission construction panic"
        } else {
            "admission poll panic"
        };
        assert!(matches!(&failure, RunOutcome::Failed { message } if message.contains(expected)));
        control.cancel();
        assert_eq!(control.drain_admissions().await, Some(failure.clone()));
        assert_eq!(
            control
                .admit_model_call(ModelCallKind::Agent, &model())
                .await,
            Err(failure)
        );
        assert_eq!(control.model_calls(), 1);
    }
}

#[tokio::test]
async fn speculative_admission_preserves_the_foreground_reserve_without_stopping() {
    let control = limited(3);
    assert!(control.has_model_call_allowance(3));
    assert!(!control.has_model_call_allowance(4));
    let summary = control
        .try_admit_model_call(ModelCallKind::Summary, &model(), 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.ordinal, 1);
    let summary = control
        .try_admit_model_call(ModelCallKind::Summary, &model(), 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.ordinal, 2);
    assert_eq!(
        control
            .try_admit_model_call(ModelCallKind::Summary, &model(), 1)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        control
            .try_admit_model_call(ModelCallKind::Summary, &model(), u64::MAX)
            .await
            .unwrap(),
        None
    );
    assert_eq!(control.stopped(), None);
    assert_eq!(control.model_calls(), 2);
    assert_eq!(
        control
            .admit_model_call(ModelCallKind::Agent, &model())
            .await
            .unwrap()
            .ordinal,
        3
    );
    assert!(!control.has_model_call_allowance(1));
    assert_eq!(control.stopped(), None);
}

#[tokio::test]
async fn speculative_admission_waits_for_required_write_and_keeps_the_reserve() {
    let write = RequiredWrite::new();
    let control = RunControl::with_admission_observer(
        RunLimits {
            max_model_calls: Some(2),
            ..RunLimits::default()
        },
        Some(write.observer(false)),
    );
    let model = model();
    let mut first = Box::pin(control.try_admit_model_call(ModelCallKind::Summary, &model, 1));
    tokio::select! {
        result = &mut first => panic!("write completed before release: {result:?}"),
        () = write.entered.notified() => {}
    }
    let mut second = Box::pin(control.try_admit_model_call(ModelCallKind::Summary, &model, 1));
    assert!(poll_once(second.as_mut()).await.is_pending());
    assert_eq!(control.model_calls(), 1);
    write.release.notify_one();
    let (first, second) = tokio::join!(first, second);
    assert_eq!(first.unwrap().unwrap().ordinal, 1);
    assert_eq!(second.unwrap(), None);
    assert_eq!(control.stopped(), None);
    assert_eq!(
        control
            .admit_model_call(ModelCallKind::Agent, &model)
            .await
            .unwrap()
            .ordinal,
        2
    );
}

#[tokio::test]
async fn dropping_an_agent_keeps_its_required_write_alive_until_cleanup() {
    let write = RequiredWrite::new();
    let control =
        RunControl::with_admission_observer(RunLimits::default(), Some(write.observer(false)));
    let dispatches = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&dispatches);
    let owner = agent(scripted(move |_, _, _| {
        count.fetch_add(1, Ordering::SeqCst);
        assistant_text("done")
    }));
    let mut prompt = Box::pin(owner.prompt_with_control("go", control.clone()));
    tokio::select! {
        result = &mut prompt => panic!("write completed before release: {result:?}"),
        () = write.entered.notified() => {}
    }
    drop(prompt);
    drop(owner);
    write.release.notify_one();
    assert_eq!(
        control.drain_admissions().await,
        Some(RunOutcome::Cancelled)
    );
    assert!(!write.dropped_early.load(Ordering::SeqCst));
    assert_eq!(dispatches.load(Ordering::SeqCst), 0);
    assert_eq!(
        std::fs::read(write.journal.0.join("1.debit")).unwrap(),
        b"reserved\n"
    );
}

#[tokio::test]
async fn dropped_agent_aborts_stubborn_tools_after_a_shared_admission_drains() {
    struct ToolDrop(Arc<Notify>);
    impl Drop for ToolDrop {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (ready, gate) = (Arc::clone(&entered), Arc::clone(&release));
    let observer: AdmissionObserver = Arc::new(move |admission| {
        let (ready, gate) = (Arc::clone(&ready), Arc::clone(&gate));
        Box::pin(async move {
            if admission.ordinal == 2 {
                ready.notify_one();
                gate.notified().await;
            }
            Ok(())
        })
    });
    let control = RunControl::with_admission_observer(RunLimits::default(), Some(observer));
    let tool_started = Arc::new(Notify::new());
    let tool_dropped = Arc::new(Notify::new());
    let owner = agent(tools_then_done(vec![call("wait", "stubborn", json!({}))]));
    let (started, dropped) = (Arc::clone(&tool_started), Arc::clone(&tool_dropped));
    owner.set_tools(vec![Arc::new(AgentTool::new(
        "stubborn",
        "stubborn",
        "stubborn",
        empty_schema(),
        move |_, _, _, _, _ctx| {
            let (started, guard) = (Arc::clone(&started), ToolDrop(Arc::clone(&dropped)));
            Box::pin(async move {
                let _guard = guard;
                started.notify_one();
                std::future::pending::<()>().await;
                Ok(AgentToolResult::text("never"))
            })
        },
    ))]);
    let mut prompt = Box::pin(owner.prompt_with_control("go", control.clone()));
    tokio::select! {
        result = &mut prompt => panic!("tool completed unexpectedly: {result:?}"),
        () = tool_started.notified() => {}
    }
    let model = model();
    let mut summary = Box::pin(control.admit_model_call(ModelCallKind::Summary, &model));
    tokio::select! {
        result = &mut summary => panic!("debit completed before release: {result:?}"),
        () = entered.notified() => {}
    }
    drop(prompt);
    drop(owner);
    release.notify_one();
    assert_eq!(
        control.drain_admissions().await,
        Some(RunOutcome::Cancelled)
    );
    assert_eq!(summary.await, Err(RunOutcome::Cancelled));
    tokio::time::timeout(Duration::from_secs(30), tool_dropped.notified())
        .await
        .expect("deferred abort must drop the signal-ignoring tool");
}

#[tokio::test]
async fn shutdown_waits_for_a_blocking_journal_sync_and_its_acknowledgement() {
    use std::sync::Condvar;
    use tokio::sync::oneshot;

    struct BlockingWrite {
        gate: Arc<(Mutex<bool>, Condvar)>,
        worker: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
    }
    impl BlockingWrite {
        fn release(&self) {
            *self.gate.0.lock().unwrap() = true;
            self.gate.1.notify_all();
        }
    }
    impl Drop for BlockingWrite {
        fn drop(&mut self) {
            self.release();
            if let Some(worker) = self.worker.lock().unwrap().take() {
                let _ = worker.join();
            }
        }
    }

    let blocking = BlockingWrite {
        gate: Arc::new((Mutex::new(false), Condvar::new())),
        worker: Arc::new(Mutex::new(None)),
    };
    let journal = AdmissionJournal::new();
    let entered = Arc::new(Notify::new());
    let synced = Arc::new(AtomicBool::new(false));
    let dropped_early = Arc::new(AtomicBool::new(false));
    let (gate, worker, destination, ready, finished, early) = (
        Arc::clone(&blocking.gate),
        Arc::clone(&blocking.worker),
        Arc::clone(&journal),
        Arc::clone(&entered),
        Arc::clone(&synced),
        Arc::clone(&dropped_early),
    );
    let observer: AdmissionObserver = Arc::new(move |_| {
        let (gate, destination, ready, finished) = (
            Arc::clone(&gate),
            Arc::clone(&destination),
            Arc::clone(&ready),
            Arc::clone(&finished),
        );
        let (done, completion) = oneshot::channel();
        let mut owned = worker.lock().unwrap();
        assert!(owned.is_none());
        *owned = Some(std::thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(destination.0.join("1.debit"))
                    .map_err(|error| error.to_string())?;
                file.write_all(b"reserved\n")
                    .map_err(|error| error.to_string())?;
                ready.notify_one();
                let released = gate
                    .1
                    .wait_while(gate.0.lock().unwrap(), |released| !*released)
                    .unwrap();
                drop(released);
                file.sync_data().map_err(|error| error.to_string())?;
                finished.store(true, Ordering::SeqCst);
                Ok(())
            })();
            let _ = done.send(result);
        }));
        let dropped_early = Arc::clone(&early);
        Box::pin(async move {
            let mut write = InterruptedWrite {
                completed: false,
                dropped_early,
            };
            completion.await.map_err(|error| error.to_string())??;
            write.completed = true;
            Err("journal acknowledgement failed".into())
        })
    });
    let control = RunControl::with_admission_observer(RunLimits::default(), Some(observer));
    let dispatches = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&dispatches);
    let owner = agent(scripted(move |_, _, _| {
        count.fetch_add(1, Ordering::SeqCst);
        assistant_text("done")
    }));
    let mut prompt = Box::pin(owner.prompt_with_control("go", control.clone()));
    tokio::select! {
        result = &mut prompt => panic!("write completed before release: {result:?}"),
        () = entered.notified() => {}
    }
    let mut shutdown = Box::pin(owner.shutdown());
    let waiting = poll_once(shutdown.as_mut()).await.is_pending();
    let completed_before_release = synced.load(Ordering::SeqCst);
    blocking.release();
    let ((), outcome) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(shutdown, prompt)
    })
    .await
    .unwrap();
    drop(blocking);
    assert!(waiting && !completed_before_release);
    assert!(synced.load(Ordering::SeqCst));
    assert!(!dropped_early.load(Ordering::SeqCst));
    assert_eq!(
        outcome.unwrap(),
        RunOutcome::Failed {
            message: "journal acknowledgement failed".into()
        }
    );
    assert_eq!(dispatches.load(Ordering::SeqCst), 0);
    assert_eq!(
        std::fs::read(journal.0.join("1.debit")).unwrap(),
        b"reserved\n"
    );
}

#[tokio::test]
async fn zero_allowance_emits_no_synthetic_assistant_or_provider_call() {
    let agent = agent(Arc::new(|_, _, _| {
        panic!("denied request reached provider")
    }));
    let ended = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&ended);
    let _subscription = agent.subscribe(move |event, _| {
        if matches!(event, AgentEvent::AgentEnd { .. }) {
            count.fetch_add(1, Ordering::SeqCst);
        }
        Box::pin(async {})
    });
    let control = limited(0);
    assert_eq!(
        agent
            .prompt_with_control("go", control.clone())
            .await
            .unwrap(),
        RunOutcome::BudgetExhausted {
            limit: BudgetLimit::ModelCalls
        }
    );
    assert_eq!(control.model_calls(), 0);
    assert_eq!(ended.load(Ordering::SeqCst), 1);
    assert!(
        !agent
            .messages()
            .iter()
            .any(|message| message.as_assistant().is_some())
    );
    assert!(!agent.is_streaming());
}

#[tokio::test]
async fn tool_loop_stops_before_request_n_plus_one() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let agent = agent(scripted(move |index, _, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        tool_use(vec![call(
            &index.to_string(),
            "echo",
            json!({"value":"ok"}),
        )])
    }));
    let executed = Arc::new(Mutex::new(Vec::new()));
    agent.set_tools(vec![Arc::new(echo_tool(Arc::clone(&executed)))]);
    let control = limited(3);
    assert_eq!(
        agent
            .prompt_with_control("go", control.clone())
            .await
            .unwrap(),
        RunOutcome::BudgetExhausted {
            limit: BudgetLimit::ModelCalls
        }
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(executed.lock().unwrap().len(), 3);
    assert_eq!(control.model_calls(), 3);
    assert_eq!(
        agent
            .messages()
            .iter()
            .filter(|message| message.as_assistant().is_some())
            .count(),
        3
    );
}

#[tokio::test]
async fn subsequent_prompt_shares_allowance_and_ignores_stale_success() {
    let agent = agent(scripted(|_, _, _| assistant_text("done")));
    let control = limited(1);
    assert_eq!(
        agent
            .prompt_with_control("first", control.clone())
            .await
            .unwrap(),
        RunOutcome::FinishedTurn
    );
    assert_eq!(
        agent
            .prompt_with_control("second", control.clone())
            .await
            .unwrap(),
        RunOutcome::BudgetExhausted {
            limit: BudgetLimit::ModelCalls
        }
    );
    assert_eq!(control.model_calls(), 1);
    assert_eq!(
        agent
            .messages()
            .iter()
            .filter(|message| message.as_assistant().is_some())
            .count(),
        1
    );
}

#[tokio::test]
async fn provider_failure_is_an_explicit_outcome() {
    let agent = agent(scripted(|_, _, _| failed(StopReason::Error)));
    assert_eq!(
        agent
            .prompt_with_control("go", RunControl::default())
            .await
            .unwrap(),
        RunOutcome::Failed {
            message: "error".into()
        }
    );
}

#[tokio::test(start_paused = true)]
async fn deadline_signals_tool_and_awaits_its_cleanup() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let cleaned = Arc::new(AtomicBool::new(false));
    let (e, r, c) = (
        Arc::clone(&entered),
        Arc::clone(&release),
        Arc::clone(&cleaned),
    );
    let agent = Arc::new(agent(tools_then_done(vec![call("1", "wait", json!({}))])));
    agent.set_tools(vec![Arc::new(AgentTool::new(
        "wait",
        "wait",
        "wait",
        empty_schema(),
        move |_, _, signal, _, _ctx| {
            let (e, r, c) = (Arc::clone(&e), Arc::clone(&r), Arc::clone(&c));
            Box::pin(async move {
                e.notify_one();
                signal.unwrap().cancelled().await;
                r.notified().await;
                c.store(true, Ordering::SeqCst);
                Ok(AgentToolResult::text("cleaned"))
            })
        },
    ))]);
    let control = RunControl::new(RunLimits {
        deadline: Some(Instant::now() + Duration::from_secs(1)),
        ..RunLimits::default()
    });
    let owner = Arc::clone(&agent);
    let prompt = tokio::spawn(async move { owner.prompt_with_control("go", control).await });
    entered.notified().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert!(!prompt.is_finished());
    release.notify_one();
    assert_eq!(
        prompt.await.unwrap().unwrap(),
        RunOutcome::BudgetExhausted {
            limit: BudgetLimit::Deadline
        }
    );
    assert!(cleaned.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn deadline_awaits_provider_after_its_terminal_event() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let terminal = Arc::new(Notify::new());
    let (e, r, t) = (
        Arc::clone(&entered),
        Arc::clone(&release),
        Arc::clone(&terminal),
    );
    let agent = Arc::new(agent(Arc::new(move |_, _, options| {
        let (sender, stream) = assistant_message_channel();
        let (e, r, t) = (Arc::clone(&e), Arc::clone(&r), Arc::clone(&t));
        tokio::spawn(async move {
            e.notify_one();
            options.base.signal.unwrap().cancelled().await;
            sender.finish(failed(StopReason::Aborted));
            t.notify_one();
            r.notified().await;
            drop(sender);
        });
        Ok(stream)
    })));
    let control = RunControl::new(RunLimits {
        deadline: Some(Instant::now() + Duration::from_secs(1)),
        ..RunLimits::default()
    });
    let owner = Arc::clone(&agent);
    let prompt = tokio::spawn(async move { owner.prompt_with_control("go", control).await });
    entered.notified().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    terminal.notified().await;
    assert!(!prompt.is_finished());
    release.notify_one();
    assert_eq!(
        prompt.await.unwrap().unwrap(),
        RunOutcome::BudgetExhausted {
            limit: BudgetLimit::Deadline
        }
    );
}

#[tokio::test]
async fn mandatory_admission_failure_outranks_raced_cancel_and_keeps_debit() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (e, r) = (Arc::clone(&entered), Arc::clone(&release));
    let observer: AdmissionObserver = Arc::new(move |_| {
        let (e, r) = (Arc::clone(&e), Arc::clone(&r));
        Box::pin(async move {
            e.notify_one();
            r.notified().await;
            Err("debit sync failed".into())
        })
    });
    let control = RunControl::with_admission_observer(RunLimits::default(), Some(observer));
    let c = control.clone();
    let admission =
        tokio::spawn(async move { c.admit_model_call(ModelCallKind::Agent, &model()).await });
    entered.notified().await;
    control.cancel();
    assert!(!admission.is_finished());
    release.notify_one();
    assert_eq!(
        admission.await.unwrap().unwrap_err(),
        RunOutcome::Failed {
            message: "debit sync failed".into()
        }
    );
    assert_eq!(control.model_calls(), 1);
}

#[tokio::test]
async fn failed_provider_message_with_a_tool_call_remains_failed_without_execution() {
    let executed = Arc::new(Mutex::new(Vec::new()));
    let agent = agent(scripted(|_, _, _| {
        let mut message = tool_use(vec![call("1", "echo", json!({"value":"must not execute"}))]);
        message.stop_reason = StopReason::Error;
        message.error_message = Some("truncated tool arguments".into());
        message
    }));
    agent.set_tools(vec![Arc::new(echo_tool(Arc::clone(&executed)))]);
    assert_eq!(
        agent
            .prompt_with_control("go", RunControl::default())
            .await
            .unwrap(),
        RunOutcome::Failed {
            message: "truncated tool arguments".into()
        }
    );
    assert!(executed.lock().unwrap().is_empty());
}

#[test]
fn settlement_keeps_the_first_stop_except_for_required_write_failure() {
    let control = RunControl::default();
    control.cancel();
    assert_eq!(
        control.settle(RunOutcome::Failed {
            message: "late provider error".into()
        }),
        RunOutcome::Cancelled
    );
    assert_eq!(
        control.fail("required write failed".into()),
        RunOutcome::Failed {
            message: "required write failed".into()
        }
    );
    assert_eq!(
        control.settle(RunOutcome::FinishedTurn),
        RunOutcome::Failed {
            message: "required write failed".into()
        }
    );
}
