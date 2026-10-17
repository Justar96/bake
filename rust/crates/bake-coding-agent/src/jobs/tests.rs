//! The registry's contract, over the production gate. Cases adapted from
//! Bake's TypeScript `packages/jobs/jobs-local/tests/jobs.spec.ts` name the
//! case they follow; the rest are Bake's own for what the Rust ownership
//! adds (completing, resource settlement, held reports, retention). Every
//! barrier is a channel or a hand-polled future; every bounded wait is for
//! a state the test already caused, so the bound only stops a broken
//! registry from hanging the suite.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::task::{Context, Poll};

use bake_ai::AbortController;
use tokio::sync::oneshot;

use super::*;

const BOUND: Duration = Duration::from_secs(10);

async fn bounded<F: Future>(what: &str, future: F) -> F::Output {
    tokio::time::timeout(BOUND, future)
        .await
        .unwrap_or_else(|_| panic!("{what} did not finish"))
}

fn poll_once<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
    let mut cx = Context::from_waker(std::task::Waker::noop());
    future.poll(&mut cx)
}

fn spec(kind: &'static str, output: OutputMode) -> JobSpec {
    JobSpec {
        kind,
        label: format!("{kind} job"),
        output,
    }
}

fn registry(limits: JobLimits) -> (WorkGate, JobRegistry) {
    let gate = WorkGate::default();
    let registry = JobRegistry::new(gate.clone(), limits);
    (gate, registry)
}

/// Waits until `id` is terminal, without claiming its report.
async fn settled(registry: &JobRegistry, id: JobId) -> JobSnapshot {
    bounded("the settlement", async {
        loop {
            let mut notified = pin!(registry.inner.settled.notified());
            notified.as_mut().enable();
            let snapshot = registry.get(id).expect("kept");
            if snapshot.status.is_terminal() {
                return snapshot;
            }
            notified.await;
        }
    })
    .await
}

/// Waits until `id` has `status`.
async fn reaches(registry: &JobRegistry, id: JobId, status: JobStatus) {
    bounded("the status", async {
        while registry.get(id).expect("kept").status != status {
            tokio::task::yield_now().await;
        }
    })
    .await;
}

/// A producer held at a barrier: it reports its signal when it starts,
/// then returns the outcome it is released with.
struct Held {
    id: JobId,
    started: oneshot::Receiver<AbortSignal>,
    release: oneshot::Sender<JobOutcome>,
}

fn start_held(registry: &JobRegistry, kind: &'static str) -> Held {
    let (started_tx, started) = oneshot::channel();
    let (release, released) = oneshot::channel::<JobOutcome>();
    let id = registry
        .start(spec(kind, OutputMode::Final), move |context| async move {
            let _ = started_tx.send(context.signal().clone());
            released
                .await
                .unwrap_or_else(|_| JobOutcome::failed("never released"))
        })
        .expect("admitted");
    Held {
        id,
        started,
        release,
    }
}

/// Sets its flag when dropped: the end of a resource's cleanup.
struct Cleanup(Arc<AtomicBool>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A producer that leaves a resource in its job's gate and returns
/// `Completed` at once. The resource sees the job's cancellation, then
/// holds its cleanup until released.
struct Resource {
    id: JobId,
    cancelled: oneshot::Receiver<()>,
    release: oneshot::Sender<()>,
    cleaned: Arc<AtomicBool>,
}

fn start_with_resource(registry: &JobRegistry) -> Resource {
    let cleaned = Arc::new(AtomicBool::new(false));
    let (saw_cancel, cancelled) = oneshot::channel();
    let (release, released) = oneshot::channel::<()>();
    let flag = Arc::clone(&cleaned);
    let id = registry
        .start(spec("res", OutputMode::Final), move |context| async move {
            drop(
                context
                    .scope()
                    .permit()
                    .spawn(move |scope| async move {
                        let _cleanup = Cleanup(flag);
                        scope.signal().cancelled().await;
                        let _ = saw_cancel.send(());
                        let _ = released.await;
                    })
                    .expect("the resource is admitted"),
            );
            JobOutcome::completed("exit code: 0")
        })
        .expect("admitted");
    Resource {
        id,
        cancelled,
        release,
        cleaned,
    }
}

// jobs.spec.ts: "issues kind-prefixed ids from per-kind counters"; ids are
// parsed only as this registry issued them.
#[tokio::test]
async fn ids_are_numbered_per_kind_and_parsed_strictly() {
    let (_gate, registry) = registry(JobLimits::default());
    let a = start_held(&registry, "bash");
    let b = start_held(&registry, "bash");
    let c = start_held(&registry, "subagent");
    assert_eq!(
        [a.id.to_string(), b.id.to_string(), c.id.to_string()],
        ["bash-1", "bash-2", "subagent-1"]
    );
    assert_eq!((b.id.kind(), b.id.ordinal()), ("bash", 2));
    assert_eq!(registry.id("bash-2"), Ok(b.id));
    for text in [
        "bash-3",
        "bash-0",
        "bash-02",
        "bash-+1",
        "bash-",
        "bash",
        "-1",
        "other-1",
        "bash-1x",
        "bash-99999999999999999999999",
    ] {
        assert!(
            matches!(registry.id(text), Err(JobError::Unknown(_))),
            "{text} parsed"
        );
    }
    let long = "x".repeat(500);
    let Err(JobError::Unknown(echoed)) = registry.id(&long) else {
        panic!("parsed");
    };
    assert_eq!(echoed.len(), 64, "the error text is bounded");
    drop((a, b, c));
    bounded("the close", registry.close()).await;
}

// jobs.spec.ts: "rejects an empty kind, empty label, and invalid output
// limit": refused before the producer runs, with no id used.
#[tokio::test]
async fn invalid_specs_are_refused_before_the_producer_runs() {
    let (_gate, registry) = registry(JobLimits::default());
    let ran = Arc::new(AtomicBool::new(false));
    let attempt = |kind: &'static str, label: &str| {
        let ran = Arc::clone(&ran);
        registry.start(
            JobSpec {
                kind,
                label: label.to_owned(),
                output: OutputMode::Final,
            },
            move |_| {
                ran.store(true, Ordering::SeqCst);
                async { JobOutcome::completed("") }
            },
        )
    };
    let too_long: &'static str = "a234567890123456789012345678901234";
    for kind in ["", "Bash", "a-b", "1a", "a b", too_long] {
        assert_eq!(
            attempt(kind, "label"),
            Err(StartError::InvalidKind),
            "{kind:?}"
        );
    }
    for label in ["", "  \n"] {
        assert_eq!(attempt("bash", label), Err(StartError::InvalidLabel));
    }
    tokio::task::yield_now().await;
    assert!(!ran.load(Ordering::SeqCst), "a refused producer ran");
    assert!(registry.list().is_empty());
    let label = "é".repeat(MAX_TEXT_BYTES);
    let id = registry
        .start(
            JobSpec {
                kind: "bash",
                label,
                output: OutputMode::Final,
            },
            |_| async { JobOutcome::completed("") },
        )
        .expect("admitted");
    assert_eq!(id.to_string(), "bash-1", "a refusal used no id");
    let kept = registry.get(id).expect("kept").label;
    assert_eq!(kept.len(), MAX_TEXT_BYTES, "cut at a character boundary");
    bounded("the close", registry.close()).await;
}

// jobs.spec.ts: "rejects before producer start and id allocation, then
// admits immediately after settlement" and "keeps a stopping job in the
// bucket until producer settlement"; Bake adds that a completing job,
// whose resources are still settling, keeps its place too.
#[tokio::test]
async fn the_limit_counts_running_stopping_and_completing_jobs() {
    let (_gate, registry) = registry(JobLimits {
        max_active: 2,
        ..JobLimits::default()
    });
    let held = start_held(&registry, "bash");
    let resource = start_with_resource(&registry);
    let ran = Arc::new(AtomicBool::new(false));
    let refused = |registry: &JobRegistry| {
        let ran = Arc::clone(&ran);
        registry.start(spec("bash", OutputMode::Final), move |_| {
            ran.store(true, Ordering::SeqCst);
            async { JobOutcome::completed("") }
        })
    };
    assert_eq!(
        refused(&registry),
        Err(StartError::LimitReached { limit: 2 })
    );
    assert_eq!(
        StartError::LimitReached { limit: 2 }.to_string(),
        "background job limit reached for this session (limit: 2); stop an unneeded job or wait for one to finish, then retry"
    );

    let signal = bounded("the start", held.started).await.expect("started");
    assert_eq!(registry.kill(held.id, None), Ok(KillResult::Requested));
    assert!(signal.aborted());
    assert_eq!(
        refused(&registry),
        Err(StartError::LimitReached { limit: 2 })
    );
    held.release
        .send(JobOutcome::killed("signal: SIGTERM"))
        .expect("held");
    assert_eq!(settled(&registry, held.id).await.status, JobStatus::Killed);

    reaches(&registry, resource.id, JobStatus::Completing).await;
    let other = start_held(&registry, "other");
    assert_eq!(
        refused(&registry),
        Err(StartError::LimitReached { limit: 2 })
    );
    tokio::task::yield_now().await;
    assert!(!ran.load(Ordering::SeqCst), "a refused producer ran");
    bounded("the resource's cancel", resource.cancelled)
        .await
        .expect("cancelled");
    resource.release.send(()).expect("held");
    assert_eq!(
        settled(&registry, resource.id).await.status,
        JobStatus::Completed
    );
    let admitted = refused(&registry).expect("admitted once a place is free");
    assert_eq!(admitted.to_string(), "bash-2", "no refusal used an id");
    drop(other.release);
    bounded("the close", registry.close()).await;
}

// Admission is atomic: threads racing for the last places admit exactly
// the limit, with distinct ids.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_racing_from_many_threads_admits_exactly_the_limit() {
    let (_gate, registry) = registry(JobLimits {
        max_active: 3,
        ..JobLimits::default()
    });
    let handle = tokio::runtime::Handle::current();
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let registry = registry.clone();
            let handle = handle.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let _runtime = handle.enter();
                barrier.wait();
                registry.start(spec("bash", OutputMode::Final), |context| async move {
                    context.signal().cancelled().await;
                    JobOutcome::killed("stopped")
                })
            })
        })
        .collect();
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().expect("joined"))
        .collect();
    let mut admitted: Vec<String> = results
        .iter()
        .filter_map(|result| result.as_ref().ok().map(ToString::to_string))
        .collect();
    admitted.sort();
    assert_eq!(admitted, ["bash-1", "bash-2", "bash-3"]);
    assert!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .all(|error| *error == StartError::LimitReached { limit: 3 })
    );
    bounded("the close", registry.close()).await;
    assert!(
        registry
            .list()
            .iter()
            .all(|job| job.status == JobStatus::Killed)
    );
}

// jobs.spec.ts: "stream kinds read a consuming delta; terminal reads mark
// reported" and "final-output kinds read empty while live, the outcome
// output idempotently once settled".
#[tokio::test]
async fn a_stream_read_consumes_and_a_final_read_repeats() {
    let (_gate, registry) = registry(JobLimits::default());
    let (more_tx, more) = oneshot::channel::<()>();
    let (wrote_tx, wrote) = oneshot::channel::<()>();
    let stream = registry
        .start(
            spec("bash", OutputMode::Stream),
            move |context| async move {
                let output = context.output().expect("a stream job").clone();
                output.append(b"one\n");
                let _ = wrote_tx.send(());
                let _ = more.await;
                output.append(b"two\n");
                JobOutcome::completed("exit code: 0").with_output("ignored for a stream")
            },
        )
        .expect("admitted");
    bounded("the first write", wrote).await.expect("wrote");
    let first = registry.read(stream).expect("read");
    assert_eq!(first.text, "one\n");
    assert!(!first.snapshot.reported, "a live read claims nothing");
    assert_eq!(registry.read(stream).expect("read").text, "", "consumed");
    more_tx.send(()).expect("held");
    settled(&registry, stream).await;
    let last = registry.read(stream).expect("read");
    assert_eq!(last.text, "two\n");
    assert!(last.snapshot.reported, "a terminal read claims the report");
    assert_eq!(registry.read(stream).expect("read").text, "");

    let held = start_held(&registry, "subagent");
    let live = registry.read(held.id).expect("read");
    assert_eq!((live.text.as_str(), live.snapshot.reported), ("", false));
    held.release
        .send(JobOutcome::completed("max-tokens").with_output("the answer"))
        .expect("held");
    settled(&registry, held.id).await;
    for _ in 0..2 {
        let read = registry.read(held.id).expect("read");
        assert_eq!(read.text, "the answer");
        assert_eq!(read.snapshot.detail.as_deref(), Some("max-tokens"));
        assert!(read.snapshot.reported);
    }
    bounded("the close", registry.close()).await;
}

// Bounded retention of streamed and final output: the newest bytes are
// kept, the dropped ones counted, and no read starts or ends inside a
// character.
#[test]
fn output_is_bounded_and_split_only_between_characters() {
    let mut buffer = StreamBuffer::new(8);
    assert!(buffer.append(b"abcdefghij"));
    assert_eq!(buffer.take(false), ("cdefghij".to_owned(), 2));
    assert_eq!(buffer.take(false), (String::new(), 0));

    // A character split across appends waits for its end.
    assert!(buffer.append(&[b'x', 0xC3]));
    assert_eq!(buffer.take(false), ("x".to_owned(), 0));
    assert!(buffer.append(&[0xA9]));
    assert_eq!(buffer.take(false), ("é".to_owned(), 0));

    // The bound cuts inside `é`; its rest is dropped too.
    let mut buffer = StreamBuffer::new(4);
    assert!(buffer.append("aé€".as_bytes()));
    assert_eq!(buffer.take(false), ("€".to_owned(), 3));

    // One append larger than the bound keeps only its tail.
    let mut buffer = StreamBuffer::new(4);
    assert!(buffer.append(b"12"));
    assert!(buffer.append(b"abcdefgh"));
    assert_eq!(buffer.take(false), ("efgh".to_owned(), 6));

    // A terminal take flushes an incomplete character as U+FFFD; a closed
    // buffer keeps nothing more.
    assert!(buffer.append(&[0xE2, 0x82]));
    assert_eq!(buffer.take(true), ("\u{FFFD}".to_owned(), 0));
    buffer.closed = true;
    assert!(!buffer.append(b"late"));
    assert_eq!(buffer.take(true), (String::new(), 0));

    assert_eq!(tail("aé€".to_owned(), 4), ("€".to_owned(), 3));
    assert_eq!(tail("abc".to_owned(), 4), ("abc".to_owned(), 0));
    assert_eq!(cut("aé".to_owned(), 2), "a");
}

#[tokio::test]
async fn final_output_keeps_its_tail_and_counts_the_rest() {
    let (_gate, registry) = registry(JobLimits {
        final_output_bytes: 4,
        ..JobLimits::default()
    });
    let id = registry
        .start(spec("subagent", OutputMode::Final), |_| async {
            JobOutcome::completed("done").with_output("aé€")
        })
        .expect("admitted");
    settled(&registry, id).await;
    let read = registry.read(id).expect("read");
    assert_eq!((read.text.as_str(), read.omitted_bytes), ("€", 3));
    bounded("the close", registry.close()).await;
}

// Snapshots are values: a later change does not reach one already taken.
#[tokio::test]
async fn snapshots_are_fresh_copies() {
    let (_gate, registry) = registry(JobLimits::default());
    let held = start_held(&registry, "bash");
    let before = registry.get(held.id).expect("kept");
    let listed = registry.list();
    registry.kill(held.id, None).expect("killed");
    assert_eq!(before.status, JobStatus::Running);
    assert!(!before.reported);
    assert_eq!(listed[0], before);
    assert_eq!(
        registry.get(held.id).expect("kept").status,
        JobStatus::Stopping
    );
    drop(held.release);
    bounded("the close", registry.close()).await;
}

// jobs.spec.ts: "cancels a live job with the forwarded reason and
// suppresses the notice" and "reports an already-finished job instead of
// failing"; the request is idempotent and keeps the first reason.
#[tokio::test]
async fn a_kill_is_an_idempotent_request() {
    let (_gate, registry) = registry(JobLimits::default());
    let held = start_held(&registry, "bash");
    let signal = bounded("the start", held.started).await.expect("started");
    assert_eq!(
        registry.kill(held.id, Some("x".repeat(2 * MAX_TEXT_BYTES))),
        Ok(KillResult::Requested)
    );
    let stopping = registry.get(held.id).expect("kept");
    assert_eq!(stopping.status, JobStatus::Stopping);
    assert!(stopping.reported, "a kill claims the report");
    let Some(StopCause::Kill {
        reason: Some(reason),
    }) = &stopping.stop
    else {
        panic!("no stop recorded");
    };
    assert_eq!(reason.len(), MAX_TEXT_BYTES);
    assert!(signal.aborted(), "the producer sees the request");
    assert_eq!(
        registry.kill(held.id, Some("again".into())),
        Ok(KillResult::AlreadyStopping)
    );
    assert_eq!(registry.get(held.id).expect("kept").stop, stopping.stop);
    held.release
        .send(JobOutcome::killed("signal: SIGTERM"))
        .expect("held");
    let ended = settled(&registry, held.id).await;
    assert_eq!(ended.status, JobStatus::Killed);
    assert!(ended.finished_at.is_some_and(|end| end >= ended.started_at));
    assert_eq!(
        registry.kill(held.id, None),
        Ok(KillResult::AlreadyFinished)
    );
    bounded("the close", registry.close()).await;
}

// The producer's outcome wins over the request: work that finishes as it
// is stopped ends completed, with the stop still recorded; a kill after
// the outcome, while resources settle or once terminal, changes nothing.
#[tokio::test]
async fn completion_before_during_and_after_a_stop() {
    let (_gate, registry) = registry(JobLimits::default());

    // During: the producer ignores the signal and completes.
    let during = start_held(&registry, "bash");
    bounded("the start", during.started).await.expect("started");
    registry.kill(during.id, None).expect("killed");
    during
        .release
        .send(JobOutcome::completed("exit code: 0"))
        .expect("held");
    let ended = settled(&registry, during.id).await;
    assert_eq!(ended.status, JobStatus::Completed);
    assert_eq!(ended.stop, Some(StopCause::Kill { reason: None }));

    // After: terminal first.
    let after = start_held(&registry, "bash");
    after
        .release
        .send(JobOutcome::completed("exit code: 0"))
        .expect("held");
    settled(&registry, after.id).await;
    assert_eq!(
        registry.kill(after.id, None),
        Ok(KillResult::AlreadyFinished)
    );
    assert_eq!(registry.get(after.id).expect("kept").stop, None);

    // While completing: the outcome is decided; the drain continues.
    let resource = start_with_resource(&registry);
    reaches(&registry, resource.id, JobStatus::Completing).await;
    assert_eq!(
        registry.kill(resource.id, None),
        Ok(KillResult::AlreadyStopping)
    );
    resource.release.send(()).expect("held");
    let ended = settled(&registry, resource.id).await;
    assert_eq!((ended.status, ended.stop), (JobStatus::Completed, None));
    bounded("the close", registry.close()).await;
}

// A stop that lands before the job's task first runs: the producer is
// never called. On one thread the driver has not run when `kill` returns.
#[tokio::test]
async fn a_stop_before_the_producer_starts_runs_nothing() {
    let (_gate, registry) = registry(JobLimits::default());
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    let id = registry
        .start(spec("bash", OutputMode::Final), move |_| {
            flag.store(true, Ordering::SeqCst);
            async { JobOutcome::completed("") }
        })
        .expect("admitted");
    assert_eq!(registry.kill(id, None), Ok(KillResult::Requested));
    let ended = settled(&registry, id).await;
    assert_eq!(ended.status, JobStatus::Killed);
    assert_eq!(ended.detail.as_deref(), Some("stopped before it started"));
    assert!(!ran.load(Ordering::SeqCst), "the producer ran");
    bounded("the close", registry.close()).await;
}

// Terminal only once resources settle: the producer has returned, but the
// task it left in the job's gate holds its cleanup, so the job stays
// completing; no wait, read, report, or subscription sees an end until the
// cleanup has run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_is_terminal_only_once_its_resources_settle() {
    let (_gate, registry) = registry(JobLimits::default());
    let mut events = registry.subscribe();
    let resource = start_with_resource(&registry);
    bounded("the resource's cancel", resource.cancelled)
        .await
        .expect("the producer's return cancels what it left");
    reaches(&registry, resource.id, JobStatus::Completing).await;
    let mut wait = Box::pin(registry.wait(resource.id, Duration::from_secs(30), None));
    assert!(poll_once(wait.as_mut()).is_pending());
    let mut changed = Box::pin(events.changed());
    assert!(poll_once(changed.as_mut()).is_pending());
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    let snapshot = registry.get(resource.id).expect("kept");
    assert_eq!(
        snapshot.status,
        JobStatus::Completing,
        "terminal before its resources settled"
    );
    assert!(!resource.cleaned.load(Ordering::SeqCst));
    assert!(poll_once(wait.as_mut()).is_pending());
    assert!(poll_once(changed.as_mut()).is_pending());
    assert!(registry.claim_unreported().is_empty());
    assert_eq!(registry.read(resource.id).expect("read").text, "");

    resource.release.send(()).expect("held");
    let ended = bounded("the wait", wait).await.expect("terminal");
    assert!(
        resource.cleaned.load(Ordering::SeqCst),
        "the cleanup ran first"
    );
    assert_eq!(ended.status, JobStatus::Completed);
    assert!(ended.reported, "the pending wait claimed the report");
    drop(changed);
    bounded("the close", registry.close()).await;
}

// jobs.spec.ts: "rejects a non-positive or non-finite timeout", "returns
// the live snapshot on timeout without marking reported", "unregisters
// timed-out and aborted wait resolvers while the job remains live", "an
// aborted signal rejects the wait only — the job stays alive", and
// "returns immediately for an already-finished job".
#[tokio::test(start_paused = true)]
async fn waits_are_bounded_and_unregister_however_they_end() {
    let (_gate, registry) = registry(JobLimits::default());
    let held = start_held(&registry, "bash");
    let id = held.id;
    let max = registry.limits().max_wait;
    for timeout in [Duration::ZERO, max + Duration::from_millis(1)] {
        assert_eq!(
            registry.wait(id, timeout, None).await,
            Err(JobError::InvalidTimeout { max })
        );
    }

    let live = registry
        .wait(id, Duration::from_millis(50), None)
        .await
        .expect("timed out");
    assert_eq!((live.status, live.reported), (JobStatus::Running, false));
    assert_eq!(registry.waiters(id), 0);

    let abort = AbortController::new();
    let signal = abort.signal();
    let mut cancelled = Box::pin(registry.wait(id, max, Some(&signal)));
    assert!(poll_once(cancelled.as_mut()).is_pending());
    assert_eq!(registry.waiters(id), 1);
    abort.abort();
    assert_eq!(cancelled.await, Err(JobError::WaitCancelled(id)));
    assert_eq!(registry.waiters(id), 0);
    assert_eq!(
        registry.wait(id, max, Some(&signal)).await,
        Err(JobError::WaitCancelled(id)),
        "an already-aborted signal rejects a live wait at once"
    );

    let mut dropped = Box::pin(registry.wait(id, max, None));
    assert!(poll_once(dropped.as_mut()).is_pending());
    assert_eq!(registry.waiters(id), 1);
    drop(dropped);
    assert_eq!(registry.waiters(id), 0);

    let job = registry.get(id).expect("kept");
    assert_eq!((job.status, job.reported), (JobStatus::Running, false));
    let started = held.started.await.expect("started");
    assert!(!started.aborted(), "no wait stopped the job");

    held.release
        .send(JobOutcome::completed("exit code: 0"))
        .expect("held");
    settled(&registry, id).await;
    let ended = registry
        .wait(id, Duration::from_millis(1), Some(&signal))
        .await
        .expect("a terminal job wins over an aborted signal");
    assert!(ended.reported);
    assert_eq!(registry.waiters(id), 0);
    drop(registry.close());
}

// jobs.spec.ts: "resolves with the terminal snapshot when the job
// settles, marked reported", "an abort racing settlement in the same tick
// does not swallow the notice", and "an abort landing after settlement
// still delivers the terminal snapshot it owes".
#[tokio::test]
async fn a_wait_pending_at_settlement_holds_the_report_for_its_caller() {
    let (_gate, registry) = registry(JobLimits::default());
    let mut events = registry.subscribe();
    let held = start_held(&registry, "bash");
    let abort = AbortController::new();
    let signal = abort.signal();
    let mut wait = Box::pin(registry.wait(held.id, Duration::from_secs(30), Some(&signal)));
    assert!(poll_once(wait.as_mut()).is_pending());
    held.release
        .send(JobOutcome::completed("exit code: 0"))
        .expect("held");
    let snapshot = settled(&registry, held.id).await;
    assert!(snapshot.reported, "held for the pending wait");
    assert!(registry.claim_unreported().is_empty());
    assert_eq!(registry.claim_report(held.id), Ok(false));
    let mut changed = Box::pin(events.changed());
    assert!(
        poll_once(changed.as_mut()).is_pending(),
        "a held report is not announced"
    );
    abort.abort();
    let ended = bounded("the wait", wait).await.expect("owed");
    assert_eq!(ended.status, JobStatus::Completed);
    assert!(ended.reported);
    assert_eq!(registry.waiters(held.id), 0);
    assert!(registry.claim_unreported().is_empty());
    drop(changed);
    bounded("the close", registry.close()).await;
}

// A dropped caller: a wait that held the report and is dropped before it
// returns gives it back, and subscribers are told.
#[tokio::test]
async fn a_dropped_wait_gives_its_held_report_back() {
    let (_gate, registry) = registry(JobLimits::default());
    let mut events = registry.subscribe();
    let held = start_held(&registry, "bash");
    let mut first = Box::pin(registry.wait(held.id, Duration::from_secs(30), None));
    let mut second = Box::pin(registry.wait(held.id, Duration::from_secs(30), None));
    assert!(poll_once(first.as_mut()).is_pending());
    assert!(poll_once(second.as_mut()).is_pending());
    held.release
        .send(JobOutcome::completed("exit code: 0"))
        .expect("held");
    settled(&registry, held.id).await;
    drop(first);
    assert!(
        registry.get(held.id).expect("kept").reported,
        "still held for the second wait"
    );
    drop(second);
    assert!(!registry.get(held.id).expect("kept").reported);
    assert!(bounded("the event", events.changed()).await);
    let reported = registry.claim_unreported();
    assert_eq!(
        reported.iter().map(|job| job.id).collect::<Vec<_>>(),
        [held.id]
    );
    assert!(reported[0].reported);
    assert!(registry.claim_unreported().is_empty(), "claimed once");
    assert_eq!(registry.claim_report(held.id), Ok(false));
    bounded("the close", registry.close()).await;
}

// An unreported settlement is announced; the report is claimed exactly
// once, by whichever caller asks first.
#[tokio::test]
async fn an_unreported_settlement_is_announced_and_claimed_once() {
    let (_gate, registry) = registry(JobLimits::default());
    let mut events = registry.subscribe();
    let held = start_held(&registry, "bash");
    assert_eq!(registry.claim_report(held.id), Ok(false), "still live");
    held.release
        .send(JobOutcome::completed("exit code: 0"))
        .expect("held");
    assert!(bounded("the event", events.changed()).await);
    assert_eq!(registry.claim_report(held.id), Ok(true));
    assert_eq!(registry.claim_report(held.id), Ok(false));
    assert!(registry.claim_unreported().is_empty());
    let mut quiet = Box::pin(events.changed());
    assert!(poll_once(quiet.as_mut()).is_pending());
    drop(quiet);
    drop(events);
    bounded("the close", registry.close()).await;
}

// jobs.spec.ts: "fences read/kill/wait to the owning session" and
// "isolates exact owners": two sessions' registries issue the same text
// ids, and each refuses the other's; closing one leaves the other's jobs
// running.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_are_isolated() {
    let (gate_a, a) = registry(JobLimits::default());
    let (_gate_b, b) = registry(JobLimits::default());
    let held_a = start_held(&a, "bash");
    let held_b = start_held(&b, "bash");
    assert_eq!(held_a.id.to_string(), held_b.id.to_string());
    assert_ne!(held_a.id, held_b.id);
    assert_eq!(a.id("bash-1"), Ok(held_a.id));
    assert_eq!(b.id("bash-1"), Ok(held_b.id));
    let foreign = Err(JobError::Foreign(held_b.id));
    assert_eq!(a.get(held_b.id), foreign);
    assert_eq!(
        a.read(held_b.id).map(|_| ()),
        foreign.clone().map(|_: JobSnapshot| ())
    );
    assert_eq!(
        a.kill(held_b.id, None),
        foreign.clone().map(|_| KillResult::Requested)
    );
    assert_eq!(
        a.wait(held_b.id, Duration::from_millis(1), None).await,
        foreign
    );
    assert_eq!(a.claim_report(held_b.id), Err(JobError::Foreign(held_b.id)));
    assert_eq!(
        JobError::Foreign(held_b.id).to_string(),
        "job bash-1 belongs to another session"
    );
    let b_signal = bounded("b's start", held_b.started).await.expect("started");

    drop(held_a.release);
    drop(a.close());
    bounded("a's gate", gate_a.close()).await;
    assert_eq!(
        a.get(held_a.id).expect("kept").status,
        JobStatus::Failed,
        "a's producer, released without an outcome, failed"
    );
    assert!(!b_signal.aborted(), "a's close reached b");
    let b_job = b.get(held_b.id).expect("kept");
    assert_eq!((b_job.status, b_job.reported), (JobStatus::Running, false));
    held_b
        .release
        .send(JobOutcome::completed("exit code: 0"))
        .expect("held");
    assert_eq!(settled(&b, held_b.id).await.status, JobStatus::Completed);
    bounded("b's close", b.close()).await;
}

// Bounded retention: terminal records beyond the limit are dropped,
// reported ones first, and their ids answer `Expired`; ids an earlier
// process issued are reserved and answer `EarlierProcess`.
#[tokio::test]
async fn retention_is_bounded_and_ids_are_never_reused() {
    let (_gate, registry) = registry(JobLimits {
        max_retained: 2,
        ..JobLimits::default()
    });
    registry.reserve_ordinals("bash", 5).expect("reserved");
    assert_eq!(
        registry.reserve_ordinals("Bad", 1),
        Err(StartError::InvalidKind)
    );
    let finish = |registry: &JobRegistry| {
        registry
            .start(spec("bash", OutputMode::Final), |_| async {
                JobOutcome::completed("exit code: 0")
            })
            .expect("admitted")
    };
    let first = finish(&registry);
    assert_eq!(first.to_string(), "bash-6", "reserved ids are not issued");
    settled(&registry, first).await;
    let second = finish(&registry);
    settled(&registry, second).await;
    registry.read(second).expect("read");
    let third = finish(&registry);
    // `settled` cannot see a dropped record; wait on the third's own end.
    bounded("the third", async {
        while registry
            .get(third)
            .map_or(true, |job| !job.status.is_terminal())
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert_eq!(registry.get(second), Err(JobError::Expired(second)));
    assert_eq!(
        registry.list().iter().map(|job| job.id).collect::<Vec<_>>(),
        [first, third],
        "the reported record went first"
    );

    let earlier = registry.id("bash-3").expect("reserved");
    assert_eq!(
        registry.get(earlier),
        Err(JobError::EarlierProcess(earlier))
    );
    assert_eq!(
        registry.read(earlier).map(|_| ()),
        Err(JobError::EarlierProcess(earlier))
    );
    assert!(matches!(registry.id("bash-9"), Err(JobError::Unknown(_))));
    bounded("the close", registry.close()).await;
}

// A record a wait is registered on is not dropped, whatever the bound.
#[tokio::test]
async fn retention_keeps_a_record_with_a_pending_wait() {
    let (_gate, registry) = registry(JobLimits {
        max_retained: 0,
        ..JobLimits::default()
    });
    let held = start_held(&registry, "bash");
    let mut wait = Box::pin(registry.wait(held.id, Duration::from_secs(30), None));
    assert!(poll_once(wait.as_mut()).is_pending());
    held.release
        .send(JobOutcome::completed("exit code: 0"))
        .expect("held");
    let ended = bounded("the wait", wait).await.expect("terminal");
    assert_eq!(ended.status, JobStatus::Completed);
    let next = registry
        .start(spec("bash", OutputMode::Final), |_| async {
            JobOutcome::completed("")
        })
        .expect("admitted");
    bounded("the eviction", async {
        while registry.get(next).is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert_eq!(registry.get(held.id), Err(JobError::Expired(held.id)));
    assert!(registry.list().is_empty());
    bounded("the close", registry.close()).await;
}

// jobs.spec.ts: "cancels live jobs, awaits settlement, and silences
// listeners" and "reports a teardown-cancelled record so completion
// reporters stay quiet": close refuses at once, stops and claims every
// job, closes subscriptions, and resolves only once every job's resources
// have settled; a completion after it has no reporting effect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_stops_claims_and_awaits_every_job() {
    let (_gate, registry) = registry(JobLimits::default());
    let mut events = registry.subscribe();
    let stubborn = start_held(&registry, "bash");
    let resource = start_with_resource(&registry);
    let finished = registry
        .start(spec("bash", OutputMode::Final), |_| async {
            JobOutcome::completed("")
        })
        .expect("admitted");
    settled(&registry, finished).await;
    assert!(bounded("the event", events.changed()).await);
    let signal = bounded("the start", stubborn.started)
        .await
        .expect("started");

    let mut close = Box::pin(registry.close());
    assert!(registry.is_closed());
    assert_eq!(
        registry
            .start(spec("bash", OutputMode::Final), |_| async {
                JobOutcome::completed("")
            })
            .map(|_| ()),
        Err(StartError::Closed)
    );
    assert!(signal.aborted());
    let stopping = registry.get(stubborn.id).expect("kept");
    assert_eq!(stopping.status, JobStatus::Stopping);
    assert_eq!(stopping.stop, Some(StopCause::Shutdown));
    assert!(registry.list().iter().all(|job| job.reported));
    assert!(registry.claim_unreported().is_empty());
    assert!(!bounded("the closed subscription", events.changed()).await);

    bounded("the resource's cancel", resource.cancelled)
        .await
        .expect("cancelled");
    assert!(poll_once(close.as_mut()).is_pending());
    // The producer ignores the stop and completes after the close.
    stubborn
        .release
        .send(JobOutcome::completed("exit code: 0"))
        .expect("held");
    assert_eq!(
        settled(&registry, stubborn.id).await.status,
        JobStatus::Completed
    );
    assert!(registry.claim_unreported().is_empty());
    assert_eq!(registry.claim_report(stubborn.id), Ok(false));
    assert!(
        poll_once(close.as_mut()).is_pending(),
        "closed while a resource settled"
    );
    resource.release.send(()).expect("held");
    bounded("the close", close).await;
    assert!(resource.cleaned.load(Ordering::SeqCst));
    assert!(registry.list().iter().all(|job| job.status.is_terminal()));
}

// The session's gate closing stops its jobs as a shutdown, and its drain
// waits for each job's resources; a dropped registry close waits for
// nothing and stops nothing more.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_owning_gate_stops_and_awaits_its_jobs() {
    let (gate, registry) = registry(JobLimits::default());
    let held = start_held(&registry, "bash");
    let resource = start_with_resource(&registry);
    let signal = bounded("the start", held.started).await.expect("started");
    let mut drained = Box::pin(gate.close());
    assert_eq!(
        registry
            .start(spec("bash", OutputMode::Final), |_| async {
                JobOutcome::completed("")
            })
            .map(|_| ()),
        Err(StartError::Closed)
    );
    bounded("the stop", signal.cancelled()).await;
    let stopping = registry.get(held.id).expect("kept");
    assert_eq!(
        (stopping.status, stopping.stop, stopping.reported),
        (JobStatus::Stopping, Some(StopCause::Shutdown), true)
    );
    held.release
        .send(JobOutcome::killed("stopped"))
        .expect("held");
    bounded("the resource's cancel", resource.cancelled)
        .await
        .expect("cancelled");
    assert!(poll_once(drained.as_mut()).is_pending());
    assert!(
        !registry
            .get(resource.id)
            .expect("kept")
            .status
            .is_terminal()
    );
    resource.release.send(()).expect("held");
    bounded("the drain", drained).await;
    assert!(resource.cleaned.load(Ordering::SeqCst));
    assert!(registry.list().iter().all(|job| job.status.is_terminal()));
}

fn explode() -> JobOutcome {
    panic!("producer bug")
}

// A producer that panics fails its job, and its place and resources are
// released.
#[tokio::test]
async fn a_panicking_producer_fails_its_job() {
    let (_gate, registry) = registry(JobLimits {
        max_active: 1,
        ..JobLimits::default()
    });
    let id = registry
        .start(spec("bash", OutputMode::Final), |_| async { explode() })
        .expect("admitted");
    let ended = settled(&registry, id).await;
    assert_eq!(ended.status, JobStatus::Failed);
    assert!(
        ended
            .detail
            .is_some_and(|detail| detail.starts_with("the job's task failed"))
    );
    let next = start_held(&registry, "bash");
    drop(next.release);
    bounded("the close", registry.close()).await;
}

// The producer is called on the job's task, outside the registry's lock,
// and may use the registry; so may its future.
#[tokio::test]
async fn the_producer_runs_outside_the_registry_lock() {
    let (_gate, registry) = registry(JobLimits::default());
    let unlocked = Arc::new(AtomicUsize::new(0));
    let (seen, saw) = (Arc::clone(&unlocked), registry.clone());
    let id = registry
        .start(spec("bash", OutputMode::Final), move |context| {
            if saw.inner.state.try_lock().is_ok() {
                seen.fetch_add(1, Ordering::SeqCst);
            }
            let own = saw.get(context.id()).expect("visible to its producer");
            assert_eq!(own.status, JobStatus::Running);
            async move {
                if saw.inner.state.try_lock().is_ok() {
                    seen.fetch_add(1, Ordering::SeqCst);
                }
                assert_eq!(saw.list().len(), 1);
                JobOutcome::completed("")
            }
        })
        .expect("admitted");
    settled(&registry, id).await;
    assert_eq!(unlocked.load(Ordering::SeqCst), 2);
    bounded("the close", registry.close()).await;
}

// Outside a runtime the gate refuses the job; nothing is admitted.
#[test]
fn a_start_outside_a_runtime_is_refused() {
    let (_gate, registry) = registry(JobLimits::default());
    assert_eq!(
        registry
            .start(spec("bash", OutputMode::Final), |_| async {
                JobOutcome::completed("")
            })
            .map(|_| ()),
        Err(StartError::Refused(Refused::NoRuntime))
    );
    assert!(registry.list().is_empty());
}

#[cfg(unix)]
mod processes {
    //! The process producer over real children (`sh`): output, exit code,
    //! and a kill's signal, after `background.ts`'s `processOutcome`.

    use super::*;
    use crate::jobs::process::run_process;

    fn sh(script: &str) -> ProcessSpec {
        ProcessSpec {
            program: "sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: None,
            env: None,
            stdin: true,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_process_job_streams_its_output_and_completes_with_its_code() {
        let (_gate, registry) = registry(JobLimits::default());
        let spec_ = sh("printf 'out\\n'; printf 'err\\n' >&2; exit 3");
        let id = registry
            .start(spec("bash", OutputMode::Stream), move |context| {
                run_process(context, spec_)
            })
            .expect("admitted");
        let ended = settled(&registry, id).await;
        assert_eq!(ended.status, JobStatus::Completed);
        assert_eq!(ended.detail.as_deref(), Some("exit code: 3"));
        let text = registry.read(id).expect("read").text;
        let mut lines: Vec<&str> = text.lines().collect();
        lines.sort_unstable();
        assert_eq!(lines, ["err", "out"]);
        bounded("the close", registry.close()).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_killed_process_job_ends_killed_by_its_signal() {
        let (_gate, registry) = registry(JobLimits::default());
        let spec_ = sh("echo ready; exec sleep 1000");
        let id = registry
            .start(spec("bash", OutputMode::Stream), move |context| {
                run_process(context, spec_)
            })
            .expect("admitted");
        bounded("the output", async {
            let mut seen = String::new();
            while !seen.contains("ready") {
                seen.push_str(&registry.read(id).expect("read").text);
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert_eq!(registry.kill(id, None), Ok(KillResult::Requested));
        let ended = settled(&registry, id).await;
        assert_eq!(ended.status, JobStatus::Killed);
        assert_eq!(ended.detail.as_deref(), Some("signal: SIGKILL"));
        bounded("the close", registry.close()).await;
    }

    #[tokio::test]
    async fn a_failed_spawn_fails_the_job() {
        let (_gate, registry) = registry(JobLimits::default());
        let id = registry
            .start(spec("bash", OutputMode::Stream), |context| {
                run_process(
                    context,
                    ProcessSpec {
                        program: "/nonexistent/bake-job-fixture".into(),
                        args: Vec::new(),
                        cwd: None,
                        env: None,
                        stdin: false,
                    },
                )
            })
            .expect("admitted");
        let ended = settled(&registry, id).await;
        assert_eq!(ended.status, JobStatus::Failed);
        assert!(
            ended
                .detail
                .is_some_and(|detail| detail.starts_with("could not start the process"))
        );
        bounded("the close", registry.close()).await;
    }
}
