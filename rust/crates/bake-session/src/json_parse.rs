//! `JSON.parse` for Session text, without a nesting bound, and stack-safe
//! walks over the values it builds.
//!
//! [`parse_json`] reads RFC 8259 JSON with an explicit stack, so a row nested
//! a million containers deep parses as `JSON.parse` parses it. For every
//! number whose integer part has at most 768 digits, it builds the
//! `serde_json::Value` `serde_json::from_str` builds for the same text under
//! this workspace's `float_roundtrip` and `preserve_order` features; a longer
//! integer part, which serde_json can misround and the scan refuses as
//! [`crate::ScanLimit::NumberLexeme`], is rounded correctly here. It fails
//! where serde_json fails, with the same split between syntax errors
//! `JSON.parse` also rejects and refusals that decide nothing:
//!
//! - Whitespace is space, tab, LF, and CR, before and after every token.
//! - A number without a fraction or exponent is a `u64` when non-negative and
//!   in range, an `i64` when negative and in range, and otherwise the nearest
//!   double; `-0` is the double -0. Any other number is the nearest double.
//!   A number whose nearest double is infinite is [`JsonParseError::NumberOutOfRange`];
//!   `JSON.parse` reads it as `Infinity`, which no writer stores.
//! - A `\u` escape of an unpaired surrogate, which `JSON.parse` keeps and a
//!   Rust string cannot hold, is [`JsonParseError::LoneSurrogate`] at the
//!   point serde_json refuses it, which D21 leaves undecided.
//! - A repeated object key keeps its first position and its last value, as
//!   `JSON.parse` defines its member and its value. `__proto__` is an ordinary
//!   key, as `JSON.parse` creates it as an own data property.
//!
//! A `Value`'s derived `Drop`, `Clone`, `PartialEq`, `Debug`, and `Display`
//! recurse once per nesting level. [`dismantle`], [`clone_value`], and
//! [`values_equal`] are their iterative replacements for values a parse may
//! have nested arbitrarily deep.

use serde_json::{Map, Number, Value};

/// Why [`parse_json`] built no value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonParseError {
    /// The text is not JSON; `JSON.parse` throws `SyntaxError`.
    Syntax,
    /// A `\u` escape leaves a surrogate unpaired.
    LoneSurrogate,
    /// A number's nearest double is infinite.
    NumberOutOfRange,
}

impl JsonParseError {
    /// Whether `JSON.parse` also rejects the text.
    pub const fn is_syntax(self) -> bool {
        matches!(self, Self::Syntax)
    }
}

/// An open container and, for an object, the key its next value takes.
enum Frame {
    Array(Vec<Value>),
    Object(Map<String, Value>, String),
}

/// Parse `text` as `JSON.parse` does, at any nesting depth.
///
/// The result may nest as deep as `text` does, and a `Value`'s derived drop,
/// `Clone`, `PartialEq`, `Debug`, and serialization recurse once per level:
/// drop a result that may be deep with [`dismantle`], and do not clone,
/// compare, format, or serialize it with those derived traits.
pub fn parse_json(text: &str) -> Result<Value, JsonParseError> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        index: 0,
    };
    let mut stack: Vec<Frame> = Vec::new();
    let result = parser.document(&mut stack);
    for frame in stack {
        match frame {
            Frame::Array(items) => items.into_iter().for_each(dismantle),
            Frame::Object(fields, _) => fields.into_iter().for_each(|(_, value)| dismantle(value)),
        }
    }
    result
}

struct Parser<'a> {
    bytes: &'a [u8],
    index: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }

    fn skip_whitespace(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.peek() {
            self.index += 1;
        }
    }

    fn document(&mut self, stack: &mut Vec<Frame>) -> Result<Value, JsonParseError> {
        let value = self.value(stack)?;
        self.skip_whitespace();
        if self.index != self.bytes.len() {
            dismantle(value);
            return Err(JsonParseError::Syntax);
        }
        Ok(value)
    }

    /// One whole value, opening containers on `stack` instead of recursing.
    fn value(&mut self, stack: &mut Vec<Frame>) -> Result<Value, JsonParseError> {
        let base = stack.len();
        'open: loop {
            self.skip_whitespace();
            let open = self.peek();
            let mut value = match open.ok_or(JsonParseError::Syntax)? {
                b'[' => {
                    self.index += 1;
                    self.skip_whitespace();
                    if self.peek() == Some(b']') {
                        self.index += 1;
                        Value::Array(Vec::new())
                    } else {
                        stack.push(Frame::Array(Vec::new()));
                        continue 'open;
                    }
                }
                b'{' => {
                    self.index += 1;
                    self.skip_whitespace();
                    match self.peek() {
                        Some(b'}') => {
                            self.index += 1;
                            Value::Object(Map::new())
                        }
                        Some(b'"') => {
                            let key = self.key()?;
                            stack.push(Frame::Object(Map::new(), key));
                            continue 'open;
                        }
                        _ => return Err(JsonParseError::Syntax),
                    }
                }
                _ => self.scalar()?,
            };
            // Close every container `value` completes, then open the next value.
            loop {
                if stack.len() == base {
                    return Ok(value);
                }
                self.skip_whitespace();
                let next = self.peek();
                match stack.last_mut().expect("an open container") {
                    Frame::Array(items) => {
                        items.push(value);
                        match next {
                            Some(b',') => {
                                self.index += 1;
                                continue 'open;
                            }
                            Some(b']') => {
                                self.index += 1;
                                let Some(Frame::Array(items)) = stack.pop() else {
                                    unreachable!("the top frame is an array");
                                };
                                value = Value::Array(items);
                            }
                            _ => return Err(JsonParseError::Syntax),
                        }
                    }
                    Frame::Object(fields, key) => {
                        if let Some(previous) = fields.insert(std::mem::take(key), value) {
                            dismantle(previous);
                        }
                        match next {
                            Some(b',') => {
                                self.index += 1;
                                self.skip_whitespace();
                                if self.peek() != Some(b'"') {
                                    return Err(JsonParseError::Syntax);
                                }
                                *key = self.key()?;
                                continue 'open;
                            }
                            Some(b'}') => {
                                self.index += 1;
                                let Some(Frame::Object(fields, _)) = stack.pop() else {
                                    unreachable!("the top frame is an object");
                                };
                                value = Value::Object(fields);
                            }
                            _ => return Err(JsonParseError::Syntax),
                        }
                    }
                }
            }
        }
    }

    /// A member name and its colon; the cursor is on the opening quote.
    fn key(&mut self) -> Result<String, JsonParseError> {
        self.index += 1;
        let key = self.string()?;
        self.skip_whitespace();
        if self.peek() != Some(b':') {
            return Err(JsonParseError::Syntax);
        }
        self.index += 1;
        Ok(key)
    }

    fn scalar(&mut self) -> Result<Value, JsonParseError> {
        match self.peek() {
            Some(b'"') => {
                self.index += 1;
                self.string().map(Value::String)
            }
            Some(b't') => self.literal(b"true", Value::Bool(true)),
            Some(b'f') => self.literal(b"false", Value::Bool(false)),
            Some(b'n') => self.literal(b"null", Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(JsonParseError::Syntax),
        }
    }

    fn literal(&mut self, word: &[u8], value: Value) -> Result<Value, JsonParseError> {
        if self.bytes[self.index..].starts_with(word) {
            self.index += word.len();
            Ok(value)
        } else {
            Err(JsonParseError::Syntax)
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.index;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.index += 1;
        }
        self.index - start
    }

    fn number(&mut self) -> Result<Value, JsonParseError> {
        let start = self.index;
        let negative = self.peek() == Some(b'-');
        if negative {
            self.index += 1;
        }
        let integer_start = self.index;
        match self.peek() {
            Some(b'0') => {
                self.index += 1;
                if self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    return Err(JsonParseError::Syntax);
                }
            }
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(JsonParseError::Syntax),
        }
        let integer_end = self.index;
        let mut integral = true;
        if self.peek() == Some(b'.') {
            self.index += 1;
            integral = false;
            if self.digits() == 0 {
                return Err(JsonParseError::Syntax);
            }
        }
        if let Some(b'e' | b'E') = self.peek() {
            self.index += 1;
            integral = false;
            if let Some(b'+' | b'-') = self.peek() {
                self.index += 1;
            }
            if self.digits() == 0 {
                return Err(JsonParseError::Syntax);
            }
        }
        let lexeme =
            std::str::from_utf8(&self.bytes[start..self.index]).expect("a number lexeme is ASCII");
        if integral {
            let digits = std::str::from_utf8(&self.bytes[integer_start..integer_end])
                .expect("integer digits are ASCII");
            if let Ok(significand) = digits.parse::<u64>() {
                // serde_json's integer rule: `-0` and negatives below
                // `i64::MIN` become doubles.
                return Ok(if !negative {
                    Value::Number(significand.into())
                } else if significand == 0 || significand > 1 << 63 {
                    double(-(significand as f64))?
                } else {
                    Value::Number((significand as i64).wrapping_neg().into())
                });
            }
        }
        double(lexeme.parse::<f64>().map_err(|_| JsonParseError::Syntax)?)
    }

    /// A string's contents and closing quote; the cursor is past the opening quote.
    fn string(&mut self) -> Result<String, JsonParseError> {
        let mut text = String::new();
        loop {
            let start = self.index;
            while let Some(byte) = self.peek() {
                if byte == b'"' || byte == b'\\' || byte < 0x20 {
                    break;
                }
                self.index += 1;
            }
            text.push_str(
                std::str::from_utf8(&self.bytes[start..self.index])
                    .expect("the input is UTF-8 and the run ends at an ASCII byte"),
            );
            match self.peek() {
                Some(b'"') => {
                    self.index += 1;
                    return Ok(text);
                }
                Some(b'\\') => {
                    self.index += 1;
                    self.escape(&mut text)?;
                }
                _ => return Err(JsonParseError::Syntax),
            }
        }
    }

    fn escape(&mut self, text: &mut String) -> Result<(), JsonParseError> {
        let byte = self.peek().ok_or(JsonParseError::Syntax)?;
        self.index += 1;
        let decoded = match byte {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => return self.unicode_escape(text),
            _ => return Err(JsonParseError::Syntax),
        };
        text.push(decoded);
        Ok(())
    }

    /// serde_json's `parse_unicode_escape` with validation: the cursor is
    /// past `\u`. Each refusal happens where serde_json's does, so an
    /// earlier syntax error still wins.
    fn unicode_escape(&mut self, text: &mut String) -> Result<(), JsonParseError> {
        let first = self.hex_escape()?;
        if (0xDC00..=0xDFFF).contains(&first) {
            return Err(JsonParseError::LoneSurrogate);
        }
        if !(0xD800..=0xDBFF).contains(&first) {
            text.push(char::from_u32(first.into()).expect("not a surrogate"));
            return Ok(());
        }
        for expected in *b"\\u" {
            match self.peek() {
                None => return Err(JsonParseError::Syntax),
                Some(byte) if byte == expected => self.index += 1,
                Some(_) => return Err(JsonParseError::LoneSurrogate),
            }
        }
        let second = self.hex_escape()?;
        if !(0xDC00..=0xDFFF).contains(&second) {
            return Err(JsonParseError::LoneSurrogate);
        }
        let code = 0x1_0000 + ((u32::from(first) - 0xD800) << 10) + (u32::from(second) - 0xDC00);
        text.push(char::from_u32(code).expect("a surrogate pair is a scalar value"));
        Ok(())
    }

    fn hex_escape(&mut self) -> Result<u16, JsonParseError> {
        let digits = self
            .bytes
            .get(self.index..self.index + 4)
            .ok_or(JsonParseError::Syntax)?;
        self.index += 4;
        let mut code = 0u16;
        for &digit in digits {
            let value = (digit as char).to_digit(16).ok_or(JsonParseError::Syntax)?;
            code = code * 16 + value as u16;
        }
        Ok(code)
    }
}

/// The nearest double as serde_json stores it.
fn double(value: f64) -> Result<Value, JsonParseError> {
    Number::from_f64(value)
        .map(Value::Number)
        .ok_or(JsonParseError::NumberOutOfRange)
}

/// Drop `value` without recursing: each container's children are moved onto
/// a heap stack before the container itself drops empty.
pub fn dismantle(value: Value) {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Array(items) => pending.extend(items),
            Value::Object(fields) => pending.extend(fields.into_iter().map(|(_, value)| value)),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
}

/// [`dismantle`] every member value of `fields`.
pub(crate) fn dismantle_fields(fields: Map<String, Value>) {
    dismantle(Value::Object(fields));
}

/// `fields.insert(key, value)`, dismantling the value it replaces.
pub(crate) fn replace_member(fields: &mut Map<String, Value>, key: &str, value: Value) {
    if let Some(previous) = fields.insert(key.to_owned(), value) {
        dismantle(previous);
    }
}

/// `fields.shift_remove(key)`, dismantling the removed value.
pub(crate) fn remove_member(fields: &mut Map<String, Value>, key: &str) {
    if let Some(previous) = fields.shift_remove(key) {
        dismantle(previous);
    }
}

/// A container being copied and the source children still to copy.
enum CopyFrame<'a> {
    Array(Vec<Value>, std::slice::Iter<'a, Value>),
    Object(
        Map<String, Value>,
        serde_json::map::Iter<'a>,
        Option<String>,
    ),
}

/// `value.clone()` without recursing.
pub(crate) fn clone_value(value: &Value) -> Value {
    let mut stack: Vec<CopyFrame<'_>> = Vec::new();
    let mut next = Some(value);
    loop {
        // Copy `next`, opening a frame for a non-empty container.
        let mut copied = match next.take() {
            Some(Value::Array(items)) if !items.is_empty() => {
                stack.push(CopyFrame::Array(
                    Vec::with_capacity(items.len()),
                    items.iter(),
                ));
                None
            }
            Some(Value::Object(fields)) if !fields.is_empty() => {
                stack.push(CopyFrame::Object(Map::new(), fields.iter(), None));
                None
            }
            Some(Value::Array(_)) => Some(Value::Array(Vec::new())),
            Some(Value::Object(_)) => Some(Value::Object(Map::new())),
            Some(scalar) => Some(scalar.clone()),
            None => None,
        };
        // Attach the copy to its parent, closing finished frames.
        loop {
            let Some(frame) = stack.last_mut() else {
                return copied.expect("the root copy is complete");
            };
            match frame {
                CopyFrame::Array(items, source) => {
                    if let Some(value) = copied.take() {
                        items.push(value);
                    }
                    if let Some(child) = source.next() {
                        next = Some(child);
                        break;
                    }
                }
                CopyFrame::Object(fields, source, key) => {
                    if let Some(value) = copied.take() {
                        fields.insert(key.take().expect("a pending key"), value);
                    }
                    if let Some((child_key, child)) = source.next() {
                        *key = Some(child_key.clone());
                        next = Some(child);
                        break;
                    }
                }
            }
            copied = Some(match stack.pop().expect("the frame just read") {
                CopyFrame::Array(items, _) => Value::Array(items),
                CopyFrame::Object(fields, _, _) => Value::Object(fields),
            });
        }
    }
}

/// [`clone_value`] for an object's members.
pub(crate) fn clone_fields(fields: &Map<String, Value>) -> Map<String, Value> {
    let mut copy = Map::new();
    for (key, value) in fields {
        copy.insert(key.clone(), clone_value(value));
    }
    copy
}

/// `left == right` without recursing. Object equality ignores member order,
/// as serde_json's `preserve_order` map equality does.
pub(crate) fn values_equal(left: &Value, right: &Value) -> bool {
    let mut pending = vec![(left, right)];
    while let Some((left, right)) = pending.pop() {
        match (left, right) {
            (Value::Array(left), Value::Array(right)) => {
                if left.len() != right.len() {
                    return false;
                }
                pending.extend(left.iter().zip(right));
            }
            (Value::Object(left), Value::Object(right)) => {
                if left.len() != right.len() {
                    return false;
                }
                for (key, value) in left {
                    let Some(other) = right.get(key) else {
                        return false;
                    };
                    pending.push((value, other));
                }
            }
            (Value::Array(_) | Value::Object(_), _) | (_, Value::Array(_) | Value::Object(_)) => {
                return false;
            }
            (left, right) => {
                if left != right {
                    return false;
                }
            }
        }
    }
    true
}

/// [`values_equal`] for two objects.
pub(crate) fn fields_equal(left: &Map<String, Value>, right: &Map<String, Value>) -> bool {
    left.len() == right.len()
        && left.iter().all(|(key, value)| {
            right
                .get(key)
                .is_some_and(|other| values_equal(value, other))
        })
}

/// JSON that a parse may have nested arbitrarily deep: a value, an object's
/// members, or a collection of them.
pub(crate) trait DeepJson: Default {
    /// Move every contained value onto `out`.
    fn into_values(self, out: &mut Vec<Value>);
    /// A copy made without recursing.
    fn deep_clone(&self) -> Self;
    /// Equality decided without recursing.
    fn deep_eq(&self, other: &Self) -> bool;
    /// Diagnostic text written without recursing.
    fn deep_debug(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result;
}

impl DeepJson for Value {
    fn into_values(self, out: &mut Vec<Value>) {
        out.push(self);
    }

    fn deep_clone(&self) -> Self {
        clone_value(self)
    }

    fn deep_eq(&self, other: &Self) -> bool {
        values_equal(self, other)
    }

    fn deep_debug(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::json_text(self))
    }
}

impl DeepJson for Map<String, Value> {
    fn into_values(self, out: &mut Vec<Value>) {
        out.extend(self.into_iter().map(|(_, value)| value));
    }

    fn deep_clone(&self) -> Self {
        clone_fields(self)
    }

    fn deep_eq(&self, other: &Self) -> bool {
        fields_equal(self, other)
    }

    fn deep_debug(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::json_text::json_object_text(self))
    }
}

impl DeepJson for Option<Value> {
    fn into_values(self, out: &mut Vec<Value>) {
        out.extend(self);
    }

    fn deep_clone(&self) -> Self {
        self.as_ref().map(clone_value)
    }

    fn deep_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Some(left), Some(right)) => values_equal(left, right),
            (left, right) => left.is_none() && right.is_none(),
        }
    }

    fn deep_debug(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Some(value) => {
                f.write_str("Some(")?;
                value.deep_debug(f)?;
                f.write_str(")")
            }
            None => f.write_str("None"),
        }
    }
}

/// An object's members with the position or seq they were found at.
impl DeepJson for (u64, Map<String, Value>) {
    fn into_values(self, out: &mut Vec<Value>) {
        out.push(Value::Object(self.1));
    }

    fn deep_clone(&self) -> Self {
        (self.0, clone_fields(&self.1))
    }

    fn deep_eq(&self, other: &Self) -> bool {
        self.0 == other.0 && fields_equal(&self.1, &other.1)
    }

    fn deep_debug(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({}, ", self.0)?;
        self.1.deep_debug(f)?;
        f.write_str(")")
    }
}

impl<T: DeepJson> DeepJson for Vec<T> {
    fn into_values(self, out: &mut Vec<Value>) {
        for item in self {
            item.into_values(out);
        }
    }

    fn deep_clone(&self) -> Self {
        self.iter().map(DeepJson::deep_clone).collect()
    }

    fn deep_eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .iter()
                .zip(other)
                .all(|(left, right)| left.deep_eq(right))
    }

    fn deep_debug(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[")?;
        for (index, item) in self.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            item.deep_debug(f)?;
        }
        f.write_str("]")
    }
}

/// Owned JSON whose `Drop`, `Clone`, `PartialEq`, and `Debug` never recurse
/// over its nesting. Every crate-internal structure that keeps parsed Session
/// JSON holds it in one of these, so a payload nested a million levels deep is safe to
/// keep, copy, compare, and drop on a small stack.
pub(crate) struct Deep<T: DeepJson>(T);

impl<T: DeepJson> Deep<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self(value)
    }

    /// The JSON, in a `const` context.
    pub(crate) const fn as_inner(&self) -> &T {
        &self.0
    }

    /// The JSON itself, which the caller now drops or keeps.
    pub(crate) fn into_inner(mut self) -> T {
        std::mem::take(&mut self.0)
    }
}

impl<T: DeepJson> Drop for Deep<T> {
    fn drop(&mut self) {
        let mut values = Vec::new();
        std::mem::take(&mut self.0).into_values(&mut values);
        values.into_iter().for_each(dismantle);
    }
}

impl<T: DeepJson> Clone for Deep<T> {
    fn clone(&self) -> Self {
        Self(self.0.deep_clone())
    }
}

impl<T: DeepJson> PartialEq for Deep<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0.deep_eq(&other.0)
    }
}

impl<T: DeepJson + Eq> Eq for Deep<T> {}

impl<'a, T: DeepJson> IntoIterator for &'a Deep<Vec<T>> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<T: DeepJson> FromIterator<T> for Deep<Vec<T>> {
    fn from_iter<I: IntoIterator<Item = T>>(items: I) -> Self {
        Self(items.into_iter().collect())
    }
}

impl<T: DeepJson> Default for Deep<T> {
    fn default() -> Self {
        Self(T::default())
    }
}

impl<T: DeepJson> std::fmt::Debug for Deep<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.deep_debug(f)
    }
}

impl<T: DeepJson> std::ops::Deref for Deep<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: DeepJson> std::ops::DerefMut for Deep<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

/// [`DeepJson::deep_debug`] as a `Debug` value, for a field of a manual
/// `Debug` implementation.
pub(crate) struct DebugJson<'a, T: DeepJson>(pub(crate) &'a T);

impl<T: DeepJson> std::fmt::Debug for DebugJson<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.deep_debug(f)
    }
}

/// `Drop`, `Clone`, `PartialEq`, and `Debug` for a public migration result
/// whose `header: Value`, `events: Vec<Value>`, and
/// `inherited_event_count: u64` hold rows a parse may have nested
/// arbitrarily deep; none of them recurses over that nesting. `Debug` writes
/// the JSON as [`crate::json_text`] does.
macro_rules! deep_session_parts {
    ($name:ident) => {
        impl Drop for $name {
            fn drop(&mut self) {
                $crate::json_parse::dismantle(std::mem::take(&mut self.header));
                std::mem::take(&mut self.events)
                    .into_iter()
                    .for_each($crate::json_parse::dismantle);
            }
        }

        impl Clone for $name {
            fn clone(&self) -> Self {
                Self {
                    header: $crate::json_parse::clone_value(&self.header),
                    events: self
                        .events
                        .iter()
                        .map($crate::json_parse::clone_value)
                        .collect(),
                    inherited_event_count: self.inherited_event_count,
                }
            }
        }

        impl PartialEq for $name {
            fn eq(&self, other: &Self) -> bool {
                self.inherited_event_count == other.inherited_event_count
                    && $crate::json_parse::values_equal(&self.header, &other.header)
                    && self.events.len() == other.events.len()
                    && self
                        .events
                        .iter()
                        .zip(&other.events)
                        .all(|(left, right)| $crate::json_parse::values_equal(left, right))
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($name))
                    .field("header", &$crate::json_parse::DebugJson(&self.header))
                    .field("events", &$crate::json_parse::DebugJson(&self.events))
                    .field("inherited_event_count", &self.inherited_event_count)
                    .finish()
            }
        }
    };
}

pub(crate) use deep_session_parts;

#[cfg(test)]
mod tests {
    use super::*;

    /// serde_json 1.0.151 error codes for input that violates the JSON
    /// grammar, which `JSON.parse` also rejects; any other serde_json error
    /// decided nothing. The reader used them before this parser replaced it.
    const SERDE_SYNTAX_ERRORS: [&str; 15] = [
        "EOF while parsing a list",
        "EOF while parsing an object",
        "EOF while parsing a string",
        "EOF while parsing a value",
        "expected `:`",
        "expected `,` or `]`",
        "expected `,` or `}`",
        "expected ident",
        "expected value",
        "invalid escape",
        "invalid number",
        "control character (\\u0000-\\u001F) found while parsing a string",
        "key must be a string",
        "trailing comma",
        "trailing characters",
    ];

    fn serde_outcome(text: &str) -> Result<Value, JsonParseError> {
        serde_json::from_str(text).map_err(|error: serde_json::Error| {
            let message = error.to_string();
            let code = message
                .split_once(" at line ")
                .map_or(message.as_str(), |(code, _)| code);
            match code {
                code if SERDE_SYNTAX_ERRORS.contains(&code) => JsonParseError::Syntax,
                "number out of range" => JsonParseError::NumberOutOfRange,
                "lone leading surrogate in hex escape" | "unexpected end of hex escape" => {
                    JsonParseError::LoneSurrogate
                }
                other => panic!("{text:?}: unclassified serde_json error {other:?}"),
            }
        })
    }

    /// Same value, member order, and number representation, or the same refusal.
    fn assert_matches_serde(text: &str) {
        let ours = parse_json(text);
        let theirs = serde_outcome(text);
        match (&ours, &theirs) {
            (Ok(ours), Ok(theirs)) => assert_eq!(
                serde_json::to_string(ours).unwrap(),
                serde_json::to_string(theirs).unwrap(),
                "{text:?}"
            ),
            _ => assert_eq!(ours, theirs, "{text:?}"),
        }
    }

    #[test]
    fn values_and_refusals_match_serde_json() {
        let cases = [
            "",
            " ",
            "null",
            " true ",
            "false",
            "nul",
            "tru",
            "[",
            "]",
            "{",
            "}",
            "[]",
            "[ ]",
            "{ }",
            "[1,]",
            "[,1]",
            "[1 2]",
            "{\"a\"}",
            "{\"a\":}",
            "{\"a\":1,}",
            "{1:2}",
            "{\"a\":1 \"b\":2}",
            "[1]x",
            "[1] ",
            "\t\n\r[1]\r\n",
            "\u{c}1",
            "\u{a0}1",
            "1\u{b}",
            "0",
            "-0",
            "-",
            "-a",
            "01",
            "-01",
            "1.",
            "1.e1",
            ".5",
            "1e",
            "1e+",
            "1E-2",
            "1.0",
            "1e5",
            "100",
            "-1",
            "1.5",
            "0.1",
            "0.10",
            "5e-324",
            "2e-324",
            "1e-400",
            "-1e-400",
            "1e308",
            "1.8e308",
            "1e400",
            "-1e400",
            "0e99999999999999999999",
            "1e99999999999999999999",
            "1e-99999999999999999999",
            "9007199254740993",
            "18446744073709551615",
            "18446744073709551616",
            "-9223372036854775808",
            "-9223372036854775809",
            "-18446744073709551615",
            "-18446744073709551616",
            "123456789012345678901234567890",
            "0.30000000000000004",
            "2.2250738585072011e-308",
            "\"\"",
            "\"a",
            "\"\\\"",
            "\"\\q\"",
            "\"\\/\\b\\f\\n\\r\\t\\\\\\\"\"",
            "\"\u{1}\"",
            "\"\u{7f}\u{2028}é😀\"",
            "\"\\u0041\\u00e9\"",
            "\"\\u004\"",
            "\"\\u00G1\"",
            "\"\\uD83D\\uDE00\"",
            "\"\\ud83d\\ude00\"",
            "\"\\uD800\"",
            "\"\\uDC00\"",
            "\"\\uD800x\"",
            "\"\\uD800\\n\"",
            "\"\\uD800\\u0041\"",
            "\"\\uD800\\uD800\"",
            "\"\\uD800\\",
            "\"\\uD800",
            "\"\\uD800\\u12",
            "\"\\uDC00\\q\"",
            "\"\\u0000\"",
            "{\"a\":1,\"b\":2,\"a\":3}",
            "{\"__proto__\":1,\"b\":[]}",
            "{\"2\":1,\"1\":2,\"b\":3}",
            "{\"a\":{\"b\":[1,{\"c\":null}]},\"d\":\"e\"}",
            "[[[]],[{}],{\"a\":[]}]",
            "[\"\\uD800\", {]",
            "[{], \"\\uD800\"]",
            "[1e400, }",
            "[}, 1e400]",
        ];
        for text in cases {
            assert_matches_serde(text);
        }
        let long_fraction = format!("0.{}1", "0".repeat(800));
        let long_integer = format!("1{}", "0".repeat(700));
        assert_matches_serde(&long_fraction);
        assert_matches_serde(&long_integer);
    }

    #[test]
    fn values_match_serde_json_below_its_recursion_limit() {
        for depth in [126, 127] {
            for (open, close) in [("[", "]"), ("{\"a\":", "}")] {
                let empty = if open == "[" { "[]" } else { "{}" };
                let text = format!(
                    "{}{empty}{}",
                    open.repeat(depth - 1),
                    close.repeat(depth - 1)
                );
                assert_matches_serde(&text);
                let torn = format!("{}{empty}", open.repeat(depth - 1));
                assert_matches_serde(&torn);
            }
        }
        let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
        assert!(parse_json(&deep).is_ok());
    }

    #[test]
    fn walks_copy_compare_and_drop_deep_values_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let depth = 1_000_000;
                let nested = |middle: &str| {
                    format!("{}{middle}{}", "[{\"a\":".repeat(depth), "}]".repeat(depth))
                };
                let value = parse_json(&nested("null")).expect("deep JSON");
                let copy = clone_value(&value);
                assert!(values_equal(&value, &copy));
                let other = parse_json(&nested("1")).expect("deep JSON");
                assert!(!values_equal(&value, &other));
                dismantle(value);
                dismantle(copy);
                dismantle(other);
                assert_eq!(parse_json(&"[".repeat(depth)), Err(JsonParseError::Syntax));
            })
            .expect("spawn")
            .join()
            .expect("no stack overflow");
    }
}
