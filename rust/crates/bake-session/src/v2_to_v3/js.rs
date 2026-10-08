//! JavaScript value rules the migration reads: own-key order, the format's
//! count checks, and `String(number)` for integers.

use serde_json::{Map, Number, Value};

use super::StageError;

pub(super) const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
/// The largest array index plus one: `2^32 - 1` is not an index.
const MAX_ARRAY_INDEX: u64 = u32::MAX as u64 - 1;

/// Reorder every object as `JSON.parse` enumerates it: array-index keys in
/// ascending numeric order, then the other keys in insertion order. Values
/// are unchanged, and serde_json's `preserve_order` already keeps a repeated
/// key at its first position with its last value, as `JSON.parse` does.
pub(super) fn js_order(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(js_order).collect()),
        Value::Object(fields) => {
            let mut indices = Vec::new();
            let mut names = Vec::new();
            for (key, value) in fields {
                let value = js_order(value);
                match array_index(&key) {
                    Some(index) => indices.push((index, key, value)),
                    None => names.push((key, value)),
                }
            }
            indices.sort_by_key(|(index, _, _)| *index);
            let mut ordered = Map::new();
            for (_, key, value) in indices {
                ordered.insert(key, value);
            }
            for (key, value) in names {
                ordered.insert(key, value);
            }
            Value::Object(ordered)
        }
        scalar => scalar,
    }
}

/// The canonical decimal spelling of an integer below `2^32 - 1`.
fn array_index(key: &str) -> Option<u64> {
    let bytes = key.as_bytes();
    let canonical = !bytes.is_empty()
        && bytes.len() <= 10
        && bytes.iter().all(u8::is_ascii_digit)
        && (bytes.len() == 1 || bytes[0] != b'0');
    if !canonical {
        return None;
    }
    key.parse::<u64>()
        .ok()
        .filter(|index| *index <= MAX_ARRAY_INDEX)
}

/// TypeScript's `isSessionFormatJsonObject` check with a `<label> must be an object` error.
pub(super) fn record<'a>(
    value: Option<&'a Value>,
    label: &str,
) -> Result<&'a Map<String, Value>, StageError> {
    match value {
        Some(Value::Object(fields)) => Ok(fields),
        _ => Err(StageError::Invalid(format!("{label} must be an object"))),
    }
}

/// The required-then-unexpected key check shared by `keys` in `payload.ts`
/// and `assertReleasedV2Keys`. `fields` must already be in JavaScript order.
pub(super) fn exact_keys(
    fields: &Map<String, Value>,
    required: &[&str],
    optional: &[&str],
    label: &str,
) -> Result<(), StageError> {
    if let Some(key) = required.iter().find(|key| !fields.contains_key(**key)) {
        return Err(StageError::Invalid(format!(
            "{label} lacks required field {key}"
        )));
    }
    if let Some(key) = fields
        .keys()
        .find(|key| !required.contains(&key.as_str()) && !optional.contains(&key.as_str()))
    {
        return Err(StageError::Invalid(format!(
            "{label} has unexpected field {key}"
        )));
    }
    Ok(())
}

/// `sessionFormatCount`. A non-negative `f64` reports `limit`: JavaScript
/// accepts the integral ones, and this crate does not claim their rounding.
pub(super) fn count(value: Option<&Value>, label: &str, limit: &str) -> Result<u64, StageError> {
    let invalid = || StageError::Invalid(format!("{label} must be a non-negative safe integer"));
    let Some(Value::Number(number)) = value else {
        return Err(invalid());
    };
    if let Some(number) = number.as_u64() {
        return if number <= MAX_SAFE_INTEGER {
            Ok(number)
        } else {
            Err(invalid())
        };
    }
    if number.is_i64() || number.as_f64().is_some_and(f64::is_sign_negative) {
        return Err(invalid());
    }
    Err(StageError::NativeLimit(limit.to_owned()))
}

/// Whether `value` is the JavaScript number `expected` under `===`, for a
/// value an earlier check already admitted as a count.
pub(super) fn is_count(value: Option<&Value>, expected: u64) -> bool {
    value.and_then(Value::as_u64) == Some(expected)
}

/// JavaScript's `String(number)` for an integer serde_json stores exactly.
/// Every `u64` and `i64` is below 1e21, where both runtimes print the
/// nearest double's shortest digits without an exponent. `None` for an `f64`.
pub(super) fn integer_string(number: &Number) -> Option<String> {
    let nearest = if let Some(number) = number.as_u64() {
        number as f64
    } else {
        number.as_i64()? as f64
    };
    Some(format!("{nearest}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn objects_enumerate_array_indices_first() {
        let value: Value = serde_json::from_str(
            r#"{"b":1,"10":2,"a":{"2":0,"x":0,"1":0},"01":3,"4294967295":4,"4294967294":5,"0":6}"#,
        )
        .expect("json");
        let ordered = js_order(value);
        let keys: Vec<&str> = ordered
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            ["0", "10", "4294967294", "b", "a", "01", "4294967295"]
        );
        let nested: Vec<&str> = ordered["a"]
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(nested, ["1", "2", "x"]);
    }

    #[test]
    fn integers_render_as_javascript_numbers() {
        let render = |value: Value| match value {
            Value::Number(number) => integer_string(&number),
            _ => None,
        };
        assert_eq!(
            render(json!(9_007_199_254_740_993_u64)).as_deref(),
            Some("9007199254740992")
        );
        assert_eq!(
            render(json!(u64::MAX)).as_deref(),
            Some("18446744073709552000")
        );
        assert_eq!(
            render(json!(i64::MIN)).as_deref(),
            Some("-9223372036854776000")
        );
        assert_eq!(render(json!(1.5)), None);
    }

    #[test]
    fn counts_refuse_negative_zero_and_defer_other_floats() {
        let negative_zero: Value = serde_json::from_str("-0").expect("json");
        assert_eq!(
            count(Some(&negative_zero), "x", "limit"),
            Err(StageError::Invalid(
                "x must be a non-negative safe integer".into()
            ))
        );
        assert_eq!(
            count(Some(&json!(1.0)), "x", "limit"),
            Err(StageError::NativeLimit("limit".into()))
        );
        assert_eq!(
            count(Some(&json!(MAX_SAFE_INTEGER)), "x", "limit"),
            Ok(MAX_SAFE_INTEGER)
        );
        assert!(count(Some(&json!(MAX_SAFE_INTEGER + 1)), "x", "limit").is_err());
        assert!(count(None, "x", "limit").is_err());
    }
}
