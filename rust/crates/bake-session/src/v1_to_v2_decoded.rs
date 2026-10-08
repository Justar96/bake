//! The released v1→v2 migration's decoded stage over a decoded v1 Session,
//! as `sessionFormatV1ToV2` in
//! `packages/session/session-format-v1-to-v2/src/migration.ts` runs it when
//! v1 is the first stage of a chain, which is how production reads a v1
//! file: `migrateHeader`, `assertReleasedV2Header`, then the stage that
//! `createStage({ sourceKind: 'decoded' })` builds, fed every decoded event,
//! then `finish`.
//!
//! `DecodedReleasedV1ToV2Stage` is the transformed stage with one addition:
//! before each event whose `type` is not `assistant/chunk` and has a
//! released-v0 disposition, it runs `assertReleasedEventPayload(event, 1)`
//! from `packages/session/session-format-v0-to-v1/src/validation.ts`. That
//! check reads only its own event, so TypeScript's first refusal is at the
//! earliest event where the payload check or the transformed stage refuses,
//! and at one event the payload check refuses first. A refusal at `finish`
//! is reported only when no event refuses. [`migrate_v1_to_v2_decoded`] runs
//! the batch [`migrate_v1_to_v2_transformed`] once and payload-checks the
//! events up to and including its refusal.
//!
//! The decoder sends a packed Assistant chunk row to `transformRun` as one
//! run, while [`DecodedV1Rows`] holds only its expanded events, so the first
//! `assistant/chunk` event reports [`V1ToV2DecodedLimit::AssistantChunk`]
//! unless an earlier event refuses.

use serde_json::Value;

use crate::v0_to_v1::{assert_event_payload, has_released_v0_disposition};
use crate::v1_codec::DecodedV1Rows;
use crate::v1_to_v2::{
    MigratedV1ToV2, V1ToV2Limit, V1ToV2Location, V1ToV2Refusal, migrate_v1_to_v2_transformed,
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
    /// TypeScript throws `class` at `location` with exactly `message`.
    Rejected {
        location: V1ToV2Location,
        class: V1ToV2DecodedClass,
        message: String,
    },
    /// This crate does not decide the TypeScript outcome at `location`;
    /// nothing is claimed. Every event before `location` passed both checks,
    /// so a limit can hide a later TypeScript refusal, never an earlier one.
    NativeSubset {
        location: V1ToV2Location,
        limit: V1ToV2DecodedLimit,
    },
}

/// Input whose TypeScript outcome this port does not reproduce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V1ToV2DecodedLimit {
    /// An `assistant/chunk` event: TypeScript sends a packed row to
    /// `transformRun`, which the expanded events cannot distinguish.
    AssistantChunk,
    /// An event `type` that is not a string, which the disposition lookup
    /// converts to a property key before the payload check.
    NonStringType,
    /// A native limit of the payload check, with its name there.
    Payload(String),
    /// A limit of [`migrate_v1_to_v2_transformed`].
    Transformed(V1ToV2Limit),
}

impl V1ToV2DecodedLimit {
    /// The limit's name in `conformance/session/v1-to-v2-decoded-cases.json`:
    /// a payload or transformed-stage limit carries that step as a prefix.
    pub fn name(&self) -> String {
        match self {
            Self::AssistantChunk => "assistant-chunk".to_owned(),
            Self::NonStringType => "non-string-type".to_owned(),
            Self::Payload(limit) => format!("payload/{limit}"),
            Self::Transformed(limit) => format!("transformed/{}", limit.name()),
        }
    }
}

/// Migrate a decoded v1 Session to v2 as the released decoded stage does.
///
/// `decoded` must be [`decode_v0_v1_rows`](crate::decode_v0_v1_rows) output
/// that TypeScript decodes without error: each event's `seq` is its index.
/// A decoded v0 header is refused as `migrateHeader` refuses it. The result
/// is stage output: the `restoreReleasedV2Artifact` check of the complete
/// artifact is not run. The input is never modified.
pub fn migrate_v1_to_v2_decoded(
    decoded: &DecodedV1Rows,
) -> Result<MigratedV1ToV2, V1ToV2DecodedRefusal> {
    let transformed = migrate_v1_to_v2_transformed(decoded);
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
        _ => decoded.events.len(),
    };
    // A payload refusal at the transformed stage's refusal index wins.
    for (index, event) in decoded.events.iter().take(checked).enumerate() {
        if let Some(refusal) = check_event(index, event) {
            return Err(refusal);
        }
    }
    transformed.map_err(from_transformed)
}

/// The decoded stage's own step for the event at `index`: the first
/// `assistant/chunk` limit, a non-string `type`, or the payload check.
fn check_event(index: usize, event: &Value) -> Option<V1ToV2DecodedRefusal> {
    let location = V1ToV2Location::Event(index);
    let native = |limit| Some(V1ToV2DecodedRefusal::NativeSubset { location, limit });
    // The codec emits only objects; the transformed stage reports any other
    // value at this index.
    let Value::Object(fields) = event else {
        return None;
    };
    let event_type = match fields.get("type") {
        Some(Value::String(event_type)) => event_type,
        _ => return native(V1ToV2DecodedLimit::NonStringType),
    };
    if event_type == "assistant/chunk" {
        return native(V1ToV2DecodedLimit::AssistantChunk);
    }
    if !has_released_v0_disposition(event_type) {
        return None;
    }
    let refusal = |class, message| {
        Some(V1ToV2DecodedRefusal::Rejected {
            location,
            class,
            message,
        })
    };
    match assert_event_payload(event_type, index as u64, fields.get("data"), 1) {
        Ok(()) => None,
        Err(StageError::Invalid(message)) => refusal(V1ToV2DecodedClass::Format, message),
        Err(StageError::Unsupported(message)) => refusal(V1ToV2DecodedClass::Unsupported, message),
        Err(StageError::NativeLimit(limit)) => native(V1ToV2DecodedLimit::Payload(limit)),
    }
}

/// A transformed-stage refusal: `assertReleasedV1Header` throws a
/// `SessionFormatError`, and the stage its unsupported error.
fn from_transformed(refusal: V1ToV2Refusal) -> V1ToV2DecodedRefusal {
    match refusal {
        V1ToV2Refusal::Rejected { location, message } => V1ToV2DecodedRefusal::Rejected {
            location,
            class: match location {
                V1ToV2Location::Header => V1ToV2DecodedClass::Format,
                V1ToV2Location::Event(_) | V1ToV2Location::Finish => {
                    V1ToV2DecodedClass::Unsupported
                }
            },
            message,
        },
        V1ToV2Refusal::NativeSubset { location, limit } => V1ToV2DecodedRefusal::NativeSubset {
            location,
            limit: V1ToV2DecodedLimit::Transformed(limit),
        },
    }
}
