//! The interactive answerer: approval requests wait in order for a person
//! to answer them through a terminal.
//!
//! Adapted from Bake's TypeScript `Interactions` queue
//! (`apps/tui/packages/app/src/interactions.ts`), the approval half: each
//! request takes a fresh identity, the oldest is the one displayed and a
//! later request cannot replace it, an answer settles only the displayed
//! request it names, so a stale answer settles nothing, a request's own
//! cancellation withdraws it, [withdrawing the
//! queue](ApprovalBroker::cancel_pending) settles every queued request as
//! `cancelled` and keeps the frontend, and closing settles every request
//! as `cancelled` and fails later ones closed. The answer is `allowed-once` or
//! `rejected`, as `interactions.ts` decodes it, or `cancelled` for a
//! withdrawal; Bake adds `allowed-session`, `allowed-full-access`, and a
//! rejection with a note, each only for a request that offers it
//! ([`super::ApprovalOffers`]).
//!
//! [`ApprovalBroker`] sits behind [`super::PolicyApprover`], so the session's
//! `never` policy rejects a request before it is queued. The broker only
//! carries the answer: what a wider answer grants, and its record, are the
//! engine's ([`super::PermissionEngine`]).
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

use super::approval::{
    ApprovalDecision, ApprovalFuture, ApprovalOffers, ApprovalRequest, Approver, DecisionFuture,
    clean_note,
};
use bake_sandbox::SandboxMode;

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
    /// The sandbox mode an allowed answer runs the call under; `None` for a
    /// hook's `ask`.
    pub sandbox: Option<SandboxMode>,
    /// The wider answers the request takes.
    pub offers: ApprovalOffers,
}

/// A person's answer to the displayed request.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ApprovalAnswer {
    /// `allowed-once`: the one call runs with the access it asked for.
    AllowOnce,
    /// `allowed-session`: this call, and later requests of the same tool
    /// for the same mode in this session.
    AllowSession,
    /// `allowed-full-access`: this call, and the session switches to full
    /// access.
    AllowFullAccess,
    /// `rejected`.
    Reject,
    /// `rejected`, with a note for the model; a blank note is none.
    RejectWithNote(String),
    /// `cancelled`: the request is withdrawn without a decision.
    Cancel,
}

impl ApprovalAnswer {
    fn decision(self) -> ApprovalDecision {
        let outcome = match self {
            ApprovalAnswer::AllowOnce => EscalationOutcome::AllowedOnce,
            ApprovalAnswer::AllowSession => EscalationOutcome::AllowedSession,
            ApprovalAnswer::AllowFullAccess => EscalationOutcome::AllowedFullAccess,
            ApprovalAnswer::Reject => EscalationOutcome::Rejected,
            ApprovalAnswer::RejectWithNote(note) => {
                return ApprovalDecision {
                    outcome: EscalationOutcome::Rejected,
                    note: clean_note(&note),
                };
            }
            ApprovalAnswer::Cancel => EscalationOutcome::Cancelled,
        };
        outcome.into()
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
    answer: oneshot::Sender<ApprovalDecision>,
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
            let _ = waiting.answer.send(EscalationOutcome::Cancelled.into());
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
    /// settles nothing, and neither does a wider answer the request does
    /// not offer, which leaves it waiting.
    /// A request whose own cancellation has fired settles `cancelled`
    /// whatever the answer, and the answer reports `false`.
    pub fn answer(&self, generation: u64, answer: ApprovalAnswer) -> bool {
        let decision = answer.decision();
        let settled = {
            let mut inner = self.lock();
            if inner.queue.front().is_none_or(|front| {
                front.prompt.generation != generation || !front.prompt.offers.take(decision.outcome)
            }) {
                return false;
            }
            let Some(waiting) = inner.queue.pop_front() else {
                return false;
            };
            if waiting.cancel.as_ref().is_some_and(AbortSignal::aborted) {
                let _ = waiting.answer.send(EscalationOutcome::Cancelled.into());
                false
            } else {
                waiting.answer.send(decision).is_ok()
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

    /// Withdraws the queue without disconnecting: every queued request is
    /// `cancelled` and a later one is asked as before. A frontend that
    /// replaces the session it asked for uses this, so each question the
    /// old session asked is decided while that session can still record
    /// the decision, and the next session asks the same frontend.
    pub fn cancel_pending(&self) {
        {
            let mut inner = self.lock();
            for waiting in inner.queue.drain(..) {
                let _ = waiting.answer.send(EscalationOutcome::Cancelled.into());
            }
        }
        // The frontend learns the queue is empty, so it forgets the
        // request it was showing.
        announce(&self.inner);
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
        let decision = self.decide(request, cancel);
        Box::pin(async move { decision.await.outcome })
    }

    fn decide(&self, request: ApprovalRequest, cancel: Option<AbortSignal>) -> DecisionFuture {
        let ready = |outcome: EscalationOutcome| -> DecisionFuture {
            Box::pin(std::future::ready(outcome.into()))
        };
        if cancel.as_ref().is_some_and(AbortSignal::aborted) {
            return ready(EscalationOutcome::Cancelled);
        }
        let (answer, answered) = oneshot::channel();
        let generation = {
            let mut inner = self.lock();
            // Generations never repeat: once they run out, nobody can answer.
            let next = inner.last.checked_add(1);
            let (Phase::Open(_), Some(generation)) = (&inner.phase, next) else {
                return ready(EscalationOutcome::Unavailable);
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
                    sandbox: request.sandbox,
                    offers: request.offers,
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
            let cancelled = || ApprovalDecision::from(EscalationOutcome::Cancelled);
            let answer = tokio::select! {
                // Cancellation first: when both are ready, nothing is granted.
                biased;
                () = withdrawn => return cancelled(),
                // The answer channel closes unanswered only when the broker
                // itself is gone.
                answer = answered => answer.unwrap_or_else(|_| cancelled()),
            };
            if cancel.as_ref().is_some_and(AbortSignal::aborted) {
                return cancelled();
            }
            answer
        })
    }
}

#[cfg(test)]
#[path = "broker_tests.rs"]
mod tests;
