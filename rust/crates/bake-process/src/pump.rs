//! The live pump of a Windows tree's job account: an owned task that drains
//! the job's completion port while the process runs, so the kernel queue is
//! not left to grow with a long-lived tree's whole life (within the limits
//! under Bounds) and each reported pid is opened soon after its process
//! starts, while its identity is most likely still the reported one.
//!
//! Ownership: [`crate::OwnedProcess::spawn`] starts it in the same poll that
//! creates the owner, so no owner exists without it. The task holds only a
//! [`Weak`] reference to the tree and upgrades it for one bounded,
//! synchronous step at a time, never across an await, so it never keeps the
//! job (or its kill-on-close) alive while it waits, and stops by itself once
//! the tree is gone. A settled owner stops it and awaits it
//! ([`Pump::stop`]) before it releases its claim; a dropped owner without a
//! claim aborts it ([`Pump::abort`]), which, since it holds nothing across
//! an await, drops nothing but its weak reference. Dropped with the runtime,
//! the same holds.
//!
//! Bounds: a step ([`crate::windows_job::Job::pump`]) takes at most a
//! fixed number of messages, retries unknown members, and closes the
//! handles of members now signalled. After a step that found the queue non-empty the task
//! yields and steps again; after an empty one it waits with a backoff from
//! 1 ms to 50 ms, or until stopped. The queue therefore stays at what
//! arrives between two steps as long as the task gets CPU at least as fast
//! as the tree starts processes; a starved runtime or a faster tree makes
//! it grow until the pump catches up, which nothing here can prevent. Held
//! handles are the members not yet seen signalled, and the account keeps
//! one record per distinct pid (`job_ledger::MAX_RECORDS`). Pacing is not a
//! correctness argument: whatever the pump misses, the settle drains, and a
//! lost or unreadable message still leaves the account short, never
//! settled.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use tokio::runtime::Handle;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::tree::ProcessTree;
use crate::windows_job::{MAX_PAUSE, MIN_PAUSE, Step};

/// Shared by the owner and its task.
#[derive(Debug, Default)]
pub(crate) struct Control {
    stop: AtomicBool,
    wake: Notify,
    /// A test barrier the task must pass before it ends on a stop request.
    #[cfg(test)]
    stop_barrier: std::sync::Mutex<Option<Arc<tokio::sync::Semaphore>>>,
}

impl Control {
    /// Holds the task at its stop until a permit is added to the returned
    /// semaphore, so a test can keep a stop pending deterministically. An
    /// abort still ends the task at that wait.
    #[cfg(test)]
    pub(crate) fn hold_stop(&self) -> Arc<tokio::sync::Semaphore> {
        let barrier = Arc::new(tokio::sync::Semaphore::new(0));
        *self
            .stop_barrier
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Waits at the test barrier, if any.
    #[cfg(test)]
    async fn pass_stop_barrier(&self) {
        let barrier = self
            .stop_barrier
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(barrier) = barrier {
            let _ = barrier.acquire().await;
        }
    }

    fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
        // A permit is kept if the task is not waiting yet.
        self.wake.notify_one();
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

/// An owner's handle to its tree's pump.
#[derive(Debug)]
pub(crate) struct Pump {
    control: Arc<Control>,
    task: Option<JoinHandle<()>>,
}

impl Pump {
    /// Starts the pump on `runtime`. Synchronous: the caller keeps the
    /// returned handle in the same poll.
    pub(crate) fn start(tree: Weak<ProcessTree>, runtime: &Handle) -> Self {
        let control = Arc::new(Control::default());
        let task = runtime.spawn(run(tree, Arc::clone(&control)));
        Self {
            control,
            task: Some(task),
        }
    }

    /// Stops the pump and waits until its task has ended. Cancel safe: the
    /// task stays owned here until it is seen ended, and the next call
    /// resumes the wait. The task ends within one step of the request.
    pub(crate) async fn stop(&mut self) {
        self.control.request_stop();
        if let Some(task) = &mut self.task {
            // A panic or a runtime shutdown ends it too.
            let _ = task.await;
            self.task = None;
        }
    }

    /// Stops the pump without waiting: the kill-only fallback of a drop.
    pub(crate) fn abort(&mut self) {
        self.control.request_stop();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    /// The control, for the crate's own tests: its strong count falls to
    /// the test's own once the task has ended and this handle is gone.
    #[cfg(test)]
    pub(crate) fn control(&self) -> Arc<Control> {
        Arc::clone(&self.control)
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.abort();
    }
}

async fn run(tree: Weak<ProcessTree>, control: Arc<Control>) {
    let mut pause = MIN_PAUSE;
    loop {
        if control.stopping() {
            #[cfg(test)]
            control.pass_stop_barrier().await;
            return;
        }
        // Upgraded for one synchronous step, released before any await.
        let step = {
            let Some(tree) = tree.upgrade() else {
                return;
            };
            tree.pump_step()
        };
        match step {
            Ok(Step { empty: false, .. }) => {
                pause = MIN_PAUSE;
                tokio::task::yield_now().await;
                continue;
            }
            Ok(Step { taken, .. }) if taken > 0 => pause = MIN_PAUSE,
            // Idle, or the port could not be read: wait, then try again.
            _ => {}
        }
        tokio::select! {
            () = tokio::time::sleep(pause) => {}
            () = control.wake.notified() => {}
        }
        pause = (pause * 2).min(MAX_PAUSE);
    }
}
