//! The adjacent v0→v1 Session format migration over a decoded released v0
//! Session, as `createSessionFormatChain` runs `sessionFormatV0ToV1`.
//!
//! [`migrate_v0_to_v1`] reproduces, for the output of
//! [`decode_v0_v1_rows`](crate::decode_v0_v1_rows) at
//! [`V1CodecVersion::V0`](crate::V1CodecVersion), the chain stream that
//! `createSessionFormatChain({currentVersion: 1, migrations: [sessionFormatV0ToV1]}).createStream`
//! builds over the decoder's header and inherited cut, fed every decoded
//! event, then finished. Its sources are
//! `packages/session/session-format-v0-to-v1/src/{migration,validation,dispositions,validation-helpers,payload-validation}.ts`
//! and `packages/session/session-format/src/chain.ts`.
//!
//! TypeScript migrates each row as the codec admits it, so a migration
//! refusal at one row precedes a codec refusal at a later one; this
//! function starts from a completed decode, so it agrees only where the
//! whole input decodes. Packed Assistant chunk runs pass through the edge
//! untouched in TypeScript; their expanded `assistant/chunk` events pass
//! through every rewrite here unchanged and skip payload validation, as
//! any `assistant/chunk` event does.
//!
//! The output is the edge's, not an opened Session: the whole-artifact
//! relationship checks in `relationships.ts` and the later edges do not run.

mod dispositions;
mod normalize;

use serde_json::Value;

use crate::DecodedV1Rows;
use crate::v2_to_v3::StageError;
use normalize::{LegacyState, TYPE_COERCION, normalize_event};

const MIGRATION: &str = "bake-session-format-v0-to-v1";
/// The input is not the output of a released v0 decode.
const DECODE_INVARIANT: &str = "decode-invariant";
/// A seq spelled as a float, which the codec admits only as a zero at row 0.
const SEQ_FLOAT_LEXEME: &str = "seq-float-lexeme";

/// A migrated v0 Session's logical v1 header, events, and inherited cut.
#[derive(Debug, Clone, PartialEq)]
pub struct MigratedV1 {
    /// The decoded header with `version` 1 in its place.
    pub header: Value,
    /// The normalized events in order, one for each decoded event.
    pub events: Vec<Value>,
    /// The decoded inherited cut, unchanged.
    pub inherited_event_count: u64,
}

/// Where a refusal was raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V0ToV1Location {
    /// The header, before any event.
    Header,
    /// The decoded event at this index, which is also its seq.
    Event(usize),
}

/// Why a decoded v0 Session was not migrated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V0ToV1Refusal {
    /// TypeScript's chain throws `SessionFormatUnsupportedMigrationError` at
    /// `location` with exactly `message`: either the edge's own unsupported
    /// error or an ordinary one wrapped as
    /// `bake-session-format-v0-to-v1 refuses this format v0 Session: <detail>`.
    Rejected {
        location: V0ToV1Location,
        message: String,
    },
    /// This crate does not decide the TypeScript outcome at `location`, the
    /// check that reads the value; nothing is claimed. A limit can hide a
    /// later TypeScript refusal, never an earlier one. `limit` names it:
    /// `seq-float-lexeme` for a seq spelled as a float;
    /// `type-coercion` for a non-string `type`, which TypeScript coerces to
    /// a property key and into messages; `object-prototype-type` for an
    /// inherited JavaScript property name, where TypeScript throws an engine
    /// `TypeError`; `payload-float-lexeme` for a non-negative `f64` where a
    /// count is read or compared; `reference-float-lexeme` for a replacement
    /// `start` spelled as a float; and `legacy-goal-message` for a user
    /// message whose goal source carries `change`, which TypeScript checks
    /// against a `JSON.stringify` rendering. `decode-invariant` guards input
    /// that no released v0 decode produces.
    NativeSubset {
        location: V0ToV1Location,
        limit: String,
    },
}

/// Migrate a decoded released v0 Session to format v1.
///
/// `decoded` must come from [`decode_v0_v1_rows`](crate::decode_v0_v1_rows)
/// at version 0, whose events are in JavaScript member order with dense
/// seqs. Each event is normalized in order with the edge's state: legacy
/// event renames and rewrites, retry and compaction ids, and legacy message
/// ids, then its payload is checked against the frozen released-v0
/// disposition and payload rules and a current-generation delivery marker
/// against the Session id. The inputs are never modified.
pub fn migrate_v0_to_v1(decoded: &DecodedV1Rows) -> Result<MigratedV1, V0ToV1Refusal> {
    let header_limit = || V0ToV1Refusal::NativeSubset {
        location: V0ToV1Location::Header,
        limit: DECODE_INVARIANT.to_owned(),
    };
    let Value::Object(source_header) = &decoded.header else {
        return Err(header_limit());
    };
    let Some(Value::String(session_id)) = source_header.get("id") else {
        return Err(header_limit());
    };
    // TypeScript's chain plans no v0 edge for any other version, so this
    // edge has no outcome to mirror there.
    if source_header.get("version").and_then(Value::as_u64) != Some(0) {
        return Err(header_limit());
    }
    let mut header = source_header.clone();
    header.insert("version".to_owned(), Value::from(1));
    let has_parent = source_header.contains_key("parentSession");
    let mut state = LegacyState::default();
    let mut events = Vec::with_capacity(decoded.events.len());
    for (index, event) in decoded.events.iter().enumerate() {
        let location = V0ToV1Location::Event(index);
        let native = |limit: &str| V0ToV1Refusal::NativeSubset {
            location,
            limit: limit.to_owned(),
        };
        let Value::Object(event) = event else {
            return Err(native(DECODE_INVARIANT));
        };
        let seq = match event.get("seq") {
            Some(Value::Number(seq)) if seq.is_f64() => return Err(native(SEQ_FLOAT_LEXEME)),
            Some(seq) if seq.as_u64() == Some(index as u64) => index as u64,
            _ => return Err(native(DECODE_INVARIANT)),
        };
        if !event.get("type").is_some_and(Value::is_string) {
            return Err(native(TYPE_COERCION));
        }
        let normalized = normalize_event(event.clone(), seq, session_id, &mut state)
            .map_err(|error| refusal(location, error))?;
        let inherited = has_parent && seq < decoded.inherited_event_count;
        if is_wrong_session_marker(&normalized, session_id) && !inherited {
            return Err(refusal(
                location,
                StageError::Invalid(
                    "current-generation delivery marker names the wrong Session".to_owned(),
                ),
            ));
        }
        events.push(Value::Object(normalized));
    }
    Ok(MigratedV1 {
        header: Value::Object(header),
        events,
        inherited_event_count: decoded.inherited_event_count,
    })
}

/// `assertSourceDeliveryMarker` before its inherited exemption: a marker
/// accepted at version 0 whose `sessionId` is not this Session's id. Payload
/// validation has proven the data an object.
fn is_wrong_session_marker(event: &serde_json::Map<String, Value>, session_id: &str) -> bool {
    if event.get("type").and_then(Value::as_str) != Some("session-log-deepseek/delivery-accepted") {
        return false;
    }
    let Some(Value::Object(data)) = event.get("data") else {
        return false;
    };
    // `data['sessionFormatVersion'] ?? 0` compared with `=== 0`.
    let accepted_zero = match data.get("sessionFormatVersion") {
        None | Some(Value::Null) => true,
        Some(Value::Number(version)) => version.as_f64() == Some(0.0),
        Some(_) => false,
    };
    accepted_zero && data.get("sessionId").and_then(Value::as_str) != Some(session_id)
}

/// The chain's `throwUnsupportedRefusal`: unsupported errors pass through,
/// ordinary ones are wrapped with the migration's name.
fn refusal(location: V0ToV1Location, error: StageError) -> V0ToV1Refusal {
    let message = match error {
        StageError::Invalid(detail) => {
            format!("{MIGRATION} refuses this format v0 Session: {detail}")
        }
        StageError::Unsupported(message) => message,
        StageError::NativeLimit(limit) => {
            return V0ToV1Refusal::NativeSubset { location, limit };
        }
    };
    V0ToV1Refusal::Rejected { location, message }
}
