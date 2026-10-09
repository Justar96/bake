//! A provider token count as JavaScript holds it after `JSON.parse`: a double
//! or a string, which providers may send and the writers log unvalidated.
//!
//! The token-meter folds add and subtract counts with JavaScript's `+` and
//! `-`. [`JsCount::plus`] is `+`: string concatenation when either side is a
//! string, otherwise double addition. [`JsCount::to_number`] is `ToNumber`,
//! which `-` applies to both sides: the double itself, or `StringToNumber` of
//! the string, `NaN` when the string is not a numeric literal. Derived
//! equality is `===`: doubles compare as doubles, so `NaN` differs from
//! itself, and a string never equals a double.

use serde_json::{Number, Value};

use crate::json_text::{is_writer_spelling, json_number_text};

/// One count held by the token-usage or context-pressure state.
#[derive(Debug, Clone, PartialEq)]
pub enum JsCount {
    Number(f64),
    String(String),
}

impl JsCount {
    /// A count that is a number spelled as `JSON.stringify` writes it, or a
    /// string; `None` for any other number spelling, whose double no writer
    /// produces from that text.
    pub(crate) fn from_writer_number(number: &Number) -> Option<Self> {
        is_writer_spelling(number).then(|| Self::Number(number.as_f64().unwrap_or(f64::NAN)))
    }

    /// JavaScript `ToNumber`.
    pub fn to_number(&self) -> f64 {
        match self {
            Self::Number(value) => *value,
            Self::String(text) => string_to_number(text),
        }
    }

    /// JavaScript `self + other`.
    pub fn plus(&self, other: &Self) -> Self {
        match (self, other) {
            (Self::Number(left), Self::Number(right)) => Self::Number(left + right),
            _ => Self::String(format!("{}{}", self.to_js_string(), other.to_js_string())),
        }
    }

    /// The value `JSON.stringify` writes for this count, read back as JSON:
    /// a number, `null` for a non-finite double, or the string.
    pub fn to_json(&self) -> Value {
        match self {
            Self::Number(value) => {
                serde_json::from_str(&json_number_text(*value)).unwrap_or(Value::Null)
            }
            Self::String(text) => Value::String(text.clone()),
        }
    }

    fn to_js_string(&self) -> String {
        match self {
            Self::Number(value) => js_number_string(*value),
            Self::String(text) => text.clone(),
        }
    }
}

/// ECMAScript `Number::toString`.
pub(crate) fn js_number_string(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value.is_infinite() {
        if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned()
    } else {
        json_number_text(value)
    }
}

/// `StrWhiteSpaceChar`: ECMAScript WhiteSpace and LineTerminator.
fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\u{9}' | '\u{a}' | '\u{b}' | '\u{c}' | '\u{d}' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// ECMAScript `StringToNumber`.
fn string_to_number(text: &str) -> f64 {
    let text = text.trim_matches(is_js_space);
    if text.is_empty() {
        return 0.0;
    }
    for (prefix, bits) in [("0x", 4), ("0o", 3), ("0b", 1)] {
        if let Some(digits) = text
            .get(..2)
            .filter(|head| head.eq_ignore_ascii_case(prefix))
            .and_then(|_| text.get(2..))
            .filter(|digits| !digits.is_empty())
        {
            return power_of_two_radix(digits, bits).unwrap_or(f64::NAN);
        }
    }
    let (sign, unsigned) = match text.as_bytes()[0] {
        b'+' => (1.0, &text[1..]),
        b'-' => (-1.0, &text[1..]),
        _ => (1.0, text),
    };
    if unsigned == "Infinity" {
        return sign * f64::INFINITY;
    }
    if !is_unsigned_decimal(unsigned) {
        return f64::NAN;
    }
    // Rust parses a validated decimal literal to the nearest double.
    unsigned
        .parse::<f64>()
        .map_or(f64::NAN, |value| sign * value)
}

/// The double nearest the value of `digits` in radix `2^bits`, ties to even,
/// as the specification rounds the literal's mathematical value; `None` when
/// a character is not a digit of that radix.
fn power_of_two_radix(digits: &str, bits: u32) -> Option<f64> {
    let mut binary = Vec::with_capacity(digits.len() * bits as usize);
    for c in digits.chars() {
        let digit = c.to_digit(1 << bits)?;
        binary.extend((0..bits).rev().map(|bit| digit >> bit & 1 == 1));
    }
    let Some(first) = binary.iter().position(|bit| *bit) else {
        return Some(0.0);
    };
    let significant = &binary[first..];
    // 53 bits are exact; the rest decide rounding.
    let kept = significant.len().min(53);
    let mut mantissa = significant[..kept]
        .iter()
        .fold(0_u64, |value, bit| value << 1 | u64::from(*bit));
    let rest = &significant[kept..];
    if rest.first() == Some(&true) && (rest[1..].contains(&true) || mantissa & 1 == 1) {
        mantissa += 1;
    }
    // The scale past 1024 already overflows; `powi` keeps it exact below.
    let scale = i32::try_from(rest.len()).unwrap_or(i32::MAX).min(1100);
    Some(mantissa as f64 * 2_f64.powi(scale))
}

/// `StrUnsignedDecimalLiteral` without `Infinity`: digits with an optional
/// fraction, or a fraction alone, then an optional exponent.
fn is_unsigned_decimal(text: &str) -> bool {
    let bytes = text.as_bytes();
    let digits = |from: usize| {
        bytes[from..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count()
    };
    let whole = digits(0);
    let mut at = whole;
    let mut fraction = 0;
    if bytes.get(at) == Some(&b'.') {
        fraction = digits(at + 1);
        at += 1 + fraction;
    }
    if whole == 0 && fraction == 0 {
        return false;
    }
    if matches!(bytes.get(at), Some(b'e' | b'E')) {
        at += 1;
        if matches!(bytes.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        let exponent = digits(at);
        if exponent == 0 {
            return false;
        }
        at += exponent;
    }
    at == bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_convert_as_javascript_number_does() {
        // `Number(text)` in Node for each text.
        for (text, expected) in [
            ("5", 5.0),
            ("05", 5.0),
            (" \u{feff}\n7\u{3000}", 7.0),
            ("", 0.0),
            ("  ", 0.0),
            ("1.", 1.0),
            (".5", 0.5),
            ("-.5e1", -5.0),
            ("+1E+2", 100.0),
            ("0x10", 16.0),
            ("0B11", 3.0),
            ("0o17", 15.0),
            ("-Infinity", f64::NEG_INFINITY),
            ("1e400", f64::INFINITY),
            ("9007199254740993", 9_007_199_254_740_992.0),
        ] {
            assert_eq!(string_to_number(text), expected, "{text:?}");
        }
        for text in [
            "abc", "0abc", "1e", ".", "inf", "NaN", "infinity", "-0x10", "0x", "1_0", "\u{85}1",
            "0x1g", "0b2",
        ] {
            assert!(string_to_number(text).is_nan(), "{text:?}");
        }
        // `Number(text)` in Node: 2^53 + 1 and 2^53 + 3 round to even, a
        // sticky bit rounds up, and 2^1024 overflows.
        for (text, expected) in [
            ("0x20000000000001", 9_007_199_254_740_992.0),
            ("0x20000000000003", 9_007_199_254_740_996.0),
            (
                "0b100000000000000000000000000000000000000000000000000001001",
                72_057_594_037_927_950.0,
            ),
            (
                "0xffffffffffffffffffffffffffffffffff",
                8.711_228_593_176_025e40,
            ),
            (&format!("0x1{}", "0".repeat(256)), f64::INFINITY),
            ("0x000", 0.0),
        ] {
            assert_eq!(string_to_number(text), expected, "{text}");
        }
    }

    #[test]
    fn plus_concatenates_with_a_string() {
        let five = JsCount::String("5".to_owned());
        assert_eq!(
            JsCount::Number(0.0).plus(&five),
            JsCount::String("05".to_owned())
        );
        assert_eq!(
            JsCount::Number(f64::NAN).plus(&five),
            JsCount::String("NaN5".to_owned())
        );
        assert_eq!(
            JsCount::Number(1.5).plus(&JsCount::Number(2.0)),
            JsCount::Number(3.5)
        );
        assert_ne!(JsCount::Number(f64::NAN), JsCount::Number(f64::NAN));
        assert_eq!(JsCount::Number(f64::NAN).to_json(), Value::Null);
    }
}
