//! The session's owner of the work it runs outside the task awaiting it.
//!
//! A [`WorkGate`] is one session's lifetime. Work joins it in two ways:
//!
//! - A [`WorkPermit`] from [`WorkGate::enter`] is a claim held by work the
//!   caller runs itself, such as a model request on its stream task or a
//!   prompt's session read. The gate never cancels it; the turn that runs
//!   it does ([`crate::agent_session::AgentSession::abort`]).
//! - An [`OwnedTask`] from [`WorkGate::spawn`] or [`WorkPermit::spawn`] is a
//!   Tokio task the gate owns. It receives a [`TaskScope`] whose signal is
//!   aborted when the gate closes or the task, or the task that spawned it,
//!   is cancelled. A [`BlockingTask`] from `spawn_blocking` runs on Tokio's
//!   blocking pool and cannot be cancelled; it is only awaited.
//!
//! The only cancellations that reach owned work are [`OwnedTask::cancel`]
//! and [`WorkGate::close`]. Cancelling one task cancels the tasks spawned
//! from its scope's permit, transitively, even through a task that has
//! ended, and its retained permits then admit no task; closing the gate
//! cancels them all. Dropping an
//! [`OwnedTask`] or [`BlockingTask`] stops nothing: it gives up the result,
//! and the gate still owns, and waits for, the work.
//!
//! [`WorkGate::close`] refuses new admission as soon as it is called, before
//! the future it returns is polled, cancels every owned task, and resolves
//! once every permit is dropped. A permit is dropped only after the future
//! or closure holding it has been dropped, including its locals' cleanup, so
//! when `close` resolves no admitted work is running and none can start.
//! Closing is a state of the gate, not of the future that waits for it:
//! dropping that future neither reopens the gate nor stops the drain, and
//! any number of callers may close and wait concurrently.
//!
//! Work already admitted may extend its claim while the gate closes: a
//! permit's [`WorkPermit::share`] and [`WorkPermit::spawn_blocking`] are
//! never refused, so a cancelled change can still finish the write it
//! started. A new async task is refused once the gate is closing or its
//! parent task is cancelled, as Chord refuses an aborted task context's
//! fresh calls.
//!
//! Bake's own. Pi's Node runtime has no detached work to await; the
//! memoized, caller-independent close and the cancel-then-drain order follow
//! the object disposal of Pi's `packages/chord/src/object.ts` (v1.3.0,
//! `71023aec`; MIT, see the crate `NOTICE`), and the tests marked `Chord`
//! adapt cases of its `test/host.test.ts` and `test/review.test.ts`.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::task::{Context, Poll};

use bake_ai::{AbortController, AbortSignal};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// Admits a session's work until closed, then cancels and waits for it.
#[derive(Clone, Default)]
pub struct WorkGate {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    /// Notified each time the last permit is dropped.
    drained: Notify,
}

#[derive(Default)]
struct State {
    closing: bool,
    permits: usize,
    /// Tasks spawned with no owning task; dead entries are pruned as tasks
    /// are added.
    roots: Vec<Weak<Node>>,
}

/// One owned task in the gate's cancellation tree.
///
/// A node is held by its task's future, its scope and every permit shared
/// from that scope, its [`OwnedTask`] handle, and each child's node. It
/// therefore outlives the task's own future for exactly as long as
/// something can still spawn under it or a descendant is still tracked,
/// so cancelling an ancestor reaches a grandchild through a task that has
/// ended, and a retained permit of a cancelled task stays refused; once
/// none of those remain it is freed, so a long session keeps only live
/// work. Parents hold their children weakly, so the tree has no cycle.
///
/// The tree changes and is walked only under the gate's state lock, which
/// makes admission and cancellation atomic with each other.
struct Node {
    controller: AbortController,
    /// Kept for its ownership of the parent; never read.
    _parent: Option<Arc<Node>>,
    children: Mutex<Vec<Weak<Node>>>,
}

impl Node {
    fn cancelled(&self) -> bool {
        self.controller.signal().aborted()
    }

    fn children(&self) -> MutexGuard<'_, Vec<Weak<Node>>> {
        self.children.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Adds `node` to `list`, dropping entries whose node is gone.
fn adopt(list: &mut Vec<Weak<Node>>, node: &Arc<Node>) {
    list.retain(|entry| entry.strong_count() > 0);
    list.push(Arc::downgrade(node));
}

/// Aborts each node in `start` and every live descendant. Call with the
/// gate's state lock held.
fn cancel_tree(start: impl IntoIterator<Item = Arc<Node>>) {
    let mut pending: Vec<Arc<Node>> = start.into_iter().collect();
    while let Some(node) = pending.pop() {
        node.controller.abort();
        pending.extend(node.children().iter().filter_map(Weak::upgrade));
    }
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Why work was not admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// The gate is closing or closed.
    Closed,
    /// The task that would own it was cancelled.
    Cancelled,
    /// The caller is not inside a Tokio runtime.
    NoRuntime,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Closed => "the session is shut down",
            Self::Cancelled => "the owning task was cancelled",
            Self::NoRuntime => "owned work must start inside a Tokio runtime",
        })
    }
}

impl std::error::Error for Refused {}

/// An owned task or blocking job panicked, or its runtime shut down before
/// it finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskFailed(String);

impl fmt::Display for TaskFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TaskFailed {}

impl From<tokio::task::JoinError> for TaskFailed {
    fn from(error: tokio::task::JoinError) -> Self {
        Self(error.to_string())
    }
}

/// A claim that keeps the gate's close waiting. Dropping it ends the claim.
pub struct WorkPermit {
    inner: Arc<Inner>,
    /// The owned task whose scope this permit belongs to, if any.
    node: Option<Arc<Node>>,
}

impl WorkGate {
    /// A permit, or `None` once the gate is closing or closed.
    pub fn enter(&self) -> Option<WorkPermit> {
        let mut state = self.inner.state();
        if state.closing {
            return None;
        }
        state.permits += 1;
        Some(WorkPermit {
            inner: Arc::clone(&self.inner),
            node: None,
        })
    }

    /// Spawns `work` as a task the gate owns ([`WorkPermit::spawn`]).
    /// Refused once the gate is closing.
    pub fn spawn<T, F, Fut>(&self, work: F) -> Result<OwnedTask<T>, Refused>
    where
        T: Send + 'static,
        F: FnOnce(TaskScope) -> Fut,
        Fut: Future<Output = T> + Send + 'static,
    {
        self.enter().ok_or(Refused::Closed)?.spawn(work)
    }

    /// Runs `work` on Tokio's blocking pool under a permit
    /// ([`WorkPermit::spawn_blocking`]). Refused once the gate is closing.
    pub fn spawn_blocking<T, F>(&self, work: F) -> Result<BlockingTask<T>, Refused>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        self.enter().ok_or(Refused::Closed)?.spawn_blocking(work)
    }

    /// Closes the gate: refuses new admission and cancels every owned task
    /// at once, when called, then returns a future that resolves once every
    /// permit has been dropped. Dropping the future leaves the gate closing
    /// and its work draining; calling `close` again, from any caller, waits
    /// for the same drain.
    pub fn close(&self) -> impl Future<Output = ()> + Send + 'static {
        {
            let mut state = self.inner.state();
            state.closing = true;
            let roots: Vec<Arc<Node>> = state.roots.iter().filter_map(Weak::upgrade).collect();
            cancel_tree(roots);
        }
        let inner = Arc::clone(&self.inner);
        async move {
            loop {
                let notified = inner.drained.notified();
                tokio::pin!(notified);
                // Registered before the check, so a release between the
                // check and the await is not missed.
                notified.as_mut().enable();
                if inner.state().permits == 0 {
                    return;
                }
                notified.await;
            }
        }
    }

    /// The length of the gate's root list, live or not yet pruned.
    #[cfg(test)]
    fn tracked_tasks(&self) -> usize {
        self.inner.state().roots.len()
    }

    /// Whether [`Self::close`] was called: no new work is admitted.
    pub fn is_closing(&self) -> bool {
        self.inner.state().closing
    }

    /// Whether the gate is closing and every permit has been dropped.
    pub fn is_closed(&self) -> bool {
        let state = self.inner.state();
        state.closing && state.permits == 0
    }
}

impl fmt::Debug for WorkGate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.inner.state();
        f.debug_struct("WorkGate")
            .field("closing", &state.closing)
            .field("permits", &state.permits)
            .field("roots", &state.roots.len())
            .finish()
    }
}

impl WorkPermit {
    /// Another permit for the same claim: never refused, even while the
    /// gate closes, since this one already holds the close open.
    pub fn share(&self) -> WorkPermit {
        self.inner.state().permits += 1;
        WorkPermit {
            inner: Arc::clone(&self.inner),
            node: self.node.clone(),
        }
    }

    /// Spawns `work` on the current Tokio runtime as a task the gate owns,
    /// a child of this permit's task, if any. `work` is called at once with
    /// the task's [`TaskScope`]; the future it returns runs on the task.
    ///
    /// Refused with [`Refused::Closed`] once the gate is closing, with
    /// [`Refused::Cancelled`] once this permit's task is cancelled, and
    /// with [`Refused::NoRuntime`] outside a runtime; `work` is not called
    /// then.
    pub fn spawn<T, F, Fut>(&self, work: F) -> Result<OwnedTask<T>, Refused>
    where
        T: Send + 'static,
        F: FnOnce(TaskScope) -> Fut,
        Fut: Future<Output = T> + Send + 'static,
    {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| Refused::NoRuntime)?;
        let node = {
            let mut state = self.inner.state();
            if state.closing {
                return Err(Refused::Closed);
            }
            // The parent's node lives as long as this permit, so a parent
            // cancelled after its task ended is still seen here.
            if self.node.as_ref().is_some_and(|parent| parent.cancelled()) {
                return Err(Refused::Cancelled);
            }
            let node = Arc::new(Node {
                controller: AbortController::new(),
                _parent: self.node.clone(),
                children: Mutex::default(),
            });
            match &self.node {
                Some(parent) => adopt(&mut parent.children(), &node),
                None => adopt(&mut state.roots, &node),
            }
            // The task's own permit and its scope's.
            state.permits += 2;
            node
        };
        // Built before `work` runs, so a panic in it releases both.
        let task_permit = WorkPermit {
            inner: Arc::clone(&self.inner),
            node: Some(Arc::clone(&node)),
        };
        let scope = TaskScope {
            signal: node.controller.signal(),
            permit: WorkPermit {
                inner: Arc::clone(&self.inner),
                node: Some(Arc::clone(&node)),
            },
        };
        let handle = runtime.spawn(Owned {
            work: Box::pin(work(scope)),
            _permit: task_permit,
        });
        Ok(OwnedTask {
            handle,
            inner: Arc::clone(&self.inner),
            node,
        })
    }

    /// Runs `work` on Tokio's blocking pool under a share of this permit.
    /// Never refused for a closing gate or a cancelled task; only outside a
    /// Tokio runtime ([`Refused::NoRuntime`]). The work cannot be
    /// cancelled.
    pub fn spawn_blocking<T, F>(&self, work: F) -> Result<BlockingTask<T>, Refused>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| Refused::NoRuntime)?;
        let permit = self.share();
        let handle = runtime.spawn_blocking(move || {
            // `work` and what it captured are dropped by the call, before
            // the permit, which also drops while unwinding.
            let output = work();
            drop(permit);
            output
        });
        Ok(BlockingTask { handle })
    }
}

impl Drop for WorkPermit {
    fn drop(&mut self) {
        let mut state = self.inner.state();
        state.permits -= 1;
        if state.permits == 0 {
            self.inner.drained.notify_waiters();
        }
    }
}

impl fmt::Debug for WorkPermit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkPermit")
            .field("owned_by_task", &self.node.is_some())
            .finish()
    }
}

/// What an owned task receives: its cancellation signal and a permit of
/// its own, from which it spawns children it owns.
#[derive(Debug)]
pub struct TaskScope {
    signal: AbortSignal,
    permit: WorkPermit,
}

impl TaskScope {
    /// Aborted when this task, a task that owns it, or the gate is
    /// cancelled. Ending the work is up to the task; the gate waits for it
    /// either way.
    pub fn signal(&self) -> &AbortSignal {
        &self.signal
    }

    /// This task's permit: tasks spawned from it are its children.
    pub fn permit(&self) -> &WorkPermit {
        &self.permit
    }
}

/// An owned task's future. Fields drop in order: the work and everything
/// it holds first, then the task's permit, so the gate's drain covers the
/// work's cleanup however the task ends.
struct Owned<F> {
    work: Pin<Box<F>>,
    _permit: WorkPermit,
}

impl<F: Future> Future for Owned<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        self.work.as_mut().poll(cx)
    }
}

/// A task the gate owns. Awaiting it yields the task's output; dropping it
/// gives up the output but leaves the task running and owned.
#[must_use = "dropping an OwnedTask detaches its output; the gate still owns and awaits the task"]
pub struct OwnedTask<T> {
    handle: JoinHandle<T>,
    inner: Arc<Inner>,
    node: Arc<Node>,
}

impl<T> OwnedTask<T> {
    /// Aborts the signal of this task and of every task spawned from its
    /// scope, transitively, including tasks spawned through a task that
    /// has since ended and from permits its scope shared; those permits
    /// then admit no new task. The tasks still run to their end, which the
    /// gate awaits.
    pub fn cancel(&self) {
        let _state = self.inner.state();
        cancel_tree([Arc::clone(&self.node)]);
    }

    /// Whether Tokio has marked the task finished. That follows the drop
    /// of its work, and so may trail [`WorkGate::close`] resolving.
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }
}

impl<T> Future for OwnedTask<T> {
    type Output = Result<T, TaskFailed>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.handle)
            .poll(cx)
            .map_err(TaskFailed::from)
    }
}

impl<T> fmt::Debug for OwnedTask<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnedTask")
            .field("cancelled", &self.node.cancelled())
            .finish()
    }
}

/// Blocking work the gate awaits. It cannot be cancelled; dropping this
/// gives up the output only.
#[must_use = "dropping a BlockingTask detaches its output; the gate still awaits the work"]
#[derive(Debug)]
pub struct BlockingTask<T> {
    handle: JoinHandle<T>,
}

impl<T> Future for BlockingTask<T> {
    type Output = Result<T, TaskFailed>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.handle)
            .poll(cx)
            .map_err(TaskFailed::from)
    }
}

#[cfg(test)]
mod tests;
