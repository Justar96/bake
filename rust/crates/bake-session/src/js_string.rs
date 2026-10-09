//! JavaScript strings, lone surrogates included, held in Rust `String`s.
//!
//! `JSON.parse` returns UTF-16 strings, and a released writer can log one
//! holding a lone surrogate: `boundContextSummary` cuts a goal objective on
//! UTF-16 code units (`packages/llm/llm/src/message.ts:121-124`), and a
//! configured `agents[].sessionId` may hold one. `JSON.stringify` writes it
//! back as a lowercase `\udxxx` escape. A Rust `String`, and so a
//! `serde_json::Value::String` or an object key, cannot hold a lone
//! surrogate, so every string this crate reads from Session JSON uses one
//! spelling of a JavaScript string's code units:
//!
//! - a Unicode scalar value other than U+FDD0 is itself;
//! - U+FDD0 (a noncharacter) is [`ESCAPE`] twice;
//! - a lone surrogate code unit is [`ESCAPE`] and its four lowercase hex
//!   digits, the first of which is `d`;
//! - a high surrogate followed by a low surrogate is the scalar value they
//!   pair into, never two escapes.
//!
//! The spelling cannot collide: reading left to right, a character other
//! than [`ESCAPE`] is one unit or a pair, and [`ESCAPE`] is followed either
//! by [`ESCAPE`] or by `d`, never both, so each spelling decodes to exactly
//! one code-unit sequence. Because pairs are always joined, each code-unit
//! sequence has exactly one spelling, so `String` equality is JavaScript
//! string equality. A string without U+FDD0 or a lone surrogate, which is
//! every string no writer path above produced, is spelled as itself.
//!
//! Byte order of two spellings is not UTF-16 order, a spelling's length is
//! not `.length`, and a substring search can match across an escape: use
//! [`code_units`], [`utf16_len`], and [`cmp_code_units`] for those, and
//! [`concat()`] to join spellings, which pairs a trailing high surrogate with
//! a leading low one as JavaScript's `+` does. [`from_rust`] spells an
//! arbitrary Rust string, such as a path or an argument, and [`to_rust`]
//! writes one for the operating system as Node does, replacing each lone
//! surrogate with U+FFFD.

use std::borrow::Cow;
use std::cmp::Ordering;

/// The character that opens an escape in a JavaScript string's spelling.
pub const ESCAPE: char = '\u{FDD0}';

/// Push one UTF-16 code unit onto `text`, which must not end with a lone
/// high surrogate when `unit` is a low one; use [`push_units`] for a run.
fn push_lone_or_scalar(text: &mut String, unit: u16) {
    match char::from_u32(unit.into()) {
        Some(ESCAPE) => {
            text.push(ESCAPE);
            text.push(ESCAPE);
        }
        Some(character) => text.push(character),
        None => {
            text.push(ESCAPE);
            text.push_str(&format!("{unit:04x}"));
        }
    }
}

/// Push a sequence of UTF-16 code units onto `text`, pairing surrogates
/// that pair, including a low surrogate that pairs with a lone high
/// surrogate `text` already ends with.
pub fn push_units(text: &mut String, units: impl IntoIterator<Item = u16>) {
    let mut units = units.into_iter().peekable();
    if let Some(&low @ 0xDC00..=0xDFFF) = units.peek()
        && let Some(high) = trailing_lone_high(text)
    {
        units.next();
        text.truncate(text.len() - ESCAPE.len_utf8() - 4);
        text.push(pair(high, low));
    }
    while let Some(unit) = units.next() {
        if (0xD800..=0xDBFF).contains(&unit)
            && let Some(&low @ 0xDC00..=0xDFFF) = units.peek()
        {
            units.next();
            text.push(pair(unit, low));
        } else {
            push_lone_or_scalar(text, unit);
        }
    }
}

fn pair(high: u16, low: u16) -> char {
    let code = 0x1_0000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(low) - 0xDC00);
    char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER)
}

/// The lone high surrogate `text` ends with, if any.
fn trailing_lone_high(text: &str) -> Option<u16> {
    let start = text.len().checked_sub(ESCAPE.len_utf8() + 4)?;
    let tail = text.get(start..)?;
    let hex = tail.strip_prefix(ESCAPE)?;
    let unit = u16::from_str_radix(hex, 16).ok()?;
    if !(0xD800..=0xDBFF).contains(&unit) {
        return None;
    }
    // The escape must open a code, not end a doubled U+FDD0: count the
    // escapes before it, which pair up when it opens one.
    let escapes = text[..start]
        .chars()
        .rev()
        .take_while(|&character| character == ESCAPE)
        .count();
    (escapes % 2 == 0).then_some(unit)
}

/// The spelling of `left + right`, as JavaScript's `+` joins two strings.
pub fn concat(left: &str, right: &str) -> String {
    let mut text = left.to_owned();
    push_str(&mut text, right);
    text
}

/// Append the spelling `right` to the spelling `text`, as `+=` does.
pub fn push_str(text: &mut String, right: &str) {
    if right.starts_with(ESCAPE) && trailing_lone_high(text).is_some() {
        push_units(text, code_units(right));
    } else {
        text.push_str(right);
    }
}

/// The UTF-16 code units of a spelling, as JavaScript's `charCodeAt` reads
/// them.
pub fn code_units(text: &str) -> impl Iterator<Item = u16> + '_ {
    let mut chars = text.chars();
    let mut pending: Option<u16> = None;
    std::iter::from_fn(move || {
        if let Some(unit) = pending.take() {
            return Some(unit);
        }
        let character = chars.next()?;
        if character != ESCAPE {
            let mut buffer = [0u16; 2];
            let encoded = character.encode_utf16(&mut buffer);
            if encoded.len() == 2 {
                pending = Some(encoded[1]);
            }
            return Some(encoded[0]);
        }
        let mut lookahead = chars.clone();
        if lookahead.next() == Some(ESCAPE) {
            chars = lookahead;
            return Some(0xFDD0);
        }
        let hex: String = chars.by_ref().take(4).collect();
        Some(u16::from_str_radix(&hex, 16).unwrap_or(0xFFFD))
    })
}

/// A spelling's JavaScript `.length`: its UTF-16 code units.
pub fn utf16_len(text: &str) -> u64 {
    code_units(text).count() as u64
}

/// JavaScript's `<` order of two spellings: by UTF-16 code units.
pub fn cmp_code_units(left: &str, right: &str) -> Ordering {
    code_units(left).cmp(code_units(right))
}

/// The spelling of a Rust string: U+FDD0 doubled, nothing else changed.
pub fn from_rust(text: &str) -> Cow<'_, str> {
    if text.contains(ESCAPE) {
        Cow::Owned(text.replace(ESCAPE, "\u{FDD0}\u{FDD0}"))
    } else {
        Cow::Borrowed(text)
    }
}

/// A spelling as Node writes it as UTF-8: each lone surrogate becomes
/// U+FFFD.
pub fn to_rust(text: &str) -> Cow<'_, str> {
    if !text.contains(ESCAPE) {
        return Cow::Borrowed(text);
    }
    let units: Vec<u16> = code_units(text).collect();
    Cow::Owned(String::from_utf16_lossy(&units))
}

/// `JSON.stringify` of a spelling, appended to `out`: `QuoteJSONString`
/// over its code units, with `\b`, `\t`, `\n`, `\f`, `\r`, `\"`, and `\\`,
/// other code units below U+0020 and lone surrogates as lowercase `\uxxxx`,
/// and every other character literal.
pub fn push_quoted(out: &mut String, text: &str) {
    out.push('"');
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            character if character < ' ' => {
                out.push_str(&format!("\\u{:04x}", u32::from(character)));
            }
            ESCAPE => {
                if chars.peek() == Some(&ESCAPE) {
                    chars.next();
                    out.push(ESCAPE);
                } else {
                    out.push_str("\\u");
                    out.extend(chars.by_ref().take(4));
                }
            }
            character => out.push(character),
        }
    }
    out.push('"');
}

/// `JSON.stringify(text)` as a spelling, for a message or a comparison
/// string that concatenates it: [`push_quoted`]'s JSON text, which holds no
/// lone surrogate, with any literal U+FDD0 doubled again by [`from_rust`].
/// Output bytes take [`push_quoted`]'s text instead.
pub fn quote(text: &str) -> String {
    let mut quoted = String::new();
    push_quoted(&mut quoted, text);
    match from_rust(&quoted) {
        Cow::Borrowed(_) => quoted,
        Cow::Owned(spelled) => spelled,
    }
}

/// `JSON.stringify(text).length` for a spelling.
pub fn quoted_len(text: &str) -> u64 {
    let mut length = 2;
    let mut units = code_units(text).peekable();
    while let Some(unit) = units.next() {
        length += match unit {
            0x22 | 0x5C | 0x08 | 0x09 | 0x0A | 0x0C | 0x0D => 2,
            unit if unit < 0x20 => 6,
            0xD800..=0xDBFF if matches!(units.peek(), Some(0xDC00..=0xDFFF)) => {
                units.next();
                2
            }
            0xD800..=0xDFFF => 6,
            _ => 1,
        };
    }
    length
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spell(units: &[u16]) -> String {
        let mut text = String::new();
        push_units(&mut text, units.iter().copied());
        text
    }

    fn quoted(text: &str) -> String {
        let mut out = String::new();
        push_quoted(&mut out, text);
        out
    }

    #[test]
    fn spellings_round_trip_code_units() {
        for units in [
            &[][..],
            &[0x61],
            &[0xD800],
            &[0xDC00],
            &[0xDC00, 0xD800],
            &[0xD83D, 0xDE00],
            &[0xFDD0],
            &[0xFDD0, 0xD800],
            &[0xFDD0, 0xFDD0, 0x64, 0x38, 0x30, 0x30],
            &[0xD800, 0xD800, 0xDC00],
            &[0x64, 0xDBFF],
        ] {
            let text = spell(units);
            assert_eq!(code_units(&text).collect::<Vec<_>>(), units, "{text:?}");
            assert_eq!(utf16_len(&text), units.len() as u64);
        }
        // A literal U+FDD0 followed by `d800` is not a lone surrogate.
        assert_ne!(spell(&[0xFDD0, 0x64, 0x38, 0x30, 0x30]), spell(&[0xD800]));
        assert_eq!(spell(&[0xD83D, 0xDE00]), "😀");
    }

    #[test]
    fn concatenation_pairs_split_surrogates() {
        let high = spell(&[0x61, 0xD83D]);
        let low = spell(&[0xDE00, 0x62]);
        assert_eq!(concat(&high, &low), "a😀b");
        // A doubled U+FDD0 ending in `d83d` text opens no escape.
        let literal = spell(&[0xFDD0, 0x64, 0x38, 0x33, 0x64]);
        assert_eq!(
            code_units(&concat(&literal, &low)).collect::<Vec<_>>(),
            [0xFDD0, 0x64, 0x38, 0x33, 0x64, 0xDE00, 0x62]
        );
        let escaped_high = spell(&[0xFDD0, 0xD83D]);
        assert_eq!(concat(&escaped_high, &low), "\u{FDD0}\u{FDD0}😀b");
    }

    #[test]
    fn quoting_matches_json_stringify() {
        assert_eq!(quoted(&spell(&[0xD800])), r#""\ud800""#);
        assert_eq!(quoted(&spell(&[0xDC00, 0xD83D])), r#""\udc00\ud83d""#);
        assert_eq!(quoted(&spell(&[0xFDD0])), "\"\u{FDD0}\"");
        assert_eq!(quoted("\u{1f}\"😀"), "\"\\u001f\\\"😀\"");
        // `quote` keeps a literal U+FDD0 spelled, so it reads back as itself.
        let noncharacter = spell(&[0x61, 0xFDD0, 0xD800]);
        assert_eq!(quote(&noncharacter), "\"a\u{FDD0}\u{FDD0}\\ud800\"");
        assert_eq!(
            code_units(&quote(&noncharacter)).collect::<Vec<_>>(),
            quoted(&noncharacter).encode_utf16().collect::<Vec<_>>()
        );
        for text in [spell(&[0xD800, 0x22]), "a\u{FDD0}\u{FDD0}😀\n".into()] {
            // `quoted` writes a Rust string, whose `encode_utf16` is `.length`.
            let length = quoted(&text).encode_utf16().count() as u64;
            assert_eq!(quoted_len(&text), length, "{text:?}");
        }
    }

    #[test]
    fn order_and_conversions_follow_code_units() {
        // U+E000 sorts after a lone high surrogate and before U+FDD0.
        let lone = spell(&[0xD800]);
        assert_eq!(cmp_code_units(&lone, "\u{E000}"), Ordering::Less);
        assert_eq!(
            cmp_code_units("\u{E000}", &spell(&[0xFDD0])),
            Ordering::Less
        );
        assert_eq!(from_rust("a\u{FDD0}"), "a\u{FDD0}\u{FDD0}");
        assert_eq!(to_rust(&from_rust("a\u{FDD0}")), "a\u{FDD0}");
        assert_eq!(to_rust(&spell(&[0x61, 0xD800])), "a\u{FFFD}");
    }
}
