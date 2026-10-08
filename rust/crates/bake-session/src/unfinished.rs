//! Development-only projection of the unfinished work a restored Session
//! holds: its open turn and step, its pending tool calls, its open
//! compaction, its catalog children, and its pending inbox.
//!
//! No single TypeScript function computes all five, so [`unfinished_work`]
//! composes the production rules each owning package applies to the events
//! the read path hands the Session: the stored events, then
//! [`RestoredLog::closers`]. Work is read before the `session/end-seed` that
//! Session construction appends, which this port reports and never stores.
//! That seed would make every open compaction stale, so resume never finds
//! the compaction lock held; the projection says what was in flight when the
//! writer stopped.
//!
//! - The open turn and step are the ones `interruptedTurnClosers` in
//!   `packages/core/session/src/repair.ts` ends, and each pending tool call
//!   is one of its synthetic `tool/result` closers, in closer order, with its
//!   code and the recorded `tool/call` seq it cites. Calls left in a closed
//!   step or turn are not pending.
//! - The open compaction follows the lock check `assertNoActiveCompaction`
//!   in `packages/compaction/compaction-basic/src/region.ts`: scanning
//!   backward, the latest `compaction/start` or `compaction/end` is a start,
//!   and no stored `session/end-seed` has a greater seq. `compaction/summary`
//!   closes nothing. Its data is returned as logged, uninterpreted.
//! - The children are [`subagent_catalog`]'s view, each entry paired with
//!   the seq of the own `subagent/catalog` event it came from. 0.3 logs no
//!   subagent start or end, so the catalog from the inherited cut onward is
//!   the whole durable record of a parent's children; a child's own
//!   unfinished work lives in that child's log.
//! - The inbox is [`restored_inbox`].
//!
//! Restoration's limits apply before this projection, and the children and
//! inbox keep their folds' refusals and limits. The turn, step, and tool
//! fields are read from closers this port built, and the compaction scan
//! compares only types and seqs, so neither adds a limit.

use serde_json::Value;

use crate::{
    InboxRefusal, PendingInbox, RestoredLog, SubagentCatalogEntry, SubagentCatalogRefusal,
    restored_inbox, subagent_catalog,
};

/// The unfinished work [`unfinished_work`] reads from a restored log.
#[derive(Debug, Clone, PartialEq)]
pub struct UnfinishedWork {
    /// The turn the closers end, or `None` when the last turn ended.
    pub turn: Option<OpenTurn>,
    /// One entry per synthetic `tool/result` closer, in closer order.
    pub tools: Vec<PendingToolCall>,
    /// The unmatched `compaction/start` no later end seed made stale.
    pub compaction: Option<OpenCompaction>,
    /// The own catalog entries in event order, each with its event's seq, or
    /// the first own catalog event the payload schema rejects.
    pub children: Result<Vec<(u64, SubagentCatalogEntry)>, SubagentCatalogRefusal>,
    /// The pending inbox, or its fold's refusal.
    pub inbox: Result<PendingInbox, InboxRefusal>,
}

/// The open turn's number and its open step's, as the closers record them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenTurn {
    pub turn: u64,
    /// `None` when no step was open.
    pub step: Option<u64>,
}

/// One tool call an interrupted turn left without a result.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingToolCall {
    pub call_id: String,
    /// The seq of the synthetic `tool/result` closer that ends the call.
    pub closer_seq: u64,
    /// The requesting Assistant row's `step`, as logged; `None` when absent.
    pub step: Option<Value>,
    /// The seq of the recorded `tool/call`, or `None` when the call was never
    /// recorded as started.
    pub call_seq: Option<u64>,
}

impl PendingToolCall {
    /// The closer's error code: `TOOL_OUTCOME_UNKNOWN` for a recorded call,
    /// `TOOL_NOT_STARTED` otherwise.
    pub const fn code(&self) -> &'static str {
        match self.call_seq {
            Some(_) => "TOOL_OUTCOME_UNKNOWN",
            None => "TOOL_NOT_STARTED",
        }
    }
}

/// A `compaction/start` whose bracket is still open.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenCompaction {
    pub start_seq: u64,
    /// The start's `data`, as logged.
    pub data: Value,
}

/// Project a restored log's unfinished work, before the end seed Session
/// construction would append.
pub fn unfinished_work(restored: &RestoredLog) -> UnfinishedWork {
    let (turn, tools) = turn_and_tools(restored.closers());
    UnfinishedWork {
        turn,
        tools,
        compaction: open_compaction(restored),
        children: children(restored),
        inbox: restored_inbox(restored),
    }
}

/// The type, seq, and data of each stored event, then each closer.
fn events(restored: &RestoredLog) -> impl DoubleEndedIterator<Item = (&str, u64, &Value)> {
    let stored = restored.stored().events().map(|event| {
        let envelope = event.envelope();
        (envelope.event_type, envelope.seq, envelope.data)
    });
    let closers = restored.closers().iter().map(|closer| {
        (
            closer["type"].as_str().expect("closer type"),
            closer["seq"].as_u64().expect("closer seq"),
            &closer["data"],
        )
    });
    stored.collect::<Vec<_>>().into_iter().chain(closers)
}

/// Read the open turn, open step, and pending calls from the closers
/// [`crate::repair`] built, which end with `turn/end` when any exist.
fn turn_and_tools(closers: &[Value]) -> (Option<OpenTurn>, Vec<PendingToolCall>) {
    let Some(end) = closers.last() else {
        return (None, Vec::new());
    };
    let turn = end["data"]["turn"].as_u64().expect("closer turn");
    let step = closers
        .iter()
        .find(|closer| closer["type"] == "step/end")
        .map(|closer| closer["data"]["step"].as_u64().expect("closer step"));
    let tools = closers
        .iter()
        .filter(|closer| closer["type"] == "tool/result")
        .map(|closer| PendingToolCall {
            call_id: closer["data"]["message"]["source"]["callId"]
                .as_str()
                .expect("closer call id")
                .to_owned(),
            closer_seq: closer["seq"].as_u64().expect("closer seq"),
            step: closer["data"].get("step").cloned(),
            call_seq: closer
                .get("sourceEventSeqs")
                .map(|seqs| seqs[0].as_u64().expect("closer source")),
        })
        .collect();
    (Some(OpenTurn { turn, step }), tools)
}

/// `inspectCompactionEntryState` and `assertCompactionInactive`: the latest
/// unmatched start, unless the latest end seed's seq is greater.
fn open_compaction(restored: &RestoredLog) -> Option<OpenCompaction> {
    let mut latest_end_seed = None;
    let mut unmatched = None;
    let mut bracket_known = false;
    for (event_type, seq, data) in events(restored).rev() {
        if latest_end_seed.is_none() && event_type == "session/end-seed" {
            latest_end_seed = Some(seq);
        }
        if !bracket_known {
            match event_type {
                "compaction/start" => {
                    unmatched = Some((seq, data));
                    bracket_known = true;
                }
                "compaction/end" => bracket_known = true,
                _ => {}
            }
        }
        if bracket_known && latest_end_seed.is_some() {
            break;
        }
    }
    let (start_seq, data) = unmatched?;
    if latest_end_seed.is_some_and(|end_seed| end_seed > start_seq) {
        return None;
    }
    Some(OpenCompaction {
        start_seq,
        data: data.clone(),
    })
}

/// [`subagent_catalog`]'s entries zipped with the seqs of the events it
/// admits: every `subagent/catalog` at or after the inherited cut.
fn children(
    restored: &RestoredLog,
) -> Result<Vec<(u64, SubagentCatalogEntry)>, SubagentCatalogRefusal> {
    let entries = subagent_catalog(restored)?;
    let inherited_event_count = restored.stored().inherited_event_count();
    let seqs = events(restored)
        .filter(|(event_type, seq, _)| {
            *event_type == "subagent/catalog" && *seq >= inherited_event_count
        })
        .map(|(_, seq, _)| seq);
    Ok(seqs.zip(entries).collect())
}
