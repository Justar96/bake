//! Bake's opt-in bound for trusted read batches, on the real loop.

mod common;

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use bake_agent::{
    AgentContext, AgentEvent, AgentLoopConfig, AgentTool, AgentToolResult, BeforeToolCallResult,
    ToolEffect, ToolExecutionMode, agent_loop, hook,
};
use bake_ai::AbortController;
use common::*;
use serde_json::json;
use tokio::sync::{Notify, mpsc};

const PATIENCE: Duration = Duration::from_secs(10);

struct Batch {
    tools: Vec<AgentTool>,
    release: Arc<Vec<Notify>>,
    entered: mpsc::UnboundedReceiver<usize>,
    peak: Arc<AtomicUsize>,
}

impl Batch {
    fn new() -> Self {
        let release = Arc::new((0..4).map(|_| Notify::new()).collect::<Vec<_>>());
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (entered, receiver) = mpsc::unbounded_channel();
        let tools = (0..4)
            .map(|index| {
                let (release, active, peak, entered) = (
                    Arc::clone(&release),
                    Arc::clone(&active),
                    Arc::clone(&peak),
                    entered.clone(),
                );
                let mut tool = AgentTool::new(
                    format!("read_{index}"),
                    "Read",
                    "Read fixture",
                    empty_schema(),
                    move |_, _, _, _, _ctx| {
                        let (release, active, peak, entered) = (
                            Arc::clone(&release),
                            Arc::clone(&active),
                            Arc::clone(&peak),
                            entered.clone(),
                        );
                        Box::pin(async move {
                            let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                            peak.fetch_max(count, Ordering::SeqCst);
                            entered.send(index).unwrap();
                            release[index].notified().await;
                            active.fetch_sub(1, Ordering::SeqCst);
                            Ok(AgentToolResult::text(format!("fact {index}")))
                        })
                    },
                );
                tool.effect = Some(ToolEffect::Read);
                tool
            })
            .collect();
        Self {
            tools,
            release,
            entered: receiver,
            peak,
        }
    }

    fn context(&self) -> AgentContext {
        AgentContext {
            messages: Vec::new(),
            tools: self.tools.iter().cloned().map(Arc::new).collect(),
        }
    }

    async fn next(&mut self) -> usize {
        tokio::time::timeout(PATIENCE, self.entered.recv())
            .await
            .unwrap()
            .unwrap()
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        for release in self.release.iter() {
            release.notify_one();
        }
    }
}

fn config(cap: Option<usize>) -> AgentLoopConfig {
    let mut config = AgentLoopConfig::new(model(), identity_converter());
    config.max_concurrent_read_tools = cap.and_then(NonZeroUsize::new);
    config
}

fn response() -> bake_agent::StreamFn {
    scripted(|index, _, _| {
        if index == 0 {
            tool_use(
                (0..4)
                    .map(|index| {
                        call(
                            &format!("call_{index}"),
                            &format!("read_{index}"),
                            json!({}),
                        )
                    })
                    .collect(),
            )
        } else {
            assistant_text("done")
        }
    })
}

#[tokio::test]
async fn bounded_reads_fill_free_slots_and_keep_result_order() {
    let mut batch = Batch::new();
    let prepared = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&prepared);
    let mut config = config(Some(2));
    config.before_tool_call = Some(hook::before_tool_call(move |call, _| {
        seen.lock().unwrap().push(call.tool_call.id.clone());
        Box::pin(async { Ok(None) })
    }));
    let stream = agent_loop(
        vec![user("read all")],
        batch.context(),
        config,
        None,
        Some(response()),
    );
    assert_eq!([batch.next().await, batch.next().await], [0, 1]);
    let excess = batch.entered.try_recv().ok();
    let preparation = prepared.lock().unwrap().clone();
    batch.release[1].notify_one();
    let next = batch.next().await;
    batch.release[2].notify_one();
    let last = batch.next().await;
    batch.release[3].notify_one();
    // Observe the third completion before releasing the first call.
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(PATIENCE, stream.next())
            .await
            .unwrap()
            .unwrap();
        let done = matches!(&event, AgentEvent::ToolExecutionEnd { tool_call_id, .. } if tool_call_id == "call_3");
        events.push(event);
        if done {
            break;
        }
    }
    batch.release[0].notify_one();
    events.extend(
        tokio::time::timeout(PATIENCE, drain(&stream))
            .await
            .unwrap(),
    );
    assert_eq!(excess, None, "more than two read calls started");
    assert_eq!([next, last], [2, 3]);
    assert_eq!(batch.peak.load(Ordering::SeqCst), 2);
    assert_eq!(preparation, ["call_0", "call_1", "call_2", "call_3"]);
    assert_eq!(
        tool_end_ids(&events),
        ["call_1", "call_2", "call_3", "call_0"]
    );
    assert_eq!(
        tool_result_ids(&events),
        ["call_0", "call_1", "call_2", "call_3"]
    );
}

#[tokio::test]
async fn a_sequential_tool_is_a_whole_batch_barrier() {
    let mut batch = Batch::new();
    batch.tools[1].execution_mode = Some(ToolExecutionMode::Sequential);
    let stream = agent_loop(
        vec![user("read edit read")],
        batch.context(),
        config(Some(4)),
        None,
        Some(response()),
    );
    assert_eq!(batch.next().await, 0);
    let excess = batch.entered.try_recv().ok();
    for release in batch.release.iter() {
        release.notify_one();
    }
    let events = tokio::time::timeout(PATIENCE, drain(&stream))
        .await
        .unwrap();
    assert_eq!(excess, None);
    assert_eq!(batch.peak.load(Ordering::SeqCst), 1);
    assert_eq!(
        tool_end_ids(&events),
        ["call_0", "call_1", "call_2", "call_3"]
    );
}

/// With the cap, a tool that declares no effect runs alone, in order, while
/// the reads after it still run together.
#[tokio::test]
async fn an_undeclared_tool_is_a_barrier_for_itself_only() {
    let mut batch = Batch::new();
    batch.tools[1].effect = None;
    batch.tools[1].execution_mode = Some(ToolExecutionMode::Parallel);
    let stream = agent_loop(
        vec![user("read edit read")],
        batch.context(),
        config(Some(4)),
        None,
        Some(response()),
    );
    assert_eq!(batch.next().await, 0);
    let alone = batch.entered.try_recv().ok();
    batch.release[0].notify_one();
    assert_eq!(batch.next().await, 1);
    let before_reads = batch.entered.try_recv().ok();
    batch.release[1].notify_one();
    let reads = [batch.next().await, batch.next().await];
    batch.release[2].notify_one();
    batch.release[3].notify_one();
    let events = tokio::time::timeout(PATIENCE, drain(&stream))
        .await
        .unwrap();
    assert_eq!(alone, None, "a call ran beside the read before the barrier");
    assert_eq!(before_reads, None, "a read ran beside the undeclared tool");
    assert_eq!(reads, [2, 3]);
    assert_eq!(batch.peak.load(Ordering::SeqCst), 2);
    assert_eq!(
        tool_result_ids(&events),
        ["call_0", "call_1", "call_2", "call_3"]
    );
}

#[tokio::test]
async fn absent_cap_preserves_unlimited_unknown_parallel_tools() {
    let mut batch = Batch::new();
    for tool in &mut batch.tools {
        tool.effect = None;
    }
    let stream = agent_loop(
        vec![user("read all")],
        batch.context(),
        config(None),
        None,
        Some(response()),
    );
    let started = [
        batch.next().await,
        batch.next().await,
        batch.next().await,
        batch.next().await,
    ];
    for release in batch.release.iter() {
        release.notify_one();
    }
    tokio::time::timeout(PATIENCE, drain(&stream))
        .await
        .unwrap();
    assert_eq!(started, [0, 1, 2, 3]);
    assert_eq!(batch.peak.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn cancellation_drains_active_reads_and_skips_queued_execution() {
    let mut batch = Batch::new();
    let controller = AbortController::new();
    let stream = agent_loop(
        vec![user("read all")],
        batch.context(),
        config(Some(2)),
        Some(controller.signal()),
        Some(response()),
    );
    assert_eq!([batch.next().await, batch.next().await], [0, 1]);
    controller.abort();
    let mut result = Box::pin(stream.result());
    let pending =
        std::future::poll_fn(|cx| Poll::Ready(result.as_mut().poll(cx).is_pending())).await;
    batch.release[0].notify_one();
    batch.release[1].notify_one();
    tokio::time::timeout(PATIENCE, result).await.unwrap();
    let events = drain(&stream).await;
    assert!(pending, "cancellation returned before active read cleanup");
    assert!(
        batch.entered.try_recv().is_err(),
        "queued read executed after cancellation"
    );
    assert_eq!(
        tool_result_ids(&events),
        ["call_0", "call_1", "call_2", "call_3"]
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolExecutionEnd { is_error: true, .. }))
            .count(),
        2
    );
}

#[tokio::test]
async fn vetoed_read_never_occupies_an_execution_slot() {
    let mut batch = Batch::new();
    let mut config = config(Some(1));
    config.before_tool_call = Some(hook::before_tool_call(|call, _| {
        let block = call.tool_call.id == "call_0";
        Box::pin(async move {
            Ok(block.then(|| BeforeToolCallResult {
                block: true,
                reason: Some("denied".into()),
                terminate: false,
            }))
        })
    }));
    let stream = agent_loop(
        vec![user("read all")],
        batch.context(),
        config,
        None,
        Some(response()),
    );
    let first = batch.next().await;
    for release in batch.release.iter() {
        release.notify_one();
    }
    let events = tokio::time::timeout(PATIENCE, drain(&stream))
        .await
        .unwrap();
    assert_eq!(first, 1);
    assert_eq!(batch.peak.load(Ordering::SeqCst), 1);
    assert_eq!(
        tool_result_ids(&events),
        ["call_0", "call_1", "call_2", "call_3"]
    );
}
