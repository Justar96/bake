//! JavaScript JSON semantics for session lines.
//!
//! Pi writes each session line with `JSON.stringify` and reads it with
//! `JSON.parse`, so a line Bake writes must be the bytes Node would write for
//! the same value. [`js_stringify`] reproduces `JSON.stringify` for a parsed
//! [`Value`]: members keep their order, except that array-index keys such as
//! `"0"` come first in ascending order, as JavaScript orders object keys;
//! numbers print as `Number.prototype.toString` prints them (`1e+21`,
//! `0.000001`, `1.5e-7`, `1` for `1.0`), integers beyond 2^53 lose precision
//! as a JavaScript number does, and non-finite numbers print `null`. Strings
//! escape exactly what `JSON.stringify` escapes.
//!
//! [`js_truthy`] and [`js_to_string`] give the JavaScript truthiness and
//! string conversion the session manager relies on when it reads fields
//! without validating them.

use serde_json::Value;

use crate::session::json_line::{Node, Verbatim};

/// A JSON object; members keep their input order.
pub type JsonObject = serde_json::Map<String, Value>;

/// `JSON.stringify(value)`.
pub fn js_stringify(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value);
    out
}

/// `JSON.stringify(object)` for an object.
pub fn js_stringify_object(object: &JsonObject) -> String {
    let mut out = String::new();
    write_object(&mut out, object);
    out
}

fn write_value(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => out.push_str(&js_number_value(number)),
        Value::String(text) => write_string(out, text),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Object(object) => write_object(out, object),
    }
}

fn write_object(out: &mut String, object: &JsonObject) {
    write_members(out, object, |out, key, value| {
        write_string(out, key);
        out.push(':');
        write_value(out, value);
    });
}

/// Write an object's members in JavaScript's enumeration order: array-index
/// keys first, ascending, then the rest in insertion order. `member` writes
/// one member's key, colon, and value.
fn write_members(
    out: &mut String,
    object: &JsonObject,
    mut member: impl FnMut(&mut String, &str, &Value),
) {
    out.push('{');
    let mut first = true;
    let mut next = |out: &mut String, key: &str, value: &Value| {
        if !first {
            out.push(',');
        }
        first = false;
        member(out, key, value);
    };
    let mut indexed: Vec<(u32, &str, &Value)> = object
        .iter()
        .filter_map(|(key, value)| array_index(key).map(|index| (index, key.as_str(), value)))
        .collect();
    if indexed.is_empty() {
        for (key, value) in object {
            next(out, key, value);
        }
    } else {
        indexed.sort_by_key(|(index, _, _)| *index);
        for (_, key, value) in &indexed {
            next(out, key, value);
        }
        for (key, value) in object {
            if array_index(key).is_none() {
                next(out, key, value);
            }
        }
    }
    out.push('}');
}

/// `JSON.stringify(value)` for a parsed line, writing kept text
/// ([`Verbatim`]) in place of each value Rust holds differently from
/// `JSON.parse` while that value is unchanged. It costs the size of the
/// value plus the number of kept values.
pub fn js_stringify_held(value: &Value, verbatim: &Verbatim) -> String {
    let Some(tree) = &verbatim.0 else {
        return js_stringify(value);
    };
    let mut out = String::new();
    write_held(&mut out, value, &tree.root, &tree.line);
    out
}

/// [`js_stringify_held`] for an object.
pub fn js_stringify_object_held(object: &JsonObject, verbatim: &Verbatim) -> String {
    let Some(tree) = &verbatim.0 else {
        return js_stringify_object(object);
    };
    let mut out = String::new();
    if let Some(text) = tree
        .root
        .kept
        .as_ref()
        .and_then(|kept| kept.text_for_object(object, &tree.line))
    {
        out.push_str(text);
    } else {
        write_held_object(&mut out, object, &tree.root, &tree.line);
    }
    out
}

/// `node` holds the text kept for `value` and below it.
fn write_held(out: &mut String, value: &Value, node: &Node, line: &str) {
    if let Some(text) = node
        .kept
        .as_ref()
        .and_then(|kept| kept.text_for(value, line))
    {
        out.push_str(text);
        return;
    }
    match value {
        Value::Array(items) => {
            let mut held = node.items.iter().peekable();
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                match held.next_if(|(at, _)| *at == index) {
                    Some((_, child)) => write_held(out, item, child, line),
                    None => write_value(out, item),
                }
            }
            out.push(']');
        }
        Value::Object(object) => write_held_object(out, object, node, line),
        _ => write_value(out, value),
    }
}

fn write_held_object(out: &mut String, object: &JsonObject, node: &Node, line: &str) {
    write_members(out, object, |out, key, value| {
        match node.key_texts.get(key) {
            Some(text) => out.push_str(text),
            None => write_string(out, key),
        }
        out.push(':');
        match node.members.get(key) {
            Some(child) => write_held(out, value, child, line),
            None => write_value(out, value),
        }
    });
}

/// The array index a key names: a canonical decimal below 2^32 - 1.
fn array_index(key: &str) -> Option<u32> {
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes.len() > 10 || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if bytes.len() > 1 && bytes.first() == Some(&b'0') {
        return None;
    }
    let value: u64 = key.parse().ok()?;
    if value < u64::from(u32::MAX) {
        u32::try_from(value).ok()
    } else {
        None
    }
}

pub(crate) fn write_string(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if u32::from(ch) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", u32::from(ch)));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

const MAX_SAFE_INTEGER: u64 = 1 << 53;

fn js_number_value(number: &serde_json::Number) -> String {
    if let Some(value) = number.as_u64() {
        if value <= MAX_SAFE_INTEGER {
            return value.to_string();
        }
    } else if let Some(value) = number.as_i64()
        && value.unsigned_abs() <= MAX_SAFE_INTEGER
    {
        return value.to_string();
    }
    // Larger integers and every float are JavaScript numbers: doubles.
    js_number_to_string(number.as_f64().unwrap_or(f64::NAN))
}

/// `JSON.stringify` of a JavaScript number: `Number.prototype.toString`, with
/// `null` for NaN and the infinities.
pub fn js_number_to_string(value: f64) -> String {
    if !value.is_finite() {
        return "null".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    // Rust's `{:e}` prints the shortest digits that round-trip, as
    // ECMAScript's Number::toString requires; only the layout differs.
    let formatted = format!("{:e}", value.abs());
    let (mantissa, exponent) = formatted
        .split_once('e')
        .unwrap_or((formatted.as_str(), "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let k = i32::try_from(digits.len()).unwrap_or(i32::MAX);
    let n = exponent + 1;
    let mut out = String::new();
    if value < 0.0 {
        out.push('-');
    }
    if k <= n && n <= 21 {
        out.push_str(&digits);
        for _ in 0..(n - k) {
            out.push('0');
        }
    } else if 0 < n && n <= 21 {
        let split = usize::try_from(n).unwrap_or(0);
        out.push_str(digits.get(..split).unwrap_or(""));
        out.push('.');
        out.push_str(digits.get(split..).unwrap_or(""));
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        for _ in 0..(-n) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        out.push_str(digits.get(..1).unwrap_or(""));
        if k > 1 {
            out.push('.');
            out.push_str(digits.get(1..).unwrap_or(""));
        }
        out.push('e');
        out.push(if n - 1 < 0 { '-' } else { '+' });
        out.push_str(&(n - 1).abs().to_string());
    }
    out
}

/// JavaScript truthiness of a member; a missing member is `undefined`.
pub fn js_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}

/// JavaScript `String(value)` as `Array.prototype.join` applies it to a
/// member: `undefined` and `null` become the empty string.
pub fn js_to_string(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::Bool(flag)) => flag.to_string(),
        Some(Value::Number(number)) => js_number_value(number),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| js_to_string(Some(item)))
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".to_owned(),
    }
}

/// JavaScript `Number(value)` for a stored member; `undefined` (a missing
/// member) is `NaN`. Arrays convert through their `join(",")` string, as
/// JavaScript converts them.
pub fn js_to_number(value: Option<&Value>) -> f64 {
    match value {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(flag)) => f64::from(u8::from(*flag)),
        Some(Value::Number(number)) => number.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(text)) => js_string_to_number(text),
        Some(array @ Value::Array(_)) => js_string_to_number(&js_to_string(Some(array))),
        Some(Value::Object(_)) => f64::NAN,
    }
}

/// ECMAScript `StringToNumber`: trimmed, empty is 0, `Infinity` with an
/// optional sign, `0x`/`0o`/`0b` integers without a sign, or a decimal
/// literal; anything else is `NaN`.
pub fn js_string_to_number(text: &str) -> f64 {
    let text = js_trim(text);
    if text.is_empty() {
        return 0.0;
    }
    let (sign, unsigned) = match text.as_bytes().first() {
        Some(b'-') => (-1.0, text.get(1..).unwrap_or("")),
        Some(b'+') => (1.0, text.get(1..).unwrap_or("")),
        _ => (1.0, text),
    };
    if unsigned == "Infinity" {
        return sign * f64::INFINITY;
    }
    let radix = match text.get(..2) {
        Some("0x" | "0X") => Some(16),
        Some("0o" | "0O") => Some(8),
        Some("0b" | "0B") => Some(2),
        _ => None,
    };
    if let Some(radix) = radix {
        let digits = text.get(2..).unwrap_or("");
        if digits.is_empty() || !digits.chars().all(|ch| ch.is_digit(radix)) {
            return f64::NAN;
        }
        return digits.chars().fold(0.0, |total, ch| {
            total * f64::from(radix) + f64::from(ch.to_digit(radix).unwrap_or(0))
        });
    }
    if !is_decimal_literal(unsigned) {
        return f64::NAN;
    }
    unsigned
        .parse::<f64>()
        .map_or(f64::NAN, |value| sign * value)
}

/// `StrUnsignedDecimalLiteral` without `Infinity`: digits with an optional
/// fraction (either side may be empty, not both) and exponent.
fn is_decimal_literal(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    let digits = |index: &mut usize| {
        let start = *index;
        while bytes.get(*index).is_some_and(u8::is_ascii_digit) {
            *index += 1;
        }
        *index - start
    };
    let mut count = digits(&mut index);
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        count += digits(&mut index);
    }
    if count == 0 {
        return false;
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        if digits(&mut index) == 0 {
            return false;
        }
    }
    index == bytes.len()
}

/// Whether `ch` is JavaScript whitespace for `String.prototype.trim`: Unicode
/// `White_Space` without U+0085, plus U+FEFF.
pub fn is_js_whitespace(ch: char) -> bool {
    (ch.is_whitespace() && ch != '\u{85}') || ch == '\u{feff}'
}

/// `String.prototype.trim`.
pub fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// A member as a string, when it is one.
pub fn str_member<'a>(object: &'a JsonObject, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_print_as_javascript_does() {
        // Expected strings from Node 26: `[...].map(String)`.
        let cases: [(f64, &str); 14] = [
            (1e21, "1e+21"),
            (1.5e-7, "1.5e-7"),
            (0.000001, "0.000001"),
            (1e-7, "1e-7"),
            (123456789012345680000.0, "123456789012345680000"),
            (1.5e300, "1.5e+300"),
            (-0.0, "0"),
            (0.1 + 0.2, "0.30000000000000004"),
            (5e-324, "5e-324"),
            (100.0, "100"),
            (1.0, "1"),
            (-2.5, "-2.5"),
            (0.0000015, "0.0000015"),
            (f64::NAN, "null"),
        ];
        for (value, expected) in cases {
            assert_eq!(js_number_to_string(value), expected, "{value}");
        }
    }

    #[test]
    fn integers_beyond_two_to_the_53_become_doubles() {
        let value: Value = serde_json::from_str(
            "[9007199254740993,12345678901234567890,-9007199254740993,1.0,1e2]",
        )
        .unwrap_or(Value::Null);
        assert_eq!(
            js_stringify(&value),
            "[9007199254740992,12345678901234567000,-9007199254740992,1,100]"
        );
    }

    #[test]
    fn strings_escape_as_json_stringify_does() {
        let value = json!("\"\\\u{8}\u{c}\n\r\t\u{0}\u{1f}\u{7f}\u{2028}é😀/");
        assert_eq!(
            js_stringify(&value),
            "\"\\\"\\\\\\b\\f\\n\\r\\t\\u0000\\u001f\u{7f}\u{2028}é😀/\""
        );
    }

    #[test]
    fn array_index_keys_come_first() {
        let value: Value = serde_json::from_str(
            r#"{"b":1,"10":2,"a":3,"2":4,"01":5,"4294967295":6,"4294967294":7}"#,
        )
        .unwrap_or(Value::Null);
        // Node: JSON.stringify(JSON.parse(...)).
        assert_eq!(
            js_stringify(&value),
            r#"{"2":4,"10":2,"4294967294":7,"b":1,"a":3,"01":5,"4294967295":6}"#
        );
    }

    #[test]
    fn duplicate_keys_keep_the_first_position_and_last_value() {
        let value: Value = serde_json::from_str(r#"{"a":1,"b":2,"a":3}"#).unwrap_or(Value::Null);
        assert_eq!(js_stringify(&value), r#"{"a":3,"b":2}"#);
    }

    #[test]
    fn numbers_convert_as_javascript_number_does() {
        // Node: [...].map(Number).
        let cases: [(Value, f64); 14] = [
            (json!(" 3 "), 3.0),
            (json!("3."), 3.0),
            (json!(".5"), 0.5),
            (json!("+1e3"), 1000.0),
            (json!("0x10"), 16.0),
            (json!("0b11"), 3.0),
            (json!("-Infinity"), f64::NEG_INFINITY),
            (json!(""), 0.0),
            (json!([3]), 3.0),
            (json!([[" 2 "]]), 2.0),
            (json!([]), 0.0),
            (json!(null), 0.0),
            (json!(true), 1.0),
            (json!(2.5), 2.5),
        ];
        for (value, expected) in cases {
            assert_eq!(js_to_number(Some(&value)), expected, "{value}");
        }
        for value in [
            json!("inf"),
            json!("infinity"),
            json!("nan"),
            json!("-0x10"),
            json!("1_000"),
            json!("."),
            json!("1e"),
            json!([1, 2]),
            json!({}),
        ] {
            assert!(js_to_number(Some(&value)).is_nan(), "{value}");
        }
        assert!(js_to_number(None).is_nan());
    }

    #[test]
    fn truthiness_and_string_conversion() {
        assert!(!js_truthy(None));
        assert!(!js_truthy(Some(&json!(""))));
        assert!(!js_truthy(Some(&json!(0))));
        assert!(js_truthy(Some(&json!("x"))));
        assert!(js_truthy(Some(&json!([]))));
        assert_eq!(js_to_string(Some(&json!([1, null, "a"]))), "1,,a");
        assert_eq!(js_to_string(Some(&json!({}))), "[object Object]");
        assert_eq!(js_trim("\u{feff} a \u{3000}"), "a");
        assert_eq!(js_trim("\u{85}a"), "\u{85}a");
    }
}
