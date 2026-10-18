//! Bake: the session's ownership of its background work and teardown
//! ([`AgentSession::spawn_background`], [`AgentSession::shutdown`]), over
//! the production session, gate, and faux provider. Pending futures are
//! released by explicit channels; a future that must still be pending is
//! polled by hand, and every bounded wait is for a state the test already
//! caused, so the bound only stops a broken teardown from hanging the
//! suite.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::task::{Context, Poll};
use std::time::Duration;

use bake_ai::providers::faux::{FauxResponseStep, faux_assistant_message};
use bake_ai::{AbortSignal, StopReason};
use tokio::sync::oneshot;

use super::tests::{harness, new_file_session, options};
use super::*;
use crate::session::NewSessionOptions;
use crate::test_support::TempDir;

const BOUND: Duration = Duration::from_secs(10);

async fn bounded<F: Future>(what: &str, future: F) -> F::Output {
    tokio::time::timeout(BOUND, future)
        .await
        .unwrap_or_else(|_| panic!("{what} did not finish"))
}

fn poll_once<F: Future + Unpin>(future: &mut F) -> Poll<F::Output> {
    let mut cx = Context::from_waker(std::task::Waker::noop());
    Pin::new(future).poll(&mut cx)
}

/// Counts one task's end when dropped.
struct Ended(Arc<AtomicUsize>);

impl Drop for Ended {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Sets its flag when dropped: the end of a task's cleanup.
struct Cleanup(Arc<AtomicBool>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

async fn in_memory_session(h: &super::tests::Harness) -> Arc<AgentSession> {
    let manager = SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
        .expect("session");
    Arc::new(
        AgentSession::create(options(h, manager, "/tmp"))
            .await
            .expect("session"),
    )
}

/// A background task that reports its start and its cancellation, then
/// holds its cleanup until released.
struct Held {
    /// Kept alive: a held handle must not keep shutdown from finishing.
    _task: OwnedTask<()>,
    signal: AbortSignal,
    cancelled: oneshot::Receiver<()>,
    release: oneshot::Sender<()>,
    cleaned: Arc<AtomicBool>,
}

fn hold_background(session: &AgentSession) -> Held {
    let cleaned = Arc::new(AtomicBool::new(false));
    let (saw_cancel, cancelled) = oneshot::channel();
    let (release, released) = oneshot::channel::<()>();
    let mut signal = None;
    let task = session
        .spawn_background(|scope| {
            signal = Some(scope.signal().clone());
            let cleaned = Arc::clone(&cleaned);
            async move {
                let _cleanup = Cleanup(cleaned);
                scope.signal().cancelled().await;
                let _ = saw_cancel.send(());
                let _ = released.await;
            }
        })
        .expect("admitted");
    Held {
        _task: task,
        signal: signal.expect("the scope is given at once"),
        cancelled,
        release,
        cleaned,
    }
}

// The foreground/background boundary: Esc (`abort`) ends the turn and its
// model request but leaves background work running; shutdown cancels the
// background work and returns only after its cleanup.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abort_leaves_background_work_and_shutdown_awaits_its_cleanup() {
    let h = harness();
    let (started, request_started) = oneshot::channel::<()>();
    let started = Arc::new(Mutex::new(Some(started)));
    h.faux
        .set_responses(vec![FauxResponseStep::Factory(Arc::new(
            move |_, options, _, _| {
                let signal = options.base.signal.clone();
                if let Some(started) = started.lock().expect("lock").take() {
                    let _ = started.send(());
                }
                Box::pin(async move {
                    if let Some(signal) = signal {
                        signal.cancelled().await;
                    }
                    Ok(faux_assistant_message("never shown"))
                })
            },
        ))]);
    let session = in_memory_session(&h).await;
    let held = hold_background(&session);
    let run = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("hang", Vec::new()).await })
    };
    bounded("the request", request_started)
        .await
        .expect("request");
    bounded("the abort", session.abort()).await;
    assert_eq!(bounded("the prompt", run).await.expect("joined"), Ok(()));
    let last = session
        .messages()
        .last()
        .and_then(|message| message.as_assistant().cloned());
    assert_eq!(last.map(|m| m.stop_reason), Some(StopReason::Aborted));
    assert!(!held.signal.aborted(), "abort reached background work");
    assert!(
        !held.cleaned.load(Ordering::SeqCst),
        "abort ended background work"
    );

    let mut shutdown = Box::pin({
        let session = Arc::clone(&session);
        async move { session.shutdown().await }
    });
    assert!(poll_once(&mut shutdown).is_pending());
    bounded("the background cancel", held.cancelled)
        .await
        .expect("cancelled");
    assert!(
        poll_once(&mut shutdown).is_pending(),
        "shutdown returned while background cleanup ran"
    );
    assert!(!held.cleaned.load(Ordering::SeqCst));
    held.release.send(()).expect("task waits");
    bounded("the shutdown", shutdown).await;
    assert!(held.cleaned.load(Ordering::SeqCst));
}

// Shutdown refuses at its first poll, and a waiter dropped there neither
// reopens the session nor stops the teardown, which still releases the
// file once the background work ends. On one thread the teardown task has
// not run when the refusals are checked, so they come from the first poll.
#[tokio::test]
async fn a_dropped_shutdown_waiter_neither_reopens_nor_stops_the_teardown() {
    let dir = TempDir::new("service-dropped-waiter");
    let cwd = dir.path().to_string_lossy().into_owned();
    let h = harness();
    h.faux
        .set_responses(vec![faux_assistant_message("first").into()]);
    let session = AgentSession::create(options(&h, new_file_session(&dir), &cwd))
        .await
        .expect("session");
    session.prompt("one", Vec::new()).await.expect("one");
    let file = session.session_file().expect("persisted");
    let held = hold_background(&session);

    let mut waiter = Box::pin(session.shutdown());
    assert!(poll_once(&mut waiter).is_pending());
    drop(waiter);
    assert_eq!(
        session.spawn_background(|_| async {}).map(|_| ()),
        Err("The session is shut down.".to_owned())
    );
    assert!(session.prompt("two", Vec::new()).await.is_err());
    assert_eq!(
        session
            .set_thinking_level(ModelThinkingLevel::Low, false)
            .await,
        Err("The session is shut down.".to_owned())
    );
    bounded("the background cancel", held.cancelled)
        .await
        .expect("cancelled");
    // The writer is still held, past the lock's wait: the teardown waits
    // for the background task.
    assert!(matches!(
        SessionManager::open(&file, None, None),
        Err(crate::session::SessionError::Locked(_))
    ));
    held.release.send(()).expect("task waits");
    bounded("the shutdown", session.shutdown()).await;
    assert!(held.cleaned.load(Ordering::SeqCst));
    drop(SessionManager::open(&file, None, None).expect("the writer is released"));
}

// Registration racing shutdown from other threads: each admitted task is
// cancelled and its future dropped when any shutdown call returns; the rest
// are refused with the shutdown message.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_registration_racing_concurrent_shutdowns() {
    let h = harness();
    for _ in 0..20 {
        let session = in_memory_session(&h).await;
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let admitted = Arc::new(AtomicUsize::new(0));
        let ended = Arc::new(AtomicUsize::new(0));
        let spawners: Vec<_> = (0..2)
            .map(|_| {
                let (session, barrier, admitted, ended) = (
                    Arc::clone(&session),
                    Arc::clone(&barrier),
                    Arc::clone(&admitted),
                    Arc::clone(&ended),
                );
                let runtime = tokio::runtime::Handle::current();
                std::thread::spawn(move || {
                    let _entered = runtime.enter();
                    barrier.wait();
                    let mut tasks = Vec::new();
                    for _ in 0..8 {
                        let ended = Arc::clone(&ended);
                        match session.spawn_background(|scope| async move {
                            let _end = Ended(ended);
                            scope.signal().cancelled().await;
                        }) {
                            Ok(task) => {
                                admitted.fetch_add(1, Ordering::SeqCst);
                                tasks.push(task);
                            }
                            Err(error) => assert_eq!(error, "The session is shut down."),
                        }
                    }
                    tasks
                })
            })
            .collect();
        barrier.wait();
        let shutdowns: Vec<_> = (0..3)
            .map(|_| {
                let session = Arc::clone(&session);
                tokio::spawn(async move { session.shutdown().await })
            })
            .collect();
        let mut ended_at_return = None;
        for shutdown in shutdowns {
            bounded("a shutdown", shutdown).await.expect("joined");
            ended_at_return.get_or_insert(ended.load(Ordering::SeqCst));
        }
        let mut tasks = 0;
        for spawner in spawners {
            tasks += spawner.join().expect("spawner").len();
        }
        assert_eq!(tasks, admitted.load(Ordering::SeqCst));
        assert_eq!(
            ended_at_return,
            Some(tasks),
            "shutdown returned before every task ended"
        );
    }
}

// No listener call outlives shutdown: one already running when shutdown
// starts holds it, and none starts after. On one thread, yielding lets the
// teardown run until it waits for something outside the runtime, which can
// only be the running listener.
#[tokio::test]
async fn shutdown_waits_for_a_running_listener_and_admits_no_later_one() {
    let h = harness();
    let session = in_memory_session(&h).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let returned = Arc::new(AtomicBool::new(false));
    let (entered_tx, entered) = std::sync::mpsc::channel::<()>();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let released = Mutex::new(released);
    let _subscription = session.subscribe({
        let (calls, returned) = (Arc::clone(&calls), Arc::clone(&returned));
        move |event| {
            if let SessionEvent::SessionInfoChanged(_) = event {
                calls.fetch_add(1, Ordering::SeqCst);
                let _ = entered_tx.send(());
                let _ = released.lock().expect("lock").recv();
                returned.store(true, Ordering::SeqCst);
            }
        }
    });
    // An emit outside any owned work, as a prompt's own events are.
    let emitter = {
        let listeners = Arc::clone(&session.listeners);
        std::thread::spawn(move || listeners.emit(&SessionEvent::SessionInfoChanged(None)))
    };
    tokio::task::spawn_blocking(move || entered.recv_timeout(BOUND))
        .await
        .expect("joined")
        .expect("the listener runs");
    let mut shutdown = Box::pin({
        let session = Arc::clone(&session);
        async move { session.shutdown().await }
    });
    assert!(poll_once(&mut shutdown).is_pending());
    while !session.work.is_closed() {
        tokio::task::yield_now().await;
    }
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    // Closed before the running call returns: a later emit calls nothing.
    session
        .listeners
        .emit(&SessionEvent::SessionInfoChanged(None));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        poll_once(&mut shutdown).is_pending(),
        "shutdown returned while a listener ran"
    );
    release.send(()).expect("listener waits");
    bounded("the shutdown", shutdown).await;
    assert!(returned.load(Ordering::SeqCst));
    emitter.join().expect("emitter");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Wakes a parked thread.
struct ThreadWaker(std::thread::Thread);

impl std::task::Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// Drives `future` on the calling thread, outside any Tokio runtime,
/// until it completes or `deadline` passes; `None` on the deadline, so
/// the thread always ends.
fn block_on_until<F: Future>(future: F, deadline: std::time::Instant) -> Option<F::Output> {
    let waker = std::task::Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return Some(output);
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return None;
        }
        std::thread::park_timeout(deadline - now);
    }
}

// Review regression: shutdown called from threads outside any Tokio
// runtime. The first caller polls once and drops its future; the teardown
// must still run on the session's runtime, so a later caller, also outside
// Tokio, returns only once the teardown has finished: the background work
// has ended and the session file's writer is released. A return without
// the teardown, such as a stranded or dropped teardown ending its waiters,
// leaves the file locked.
#[test]
fn shutdown_outside_tokio_survives_a_dropped_first_waiter() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let dir = TempDir::new("service-outside-tokio");
    let cwd = dir.path().to_string_lossy().into_owned();
    let h = harness();
    h.faux
        .set_responses(vec![faux_assistant_message("first").into()]);
    let session = runtime.block_on(async {
        let session = AgentSession::create(options(&h, new_file_session(&dir), &cwd))
            .await
            .expect("session");
        session.prompt("one", Vec::new()).await.expect("one");
        Arc::new(session)
    });
    let file = session.session_file().expect("persisted");
    let held = {
        let _entered = runtime.enter();
        hold_background(&session)
    };
    let first = {
        let session = Arc::clone(&session);
        std::thread::spawn(move || {
            let mut waiter = Box::pin(session.shutdown());
            // Pending while the background task runs; dropped either way.
            let _ = poll_once(&mut waiter);
        })
    };
    first.join().expect("first caller");
    runtime
        .block_on(bounded("the background cancel", held.cancelled))
        .expect("cancelled");
    held.release.send(()).expect("task waits");
    let second = {
        let session = Arc::clone(&session);
        std::thread::spawn(move || {
            block_on_until(session.shutdown(), std::time::Instant::now() + BOUND).is_some()
        })
    };
    let finished = second.join().expect("second caller");
    assert!(finished, "a later shutdown outside Tokio never returned");
    assert!(held.cleaned.load(Ordering::SeqCst));
    let reopened = SessionManager::open(&file, None, None).map(drop);
    drop(session);
    drop(runtime);
    assert!(
        reopened.is_ok(),
        "shutdown returned before its teardown released the file: {reopened:?}"
    );
}
