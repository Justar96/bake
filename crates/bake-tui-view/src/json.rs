//! Output that is one JSON value, laid out to read: an object or an array,
//! often minified onto one line by an MCP tool, becomes one member or item
//! a line, indented two spaces a level, so a card can preview it and colour
//! it as code. Bake's own.
//!
//! The text is untrusted. It is read once, front to back, with a stack of
//! the containers it is inside rather than recursion, and any text that is
//! not exactly one JSON object or array under RFC 8259's grammar is left as
//! it came. Strings, numbers, and literals are copied as written, so the
//! lines read back as the same value; the control characters JSON lets a
//! string hold raw, such as U+007F, are written as their escapes.

/// Containers deeper than this leave the text as it came: each level adds
/// two cells of indent to every line inside it.
pub const MAX_DEPTH: usize = 32;
/// Text longer than this, in bytes, is left as it came, so laying it out
/// never costs a frame more than this much reading.
pub const MAX_BYTES: usize = 256 * 1024;

/// What the reader expects next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    Value,
    Key,
    Colon,
    CommaOrClose,
    End,
}

/// The lines being written: whole lines, and the one being filled.
struct Out {
    lines: Vec<String>,
    line: String,
}

impl Out {
    /// `token` on the current line, opening it at `depth`'s indent.
    fn token(&mut self, token: &str, depth: usize) {
        if self.line.is_empty() {
            self.line.push_str(&"  ".repeat(depth));
        }
        self.line.push_str(token);
    }

    fn newline(&mut self) {
        if !self.line.is_empty() {
            self.lines.push(std::mem::take(&mut self.line));
        }
    }
}

fn whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

/// The index past the whitespace at or after `at`.
fn skip(bytes: &[u8], mut at: usize) -> usize {
    while bytes.get(at).copied().is_some_and(whitespace) {
        at += 1;
    }
    at
}

/// The index past the string opening at `at`, or `None` when it is not a
/// valid JSON string.
fn string_end(bytes: &[u8], at: usize) -> Option<usize> {
    let mut i = at + 1;
    loop {
        match *bytes.get(i)? {
            b'"' => return Some(i + 1),
            b'\\' => match *bytes.get(i + 1)? {
                b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => i += 2,
                b'u' => {
                    let hex = bytes.get(i + 2..i + 6)?;
                    if !hex.iter().all(u8::is_ascii_hexdigit) {
                        return None;
                    }
                    i += 6;
                }
                _ => return None,
            },
            byte if byte < 0x20 => return None,
            _ => i += 1,
        }
    }
}

/// A string token with each control character JSON lets it hold raw, such
/// as U+007F or U+0085, written as its `\u` escape: the same string, and
/// nothing a terminal would act on.
fn escaped(token: &str) -> std::borrow::Cow<'_, str> {
    if !token.contains(char::is_control) {
        return token.into();
    }
    token
        .chars()
        .map(|c| {
            if c.is_control() {
                format!("\\u{:04x}", u32::from(c))
            } else {
                c.to_string()
            }
        })
        .collect::<String>()
        .into()
}

/// The index past the digits at or after `at`; `None` when there are none.
fn digits(bytes: &[u8], at: usize) -> Option<usize> {
    let end = at
        + bytes[at.min(bytes.len())..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
    (end > at).then_some(end)
}

/// The index past the number opening at `at`, or `None` when it is not a
/// valid JSON number: `-`, then `0` or digits not opening with `0`, then
/// any fraction and exponent.
fn number_end(bytes: &[u8], at: usize) -> Option<usize> {
    let mut i = at;
    if bytes.get(i) == Some(&b'-') {
        i += 1;
    }
    i = match bytes.get(i)? {
        b'0' => i + 1,
        b'1'..=b'9' => digits(bytes, i)?,
        _ => return None,
    };
    if bytes.get(i) == Some(&b'.') {
        i = digits(bytes, i + 1)?;
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(bytes.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        i = digits(bytes, i)?;
    }
    Some(i)
}

/// `text` as indented lines when all of it, but for whitespace around it,
/// is one JSON object or array; `None` for anything else, for containers
/// nested past [`MAX_DEPTH`], and for text past [`MAX_BYTES`]. An empty
/// object or array stays on its line, as `{}` or `[]`.
pub fn pretty(text: &str) -> Option<Vec<String>> {
    let bytes = text.as_bytes();
    if bytes.len() > MAX_BYTES {
        return None;
    }
    let first = skip(bytes, 0);
    if !matches!(bytes.get(first), Some(b'{' | b'[')) {
        return None;
    }
    let mut out = Out {
        lines: Vec::new(),
        line: String::new(),
    };
    // The open containers, each its opening byte.
    let mut stack: Vec<u8> = Vec::new();
    let mut expect = Expect::Value;
    let mut i = first;
    let after_value = |stack: &[u8]| {
        if stack.is_empty() {
            Expect::End
        } else {
            Expect::CommaOrClose
        }
    };
    loop {
        i = skip(bytes, i);
        let Some(&byte) = bytes.get(i) else {
            break;
        };
        let depth = stack.len();
        match (expect, byte) {
            (Expect::Value, b'{' | b'[') => {
                let close = if byte == b'{' { b'}' } else { b']' };
                let next = skip(bytes, i + 1);
                if bytes.get(next) == Some(&close) {
                    out.token(if byte == b'{' { "{}" } else { "[]" }, depth);
                    i = next + 1;
                    expect = after_value(&stack);
                    continue;
                }
                if depth == MAX_DEPTH {
                    return None;
                }
                out.token(&text[i..=i], depth);
                out.newline();
                stack.push(byte);
                i += 1;
                expect = if byte == b'{' {
                    Expect::Key
                } else {
                    Expect::Value
                };
            }
            (Expect::Value | Expect::Key, b'"') => {
                let end = string_end(bytes, i)?;
                out.token(&escaped(&text[i..end]), depth);
                i = end;
                expect = if expect == Expect::Key {
                    Expect::Colon
                } else {
                    after_value(&stack)
                };
            }
            (Expect::Value, b'-' | b'0'..=b'9') => {
                let end = number_end(bytes, i)?;
                out.token(&text[i..end], depth);
                i = end;
                expect = after_value(&stack);
            }
            (Expect::Value, b't' | b'f' | b'n') => {
                let literal = ["true", "false", "null"]
                    .into_iter()
                    .find(|literal| bytes[i..].starts_with(literal.as_bytes()))?;
                out.token(literal, depth);
                i += literal.len();
                expect = after_value(&stack);
            }
            (Expect::Colon, b':') => {
                out.line.push_str(": ");
                i += 1;
                expect = Expect::Value;
            }
            (Expect::CommaOrClose, b',') => {
                out.line.push(',');
                out.newline();
                i += 1;
                expect = if stack.last() == Some(&b'{') {
                    Expect::Key
                } else {
                    Expect::Value
                };
            }
            (Expect::CommaOrClose, b'}' | b']') => {
                let open = stack.pop()?;
                if (open, byte) != (b'{', b'}') && (open, byte) != (b'[', b']') {
                    return None;
                }
                out.newline();
                out.token(&text[i..=i], stack.len());
                i += 1;
                expect = after_value(&stack);
            }
            _ => return None,
        }
    }
    if expect != Expect::End {
        return None;
    }
    out.newline();
    Some(out.lines)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::Value;

    use super::*;

    #[test]
    fn a_minified_object_reads_one_member_a_line() {
        let text = r#"{"number":214,"title":"Fix: it","labels":["bug","ui"],"draft":false,"body":null,"meta":{}}"#;
        assert_eq!(
            pretty(text).unwrap(),
            [
                "{",
                r#"  "number": 214,"#,
                r#"  "title": "Fix: it","#,
                r#"  "labels": ["#,
                r#"    "bug","#,
                r#"    "ui""#,
                "  ],",
                r#"  "draft": false,"#,
                r#"  "body": null,"#,
                r#"  "meta": {}"#,
                "}",
            ]
        );
        assert_eq!(pretty("  [ ]\n").unwrap(), ["[]"]);
        assert_eq!(
            pretty(r#"[1.5e-3,-0,"é\n"]"#).unwrap(),
            ["[", "  1.5e-3,", "  -0,", r#"  "é\n""#, "]"]
        );
        assert_eq!(
            pretty("[\"a\u{7f}b\u{85}\"]").unwrap(),
            ["[", r#"  "a\u007fb\u0085""#, "]"]
        );
    }

    #[test]
    fn anything_but_one_object_or_array_is_left_as_it_came() {
        for text in [
            "",
            "42",
            r#""text""#,
            "true",
            "{",
            "[1,]",
            r#"{"a" 1}"#,
            r#"{"a":1}{"b":2}"#,
            "[1] trailing",
            "[01]",
            "[1.]",
            "[-]",
            "[tru]",
            "[\"tab\there\"]",
            r#"["\x"]"#,
            r#"["\u12"]"#,
            "{1:2}",
            "[1}",
            "{\"a\":1]",
            "Found 3 issues: [1, 2, 3]",
        ] {
            assert_eq!(pretty(text), None, "{text:?}");
        }
        let deep = format!(
            "{}1{}",
            "[".repeat(MAX_DEPTH + 1),
            "]".repeat(MAX_DEPTH + 1)
        );
        assert_eq!(pretty(&deep), None);
        let deepest = format!("{}1{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert_eq!(
            pretty(&deepest).map(|lines| lines.len()),
            Some(2 * MAX_DEPTH + 1)
        );
    }

    /// Any JSON value, a few levels deep.
    fn value() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::from),
            any::<i64>().prop_map(Value::from),
            any::<f64>()
                .prop_filter("finite", |n| n.is_finite())
                .prop_map(Value::from),
            ".{0,12}".prop_map(Value::from),
        ];
        leaf.prop_recursive(4, 32, 6, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
                prop::collection::vec((".{0,8}", inner), 0..6)
                    .prop_map(|members| Value::Object(members.into_iter().collect())),
            ]
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        // An object or array, minified or already indented, reads back as
        // the same value, one line holding no line break or control
        // character.
        #[test]
        fn laid_out_json_reads_back_as_the_same_value(value in value()) {
            prop_assume!(value.is_object() || value.is_array());
            for text in [
                serde_json::to_string(&value).unwrap(),
                serde_json::to_string_pretty(&value).unwrap(),
            ] {
                let lines = pretty(&text);
                prop_assert!(lines.is_some(), "{text}");
                let lines = lines.unwrap();
                for line in &lines {
                    prop_assert!(!line.contains(char::is_control), "{line:?}");
                }
                let back: Value = serde_json::from_str(&lines.join("\n")).unwrap();
                prop_assert_eq!(&back, &value);
            }
        }

        // Any text reads without a panic, and text laid out is the value
        // the text was.
        #[test]
        fn any_text_reads_as_its_value_or_is_left_alone(
            text in "[\\[\\]{}:,\" 0-9a-z\\\\.eE+-]{0,40}|.{0,40}",
        ) {
            if let Some(lines) = pretty(&text) {
                let back = serde_json::from_str::<Value>(&lines.join("\n")).ok();
                prop_assert_eq!(back, serde_json::from_str::<Value>(&text).ok());
            }
        }
    }
}
