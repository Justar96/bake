//! Development-only folds of a restored Session's subagent identity and
//! active-turn timing, as `subagentIdentityProjectionDefinition` and
//! `subagentTimingProjectionDefinition` in
//! `packages/subagent/subagent/src/projection.ts` fold the same events.
//!
//! Both folds run from `init()` over the stored events, the inherited prefix
//! included, and then the closers. Every `subagent/descriptor` resets both
//! folds, so an inherited ancestor descriptor counts until the child's own
//! replaces it.
//!
//! [`subagent_identity`] keeps the last descriptor's identity. It validates
//! the whole payload as `foldSubagentDescriptor` in
//! `packages/subagent/subagent/src/descriptor.ts` does, including fields the
//! identity does not expose. A payload that parsing rejects, or whose version
//! is not `SUBAGENT_DESCRIPTOR_VERSION`, 3, resets the identity to `None`.
//! Restoration admits any JSON as descriptor data, and every part of it has an
//! exact answer here: `JSON.parse` and the workspace's `float_roundtrip`
//! parsing read a number the same way, so comparing the version with 3 needs
//! no native limit.
//!
//! [`subagent_timing`] reads each event's envelope `time`, a safe integer that
//! may be negative. A `turn/start` before any descriptor is held as the
//! pending start, and the descriptor opens the active interval from it, or
//! from an interval already open. After a descriptor, a `turn/start` opens a
//! fresh interval, a `turn/end` adds its non-negative length to the settled
//! total and closes it, and any other event, a closer included, moves its
//! `through`. A `turn/end` before any descriptor drops the pending start.
//! The settled total is a JavaScript number, so this fold uses `f64`
//! arithmetic in the same order: each safe-integer time converts exactly,
//! and the length and the running total round as JavaScript rounds them,
//! even above 2^53 − 1.
//!
//! Session construction may append a `session/end-seed` after the closers.
//! It changes no identity, but it is stamped with the current time, so it
//! would move an open interval's `through`. Neither fold includes it, as the
//! fold over a log's rows and closers does not.

use serde_json::{Map, Value};

use crate::RestoredLog;

/// `SUBAGENT_DESCRIPTOR_VERSION`, the only descriptor version parsed.
const DESCRIPTOR_VERSION: f64 = 3.0;

const ONE_SHOT_KEYS: [&str; 4] = ["version", "mode", "provider", "label"];
const CONTINUABLE_KEYS: [&str; 9] = [
    "version",
    "mode",
    "provider",
    "label",
    "agentProvider",
    "agentModel",
    "agentReasoningEffort",
    "persona",
    "toolFilter",
];
const TOOL_FILTER_KEYS: [&str; 2] = ["allow", "deny"];

/// The identity of the last valid descriptor, with that event's seq.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentIdentity {
    OneShot { seq: u64, label: Option<String> },
    Continuable { seq: u64, label: String },
}

/// An open interval, from its turn's start to the latest event's time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentActiveInterval {
    pub since: i64,
    pub through: i64,
}

/// The timing fold's full state. Its view drops `pending_turn_start` and
/// `descriptor_seen`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubagentTimingState {
    /// Milliseconds across the turns ended since the last descriptor. Each
    /// arithmetic operation rounds as in JavaScript.
    pub settled_ms: f64,
    pub active: Option<SubagentActiveInterval>,
    /// The latest turn start before any descriptor; `None` once one is seen.
    pub pending_turn_start: Option<i64>,
    pub descriptor_seen: bool,
}

/// Fold `subagentIdentityProjectionDefinition` over the restored stored
/// events and then the closers. `None` is the `null` view: no descriptor, or
/// a last one that does not parse.
pub fn subagent_identity(restored: &RestoredLog) -> Option<SubagentIdentity> {
    events(restored)
        .filter(|event| event.event_type == "subagent/descriptor")
        .last()
        .and_then(|event| descriptor_identity(event.seq, event.data))
}

/// Fold `subagentTimingProjectionDefinition` over the restored stored events
/// and then the closers.
pub fn subagent_timing(restored: &RestoredLog) -> SubagentTimingState {
    let mut state = SubagentTimingState {
        settled_ms: 0.0,
        active: None,
        pending_turn_start: None,
        descriptor_seen: false,
    };
    for Event {
        event_type, time, ..
    } in events(restored)
    {
        match event_type {
            "turn/start" if state.descriptor_seen => {
                state.active = Some(SubagentActiveInterval {
                    since: time,
                    through: time,
                });
            }
            "turn/start" => state.pending_turn_start = Some(time),
            "subagent/descriptor" => {
                let since = state
                    .active
                    .map(|active| active.since)
                    .or(state.pending_turn_start);
                state = SubagentTimingState {
                    settled_ms: 0.0,
                    active: since.map(|since| SubagentActiveInterval {
                        since,
                        through: time,
                    }),
                    pending_turn_start: None,
                    descriptor_seen: true,
                };
            }
            "turn/end" if !state.descriptor_seen => state.pending_turn_start = None,
            "turn/end" => {
                if let Some(active) = state.active.take() {
                    state.settled_ms = settle(state.settled_ms, time, active.since);
                }
            }
            _ => {
                if let Some(active) = &mut state.active {
                    active.through = time;
                }
            }
        }
    }
    state
}

/// `settledMs + Math.max(0, time - since)` in JavaScript's order. Both
/// times are safe integers, so their conversion to `f64` is exact.
fn settle(settled_ms: f64, time: i64, since: i64) -> f64 {
    settled_ms + (time as f64 - since as f64).max(0.0)
}

struct Event<'a> {
    seq: u64,
    event_type: &'a str,
    time: i64,
    data: &'a Value,
}

/// The stored events and then the closers.
fn events(restored: &RestoredLog) -> impl Iterator<Item = Event<'_>> {
    let stored = restored.stored().events().map(|event| {
        let envelope = event.envelope();
        Event {
            seq: envelope.seq,
            event_type: envelope.event_type,
            time: envelope.time,
            data: envelope.data,
        }
    });
    let closers = restored.closers().iter().map(|closer| Event {
        seq: closer["seq"].as_u64().expect("closer seq"),
        event_type: closer["type"].as_str().expect("closer type"),
        time: closer["time"].as_i64().expect("closer time"),
        data: &closer["data"],
    });
    stored.chain(closers)
}

/// `descriptorIdentity`: the identity of a supported, complete descriptor,
/// or `None` where parsing rejects the payload or does not support its
/// version.
fn descriptor_identity(seq: u64, data: &Value) -> Option<SubagentIdentity> {
    let fields = data.as_object()?;
    // JavaScript compares the parsed number with 3; `float_roundtrip` reads
    // every spelling, `3.0` and `3e0` included, to the same value.
    if fields.get("version")?.as_f64()? != DESCRIPTOR_VERSION {
        return None;
    }
    let one_shot = match fields.get("mode")?.as_str()? {
        "one-shot" => true,
        "continuable" => false,
        _ => return None,
    };
    let keys: &[&str] = if one_shot {
        &ONE_SHOT_KEYS
    } else {
        &CONTINUABLE_KEYS
    };
    if !known_keys(fields, keys) || !fields.get("provider")?.is_string() {
        return None;
    }
    if one_shot {
        let label = optional_string(fields, "label")?.map(str::to_owned);
        return Some(SubagentIdentity::OneShot { seq, label });
    }
    let label = fields.get("label")?.as_str()?.to_owned();
    for key in [
        "agentProvider",
        "agentModel",
        "agentReasoningEffort",
        "persona",
    ] {
        optional_string(fields, key)?;
    }
    if let Some(filter) = fields.get("toolFilter") {
        tool_filter(filter)?;
    }
    Some(SubagentIdentity::Continuable { seq, label })
}

/// `parseToolFilter`: an object of only `allow` and `deny`, declaring at
/// least one, each an array of strings.
fn tool_filter(value: &Value) -> Option<()> {
    let fields = value.as_object()?;
    if !known_keys(fields, &TOOL_FILTER_KEYS) || fields.is_empty() {
        return None;
    }
    fields
        .values()
        .all(|list| {
            list.as_array()
                .is_some_and(|items| items.iter().all(Value::is_string))
        })
        .then_some(())
}

fn known_keys(fields: &Map<String, Value>, keys: &[&str]) -> bool {
    fields.keys().all(|key| keys.contains(&key.as_str()))
}

/// An absent field is `Some(None)`; a present non-string, `null` included,
/// is `None`.
fn optional_string<'a>(fields: &'a Map<String, Value>, key: &str) -> Option<Option<&'a str>> {
    match fields.get(key) {
        None => Some(None),
        Some(value) => value.as_str().map(Some),
    }
}
