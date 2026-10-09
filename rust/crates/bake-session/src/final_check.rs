//! The catalog's final check of a migrated Session, as
//! `sessionFormatCatalog.createRestore(header, {validation: 'transformed'})`
//! runs it after a historical generation's last format edge:
//! `restoreTransformedCurrent`, which is `restoreReleasedV3Artifact` from
//! `packages/session/session-format-v2-to-v3/src/validation.ts` with
//! `KNOWN_SESSION_EVENT_TYPES`, and the catalog's wrapping of its refusals in
//! `packages/session/session-format/src/catalog.ts`.
//!
//! [`check_transformed_artifact`] runs its stages in TypeScript's order, so
//! the first refusal wins:
//!
//! 1. `assertReleasedV3Header`.
//! 2. For each event in order: `assertV3EventAdmission`, `assertV3Event`
//!    with the installed vocabulary, the protected system head's checks, and
//!    the private relationship view, which renames PTC dispatches to their
//!    released names, turns obsolete ignorable dispatches opaque and a
//!    `system/message` into a `user/message`, rewrites a `TOOL_NOT_STARTED`
//!    repair's message id to the event's own seq, and spells replacement
//!    endpoints `start` and `end`.
//! 3. `validateReleasedV2Artifact` in `current` mode over that view: the
//!    inherited cut, each event's vocabulary, keys, density, time, and
//!    `ignorable` marker, and the inherited end-seed marker.
//! 4. [`check_released_relationships`] over the view with header version 3
//!    and the released v2 extensions: `assistant/attempt` is a step event
//!    and a title request's framed text is not compared.
//!
//! The check decides only acceptance: TypeScript returns the artifact it was
//! given, so a Session it accepts is the migration's output unchanged.
//!
//! # Domain
//!
//! The input must be the output of [`migrate_v2_rows`](crate::migrate_v2_rows)
//! or [`migrate_released_history`](crate::migrate_released_history): its
//! header is a v3 header the migration built, and every event is an object
//! whose members are in JavaScript's own-key order and whose canonical form
//! the migration already checked with `assertV3Event`. On such input stages
//! 1 to 3 never refuse, because the migration checks the same header, admits
//! only audited types, numbers events densely, and places the system head,
//! the inherited cut, and the end-seed marker itself; Rust still runs them
//! in order. Where a value outside that domain would make TypeScript's
//! outcome depend on JavaScript conversion, the result is a native limit.
//! Nothing is read from or written to a file.

use serde_json::{Map, Value};

use crate::json_parse::{Deep, clone_fields, replace_member};
use crate::replay::KNOWN_EVENT_TYPES;
use crate::v2_to_v3::{
    Lookup, StageError, assert_v3_event, exact_keys, is_repair_identity, js_record, lookup, quote,
    v3_safe_integer,
};
use crate::{
    MAX_SAFE_INTEGER, MigratedV2, PathPlatform, RelationshipExtensions, RelationshipRefusal,
    check_released_relationships, is_absolute,
};

type Record = Map<String, Value>;

/// `SURFACE_TYPES` in `session-format-v2-to-v3/src/payload.ts`.
const V3_SURFACE_TYPES: [&str; 4] = [
    "system/message",
    "user/message",
    "assistant/message",
    "tool/result",
];
/// `SURFACE_TYPES` in `session-format-v1-to-v2/src/validation.ts`.
const V2_SURFACE_TYPES: [&str; 3] = ["user/message", "assistant/message", "tool/result"];
const HEADER_REQUIRED: [&str; 5] = ["version", "id", "createdAt", "isSeeded", "delegationDepth"];
const HEADER_OPTIONAL: [&str; 4] = ["cwd", "parentSession", "origin", "agentPreset"];
const EVENT_REQUIRED: [&str; 4] = ["type", "seq", "time", "data"];
const SURFACE_OPTIONAL: [&str; 3] = ["ignorable", "sourceEventSeqs", "surfaceOp"];
const LOG_OPTIONAL: [&str; 1] = ["ignorable"];
const OBSOLETE_DISPATCHES: [&str; 2] = ["tool/code-dispatch-start", "tool/code-dispatch"];
/// The limit for input outside the documented domain.
const PRECONDITION: &str = "precondition";

/// Why the final check refused a migrated Session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalCheckRefusal {
    /// The check throws a `SessionFormatError` with exactly `message`, which
    /// the catalog wraps in a `SessionFormatUnsupportedMigrationError`.
    Rejected { message: String },
    /// The check throws a `SessionFormatUnsupportedMigrationError` with
    /// exactly `message`, which the catalog passes through unwrapped.
    Unsupported { message: String },
    /// This crate does not decide the TypeScript outcome; nothing is claimed.
    /// The name is `header` for a header the check refuses, `precondition`
    /// for an event that is not an object with a string `type` and a `seq`
    /// equal to its index, a limit `assertV3Event` reports under its name in
    /// [`crate::V2ToV3Refusal::NativeSubset`], or a relationship limit
    /// prefixed with `relationships/` and named as
    /// [`crate::RelationshipLimit::name`] names it. None of them is reached
    /// on input in the domain except a relationship limit.
    NativeSubset(String),
}

impl FinalCheckRefusal {
    /// The `SessionFormatUnsupportedMigrationError` message the catalog
    /// throws when the transformed artifact of a v`source_version` Session is
    /// refused, or `None` for a native limit.
    pub fn catalog_message(&self, source_version: u64) -> Option<String> {
        match self {
            Self::Rejected { message } => Some(format!(
                "Session migration from v{source_version} to v3 refuses the transformed artifact: {message}"
            )),
            Self::Unsupported { message } => Some(message.clone()),
            Self::NativeSubset(_) => None,
        }
    }
}

type Checked<T = ()> = Result<T, FinalCheckRefusal>;

fn reject<T>(message: &str) -> Checked<T> {
    Err(FinalCheckRefusal::Rejected {
        message: message.to_owned(),
    })
}

fn limit<T>(name: &str) -> Checked<T> {
    Err(FinalCheckRefusal::NativeSubset(name.to_owned()))
}

fn stage(error: StageError) -> FinalCheckRefusal {
    match error {
        StageError::Invalid(message) => FinalCheckRefusal::Rejected { message },
        StageError::Unsupported(message) => FinalCheckRefusal::Unsupported { message },
        StageError::NativeLimit(name) => FinalCheckRefusal::NativeSubset(name),
    }
}

fn record<'a>(value: Option<&'a Value>, label: &str) -> Checked<&'a Record> {
    js_record(value, label).map_err(stage)
}

/// JavaScript's `value === number` for a count: a JSON number with that
/// double value. Both sides are doubles in JavaScript, and a count is exact.
fn is_number(value: Option<&Value>, number: u64) -> bool {
    // A count is at most 2^53 - 1, so the conversion is exact.
    value.and_then(Value::as_f64) == Some(number as f64)
}

/// Run the catalog's final check over a migrated Session, as the JSONL
/// backend's historical read and write `open` run it, with `platform`'s rule
/// for an absolute `cwd`, which TypeScript takes from its host.
///
/// The result claims TypeScript's outcome only for input in the module's
/// domain; [`FinalCheckRefusal::catalog_message`] gives the message the
/// catalog throws.
pub fn check_transformed_artifact(
    migrated: &MigratedV2,
    platform: PathPlatform,
) -> Result<(), FinalCheckRefusal> {
    let Some(is_seeded) = released_v3_header(&migrated.header, platform) else {
        return limit("header");
    };
    let view = Deep::new(system_head_and_view(&migrated.events)?);
    validate_released_v2(&view, migrated.inherited_event_count, is_seeded)?;
    let extensions = RelationshipExtensions {
        step_events: vec!["assistant/attempt".to_owned()],
        preserved_source_title_request_text: true,
        legacy_interrupted_turn_restart: false,
    };
    check_released_relationships(
        &migrated.header,
        migrated.inherited_event_count,
        &view,
        &extensions,
    )
    .map_err(|refusal| match refusal {
        RelationshipRefusal::Rejected { message, .. } => FinalCheckRefusal::Rejected { message },
        RelationshipRefusal::NativeSubset { limit, .. } => {
            FinalCheckRefusal::NativeSubset(format!("relationships/{}", limit.name()))
        }
    })
}

/// `assertReleasedV3Header`, which ends with `assertReleasedV2Header` over a
/// copy at version 2: the header's `isSeeded`, or `None` when it refuses.
fn released_v3_header(header: &Value, platform: PathPlatform) -> Option<bool> {
    let fields = header.as_object()?;
    let admitted = HEADER_REQUIRED.iter().all(|key| fields.contains_key(*key))
        && fields.keys().all(|key| {
            HEADER_REQUIRED.contains(&key.as_str()) || HEADER_OPTIONAL.contains(&key.as_str())
        });
    let count = |key: &str| {
        fields
            .get(key)
            .and_then(Value::as_u64)
            .is_some_and(|value| value <= MAX_SAFE_INTEGER)
    };
    let optional_string = |key: &str| fields.get(key).is_none_or(Value::is_string);
    let cwd = fields
        .get("cwd")
        .is_none_or(|cwd| cwd.as_str().is_some_and(|cwd| is_absolute(cwd, platform)));
    let origin = fields
        .get("origin")
        .is_none_or(|origin| origin == "subagent");
    let valid = admitted
        && fields.get("version").and_then(Value::as_u64) == Some(3)
        && fields.get("id").is_some_and(Value::is_string)
        && count("createdAt")
        && count("delegationDepth")
        && cwd
        && optional_string("parentSession")
        && optional_string("agentPreset")
        && origin;
    if !valid {
        return None;
    }
    fields.get("isSeeded").and_then(Value::as_bool)
}

/// `assertV3EventAdmission`: an obsolete PTC dispatch must be ignorable.
fn assert_v3_event_admission(event: &Record, event_type: &str, seq: u64) -> Checked {
    if OBSOLETE_DISPATCHES.contains(&event_type)
        && event.get("ignorable") != Some(&Value::Bool(true))
    {
        return Err(FinalCheckRefusal::Unsupported {
            message: format!(
                "format v3 contains unknown event type {} at seq {seq}",
                quote(event_type)
            ),
        });
    }
    Ok(())
}

/// Stage 2 of `restoreReleasedV3Artifact`: each event's admission and
/// canonical form, the protected system head, and the relationship view.
fn system_head_and_view(events: &[Value]) -> Checked<Vec<Value>> {
    // The open step's `turn` and `step` values, as `step/start` carried them.
    let mut step: Option<(Option<&Value>, Option<&Value>)> = None;
    let mut head: Option<u64> = None;
    let mut has_surface = false;
    let mut view = Deep::new(Vec::with_capacity(events.len()));
    for (index, event) in events.iter().enumerate() {
        let seq = u64::try_from(index).unwrap_or(u64::MAX);
        let Some(event) = event.as_object() else {
            return limit(PRECONDITION);
        };
        let Some(event_type) = event.get("type").and_then(Value::as_str) else {
            return limit(PRECONDITION);
        };
        if event.get("seq").and_then(Value::as_u64) != Some(seq) {
            return limit(PRECONDITION);
        }
        assert_v3_event_admission(event, event_type, seq)?;
        assert_v3_event(event, &KNOWN_EVENT_TYPES).map_err(stage)?;
        let surface = V3_SURFACE_TYPES.contains(&event_type);
        let operation = event.get("surfaceOp");
        let appended = operation.is_some_and(|operation| operation == "append");
        if event_type == "step/start" {
            let data = record(event.get("data"), event_type)?;
            step = Some((data.get("turn"), data.get("step")));
        } else if matches!(event_type, "step/end" | "turn/end") {
            step = None;
        }
        let is_head = |seq: u64| head == Some(seq);
        if event_type == "system/message" {
            let data = record(event.get("data"), "system/message")?;
            // `assertV3Event` proved the system coordinates counts.
            let (Some(turn), Some(step_number)) = (
                data.get("turn").and_then(Value::as_u64),
                data.get("step").and_then(Value::as_u64),
            ) else {
                return limit(PRECONDITION);
            };
            let matches = step.is_some_and(|(open_turn, open_step)| {
                is_number(open_turn, turn) && is_number(open_step, step_number)
            });
            if !matches {
                return reject("system/message does not match an open step");
            }
            if has_surface && head.is_none() {
                return reject("system/message requires a protected first surface head");
            }
            if appended {
                if !has_surface {
                    head = Some(seq);
                }
            } else {
                let (start, end) = endpoints(record(operation, "system replacement")?)?;
                if is_head(start) || is_head(end) {
                    if !is_head(start) || !is_head(end) {
                        return reject(
                            "system/message must replace exactly the current system head",
                        );
                    }
                    head = Some(seq);
                }
            }
        } else if surface && !appended {
            let (start, end) = endpoints(record(operation, "surface replacement")?)?;
            if is_head(start) || is_head(end) {
                return reject("surface replacement cannot shadow the protected system head");
            }
        }
        if matches!(event_type, "compaction/prune" | "compaction/summary") {
            let data = record(event.get("data"), event_type)?;
            if let (Some(Value::Array(seqs)), Some(head)) = (data.get("shadowedSeqs"), head)
                && seqs.iter().any(|seq| is_number(Some(seq), head))
            {
                return reject("compaction cannot shadow the protected system head");
            }
        }
        if surface {
            has_surface = true;
        }
        let mut projected = Deep::new(relationship_event(event, event_type, seq)?);
        if surface && !appended {
            let (start, end) = endpoints(record(operation, "surface replacement")?)?;
            let mut released = Map::new();
            released.insert("op".to_owned(), Value::from("replace"));
            released.insert("start".to_owned(), Value::from(start));
            released.insert("end".to_owned(), Value::from(end));
            replace_member(&mut projected, "surfaceOp", Value::Object(released));
        }
        view.push(Value::Object(projected.into_inner()));
    }
    Ok(view.into_inner())
}

/// A surface replacement's `startSeq` and `endSeq`, which `assertV3Event`
/// proved counts.
fn endpoints(replace: &Record) -> Checked<(u64, u64)> {
    match (
        replace.get("startSeq").and_then(Value::as_u64),
        replace.get("endSeq").and_then(Value::as_u64),
    ) {
        (Some(start), Some(end)) => Ok((start, end)),
        _ => limit(PRECONDITION),
    }
}

/// `relationshipEvent`: the private view of one event for the frozen
/// relationship check. Inserting an existing member keeps its position, as
/// an object spread does.
fn relationship_event(event: &Record, event_type: &str, seq: u64) -> Checked<Record> {
    let mut projected = Deep::new(clone_fields(event));
    let renamed = match event_type {
        "tool/ptc-dispatch-start" => Some("tool/code-dispatch-start"),
        "tool/ptc-dispatch" => Some("tool/code-dispatch"),
        "tool/code-dispatch-start" | "tool/code-dispatch" => {
            assert_v3_event_admission(event, event_type, seq)?;
            Some("v3/opaque-released-event")
        }
        _ => None,
    };
    if let Some(renamed) = renamed {
        projected.insert("type".to_owned(), Value::from(renamed));
        return Ok(projected.into_inner());
    }
    if event_type == "system/message" {
        let data = record(event.get("data"), "system data")?;
        let mut message = clone_fields(record(data.get("message"), "system message")?);
        replace_member(&mut message, "role", Value::from("user"));
        projected.insert("type".to_owned(), Value::from("user/message"));
        replace_member(&mut projected, "data", Value::Object(message));
        return Ok(projected.into_inner());
    }
    if event_type != "tool/result" {
        return Ok(projected.into_inner());
    }
    let data = record(event.get("data"), "tool result")?;
    if !data.contains_key("error") {
        return Ok(projected.into_inner());
    }
    let error = record(data.get("error"), "tool error")?;
    if error
        .get("code")
        .is_none_or(|code| code != "TOOL_NOT_STARTED")
    {
        return Ok(projected.into_inner());
    }
    let message = record(data.get("message"), "tool message")?;
    let source = record(message.get("source"), "tool source")?;
    let call_id = source.get("callId");
    if !is_repair_identity(message.get("id"), call_id) {
        return Ok(projected.into_inner());
    }
    // `isRepairIdentity` proved the call id a string.
    let call_id = call_id.and_then(Value::as_str).unwrap_or_default();
    let mut message = clone_fields(message);
    replace_member(
        &mut message,
        "id",
        Value::from(format!("interrupted-tool-result-{call_id}-{seq}")),
    );
    let mut data = clone_fields(data);
    replace_member(&mut data, "message", Value::Object(message));
    replace_member(&mut projected, "data", Value::Object(data));
    Ok(projected.into_inner())
}

/// `validateReleasedV2Artifact(view, 'current', KNOWN_SESSION_EVENT_TYPES)`
/// after its header check, which the same header already passed in stage 1.
fn validate_released_v2(view: &[Value], cut: u64, is_seeded: bool) -> Checked {
    if cut > MAX_SAFE_INTEGER {
        return reject("format v2 inherited event count must be a non-negative safe integer");
    }
    if usize::try_from(cut).map_or(true, |cut| cut > view.len()) {
        return reject("format v2 inherited event count exceeds its events");
    }
    if !is_seeded && cut != 0 {
        return reject("unseeded format v2 Session has inherited events");
    }
    let mut last_inherited_marker = None;
    for (index, event) in view.iter().enumerate() {
        let label = format!("format v2 event {index}");
        let fields = record(Some(event), &label)?;
        let Some(event_type) = fields.get("type").and_then(Value::as_str) else {
            return reject(&format!("{label} type must be a string"));
        };
        let disposition = lookup(event_type);
        let installed = KNOWN_EVENT_TYPES.contains(&event_type);
        let ignorable = fields.get("ignorable") == Some(&Value::Bool(true));
        let undefined = disposition == Lookup::Absent;
        if undefined && !installed && !ignorable {
            return Err(FinalCheckRefusal::Unsupported {
                message: format!(
                    "format v2 contains unknown event type {} at seq {index}",
                    quote(event_type)
                ),
            });
        }
        let optional: &[&str] = if undefined || V2_SURFACE_TYPES.contains(&event_type) {
            &SURFACE_OPTIONAL
        } else {
            &LOG_OPTIONAL
        };
        exact_keys(fields, &EVENT_REQUIRED, optional, &label).map_err(stage)?;
        let index_value = u64::try_from(index).unwrap_or(u64::MAX);
        if !is_number(fields.get("seq"), index_value) {
            return reject(&format!("{label} is not dense"));
        }
        v3_safe_integer(fields.get("time"), &format!("{label} time")).map_err(stage)?;
        if fields.get("ignorable").is_some_and(|flag| flag != true) {
            return reject(&format!("{label} ignorable must be true when present"));
        }
        if event_type == "session/end-seed" {
            let data = record(
                fields.get("data"),
                &format!("session/end-seed {index} data"),
            )?;
            if data.get("inherited") == Some(&Value::Bool(true)) {
                last_inherited_marker = Some(index_value);
            }
        }
    }
    if is_seeded && last_inherited_marker != Some(cut) {
        return reject("format v2 seeded header disagrees with its last inherited end-seed marker");
    }
    if !is_seeded && last_inherited_marker.is_some() {
        return reject("format v2 unseeded Session contains an inherited end-seed marker");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn artifact(events: Vec<Value>) -> MigratedV2 {
        MigratedV2 {
            header: json!({
                "version": 3,
                "id": "s",
                "createdAt": 0,
                "isSeeded": false,
                "delegationDepth": 0,
            }),
            events,
            inherited_event_count: 0,
        }
    }

    fn check(events: Vec<Value>) -> Result<(), FinalCheckRefusal> {
        check_transformed_artifact(&artifact(events), PathPlatform::Posix)
    }

    fn rejected(message: &str) -> Result<(), FinalCheckRefusal> {
        Err(FinalCheckRefusal::Rejected {
            message: message.to_owned(),
        })
    }

    #[test]
    fn a_later_system_head_refusal_precedes_an_earlier_relationship_refusal() {
        let system = json!({
            "type": "system/message", "seq": 2, "time": 0,
            "data": {"turn": 2, "step": 2, "message": {
                "id": "h", "role": "system",
                "source": {"kind": "plugin", "plugin": "p"}, "content": [],
            }},
            "surfaceOp": "append",
        });
        let events = vec![
            json!({"type": "turn/start", "seq": 0, "time": 0, "data": {"turn": 2}}),
            json!({"type": "step/start", "seq": 1, "time": 0, "data": {"turn": 2, "step": 1}}),
            system,
        ];
        assert_eq!(
            check(events.clone()),
            rejected("system/message does not match an open step")
        );
        let mut matching = events;
        matching[2]["data"]["step"] = json!(1);
        assert_eq!(
            check(matching),
            rejected("turn/start 2 does not open expected turn 1")
        );
    }

    #[test]
    fn delivery_markers_are_checked_at_version_three() {
        let marker = |version: u64| {
            vec![json!({
                "type": "session-log-deepseek/delivery-accepted", "seq": 0, "time": 0,
                "data": {"sessionId": "other", "throughSeq": 0, "sessionFormatVersion": version},
            })]
        };
        assert_eq!(
            check(marker(3)),
            rejected("current-generation delivery marker names the wrong Session")
        );
        assert_eq!(check(marker(2)), Ok(()));
    }

    #[test]
    fn values_outside_the_domain_are_limits() {
        let gap = vec![json!({"type": "turn/start", "seq": 1, "time": 0, "data": {"turn": 1}})];
        assert_eq!(
            check(gap),
            Err(FinalCheckRefusal::NativeSubset("precondition".to_owned()))
        );
        let mut relative = artifact(Vec::new());
        relative.header["cwd"] = json!("relative");
        assert_eq!(
            check_transformed_artifact(&relative, PathPlatform::Posix),
            Err(FinalCheckRefusal::NativeSubset("header".to_owned()))
        );
        assert_eq!(
            FinalCheckRefusal::NativeSubset("header".to_owned()).catalog_message(2),
            None
        );
    }
}
