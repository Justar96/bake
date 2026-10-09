//! Development-only selection of the events `SessionStore.fork` copies from a
//! restored Session into a child, as `_forkSeed` in
//! `packages/core/session/src/index.ts` selects them.
//!
//! The source is the Session `Session.fromRestore` builds from a
//! [`RestoredLog`]: its stored events, with `sourceEventSeqs` expanded, then
//! its closers, then the ordinary `session/end-seed` Session construction
//! appends unless the last event is one. The boundary is an inclusive seq;
//! omitted, it is the source's last event. Checks run in TypeScript's order:
//!
//! 1. The boundary must be a safe integer, then less than the source's next
//!    seq (`INVALID_BOUNDARY`).
//! 2. The last `turn/start` or `turn/end` in the selected prefix must not be a
//!    `turn/start` (`OPEN_TURN`).
//! 3. The child's Session construction snapshots each selected event as
//!    lossless JSON and refuses the first that holds -0 with a plain `Error`.
//!    `Session.fromRestore` takes no such snapshot, so a restored log can
//!    carry -0 in a log-only row that its fork cannot.
//!
//! Every other check of the child's construction is one restoration already
//! passed on the same events, in the same store. The contiguity check cannot
//! fail on a restored log. Neither can a missing or foreign source, or a
//! taken child id, which belong to the live store this port has none of.

use serde_json::Value;

use crate::json_parse::{Deep, clone_value, dismantle};
use crate::{MAX_SAFE_INTEGER, RestoredLog};

/// The events a fork of a restored Session inherits, in seq order. They are
/// held so that the seed drops, copies, and compares without recursing over
/// an event's nesting.
#[derive(Debug, Clone, PartialEq)]
pub struct ForkSeed {
    events: Deep<Vec<Value>>,
    end_seed: bool,
}

impl ForkSeed {
    /// The inherited stored events, as exact JSON with `sourceEventSeqs`
    /// expanded, then the inherited closers. The appended end seed, when
    /// inherited, is not included.
    pub fn events(&self) -> &[Value] {
        &self.events
    }

    /// Whether the seed ends with the ordinary `session/end-seed` that
    /// restoration appended, with `data: {}` and the next seq after
    /// [`ForkSeed::events`]. It is reported, not built, because TypeScript
    /// stamps it with the current time.
    pub const fn end_seed(&self) -> bool {
        self.end_seed
    }

    /// The child's inherited event count: every inherited event, the
    /// appended end seed included.
    pub fn inherited_event_count(&self) -> u64 {
        (self.events.len() + usize::from(self.end_seed)) as u64
    }
}

/// Why a fork of a restored Session inherits nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkRefusal {
    /// `SessionForkError` with code `INVALID_BOUNDARY` and this message.
    InvalidBoundary(String),
    /// `SessionForkError` with code `OPEN_TURN` and this message.
    OpenTurn(String),
    /// The child's Session construction throws a plain `Error` because the
    /// selected event at `index` holds -0.
    NotLossless { index: u64 },
    /// This port cannot reproduce the outcome; nothing is claimed. `seq`
    /// names the row.
    NativeSubset { seq: u64, limit: ForkLimit },
}

impl ForkRefusal {
    /// The TypeScript error's class name, or `None` for a native limit.
    pub const fn class(&self) -> Option<&'static str> {
        match self {
            Self::InvalidBoundary(_) | Self::OpenTurn(_) => Some("SessionForkError"),
            Self::NotLossless { .. } => Some("Error"),
            Self::NativeSubset { .. } => None,
        }
    }

    /// The `SessionForkError` code, or `None` for any other refusal.
    pub const fn code(&self) -> Option<&'static str> {
        match self {
            Self::InvalidBoundary(_) => Some("INVALID_BOUNDARY"),
            Self::OpenTurn(_) => Some("OPEN_TURN"),
            Self::NotLossless { .. } | Self::NativeSubset { .. } => None,
        }
    }

    /// TypeScript's exact message, or `None` for a native limit.
    pub fn message(&self) -> Option<String> {
        match self {
            Self::InvalidBoundary(message) | Self::OpenTurn(message) => Some(message.clone()),
            Self::NotLossless { index } => Some(format!(
                "seed event at index {index} is not losslessly JSON-serializable"
            )),
            Self::NativeSubset { .. } => None,
        }
    }
}

/// Input whose TypeScript fork outcome this port does not reproduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkLimit {
    /// The open `turn/start` named by an `OPEN_TURN` message has a `turn`
    /// other than a string, `null`, an absent member, or a non-negative safe
    /// integer written without a fraction or exponent. JavaScript formats it with
    /// `String`, which this port does not reproduce.
    TurnDiagnostic,
}

/// Select the events `SessionStore.fork(source, boundary)` copies from the
/// Session restored as `restored`, or refuse as TypeScript does.
///
/// A boundary above 2^53 − 1 is refused as JavaScript's rounded number is.
/// A negative or fractional boundary, which TypeScript also refuses, has no
/// `u64` spelling.
pub fn fork_seed(restored: &RestoredLog, boundary: Option<u64>) -> Result<ForkSeed, ForkRefusal> {
    let stored = restored.stored();
    let session = &stored.header().id;
    let rows = stored.rows();
    let closers = restored.closers();
    let listed = rows.len() + closers.len();
    let next_seq = (listed + usize::from(restored.end_seed_appended())) as u64;
    let Some(boundary) = boundary.or_else(|| next_seq.checked_sub(1)) else {
        return Ok(ForkSeed {
            events: Deep::default(),
            end_seed: false,
        });
    };
    if boundary > MAX_SAFE_INTEGER {
        // `String(boundary)` of the double JavaScript holds; both print the
        // shortest round-trip digits without an exponent below 10^21.
        let shown = boundary as f64;
        return Err(ForkRefusal::InvalidBoundary(format!(
            "fork boundary for session \"{session}\" must be a non-negative safe integer, got {shown}"
        )));
    }
    if boundary >= next_seq {
        let last = next_seq
            .checked_sub(1)
            .map_or_else(|| "none".to_owned(), |seq| seq.to_string());
        return Err(ForkRefusal::InvalidBoundary(format!(
            "fork boundary {boundary} does not exist in session \"{session}\" (last seq: {last})"
        )));
    }
    let count = usize::try_from(boundary).expect("below the event count") + 1;
    let end_seed = count > listed;
    let selected = rows.iter().chain(closers).take(count);
    let last_turn = selected
        .clone()
        .zip(0u64..)
        .filter(|(event, _)| event["type"] == "turn/start" || event["type"] == "turn/end")
        .last();
    if let Some((event, seq)) = last_turn
        && event["type"] == "turn/start"
    {
        let turn = turn_label(&event["data"]).ok_or(ForkRefusal::NativeSubset {
            seq,
            limit: ForkLimit::TurnDiagnostic,
        })?;
        return Err(ForkRefusal::OpenTurn(format!(
            "fork boundary {boundary} in session \"{session}\" ends inside open turn {turn}"
        )));
    }
    if let Some((_, index)) = selected
        .zip(0u64..)
        .find(|(event, _)| holds_negative_zero(event))
    {
        return Err(ForkRefusal::NotLossless { index });
    }
    let mut events: Deep<Vec<Value>> = stored
        .events()
        .zip(rows)
        .take(count)
        .map(|(event, row)| {
            let mut row = clone_value(row);
            if let Some(seqs) = &event.envelope().source_event_seqs
                && let Some(fields) = row.as_object_mut()
                && let Some(logged) = fields.insert(
                    "sourceEventSeqs".to_owned(),
                    seqs.iter().copied().map(Value::from).collect(),
                )
            {
                dismantle(logged);
            }
            row
        })
        .collect();
    let inherited = events.len();
    events.extend(closers.iter().take(count - inherited).map(clone_value));
    Ok(ForkSeed { events, end_seed })
}

/// JavaScript's `String(data.turn)` where this port reproduces it.
/// Restoration already limited `null` data in a `turn/start`, where the
/// closer scan would throw, and other non-object data has no `turn` member.
fn turn_label(data: &Value) -> Option<String> {
    match data.get("turn") {
        None => Some("undefined".to_owned()),
        Some(Value::Null) => Some("null".to_owned()),
        Some(Value::String(turn)) => Some(turn.clone()),
        Some(Value::Number(turn)) => turn
            .as_u64()
            .filter(|turn| *turn <= MAX_SAFE_INTEGER)
            .map(|turn| turn.to_string()),
        Some(_) => None,
    }
}

/// Whether `value` holds a number JavaScript reads as -0, which
/// `snapshotJsonValue` refuses. [`crate::parse_json`] parses every such
/// spelling to a negative-zero float. The walk is iterative, so it is safe at
/// any depth the parser produced.
pub(crate) fn holds_negative_zero(value: &Value) -> bool {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Number(number) => {
                if number
                    .as_f64()
                    .is_some_and(|number| number == 0.0 && number.is_sign_negative())
                {
                    return true;
                }
            }
            Value::Array(items) => pending.extend(items),
            Value::Object(fields) => pending.extend(fields.values()),
            Value::Null | Value::Bool(_) | Value::String(_) => {}
        }
    }
    false
}
