//! Limits shared by every model request belonging to one caller-owned run.
//!
//! Logical calls are admissions to a [`crate::StreamFn`], including summary
//! and retry attempts. Transport retries inside a provider are not separate
//! logical calls. A stopped run refuses admission; its owner still awaits
//! cancellation and cleanup before publishing an outcome.

use std::future::poll_fn;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Wake, Waker};

use bake_ai::{AbortController, AbortSignal, Model};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use crate::BoxFuture;

/// Opt-in limits for one execution interval. Cloning its [`RunControl`]
/// preserves both the original deadline and the consumed allowance.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunLimits {
    /// Absolute monotonic expiry, including preparation, retries and tools.
    pub deadline: Option<Instant>,
    /// Maximum admitted logical model calls; `Some(0)` admits none.
    pub max_model_calls: Option<u64>,
}

/// The local limit that stopped an execution interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetLimit {
    /// The absolute monotonic deadline expired.
    Deadline,
    /// Another model call would exceed the shared allowance.
    ModelCalls,
}

/// Why one execution interval ended, independently of a model stop reason
/// or any claim that the user's task has been verified complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunOutcome {
    /// The executor finished its current turn and queued continuations.
    FinishedTurn,
    /// Its owner requested cancellation.
    Cancelled,
    /// A local execution limit stopped admission and cancelled active work.
    BudgetExhausted {
        /// Which limit was reached.
        limit: BudgetLimit,
    },
    /// Execution or a required durable write failed.
    Failed {
        /// The failure reported by the owner.
        message: String,
    },
}

impl RunOutcome {
    /// Whether this outcome stopped work through cancellation.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled | Self::BudgetExhausted { .. })
    }
}

impl std::fmt::Display for RunOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FinishedTurn => formatter.write_str("Finished turn"),
            Self::Cancelled => formatter.write_str("Run cancelled"),
            Self::BudgetExhausted {
                limit: BudgetLimit::Deadline,
            } => formatter.write_str("Run deadline reached"),
            Self::BudgetExhausted {
                limit: BudgetLimit::ModelCalls,
            } => formatter.write_str("Run model-call allowance exhausted"),
            Self::Failed { message } => formatter.write_str(message),
        }
    }
}

/// The request's purpose, recorded by a future durable admission journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCallKind {
    /// A foreground agent request, including an agent retry.
    Agent,
    /// A summary request, including each split summary and summary retry.
    Summary,
    /// A caller's explicit verification request.
    Verification,
}

/// One reserved debit. Its ordinal is never reused, including on failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCallAdmission {
    /// One-based ordinal within the shared run.
    pub ordinal: u64,
    /// Why the request was admitted.
    pub kind: ModelCallKind,
    /// Requested provider.
    pub provider: String,
    /// Requested model.
    pub model: String,
}

/// Required work before an admitted model call may start. The future journal
/// uses this seam to sync a debit. Failure stops the run; a reserved debit is
/// never refunded. RunControl retains and drains this future when its admission
/// waiter is cancelled. The observer must await its own I/O and cleanup.
pub type AdmissionObserver =
    Arc<dyn Fn(ModelCallAdmission) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

#[derive(Default)]
struct State {
    calls: u64,
    stopped: Option<RunOutcome>,
    stopped_at: Option<Instant>,
}

struct Inner {
    limits: RunLimits,
    state: Mutex<State>,
    admission: Mutex<Option<Arc<PendingWrite>>>,
    observer: Option<AdmissionObserver>,
    abort: AbortController,
}

struct Observation {
    future: Option<BoxFuture<'static, Result<(), String>>>,
    result: Option<Result<(), RunOutcome>>,
}

struct PendingWrite {
    observation: Mutex<Observation>,
    wake: Arc<WriteWake>,
    abort_after: Mutex<Vec<tokio::task::AbortHandle>>,
}

/// The observer always receives this stable waker. Dropping its most recent
/// waiter cannot strand another waiter that is still driving the same write.
#[derive(Default)]
struct WriteWake(Mutex<Vec<Waker>>);

impl WriteWake {
    fn register(&self, waker: &Waker) {
        let mut waiters = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if !waiters.iter().any(|old| old.will_wake(waker)) {
            waiters.push(waker.clone());
        }
    }

    fn notify(&self) {
        let waiters = std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner));
        for waiter in waiters {
            waiter.wake();
        }
    }
}

impl Wake for WriteWake {
    fn wake(self: Arc<Self>) {
        self.notify();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.notify();
    }
}

struct AdmissionWait<'a> {
    control: &'a RunControl,
    completed: bool,
}

impl Drop for AdmissionWait<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.control.cancel();
        }
    }
}

enum AdmissionStep {
    Wait(Arc<PendingWrite>),
    Reserved(ModelCallAdmission, Option<Arc<PendingWrite>>),
}

/// Shared allowance and cancellation for one caller-owned execution interval.
/// A finished turn does not renew or close it, so a later prompt can share it.
#[derive(Clone)]
pub struct RunControl(Arc<Inner>);

impl Default for RunControl {
    fn default() -> Self {
        Self::new(RunLimits::default())
    }
}

impl std::fmt::Debug for RunControl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunControl")
            .field("limits", &self.0.limits)
            .field("model_calls", &self.model_calls())
            .finish_non_exhaustive()
    }
}

impl RunControl {
    /// An interval with `limits`, without a durable admission observer.
    pub fn new(limits: RunLimits) -> Self {
        Self::with_admission_observer(limits, None)
    }

    /// An interval whose model debits must pass `observer` before dispatch.
    pub fn with_admission_observer(limits: RunLimits, observer: Option<AdmissionObserver>) -> Self {
        Self::restore(limits, 0, observer)
    }

    /// Restores an interval's original limits and already-spent debits.
    /// The caller must validate its durable journal and deadline continuity;
    /// this constructor never refunds calls, even when they exceed the limit.
    pub fn restore(
        limits: RunLimits,
        model_calls: u64,
        observer: Option<AdmissionObserver>,
    ) -> Self {
        Self(Arc::new(Inner {
            limits,
            state: Mutex::new(State {
                calls: model_calls,
                ..State::default()
            }),
            admission: Mutex::new(None),
            observer,
            abort: AbortController::new(),
        }))
    }

    /// The unchanged limits supplied at construction.
    pub fn limits(&self) -> RunLimits {
        self.0.limits
    }

    /// Debits reserved so far, including failed admission observers.
    pub fn model_calls(&self) -> u64 {
        self.0
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .calls
    }

    /// Whether `calls` more logical requests currently fit. This preflight
    /// estimate reserves nothing; actual admission rechecks under its lock.
    pub fn has_model_call_allowance(&self, calls: u64) -> bool {
        if self.stopped().is_some() {
            return false;
        }
        let state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.stopped.is_none() && self.calls_fit(state.calls, calls)
    }

    fn calls_fit(&self, used: u64, additional: u64) -> bool {
        used.checked_add(additional).is_some_and(|total| {
            self.0
                .limits
                .max_model_calls
                .is_none_or(|maximum| total <= maximum)
        })
    }

    pub(crate) fn abort_after_admissions(&self, task: tokio::task::AbortHandle) {
        let active = self
            .0
            .admission
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(pending) = active.as_ref() {
            pending
                .abort_after
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(task);
        } else {
            task.abort();
        }
    }

    /// Finish required admission writes, including one whose original waiter
    /// was dropped or remains parked. No background task is spawned. Owners
    /// cancel first when closing the interval, then await this drain before
    /// publishing a final outcome or dropping the last control. A late write
    /// failure outranks cancellation. Returns the resulting stopping cause.
    pub async fn drain_admissions(&self) -> Option<RunOutcome> {
        loop {
            let pending = self
                .0
                .admission
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let Some(pending) = pending else {
                return self.stopped();
            };
            let _ = self.observe(&pending).await;
        }
    }

    async fn observe(&self, pending: &Arc<PendingWrite>) -> Result<(), RunOutcome> {
        poll_fn(|cx| {
            let mut observation = pending
                .observation
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(result) = &observation.result {
                return Poll::Ready(result.clone());
            }
            pending.wake.register(cx.waker());
            let waker = Waker::from(Arc::clone(&pending.wake));
            let mut context = Context::from_waker(&waker);
            let result = match observation.future.as_mut() {
                Some(future) => match future.as_mut().poll(&mut context) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(result) => result.map_err(|error| self.fail(error)),
                },
                None => Err(self.fail("Required admission write lost its future".into())),
            };
            observation.future = None;
            observation.result = Some(result.clone());
            // Publish failure before releasing admission order. The mutex is
            // held only during a poll, so another owner can take over a parked
            // caller's write without dropping or recreating its I/O future.
            let mut active = self
                .0
                .admission
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if active
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, pending))
            {
                *active = None;
            }
            drop(active);
            drop(observation);
            let abort_after = std::mem::take(
                &mut *pending
                    .abort_after
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner),
            );
            for task in abort_after {
                task.abort();
            }
            pending.wake.notify();
            Poll::Ready(result)
        })
        .await
    }

    /// The monotonic moment admission stopped. Cleanup is measured from
    /// here separately from the execution deadline.
    pub fn stopped_at(&self) -> Option<Instant> {
        self.0
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stopped_at
    }

    /// The signal owners link to their existing cancellation controllers.
    pub fn signal(&self) -> AbortSignal {
        self.0.abort.signal()
    }

    fn stop(&self, outcome: RunOutcome, required_failure: bool) -> RunOutcome {
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.stopped.is_none()
            || (required_failure && !matches!(state.stopped, Some(RunOutcome::Failed { .. })))
        {
            state.stopped = Some(outcome.clone());
            state.stopped_at.get_or_insert_with(Instant::now);
        }
        let outcome = state.stopped.clone().unwrap_or(outcome);
        drop(state);
        self.0.abort.abort();
        outcome
    }

    /// Requests cancellation without replacing an earlier stopping cause.
    pub fn cancel(&self) {
        self.stop(RunOutcome::Cancelled, false);
    }

    /// Records a required-work failure. In particular, a failed durable
    /// write outranks cancellation or a deadline that raced with the write.
    pub fn fail(&self, message: String) -> RunOutcome {
        self.stop(RunOutcome::Failed { message }, true)
    }

    /// Resolves the owner's final outcome against the first stopping cause.
    /// A finished turn leaves the shared interval open for another prompt.
    /// Required persistence failures use [`Self::fail`] instead.
    pub fn settle(&self, outcome: RunOutcome) -> RunOutcome {
        if let Some(stopped) = self.stopped() {
            return stopped;
        }
        match outcome {
            RunOutcome::FinishedTurn => outcome,
            _ => self.stop(outcome, false),
        }
    }

    /// Closes a dedicated interval once all its producers have settled.
    /// Cancellation and expiry before this boundary win; later cancellation
    /// cannot replace completion. Required writes may still fail through
    /// [`Self::fail`]. Shared multi-prompt owners should use [`Self::settle`].
    pub fn finish_interval(&self, outcome: RunOutcome) -> RunOutcome {
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.stopped.is_none() {
            let outcome = if self
                .0
                .limits
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                RunOutcome::BudgetExhausted {
                    limit: BudgetLimit::Deadline,
                }
            } else {
                outcome
            };
            state.stopped = Some(outcome);
            state.stopped_at.get_or_insert_with(Instant::now);
        }
        let outcome = state.stopped.clone().unwrap_or(RunOutcome::Cancelled);
        drop(state);
        self.0.abort.abort();
        outcome
    }

    /// The stopping cause, checking the original deadline before returning.
    pub fn stopped(&self) -> Option<RunOutcome> {
        if self
            .0
            .limits
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.stop(
                RunOutcome::BudgetExhausted {
                    limit: BudgetLimit::Deadline,
                },
                false,
            );
        }
        self.0
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stopped
            .clone()
    }

    /// Waits for cancellation or expiry. No timer task outlives the waiter.
    pub async fn wait_stopped(&self) -> RunOutcome {
        if let Some(outcome) = self.stopped() {
            return outcome;
        }
        let signal = self.signal();
        match self.0.limits.deadline {
            Some(deadline) => tokio::select! {
                () = signal.cancelled() => {}
                () = tokio::time::sleep_until(deadline) => {
                    self.stop(RunOutcome::BudgetExhausted { limit: BudgetLimit::Deadline }, false);
                }
            },
            None => signal.cancelled().await,
        }
        self.stopped().unwrap_or(RunOutcome::Cancelled)
    }

    /// Reserves and observes one debit immediately before dispatch. Every
    /// clone shares this serialized boundary; no denied call reaches a model.
    pub async fn admit_model_call(
        &self,
        kind: ModelCallKind,
        model: &Model,
    ) -> Result<ModelCallAdmission, RunOutcome> {
        match self.admit(kind, model, None).await? {
            Some(admission) => Ok(admission),
            None => Err(self.fail("Required model admission returned no debit".into())),
        }
    }

    /// Admit speculative work only when one call plus `reserve` still fits.
    /// `Ok(None)` spends nothing and leaves foreground admission open. Actual
    /// debits use the same required observer, serialization, and no-refund rule.
    pub async fn try_admit_model_call(
        &self,
        kind: ModelCallKind,
        model: &Model,
        reserve: u64,
    ) -> Result<Option<ModelCallAdmission>, RunOutcome> {
        self.admit(kind, model, Some(reserve)).await
    }

    async fn admit(
        &self,
        kind: ModelCallKind,
        model: &Model,
        reserve: Option<u64>,
    ) -> Result<Option<ModelCallAdmission>, RunOutcome> {
        loop {
            let step = {
                let mut active = self
                    .0
                    .admission
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                match active.as_ref() {
                    Some(pending) => AdmissionStep::Wait(Arc::clone(pending)),
                    None => {
                        let Some(admission) = self.reserve(kind, model, reserve)? else {
                            return Ok(None);
                        };
                        let pending = self.0.observer.as_ref().map(|observer| {
                            let observer = Arc::clone(observer);
                            let observed = admission.clone();
                            let future = crate::agent_loop::catching(Box::pin(async move {
                                observer(observed).await
                            }));
                            Arc::new(PendingWrite {
                                observation: Mutex::new(Observation {
                                    future: Some(Box::pin(future)),
                                    result: None,
                                }),
                                wake: Arc::new(WriteWake::default()),
                                abort_after: Mutex::new(Vec::new()),
                            })
                        });
                        *active = pending.clone();
                        AdmissionStep::Reserved(admission, pending)
                    }
                }
            };
            match step {
                AdmissionStep::Wait(pending) => {
                    self.observe(&pending).await?;
                }
                AdmissionStep::Reserved(admission, pending) => {
                    if let Some(pending) = pending {
                        let mut waiter = AdmissionWait {
                            control: self,
                            completed: false,
                        };
                        let result = self.observe(&pending).await;
                        waiter.completed = true;
                        result?;
                    }
                    return match self.stopped() {
                        Some(outcome) => Err(outcome),
                        None => Ok(Some(admission)),
                    };
                }
            }
        }
    }

    fn reserve(
        &self,
        kind: ModelCallKind,
        model: &Model,
        reserve: Option<u64>,
    ) -> Result<Option<ModelCallAdmission>, RunOutcome> {
        if let Some(outcome) = self.stopped() {
            return Err(outcome);
        }
        let ordinal = {
            let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(outcome) = &state.stopped {
                return Err(outcome.clone());
            }
            let fits = reserve
                .unwrap_or(0)
                .checked_add(1)
                .is_some_and(|additional| self.calls_fit(state.calls, additional));
            if !fits {
                drop(state);
                if reserve.is_some() {
                    return Ok(None);
                }
                return Err(self.stop(
                    RunOutcome::BudgetExhausted {
                        limit: BudgetLimit::ModelCalls,
                    },
                    false,
                ));
            }
            state.calls += 1;
            state.calls
        };
        Ok(Some(ModelCallAdmission {
            ordinal,
            kind,
            provider: model.provider.clone(),
            model: model.id.clone(),
        }))
    }
}
