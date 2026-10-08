//! The released v1→v2 migration's decoded stage over a decoded v1 Session,
//! as `sessionFormatV1ToV2` in
//! `packages/session/session-format-v1-to-v2/src/migration.ts` runs it when
//! v1 is the first stage of a chain, which is how production reads a v1
//! file: `migrateHeader`, `assertReleasedV2Header`, then the stage that
//! `createStage({ sourceKind: 'decoded' })` builds, fed each decoded event
//! and packed run, then `finish`.
//!
//! `DecodedReleasedV1ToV2Stage` is the transformed stage with one addition:
//! before each event whose `type` is not `assistant/chunk` and has a
//! released-v0 disposition, `transformEvent` runs
//! `assertReleasedEventPayload(event, 1)` from
//! `packages/session/session-format-v0-to-v1/src/validation.ts`. The stage
//! does not override `transformRun`, so a packed Assistant chunk row reaches
//! the transformed stage's run path unchecked. The check reads only its own
//! event, so TypeScript's first refusal is at the earliest item where the
//! payload check or the transformed stage refuses, and at one event the
//! payload check refuses first. A refusal at `finish` is reported only when
//! no item refuses. [`migrate_v1_to_v2_decoded`] runs the batch
//! [`migrate_v1_to_v2_transformed_items`] once and payload-checks the event
//! items up to and including its refusal.
//!
//! An item refusal is located at the item's first expanded seq, the count of
//! events the decoder emitted before it, which is where TypeScript's decoder
//! context reports it.

use serde_json::Value;

use crate::v0_to_v1::{assert_event_payload, has_released_v0_disposition};
use crate::v1_codec::{DecodedV1Items, V1Item};
use crate::v1_to_v2::{
    MigratedV1ToV2, V1ToV2Limit, V1ToV2Location, V1ToV2Refusal, migrate_v1_to_v2_transformed_items,
};
use crate::v2_to_v3::StageError;

/// The error class TypeScript throws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V1ToV2DecodedClass {
    /// `SessionFormatError`: the header check or the payload check.
    Format,
    /// `SessionFormatUnsupportedMigrationError`, a subclass of
    /// `SessionFormatError`: the transformed stage's own refusals.
    Unsupported,
}

/// Why a decoded v1 Session was not migrated by the decoded stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V1ToV2DecodedRefusal {
    /// TypeScript throws `class` at `location` with exactly `message`. A
    /// [`V1ToV2Location::Event`] is the first expanded seq of the refusing
    /// event or packed run.
    Rejected {
        location: V1ToV2Location,
        class: V1ToV2DecodedClass,
        message: String,
    },
    /// This crate does not decide the TypeScript outcome at `location`, an
    /// expanded seq as for `Rejected`; nothing is claimed. Every item before
    /// `location` passed both checks, so a limit can hide a later TypeScript
    /// refusal, never an earlier one.
    NativeSubset {
        location: V1ToV2Location,
        limit: V1ToV2DecodedLimit,
    },
}

/// Input whose TypeScript outcome this port does not reproduce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V1ToV2DecodedLimit {
    /// An event `type` that is not a string, which the disposition lookup
    /// converts to a property key before the payload check.
    NonStringType,
    /// A native limit of the payload check, with its name there.
    Payload(String),
    /// A limit of [`migrate_v1_to_v2_transformed_items`].
    Transformed(V1ToV2Limit),
}

impl V1ToV2DecodedLimit {
    /// The limit's name in `conformance/session/v1-to-v2-decoded-cases.json`:
    /// a payload or transformed-stage limit carries that step as a prefix.
    pub fn name(&self) -> String {
        match self {
            Self::NonStringType => "non-string-type".to_owned(),
            Self::Payload(limit) => format!("payload/{limit}"),
            Self::Transformed(limit) => format!("transformed/{}", limit.name()),
        }
    }
}

/// Migrate a decoded v1 Session to v2 as the released decoded stage does.
///
/// `decoded` must be [`decode_v0_v1_items`](crate::decode_v0_v1_items)
/// output that TypeScript decodes without error: each item starts at the
/// count of events before it. A decoded v0 header is refused as
/// `migrateHeader` refuses it. The result is stage output: the
/// `restoreReleasedV2Artifact` check of the complete artifact is not run.
/// The input is never modified.
pub fn migrate_v1_to_v2_decoded(
    decoded: &DecodedV1Items,
) -> Result<MigratedV1ToV2, V1ToV2DecodedRefusal> {
    let transformed = migrate_v1_to_v2_transformed_items(decoded);
    let checked = match &transformed {
        Err(
            V1ToV2Refusal::Rejected {
                location: V1ToV2Location::Header,
                ..
            }
            | V1ToV2Refusal::NativeSubset {
                location: V1ToV2Location::Header,
                ..
            },
        ) => 0,
        Err(
            V1ToV2Refusal::Rejected {
                location: V1ToV2Location::Event(index),
                ..
            }
            | V1ToV2Refusal::NativeSubset {
                location: V1ToV2Location::Event(index),
                ..
            },
        ) => index.saturating_add(1),
        _ => decoded.items.len(),
    };
    // A payload refusal at the transformed stage's refusal item wins. A
    // packed run is never payload-checked.
    let mut seq: u64 = 0;
    for item in decoded.items.iter().take(checked) {
        match item {
            V1Item::Event(event) => {
                if let Some(refusal) = check_event(seq, event) {
                    return Err(refusal);
                }
                seq = seq.saturating_add(1);
            }
            V1Item::AssistantChunkRun(run) => seq = seq.saturating_add(run.event_count()),
        }
    }
    transformed.map_err(|refusal| from_transformed(refusal, &decoded.items))
}

/// The first expanded seq of the item at `index`: the events the items
/// before it expand to.
fn first_seq(items: &[V1Item], index: usize) -> usize {
    let seq = items
        .iter()
        .take(index)
        .fold(0_u64, |seq, item| match item {
            V1Item::Event(_) => seq.saturating_add(1),
            V1Item::AssistantChunkRun(run) => seq.saturating_add(run.event_count()),
        });
    usize::try_from(seq).unwrap_or(usize::MAX)
}

/// The decoded stage's own step for the event at `seq`: a non-string
/// `type`, or the payload check. An `assistant/chunk` event skips it.
fn check_event(seq: u64, event: &Value) -> Option<V1ToV2DecodedRefusal> {
    let location = V1ToV2Location::Event(usize::try_from(seq).unwrap_or(usize::MAX));
    let native = |limit| Some(V1ToV2DecodedRefusal::NativeSubset { location, limit });
    // The codec emits only objects; the transformed stage reports any other
    // value at this item.
    let Value::Object(fields) = event else {
        return None;
    };
    let event_type = match fields.get("type") {
        Some(Value::String(event_type)) => event_type,
        _ => return native(V1ToV2DecodedLimit::NonStringType),
    };
    if event_type == "assistant/chunk" || !has_released_v0_disposition(event_type) {
        return None;
    }
    let refusal = |class, message| {
        Some(V1ToV2DecodedRefusal::Rejected {
            location,
            class,
            message,
        })
    };
    match assert_event_payload(event_type, seq, fields.get("data"), 1) {
        Ok(()) => None,
        Err(StageError::Invalid(message)) => refusal(V1ToV2DecodedClass::Format, message),
        Err(StageError::Unsupported(message)) => refusal(V1ToV2DecodedClass::Unsupported, message),
        Err(StageError::NativeLimit(limit)) => native(V1ToV2DecodedLimit::Payload(limit)),
    }
}

/// A transformed-stage refusal, its item index mapped to the item's first
/// expanded seq: `assertReleasedV1Header` throws a `SessionFormatError`, and
/// the stage its unsupported error.
fn from_transformed(refusal: V1ToV2Refusal, items: &[V1Item]) -> V1ToV2DecodedRefusal {
    let at = |location| match location {
        V1ToV2Location::Event(index) => V1ToV2Location::Event(first_seq(items, index)),
        V1ToV2Location::Header | V1ToV2Location::Finish => location,
    };
    match refusal {
        V1ToV2Refusal::Rejected { location, message } => V1ToV2DecodedRefusal::Rejected {
            location: at(location),
            class: match location {
                V1ToV2Location::Header => V1ToV2DecodedClass::Format,
                V1ToV2Location::Event(_) | V1ToV2Location::Finish => {
                    V1ToV2DecodedClass::Unsupported
                }
            },
            message,
        },
        V1ToV2Refusal::NativeSubset { location, limit } => V1ToV2DecodedRefusal::NativeSubset {
            location: at(location),
            limit: V1ToV2DecodedLimit::Transformed(limit),
        },
    }
}
