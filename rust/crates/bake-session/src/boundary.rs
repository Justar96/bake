//! Development-only folds of a restored Session's turn and step boundaries
//! and its latest title, as `turnBoundaryProjectionDefinition` in
//! `packages/core/agent-loop/src/index.ts` and `titleProjectionDefinition` in
//! `packages/session/session-title/src/index.ts` fold the same events.
//!
//! [`turn_boundary`] reads only event types and seqs, except that a
//! `turn/start` copies its `data.turn` into `lastTurn`. A `turn/end` clears
//! the open turn, a `step/start` records its seq as the last step start and
//! the last step boundary, and a `step/end` records only the last step
//! boundary. [`session_title`] copies each `session/title`'s `data.title`;
//! its view is its state.
//!
//! Both folds run over the stored events, the inherited prefix and its end
//! seed included, and then the closers, whose `step/end` and `turn/end` move
//! the boundary. The end seed Session construction may append changes
//! neither fold. Restoration refused `null` `turn/start` and `step/start`
//! data and an open tail turn or step that is not a safe count; it checked no
//! other part of either payload. Neither TypeScript fold validates what it
//! copies, so the copies stay JSON values. Where
//! JavaScript would produce `undefined`, throw a `TypeError`, or hold a
//! number this port cannot prove it reads alike, the folds refuse with a
//! [`BoundaryLimit`] and claim no TypeScript outcome.

use serde_json::Value;

use crate::json_parse::{DebugJson, clone_value, dismantle, values_equal};
use crate::{MAX_SAFE_INTEGER, RestoredLog};

/// Which boundary a step most recently crossed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepBoundaryKind {
    Start,
    End,
}

impl StepBoundaryKind {
    /// The kind as the projection spells it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::End => "end",
        }
    }
}

/// `lastStepBoundary`: the latest `step/start` or `step/end` and its seq.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepBoundary {
    pub kind: StepBoundaryKind,
    pub seq: u64,
}

/// `TurnBoundaryProjection` after the closers.
///
/// `last_turn` may nest as deep as its row; this struct's `Drop`, `Clone`,
/// `PartialEq`, and `Debug` do not recurse over it, so its fields cannot be
/// moved out; take `last_turn` with [`std::mem::take`].
pub struct TurnBoundaryState {
    /// The seq of a `turn/start` no `turn/end` has followed. A closer ends
    /// any turn interruptedTurnClosers sees as open, so only a turn whose
    /// `turn` is `null` stays open.
    pub open_turn_start_seq: Option<u64>,
    /// The latest `step/start`'s seq; a `step/end` keeps it.
    pub last_step_start_seq: Option<u64>,
    pub last_step_boundary: Option<StepBoundary>,
    /// `0` until a `turn/start`, then that event's `data.turn` as logged. It
    /// may nest as deep as its row; drop it with [`crate::dismantle`].
    pub last_turn: Value,
}

impl Drop for TurnBoundaryState {
    fn drop(&mut self) {
        dismantle(std::mem::take(&mut self.last_turn));
    }
}

impl Clone for TurnBoundaryState {
    fn clone(&self) -> Self {
        Self {
            open_turn_start_seq: self.open_turn_start_seq,
            last_step_start_seq: self.last_step_start_seq,
            last_step_boundary: self.last_step_boundary,
            last_turn: clone_value(&self.last_turn),
        }
    }
}

impl PartialEq for TurnBoundaryState {
    fn eq(&self, other: &Self) -> bool {
        self.open_turn_start_seq == other.open_turn_start_seq
            && self.last_step_start_seq == other.last_step_start_seq
            && self.last_step_boundary == other.last_step_boundary
            && values_equal(&self.last_turn, &other.last_turn)
    }
}

impl Eq for TurnBoundaryState {}

impl std::fmt::Debug for TurnBoundaryState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnBoundaryState")
            .field("open_turn_start_seq", &self.open_turn_start_seq)
            .field("last_step_start_seq", &self.last_step_start_seq)
            .field("last_step_boundary", &self.last_step_boundary)
            .field("last_turn", &DebugJson(&self.last_turn))
            .finish()
    }
}

/// Event `seq` needs JavaScript behavior this port does not reproduce;
/// nothing is claimed about TypeScript's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundaryRefusal {
    pub seq: u64,
    pub limit: BoundaryLimit,
}

/// Input whose fold depends on JavaScript property reads or number reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryLimit {
    /// The final `lastTurn` or title comes from data that is not an object
    /// holding the member, which JavaScript reads as `undefined` and JSON
    /// cannot express.
    UndefinedMember,
    /// A `session/title`'s data is `null`, whose member read throws a
    /// `TypeError` there. Restoration already refuses `null` `turn/start`
    /// data.
    NullData,
    /// The final `lastTurn` or title holds a number spelled with a fraction
    /// or an exponent, written as -0, or beyond the safe-integer range,
    /// which `JSON.parse` may read as a different value than its spelling.
    Number,
}

/// A copied member, the seq of its event, and `None` for `undefined`.
type Copied<'a> = (u64, Option<&'a Value>);

/// Fold `turnBoundaryProjectionDefinition` from `init()` over the restored
/// stored events and then the closers.
pub fn turn_boundary(restored: &RestoredLog) -> Result<TurnBoundaryState, BoundaryRefusal> {
    let mut open_turn_start_seq = None;
    let mut last_step_start_seq = None;
    let mut last_step_boundary = None;
    let mut last_turn: Option<Copied<'_>> = None;
    for (seq, event_type, data) in events(restored) {
        match event_type {
            "turn/start" => {
                open_turn_start_seq = Some(seq);
                last_turn = Some(member(seq, data, "turn")?);
            }
            "turn/end" => open_turn_start_seq = None,
            "step/start" => {
                last_step_start_seq = Some(seq);
                last_step_boundary = Some(StepBoundary {
                    kind: StepBoundaryKind::Start,
                    seq,
                });
            }
            "step/end" => {
                last_step_boundary = Some(StepBoundary {
                    kind: StepBoundaryKind::End,
                    seq,
                });
            }
            _ => {}
        }
    }
    let last_turn = match last_turn {
        None => Value::from(0),
        Some(copied) => settle(copied)?,
    };
    Ok(TurnBoundaryState {
        open_turn_start_seq,
        last_step_start_seq,
        last_step_boundary,
        last_turn,
    })
}

/// Fold `titleProjectionDefinition` from `init()` over the restored stored
/// events and then the closers: `null` until a `session/title`, then that
/// event's `data.title` as logged. It may nest as deep as its row; drop it
/// with [`crate::dismantle`], and note that its derived `Clone`,
/// `PartialEq`, and `Debug` recurse over that nesting.
pub fn session_title(restored: &RestoredLog) -> Result<Value, BoundaryRefusal> {
    let mut title: Option<Copied<'_>> = None;
    for (seq, event_type, data) in events(restored) {
        if event_type == "session/title" {
            title = Some(member(seq, data, "title")?);
        }
    }
    title.map_or(Ok(Value::Null), settle)
}

/// The stored events and then the closers, as seq, type, and data.
fn events(restored: &RestoredLog) -> impl Iterator<Item = (u64, &str, &Value)> {
    let stored = restored.stored().events().map(|event| {
        let envelope = event.envelope();
        (envelope.seq, envelope.event_type, envelope.data)
    });
    let closers = restored.closers().iter().map(|closer| {
        (
            closer["seq"].as_u64().expect("closer seq"),
            closer["type"].as_str().expect("closer type"),
            &closer["data"],
        )
    });
    stored.chain(closers)
}

/// `data[key]` as JavaScript reads it: `null` data throws, and any other
/// data without the member gives `undefined`.
fn member<'a>(seq: u64, data: &'a Value, key: &str) -> Result<Copied<'a>, BoundaryRefusal> {
    if data.is_null() {
        return Err(BoundaryRefusal {
            seq,
            limit: BoundaryLimit::NullData,
        });
    }
    Ok((seq, data.get(key)))
}

/// The final copy, refused where it is `undefined` or holds a number whose
/// JavaScript value this port does not claim.
fn settle((seq, value): Copied<'_>) -> Result<Value, BoundaryRefusal> {
    let refuse = |limit| BoundaryRefusal { seq, limit };
    let value = value.ok_or_else(|| refuse(BoundaryLimit::UndefinedMember))?;
    let mut pending = vec![value];
    while let Some(next) = pending.pop() {
        match next {
            Value::Number(number) => {
                let safe = number.as_u64().is_some_and(|n| n <= MAX_SAFE_INTEGER)
                    || number
                        .as_i64()
                        .is_some_and(|n| n.unsigned_abs() <= MAX_SAFE_INTEGER);
                if !safe {
                    return Err(refuse(BoundaryLimit::Number));
                }
            }
            Value::Array(items) => pending.extend(items),
            Value::Object(fields) => pending.extend(fields.values()),
            Value::Null | Value::Bool(_) | Value::String(_) => {}
        }
    }
    Ok(clone_value(value))
}
