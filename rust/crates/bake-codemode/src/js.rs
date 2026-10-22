//! JavaScript's text forms of numbers and JSON values, for text that Pi
//! renders with `String(number)` and `JSON.stringify`.

use serde_json::Value;

/// `String(value)` for a JavaScript number (ECMAScript `Number::toString`).
pub(crate) fn number_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    // Rust's `{:e}` is the shortest representation that round-trips, as
    // JavaScript requires: `<d>[.<ddd>]e<exponent>`.
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific
        .split_once('e')
        .unwrap_or((scientific.as_str(), "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i64 = exponent.parse().unwrap_or(0);
    let k = digits.len() as i64;
    let n = exponent + 1;
    let mut text = String::new();
    if value < 0.0 {
        text.push('-');
    }
    if k <= n && n <= 21 {
        text.push_str(&digits);
        text.extend(std::iter::repeat_n('0', (n - k) as usize));
    } else if 0 < n && n <= 21 {
        let (whole, fraction) = digits.split_at(n as usize);
        text.push_str(whole);
        text.push('.');
        text.push_str(fraction);
    } else if -6 < n && n <= 0 {
        text.push_str("0.");
        text.extend(std::iter::repeat_n('0', (-n) as usize));
        text.push_str(&digits);
    } else {
        let (first, rest) = digits.split_at(1);
        text.push_str(first);
        if !rest.is_empty() {
            text.push('.');
            text.push_str(rest);
        }
        text.push('e');
        text.push(if n - 1 < 0 { '-' } else { '+' });
        text.push_str(&(n - 1).abs().to_string());
    }
    text
}

/// `JSON.stringify(value)` without indentation: object members in their
/// order and numbers in JavaScript's form (`1.0` is `1`).
pub(crate) fn stringify(value: &Value) -> String {
    let mut text = String::new();
    write_value(&mut text, value);
    text
}

fn write_value(text: &mut String, value: &Value) {
    match value {
        Value::Null => text.push_str("null"),
        Value::Bool(flag) => text.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => match number.as_f64() {
            Some(float) if float.is_finite() => text.push_str(&number_to_string(float)),
            _ => text.push_str("null"),
        },
        Value::String(string) => write_string(text, string),
        Value::Array(items) => {
            text.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }
                write_value(text, item);
            }
            text.push(']');
        }
        Value::Object(members) => {
            text.push('{');
            for (index, (key, item)) in members.iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }
                write_string(text, key);
                text.push(':');
                write_value(text, item);
            }
            text.push('}');
        }
    }
}

/// A JSON string literal as `JSON.stringify` writes it.
pub(crate) fn write_string(text: &mut String, string: &str) {
    text.push('"');
    for character in string.chars() {
        match character {
            '"' => text.push_str("\\\""),
            '\\' => text.push_str("\\\\"),
            '\u{8}' => text.push_str("\\b"),
            '\u{c}' => text.push_str("\\f"),
            '\n' => text.push_str("\\n"),
            '\r' => text.push_str("\\r"),
            '\t' => text.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                text.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => text.push(other),
        }
    }
    text.push('"');
}

/// The length of `text` in UTF-16 code units, JavaScript's `length`.
pub(crate) fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_numbers_as_javascript_does() {
        let cases: [(f64, &str); 14] = [
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-20.0, "-20"),
            (1.5, "1.5"),
            (0.1, "0.1"),
            (123456789.125, "123456789.125"),
            (1e21, "1e+21"),
            (1.5e21, "1.5e+21"),
            (1e20, "100000000000000000000"),
            (0.000001, "0.000001"),
            (1e-7, "1e-7"),
            (1.25e-7, "1.25e-7"),
            (18446744073709551615.0, "18446744073709552000"),
        ];
        for (value, expected) in cases {
            assert_eq!(number_to_string(value), expected, "{value}");
        }
        assert_eq!(number_to_string(f64::NAN), "NaN");
        assert_eq!(number_to_string(f64::NEG_INFINITY), "-Infinity");
    }

    #[test]
    fn stringifies_json_as_javascript_does() {
        let value: Value =
            serde_json::from_str(r#"{"b":1.0,"a":[true,null,"x\u0001\n\u2028"],"c":-0.5}"#)
                .unwrap_or(Value::Null);
        assert_eq!(
            stringify(&value),
            "{\"b\":1,\"a\":[true,null,\"x\\u0001\\n\u{2028}\"],\"c\":-0.5}"
        );
        assert_eq!(utf16_len("a😀"), 3);
    }
}
