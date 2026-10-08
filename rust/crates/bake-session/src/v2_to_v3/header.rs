//! The released v2 codec's `decodePhysicalHeader`, then the v2→v3 header
//! migration, as `createDecoder` and `createSessionFormatChain` run them.

use serde_json::{Map, Value};

use super::StageError;
use super::js::{count, exact_keys, js_order};
use crate::{PathPlatform, is_absolute};

const PHYSICAL: &str = "released v2 physical header";
const REQUIRED: [&str; 6] = [
    "type",
    "version",
    "id",
    "createdAt",
    "isSeeded",
    "delegationDepth",
];
const OPTIONAL: [&str; 4] = ["cwd", "parentSession", "origin", "agentPreset"];
const LIMIT: &str = "header-float-lexeme";

/// The source header fields the migration stage reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SourceHeader {
    pub(super) id: String,
    pub(super) is_seeded: bool,
    pub(super) has_parent: bool,
}

/// Decode the physical v2 header and migrate it. Every refusal is a codec
/// error: once the codec admits a header, the migration's own header checks
/// and its stage constructor cannot fail.
pub(super) fn decode(
    header: &Value,
    platform: PathPlatform,
) -> Result<(SourceHeader, Value), StageError> {
    // `snapshotSessionFormatJson`: JSON input can be lossy only through -0.
    if contains_negative_zero(header) {
        return Err(StageError::Invalid(format!(
            "{PHYSICAL} is not lossless JSON"
        )));
    }
    let Value::Object(fields) = js_order(header.clone()) else {
        return Err(StageError::Invalid(format!("{PHYSICAL} must be an object")));
    };
    physical_keys(&fields)?;
    if fields["type"] != "session" || !is_version_two(&fields["version"])? {
        return Err(StageError::Invalid(
            "expected released v2 physical Session header".to_owned(),
        ));
    }
    let Value::String(id) = &fields["id"] else {
        return Err(StageError::Invalid(
            "released v2 header id must be a string".to_owned(),
        ));
    };
    let created_at = count(
        fields.get("createdAt"),
        "released v2 header createdAt",
        LIMIT,
    )?;
    let delegation_depth = count(
        fields.get("delegationDepth"),
        "released v2 header delegationDepth",
        LIMIT,
    )?;
    let Value::Bool(is_seeded) = fields["isSeeded"] else {
        return Err(StageError::Invalid(
            "released v2 header isSeeded must be boolean".to_owned(),
        ));
    };
    for key in ["cwd", "parentSession", "agentPreset"] {
        if fields.get(key).is_some_and(|value| !value.is_string()) {
            return Err(StageError::Invalid(format!(
                "released v2 header {key} must be a string"
            )));
        }
    }
    if fields
        .get("origin")
        .is_some_and(|origin| origin != "subagent")
    {
        return Err(StageError::Invalid(
            "released v2 header origin must be \"subagent\"".to_owned(),
        ));
    }
    // `assertReleasedV2Header` on the logical header adds only the path check.
    if let Some(Value::String(cwd)) = fields.get("cwd")
        && !is_absolute(cwd, platform)
    {
        return Err(StageError::Invalid(
            "format v2 header cwd must be absolute".to_owned(),
        ));
    }
    let mut logical = Map::new();
    logical.insert("version".to_owned(), Value::from(3_u64));
    logical.insert("id".to_owned(), Value::String(id.clone()));
    logical.insert("createdAt".to_owned(), Value::from(created_at));
    for key in ["cwd", "parentSession"] {
        if let Some(value) = fields.get(key) {
            logical.insert(key.to_owned(), value.clone());
        }
    }
    logical.insert("isSeeded".to_owned(), Value::Bool(is_seeded));
    if let Some(origin) = fields.get("origin") {
        logical.insert("origin".to_owned(), origin.clone());
    }
    logical.insert("delegationDepth".to_owned(), Value::from(delegation_depth));
    if let Some(Value::String(preset)) = fields.get("agentPreset") {
        let preset = if preset == "code" { "ptc" } else { preset };
        logical.insert("agentPreset".to_owned(), Value::String(preset.to_owned()));
    }
    let source = SourceHeader {
        id: id.clone(),
        is_seeded,
        has_parent: fields.contains_key("parentSession"),
    };
    Ok((source, Value::Object(logical)))
}

/// The codec's `exactKeys`, whose missing-key message omits "required field".
fn physical_keys(fields: &Map<String, Value>) -> Result<(), StageError> {
    if let Some(key) = REQUIRED.iter().find(|key| !fields.contains_key(**key)) {
        return Err(StageError::Invalid(format!("{PHYSICAL} lacks {key}")));
    }
    exact_keys(fields, &REQUIRED, &OPTIONAL, PHYSICAL)
}

/// `record['version'] !== 2`, undecided for a non-negative `f64`.
fn is_version_two(version: &Value) -> Result<bool, StageError> {
    match version {
        Value::Number(number) if number.as_u64().is_some() => Ok(number.as_u64() == Some(2)),
        Value::Number(number)
            if !number.is_i64() && !number.as_f64().is_some_and(f64::is_sign_negative) =>
        {
            Err(StageError::NativeLimit(LIMIT.to_owned()))
        }
        _ => Ok(false),
    }
}

fn contains_negative_zero(value: &Value) -> bool {
    match value {
        Value::Number(number) => {
            !number.is_i64()
                && !number.is_u64()
                && number
                    .as_f64()
                    .is_some_and(|number| number == 0.0 && number.is_sign_negative())
        }
        Value::Array(items) => items.iter().any(contains_negative_zero),
        Value::Object(fields) => fields.values().any(contains_negative_zero),
        _ => false,
    }
}
