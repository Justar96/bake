//! A released v0 or v1 Session read through every format edge to v3, as
//! `createSessionFormatChain` runs it over a decoded Session: a v0 Session
//! through `sessionFormatV0ToV1`, the transformed `sessionFormatV1ToV2`
//! stage, and `sessionFormatV2ToV3`; a v1 Session through the decoded
//! `sessionFormatV1ToV2` stage, which a chain runs when v1 is its first
//! version, and `sessionFormatV2ToV3`.
//!
//! TypeScript streams each decoded event or packed run through every stage
//! before the next one starts, so the refusal it reports is the one at the
//! earliest source item, and within that item the one from the earliest
//! stage. The Rust stages are batch functions over a whole log.
//! [`migrate_released_history`] recovers the streaming order by rerunning
//! each later stage over the source prefix before the earliest refusal found
//! so far: a stage is deterministic over a prefix, and an item's stage-one
//! checks precede its emission.
//!
//! v1→v2 does not emit every item as it arrives: it holds an Assistant
//! attempt's chunks, and the events after its last chunk, until a later item
//! or `finish` closes the attempt. A v2→v3 refusal of an event v1→v2 emitted
//! is located at the item, or `finish`, whose processing emitted it. When
//! v1→v2 refuses an item after it may already have emitted events for it,
//! which v2→v3 would have checked first, the outcome is the
//! `interleaved-emission` limit.
//!
//! Its sources are `packages/session/session-format/src/chain.ts` and the
//! three migrations' `migration.ts`.

use serde_json::Value;

use crate::json_parse::{clone_value, values_equal};

use crate::v0_to_v1::{V0ToV1Location, V0ToV1Refusal, migrate_v0_to_v1};
use crate::v1_codec::{DecodedV1Items, DecodedV1Rows, V1Item};
use crate::v1_to_v2::{
    StreamedV1ToV2, V1ToV2Limit, V1ToV2Location, V1ToV2Refusal, stream_v1_to_v2_items,
};
use crate::v1_to_v2_decoded::{
    V1ToV2DecodedClass, V1ToV2DecodedLimit, V1ToV2DecodedRefusal, check_event, first_seq,
    migrate_v1_to_v2_decoded,
};
use crate::v2_to_v3::{
    MigratedV2, V2ToV3Location, V2ToV3Refusal, check_logical_v2_prefix, migrate_logical_v2,
};

/// The v1→v2 migration's name, which the chain puts in a wrapped refusal.
const V1_TO_V2: &str = "bake-session-format-v1-to-v2";

/// Where a refusal was raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryLocation {
    /// The chain's header migration, before any event.
    Header,
    /// The decoded event at this seq, or the packed run whose first seq it
    /// is, while some stage handled it or the events an earlier stage
    /// emitted for it.
    Event(usize),
    /// The stages' `finish`, after every event.
    Finish,
}

/// Why a decoded v0 or v1 Session was not read to format v3.
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
    /// nothing is claimed. Every source item before `location` passed every
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
    /// An event without `time`, which v1→v2 copies into an event it
    /// synthesizes and JSON cannot carry as `undefined`.
    UntimedEvent,
    /// v1→v2 refuses an item, or at `finish`, after it may already have
    /// emitted a synthesized end-seed, interrupted `turn/end`, or
    /// `goal/change` for it, or a pending Assistant attempt and the events
    /// buffered after it, which v2→v3 would have checked first.
    InterleavedEmission,
    /// A stage disagreed with what an earlier stage or the decoder proved.
    DecodeInvariant,
    /// A limit of [`migrate_v0_to_v1`], with its name there.
    V0ToV1(String),
    /// A limit of the transformed v1→v2 stage a v0 Session takes, as
    /// [`migrate_v1_to_v2_transformed_items`](crate::migrate_v1_to_v2_transformed_items) names it.
    V1ToV2(V1ToV2Limit),
    /// A limit of the decoded v1→v2 stage a v1 Session takes, as
    /// [`migrate_v1_to_v2_decoded`] names it.
    V1ToV2Decoded(V1ToV2DecodedLimit),
    /// A limit of the v2→v3 stage, with its name in [`migrate_v2_rows`](crate::migrate_v2_rows).
    V2ToV3(String),
}

impl HistoryLimit {
    /// The limit's name in `conformance/session/history-cases.json`: a
    /// stage's own limit carries that stage's name as a prefix.
    pub fn name(&self) -> String {
        match self {
            Self::UntimedEvent => "untimed-event".to_owned(),
            Self::InterleavedEmission => "interleaved-emission".to_owned(),
            Self::DecodeInvariant => "decode-invariant".to_owned(),
            Self::V0ToV1(limit) => format!("v0-to-v1/{limit}"),
            Self::V1ToV2(limit) => format!("v1-to-v2/{}", limit.name()),
            Self::V1ToV2Decoded(limit) => format!("v1-to-v2-decoded/{}", limit.name()),
            Self::V2ToV3(limit) => format!("v2-to-v3/{limit}"),
        }
    }
}

fn native(location: HistoryLocation, limit: HistoryLimit) -> HistoryRefusal {
    HistoryRefusal::NativeSubset { location, limit }
}

/// Read a decoded released v0 or v1 Session to format v3.
///
/// `decoded` must come from [`decode_v0_v1_items`](crate::decode_v0_v1_items),
/// which TypeScript would have decoded without error; a codec refusal at a
/// later row than a migration refusal is not modeled. A header with
/// `version` 1 takes the v1 chain, any other the v0 chain. The result is the
/// v3 header, the events the v2→v3 stage emits, and its inherited cut, or the
/// refusal TypeScript's chain reports first. The output is stage output: the
/// catalog's final check of the v3 artifact is not run. The input is never
/// modified.
pub fn migrate_released_history(decoded: &DecodedV1Items) -> Result<MigratedV2, HistoryRefusal> {
    if decoded.header.get("version").and_then(Value::as_u64) == Some(1) {
        from_v1(decoded)
    } else {
        from_v0(decoded)
    }
}

/// The v0 chain. v0→v1 forwards a packed run untouched, so it runs over the
/// expanded events and each run member must come out unchanged.
fn from_v0(decoded: &DecodedV1Items) -> Result<MigratedV2, HistoryRefusal> {
    let rows = DecodedV1Rows {
        header: clone_value(&decoded.header),
        inherited_event_count: decoded.inherited_event_count,
        events: expand(&decoded.items),
    };
    // Step one: the first v0→v1 refusal ends the prefix the later stages see.
    let (mut migrated, stage_one) = match migrate_v0_to_v1(&rows) {
        Ok(migrated) => (migrated, None),
        Err(refusal) => {
            let (seq, refusal) = match v0_to_v1_refusal(refusal) {
                (Some(seq), refusal) => (seq, refusal),
                (None, refusal) => return Err(refusal),
            };
            let prefix = DecodedV1Rows {
                header: clone_value(&rows.header),
                inherited_event_count: rows.inherited_event_count,
                events: rows
                    .events
                    .get(..seq)
                    .unwrap_or_default()
                    .iter()
                    .map(clone_value)
                    .collect(),
            };
            let migrated = migrate_v0_to_v1(&prefix).map_err(|_| invariant(seq))?;
            (migrated, Some((seq, refusal)))
        }
    };
    let end = stage_one.as_ref().map_or(usize::MAX, |(seq, _)| *seq);
    let mut items = Vec::new();
    let mut seq = 0_usize;
    for item in &decoded.items {
        if seq >= end {
            break;
        }
        match item {
            V1Item::Event(_) => {
                let event = migrated.events.get(seq).ok_or_else(|| invariant(seq))?;
                items.push(V1Item::Event(clone_value(event)));
                seq += 1;
            }
            V1Item::AssistantChunkRun(run) => {
                let next = usize::try_from(run.event_count())
                    .ok()
                    .and_then(|count| seq.checked_add(count))
                    .filter(|next| *next <= end)
                    .ok_or_else(|| invariant(seq))?;
                let expanded = run.expand();
                let unchanged = migrated.events.get(seq..next).is_some_and(|events| {
                    events.len() == expanded.len()
                        && events
                            .iter()
                            .zip(&expanded)
                            .all(|(left, right)| values_equal(left, right))
                });
                if !unchanged {
                    return Err(invariant(seq));
                }
                items.push(item.clone());
                seq = next;
            }
        }
    }
    let chain = Chain {
        header: std::mem::take(&mut migrated.header),
        source_cut: migrated.inherited_event_count,
        is_seeded: decoded.header.get("isSeeded") == Some(&Value::Bool(true)),
        first: FirstStage::V0ToV1,
    };
    chain.read(items, stage_one.map(|(_, refusal)| refusal))
}

/// The v1 chain. The decoded v1→v2 stage's payload check emits nothing, so
/// its refusal ends the prefix as a v0→v1 refusal does.
fn from_v1(decoded: &DecodedV1Items) -> Result<MigratedV2, HistoryRefusal> {
    let stage_one = match migrate_v1_to_v2_decoded(decoded) {
        // The v1 decoder already ran the header check the chain repeats.
        Err(
            V1ToV2DecodedRefusal::Rejected {
                location: V1ToV2Location::Header,
                ..
            }
            | V1ToV2DecodedRefusal::NativeSubset {
                location: V1ToV2Location::Header,
                ..
            },
        ) => {
            return Err(native(
                HistoryLocation::Header,
                HistoryLimit::DecodeInvariant,
            ));
        }
        Err(
            V1ToV2DecodedRefusal::Rejected {
                location: V1ToV2Location::Event(seq),
                ..
            }
            | V1ToV2DecodedRefusal::NativeSubset {
                location: V1ToV2Location::Event(seq),
                ..
            },
        ) => {
            let index = item_at(&decoded.items, seq).ok_or_else(|| invariant(seq))?;
            match decoded.items.get(index) {
                // A payload refusal there is the one the decoded stage reported.
                Some(V1Item::Event(event)) => check_event(seq as u64, event)
                    .map(|refusal| (index, payload_refusal(seq, refusal))),
                _ => None,
            }
        }
        _ => None,
    };
    let end = stage_one
        .as_ref()
        .map_or(decoded.items.len(), |(index, _)| *index);
    let chain = Chain {
        header: clone_value(&decoded.header),
        source_cut: decoded.inherited_event_count,
        is_seeded: decoded.header.get("isSeeded") == Some(&Value::Bool(true)),
        first: FirstStage::V1ToV2Decoded,
    };
    let items = decoded.items.get(..end).unwrap_or_default().to_vec();
    chain.read(items, stage_one.map(|(_, refusal)| refusal))
}

/// The decoded stage's payload refusal at `seq` as the chain reports it: a
/// `SessionFormatError` wrapped with the migration's name.
fn payload_refusal(seq: usize, refusal: V1ToV2DecodedRefusal) -> HistoryRefusal {
    let location = HistoryLocation::Event(seq);
    match refusal {
        V1ToV2DecodedRefusal::Rejected {
            class: V1ToV2DecodedClass::Format,
            message,
            ..
        } => HistoryRefusal::Rejected {
            location,
            message: format!("{V1_TO_V2} refuses this format v1 Session: {message}"),
        },
        V1ToV2DecodedRefusal::Rejected { message, .. } => {
            HistoryRefusal::Rejected { location, message }
        }
        V1ToV2DecodedRefusal::NativeSubset { limit, .. } => {
            native(location, HistoryLimit::V1ToV2Decoded(limit))
        }
    }
}

/// Which stage the chain runs first, which names v1→v2's limits.
#[derive(Clone, Copy)]
enum FirstStage {
    V0ToV1,
    V1ToV2Decoded,
}

/// What the v1→v2 and v2→v3 reruns share.
struct Chain {
    /// The v1 header the transformed stage reads.
    header: Value,
    /// The decoded inherited cut, which every prefix keeps.
    source_cut: u64,
    is_seeded: bool,
    first: FirstStage,
}

impl Chain {
    /// The chain after stage one over v1 `items`, the prefix before
    /// `stage_one`, the first refusal stage one raised, if any.
    fn read(
        &self,
        mut items: Vec<V1Item>,
        mut stage_one: Option<HistoryRefusal>,
    ) -> Result<MigratedV2, HistoryRefusal> {
        // An untimed event ends the prefix too.
        let untimed = items
            .iter()
            .enumerate()
            .find_map(|(index, item)| match item {
                V1Item::Event(event) if event.get("time").is_none() => Some(index),
                _ => None,
            });
        if let Some(index) = untimed {
            let seq = first_seq(&items, index);
            stage_one = Some(native(
                HistoryLocation::Event(seq),
                HistoryLimit::UntimedEvent,
            ));
            items.truncate(index);
        }
        // The first v1→v2 refusal in that prefix ends it again.
        let (streamed, refusal) = match self.v1_to_v2(&items) {
            Ok(streamed) => (streamed, stage_one),
            Err(
                V1ToV2Refusal::Rejected {
                    location: V1ToV2Location::Event(index),
                    ..
                }
                | V1ToV2Refusal::NativeSubset {
                    location: V1ToV2Location::Event(index),
                    ..
                },
            ) if index >= items.len() => return Err(invariant(first_seq(&items, index))),
            Err(V1ToV2Refusal::Rejected {
                location: V1ToV2Location::Event(index),
                message,
            }) => {
                let seq = first_seq(&items, index);
                let before = self
                    .v1_to_v2(items.get(..index).unwrap_or_default())
                    .map_err(|_| invariant(seq))?;
                let location = HistoryLocation::Event(seq);
                let refusal = if self.may_have_emitted(&items, index, before.pending) {
                    native(location, HistoryLimit::InterleavedEmission)
                } else {
                    HistoryRefusal::Rejected { location, message }
                };
                items.truncate(index);
                (before, Some(refusal))
            }
            Err(V1ToV2Refusal::NativeSubset {
                location: V1ToV2Location::Event(index),
                limit,
            }) => {
                let seq = first_seq(&items, index);
                items.truncate(index);
                let before = self.v1_to_v2(&items).map_err(|_| invariant(seq))?;
                let location = HistoryLocation::Event(seq);
                (before, Some(native(location, self.v1_to_v2_limit(limit))))
            }
            // The header passed v0→v1 or the v1 decoder, and `finish`
            // refusals come back in the streamed result.
            Err(
                V1ToV2Refusal::Rejected { location, .. }
                | V1ToV2Refusal::NativeSubset { location, .. },
            ) => {
                let location = match location {
                    V1ToV2Location::Header => HistoryLocation::Header,
                    _ => HistoryLocation::Finish,
                };
                return Err(native(location, HistoryLimit::DecodeInvariant));
            }
        };
        // v2→v3 over what v1→v2 emitted for that prefix.
        let streamed_len = streamed.streamed_len;
        let emitted = streamed.events.get(..streamed_len).unwrap_or_default();
        let checked = |events: &[Value]| {
            check_logical_v2_prefix(&streamed.header, events)
                .map_err(|failure| self.v2_to_v3_refusal(failure, &items, streamed_len))
        };
        if let Some(refusal) = refusal {
            checked(emitted)?;
            return Err(refusal);
        }
        match streamed.finished {
            Ok(_) => migrate_logical_v2(&streamed.header, &streamed.events)
                .map_err(|failure| self.v2_to_v3_refusal(failure, &items, streamed_len)),
            Err(refusal) => {
                checked(emitted)?;
                // `finish` emits a pending attempt and its buffered events
                // before it can refuse one of them.
                Err(match refusal {
                    V1ToV2Refusal::Rejected { .. } if streamed.pending => {
                        native(HistoryLocation::Finish, HistoryLimit::InterleavedEmission)
                    }
                    V1ToV2Refusal::Rejected { message, .. } => HistoryRefusal::Rejected {
                        location: HistoryLocation::Finish,
                        message,
                    },
                    V1ToV2Refusal::NativeSubset { limit, .. } => {
                        native(HistoryLocation::Finish, self.v1_to_v2_limit(limit))
                    }
                })
            }
        }
    }

    fn v1_to_v2(&self, items: &[V1Item]) -> Result<StreamedV1ToV2, V1ToV2Refusal> {
        stream_v1_to_v2_items(&DecodedV1Items {
            header: clone_value(&self.header),
            inherited_event_count: self.source_cut,
            items: items.to_vec(),
        })
    }

    fn v1_to_v2_limit(&self, limit: V1ToV2Limit) -> HistoryLimit {
        match self.first {
            FirstStage::V0ToV1 => HistoryLimit::V1ToV2(limit),
            FirstStage::V1ToV2Decoded => {
                HistoryLimit::V1ToV2Decoded(V1ToV2DecodedLimit::Transformed(limit))
            }
        }
    }

    /// Whether v1→v2 may have emitted an event for item `index` before
    /// refusing it: a pending attempt and the events buffered after it,
    /// which `pending` reports for the items before `index`; the end-seed
    /// `ensureTargetCut` synthesizes at the cut; the interrupted `turn/end` a
    /// `turn/start` right after an inbox splice can close; or a legacy goal
    /// message's `goal/change`.
    fn may_have_emitted(&self, items: &[V1Item], index: usize, pending: bool) -> bool {
        let event_at = |at: usize| match items.get(at) {
            Some(V1Item::Event(event)) => Some(event),
            _ => None,
        };
        let type_at = |at: usize| event_at(at).and_then(|event| event.get("type"));
        let event_type = type_at(index).and_then(Value::as_str);
        let at_cut = self.is_seeded
            && first_seq(items, index) as u64 == self.source_cut
            && event_type != Some("session/end-seed");
        let after_splice = event_type == Some("turn/start")
            && index
                .checked_sub(1)
                .and_then(type_at)
                .is_some_and(|previous| previous == "agent/inbox/spliced");
        let goal = event_type == Some("user/message")
            && event_at(index)
                .and_then(|event| event.get("data"))
                .and_then(|data| data.get("source"))
                .is_some_and(|source| {
                    source.get("kind") == Some(&Value::from("goal"))
                        && source.get("change").is_some()
                });
        pending || at_cut || after_splice || goal
    }

    /// A v2→v3 refusal over v1→v2 output located at the source item whose
    /// processing emitted the event it checked. `streamed_len` is the output
    /// length before v1→v2's `finish`; a refusal of what `finish` emitted is
    /// at `Finish`.
    fn v2_to_v3_refusal(
        &self,
        failure: V2ToV3Refusal,
        items: &[V1Item],
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
                match self.origin(items, emitted) {
                    Some(index) => HistoryLocation::Event(first_seq(items, index)),
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

    /// The item whose v1→v2 processing emitted output event `emitted`: the
    /// first `index` whose prefix through it streams more than `emitted`
    /// events before `finish`. Streamed output only grows, so a binary
    /// search finds it.
    fn origin(&self, items: &[V1Item], emitted: usize) -> Option<usize> {
        let (mut low, mut high) = (0, items.len());
        while low < high {
            let middle = low + (high - low) / 2;
            let prefix = self.v1_to_v2(items.get(..=middle)?).ok()?;
            if prefix.streamed_len > emitted {
                high = middle;
            } else {
                low = middle + 1;
            }
        }
        (low < items.len()).then_some(low)
    }
}

/// Every item's events, each run expanded as `run.expand()` yields them.
fn expand(items: &[V1Item]) -> Vec<Value> {
    let mut events = Vec::new();
    for item in items {
        match item {
            V1Item::Event(event) => events.push(clone_value(event)),
            V1Item::AssistantChunkRun(run) => events.extend(run.expand()),
        }
    }
    events
}

/// The index of the item whose first seq is `seq`.
fn item_at(items: &[V1Item], seq: usize) -> Option<usize> {
    let mut first = 0_u64;
    for (index, item) in items.iter().enumerate() {
        if first == seq as u64 {
            return Some(index);
        }
        first = first.saturating_add(match item {
            V1Item::Event(_) => 1,
            V1Item::AssistantChunkRun(run) => run.event_count(),
        });
    }
    None
}

/// A v0→v1 refusal as a history refusal, with its event seq when it has one.
fn v0_to_v1_refusal(refusal: V0ToV1Refusal) -> (Option<usize>, HistoryRefusal) {
    let (location, refusal) = match refusal {
        V0ToV1Refusal::Rejected { location, message } => (location, Ok(message)),
        V0ToV1Refusal::NativeSubset { location, limit } => (location, Err(limit)),
    };
    let (seq, location) = match location {
        V0ToV1Location::Header => (None, HistoryLocation::Header),
        V0ToV1Location::Event(seq) => (Some(seq), HistoryLocation::Event(seq)),
    };
    let refusal = match refusal {
        Ok(message) => HistoryRefusal::Rejected { location, message },
        Err(limit) => native(location, HistoryLimit::V0ToV1(limit)),
    };
    (seq, refusal)
}

fn invariant(seq: usize) -> HistoryRefusal {
    native(HistoryLocation::Event(seq), HistoryLimit::DecodeInvariant)
}
