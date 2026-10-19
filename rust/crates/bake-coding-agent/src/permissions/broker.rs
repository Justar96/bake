//! The interactive answerer: approval requests wait in order for a person
//! to answer them through a terminal.
//!
//! Adapted from Bake's TypeScript `Interactions` queue
//! (`apps/tui/packages/app/src/interactions.ts`), the approval half: each
//! request takes a fresh identity, the oldest is the one displayed and a
//! later request cannot replace it, an answer settles only the displayed
//! request it names, so a stale answer settles nothing, a request's own
//! cancellation withdraws it, and closing settles every request as
//! `cancelled` and fails later ones closed. The answer is `allowed-once` or
//! `rejected`, as `interactions.ts` decodes it, or `cancelled` for a
//! withdrawal.
//!
//! [`ApprovalBroker`] sits behind [`super::PolicyApprover`], so the session's
//! `never` policy rejects a request before it is queued, and the grant is
//! for the one call the request names. It grants nothing else: no standing
//! permission, preset, or later call changes.
//!
//! One frontend [connects](ApprovalBroker::connect) a listener that
//! receives the whole queue, oldest first, whenever it changes. No lock is
//! held while the listener runs, so it may call back into the broker; one
//! caller at a time announces, and a change made meanwhile is announced by
//! that caller once its listener returns, so the listener sees the queue's
//! states in order, ending with the latest, as `AgentSession`'s queue
//! announcements do. A listener that reports the frontend gone, or panics,
//! closes the broker. Before a frontend connects, nobody can answer, so a
//! request is `unavailable`; a second frontend cannot connect.
//!
//! Cancellation outranks an answer: a request whose signal has aborted is
//! settled `cancelled` even when an answer arrives at the same moment.

use std::collections::VecDeque;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use bake_ai::AbortSignal;
use bake_sandbox::EscalationOutcome;
use tokio::sync::oneshot;

use serde_json::Value;

use super::approval::{ApprovalFuture, ApprovalRequest, Approver};

/// One queued approval request as a frontend shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalPrompt {
    /// The request's identity, unique for the broker's life. Two requests
    /// for the same call id still differ here, so an answer meant for one
    /// never settles the other.
    pub generation: u64,
    /// The tool the request is about.
    pub tool_name: String,
    /// The exact tool call being decided.
    pub call_id: String,
    /// The tool call that made this one, when a tool such as code mode ran
    /// it through its nested-call executor.
    pub parent_tool_call_id: Option<String>,
    /// Why the tool asks, such as `escalate sandbox to …: <justification>`.
    pub reason: String,
    /// The call's arguments, so the person sees what would run.
    pub arguments: Value,
}

/// A person's answer to the displayed request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApprovalAnswer {
    /// `allowed-once`: the one call runs with the access it asked for.
    AllowOnce,
    /// `rejected`.
    Reject,
    /// `cancelled`: the request is withdrawn without a decision.
    Cancel,
}

impl ApprovalAnswer {
    fn outcome(self) -> EscalationOutcome {
        match self {
            ApprovalAnswer::AllowOnce => EscalationOutcome::AllowedOnce,
            ApprovalAnswer::Reject => EscalationOutcome::Rejected,
            ApprovalAnswer::Cancel => EscalationOutcome::Cancelled,
        }
    }
}

/// Receives the queue, oldest first, whenever it changes; returns `false`
/// once the frontend is gone.
pub type ApprovalListener = Box<dyn Fn(Vec<ApprovalPrompt>) -> bool + Send + Sync>;

enum Phase {
    /// No frontend yet: nobody can answer.
    Unconnected,
    Open(Arc<ApprovalListener>),
    /// The frontend has gone: nobody can answer again.
    Closed,
}

struct Waiting {
    prompt: ApprovalPrompt,
    answer: oneshot::Sender<EscalationOutcome>,
    cancel: Option<AbortSignal>,
}

struct Inner {
    phase: Phase,
    queue: VecDeque<Waiting>,
    /// The last generation handed out.
    last: u64,
    /// Whether a caller is announcing, and whether the queue changed since
    /// its last snapshot.
    announcing: bool,
    changed: bool,
}

impl Inner {
    /// Settles every queued request as `cancelled`; later requests are
    /// `unavailable`.
    fn close(&mut self) {
        self.phase = Phase::Closed;
        for waiting in self.queue.drain(..) {
            let _ = waiting.answer.send(EscalationOutcome::Cancelled);
        }
    }
}

/// Queues approval requests for a person to answer, in the order they were
/// asked; see the [module documentation](self).
#[derive(Clone)]
pub struct ApprovalBroker {
    inner: Arc<Mutex<Inner>>,
}

impl Default for ApprovalBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ApprovalBroker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApprovalBroker")
            .field("pending", &self.pending().len())
            .finish_non_exhaustive()
    }
}

impl ApprovalBroker {
    /// A broker no frontend has connected to yet.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                phase: Phase::Unconnected,
                queue: VecDeque::new(),
                last: 0,
                announcing: false,
                changed: false,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        lock(&self.inner)
    }

    /// Connects the one frontend that shows the queue and answers it, and
    /// sends it the queue at once. Returns `false`, connecting nothing,
    /// when a frontend has already connected or the broker is closed.
    pub fn connect(&self, listener: ApprovalListener) -> bool {
        {
            let mut inner = self.lock();
            if !matches!(inner.phase, Phase::Unconnected) {
                return false;
            }
            inner.phase = Phase::Open(Arc::new(listener));
        }
        announce(&self.inner);
        true
    }

    /// Settles the displayed request, the oldest, with `answer`, if it is
    /// the request `generation` names. Returns whether it did: an answer
    /// for a request already settled, withdrawn, or not yet displayed
    /// settles nothing.
    /// A request whose own cancellation has fired settles `cancelled`
    /// whatever the answer, and the answer reports `false`.
    pub fn answer(&self, generation: u64, answer: ApprovalAnswer) -> bool {
        let settled = {
            let mut inner = self.lock();
            if inner
                .queue
                .front()
                .is_none_or(|front| front.prompt.generation != generation)
            {
                return false;
            }
            let Some(waiting) = inner.queue.pop_front() else {
                return false;
            };
            if waiting.cancel.as_ref().is_some_and(AbortSignal::aborted) {
                let _ = waiting.answer.send(EscalationOutcome::Cancelled);
                false
            } else {
                waiting.answer.send(answer.outcome()).is_ok()
            }
        };
        announce(&self.inner);
        settled
    }

    /// The queue, oldest first.
    pub fn pending(&self) -> Vec<ApprovalPrompt> {
        self.lock().queue.iter().map(|w| w.prompt.clone()).collect()
    }

    /// Disconnects the frontend: every queued request is `cancelled`, and
    /// every later one `unavailable`. Safe to call again.
    pub fn close(&self) {
        // The listener goes with the phase, so nothing is announced.
        self.lock().close();
    }

    /// Whether the broker is closed.
    pub fn is_closed(&self) -> bool {
        matches!(self.lock().phase, Phase::Closed)
    }
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Tells the listener what waits now, or leaves that to the caller
/// already announcing. No lock is held while the listener runs; a listener
/// that is gone or panics closes the broker.
fn announce(shared: &Mutex<Inner>) {
    {
        let mut inner = lock(shared);
        if inner.announcing {
            inner.changed = true;
            return;
        }
        inner.announcing = true;
    }
    loop {
        let (listener, snapshot) = {
            let mut inner = lock(shared);
            inner.changed = false;
            let Phase::Open(listener) = &inner.phase else {
                inner.announcing = false;
                return;
            };
            let snapshot: Vec<ApprovalPrompt> =
                inner.queue.iter().map(|w| w.prompt.clone()).collect();
            (Arc::clone(listener), snapshot)
        };
        let delivered =
            std::panic::catch_unwind(AssertUnwindSafe(|| listener(snapshot))).unwrap_or(false);
        let mut inner = lock(shared);
        if !delivered {
            inner.close();
        }
        if !inner.changed {
            inner.announcing = false;
            return;
        }
    }
}

/// Withdraws its request from the queue when the request's future is
/// dropped unanswered, so a dropped future never leaves a prompt behind,
/// and its generation never answers again.
struct Withdraw {
    inner: Weak<Mutex<Inner>>,
    generation: u64,
}

impl Drop for Withdraw {
    fn drop(&mut self) {
        let Some(shared) = self.inner.upgrade() else {
            return;
        };
        let removed = {
            let mut inner = lock(&shared);
            let index = inner
                .queue
                .iter()
                .position(|w| w.prompt.generation == self.generation);
            index.and_then(|index| inner.queue.remove(index)).is_some()
        };
        if removed {
            announce(&shared);
        }
    }
}

impl Approver for ApprovalBroker {
    fn request(&self, request: ApprovalRequest, cancel: Option<AbortSignal>) -> ApprovalFuture {
        if cancel.as_ref().is_some_and(AbortSignal::aborted) {
            return Box::pin(std::future::ready(EscalationOutcome::Cancelled));
        }
        let (answer, answered) = oneshot::channel();
        let generation = {
            let mut inner = self.lock();
            // Generations never repeat: once they run out, nobody can answer.
            let next = inner.last.checked_add(1);
            let (Phase::Open(_), Some(generation)) = (&inner.phase, next) else {
                return Box::pin(std::future::ready(EscalationOutcome::Unavailable));
            };
            inner.last = generation;
            inner.queue.push_back(Waiting {
                prompt: ApprovalPrompt {
                    generation,
                    tool_name: request.tool_name,
                    call_id: request.call_id,
                    parent_tool_call_id: request.parent_tool_call_id,
                    reason: request.reason,
                    arguments: request.arguments,
                },
                answer,
                cancel: cancel.clone(),
            });
            generation
        };
        announce(&self.inner);
        let withdraw = Withdraw {
            inner: Arc::downgrade(&self.inner),
            generation,
        };
        Box::pin(async move {
            let _withdraw = withdraw;
            let withdrawn = async {
                match &cancel {
                    Some(signal) => signal.cancelled().await,
                    None => std::future::pending().await,
                }
            };
            let answer = tokio::select! {
                // Cancellation first: when both are ready, nothing is granted.
                biased;
                () = withdrawn => return EscalationOutcome::Cancelled,
                // The answer channel closes unanswered only when the broker
                // itself is gone.
                answer = answered => answer.unwrap_or(EscalationOutcome::Cancelled),
            };
            if cancel.as_ref().is_some_and(AbortSignal::aborted) {
                return EscalationOutcome::Cancelled;
            }
            answer
        })
    }
}

#[cfg(test)]
#[path = "broker_tests.rs"]
mod tests;
