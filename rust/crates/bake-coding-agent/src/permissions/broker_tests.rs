//! The interactive broker's lifecycle. Each test names the TypeScript case
//! it follows: `apps/tui/packages/app/tests/interactions.spec.ts` for the
//! queue, and `packages/interaction/user-approval/tests/approval.spec.ts`
//! for the service in front of it. Waits are on published snapshots and
//! settled futures, never on time.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use bake_ai::AbortController;
use bake_sandbox::EscalationOutcome;

use super::*;
use crate::permissions::{ApprovalPolicy, PolicyApprover, StandingPermissions};

/// A broker whose listener records the generations of every snapshot.
fn connected() -> (ApprovalBroker, Arc<Mutex<Vec<Vec<u64>>>>) {
    let broker = ApprovalBroker::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    assert!(broker.connect(Box::new(move |queue| {
        record
            .lock()
            .unwrap()
            .push(queue.iter().map(|prompt| prompt.generation).collect());
        true
    })));
    (broker, seen)
}

fn ask(tool: &str, call: &str) -> ApprovalRequest {
    ApprovalRequest {
        tool_name: tool.into(),
        call_id: call.into(),
        parent_tool_call_id: None,
        reason: format!("escalate sandbox to workspace-write: {call}"),
        arguments: serde_json::json!({ "command": format!("touch {call}") }),
    }
}

fn generations(broker: &ApprovalBroker) -> Vec<u64> {
    broker.pending().iter().map(|p| p.generation).collect()
}

// interactions.spec: "queues approvals and questions, rejects stale
// answers, and preserves question detail" (the approval half).
#[tokio::test]
async fn requests_queue_in_order_and_a_stale_answer_settles_nothing() {
    let (broker, seen) = connected();
    let first = tokio::spawn(broker.request(ask("bash", "call-1"), None));
    let second = tokio::spawn(broker.request(ask("write", "call-2"), None));
    let queue = broker.pending();
    assert_eq!(queue.len(), 2);
    let (one, two) = (queue[0].clone(), queue[1].clone());
    assert_eq!(
        (one.tool_name.as_str(), one.call_id.as_str()),
        ("bash", "call-1")
    );
    assert_eq!(
        one.reason, "escalate sandbox to workspace-write: call-1",
        "the reason is the request's own"
    );
    assert_eq!(
        one.arguments,
        serde_json::json!({ "command": "touch call-1" })
    );
    // A later request cannot replace the displayed one, and only the
    // displayed one takes an answer.
    assert!(!broker.answer(two.generation, ApprovalAnswer::AllowOnce));
    assert_eq!(generations(&broker), [one.generation, two.generation]);
    assert!(broker.answer(one.generation, ApprovalAnswer::AllowOnce));
    assert_eq!(first.await.unwrap(), EscalationOutcome::AllowedOnce);
    // The answer to the first, sent again, cannot reach the second.
    assert!(!broker.answer(one.generation, ApprovalAnswer::Reject));
    assert!(!broker.answer(one.generation, ApprovalAnswer::AllowOnce));
    assert_eq!(generations(&broker), [two.generation]);
    assert!(broker.answer(two.generation, ApprovalAnswer::Reject));
    assert_eq!(second.await.unwrap(), EscalationOutcome::Rejected);
    assert_eq!(
        *seen.lock().unwrap(),
        [
            vec![],
            vec![one.generation],
            vec![one.generation, two.generation],
            vec![two.generation],
            vec![],
        ]
    );
}

#[tokio::test]
async fn two_requests_for_one_call_id_are_answered_apart() {
    let (broker, _) = connected();
    let first = tokio::spawn(broker.request(ask("bash", "dup"), None));
    let second = tokio::spawn(broker.request(ask("bash", "dup"), None));
    let queue = broker.pending();
    assert_eq!(queue[0].call_id, queue[1].call_id);
    assert_ne!(queue[0].generation, queue[1].generation);
    assert!(broker.answer(queue[0].generation, ApprovalAnswer::AllowOnce));
    assert_eq!(first.await.unwrap(), EscalationOutcome::AllowedOnce);
    assert_eq!(generations(&broker), [queue[1].generation]);
    assert!(broker.answer(queue[1].generation, ApprovalAnswer::Reject));
    assert_eq!(second.await.unwrap(), EscalationOutcome::Rejected);
}

// interactions.spec: "withdraws queued and visible requests on abort and
// settles every request on disposal".
#[tokio::test]
async fn an_abort_withdraws_its_request_and_closing_cancels_the_rest() {
    let (broker, _) = connected();
    let aborted = AbortController::new();
    aborted.abort();
    assert_eq!(
        broker
            .request(ask("bash", "a"), Some(aborted.signal()))
            .await,
        EscalationOutcome::Cancelled
    );
    assert!(
        broker.pending().is_empty(),
        "an aborted request is never shown"
    );

    let visible = tokio::spawn(broker.request(ask("bash", "visible"), None));
    let abort = AbortController::new();
    let queued = tokio::spawn(broker.request(ask("bash", "queued"), Some(abort.signal())));
    while broker.pending().len() < 2 {
        tokio::task::yield_now().await;
    }
    let queued_generation = broker.pending()[1].generation;
    abort.abort();
    assert_eq!(queued.await.unwrap(), EscalationOutcome::Cancelled);
    assert!(
        !broker
            .pending()
            .iter()
            .any(|p| p.generation == queued_generation)
    );
    // The late answer for the withdrawn request settles nothing.
    assert!(!broker.answer(queued_generation, ApprovalAnswer::AllowOnce));
    assert_eq!(broker.pending()[0].call_id, "visible");

    broker.close();
    assert_eq!(visible.await.unwrap(), EscalationOutcome::Cancelled);
    assert!(broker.pending().is_empty());
    // Disposed: no one can answer again.
    assert_eq!(
        broker.request(ask("bash", "later"), None).await,
        EscalationOutcome::Unavailable
    );
    assert!(
        !broker.connect(Box::new(|_| true)),
        "a closed broker stays closed"
    );
    broker.close();
}

// interactions.spec: "delegates requests for other agents and cancels only
// the displayed request" (the cancel half).
#[tokio::test]
async fn cancel_settles_only_the_displayed_request() {
    let (broker, _) = connected();
    let first = tokio::spawn(broker.request(ask("bash", "1"), None));
    let second = tokio::spawn(broker.request(ask("bash", "2"), None));
    let front = broker.pending()[0].generation;
    assert!(broker.answer(front, ApprovalAnswer::Cancel));
    assert_eq!(first.await.unwrap(), EscalationOutcome::Cancelled);
    assert_eq!(broker.pending()[0].call_id, "2");
    broker.close();
    assert_eq!(second.await.unwrap(), EscalationOutcome::Cancelled);
}

// approval.spec: "fails closed to unavailable when nobody listens".
#[tokio::test]
async fn without_a_frontend_a_request_is_unavailable() {
    let broker = ApprovalBroker::new();
    assert_eq!(
        broker.request(ask("bash", "x"), None).await,
        EscalationOutcome::Unavailable
    );
    assert!(broker.pending().is_empty());
}

#[tokio::test]
async fn a_dropped_future_withdraws_its_prompt() {
    let (broker, seen) = connected();
    let future = broker.request(ask("bash", "dropped"), None);
    let generation = broker.pending()[0].generation;
    drop(future);
    assert!(broker.pending().is_empty());
    assert_eq!(seen.lock().unwrap().last(), Some(&vec![]));
    assert!(!broker.answer(generation, ApprovalAnswer::AllowOnce));
}

#[tokio::test]
async fn a_gone_or_panicking_frontend_fails_every_request_closed() {
    // The listener reports the frontend gone on the second snapshot.
    let broker = ApprovalBroker::new();
    let calls = Arc::new(Mutex::new(0));
    let counted = Arc::clone(&calls);
    assert!(broker.connect(Box::new(move |_| {
        let mut calls = counted.lock().unwrap();
        *calls += 1;
        *calls < 2
    })));
    assert_eq!(
        broker.request(ask("bash", "x"), None).await,
        EscalationOutcome::Cancelled
    );
    assert!(broker.is_closed());
    assert_eq!(
        broker.request(ask("bash", "y"), None).await,
        EscalationOutcome::Unavailable
    );

    let broker = ApprovalBroker::new();
    let armed = Arc::new(AtomicBool::new(false));
    let trip = Arc::clone(&armed);
    assert!(broker.connect(Box::new(move |_| {
        assert!(!trip.load(Ordering::SeqCst), "frontend bug");
        true
    })));
    armed.store(true, Ordering::SeqCst);
    assert_eq!(
        broker.request(ask("bash", "z"), None).await,
        EscalationOutcome::Cancelled
    );
    assert!(broker.is_closed());
}

// approval.spec: "a never config rejects deterministically without
// consulting any answerer".
#[tokio::test]
async fn the_never_policy_rejects_before_anything_is_shown() {
    let (broker, seen) = connected();
    let standing = Arc::new(StandingPermissions::new(
        bake_sandbox::SandboxMode::WorkspaceWrite,
        ApprovalPolicy::Never,
        "x",
        "/session-project",
        None,
    ));
    let service = PolicyApprover::new(Arc::clone(&standing), Arc::new(broker.clone()));
    assert_eq!(
        service.request(ask("bash", "n"), None).await,
        EscalationOutcome::Rejected
    );
    assert_eq!(
        *seen.lock().unwrap(),
        [Vec::<u64>::new()],
        "no prompt was shown"
    );

    // Under `ask`, the service's own abort withdraws the shown prompt.
    standing.set(
        bake_sandbox::SandboxMode::WorkspaceWrite,
        ApprovalPolicy::Ask,
        "x",
    );
    let abort = AbortController::new();
    let asked = tokio::spawn(service.request(ask("bash", "a"), Some(abort.signal())));
    while broker.pending().is_empty() {
        tokio::task::yield_now().await;
    }
    abort.abort();
    assert_eq!(asked.await.unwrap(), EscalationOutcome::Cancelled);
    assert!(broker.pending().is_empty());
}

#[tokio::test]
async fn only_one_frontend_connects() {
    let (broker, seen) = connected();
    let other = Arc::new(AtomicBool::new(false));
    let called = Arc::clone(&other);
    assert!(!broker.connect(Box::new(move |_| {
        called.store(true, Ordering::SeqCst);
        true
    })));
    let asked = tokio::spawn(broker.request(ask("bash", "x"), None));
    assert!(
        !other.load(Ordering::SeqCst),
        "the second frontend saw a request"
    );
    assert_eq!(seen.lock().unwrap().last().map(Vec::len), Some(1));
    broker.close();
    assert_eq!(asked.await.unwrap(), EscalationOutcome::Cancelled);
}

#[tokio::test]
async fn a_listener_may_call_back_into_the_broker_and_sees_every_state_in_order() {
    let broker = ApprovalBroker::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (record, inner) = (Arc::clone(&seen), broker.clone());
    assert!(broker.connect(Box::new(move |queue| {
        // Reentrant: reads the queue and answers its front from inside the
        // announcement, as a frontend on the same thread might.
        let generations: Vec<u64> = queue.iter().map(|p| p.generation).collect();
        let _ = inner.pending();
        if let Some(front) = queue.first() {
            inner.answer(front.generation, ApprovalAnswer::Reject);
        }
        record.lock().unwrap().push(generations);
        true
    })));
    assert_eq!(
        broker.request(ask("bash", "a"), None).await,
        EscalationOutcome::Rejected
    );
    // The queue's states arrive in order and end with the latest.
    assert_eq!(*seen.lock().unwrap(), [vec![], vec![1], vec![]]);

    // A listener that closes the broker from inside settles the request.
    let broker = ApprovalBroker::new();
    let inner = broker.clone();
    assert!(broker.connect(Box::new(move |queue| {
        if !queue.is_empty() {
            inner.close();
        }
        true
    })));
    assert_eq!(
        broker.request(ask("bash", "b"), None).await,
        EscalationOutcome::Cancelled
    );
}

#[tokio::test]
async fn cancellation_outranks_an_answer_that_arrives_with_it() {
    let (broker, _) = connected();
    let abort = AbortController::new();
    let asked = broker.request(ask("bash", "race"), Some(abort.signal()));
    let generation = broker.pending()[0].generation;
    // Both happen before the request's future looks at either.
    abort.abort();
    assert!(
        !broker.answer(generation, ApprovalAnswer::AllowOnce),
        "an aborted request took the grant"
    );
    assert_eq!(asked.await, EscalationOutcome::Cancelled);
    assert!(broker.pending().is_empty());
}

/// Aborts the request's own signal, then answers `allowed-once` at once.
struct AbortsThenAllows(AbortController);

impl Approver for AbortsThenAllows {
    fn request(&self, _request: ApprovalRequest, _cancel: Option<AbortSignal>) -> ApprovalFuture {
        self.0.abort();
        Box::pin(std::future::ready(EscalationOutcome::AllowedOnce))
    }
}

#[tokio::test]
async fn the_policy_service_overrules_an_answer_given_after_its_abort() {
    let standing = Arc::new(StandingPermissions::new(
        bake_sandbox::SandboxMode::WorkspaceWrite,
        ApprovalPolicy::Ask,
        "x",
        "/session-project",
        None,
    ));
    let abort = AbortController::new();
    let signal = abort.signal();
    let service = PolicyApprover::new(standing, Arc::new(AbortsThenAllows(abort)));
    assert_eq!(
        service.request(ask("bash", "a"), Some(signal)).await,
        EscalationOutcome::Cancelled
    );
}

#[tokio::test]
async fn generations_never_wrap() {
    let (broker, _) = connected();
    lock(&broker.inner).last = u64::MAX - 1;
    let last = tokio::spawn(broker.request(ask("bash", "last"), None));
    assert_eq!(broker.pending()[0].generation, u64::MAX);
    assert_eq!(
        broker.request(ask("bash", "none"), None).await,
        EscalationOutcome::Unavailable
    );
    assert!(broker.answer(u64::MAX, ApprovalAnswer::Reject));
    assert_eq!(last.await.unwrap(), EscalationOutcome::Rejected);
}
