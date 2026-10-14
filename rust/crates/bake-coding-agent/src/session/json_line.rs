//! Reading one session line as `JSON.parse` reads it.
//!
//! Pi reads each line with `JSON.parse`, which accepts any nesting depth,
//! string escapes that name a lone UTF-16 surrogate (`"\ud800"`), and numbers
//! beyond the double range (`1e400` is `Infinity`). `serde_json` rejects all
//! three, and a line it rejects would be skipped: the entries after it lose
//! their parent and the resumed context stops at the gap. The reader here
//! accepts exactly what `JSON.parse` accepts, without recursion, and reads
//! these cases as follows:
//!
//! - **Depth.** Values nest up to [`MAX_HELD_DEPTH`] levels in memory (the
//!   line itself is level 1), the limit `serde_json` itself parses to and a
//!   depth every recursive `serde_json` operation handles on a 1 MiB thread
//!   stack. A deeper container reads as `null` in memory, and its text is
//!   kept.
//! - **Lone surrogates.** A string holding one reads with U+FFFD in its place,
//!   and the string's `JSON.stringify` text, which escapes the surrogate as
//!   `\udxxx`, is kept. An object with such a key keeps its text as read,
//!   and each such key its `JSON.stringify` text. Two keys that differ only
//!   in lone surrogates are one key in memory: the object's text keeps both
//!   while it is unchanged, and the last one read wins once it changes.
//! - **Out-of-range numbers** read as `null`. `JSON.stringify` writes
//!   `Infinity` as `null`, so the line's bytes do not change; only a reader
//!   of that member sees `null` where Pi sees an infinity.
//!
//! The kept text ([`Verbatim`]) travels with the entry and is written in
//! place of the value it stands for as long as that value is unchanged, so a
//! migration rewrite, a fork, or a branched session leaves these lines as Pi
//! leaves them. Kept container text is written as it was read, where Pi
//! would re-serialize it; the two agree for every line `JSON.stringify`
//! wrote.
//!
//! Reading and writing cost time linear in the line: kept text sits in a
//! tree that follows the value only where something is kept, a
//! repeated key replaces its member's subtree in one step, and an object
//! kept for its keys is compared by reading its text again rather than by a
//! copy of its value.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use serde_json::Value;

use crate::session::json::{JsonObject, write_string};

/// The deepest nesting held in memory; deeper containers read as `null` and
/// keep their text. It is `serde_json`'s own parse limit. Every recursive
/// `serde_json` operation on a value this deep (drop, clone, comparison,
/// `Debug`, typed conversion) needs about 350 KiB of stack in a debug build;
/// a test runs them on a 1 MiB thread, Windows' default main-thread
/// stack. Callers that read session values need that much stack.
pub const MAX_HELD_DEPTH: usize = 128;

/// Why a value's text is kept.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Kept {
    /// A string holding a lone surrogate: the string in memory and its
    /// `JSON.stringify` text.
    String { value: String, text: String },
    /// A container nested past [`MAX_HELD_DEPTH`], `null` in memory: where
    /// its text lies in the line.
    Deep(Range<usize>),
    /// An object with a key holding a lone surrogate, inside `depth`
    /// containers: where its text lies in the line. Two such keys can name
    /// one key in memory, so the whole text is kept.
    LossyKeys { range: Range<usize>, depth: usize },
}

impl Kept {
    /// The kept text, when the value in memory is still the one read.
    /// A [`Kept::LossyKeys`] object is compared with its text read again,
    /// which costs the length of that text and keeps no copy in memory.
    pub(crate) fn text_for<'a>(&'a self, value: &Value, line: &'a str) -> Option<&'a str> {
        match self {
            Self::String { value: read, text } => {
                (value.as_str() == Some(read.as_str())).then_some(text.as_str())
            }
            Self::Deep(range) => value.is_null().then(|| line.get(range.clone())).flatten(),
            Self::LossyKeys { range, depth } => {
                let text = line.get(range.clone())?;
                (parse_at_depth(text, *depth)? == *value).then_some(text)
            }
        }
    }

    /// [`Kept::text_for`] for an object value.
    pub(crate) fn text_for_object<'a>(
        &'a self,
        object: &JsonObject,
        line: &'a str,
    ) -> Option<&'a str> {
        match self {
            Self::LossyKeys { range, depth } => {
                let text = line.get(range.clone())?;
                match parse_at_depth(text, *depth)? {
                    Value::Object(read) => (read == *object).then_some(text),
                    _ => None,
                }
            }
            Self::String { .. } | Self::Deep(_) => None,
        }
    }
}

/// The kept text below one value: a tree that mirrors the value only where
/// something is kept, so writing it costs the size of the value plus the
/// number of kept values, and memory grows with the number of kept values,
/// not with their depth.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Node {
    pub(crate) kept: Option<Kept>,
    /// `JSON.stringify` text of each key holding a lone surrogate, by its key
    /// in memory.
    pub(crate) key_texts: HashMap<String, String>,
    /// Object members with kept text below them.
    pub(crate) members: HashMap<String, Node>,
    /// Array items with kept text below them, by ascending index.
    pub(crate) items: Vec<(usize, Node)>,
}

impl Node {
    fn is_empty(&self) -> bool {
        self.kept.is_none()
            && self.key_texts.is_empty()
            && self.members.is_empty()
            && self.items.is_empty()
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Tree {
    /// The line the ranges of [`Kept`] point into.
    pub(crate) line: Box<str>,
    pub(crate) root: Node,
}

/// The text of values a line holds differently from `JSON.parse`; empty for
/// every line within [`MAX_HELD_DEPTH`] without lone surrogates. Cloning it
/// is cheap.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Verbatim(pub(crate) Option<Arc<Tree>>);

impl Verbatim {
    /// Whether every value of the line is held as `JSON.parse` reads it.
    pub fn is_empty(&self) -> bool {
        self.0.is_none()
    }
}

/// A parsed line.
#[derive(Debug)]
pub(crate) struct ParsedJson {
    pub(crate) value: Value,
    pub(crate) verbatim: Verbatim,
    /// The line is a single out-of-range number, truthy in JavaScript.
    pub(crate) non_finite_root: bool,
}

enum Frame {
    Array {
        items: Vec<Value>,
        node: Node,
    },
    Object {
        map: JsonObject,
        key: String,
        /// The pending key's `JSON.stringify` text when it holds a lone
        /// surrogate.
        key_text: Option<String>,
        start: usize,
        lossy_key: bool,
        node: Node,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Array,
    Object,
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    /// Containers around the text being read, for a text cut from a line.
    base_depth: usize,
    stack: Vec<Frame>,
    /// Containers below [`MAX_HELD_DEPTH`], validated without being built.
    skipped: Vec<Kind>,
    skip_start: usize,
    root: Option<(Value, Node)>,
}

/// `JSON.parse(text)`, or `None` where it throws.
pub(crate) fn parse_json_line(text: &str) -> Option<ParsedJson> {
    let (value, node) = parse_at_depth_with_node(text, 0)?;
    let first = text
        .bytes()
        .find(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | b'\r'));
    let non_finite_root = value.is_null() && first != Some(b'n');
    let verbatim = if node.is_empty() {
        Verbatim::default()
    } else {
        Verbatim(Some(Arc::new(Tree {
            line: text.into(),
            root: node,
        })))
    };
    Some(ParsedJson {
        value,
        verbatim,
        non_finite_root,
    })
}

/// A value's text cut from a line, inside `depth` containers, read as it was
/// read in the line.
fn parse_at_depth(text: &str, depth: usize) -> Option<Value> {
    parse_at_depth_with_node(text, depth).map(|(value, _)| value)
}

fn parse_at_depth_with_node(text: &str, base_depth: usize) -> Option<(Value, Node)> {
    let mut parser = Parser {
        text,
        bytes: text.as_bytes(),
        pos: 0,
        base_depth,
        stack: Vec::new(),
        skipped: Vec::new(),
        skip_start: 0,
        root: None,
    };
    parser.run()?;
    parser.root
}

impl<'a> Parser<'a> {
    fn skip_whitespace(&mut self) {
        while matches!(self.bytes.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn top_kind(&self) -> Option<Kind> {
        if let Some(kind) = self.skipped.last() {
            return Some(*kind);
        }
        self.stack.last().map(|frame| match frame {
            Frame::Array { .. } => Kind::Array,
            Frame::Object { .. } => Kind::Object,
        })
    }

    fn run(&mut self) -> Option<()> {
        let mut expect_value = true;
        loop {
            self.skip_whitespace();
            if expect_value {
                // A container left open expects its first value next.
                expect_value = self.value()?;
                continue;
            }
            let Some(kind) = self.top_kind() else {
                return (self.pos == self.bytes.len()).then_some(());
            };
            match (self.peek()?, kind) {
                (b',', Kind::Array) => {
                    self.pos += 1;
                    expect_value = true;
                }
                (b',', Kind::Object) => {
                    self.pos += 1;
                    self.member_key()?;
                    expect_value = true;
                }
                (b']', Kind::Array) | (b'}', Kind::Object) => {
                    self.pos += 1;
                    self.close()?;
                }
                _ => return None,
            }
        }
    }

    /// One value at the cursor; a container is opened, and closed at once
    /// when empty. `true` when a container was left open.
    fn value(&mut self) -> Option<bool> {
        let start = self.pos;
        match self.peek()? {
            open @ (b'[' | b'{') => {
                self.pos += 1;
                let kind = if open == b'[' {
                    Kind::Array
                } else {
                    Kind::Object
                };
                if !self.skipped.is_empty() || self.stack.len() + self.base_depth >= MAX_HELD_DEPTH
                {
                    if self.skipped.is_empty() {
                        self.skip_start = start;
                    }
                    self.skipped.push(kind);
                } else if kind == Kind::Array {
                    self.stack.push(Frame::Array {
                        items: Vec::new(),
                        node: Node::default(),
                    });
                } else {
                    self.stack.push(Frame::Object {
                        map: JsonObject::new(),
                        key: String::new(),
                        key_text: None,
                        start,
                        lossy_key: false,
                        node: Node::default(),
                    });
                }
                self.skip_whitespace();
                let close = if kind == Kind::Array { b']' } else { b'}' };
                if self.peek() == Some(close) {
                    self.pos += 1;
                    self.close()?;
                    return Some(false);
                }
                if kind == Kind::Object {
                    self.member_key()?;
                }
                Some(true)
            }
            b'"' => {
                let (text, canonical) = self.string()?;
                let node = match canonical {
                    Some(canonical) if self.skipped.is_empty() => Node {
                        kept: Some(Kept::String {
                            value: text.clone(),
                            text: canonical,
                        }),
                        ..Node::default()
                    },
                    _ => Node::default(),
                };
                self.attach(Value::String(text), node);
                Some(false)
            }
            b't' => self.literal("true", Value::Bool(true)),
            b'f' => self.literal("false", Value::Bool(false)),
            b'n' => self.literal("null", Value::Null),
            b'-' | b'0'..=b'9' => {
                let token = self.number()?;
                if self.skipped.is_empty() {
                    // Out of the double range: `Infinity`, which
                    // `JSON.stringify` writes as `null`.
                    let value = serde_json::from_str::<Value>(token).unwrap_or(Value::Null);
                    self.attach(value, Node::default());
                }
                Some(false)
            }
            _ => None,
        }
    }

    fn literal(&mut self, word: &str, value: Value) -> Option<bool> {
        let end = self.pos + word.len();
        if self.bytes.get(self.pos..end)? != word.as_bytes() {
            return None;
        }
        self.pos = end;
        self.attach(value, Node::default());
        Some(false)
    }

    /// `"key"` and `:` after `{` or `,`.
    fn member_key(&mut self) -> Option<()> {
        self.skip_whitespace();
        if self.peek()? != b'"' {
            return None;
        }
        let (key, canonical) = self.string()?;
        self.skip_whitespace();
        if self.peek()? != b':' {
            return None;
        }
        self.pos += 1;
        if self.skipped.is_empty()
            && let Some(Frame::Object {
                key: pending,
                key_text,
                lossy_key,
                ..
            }) = self.stack.last_mut()
        {
            *pending = key;
            *lossy_key |= canonical.is_some();
            *key_text = canonical;
        }
        Some(())
    }

    /// Close the innermost container, whose closing byte was just read.
    fn close(&mut self) -> Option<()> {
        if self.skipped.pop().is_some() {
            if self.skipped.is_empty() {
                let node = Node {
                    kept: Some(Kept::Deep(self.skip_start..self.pos)),
                    ..Node::default()
                };
                self.attach(Value::Null, node);
            }
            return Some(());
        }
        match self.stack.pop()? {
            Frame::Array { items, node } => self.attach(Value::Array(items), node),
            Frame::Object {
                map,
                start,
                lossy_key,
                mut node,
                ..
            } => {
                if lossy_key {
                    node.kept = Some(Kept::LossyKeys {
                        range: start..self.pos,
                        depth: self.stack.len() + self.base_depth,
                    });
                }
                self.attach(Value::Object(map), node);
            }
        }
        Some(())
    }

    /// Give a finished value and the text kept below it to its container;
    /// values inside a skipped container are dropped. Each step costs the
    /// same however many values are kept, so a line reads in linear time.
    fn attach(&mut self, value: Value, below: Node) {
        if !self.skipped.is_empty() {
            return;
        }
        match self.stack.last_mut() {
            None => self.root = Some((value, below)),
            Some(Frame::Array { items, node }) => {
                if !below.is_empty() {
                    node.items.push((items.len(), below));
                }
                items.push(value);
            }
            Some(Frame::Object {
                map,
                key,
                key_text,
                node,
                ..
            }) => {
                let key = std::mem::take(key);
                match key_text.take() {
                    Some(text) => {
                        node.key_texts.insert(key.clone(), text);
                    }
                    None if !node.key_texts.is_empty() => {
                        node.key_texts.remove(&key);
                    }
                    None => {}
                }
                if !below.is_empty() {
                    node.members.insert(key.clone(), below);
                } else if !node.members.is_empty() {
                    // A repeated key: the last value wins, as in
                    // `JSON.parse`, and so does the text kept below it.
                    node.members.remove(&key);
                }
                map.insert(key, value);
            }
        }
    }

    /// A string at the cursor: its value, and when it holds a lone
    /// surrogate, its `JSON.stringify` text.
    fn string(&mut self) -> Option<(String, Option<String>)> {
        let start = self.pos;
        let (out, lossy) = self.string_units(None)?;
        if !lossy {
            return Some((out, None));
        }
        let end = self.pos;
        self.pos = start;
        let mut units = Vec::new();
        self.string_units(Some(&mut units))?;
        self.pos = end;
        Some((out, Some(canonical_string(&units))))
    }

    /// Decode a string, collecting its pieces when asked; the flag is
    /// whether it holds a lone surrogate.
    fn string_units(&mut self, mut units: Option<&mut Vec<Unit>>) -> Option<(String, bool)> {
        self.pos += 1;
        let mut out = String::new();
        let mut lossy = false;
        let mut run = self.pos;
        loop {
            match self.peek()? {
                b'"' => {
                    let tail = self.text.get(run..self.pos)?;
                    out.push_str(tail);
                    if let Some(units) = units.as_deref_mut() {
                        units.push(Unit::Text(tail.to_owned()));
                    }
                    self.pos += 1;
                    return Some((out, lossy));
                }
                b'\\' => {
                    let chunk = self.text.get(run..self.pos)?;
                    out.push_str(chunk);
                    self.pos += 1;
                    let escape = self.peek()?;
                    self.pos += 1;
                    let decoded = match escape {
                        b'"' => Ok('"'),
                        b'\\' => Ok('\\'),
                        b'/' => Ok('/'),
                        b'b' => Ok('\u{8}'),
                        b'f' => Ok('\u{c}'),
                        b'n' => Ok('\n'),
                        b'r' => Ok('\r'),
                        b't' => Ok('\t'),
                        b'u' => {
                            let unit = self.hex4()?;
                            self.code_point(unit)
                        }
                        _ => return None,
                    };
                    let ch = decoded.unwrap_or('\u{fffd}');
                    lossy |= decoded.is_err();
                    out.push(ch);
                    if let Some(units) = units.as_deref_mut() {
                        units.push(Unit::Text(chunk.to_owned()));
                        units.push(match decoded {
                            Ok(ch) => Unit::Text(ch.to_string()),
                            Err(lone) => Unit::Lone(lone),
                        });
                    }
                    run = self.pos;
                }
                0..=0x1f => return None,
                _ => self.pos += 1,
            }
        }
    }

    fn hex4(&mut self) -> Option<u16> {
        let digits = self.text.get(self.pos..self.pos + 4)?;
        if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        self.pos += 4;
        u16::from_str_radix(digits, 16).ok()
    }

    /// The character a `\u` escape starts, consuming a following low
    /// surrogate escape; `Err` with the unit for a lone surrogate.
    fn code_point(&mut self, unit: u16) -> Result<char, u16> {
        if (0xd800..0xdc00).contains(&unit)
            && self.bytes.get(self.pos..self.pos + 2) == Some(b"\\u")
        {
            let saved = self.pos;
            self.pos += 2;
            match self.hex4() {
                Some(low) if (0xdc00..0xe000).contains(&low) => {
                    let code =
                        0x10000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(low) - 0xdc00);
                    if let Some(ch) = char::from_u32(code) {
                        return Ok(ch);
                    }
                    self.pos = saved;
                }
                _ => self.pos = saved,
            }
        }
        char::from_u32(u32::from(unit)).ok_or(unit)
    }

    /// A number token at the cursor, in JSON's grammar.
    fn number(&mut self) -> Option<&'a str> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek()? {
            b'0' => self.pos += 1,
            b'1'..=b'9' => self.digits(),
            _ => return None,
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !self.peek()?.is_ascii_digit() {
                return None;
            }
            self.digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !self.peek()?.is_ascii_digit() {
                return None;
            }
            self.digits();
        }
        self.text.get(start..self.pos)
    }

    fn digits(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.pos += 1;
        }
    }
}

enum Unit {
    Text(String),
    Lone(u16),
}

/// `JSON.stringify` of a string with lone surrogates, which it escapes as
/// lowercase `\udxxx`.
fn canonical_string(units: &[Unit]) -> String {
    let mut out = String::from("\"");
    for unit in units {
        match unit {
            Unit::Text(part) => {
                let mut quoted = String::new();
                write_string(&mut quoted, part);
                out.push_str(quoted.get(1..quoted.len() - 1).unwrap_or(""));
            }
            Unit::Lone(unit) => out.push_str(&format!("\\u{unit:04x}")),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::json::js_stringify_held;

    fn parse(text: &str) -> Option<ParsedJson> {
        parse_json_line(text)
    }

    fn nested(depth: usize, leaf: &str) -> String {
        format!("{}{leaf}{}", "[".repeat(depth), "]".repeat(depth))
    }

    #[test]
    fn reads_what_serde_json_reads_within_its_limits() {
        let lines = [
            r#"{"a":1,"b":[true,false,null,"x\n\u00e9\ud83d\ude00\/"],"c":{"d":-0.5e-3}}"#,
            r#" {"a":1,"b":2,"a":3} "#,
            "[9007199254740993,12345678901234567890,-9223372036854775808,1E2,0,-0]",
            "\"\"",
            "{}\r",
            "[[],{}]",
        ];
        for line in lines {
            let ours = parse(line).map(|parsed| parsed.value);
            let theirs: Option<Value> = serde_json::from_str(line).ok();
            assert_eq!(ours, theirs, "{line}");
            assert!(
                parse(line).is_some_and(|parsed| parsed.verbatim.is_empty()),
                "{line}"
            );
        }
    }

    #[test]
    fn rejects_what_json_parse_rejects() {
        let lines = [
            "",
            " ",
            "[1,]",
            "{\"a\":1,}",
            "01",
            "1.",
            ".5",
            "1e",
            "-",
            "+1",
            "tru",
            "nul",
            "[1 2]",
            "{\"a\" 1}",
            "{a:1}",
            "'a'",
            "\"a",
            "\"\t\"",
            "\"\\x\"",
            "\"\\u12\"",
            "NaN",
            "Infinity",
            "[1]]",
            "{}{}",
            "[",
            "{\"a\":",
            "\"\\ud800\\u12\"",
        ];
        for line in lines {
            assert!(parse(line).is_none(), "{line:?}");
        }
    }

    #[test]
    fn nesting_has_no_limit_and_no_recursion() {
        let deep = nested(1_000_000, "1");
        let parsed = parse(&deep).expect("deep line");
        assert!(!parsed.verbatim.is_empty());
        assert_eq!(js_stringify_held(&parsed.value, &parsed.verbatim), deep);
        // An unclosed or malformed deep line fails without a stack overflow.
        assert!(parse(&"[".repeat(1_000_000)).is_none());
        assert!(
            parse(&format!(
                "{}x{}",
                "{\"a\":".repeat(100_000),
                "}".repeat(100_000)
            ))
            .is_none()
        );
    }

    #[test]
    fn values_below_the_held_depth_read_as_null_and_keep_their_text() {
        let within = nested(MAX_HELD_DEPTH, "");
        let parsed = parse(&within).expect("within");
        assert!(parsed.verbatim.is_empty());

        let line = format!(
            r#"{{"a":1,"data":{},"z":"end"}}"#,
            nested(MAX_HELD_DEPTH, "{\"k\": 1e400}")
        );
        let parsed = parse(&line).expect("beyond");
        assert!(!parsed.verbatim.is_empty());
        assert_eq!(js_stringify_held(&parsed.value, &parsed.verbatim), line);
        // Changing the value drops the kept text.
        let mut changed = parsed.value.clone();
        if let Some(object) = changed.as_object_mut() {
            object.insert("data".into(), Value::from(2));
        }
        assert_eq!(
            js_stringify_held(&changed, &parsed.verbatim),
            r#"{"a":1,"data":2,"z":"end"}"#
        );
    }

    #[test]
    fn a_repeated_key_drops_the_kept_text_it_replaces() {
        let line = format!(r#"{{"a":{},"a":null}}"#, nested(MAX_HELD_DEPTH, ""));
        let parsed = parse(&line).expect("line");
        assert!(parsed.verbatim.is_empty());
        assert_eq!(
            js_stringify_held(&parsed.value, &parsed.verbatim),
            r#"{"a":null}"#
        );
        // The value that wins keeps its own text (Node:
        // `JSON.stringify(JSON.parse(line))`).
        let line = format!(r#"{{"a":{},"a":["\ud800"]}}"#, nested(MAX_HELD_DEPTH, ""));
        let parsed = parse(&line).expect("line");
        assert_eq!(
            js_stringify_held(&parsed.value, &parsed.verbatim),
            r#"{"a":["\ud800"]}"#
        );
    }

    #[test]
    fn lone_surrogates_read_as_replacement_and_write_as_json_stringify_does() {
        // Node: JSON.stringify(JSON.parse(line)).
        let line = r#"{"a":"x\uD800\/y\ud83d\ude00","b":["\udc00"],"c":"\ud800\u0041"}"#;
        let parsed = parse(line).expect("line");
        assert_eq!(parsed.value["a"], "x\u{fffd}/y😀");
        assert_eq!(parsed.value["b"][0], "\u{fffd}");
        assert_eq!(parsed.value["c"], "\u{fffd}A");
        assert_eq!(
            js_stringify_held(&parsed.value, &parsed.verbatim),
            r#"{"a":"x\ud800/y😀","b":["\udc00"],"c":"\ud800A"}"#
        );
        let key = r#"{"o":{"\ud800":1}}"#;
        let parsed = parse(key).expect("key");
        assert_eq!(js_stringify_held(&parsed.value, &parsed.verbatim), key);
    }

    #[test]
    fn out_of_range_numbers_read_as_null() {
        let parsed = parse(r#"{"a":1e400,"b":-1e400,"c":1e-400}"#).expect("line");
        assert_eq!(
            js_stringify_held(&parsed.value, &parsed.verbatim),
            r#"{"a":null,"b":null,"c":0}"#
        );
        assert!(!parsed.non_finite_root);
        assert!(parse("1e400").is_some_and(|parsed| parsed.non_finite_root));
        assert!(parse(" null").is_some_and(|parsed| !parsed.non_finite_root));
    }

    #[test]
    fn a_changed_object_keeps_the_text_of_its_keys_and_unchanged_members() {
        // Two keys that are one key in memory: the object is kept whole.
        let inner = r#"{"\ud800":1,"\udc00":2}"#;
        let line = format!(r#"{{"\ud801":"x","o":{inner},"s":"\udfff"}}"#);
        let parsed = parse(&line).expect("line");
        assert_eq!(js_stringify_held(&parsed.value, &parsed.verbatim), line);
        let mut changed = parsed.value.clone();
        if let Some(object) = changed.as_object_mut() {
            object.insert("n".into(), Value::from(1));
        }
        // Node: JSON.stringify of the parsed line with `n: 1` added.
        assert_eq!(
            js_stringify_held(&changed, &parsed.verbatim),
            format!(r#"{{"\ud801":"x","o":{inner},"s":"\udfff","n":1}}"#)
        );
        // Once such an object changes, its two keys are one: the last read
        // wins with the text it was read with. Pi writes both,
        // `{"\ud800":1,"\ufffd":2,"n":1}` (Node), a documented deviation.
        let line = r#"{"\ud800":1,"\ufffd":2}"#;
        let parsed = parse(line).expect("line");
        let mut changed = parsed.value.clone();
        if let Some(object) = changed.as_object_mut() {
            object.insert("n".into(), Value::from(1));
        }
        assert_eq!(
            js_stringify_held(&changed, &parsed.verbatim),
            "{\"\u{fffd}\":2,\"n\":1}"
        );
    }

    /// An object kept for its keys is read again at its own depth, so a
    /// container inside it past [`MAX_HELD_DEPTH`] is `null` again and the
    /// object still matches.
    #[test]
    fn a_surrogate_key_object_near_the_depth_limit_compares_at_its_depth() {
        let outer = MAX_HELD_DEPTH - 8;
        let object = format!(r#"{{"\ud800":1,"\udc00":{}}}"#, nested(20, "1"));
        let line = format!("{}{object}{}", "[".repeat(outer), "]".repeat(outer));
        let parsed = parse(&line).expect("line");
        assert_eq!(js_stringify_held(&parsed.value, &parsed.verbatim), line);
    }

    /// Nested objects with surrogate keys keep no copy of their values, and
    /// a change at the root reads the text below it again once, not once per
    /// level. The work runs on a worker that must finish within a bound.
    #[test]
    fn nested_surrogate_key_objects_write_in_linear_time() {
        let levels = MAX_HELD_DEPTH - 10;
        let strings = vec![r#""\udc00""#; 50_000].join(",");
        let body = format!(
            "{}[{strings}]{}",
            r#"{"\ud800":"#.repeat(levels),
            "}".repeat(levels)
        );
        let line = format!(r#"{{"\ud801":1,"o":{body}}}"#);
        let expected = format!(r#"{{"\ud801":1,"o":{body},"n":1}}"#);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let parsed = parse(&line).expect("line");
            let unchanged = js_stringify_held(&parsed.value, &parsed.verbatim);
            let mut changed = parsed.value.clone();
            if let Some(object) = changed.as_object_mut() {
                object.insert("n".into(), Value::from(1));
            }
            let written = js_stringify_held(&changed, &parsed.verbatim);
            let _ = sender.send((unchanged == line, written));
        });
        let (unchanged, written) = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("linear time");
        assert!(unchanged);
        assert_eq!(written, expected);
    }
}
