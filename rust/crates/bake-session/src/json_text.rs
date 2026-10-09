//! `JSON.stringify` text for parsed JSON values, as JavaScript writes it.
//!
//! [`json_number_text`] is ECMAScript `Number::toString` for a finite double,
//! which `JSON.stringify` writes for a number, and `null` otherwise: the
//! shortest digits that read back as the same double, laid out in plain
//! decimal for decimal exponents from -7 through 20 and in exponent form,
//! with an explicit sign, outside them. Both zeros print `0`. Rust's `{:e}`
//! formatting supplies the shortest round-trip digits, closest to the value.
//!
//! [`json_text`] writes a whole value: object members in JavaScript
//! enumeration order, array-index keys first, and strings escaped as
//! `JSON.stringify` escapes them, which is the escaping serde_json applies to
//! a Rust string.
//!
//! [`is_writer_spelling`] holds when serde_json stores a number as it stores
//! the text `JSON.stringify` writes for its value. A released writer produces
//! only such numbers, so a `Value` equality between two of them is a
//! JavaScript `===` between their values.

use serde_json::{Number, Value};

use crate::request::js_members;

/// `JSON.stringify` of a double: `Number::toString` when finite, else `null`.
pub fn json_number_text(value: f64) -> String {
    if !value.is_finite() {
        return "null".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    let sign = if value < 0.0 { "-" } else { "" };
    // `{:e}` writes the shortest round-trip digits as `d[.ddd]e[-]x`.
    let exponential = format!("{:e}", value.abs());
    let (mantissa, exponent) = exponential
        .split_once('e')
        .expect("`{:e}` writes an exponent");
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i32 = exponent.parse().expect("`{:e}` writes an integer exponent");
    // ECMAScript's n: the value is 0.digits × 10^n.
    let n = exponent + 1;
    let k = digits.len() as i32;
    let text = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        let (whole, fraction) = digits.split_at(n as usize);
        format!("{whole}.{fraction}")
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let exponent = n - 1;
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        let (first, rest) = digits.split_at(1);
        let point = if rest.is_empty() { "" } else { "." };
        format!(
            "{first}{point}{rest}e{exponent_sign}{}",
            exponent.unsigned_abs()
        )
    };
    format!("{sign}{text}")
}

/// Whether serde_json stores `number` exactly as it stores the
/// `JSON.stringify` text of its value. That excludes -0, an integral value
/// spelled with a fraction or exponent below 2^64, such as `1.0` or `1e5`, and
/// an integer whose digits do not round-trip, such as `9007199254740993`.
/// Spellings that differ only in digits the value fixes, such as `0.10` and
/// `0.1`, are stored alike and are indistinguishable here.
pub(crate) fn is_writer_spelling(number: &Number) -> bool {
    let Some(value) = number.as_f64() else {
        return false;
    };
    if value == 0.0 && value.is_sign_negative() {
        return false;
    }
    serde_json::from_str::<Number>(&json_number_text(value)).is_ok_and(|text| text == *number)
}

/// `JSON.stringify(value)` for a parsed value. A number prints from the
/// double `JSON.parse` would read, so any spelling prints as JavaScript prints
/// its value. The recursion follows the value's nesting, which serde_json's
/// parser bounds.
pub fn json_text(value: &Value) -> String {
    let mut text = String::new();
    write_json(value, &mut text);
    text
}

fn write_json(value: &Value, text: &mut String) {
    match value {
        Value::Number(number) => {
            text.push_str(&json_number_text(number.as_f64().unwrap_or(f64::NAN)));
        }
        Value::Array(items) => {
            text.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }
                write_json(item, text);
            }
            text.push(']');
        }
        Value::Object(fields) => {
            text.push('{');
            for (index, (key, item)) in js_members(fields).into_iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }
                write_string(key, text);
                text.push(':');
                write_json(item, text);
            }
            text.push('}');
        }
        Value::String(string) => write_string(string, text),
        Value::Null | Value::Bool(_) => text.push_str(&value.to_string()),
    }
}

fn write_string(string: &str, text: &mut String) {
    text.push_str(&Value::String(string.to_owned()).to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strings_escape_as_json_stringify_does() {
        // `JSON.stringify` output for each string, from the ECMAScript
        // `QuoteJSONString` table.
        for (string, expected) in [
            ("\u{0}\u{1}\u{1f}", r#""\u0000\u0001\u001f""#),
            ("\u{8}\t\n\u{c}\r", r#""\b\t\n\f\r""#),
            ("\"\\/", r#""\"\\/""#),
            ("\u{7f}\u{2028}é😀", "\"\u{7f}\u{2028}é😀\""),
        ] {
            assert_eq!(json_text(&json!(string)), expected, "{string:?}");
        }
    }

    #[test]
    fn members_print_in_javascript_order() {
        let value: Value =
            serde_json::from_str(r#"{"b":1,"2":[true,null],"a":{"1":0.5,"0":-0}}"#).expect("JSON");
        assert_eq!(
            json_text(&value),
            r#"{"2":[true,null],"b":1,"a":{"0":0,"1":0.5}}"#
        );
    }

    #[test]
    fn writer_spellings_are_the_stored_texts_of_their_values() {
        for (text, writer) in [
            ("0", true),
            ("-0", false),
            ("-0.0", false),
            ("0.7", true),
            ("0.70", true),
            ("1", true),
            ("1.0", false),
            ("1e5", false),
            ("1e21", true),
            ("1e20", true),
            ("100000000000000000000", true),
            ("-9223372036854775808", false),
            ("-9223372036854775809", true),
            ("-9223372036854776000", true),
            ("9223372036854776000", true),
            ("9007199254740993", false),
            ("18446744073709551615", false),
            ("18446744073709552000", true),
        ] {
            let number: Number = serde_json::from_str(text).expect("number");
            assert_eq!(is_writer_spelling(&number), writer, "{text}");
        }
    }
}
