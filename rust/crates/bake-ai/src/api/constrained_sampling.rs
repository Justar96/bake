//! JSON-schema strict sampling for tool declarations.
//!
//! Ported from Pi `packages/ai/src/api/constrained-sampling.ts` (v1.1.0):
//! `makeStrictJsonSchema`, `resolveJsonSchemaStrictSampling`, and
//! `getJsonSchemaToolParameters`. Grammar-constrained (custom) tools are not
//! ported; those tools are sent as plain function tools.

use serde_json::Value;

use crate::types::{ConstrainedSampling, StrictPreference, Tool};

/// Whether a provider's strict mode rejects a keyword with a value.
pub type UnsupportedKeywordCheck = fn(&str, &Value) -> bool;

const UNSUPPORTED_STRICT_SCHEMA_KEYS: &[&str] = &[
    "$ref",
    "$defs",
    "definitions",
    "allOf",
    "oneOf",
    "patternProperties",
    "dependentSchemas",
    "dependencies",
    "unevaluatedProperties",
    "propertyNames",
    "contains",
    "prefixItems",
    "not",
    "if",
    "then",
    "else",
];

struct Unsupported(String);

fn is_structured(schema: &Value) -> bool {
    let Some(object) = schema.as_object() else {
        return false;
    };
    let types: Vec<&str> = match object.get("type") {
        Some(Value::String(kind)) => vec![kind.as_str()],
        Some(Value::Array(kinds)) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    types.contains(&"object")
        || types.contains(&"array")
        || object.contains_key("properties")
        || object.contains_key("items")
}

fn allows_null(schema: &Value) -> bool {
    let Some(object) = schema.as_object() else {
        return false;
    };
    match object.get("type") {
        Some(Value::String(kind)) if kind == "null" => return true,
        Some(Value::Array(kinds)) if kinds.iter().any(|kind| kind == "null") => return true,
        _ => {}
    }
    if object.get("const") == Some(&Value::Null)
        || object
            .get("enum")
            .and_then(Value::as_array)
            .is_some_and(|values| values.contains(&Value::Null))
    {
        return true;
    }
    object
        .get("anyOf")
        .and_then(Value::as_array)
        .is_some_and(|variants| variants.iter().any(allows_null))
}

fn make_node_strict(
    schema: &mut Value,
    check: Option<UnsupportedKeywordCheck>,
) -> Result<(), Unsupported> {
    let Some(object) = schema.as_object_mut() else {
        return Err(Unsupported("boolean schemas are unsupported".into()));
    };
    for key in UNSUPPORTED_STRICT_SCHEMA_KEYS {
        if object.contains_key(*key) {
            return Err(Unsupported(format!("{key} schemas are unsupported")));
        }
    }
    if let Some(check) = check {
        for (key, value) in object.iter() {
            if check(key, value) {
                return Err(Unsupported(format!("{key}: {value} is unsupported")));
            }
        }
    }
    if let Some(any_of) = object.get_mut("anyOf") {
        let Some(variants) = any_of
            .as_array_mut()
            .filter(|variants| !variants.is_empty())
        else {
            return Err(Unsupported("anyOf must contain at least one schema".into()));
        };
        for variant in variants {
            if is_structured(variant) {
                return Err(Unsupported(
                    "object and array unions are unsupported".into(),
                ));
            }
            make_node_strict(variant, check)?;
        }
    }
    if let Some(items) = object.get_mut("items") {
        if items.is_array() {
            return Err(Unsupported("tuple schemas are unsupported".into()));
        }
        make_node_strict(items, check)?;
    }
    let is_object = object.get("type").and_then(Value::as_str) == Some("object");
    if object.contains_key("properties") && !is_object {
        return Err(Unsupported("properties require type object".into()));
    }
    if !is_object {
        return Ok(());
    }
    if object
        .get("additionalProperties")
        .is_some_and(|value| *value != Value::Bool(false))
    {
        return Err(Unsupported(
            "schema-valued or true additionalProperties is unsupported".into(),
        ));
    }
    if object
        .get("properties")
        .is_some_and(|value| !value.is_object())
    {
        return Err(Unsupported("object properties must be a schema map".into()));
    }
    let required: Vec<String> = match object.get("required") {
        None => Vec::new(),
        Some(Value::Array(keys)) if keys.iter().all(Value::is_string) => keys
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Some(_) => return Err(Unsupported("object required must be a string array".into())),
    };
    let names: Vec<String> = object
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default();
    if required.iter().any(|key| !names.contains(key)) {
        return Err(Unsupported("required contains an unknown property".into()));
    }
    if let Some(Value::Object(properties)) = object.get_mut("properties") {
        for (key, property) in properties.iter_mut() {
            make_node_strict(property, check)?;
            if !required.contains(key) && !allows_null(property) {
                let original = std::mem::take(property);
                *property = serde_json::json!({ "anyOf": [original, { "type": "null" }] });
            }
        }
    }
    object.insert(
        "required".into(),
        Value::Array(names.into_iter().map(Value::String).collect()),
    );
    object.insert("additionalProperties".into(), Value::Bool(false));
    Ok(())
}

/// The strict subset of a tool schema, or why it cannot be made strict.
pub fn make_strict_json_schema(
    schema: &Value,
    check: Option<UnsupportedKeywordCheck>,
) -> Result<Value, String> {
    let mut cloned = schema.clone();
    if !cloned.is_object() {
        return Err("root schema must have type object".into());
    }
    make_node_strict(&mut cloned, check).map_err(|error| error.0)?;
    if cloned.get("type").and_then(Value::as_str) != Some("object") {
        return Err("root schema must have type object".into());
    }
    Ok(cloned)
}

/// Whether to send `tool` in strict mode. `Ok(None)` sends it non-strict;
/// `Err` fails the request for a tool that requires strict sampling.
pub fn resolve_json_schema_strict_sampling(
    tool: &Tool,
    supports_strict_mode: bool,
    check: Option<UnsupportedKeywordCheck>,
) -> Result<Option<bool>, String> {
    let Some(ConstrainedSampling::JsonSchema { strict }) = &tool.constrained_sampling else {
        return Ok(None);
    };
    if supports_strict_mode {
        return match make_strict_json_schema(&tool.parameters, check) {
            Ok(_) => Ok(Some(true)),
            Err(_) if *strict != StrictPreference::Require => Ok(None),
            Err(reason) => Err(format!(
                "Tool \"{}\" requires JSON-schema constrained sampling, but {reason}.",
                tool.name
            )),
        };
    }
    if *strict == StrictPreference::Require {
        return Err(format!(
            "Tool \"{}\" requires JSON-schema constrained sampling, but strict tools are unsupported.",
            tool.name
        ));
    }
    Ok(None)
}

/// The parameters to send: the strict subset when `strict`.
pub fn get_json_schema_tool_parameters(tool: &Tool, strict: Option<bool>) -> Result<Value, String> {
    if strict == Some(true) {
        make_strict_json_schema(&tool.parameters, None)
    } else {
        Ok(tool.parameters.clone())
    }
}

#[cfg(test)]
mod tests {
    //! Cases from Pi `packages/ai/test/constrained-sampling.test.ts`.

    use super::*;
    use serde_json::json;

    #[test]
    fn makes_optional_properties_nullable_and_closes_objects() {
        let schema = json!({
            "type": "object",
            "properties": { "path": { "type": "string" }, "limit": { "type": "number" } },
            "required": ["path"]
        });
        assert_eq!(
            make_strict_json_schema(&schema, None).unwrap(),
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "limit": { "anyOf": [{ "type": "number" }, { "type": "null" }] }
                },
                "required": ["path", "limit"],
                "additionalProperties": false
            })
        );
    }

    #[test]
    fn refuses_unsupported_schemas() {
        assert_eq!(
            make_strict_json_schema(&json!({ "type": "object", "oneOf": [] }), None).unwrap_err(),
            "oneOf schemas are unsupported"
        );
        assert_eq!(
            make_strict_json_schema(&json!({ "type": "string" }), None).unwrap_err(),
            "root schema must have type object"
        );
        let tool = Tool {
            name: "t".into(),
            description: "d".into(),
            parameters: json!({ "type": "object", "additionalProperties": true }),
            constrained_sampling: Some(ConstrainedSampling::JsonSchema {
                strict: StrictPreference::Require,
            }),
        };
        assert_eq!(
            resolve_json_schema_strict_sampling(&tool, true, None).unwrap_err(),
            "Tool \"t\" requires JSON-schema constrained sampling, but schema-valued or true additionalProperties is unsupported."
        );
        let prefer = Tool {
            constrained_sampling: Some(ConstrainedSampling::JsonSchema {
                strict: StrictPreference::Prefer,
            }),
            ..tool.clone()
        };
        assert_eq!(
            resolve_json_schema_strict_sampling(&prefer, true, None),
            Ok(None)
        );
        assert_eq!(
            resolve_json_schema_strict_sampling(&tool, false, None).unwrap_err(),
            "Tool \"t\" requires JSON-schema constrained sampling, but strict tools are unsupported."
        );
    }
}
