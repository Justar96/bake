//! A released v0 Session read through every format edge to v3, as
//! `createSessionFormatChain` runs `sessionFormatV0ToV1`,
//! `sessionFormatV1ToV2`, and `sessionFormatV2ToV3` over a decoded v0
//! Session, without Assistant chunks.
//!
//! TypeScript streams each decoded event through every stage before the next
//! one starts, so the refusal it reports is the one at the earliest source
//! event, and within that event the one from the earliest stage. The Rust
//! stages are batch functions over a whole log. [`migrate_released_v0_history`]
//! recovers the streaming order by rerunning each later stage over the source
//! prefix before the earliest refusal found so far: a stage is deterministic
//! over a prefix, and an event's stage-one checks precede its emission.
//!
//! Its sources are `packages/session/session-format/src/chain.ts` and the
//! three migrations' `migration.ts`. The v1→v2 stage is the transformed one,
//! as a chain runs it after v0→v1. A directly decoded v1 Session takes the
//! decoded stage instead, ported as [`crate::migrate_v1_to_v2_decoded`] but
//! not yet routed through this chain.

use serde_json::Value;

use crate::v0_to_v1::{V0ToV1Location, V0ToV1Refusal, migrate_v0_to_v1};
use crate::v1_codec::DecodedV1Rows;
use crate::v1_to_v2::{
    MigratedV1ToV2, V1ToV2Limit, V1ToV2Location, V1ToV2Refusal, migrate_v1_to_v2_transformed,
};
use crate::v2_to_v3::{
    MigratedV2, V2ToV3Location, V2ToV3Refusal, check_logical_v2_prefix, migrate_logical_v2,
};

/// Where a refusal was raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryLocation {
    /// The chain's header migration, before any event.
    Header,
    /// The decoded event at this index, while some stage handled it or the
    /// events an earlier stage emitted for it.
    Event(usize),
    /// The stages' `finish`, after every event.
    Finish,
}

/// Why a decoded v0 Session was not read to format v3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryRefusal {
    /// TypeScript's chain throws `SessionFormatUnsupportedMigrationError` at
    /// `location` with exactly `message`: a stage's own unsupported error, or
    /// an ordinary one wrapped as `<migration> refuses this format vN Session: <detail>`.
    Rejected {
        location: HistoryLocation,
        message: String,
    },
    /// This crate does not decide the TypeScript outcome at `location`;
    /// nothing is claimed. Every source event before `location` passed every
    /// stage, so a limit can hide a later TypeScript refusal, never an
    /// earlier one.
    NativeSubset {
        location: HistoryLocation,
        limit: HistoryLimit,
    },
}

/// Input whose TypeScript outcome this port does not reproduce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryLimit {
    /// A decoded v1 Session, which a chain reads with the v1→v2 decoded stage.
    V1DecodedStage,
    /// An `assistant/chunk` event that v0→v1 admitted. TypeScript starts an
    /// Assistant attempt there, and a packed run takes a different path than
    /// its expanded events.
    AssistantChunk,
    /// An event without `time`, which v1→v2 copies into an event it
    /// synthesizes and JSON cannot carry as `undefined`.
    UntimedEvent,
    /// v1→v2 refuses an event after it may already have emitted a
    /// synthesized end-seed, interrupted `turn/end`, or `goal/change` for
    /// it, which v2→v3 would have checked first.
    InterleavedEmission,
    /// A stage disagreed with what an earlier stage or the decoder proved.
    DecodeInvariant,
    /// A limit of [`migrate_v0_to_v1`], with its name there.
    V0ToV1(String),
    /// A limit of [`migrate_v1_to_v2_transformed`].
    V1ToV2(V1ToV2Limit),
    /// A limit of the v2→v3 stage, with its name in [`migrate_v2_rows`](crate::migrate_v2_rows).
    V2ToV3(String),
}

impl HistoryLimit {
    /// The limit's name in `conformance/session/history-cases.json`: a
    /// stage's own limit carries that stage's name as a prefix.
    pub fn name(&self) -> String {
        match self {
            Self::V1DecodedStage => "v1-decoded-stage".to_owned(),
            Self::AssistantChunk => "assistant-chunk".to_owned(),
            Self::UntimedEvent => "untimed-event".to_owned(),
            Self::InterleavedEmission => "interleaved-emission".to_owned(),
            Self::DecodeInvariant => "decode-invariant".to_owned(),
            Self::V0ToV1(limit) => format!("v0-to-v1/{limit}"),
            Self::V1ToV2(limit) => format!("v1-to-v2/{}", limit.name()),
            Self::V2ToV3(limit) => format!("v2-to-v3/{limit}"),
        }
    }
}

fn native(location: HistoryLocation, limit: HistoryLimit) -> HistoryRefusal {
    HistoryRefusal::NativeSubset { location, limit }
}

/// Read a decoded released v0 Session to format v3.
///
/// `decoded` must come from [`decode_v0_v1_rows`](crate::decode_v0_v1_rows)
/// at version 0, which TypeScript would have decoded without error; a codec
/// refusal at a later row than a migration refusal is not modeled. The
/// result is the v3 header, the events the v2→v3 stage emits, and its
/// inherited cut, or the refusal TypeScript's chain reports first. The
/// output is stage output: the catalog's final check of the v3 artifact is
/// not run. The input is never modified.
pub fn migrate_released_v0_history(decoded: &DecodedV1Rows) -> Result<MigratedV2, HistoryRefusal> {
    if decoded.header.get("version").and_then(Value::as_u64) == Some(1) {
        return Err(native(
            HistoryLocation::Header,
            HistoryLimit::V1DecodedStage,
        ));
    }
    // Step one: the first v0→v1 refusal ends the prefix the later stages see.
    let (v1_header, source_cut, mut v1_events, mut pending) = match migrate_v0_to_v1(decoded) {
        Ok(migrated) => (
            migrated.header,
            migrated.inherited_event_count,
            migrated.events,
            None,
        ),
        Err(refusal) => {
            let (index, refusal) = match v0_to_v1_refusal(refusal) {
                (Some(index), refusal) => (index, refusal),
                (None, refusal) => return Err(refusal),
            };
            let prefix = with_events(decoded, decoded.events.get(..index).unwrap_or_default());
            let migrated = migrate_v0_to_v1(&prefix).map_err(|_| invariant(index))?;
            (
                migrated.header,
                migrated.inherited_event_count,
                migrated.events,
                Some((index, refusal)),
            )
        }
    };
    // Step two: an admitted chunk or an untimed event ends the prefix too.
    let end = pending
        .as_ref()
        .map_or(v1_events.len(), |(index, _)| *index);
    let unported = decoded
        .events
        .iter()
        .take(end)
        .enumerate()
        .find_map(|(index, event)| {
            if event.get("type") == Some(&Value::from("assistant/chunk")) {
                Some((index, HistoryLimit::AssistantChunk))
            } else if event.get("time").is_none() {
                Some((index, HistoryLimit::UntimedEvent))
            } else {
                None
            }
        });
    if let Some((index, limit)) = unported {
        pending = Some((index, native(HistoryLocation::Event(index), limit)));
        v1_events.truncate(index);
    }
    let chain = Chain {
        header: v1_header,
        source_cut,
        is_seeded: decoded.header.get("isSeeded") == Some(&Value::Bool(true)),
    };
    // Step three: the first v1→v2 refusal in that prefix ends it again.
    let migrated = match chain.v1_to_v2(&v1_events) {
        Ok(migrated) => migrated,
        Err(V1ToV2Refusal::Rejected {
            location: V1ToV2Location::Event(index),
            message,
        }) => {
            let refusal = if chain.may_have_emitted(&v1_events, index) {
                native(
                    HistoryLocation::Event(index),
                    HistoryLimit::InterleavedEmission,
                )
            } else {
                HistoryRefusal::Rejected {
                    location: HistoryLocation::Event(index),
                    message,
                }
            };
            v1_events.truncate(index);
            pending = Some((index, refusal));
            chain.v1_to_v2(&v1_events).map_err(|_| invariant(index))?
        }
        Err(V1ToV2Refusal::NativeSubset {
            location: V1ToV2Location::Event(index),
            limit,
        }) => {
            v1_events.truncate(index);
            pending = Some((
                index,
                native(HistoryLocation::Event(index), HistoryLimit::V1ToV2(limit)),
            ));
            chain.v1_to_v2(&v1_events).map_err(|_| invariant(index))?
        }
        // The header was v0→v1's output, and every event before the end has
        // a `time`, so neither the header check nor `finish` can refuse.
        Err(
            V1ToV2Refusal::Rejected { location, .. } | V1ToV2Refusal::NativeSubset { location, .. },
        ) => {
            let location = match location {
                V1ToV2Location::Header => HistoryLocation::Header,
                _ => HistoryLocation::Finish,
            };
            return Err(native(location, HistoryLimit::DecodeInvariant));
        }
    };
    // Step four: v2→v3 over what v1→v2 emitted for that prefix.
    match pending {
        Some((index, refusal)) => {
            let emitted = chain.without_finish_seed(migrated.events, index);
            match check_logical_v2_prefix(&migrated.header, &emitted) {
                Ok(()) => Err(refusal),
                Err(failure) => Err(chain.v2_to_v3_refusal(failure, &v1_events, emitted.len())),
            }
        }
        None => {
            let streamed = v1_events.len();
            let before_finish = chain
                .without_finish_seed(migrated.events.clone(), streamed)
                .len();
            migrate_logical_v2(&migrated.header, &migrated.events)
                .map_err(|failure| chain.v2_to_v3_refusal(failure, &v1_events, before_finish))
        }
    }
}

/// What the v1→v2 and v2→v3 reruns share.
struct Chain {
    /// The v1 header v0→v1 emitted.
    header: Value,
    /// The decoded inherited cut, which every prefix keeps.
    source_cut: u64,
    is_seeded: bool,
}

impl Chain {
    fn v1_to_v2(&self, events: &[Value]) -> Result<MigratedV1ToV2, V1ToV2Refusal> {
        migrate_v1_to_v2_transformed(&DecodedV1Rows {
            header: self.header.clone(),
            inherited_event_count: self.source_cut,
            events: events.to_vec(),
        })
    }

    /// v1→v2 output for the first `streamed` source events without the
    /// end-seed `finish` adds. Without chunks every source event emits at
    /// least itself, so `finish` adds one exactly when a seeded log has
    /// emitted nothing at or after its cut.
    fn without_finish_seed(&self, mut events: Vec<Value>, streamed: usize) -> Vec<Value> {
        if self.is_seeded && streamed as u64 <= self.source_cut {
            events.pop();
        }
        events
    }

    /// Whether v1→v2 may have emitted an event for source event `index`
    /// before refusing it: the end-seed `ensureTargetCut` synthesizes at the
    /// cut, the interrupted `turn/end` a `turn/start` right after an inbox
    /// splice can close, or a legacy goal message's `goal/change`.
    fn may_have_emitted(&self, events: &[Value], index: usize) -> bool {
        let type_at = |at: usize| events.get(at).and_then(|event| event.get("type"));
        let event_type = type_at(index).and_then(Value::as_str);
        let at_cut = self.is_seeded
            && index as u64 == self.source_cut
            && event_type != Some("session/end-seed");
        let after_splice = event_type == Some("turn/start")
            && index
                .checked_sub(1)
                .and_then(type_at)
                .is_some_and(|previous| previous == "agent/inbox/spliced");
        let goal = event_type == Some("user/message")
            && events
                .get(index)
                .and_then(|event| event.get("data"))
                .and_then(|data| data.get("source"))
                .is_some_and(|source| {
                    source.get("kind") == Some(&Value::from("goal"))
                        && source.get("change").is_some()
                });
        at_cut || after_splice || goal
    }

    /// A v2→v3 refusal over v1→v2 output located at the source event whose
    /// emission it checked. `streamed_len` is the output length before the
    /// end-seed `finish` adds; a refusal of that end-seed is at `Finish`.
    fn v2_to_v3_refusal(
        &self,
        failure: V2ToV3Refusal,
        v1_events: &[Value],
        streamed_len: usize,
    ) -> HistoryRefusal {
        let (location, refusal) = match failure {
            V2ToV3Refusal::Rejected {
                location, message, ..
            } => (location, Ok(message)),
            V2ToV3Refusal::NativeSubset { location, limit } => (location, Err(limit)),
        };
        let location = match location {
            V2ToV3Location::Header => HistoryLocation::Header,
            V2ToV3Location::Row(emitted) if emitted < streamed_len => {
                match self.origin(v1_events, emitted) {
                    Some(index) => HistoryLocation::Event(index),
                    None => return native(HistoryLocation::Finish, HistoryLimit::DecodeInvariant),
                }
            }
            V2ToV3Location::Row(_) | V2ToV3Location::Finish => HistoryLocation::Finish,
        };
        match refusal {
            Ok(message) => HistoryRefusal::Rejected { location, message },
            Err(limit) => native(location, HistoryLimit::V2ToV3(limit)),
        }
    }

    /// The source event whose v1→v2 emission includes output event
    /// `emitted`: the first `index` whose prefix through it emits more than
    /// `emitted` events. Prefix output only grows, so a binary search finds it.
    fn origin(&self, v1_events: &[Value], emitted: usize) -> Option<usize> {
        let (mut low, mut high) = (0, v1_events.len());
        while low < high {
            let middle = low + (high - low) / 2;
            let streamed = middle + 1;
            let prefix = self.v1_to_v2(&v1_events[..streamed]).ok()?;
            if self.without_finish_seed(prefix.events, streamed).len() > emitted {
                high = middle;
            } else {
                low = middle + 1;
            }
        }
        (low < v1_events.len()).then_some(low)
    }
}

/// A v0→v1 refusal as a history refusal, with its event index when it has one.
fn v0_to_v1_refusal(refusal: V0ToV1Refusal) -> (Option<usize>, HistoryRefusal) {
    let (location, refusal) = match refusal {
        V0ToV1Refusal::Rejected { location, message } => (location, Ok(message)),
        V0ToV1Refusal::NativeSubset { location, limit } => (location, Err(limit)),
    };
    let (index, location) = match location {
        V0ToV1Location::Header => (None, HistoryLocation::Header),
        V0ToV1Location::Event(index) => (Some(index), HistoryLocation::Event(index)),
    };
    let refusal = match refusal {
        Ok(message) => HistoryRefusal::Rejected { location, message },
        Err(limit) => native(location, HistoryLimit::V0ToV1(limit)),
    };
    (index, refusal)
}

fn with_events(decoded: &DecodedV1Rows, events: &[Value]) -> DecodedV1Rows {
    DecodedV1Rows {
        header: decoded.header.clone(),
        inherited_event_count: decoded.inherited_event_count,
        events: events.to_vec(),
    }
}

fn invariant(index: usize) -> HistoryRefusal {
    native(HistoryLocation::Event(index), HistoryLimit::DecodeInvariant)
}
