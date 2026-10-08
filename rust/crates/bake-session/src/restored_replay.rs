//! Development-only request derivation over a restored current-format log,
//! seeded, resumed, or migrated, as a spec-local composition of the TypeScript
//! read path and the `replayRequests` cut rule rebuilds each request.
//!
//! The TypeScript test helper `replayRequests` in
//! `packages/core/agent-loop/tests/runtime-fixture.ts` refuses a seeded log
//! and constructs each prefix's Session without message projections, so
//! [`crate::replay_requests`] refuses both too. Restoration admits both: it
//! reads the inherited cut from the last tagged `session/end-seed`, admits the
//! ordinary resume marker, and applies the catalog's `image/offload`
//! projection. The oracle this function reproduces, in
//! `packages/core/agent-loop/tests/restored-request-derivation-conformance.spec.ts`,
//! restores the whole log as the production read path does, then, for each
//! cut, passes the stored events before it to `Session.fromRestore` with the
//! scanned header, the inherited cut, `'detached'`, and the catalog's message
//! projections, and assembles the request from `deriveMessages`,
//! `toolHistory`, and `foldRequestHeader` exactly as `replayRequests` does.
//! The live check of the same rule is the agent loop's invariant companion,
//! `packages/core/agent-loop/src/invariant.ts`.
//!
//! [`replay_restored_requests`] takes a [`RestoredLog`], so every restoration
//! refusal and native limit, including the qualification of projected
//! payloads, has already applied. It then runs these stages:
//!
//! 1. Coordinate qualification of every stored `step/start`, `assistant/attempt`,
//!    and `assistant/message`, as [`crate::replay_requests`] qualifies them.
//! 2. The cut rule of `replayRequests`: each `step/start` in log order yields
//!    one request per Assistant settlement with its coordinate, in log order,
//!    and a step whose first settlement is missing or earlier refuses the log.
//!    Closers hold no settlement, so every cut is a stored event.
//! 3. A cut below the inherited event count yields no request and is not
//!    checked for a header. `Session.fromRestore` refuses an inherited count
//!    beyond its seed, so the oracle cannot restore such an ancestor cut;
//!    those prefixes are outside this function's domain. Every other cut is
//!    the child's own dispatch, carrying the restored Session's id.
//! 4. Each own cut's request is the fold of the stored events before it, with
//!    the `image/offload` projection installed. Restoration takes no lossless
//!    snapshot, so neither does this fold: a -0 in a payload no request
//!    carries does not refuse the log.
//!
//! The same admission and fold already accepted every stored event during
//! restoration, so folding a prefix again cannot fail. Migrated logs need no
//! separate path: [`crate::restore_migrated`] returns a [`RestoredLog`] of the
//! migration's encoded output.

use std::collections::{BTreeMap, BTreeSet};

use crate::replay::{Coordinate, admit, coordinate};
use crate::request::{Request, RequestFold};
use crate::{RestoredLog, V3CodecEvent};

/// Why [`replay_restored_requests`] returned no requests.
///
/// The first two variants claim the oracle helper's exact message, which
/// [`RestoredReplayRefusal::message`] renders; the native subset claims
/// nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoredReplayRefusal {
    /// The step has no Assistant settlement after its `step/start`. The
    /// helper throws "step `turn`.`step` has no later Assistant settlement".
    NoLaterSettlement { turn: u64, step: u64 },
    /// No `request/header` precedes one of the step's own settlements. The
    /// helper throws "step `turn`.`step` has no request header".
    NoRequestHeader { turn: u64, step: u64 },
    /// This port cannot reproduce the outcome; nothing is claimed. `seq`
    /// names the row.
    NativeSubset {
        seq: u64,
        limit: RestoredReplayLimit,
    },
}

impl RestoredReplayRefusal {
    /// The oracle helper's exact message, or `None` for a native limit.
    pub fn message(&self) -> Option<String> {
        match self {
            Self::NoLaterSettlement { turn, step } => Some(format!(
                "step {turn}.{step} has no later Assistant settlement"
            )),
            Self::NoRequestHeader { turn, step } => {
                Some(format!("step {turn}.{step} has no request header"))
            }
            Self::NativeSubset { .. } => None,
        }
    }
}

/// Input this port does not derive, whatever TypeScript does with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoredReplayLimit {
    /// A `step/start`, `assistant/attempt`, or `assistant/message` coordinate
    /// does not hold `turn` and `step` as non-negative safe integers written as
    /// integers, which the helper compares with `===`.
    Coordinate,
    /// Two `step/start` rows share a coordinate.
    RepeatedCoordinate,
}

/// Rebuild the request each of a restored Session's own dispatches sent.
///
/// Requests come in the order [`crate::replay_requests`] returns them, for
/// the cuts at or after [`crate::ScannedLog::inherited_event_count`]. An
/// unseeded log has a zero cut, so every settlement yields a request; a seeded
/// child whose own events hold no settlement yields none.
pub fn replay_restored_requests(
    restored: &RestoredLog,
) -> Result<Vec<Request>, RestoredReplayRefusal> {
    let stored = restored.stored();
    let inherited = stored.inherited_event_count();
    let events: Vec<V3CodecEvent<'_>> = stored.events().collect();
    let mut starts: Vec<(Coordinate, u64)> = Vec::new();
    let mut started = BTreeSet::new();
    let mut settlements: BTreeMap<Coordinate, Vec<u64>> = BTreeMap::new();
    for event in &events {
        let envelope = event.envelope();
        let seq = envelope.seq;
        if !matches!(
            envelope.event_type,
            "step/start" | "assistant/message" | "assistant/attempt"
        ) {
            continue;
        }
        let at = coordinate(envelope.data).ok_or(limit(seq, RestoredReplayLimit::Coordinate))?;
        if envelope.event_type == "step/start" {
            starts.push((at, seq));
            if !started.insert(at) {
                return Err(limit(seq, RestoredReplayLimit::RepeatedCoordinate));
            }
        } else {
            settlements.entry(at).or_default().push(seq);
        }
    }
    // The helper checks each step in log order, then each of its own cuts'
    // headers, and stops at the first step whose first settlement is missing
    // or earlier, so later steps contribute no cut.
    let steps: Vec<(Coordinate, Option<Vec<u64>>)> = starts
        .iter()
        .map(|(at, start)| {
            let own = settlements
                .get(at)
                .filter(|cuts| cuts.first().is_some_and(|first| first > start))
                .map(|cuts| {
                    cuts.iter()
                        .copied()
                        .filter(|cut| *cut >= inherited)
                        .collect()
                });
            (*at, own)
        })
        .collect();
    let cut_set: BTreeSet<u64> = steps
        .iter()
        .map_while(|(_, own)| own.as_ref())
        .flatten()
        .copied()
        .collect();
    let end = cut_set.last().copied().unwrap_or(0);
    let mut fold = RequestFold::with_image_offload(stored.header().id.clone());
    let mut snapshots: BTreeMap<u64, Option<Request>> = BTreeMap::new();
    for event in events.iter().take_while(|event| event.envelope().seq < end) {
        let envelope = event.envelope();
        if cut_set.contains(&envelope.seq) {
            snapshots.insert(envelope.seq, fold.request());
        }
        let fact = admit(envelope).expect("restoration admitted every stored event");
        fold.append(fact)
            .expect("restoration folded every stored event in order");
    }
    if cut_set.contains(&end) {
        snapshots.insert(end, fold.request());
    }
    let mut requests = Vec::new();
    for ((turn, step), own) in steps {
        let Some(own) = own else {
            return Err(RestoredReplayRefusal::NoLaterSettlement { turn, step });
        };
        for cut in own {
            let request = snapshots.get(&cut).cloned().flatten();
            requests.push(request.ok_or(RestoredReplayRefusal::NoRequestHeader { turn, step })?);
        }
    }
    Ok(requests)
}

const fn limit(seq: u64, limit: RestoredReplayLimit) -> RestoredReplayRefusal {
    RestoredReplayRefusal::NativeSubset { seq, limit }
}
