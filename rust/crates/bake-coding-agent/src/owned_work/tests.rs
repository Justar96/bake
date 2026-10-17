//! The gate's admission, cancellation, and drain, driven with real pending
//! futures. A future under test is polled by hand with a no-op waker where
//! the case is "still pending", so no assertion depends on how long a
//! scheduler takes; the bounded waits only stop a broken gate from hanging
//! the suite.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::oneshot;

use super::*;

/// Generous: each bounded wait is for a state the test has already caused.
const BOUND: Duration = Duration::from_secs(10);

/// Polls `future` once.
fn poll_once<F: Future + Unpin>(future: &mut F) -> Poll<F::Output> {
    let mut cx = Context::from_waker(std::task::Waker::noop());
    Pin::new(future).poll(&mut cx)
}

async fn bounded<F: Future>(what: &str, future: F) -> F::Output {
    tokio::time::timeout(BOUND, future)
        .await
        .unwrap_or_else(|_| panic!("{what} did not finish"))
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

#[tokio::test]
async fn close_refuses_admission_before_it_is_polled() {
    let gate = WorkGate::default();
    let closing = gate.close();
    assert!(gate.is_closing());
    assert!(gate.enter().is_none());
    assert_eq!(
        gate.spawn(|_| async {}).map(|_| ()),
        Err(Refused::Closed),
        "a closing gate spawns nothing"
    );
    assert_eq!(gate.spawn_blocking(|| ()).map(|_| ()), Err(Refused::Closed));
    closing.await;
    assert!(gate.is_closed());
}

#[tokio::test]
async fn close_waits_for_every_permit() {
    let gate = WorkGate::default();
    let first = gate.enter().expect("open gate admits work");
    let second = first.share();
    let mut closing = Box::pin(gate.close());
    assert!(poll_once(&mut closing).is_pending());
    drop(first);
    assert!(
        poll_once(&mut closing).is_pending(),
        "a share still holds it"
    );
    assert!(!gate.is_closed());
    drop(second);
    assert!(poll_once(&mut closing).is_ready());
    assert!(gate.is_closed());
    // Closing again returns at once.
    assert!(poll_once(&mut Box::pin(gate.close())).is_ready());
}

// A cancelled shutdown waiter must not reopen the gate: closing is the
// gate's state, not the waiter's.
#[tokio::test]
async fn dropping_the_close_waiter_neither_reopens_nor_stops_the_drain() {
    let gate = WorkGate::default();
    let permit = gate.enter().expect("admitted");
    let mut closing = Box::pin(gate.close());
    assert!(poll_once(&mut closing).is_pending());
    drop(closing);
    assert!(gate.enter().is_none(), "the gate stays closed");
    assert!(gate.spawn(|_| async {}).is_err());
    let mut again = Box::pin(gate.close());
    assert!(
        poll_once(&mut again).is_pending(),
        "the permit is still held"
    );
    drop(permit);
    assert!(poll_once(&mut again).is_ready());
}

#[tokio::test]
async fn concurrent_closes_all_wait_for_the_same_drain() {
    let gate = WorkGate::default();
    let permit = gate.enter().expect("admitted");
    let mut waiters: Vec<_> = (0..4).map(|_| Box::pin(gate.close())).collect();
    for waiter in &mut waiters {
        assert!(poll_once(waiter).is_pending());
    }
    drop(permit);
    for waiter in &mut waiters {
        assert!(poll_once(waiter).is_ready());
    }
}

// Chord `forced disposal makes references gone and aborts in-flight
// calls`: closing aborts a running task's signal, waits for it, including
// its cleanup after the abort, and admits nothing more.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_cancels_a_task_and_waits_for_its_cleanup() {
    let gate = WorkGate::default();
    let cleaned = Arc::new(AtomicBool::new(false));
    let (saw_abort, aborted) = oneshot::channel();
    let (finish, finished) = oneshot::channel::<()>();
    let task = gate
        .spawn({
            let cleaned = Arc::clone(&cleaned);
            move |scope| async move {
                let _cleanup = Cleanup(cleaned);
                scope.signal().cancelled().await;
                let _ = saw_abort.send(());
                // Cleanup that outlasts the abort.
                let _ = finished.await;
            }
        })
        .expect("admitted");
    let mut closing = Box::pin(gate.close());
    bounded("the task's abort", aborted).await.expect("abort");
    assert!(poll_once(&mut closing).is_pending(), "cleanup is running");
    assert!(!cleaned.load(Ordering::SeqCst));
    finish.send(()).expect("task waits");
    bounded("the drain", closing).await;
    // Nothing of the task runs once the drain returned.
    assert!(cleaned.load(Ordering::SeqCst));
    assert_eq!(task.await, Ok(()));
}

// Dropping an owned task's handle gives up its output, not the task: the
// gate still waits for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_handle_leaves_the_task_owned() {
    let gate = WorkGate::default();
    let cleaned = Arc::new(AtomicBool::new(false));
    let (started, ready) = oneshot::channel();
    let (finish, finished) = oneshot::channel::<()>();
    drop(
        gate.spawn({
            let cleaned = Arc::clone(&cleaned);
            move |_scope| async move {
                let _cleanup = Cleanup(cleaned);
                let _ = started.send(());
                // Ignores the abort: the gate still waits for it.
                let _ = finished.await;
            }
        })
        .expect("admitted"),
    );
    bounded("the task's start", ready).await.expect("start");
    let mut closing = Box::pin(gate.close());
    assert!(poll_once(&mut closing).is_pending());
    finish.send(()).expect("task waits");
    bounded("the drain", closing).await;
    assert!(cleaned.load(Ordering::SeqCst));
}

// Cancelling one task reaches the tasks spawned from its scope, not its
// siblings or foreground permits: the shape of an ordinary abort that
// leaves background work alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_reaches_descendants_only() {
    let gate = WorkGate::default();
    let foreground = gate.enter().expect("admitted");
    let (child_tx, child_rx) = oneshot::channel();
    let parent = gate
        .spawn(move |scope| {
            let child = scope
                .permit()
                .spawn(|scope| async move { scope.signal().cancelled().await })
                .expect("child admitted");
            let _ = child_tx.send(child);
            async move { scope.signal().cancelled().await }
        })
        .expect("admitted");
    let child = child_rx.await.expect("child");
    let (sibling_tx, sibling_rx) = oneshot::channel::<AbortSignal>();
    let sibling = gate
        .spawn(move |scope| {
            let _ = sibling_tx.send(scope.signal().clone());
            async move { scope.signal().cancelled().await }
        })
        .expect("admitted");
    let sibling_signal = sibling_rx.await.expect("signal");
    parent.cancel();
    bounded("the parent", parent).await.expect("parent");
    bounded("the child", child).await.expect("child");
    assert!(!sibling_signal.aborted(), "a sibling is not cancelled");
    assert!(
        !gate.is_closing(),
        "cancelling a task does not close the gate"
    );
    drop(foreground);
    sibling.cancel();
    bounded("the sibling", sibling).await.expect("sibling");
    bounded("the drain", gate.close()).await;
}

// Chord `an aborted task ctx cannot start fresh calls`: a cancelled task
// spawns no new task, but may still run the blocking cleanup it owns.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_task_spawns_no_task_but_finishes_blocking_cleanup() {
    let gate = WorkGate::default();
    let wrote = Arc::new(AtomicBool::new(false));
    let task = gate
        .spawn({
            let wrote = Arc::clone(&wrote);
            move |scope| async move {
                scope.signal().cancelled().await;
                let refused = scope.permit().spawn(|_| async {}).map(|_| ());
                let cleanup = scope
                    .permit()
                    .spawn_blocking(move || wrote.store(true, Ordering::SeqCst))
                    .expect("blocking cleanup is admitted while closing");
                cleanup.await.expect("cleanup");
                refused
            }
        })
        .expect("admitted");
    let closing = gate.close();
    // Negative control: a gate-level admission while closing is refused
    // for blocking work too; only an admitted claim may extend itself.
    assert!(gate.spawn_blocking(|| ()).is_err());
    bounded("the drain", closing).await;
    assert!(wrote.load(Ordering::SeqCst));
    assert_eq!(task.await, Ok(Err(Refused::Closed)));
}

// A permit taken before the gate started closing, as a model request's
// is, starts no task after; the request then ends without one.
#[tokio::test]
async fn a_permit_spawns_no_task_once_the_gate_is_closing() {
    let gate = WorkGate::default();
    let permit = gate.enter().expect("admitted");
    let closing = gate.close();
    let called = AtomicBool::new(false);
    let refused = permit.spawn(|_| {
        called.store(true, Ordering::SeqCst);
        async {}
    });
    assert_eq!(refused.map(|_| ()), Err(Refused::Closed));
    assert!(!called.load(Ordering::SeqCst));
    drop(permit);
    bounded("the drain", closing).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_parent_refuses_a_child_while_the_gate_is_open() {
    let gate = WorkGate::default();
    let task = gate
        .spawn(|scope| async move {
            scope.signal().cancelled().await;
            scope.permit().spawn(|_| async {}).map(|_| ())
        })
        .expect("admitted");
    task.cancel();
    assert_eq!(bounded("the task", task).await, Ok(Err(Refused::Cancelled)));
    assert!(gate.enter().is_some(), "the gate stays open");
}

// A panic in the task, or in building its future, releases what it held.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn panics_release_their_claims() {
    let gate = WorkGate::default();
    let task = gate
        .spawn(|_| async { panic!("task failed") })
        .expect("admitted");
    assert!(bounded("the task", task).await.is_err());
    let blocking = gate
        .spawn_blocking(|| panic!("blocking failed"))
        .expect("admitted");
    assert!(bounded("the blocking work", blocking).await.is_err());
    let built = std::panic::catch_unwind(AssertUnwindSafe(|| {
        gate.spawn(|_| -> std::future::Ready<()> { panic!("build failed") })
    }));
    assert!(built.is_err());
    assert!(poll_once(&mut Box::pin(gate.close())).is_ready());
}

#[test]
fn spawning_outside_a_runtime_is_refused() {
    let gate = WorkGate::default();
    let called = AtomicBool::new(false);
    let refused = gate.spawn(|_| {
        called.store(true, Ordering::SeqCst);
        async {}
    });
    assert_eq!(refused.map(|_| ()), Err(Refused::NoRuntime));
    assert!(!called.load(Ordering::SeqCst), "the work is not built");
    assert_eq!(
        gate.spawn_blocking(|| ()).map(|_| ()),
        Err(Refused::NoRuntime)
    );
    // The refused admissions hold nothing.
    assert!(poll_once(&mut Box::pin(gate.close())).is_ready());
}

// Admission races close from other threads. Every task admitted must see
// its cancellation: one admitted after close swept the tasks, without its
// signal aborted, would wait for it forever and hang the drain. Each task
// counts its end as its future is dropped, which the drain covers; Tokio
// marks the task finished only after that, so `is_finished` may trail it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admission_racing_close_is_cancelled_or_refused() {
    for _ in 0..200 {
        let gate = WorkGate::default();
        let admitted = Arc::new(AtomicUsize::new(0));
        let ended = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let spawners: Vec<_> = (0..3)
            .map(|_| {
                let (gate, admitted, ended, barrier) = (
                    gate.clone(),
                    Arc::clone(&admitted),
                    Arc::clone(&ended),
                    Arc::clone(&barrier),
                );
                let runtime = tokio::runtime::Handle::current();
                std::thread::spawn(move || {
                    let _entered = runtime.enter();
                    barrier.wait();
                    let mut tasks = Vec::new();
                    for _ in 0..8 {
                        let ended = Arc::clone(&ended);
                        match gate.spawn(|scope| async move {
                            let _end = Ended(ended);
                            scope.signal().cancelled().await;
                        }) {
                            Ok(task) => {
                                admitted.fetch_add(1, Ordering::SeqCst);
                                tasks.push(task);
                            }
                            Err(refused) => assert_eq!(refused, Refused::Closed),
                        }
                    }
                    tasks
                })
            })
            .collect();
        barrier.wait();
        let closing = gate.close();
        assert!(gate.enter().is_none());
        bounded("the drain", closing).await;
        let ended_at_drain = ended.load(Ordering::SeqCst);
        let mut tasks = 0;
        for spawner in spawners {
            tasks += spawner.join().expect("spawner").len();
        }
        assert_eq!(tasks, admitted.load(Ordering::SeqCst));
        assert_eq!(ended_at_drain, tasks, "drained before every task ended");
        assert!(gate.is_closed());
    }
}

/// A task that reports when its future has been dropped.
fn ended_flag() -> (Arc<AtomicBool>, Cleanup) {
    let flag = Arc::new(AtomicBool::new(false));
    (Arc::clone(&flag), Cleanup(flag))
}

/// Closes `gate` within the bound, so a failed assertion below leaves no
/// owned task running.
async fn drain(gate: &WorkGate) {
    bounded("the drain", gate.close()).await;
}

// Review regression: A -> B -> C, where B has ended before A is cancelled
// or the gate closes. Either must still reach C, which B spawned from its
// scope. C's handle is kept only so a failure can still end C and drain.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_reaches_a_grandchild_through_an_ended_task() {
    for by_close in [false, true] {
        let gate = WorkGate::default();
        let (c_tx, c_rx) = oneshot::channel::<(OwnedTask<()>, AbortSignal)>();
        let (b_ended, b_cleanup) = ended_flag();
        let (b_done_tx, b_done) = oneshot::channel::<()>();
        let a = gate
            .spawn(move |a_scope| {
                let b = a_scope
                    .permit()
                    .spawn(move |b_scope| {
                        let mut signal = None;
                        let c = b_scope
                            .permit()
                            .spawn(|c_scope| {
                                signal = Some(c_scope.signal().clone());
                                async move { c_scope.signal().cancelled().await }
                            })
                            .expect("C admitted");
                        let _ = c_tx.send((c, signal.expect("C's scope")));
                        async move {
                            // B ends at once; its future and scope are dropped.
                            drop(b_cleanup);
                            drop(b_scope);
                        }
                    })
                    .expect("B admitted");
                async move {
                    let _ = b.await;
                    let _ = b_done_tx.send(());
                    a_scope.signal().cancelled().await;
                }
            })
            .expect("A admitted");
        let (c, c_signal) = bounded("C's start", c_rx).await.expect("C");
        bounded("B's end", b_done).await.expect("B");
        assert!(b_ended.load(Ordering::SeqCst));
        let closing = if by_close {
            Some(gate.close())
        } else {
            a.cancel();
            None
        };
        let reached = c_signal.aborted();
        // Cleanup that does not depend on the rule under test.
        c.cancel();
        a.cancel();
        match closing {
            Some(closing) => bounded("the drain", closing).await,
            None => drain(&gate).await,
        }
        assert!(
            reached,
            "{} did not reach C through ended B",
            if by_close { "close" } else { "A's cancel" }
        );
    }
}

// Review regression: a permit shared out of a task's scope outlives the
// task. Once that task is cancelled, directly or through its parent, the
// permit spawns nothing, even after the task has ended; and a child it
// spawned after the task ended is still cancelled with the task's parent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retained_permit_keeps_its_task_in_the_tree() {
    for cancel_parent in [false, true] {
        let gate = WorkGate::default();
        let (permit_tx, permit_rx) = oneshot::channel::<WorkPermit>();
        let (b_ended, b_cleanup) = ended_flag();
        let (b_tx, b_rx) = oneshot::channel::<OwnedTask<()>>();
        let a = gate
            .spawn(move |a_scope| {
                let b = a_scope
                    .permit()
                    .spawn(move |b_scope| {
                        let _ = permit_tx.send(b_scope.permit().share());
                        async move {
                            let _cleanup = b_cleanup;
                            b_scope.signal().cancelled().await;
                        }
                    })
                    .expect("B admitted");
                let _ = b_tx.send(b);
                async move { a_scope.signal().cancelled().await }
            })
            .expect("A admitted");
        let retained = bounded("B's permit", permit_rx).await.expect("permit");
        let b = bounded("B's handle", b_rx).await.expect("B");
        if cancel_parent {
            a.cancel();
        } else {
            b.cancel();
        }
        bounded("B's end", b).await.expect("B");
        assert!(b_ended.load(Ordering::SeqCst));
        let refused = retained.spawn(|_| async {}).map(|_| ());
        drop(retained);
        if !cancel_parent {
            a.cancel();
        }
        drain(&gate).await;
        assert_eq!(
            refused,
            Err(Refused::Cancelled),
            "a cancelled, ended task's retained permit admitted work (cancel_parent: {cancel_parent})"
        );
    }
}

// A retained permit of an ended, uncancelled task still spawns, as a child
// of that task: its parent's cancel reaches the new child.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_spawned_after_its_parent_ended_is_still_owned_by_the_ancestor() {
    let gate = WorkGate::default();
    let (permit_tx, permit_rx) = oneshot::channel::<WorkPermit>();
    let (b_ended, b_cleanup) = ended_flag();
    let (b_tx, b_rx) = oneshot::channel::<OwnedTask<()>>();
    let a = gate
        .spawn(move |a_scope| {
            let b = a_scope
                .permit()
                .spawn(move |b_scope| {
                    let _ = permit_tx.send(b_scope.permit().share());
                    async move { drop(b_cleanup) }
                })
                .expect("B admitted");
            let _ = b_tx.send(b);
            async move { a_scope.signal().cancelled().await }
        })
        .expect("A admitted");
    let retained = bounded("B's permit", permit_rx).await.expect("permit");
    bounded("B's end", bounded("B's handle", b_rx).await.expect("B"))
        .await
        .expect("B");
    assert!(b_ended.load(Ordering::SeqCst));
    let mut c_signal = None;
    let c = retained
        .spawn(|scope| {
            c_signal = Some(scope.signal().clone());
            async move { scope.signal().cancelled().await }
        })
        .expect("an uncancelled task's permit admits a child");
    drop(retained);
    let c_signal = c_signal.expect("C's scope");
    a.cancel();
    let reached = c_signal.aborted();
    // Cleanup that does not depend on the rule under test.
    c.cancel();
    drain(&gate).await;
    assert!(reached, "A's cancel did not reach C spawned after B ended");
    assert_eq!(bounded("C", c).await, Ok(()));
}

// The tree holds nothing for a task once its handle, scope, permits, and
// descendants are gone, so a long session does not keep every task.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ended_tasks_leave_nothing_in_the_tree() {
    let gate = WorkGate::default();
    for _ in 0..1000 {
        let task = gate
            .spawn(|scope| {
                let child = scope.permit().spawn(|_| async {}).expect("child");
                async move {
                    let _ = child.await;
                }
            })
            .expect("admitted");
        bounded("the task", task).await.expect("task");
    }
    assert!(
        gate.tracked_tasks() <= 2,
        "{} ended tasks still tracked",
        gate.tracked_tasks()
    );
    drain(&gate).await;
}
