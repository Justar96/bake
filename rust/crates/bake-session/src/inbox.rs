//! Development-only folds of a restored log's pending inbox and consumed work.
//!
//! [`restored_inbox`] folds `inboxProjectionDefinition` in
//! `packages/core/agent-loop/src/inbox.ts`, and [`consumed_work`] folds
//! `foldConsumedWork` in `packages/core/agent/src/consumed-work.ts`. Both read
//! the events the production read path hands the Session: the stored events,
//! then [`RestoredLog::closers`]. The appended `session/end-seed` is left out
//! because neither fold reads that type.
//!
//! `agent/inbox/spliced`, `turn/start`, `step/start`, and `turn/end` payloads
//! are opaque to restoration, so these folds read values no check has
//! validated. The inbox fold wraps every error it throws, `TypeError`s
//! included, in one seq-tagged message, so most malformed splices are an
//! exact refusal. Where JavaScript would coerce a value instead, the outcome
//! is a native limit that claims nothing: a property key that is not a
//! string, a count spelled with a fraction or exponent, a spread string, and
//! a `Set` key that is a number other than a safe integer lexeme.
//! `foldConsumedWork` catches nothing, so each `TypeError` it would throw is
//! a limit too.

use std::collections::HashSet;

use serde_json::Value;

use crate::json_parse::{DebugJson, Deep, DeepJson, clone_value};
use crate::{MAX_SAFE_INTEGER, RestoredLog};

/// The pending messages `inboxProjectionDefinition` restores, in list order,
/// as logged. A message may nest as deep as its log row; this struct's
/// `Drop`, `Clone`, `PartialEq`, and `Debug` do not recurse over it, so its
/// fields cannot be moved out; take them with [`std::mem::take`] and drop
/// each message with [`crate::dismantle`].
#[derive(Default)]
pub struct PendingInbox {
    pub next_turn: Vec<Value>,
    pub next_step: Vec<Value>,
}

impl Drop for PendingInbox {
    fn drop(&mut self) {
        drop(Deep::new(std::mem::take(&mut self.next_turn)));
        drop(Deep::new(std::mem::take(&mut self.next_step)));
    }
}

impl Clone for PendingInbox {
    fn clone(&self) -> Self {
        Self {
            next_turn: self.next_turn.deep_clone(),
            next_step: self.next_step.deep_clone(),
        }
    }
}

impl PartialEq for PendingInbox {
    fn eq(&self, other: &Self) -> bool {
        self.next_turn.deep_eq(&other.next_turn) && self.next_step.deep_eq(&other.next_step)
    }
}

impl Eq for PendingInbox {}

impl std::fmt::Debug for PendingInbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingInbox")
            .field("next_turn", &DebugJson(&self.next_turn))
            .field("next_step", &DebugJson(&self.next_step))
            .finish()
    }
}

/// Why [`restored_inbox`] restored no pending inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxRefusal {
    /// The fold throws for the `agent/inbox/spliced` event at `seq`; see
    /// [`InboxRefusal::message`].
    InvalidSplice { seq: u64 },
    /// This port cannot reproduce the outcome at `seq`; nothing is claimed.
    NativeSubset { seq: u64, limit: InboxLimit },
}

impl InboxRefusal {
    /// TypeScript's exact message, or `None` for a native limit.
    pub fn message(&self) -> Option<String> {
        match self {
            Self::InvalidSplice { seq } => Some(format!(
                "invalid persisted inbox splice at session seq {seq}"
            )),
            Self::NativeSubset { .. } => None,
        }
    }
}

/// A splice whose fold JavaScript decides by coercion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxLimit {
    /// `target` is present but not a string, so JavaScript converts it to a
    /// property key.
    Target,
    /// `start` or `removedCount` is a number spelled with a fraction or
    /// exponent, where only `JSON.parse`'s rounding decides the value.
    Count,
    /// `inserted` is a string, which the spread splits into characters.
    Inserted,
    /// A pending message's `id` is a number other than a safe integer
    /// lexeme, which the duplicate check compares after rounding.
    MessageId,
}

/// The account `foldConsumedWork` gives of a restored log.
///
/// `end` may nest as deep as its log row; this struct's `Drop`, `Clone`,
/// `PartialEq`, and `Debug` do not recurse over it, so its fields cannot be
/// moved out; take `end` with [`std::mem::take`].
pub struct ConsumedWork {
    /// The latest accounting `turn/end`, as logged or as a closer.
    pub end: Option<Value>,
    pub dropped_unrun: bool,
}

impl Drop for ConsumedWork {
    fn drop(&mut self) {
        drop(Deep::new(self.end.take()));
    }
}

impl Clone for ConsumedWork {
    fn clone(&self) -> Self {
        Self {
            end: self.end.deep_clone(),
            dropped_unrun: self.dropped_unrun,
        }
    }
}

impl PartialEq for ConsumedWork {
    fn eq(&self, other: &Self) -> bool {
        self.end.deep_eq(&other.end) && self.dropped_unrun == other.dropped_unrun
    }
}

impl Eq for ConsumedWork {}

impl std::fmt::Debug for ConsumedWork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsumedWork")
            .field("end", &DebugJson(&self.end))
            .field("dropped_unrun", &self.dropped_unrun)
            .finish()
    }
}

/// Input on which `foldConsumedWork` throws a `TypeError` or coerces; nothing
/// is claimed. `seq` names the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsumedWorkLimit {
    pub seq: u64,
    pub cause: ConsumedWorkCoercion,
}

/// Why [`consumed_work`] cannot follow JavaScript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumedWorkCoercion {
    /// A `turn/start`, `step/start`, `turn/end`, or `agent/inbox/spliced`
    /// whose data is `null`.
    Data,
    /// A `turn` that is a number other than a safe integer lexeme.
    Turn,
    /// A cancellation, read while nothing is yet dropped, whose `inserted` is
    /// absent, `null`, or an object.
    Inserted,
    /// A claimed turn without a step ends with an absent or `null` `reason`.
    Reason,
}

/// One restored event: its type, seq, data, and whole JSON.
struct Event<'a> {
    event_type: &'a str,
    seq: u64,
    data: &'a Value,
    json: &'a Value,
}

fn restored_events(restored: &RestoredLog) -> Vec<Event<'_>> {
    let stored = restored.stored();
    let mut events: Vec<Event<'_>> = stored
        .events()
        .zip(stored.rows())
        .map(|(event, json)| {
            let envelope = event.envelope();
            Event {
                event_type: envelope.event_type,
                seq: envelope.seq,
                data: envelope.data,
                json,
            }
        })
        .collect();
    events.extend(restored.closers().iter().map(|json| Event {
        event_type: json["type"].as_str().expect("closer type"),
        seq: json["seq"].as_u64().expect("closer seq"),
        data: &json["data"],
        json,
    }));
    events
}

/// A JavaScript `Set` key under SameValueZero, for the values JSON can hold.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key<'a> {
    Undefined,
    Null,
    Bool(bool),
    Integer(i64),
    String(&'a str),
}

/// How a value behaves as a `Set` key.
enum KeyOf<'a> {
    Key(Key<'a>),
    /// An object or array: each parsed one is its own identity, so it
    /// matches nothing a later event names.
    Unique,
    /// A number whose `JSON.parse` value this port does not reproduce.
    Undecided,
}

/// The key of a member read as `value`; `None` is `undefined`.
fn key(value: Option<&Value>) -> KeyOf<'_> {
    match value {
        None => KeyOf::Key(Key::Undefined),
        Some(Value::Null) => KeyOf::Key(Key::Null),
        Some(Value::Bool(flag)) => KeyOf::Key(Key::Bool(*flag)),
        Some(Value::String(text)) => KeyOf::Key(Key::String(text)),
        Some(Value::Number(number)) => number
            .as_i64()
            .filter(|number| number.unsigned_abs() <= MAX_SAFE_INTEGER)
            .map_or(KeyOf::Undecided, |number| KeyOf::Key(Key::Integer(number))),
        Some(Value::Array(_) | Value::Object(_)) => KeyOf::Unique,
    }
}

/// `value.member` for a value that is not `null` or `undefined`.
fn member<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    value.as_object().and_then(|fields| fields.get(name))
}

/// A splice count as `Number.isSafeInteger` and the bounds read it.
enum Count {
    Safe(u64),
    Invalid,
    Undecided,
}

fn count(value: Option<&Value>) -> Count {
    match value {
        Some(Value::Number(number)) if number.is_f64() => Count::Undecided,
        Some(Value::Number(number)) => number
            .as_u64()
            .filter(|number| *number <= MAX_SAFE_INTEGER)
            .map_or(Count::Invalid, Count::Safe),
        _ => Count::Invalid,
    }
}

/// Fold the restored log's `agent/inbox/spliced` events into its pending
/// messages, as `inboxProjectionDefinition` does from `init()`.
pub fn restored_inbox(restored: &RestoredLog) -> Result<PendingInbox, InboxRefusal> {
    let mut inbox = Lists::default();
    for event in restored_events(restored) {
        if event.event_type == "agent/inbox/spliced" {
            splice(&mut inbox, event.data).map_err(|outcome| match outcome {
                None => InboxRefusal::InvalidSplice { seq: event.seq },
                Some(limit) => InboxRefusal::NativeSubset {
                    seq: event.seq,
                    limit,
                },
            })?;
        }
    }
    Ok(PendingInbox {
        next_turn: std::mem::take(&mut *inbox.next_turn),
        next_step: std::mem::take(&mut *inbox.next_step),
    })
}

/// The pending lists while they fold, held so that a replaced list, a
/// spliced-out message, and a refused fold drop without recursing.
#[derive(Default)]
struct Lists {
    next_turn: Deep<Vec<Value>>,
    next_step: Deep<Vec<Value>>,
}

/// One splice; `Err(None)` is the wrapped throw.
fn splice(inbox: &mut Lists, data: &Value) -> Result<(), Option<InboxLimit>> {
    // `null` data throws reading `target`; any other non-object reads
    // `undefined`, which names no list.
    let Some(splice) = data.as_object() else {
        return Err(None);
    };
    let next_turn = match splice.get("target") {
        Some(Value::String(target)) if target == "next-turn" => true,
        Some(Value::String(target)) if target == "next-step" => false,
        None | Some(Value::String(_)) => return Err(None),
        Some(_) => return Err(Some(InboxLimit::Target)),
    };
    let list = if next_turn {
        &inbox.next_turn
    } else {
        &inbox.next_step
    };
    let start = count(splice.get("start"));
    let removed = match splice.get("removedCount") {
        None | Some(Value::Null) => Count::Safe(0),
        value => count(value),
    };
    if matches!(start, Count::Invalid) || matches!(removed, Count::Invalid) {
        return Err(None);
    }
    // Every other value of `inserted` throws, either at the bounds or at the
    // spread, so it refuses whatever the counts decide.
    let inserted = match splice.get("inserted") {
        Some(Value::Array(items)) => Some(items),
        Some(Value::String(_)) => None,
        _ => return Err(None),
    };
    let (Count::Safe(start), Count::Safe(removed)) = (start, removed) else {
        return Err(Some(InboxLimit::Count));
    };
    let length = list.len() as u64;
    if start > length || start + removed > length {
        return Err(None);
    }
    let inserted = inserted.ok_or(Some(InboxLimit::Inserted))?;
    let start = usize::try_from(start).expect("bounded by a length");
    let removed = usize::try_from(removed).expect("bounded by a length");
    let mut next = list.clone();
    let spliced: Deep<Vec<Value>> = next
        .splice(start..start + removed, inserted.iter().map(clone_value))
        .collect();
    drop(spliced);
    let combined = if next_turn {
        next.iter().chain(&inbox.next_step)
    } else {
        inbox.next_turn.iter().chain(&next)
    };
    unique_ids(combined)?;
    if next_turn {
        inbox.next_turn = next;
    } else {
        inbox.next_step = next;
    }
    Ok(())
}

/// The duplicate check. A `null` message or a repeated identity throws; a
/// numeric identity this port cannot compare is a limit only when nothing
/// else in the list throws.
fn unique_ids<'a>(messages: impl Iterator<Item = &'a Value>) -> Result<(), Option<InboxLimit>> {
    let mut seen = HashSet::new();
    let mut undecided = false;
    for message in messages {
        let id = match message {
            Value::Null => return Err(None),
            Value::Object(fields) => key(fields.get("id")),
            _ => KeyOf::Key(Key::Undefined),
        };
        match id {
            KeyOf::Key(id) => {
                if !seen.insert(id) {
                    return Err(None);
                }
            }
            KeyOf::Unique => {}
            KeyOf::Undecided => undecided = true,
        }
    }
    if undecided {
        return Err(Some(InboxLimit::MessageId));
    }
    Ok(())
}

/// Fold the restored log into its account of consumed work, as
/// `foldConsumedWork` does over the Session's events.
pub fn consumed_work(restored: &RestoredLog) -> Result<ConsumedWork, ConsumedWorkLimit> {
    let mut stepped = HashSet::new();
    let mut claimed = HashSet::new();
    // `None` is an `undefined` open turn, or an object or array one, which
    // no `turn/end` can name again.
    let mut open: Option<Key<'_>> = None;
    let mut end = None;
    let mut dropped_unrun = false;
    for event in restored_events(restored) {
        let seq = event.seq;
        let limit = |cause| ConsumedWorkLimit { seq, cause };
        let data = event.data;
        let turn = || match key(member(data, "turn")) {
            KeyOf::Key(turn) => Ok(Some(turn)),
            KeyOf::Unique => Ok(None),
            KeyOf::Undecided => Err(limit(ConsumedWorkCoercion::Turn)),
        };
        if matches!(
            event.event_type,
            "turn/start" | "step/start" | "agent/inbox/spliced" | "turn/end"
        ) && data.is_null()
        {
            return Err(limit(ConsumedWorkCoercion::Data));
        }
        match event.event_type {
            "turn/start" => open = turn()?.filter(|turn| *turn != Key::Undefined),
            "step/start" => {
                if let Some(turn) = turn()? {
                    stepped.insert(turn);
                }
            }
            "agent/inbox/spliced" => {
                if member(data, "removedCount").is_none() {
                    continue;
                }
                if member(data, "outcome").and_then(Value::as_str) == Some("canceled") {
                    if !dropped_unrun {
                        dropped_unrun = match member(data, "inserted") {
                            Some(Value::Array(items)) => items.is_empty(),
                            Some(Value::String(text)) => text.is_empty(),
                            Some(Value::Number(_) | Value::Bool(_)) => false,
                            _ => return Err(limit(ConsumedWorkCoercion::Inserted)),
                        };
                    }
                } else if let Some(turn) = &open {
                    claimed.insert(turn.clone());
                }
            }
            "turn/end" => {
                let turn = turn()?;
                open = None;
                let Some(turn) = turn else { continue };
                // `stepped.delete` short-circuits, leaving a claim recorded.
                let accounts = stepped.remove(&turn)
                    || (claimed.remove(&turn)
                        && accounts_for_claim(member(data, "reason"))
                            .ok_or_else(|| limit(ConsumedWorkCoercion::Reason))?);
                if accounts {
                    end = Some(Deep::new(clone_value(event.json)));
                    dropped_unrun = false;
                }
            }
            _ => {}
        }
    }
    Ok(ConsumedWork {
        end: end.map(|mut end| std::mem::take(&mut *end)),
        dropped_unrun,
    })
}

/// `accountsForClaim`; `None` where reading `reason.kind` throws.
fn accounts_for_claim(reason: Option<&Value>) -> Option<bool> {
    match reason {
        None | Some(Value::Null) => None,
        Some(reason) => Some(member(reason, "kind").and_then(Value::as_str) != Some("completed")),
    }
}
