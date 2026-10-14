//! Approval policy and answerers, ported from Bake's TypeScript
//! `packages/interaction/user-approval/src/{index,types}.ts`: the
//! `ApprovalPolicy` knob, the model-facing policy sentences, and the
//! service's `decide` step, which applies the policy before any answerer is
//! asked and fails closed when none can answer.
//!
//! The TypeScript service dispatched an `approval/request` waterfall to
//! composed answerers; here an answerer is one [`Approver`], and
//! [`PolicyApprover`] is the service in front of it. The `approval/asked`
//! and `approval/decided` audit events are not written (see the module
//! table in [`crate::permissions`]).

use std::fmt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};

use bake_agent::BoxFuture;
use bake_ai::AbortSignal;
use bake_sandbox::EscalationOutcome;

use super::engine::StandingPermissions;

/// What happens to an approval request before any answerer sees it
/// (`ApprovalPolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApprovalPolicy {
    /// `ask`: delegate to the answerer; with none, the request fails closed
    /// as `unavailable`.
    Ask,
    /// `never`: reject every request without asking anyone.
    Never,
}

impl ApprovalPolicy {
    /// Every policy (`APPROVAL_POLICIES`).
    pub const ALL: [ApprovalPolicy; 2] = [ApprovalPolicy::Ask, ApprovalPolicy::Never];

    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalPolicy::Ask => "ask",
            ApprovalPolicy::Never => "never",
        }
    }
}

impl fmt::Display for ApprovalPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A string that names no [`ApprovalPolicy`]; its message is
/// `setApprovalPolicy`'s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownApprovalPolicy(pub String);

impl fmt::Display for UnknownApprovalPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("approval policy must be one of \"ask\" or \"never\"")
    }
}

impl std::error::Error for UnknownApprovalPolicy {}

impl FromStr for ApprovalPolicy {
    type Err = UnknownApprovalPolicy;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        ApprovalPolicy::ALL
            .into_iter()
            .find(|policy| policy.as_str() == value)
            .ok_or_else(|| UnknownApprovalPolicy(value.to_owned()))
    }
}

/// The model-facing statement for `never` (`NEVER_SENTENCE`).
pub const NEVER_SENTENCE: &str = "Approval prompts: disabled. Actions that need approval are rejected automatically, so do not request sandbox escalation with `sandbox_permissions`.";
/// The model-facing statement for `ask` (`ASK_SENTENCE`).
pub const ASK_SENTENCE: &str = "Approval prompts: enabled. Actions that need approval request it before running, and fail if no one can answer.";

/// The `approval:policy` runtime-context text for `policy`.
pub fn approval_policy_sentence(policy: ApprovalPolicy) -> &'static str {
    match policy {
        ApprovalPolicy::Ask => ASK_SENTENCE,
        ApprovalPolicy::Never => NEVER_SENTENCE,
    }
}

/// One permission question (`ApprovalRequest` without the agent, which the
/// session-scoped approver already knows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    /// The tool the question is about.
    pub tool_name: String,
    /// The exact tool call being decided.
    pub call_id: String,
    /// Why the asker asks, such as `escalate sandbox to …: <justification>`.
    pub reason: String,
}

/// The answer to one [`ApprovalRequest`].
pub type ApprovalFuture = BoxFuture<'static, EscalationOutcome>;

/// Answers approval requests: a person through a UI, or a fixed policy.
/// The future owns everything it needs; dropping it withdraws the question.
/// `allowed-once` grants only the request asked.
pub trait Approver: Send + Sync {
    /// Decides `request`. Aborting `cancel` withdraws the question; the
    /// answer should then be [`EscalationOutcome::Cancelled`].
    fn request(&self, request: ApprovalRequest, cancel: Option<AbortSignal>) -> ApprovalFuture;
}

/// Rejects every request: the deterministic headless stance.
#[derive(Debug, Clone, Copy, Default)]
pub struct NeverApprover;

impl Approver for NeverApprover {
    fn request(&self, _request: ApprovalRequest, _cancel: Option<AbortSignal>) -> ApprovalFuture {
        Box::pin(std::future::ready(EscalationOutcome::Rejected))
    }
}

/// No one can answer: every request is `unavailable`, which fails closed.
/// The terminal answerer of a headless composition, as the TypeScript
/// waterfall's default was.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoApprover;

impl Approver for NoApprover {
    fn request(&self, _request: ApprovalRequest, _cancel: Option<AbortSignal>) -> ApprovalFuture {
        Box::pin(std::future::ready(EscalationOutcome::Unavailable))
    }
}

/// The approval service in front of an answerer (`ApprovalService.decide`):
/// an already-aborted request is `cancelled` without asking; under the
/// session's `never` policy every request is `rejected` without asking;
/// otherwise the answerer decides, raced against `cancel`, and an answerer
/// that panics fails the question closed as `unavailable`. The policy is
/// read from the session's standing permissions when each request starts.
pub struct PolicyApprover {
    standing: Arc<StandingPermissions>,
    answerer: Arc<dyn Approver>,
}

impl PolicyApprover {
    /// The service for one session, in front of `answerer` ([`NoApprover`]
    /// in a headless run).
    pub fn new(standing: Arc<StandingPermissions>, answerer: Arc<dyn Approver>) -> Self {
        Self { standing, answerer }
    }
}

impl fmt::Debug for PolicyApprover {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PolicyApprover").finish_non_exhaustive()
    }
}

impl Approver for PolicyApprover {
    fn request(&self, request: ApprovalRequest, cancel: Option<AbortSignal>) -> ApprovalFuture {
        let policy = self.standing.approval();
        let answerer = Arc::clone(&self.answerer);
        Box::pin(async move {
            if cancel.as_ref().is_some_and(AbortSignal::aborted) {
                return EscalationOutcome::Cancelled;
            }
            // Decided here, before any answerer: no answerer can bypass it.
            if policy == ApprovalPolicy::Never {
                return EscalationOutcome::Rejected;
            }
            let answer = CatchUnwind(answerer.request(request, cancel.clone()));
            let withdrawn = async {
                match &cancel {
                    Some(signal) => signal.cancelled().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                // A late answer after the abort is dropped with its future.
                () = withdrawn => EscalationOutcome::Cancelled,
                answer = answer => answer.unwrap_or(EscalationOutcome::Unavailable),
            }
        })
    }
}

/// Polls a future, turning a panic into `None`.
struct CatchUnwind<F>(F);

impl<F: Future + Unpin> Future for CatchUnwind<F> {
    type Output = Option<F::Output>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = &mut self.0;
        match std::panic::catch_unwind(AssertUnwindSafe(|| Pin::new(inner).poll(cx))) {
            Ok(Poll::Ready(value)) => Poll::Ready(Some(value)),
            Ok(Poll::Pending) => Poll::Pending,
            Err(_) => Poll::Ready(None),
        }
    }
}
