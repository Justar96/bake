//! JavaScript `Date` behaviour the session format depends on.
//!
//! Entry timestamps are `new Date().toISOString()` strings, and Pi turns them
//! back into milliseconds with `new Date(timestamp).getTime()`. Parsing
//! accepts the ISO 8601 forms V8 accepts (`YYYY`, `YYYY-MM`, `YYYY-MM-DD`,
//! six-digit signed years, a `T`, `t`, or space before the time, minutes
//! with optional seconds and fraction, and `Z` or a `±HH:mm` or `±HHmm`
//! offset), including V8's day overflow (`2025-02-30` is 2 March). Two
//! deviations: a date-time without an offset is read as UTC where V8 reads
//! local time, and the non-ISO legacy formats V8 falls back to yield `None`
//! (`NaN`). Pi writes neither.

use serde_json::Value;

/// ECMAScript's time-value limit, ±8.64e15 ms.
const MAX_TIME_MS: i64 = 8_640_000_000_000_000;

/// `new Date().toISOString()`.
pub fn now_iso() -> String {
    format_iso(bake_ai::now_ms())
}

/// `new Date(ms).toISOString()`, with milliseconds and a `Z`.
pub fn format_iso(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let in_day = ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let hour = in_day / 3_600_000;
    let minute = in_day / 60_000 % 60;
    let second = in_day / 1000 % 60;
    let milli = in_day % 1000;
    let year = if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else if year < 0 {
        format!("-{:06}", year.unsigned_abs())
    } else {
        format!("+{year:06}")
    };
    format!("{year}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z")
}

/// `new Date(value).getTime()` for a member read without validation, `None`
/// for `NaN`. A missing member is `undefined` (`NaN`); `null` is 0.
pub fn js_date_ms(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Null => Some(0),
        Value::Bool(flag) => Some(i64::from(*flag)),
        Value::Number(number) => time_clip(number.as_f64()?),
        Value::String(text) => parse_date_ms(text),
        Value::Array(_) | Value::Object(_) => None,
    }
}

/// ECMAScript `TimeClip`.
pub fn time_clip(ms: f64) -> Option<i64> {
    if !ms.is_finite() || ms.abs() > MAX_TIME_MS as f64 {
        return None;
    }
    // Truncation toward zero, as ToIntegerOrInfinity does; in range above.
    Some(ms.trunc() as i64)
}

/// `Date.parse` for ISO 8601 date and date-time strings.
pub fn parse_date_ms(text: &str) -> Option<i64> {
    let mut cursor = Cursor {
        bytes: text.as_bytes(),
        at: 0,
    };
    let year = match cursor.peek() {
        Some(sign @ (b'+' | b'-')) => {
            cursor.at += 1;
            let digits = cursor.digits(6)?;
            if sign == b'-' && digits == 0 {
                return None;
            }
            if sign == b'-' { -digits } else { digits }
        }
        _ => cursor.digits(4)?,
    };
    let mut month = 1;
    let mut day = 1;
    if cursor.eat(b'-') {
        month = cursor.digits(2)?;
        if cursor.eat(b'-') {
            day = cursor.digits(2)?;
        }
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut time_ms = 0;
    let mut offset_ms = 0;
    if matches!(cursor.peek(), Some(b'T' | b't' | b' ')) {
        cursor.at += 1;
        let hour = cursor.digits(2)?;
        if !cursor.eat(b':') {
            return None;
        }
        let minute = cursor.digits(2)?;
        let mut second = 0;
        let mut milli = 0;
        if cursor.eat(b':') {
            second = cursor.digits(2)?;
            if cursor.eat(b'.') {
                let start = cursor.at;
                while cursor.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    cursor.at += 1;
                }
                let fraction = text.get(start..cursor.at)?;
                if fraction.is_empty() {
                    return None;
                }
                let mut padded: String = fraction.chars().take(3).collect();
                while padded.len() < 3 {
                    padded.push('0');
                }
                milli = padded.parse::<i64>().ok()?;
            }
        }
        if hour > 24 || minute > 59 || second > 59 {
            return None;
        }
        if hour == 24 && (minute != 0 || second != 0 || milli != 0) {
            return None;
        }
        time_ms = ((hour * 60 + minute) * 60 + second) * 1000 + milli;
        match cursor.peek() {
            Some(b'Z' | b'z') => cursor.at += 1,
            Some(sign @ (b'+' | b'-')) => {
                cursor.at += 1;
                let hours = cursor.digits(2)?;
                cursor.eat(b':');
                let minutes = cursor.digits(2)?;
                if hours > 23 || minutes > 59 {
                    return None;
                }
                let offset = (hours * 60 + minutes) * 60_000;
                offset_ms = if sign == b'+' { offset } else { -offset };
            }
            // V8 reads this as local time; Bake reads it as UTC.
            _ => {}
        }
    }
    if cursor.at != cursor.bytes.len() {
        return None;
    }
    let days = days_from_civil(year, month, 1) + (day - 1);
    let ms = days
        .checked_mul(86_400_000)?
        .checked_add(time_ms)?
        .checked_sub(offset_ms)?;
    (ms.abs() <= MAX_TIME_MS).then_some(ms)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    /// Exactly `count` ASCII digits.
    fn digits(&mut self, count: usize) -> Option<i64> {
        let slice = self.bytes.get(self.at..self.at + count)?;
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        self.at += count;
        Some(
            slice
                .iter()
                .fold(0, |total, byte| total * 10 + i64::from(byte - b'0')),
        )
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The proleptic Gregorian date of a day count (`civil_from_days`).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// A file timestamp in fractional Unix milliseconds, Node's `mtimeMs`.
pub fn system_time_ms(time: std::time::SystemTime) -> f64 {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_secs_f64() * 1000.0,
        Err(before) => -(before.duration().as_secs_f64() * 1000.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_as_to_iso_string() {
        assert_eq!(format_iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_iso(1_735_689_600_123), "2025-01-01T00:00:00.123Z");
        assert_eq!(format_iso(-1), "1969-12-31T23:59:59.999Z");
        assert_eq!(
            format_iso(253_402_300_800_000),
            "+010000-01-01T00:00:00.000Z"
        );
        assert_eq!(
            format_iso(-62_198_755_200_000),
            "-000001-01-01T00:00:00.000Z"
        );
    }

    #[test]
    fn parses_as_v8_does() {
        // Expected values from Node 26: `new Date(s).getTime()`.
        let cases: [(&str, Option<i64>); 17] = [
            ("2025-02-30", Some(1_740_873_600_000)),
            ("2025-02-30T00:00:00Z", Some(1_740_873_600_000)),
            ("2025-01-01T24:00:00Z", Some(1_735_776_000_000)),
            ("2025-01-01T00:00:00.1Z", Some(1_735_689_600_100)),
            ("2025-01-01T00:00:00.123456Z", Some(1_735_689_600_123)),
            ("2025-01-01T00:00Z", Some(1_735_689_600_000)),
            ("2025-01-01T00:00:00+01:00", Some(1_735_686_000_000)),
            ("2025", Some(1_735_689_600_000)),
            ("2025-01", Some(1_735_689_600_000)),
            ("+002025-01-01T00:00:00Z", Some(1_735_689_600_000)),
            ("2025-01-01 00:00:00Z", Some(1_735_689_600_000)),
            ("2025-01-01T00:00:60Z", None),
            ("2025-13-01", None),
            (" 2025-01-01T00:00:00Z", None),
            ("2025-01-01T00:00:00z", Some(1_735_689_600_000)),
            ("2025-01-01T00:00:00+0100", Some(1_735_686_000_000)),
            ("2025-01-01T00:00:00.Z", None),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_date_ms(text), expected, "{text}");
        }
        assert_eq!(parse_date_ms("+275760-09-13T00:00:00.001Z"), None);
        assert_eq!(parse_date_ms("-000000-01-01"), None);
        assert_eq!(parse_date_ms(""), None);
        assert_eq!(parse_date_ms("2025-01-01T00:00:00.000Zjunk"), None);
    }

    #[test]
    fn round_trips_every_millisecond_offset_of_a_day_sample() {
        for ms in [
            0,
            1,
            86_399_999,
            951_782_400_000,
            4_102_444_799_999,
            -86_400_001,
        ] {
            assert_eq!(parse_date_ms(&format_iso(ms)), Some(ms));
        }
    }

    #[test]
    fn date_of_untyped_members() {
        assert_eq!(js_date_ms(None), None);
        assert_eq!(js_date_ms(Some(&Value::Null)), Some(0));
        assert_eq!(js_date_ms(Some(&serde_json::json!(12.9))), Some(12));
        assert_eq!(js_date_ms(Some(&serde_json::json!(9e15))), None);
        assert_eq!(js_date_ms(Some(&serde_json::json!({}))), None);
    }
}
