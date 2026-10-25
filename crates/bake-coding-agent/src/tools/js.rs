//! The JavaScript behaviors Pi's tools rely on and their model-visible text
//! shows: `Number.prototype.toString`, `toFixed`, `String.prototype.trim`,
//! and `Array.prototype.slice` indices.

/// JavaScript's `ToString(number)` for a finite number: shortest
/// round-trip digits, exponent form outside `1e-7..1e21`.
pub(crate) fn number_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if value == 0.0 {
        return "0".into();
    }
    if value < 0.0 {
        return format!("-{}", number_to_string(-value));
    }
    // Rust's `{:e}` prints the shortest round-trip digits, as JavaScript does.
    let formatted = format!("{value:e}");
    let (mantissa, exponent) = formatted.split_once('e').unwrap_or((&formatted, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let k = digits.len() as i32;
    // The number is 0.digits × 10^n.
    let n = exponent + 1;
    if k <= n && n <= 21 {
        let zeros = usize::try_from(n - k).unwrap_or(0);
        format!("{digits}{}", "0".repeat(zeros))
    } else if 0 < n && n <= 21 {
        let split = usize::try_from(n).unwrap_or(0);
        format!("{}.{}", &digits[..split], &digits[split..])
    } else if -6 < n && n <= 0 {
        let zeros = usize::try_from(-n).unwrap_or(0);
        format!("0.{}{digits}", "0".repeat(zeros))
    } else {
        let sign = if n - 1 < 0 { '-' } else { '+' };
        let exponent = (n - 1).abs();
        if k == 1 {
            format!("{digits}e{sign}{exponent}")
        } else {
            format!("{}.{}e{sign}{exponent}", &digits[..1], &digits[1..])
        }
    }
}

/// JavaScript's `toFixed(digits)` for a finite number below `1e21`: the
/// exact binary value rounded half away from zero.
pub(crate) fn to_fixed(value: f64, digits: usize) -> String {
    if !value.is_finite() || value.abs() >= 1e21 {
        return number_to_string(value);
    }
    let negative = value < 0.0;
    // 1100 fractional digits print any f64 exactly.
    let exact = format!("{:.1100}", value.abs());
    let (whole, fraction) = exact.split_once('.').unwrap_or((&exact, ""));
    let mut kept: Vec<u8> = whole
        .bytes()
        .chain(fraction.bytes().take(digits))
        .map(|byte| byte - b'0')
        .collect();
    let round_up = fraction
        .as_bytes()
        .get(digits)
        .is_some_and(|digit| *digit >= b'5');
    if round_up {
        let mut index = kept.len();
        loop {
            if index == 0 {
                kept.insert(0, 1);
                break;
            }
            index -= 1;
            if kept[index] == 9 {
                kept[index] = 0;
            } else {
                kept[index] += 1;
                break;
            }
        }
    }
    let text: String = kept.iter().map(|digit| char::from(b'0' + digit)).collect();
    let split = text.len() - digits;
    let body = if digits == 0 {
        text
    } else {
        format!("{}.{}", &text[..split], &text[split..])
    };
    let is_zero = body.bytes().all(|byte| byte == b'0' || byte == b'.');
    if negative && !is_zero {
        format!("-{body}")
    } else {
        body
    }
}

/// A number as `JSON.stringify` writes it: integers without a fraction.
pub(crate) fn number_value(value: f64) -> serde_json::Value {
    if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        serde_json::Value::from(value as i64)
    } else {
        serde_json::Number::from_f64(value)
            .map_or(serde_json::Value::Null, serde_json::Value::Number)
    }
}

/// JavaScript's `WhiteSpace` and `LineTerminator` code points, which
/// `trim`, `trimStart`, and `trimEnd` remove.
pub(crate) fn is_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// `String.prototype.trimEnd`.
pub(crate) fn trim_end(text: &str) -> &str {
    text.trim_end_matches(is_whitespace)
}

/// `String.prototype.trim`.
pub(crate) fn trim(text: &str) -> &str {
    text.trim_matches(is_whitespace)
}

/// `ToIntegerOrInfinity` then `Array.prototype.slice`'s clamp of a start or
/// end index to `0..=len`.
pub(crate) fn slice_index(index: f64, len: usize) -> usize {
    let index = index.trunc();
    if index < 0.0 {
        let back = -index;
        if back >= len as f64 {
            0
        } else {
            len - back as usize
        }
    } else if index >= len as f64 {
        len
    } else {
        index as usize
    }
}

/// Whether `value` is an integer an array index can hold.
pub(crate) fn as_index(value: f64) -> Option<usize> {
    (value.fract() == 0.0 && value >= 0.0 && value < usize::MAX as f64).then_some(value as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_print_as_javascript_prints_them() {
        for (value, text) in [
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-3.0, "-3"),
            (2.5, "2.5"),
            (97.5, "97.5"),
            (0.1 + 0.2, "0.30000000000000004"),
            (1e21, "1e+21"),
            (1.5e300, "1.5e+300"),
            (123456789012345680000.0, "123456789012345680000"),
            (1e-7, "1e-7"),
            (1.25e-7, "1.25e-7"),
            (0.000001, "0.000001"),
            (2000.0, "2000"),
        ] {
            assert_eq!(number_to_string(value), text, "{value}");
        }
    }

    #[test]
    fn to_fixed_rounds_the_exact_value_half_up() {
        assert_eq!(to_fixed(2.0, 2), "2.00");
        assert_eq!(to_fixed(1.2, 2), "1.20");
        assert_eq!(to_fixed(1.125, 2), "1.13");
        assert_eq!(to_fixed(1.005, 2), "1.00"); // 1.00499999999999989...
        assert_eq!(to_fixed(9.999, 2), "10.00");
        assert_eq!(to_fixed(2400.0 / 1920.0, 2), "1.25");
        assert_eq!(to_fixed(0.004, 2), "0.00");
        assert_eq!(to_fixed(-0.004, 2), "0.00");
    }

    #[test]
    fn trims_javascript_whitespace() {
        assert_eq!(trim_end("a \u{FEFF}\u{3000}\t"), "a");
        assert_eq!(trim_end("a\u{0085}"), "a\u{0085}");
        assert_eq!(trim("\r\n x \r"), "x");
    }

    #[test]
    fn slice_indices_clamp() {
        assert_eq!(slice_index(2.7, 10), 2);
        assert_eq!(slice_index(-3.0, 10), 7);
        assert_eq!(slice_index(-0.5, 10), 0);
        assert_eq!(slice_index(-30.0, 10), 0);
        assert_eq!(slice_index(1e300, 10), 10);
    }
}
