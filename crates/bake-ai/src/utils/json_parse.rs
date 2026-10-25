//! Lenient JSON for provider events and streamed tool arguments.
//!
//! Ported from Pi `packages/ai/src/utils/json-parse.ts` (v1.1.0) and the
//! `partial-json` 0.1.7 parser it calls (MIT, Copyright (c) 2023
//! Promplate; see `NOTICE`). JavaScript strings index UTF-16 code units; the
//! ported scanners index Unicode scalar values, which only differs inside
//! characters the scanners never inspect.
//!
//! Rust strings cannot hold the unpaired surrogates `JSON.parse` keeps, and
//! `serde_json` refuses their escapes. [`repair_json`] therefore also drops an
//! unpaired `\uD800`–`\uDFFF` escape, which is what Pi's outgoing
//! `sanitizeSurrogates` would later remove from that text.
//!
//! `NaN` and `Infinity`, which the partial parser accepts, become `null`
//! because JSON has no such values.
//!
//! The partial parser recurses once per nested object or array, so it stops
//! at [`MAX_PARTIAL_DEPTH`] levels and fails the whole parse. That stands in
//! for the `RangeError` a JavaScript stack overflow raises, which Pi's
//! `parseStreamingJson` catches and turns into `{}`; provider bytes can then
//! never overflow a thread stack. The cap matches `serde_json`'s own limit.

use serde_json::Value;

use crate::types::JsonObject;

fn is_hex4(chars: &[char]) -> bool {
    chars.len() == 4 && chars.iter().all(char::is_ascii_hexdigit)
}

fn hex4_value(chars: &[char]) -> Option<u32> {
    if !is_hex4(chars) {
        return None;
    }
    let text: String = chars.iter().collect();
    u32::from_str_radix(&text, 16).ok()
}

/// Repairs malformed JSON string literals: raw control characters inside
/// strings are escaped, a backslash before an invalid escape character is
/// doubled, and an unpaired surrogate escape is dropped.
pub fn repair_json(json: &str) -> String {
    rewrite_strings(json, true)
}

/// Parses JSON as JavaScript's `JSON.parse` accepts it. `serde_json` refuses
/// unpaired surrogate escapes, which `JSON.parse` keeps; they are dropped
/// first, as in [`repair_json`]. Nothing else is repaired.
pub fn parse_json_strict(json: &str) -> Result<Value, serde_json::Error> {
    match serde_json::from_str(json) {
        Ok(value) => Ok(value),
        Err(error) => {
            let rewritten = rewrite_strings(json, false);
            if rewritten != json {
                serde_json::from_str(&rewritten)
            } else {
                Err(error)
            }
        }
    }
}

/// Drops unpaired surrogate escapes inside strings and, when `repair` is
/// set, also escapes raw control characters and invalid escapes.
fn rewrite_strings(json: &str, repair: bool) -> String {
    let chars: Vec<char> = json.chars().collect();
    let mut repaired = String::with_capacity(json.len());
    let mut in_string = false;
    let mut index = 0;
    while let Some(&ch) = chars.get(index) {
        if !in_string {
            repaired.push(ch);
            if ch == '"' {
                in_string = true;
            }
            index += 1;
            continue;
        }
        if ch == '"' {
            repaired.push(ch);
            in_string = false;
            index += 1;
            continue;
        }
        if ch == '\\' {
            let Some(&next) = chars.get(index + 1) else {
                repaired.push_str(if repair { "\\\\" } else { "\\" });
                index += 1;
                continue;
            };
            if next == 'u' {
                let digits = chars.get(index + 2..index + 6).unwrap_or(&[]);
                if let Some(unit) = hex4_value(digits) {
                    let text: String = digits.iter().collect();
                    if (0xD800..0xDC00).contains(&unit) {
                        let low = chars
                            .get(index + 6..index + 12)
                            .filter(|tail| tail.first() == Some(&'\\') && tail.get(1) == Some(&'u'))
                            .and_then(|tail| hex4_value(tail.get(2..6).unwrap_or(&[])));
                        if let Some(low) = low.filter(|low| (0xDC00..0xE000).contains(low)) {
                            repaired.push_str(&format!("\\u{text}\\u{low:04x}"));
                            index += 12;
                        } else {
                            index += 6;
                        }
                        continue;
                    }
                    if (0xDC00..0xE000).contains(&unit) {
                        index += 6;
                        continue;
                    }
                    repaired.push_str("\\u");
                    repaired.push_str(&text);
                    index += 6;
                    continue;
                }
            }
            if !repair || matches!(next, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u') {
                repaired.push('\\');
                repaired.push(next);
                index += 2;
                continue;
            }
            repaired.push_str("\\\\");
            index += 1;
            continue;
        }
        if repair && (ch as u32) <= 0x1f {
            match ch {
                '\u{8}' => repaired.push_str("\\b"),
                '\u{c}' => repaired.push_str("\\f"),
                '\n' => repaired.push_str("\\n"),
                '\r' => repaired.push_str("\\r"),
                '\t' => repaired.push_str("\\t"),
                _ => repaired.push_str(&format!("\\u{:04x}", ch as u32)),
            }
        } else {
            repaired.push(ch);
        }
        index += 1;
    }
    repaired
}

/// Parses JSON, retrying once with [`repair_json`] when that changes the text.
pub fn parse_json_with_repair(json: &str) -> Result<Value, serde_json::Error> {
    match serde_json::from_str(json) {
        Ok(value) => Ok(value),
        Err(error) => {
            let repaired = repair_json(json);
            if repaired != json {
                serde_json::from_str(&repaired)
            } else {
                Err(error)
            }
        }
    }
}

/// Parses possibly incomplete JSON from a stream. Never fails: unparseable
/// text yields an empty object.
pub fn parse_streaming_json(partial_json: Option<&str>) -> Value {
    let Some(text) = partial_json.filter(|text| !text.trim().is_empty()) else {
        return Value::Object(JsonObject::new());
    };
    if let Ok(value) = parse_json_with_repair(text) {
        return value;
    }
    let partial = parse_partial(text).or_else(|_| parse_partial(&repair_json(text)));
    match partial {
        Ok(Value::Null) | Err(_) => Value::Object(JsonObject::new()),
        Ok(value) => value,
    }
}

/// [`parse_streaming_json`] narrowed to an object, as tool arguments are.
pub fn parse_streaming_json_object(partial_json: Option<&str>) -> JsonObject {
    match parse_streaming_json(partial_json) {
        Value::Object(object) => object,
        _ => JsonObject::new(),
    }
}

/// Streamed tool arguments: the text so far and its
/// [`parse_streaming_json_object`] result, kept current as deltas arrive.
///
/// Parsing the whole text again after every delta makes a long argument,
/// such as a file being written, cost the square of its length. While the
/// text ends inside a string value, a delta that only continues that string
/// is decoded onto the previous result instead. The partial parser reads
/// malformed text before the string differently as the text grows, so this
/// starts only after a full parse that strict JSON confirms. A delta with a
/// quote, a raw control character, an escape cut short or invalid, or a
/// surrogate escape parses the whole text again. What strict JSON refuses
/// stays in the text, so after a refusal the text is checked again only
/// once it has doubled, keeping the checks' cost linear.
#[derive(Debug, Clone, Default)]
pub(crate) struct StreamingJsonObject {
    text: String,
    object: JsonObject,
    open: Option<OpenString>,
    /// The text length before which strict JSON is not asked again.
    recheck_at: usize,
}

/// The string value a [`StreamingJsonObject`]'s text ends in.
#[derive(Debug, Clone)]
struct OpenString {
    path: Vec<Step>,
    /// The text's trailing whitespace, which the parse trims until
    /// something follows it.
    pending: String,
}

#[derive(Debug, Clone)]
enum Step {
    Member(String),
    Item(usize),
}

/// Ends the open string while [`confirmed_open_string`] checks it. A string
/// whose own text ends in it only makes the check fail.
const MARK: char = '\u{FFFF}';

impl StreamingJsonObject {
    /// Arguments whose text starts as `text`.
    pub fn new(text: String) -> Self {
        let mut arguments = Self {
            text,
            ..Self::default()
        };
        arguments.parse();
        arguments
    }

    /// The text so far.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The text so far, ending the stream.
    pub fn into_text(self) -> String {
        self.text
    }

    /// [`parse_streaming_json_object`] of the text so far.
    pub fn object(&self) -> &JsonObject {
        &self.object
    }

    /// Appends `delta` to the text.
    pub fn push(&mut self, delta: &str) {
        self.text.push_str(delta);
        if !self.continue_open_string(delta) {
            self.parse();
        }
    }

    fn parse(&mut self) {
        self.object = parse_streaming_json_object(Some(&self.text));
        self.open = None;
        if self.text.len() < self.recheck_at {
            return;
        }
        let Some((closed, pending)) = closed_at_open_string(&self.text) else {
            return;
        };
        self.open = confirmed_open_string(&closed, &self.object, pending);
        if self.open.is_none() {
            self.recheck_at = self.text.len().saturating_mul(2);
        }
    }

    fn continue_open_string(&mut self, delta: &str) -> bool {
        let Some(open) = &mut self.open else {
            return false;
        };
        let Some((added, pending)) = continue_string(&open.pending, delta) else {
            return false;
        };
        let Some(Value::String(value)) = member_at(&mut self.object, &open.path) else {
            return false;
        };
        value.push_str(&added);
        open.pending = pending;
        true
    }
}

/// `delta` read as more of a string literal whose text so far ends in the
/// whitespace `pending`: what it adds to the string's value, and the
/// trailing whitespace then held back. `None` for a delta that could end
/// the string or does not decode on its own.
fn continue_string(pending: &str, delta: &str) -> Option<(String, String)> {
    let mut added = String::new();
    let mut held = pending.to_owned();
    let mut chars = delta.chars();
    while let Some(ch) = chars.next() {
        let decoded = match ch {
            '"' => return None,
            '\\' => match chars.next()? {
                '"' => '"',
                '\\' => '\\',
                '/' => '/',
                'b' => '\u{8}',
                'f' => '\u{c}',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                'u' => {
                    let digits: Vec<char> = chars.by_ref().take(4).collect();
                    // `None` for a surrogate, which may pair across deltas.
                    char::from_u32(hex4_value(&digits)?)?
                }
                _ => return None,
            },
            ch if (ch as u32) <= 0x1f => return None,
            // `str::trim`, which the partial parser applies, drops these
            // from the end of the text.
            ch if ch.is_whitespace() => {
                held.push(ch);
                continue;
            }
            ch => ch,
        };
        added.push_str(&held);
        held.clear();
        added.push(decoded);
    }
    Some((added, held))
}

fn member_at<'a>(object: &'a mut JsonObject, path: &[Step]) -> Option<&'a mut Value> {
    let (Step::Member(key), rest) = path.split_first()? else {
        return None;
    };
    let mut value = object.get_mut(key)?;
    for step in rest {
        value = match (step, value) {
            (Step::Member(key), Value::Object(members)) => members.get_mut(key)?,
            (Step::Item(index), Value::Array(items)) => items.get_mut(*index)?,
            _ => return None,
        };
    }
    Some(value)
}

/// When `text` ends inside a string value, outside an escape, the text as
/// the partial parser trims it with that string ended by [`MARK`] and its
/// containers closed, and the trailing whitespace the trim removed.
fn closed_at_open_string(text: &str) -> Option<(String, &str)> {
    let trimmed = text.trim_end();
    let pending = text.get(trimmed.len()..)?;
    if pending.chars().any(|ch| (ch as u32) <= 0x1f) {
        return None;
    }
    let closers = open_value_string_closers(trimmed)?;
    let mut closed = String::with_capacity(trimmed.len() + closers.len() + 4);
    closed.push_str(trimmed);
    closed.push(MARK);
    closed.push('"');
    closed.push_str(&closers);
    Some((closed, pending))
}

/// The string [`closed_at_open_string`] marked, when strict JSON reads
/// `closed` as `object` holds it.
fn confirmed_open_string(closed: &str, object: &JsonObject, pending: &str) -> Option<OpenString> {
    let mut strict: Value = serde_json::from_str(closed).ok()?;
    let mut marked = Vec::new();
    unmark(&mut strict, &mut Vec::new(), &mut marked);
    let [path] = <[Vec<Step>; 1]>::try_from(marked).ok()?;
    (strict.as_object() == Some(object)).then(|| OpenString {
        path,
        pending: pending.to_owned(),
    })
}

/// The brackets that close `text`'s objects and arrays, when it ends inside
/// a string that is a value, not a key, outside an escape, and not after
/// the first half of a surrogate pair, which the next delta may complete.
fn open_value_string_closers(text: &str) -> Option<String> {
    // Each open container's closer, and for an object whether a key is next.
    let mut open: Vec<(char, bool)> = Vec::new();
    let (mut in_string, mut is_key, mut escaped) = (false, false, false);
    // A `\u` escape's characters still to come, its code unit so far, and
    // whether the last character ended a high surrogate.
    let (mut digits, mut unit, mut high) = (0, 0, false);
    for ch in text.chars() {
        if in_string {
            high = false;
            match ch {
                _ if digits > 0 => {
                    digits -= 1;
                    unit = unit << 4 | ch.to_digit(16).unwrap_or(0);
                    high = digits == 0 && (0xD800..0xDC00).contains(&unit);
                }
                _ if escaped => {
                    escaped = false;
                    digits = if ch == 'u' { 4 } else { 0 };
                    unit = 0;
                }
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                is_key = matches!(open.last(), Some(('}', true)));
            }
            '{' => open.push(('}', true)),
            '[' => open.push((']', false)),
            '}' | ']' => {
                open.pop();
            }
            ':' | ',' => {
                if let Some(('}', key_next)) = open.last_mut() {
                    *key_next = ch == ',';
                }
            }
            _ => {}
        }
    }
    (in_string && !is_key && !escaped && digits == 0 && !high)
        .then(|| open.iter().rev().map(|(closer, _)| closer).collect())
}

/// Removes [`MARK`] from the end of each string in `value` that has it,
/// collecting the way to each from `path`.
fn unmark(value: &mut Value, path: &mut Vec<Step>, marked: &mut Vec<Vec<Step>>) {
    match value {
        Value::String(text) => {
            if text.ends_with(MARK) {
                text.pop();
                marked.push(path.clone());
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                path.push(Step::Item(index));
                unmark(item, path, marked);
                path.pop();
            }
        }
        Value::Object(members) => {
            for (key, member) in members.iter_mut() {
                path.push(Step::Member(key.clone()));
                unmark(member, path, marked);
                path.pop();
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// Why the partial parser stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialJsonError(pub String);

/// The deepest nesting of objects and arrays [`parse_partial`] accepts.
pub const MAX_PARTIAL_DEPTH: usize = 128;

/// The `partial-json` parser with every partial kind allowed (`Allow.ALL`).
///
/// Input nested deeper than [`MAX_PARTIAL_DEPTH`] fails as a whole, even
/// where a shallower prefix would have parsed.
pub fn parse_partial(json: &str) -> Result<Value, PartialJsonError> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Err(PartialJsonError(format!("{json} is empty")));
    }
    let chars: Vec<char> = trimmed.chars().collect();
    let mut parser = PartialParser {
        chars,
        index: 0,
        depth: 0,
        too_deep: false,
    };
    let result = parser.parse_any();
    if parser.too_deep {
        return Err(PartialJsonError(format!(
            "nesting deeper than {MAX_PARTIAL_DEPTH} levels"
        )));
    }
    result
}

struct PartialParser {
    chars: Vec<char>,
    index: usize,
    /// Open objects and arrays around the current position.
    depth: usize,
    /// Set once nesting passes [`MAX_PARTIAL_DEPTH`]; the enclosing levels
    /// catch errors and return partial values, so the flag carries the
    /// failure out to [`parse_partial`].
    too_deep: bool,
}

/// JavaScript `String.prototype.substring` over `chars`: bounds clamp and
/// swap.
fn js_substring(chars: &[char], start: isize, end: isize) -> String {
    let len = chars.len() as isize;
    let clamp = |value: isize| value.clamp(0, len) as usize;
    let (a, b) = (clamp(start), clamp(end));
    let (from, to) = if a <= b { (a, b) } else { (b, a) };
    chars
        .get(from..to)
        .map(|slice| slice.iter().collect())
        .unwrap_or_default()
}

fn last_index_of(chars: &[char], needle: char) -> isize {
    chars
        .iter()
        .rposition(|ch| *ch == needle)
        .map_or(-1, |position| position as isize)
}

fn json_parse(text: &str) -> Result<Value, PartialJsonError> {
    serde_json::from_str::<Value>(text).map_err(|error| PartialJsonError(error.to_string()))
}

impl PartialParser {
    fn len(&self) -> usize {
        self.chars.len()
    }

    fn at(&self, index: usize) -> Option<char> {
        self.chars.get(index).copied()
    }

    fn rest_is_prefix_of(&self, word: &str) -> bool {
        let remaining = self.len().saturating_sub(self.index);
        if remaining >= word.chars().count() {
            return false;
        }
        let rest: String = self.chars.get(self.index..).unwrap_or(&[]).iter().collect();
        word.starts_with(&rest)
    }

    fn starts_with_word(&self, word: &str) -> bool {
        let count = word.chars().count();
        self.chars
            .get(self.index..self.index + count)
            .is_some_and(|slice| slice.iter().copied().eq(word.chars()))
    }

    fn partial(&self, message: &str) -> PartialJsonError {
        PartialJsonError(format!("{message} at position {}", self.index))
    }

    fn skip_blank(&mut self) {
        while matches!(self.at(self.index), Some(' ' | '\n' | '\r' | '\t')) {
            self.index += 1;
        }
    }

    fn parse_any(&mut self) -> Result<Value, PartialJsonError> {
        self.skip_blank();
        if self.index >= self.len() {
            return Err(self.partial("Unexpected end of input"));
        }
        match self.at(self.index) {
            Some('"') => return self.parse_str(),
            Some(open @ ('{' | '[')) => {
                if self.too_deep || self.depth >= MAX_PARTIAL_DEPTH {
                    self.too_deep = true;
                    return Err(self.partial("Maximum nesting depth exceeded"));
                }
                self.depth += 1;
                let result = if open == '{' {
                    self.parse_obj()
                } else {
                    self.parse_arr()
                };
                self.depth -= 1;
                return result;
            }
            _ => {}
        }
        for (word, value) in [
            ("null", Value::Null),
            ("true", Value::Bool(true)),
            ("false", Value::Bool(false)),
            ("Infinity", Value::Null),
        ] {
            if self.starts_with_word(word) || self.rest_is_prefix_of(word) {
                self.index += word.chars().count();
                return Ok(value);
            }
        }
        let remaining = self.len().saturating_sub(self.index);
        if self.starts_with_word("-Infinity")
            || (remaining > 1 && self.rest_is_prefix_of("-Infinity"))
        {
            self.index += 9;
            return Ok(Value::Null);
        }
        if self.starts_with_word("NaN") || self.rest_is_prefix_of("NaN") {
            self.index += 3;
            return Ok(Value::Null);
        }
        self.parse_num()
    }

    fn parse_str(&mut self) -> Result<Value, PartialJsonError> {
        let start = self.index;
        let mut escape = false;
        self.index += 1;
        while let Some(ch) = self.at(self.index) {
            let previous_is_backslash = self.index > 0 && self.at(self.index - 1) == Some('\\');
            if ch == '"' && !(escape && previous_is_backslash) {
                break;
            }
            escape = if ch == '\\' { !escape } else { false };
            self.index += 1;
        }
        if self.at(self.index) == Some('"') {
            self.index += 1;
            let end = self.index as isize - isize::from(escape);
            return json_parse(&js_substring(&self.chars, start as isize, end)).map_err(|error| {
                PartialJsonError(format!("{} at position {}", error.0, self.index))
            });
        }
        let end = self.index as isize - isize::from(escape);
        let mut text = js_substring(&self.chars, start as isize, end);
        text.push('"');
        match json_parse(&text) {
            Ok(value) => Ok(value),
            Err(_) => {
                let mut text = js_substring(
                    &self.chars,
                    start as isize,
                    last_index_of(&self.chars, '\\'),
                );
                text.push('"');
                json_parse(&text)
            }
        }
    }

    fn parse_obj(&mut self) -> Result<Value, PartialJsonError> {
        self.index += 1;
        self.skip_blank();
        let mut object = JsonObject::new();
        while self.at(self.index) != Some('}') {
            self.skip_blank();
            if self.index >= self.len() {
                return Ok(Value::Object(object));
            }
            let key = match self.parse_str() {
                Ok(Value::String(key)) => key,
                Ok(other) => other.to_string(),
                Err(_) => return Ok(Value::Object(object)),
            };
            self.skip_blank();
            self.index += 1;
            match self.parse_any() {
                Ok(value) => {
                    object.insert(key, value);
                }
                Err(_) => return Ok(Value::Object(object)),
            }
            self.skip_blank();
            if self.at(self.index) == Some(',') {
                self.index += 1;
            }
        }
        self.index += 1;
        Ok(Value::Object(object))
    }

    fn parse_arr(&mut self) -> Result<Value, PartialJsonError> {
        self.index += 1;
        let mut array = Vec::new();
        while self.at(self.index) != Some(']') {
            match self.parse_any() {
                Ok(value) => array.push(value),
                Err(_) => return Ok(Value::Array(array)),
            }
            self.skip_blank();
            if self.at(self.index) == Some(',') {
                self.index += 1;
            }
        }
        self.index += 1;
        Ok(Value::Array(array))
    }

    fn parse_num(&mut self) -> Result<Value, PartialJsonError> {
        let whole: String = self.chars.iter().collect();
        if self.index == 0 {
            if whole == "-" {
                return Err(self.partial("Not sure what '-' is"));
            }
            return match json_parse(&whole) {
                Ok(value) => Ok(value),
                Err(error) => {
                    let cut = js_substring(&self.chars, 0, last_index_of(&self.chars, 'e'));
                    json_parse(&cut).map_err(|_| self.partial(&error.0))
                }
            };
        }
        let start = self.index;
        if self.at(self.index) == Some('-') {
            self.index += 1;
        }
        while let Some(ch) = self.at(self.index) {
            if matches!(ch, ',' | ']' | '}') {
                break;
            }
            self.index += 1;
        }
        let text = js_substring(&self.chars, start as isize, self.index as isize);
        match json_parse(&text) {
            Ok(value) => Ok(value),
            Err(error) => {
                if text == "-" {
                    return Err(self.partial("Not sure what '-' is"));
                }
                let cut =
                    js_substring(&self.chars, start as isize, last_index_of(&self.chars, 'e'));
                json_parse(&cut).map_err(|_| self.partial(&error.0))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repairs_invalid_escapes_and_raw_control_characters() {
        // From Pi's anthropic-sse-parsing test "repairs malformed SSE JSON and
        // malformed streamed tool JSON".
        let raw = "{\"path\":\"A\\H\",\"text\":\"col1\tcol2\"}";
        assert_eq!(
            parse_json_with_repair(raw).unwrap(),
            json!({ "path": "A\\H", "text": "col1\tcol2" })
        );
        assert_eq!(repair_json("\"a\\"), "\"a\\\\");
        assert_eq!(repair_json("\"\\u00e9\""), "\"\\u00e9\"");
        assert_eq!(repair_json("{\"a\":\"\u{1}\"}"), "{\"a\":\"\\u0001\"}");
    }

    #[test]
    fn drops_unpaired_surrogate_escapes_and_keeps_pairs() {
        assert_eq!(
            parse_json_with_repair("\"a\\ud83d b\"").unwrap(),
            json!("a b")
        );
        assert_eq!(
            parse_json_with_repair("\"a\\udc00b\"").unwrap(),
            json!("ab")
        );
        assert_eq!(
            parse_json_with_repair("\"\\ud83d\\ude48\"").unwrap(),
            json!("🙈")
        );
    }

    #[test]
    fn strict_parsing_only_drops_unpaired_surrogates() {
        assert_eq!(parse_json_strict("\"a\\ud83d b\"").unwrap(), json!("a b"));
        assert_eq!(
            parse_json_strict("\"\\ud83d\\ude48\\\\u\"").unwrap(),
            json!("🙈\\u")
        );
        assert!(parse_json_strict("{\"a\":\"x\ty\"}").is_err());
        assert!(parse_json_strict("\"A\\H\"").is_err());
        assert!(parse_json_strict("\"a\\").is_err());
    }

    #[test]
    fn parses_incomplete_streaming_json() {
        assert_eq!(parse_streaming_json(None), json!({}));
        assert_eq!(parse_streaming_json(Some("  ")), json!({}));
        assert_eq!(
            parse_streaming_json(Some("{\"path\":\"src/ma")),
            json!({ "path": "src/ma" })
        );
        assert_eq!(
            parse_streaming_json(Some("{\"a\":1,\"b\":[1,2")),
            json!({ "a": 1, "b": [1, 2] })
        );
        assert_eq!(
            parse_streaming_json(Some("{\"a\":tr")),
            json!({ "a": true })
        );
        assert_eq!(
            parse_streaming_json(Some("{\"a\":nu")),
            json!({ "a": null })
        );
        assert_eq!(parse_streaming_json(Some("{\"a\":-")), json!({}));
        assert_eq!(parse_streaming_json(Some("{\"a\":12")), json!({ "a": 12 }));
        assert_eq!(
            parse_streaming_json(Some("{\"a\":1.5e")),
            json!({ "a": 1.5 })
        );
        assert_eq!(
            parse_streaming_json(Some("{\"a\":\"x\\")),
            json!({ "a": "x" })
        );
        // As in Pi, the raw-text partial parse already yields an object, so
        // the repaired text is not tried.
        assert_eq!(
            parse_streaming_json(Some("{\"a\":\"line\nbreak")),
            json!({})
        );
        assert_eq!(parse_streaming_json(Some("{\"a")), json!({}));
        assert_eq!(parse_streaming_json(Some("garbage")), json!({}));
        // Complete JSON parses as is, even when it is not an object.
        assert_eq!(parse_streaming_json(Some("null")), Value::Null);
    }

    #[test]
    fn narrows_non_objects_to_empty_arguments() {
        assert_eq!(
            parse_streaming_json_object(Some("[1,2]")),
            JsonObject::new()
        );
        assert_eq!(
            parse_streaming_json_object(Some("{\"k\":\"v\"}")).get("k"),
            Some(&json!("v"))
        );
    }

    #[test]
    fn partial_parser_never_panics_on_odd_input() {
        for text in [
            "{",
            "[",
            "\"",
            "-",
            "{\"",
            "{\"a\"",
            "{\"a\":",
            "[,",
            "[1,",
            "{]",
            "]",
            "}",
            "{\"a\":[{\"b\":",
            "\\",
            "{\"a\\",
            "-I",
            "-Infin",
            "NaN",
            "Inf",
            "1e",
            "{\"a\":\"\\u12",
            "\u{1F648}\"",
        ] {
            let _ = parse_partial(text);
            let _ = parse_streaming_json(Some(text));
        }
    }

    #[test]
    fn deep_nesting_yields_empty_arguments_instead_of_overflowing() {
        // Pi's parseStreamingJson catches the RangeError a deep partial parse
        // raises and returns {}; the depth cap stands in for that RangeError.
        for open in ["{\"a\":", "[", "{\"a\":["] {
            let deep = open.repeat(200_000);
            assert_eq!(parse_streaming_json(Some(&deep)), json!({}));
            assert!(parse_partial(&deep).is_err());
        }
        // Run the deep case on a quarter of a Tokio worker's 2 MiB stack.
        let handle = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(|| parse_streaming_json(Some(&"[".repeat(MAX_PARTIAL_DEPTH + 1))))
            .unwrap();
        assert_eq!(handle.join().unwrap(), json!({}));
        // Nesting at the cap still parses partially.
        let at_cap = "[".repeat(MAX_PARTIAL_DEPTH);
        assert!(parse_partial(&at_cap).is_ok());
    }

    /// Pushes each piece, checking the object against a full parse of the
    /// text so far and whether a string continues without one.
    fn stream(pieces: &[(&str, bool)]) -> StreamingJsonObject {
        let mut arguments = StreamingJsonObject::default();
        for (piece, continues) in pieces {
            arguments.push(piece);
            assert_eq!(
                arguments.object(),
                &parse_streaming_json_object(Some(arguments.text())),
                "{:?}",
                arguments.text()
            );
            assert_eq!(
                arguments.open.is_some(),
                *continues,
                "{:?}",
                arguments.text()
            );
        }
        arguments
    }

    #[test]
    fn a_streamed_string_value_continues_without_parsing_again() {
        let arguments = stream(&[
            ("{\"path\":\"a.rs\",", false),
            ("\"content\":\"", true),
            ("fn main", true),
            ("() {", true),
            ("\\n", true),
            ("    ", true),
            ("println!(", true),
            ("\\\"hi\\\"", true),
            (");\\n}", true),
            ("  ", true),
            ("\\u00E9\\/", true),
            ("\u{3000}", true),
            ("x", true),
        ]);
        assert_eq!(
            arguments.object()["content"],
            json!("fn main() {\n    println!(\"hi\");\n}  é/\u{3000}x")
        );
        assert_eq!(
            StreamingJsonObject::new("{\"a\":\"b".into()).object()["a"],
            json!("b")
        );
    }

    #[test]
    fn anything_else_parses_the_whole_text_again() {
        stream(&[
            ("{\"a\":[1,{\"b\":\"x", true),
            // An escape cut between deltas.
            ("\\", false),
            ("n", true),
            // A surrogate pair, which pairs across deltas.
            ("\\ud83d", false),
            ("\\ude00", true),
            // An escape cut inside its digits is not a refusal.
            ("\\u00", false),
            ("e9", true),
            // A key is not continued.
            ("\"}],\"c", false),
            ("\":", false),
            (" \"z", true),
            // Nor is a string strict JSON refuses, nor any after it.
            ("\\x", false),
            ("w\",\"d\":\"v", false),
        ]);
        stream(&[
            ("{\"a\":\"x", true),
            // A raw line break the parse trims, then keeps.
            ("\n", false),
            ("y", false),
        ]);
        // `NaN` reads as null, which strict JSON does not.
        stream(&[("{\"n\":NaN,\"a\":\"x", false), ("y", false)]);
        stream(&[("[\"x", false), ("y", false)]);
    }

    mod properties {
        use proptest::prelude::*;

        use super::*;

        /// JSON values as tool arguments carry them: unicode, quotes,
        /// backslashes, and control characters in strings and keys, and
        /// integers and finite floats.
        fn value() -> impl Strategy<Value = Value> {
            let text = "(\\PC|[\"\\\\/\u{0}-\u{1f}]){0,8}";
            let leaf = prop_oneof![
                Just(Value::Null),
                any::<bool>().prop_map(Value::Bool),
                any::<i64>().prop_map(Value::from),
                any::<f64>()
                    .prop_filter("finite", |n| n.is_finite())
                    .prop_map(Value::from),
                text.prop_map(Value::String),
            ];
            leaf.prop_recursive(3, 24, 4, move |inner| {
                prop_oneof![
                    prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
                    prop::collection::vec((text, inner), 0..4)
                        .prop_map(|members| Value::Object(members.into_iter().collect())),
                ]
            })
        }

        fn object() -> impl Strategy<Value = Value> {
            prop::collection::vec(("[a-z\u{e9}\u{1F648}]{0,4}", value()), 0..4)
                .prop_map(|members| Value::Object(members.into_iter().collect()))
        }

        /// `text`, JSON, with the characters of its strings escaped in the
        /// other ways JSON allows as `choices` pick: `\uXXXX` in either case,
        /// as a surrogate pair beyond the basic plane, and `\/`.
        fn escaped_differently(text: &str, choices: &[u8]) -> String {
            let mut out = String::new();
            let mut choices = choices.iter().cycle();
            let (mut in_string, mut escaped) = (false, false);
            // The hex digits of a `\u` escape still to copy.
            let mut digits = 0;
            for ch in text.chars() {
                if !in_string || escaped || digits > 0 || matches!(ch, '\\' | '"') {
                    out.push(ch);
                    if !in_string {
                        in_string = ch == '"';
                    } else if digits > 0 {
                        digits -= 1;
                    } else if escaped {
                        escaped = false;
                        digits = if ch == 'u' { 4 } else { 0 };
                    } else {
                        escaped = ch == '\\';
                        in_string = ch != '"';
                    }
                    continue;
                }
                let mut units = [0u16; 2];
                match choices.next().copied().unwrap_or(0) {
                    0 | 1 => {
                        for unit in ch.encode_utf16(&mut units) {
                            out.push_str(&format!("\\u{unit:04x}"));
                        }
                    }
                    2 => {
                        for unit in ch.encode_utf16(&mut units) {
                            out.push_str(&format!("\\u{unit:04X}"));
                        }
                    }
                    3 if ch == '/' => out.push_str("\\/"),
                    _ => out.push(ch),
                }
            }
            out
        }

        /// `text` cut into deltas of the given lengths in characters, in turn.
        fn deltas<'a>(text: &'a str, lengths: &[usize]) -> Vec<&'a str> {
            let bounds: Vec<usize> = text
                .char_indices()
                .map(|(index, _)| index)
                .chain([text.len()])
                .collect();
            let mut pieces = Vec::new();
            let (mut at, mut lengths) = (0, lengths.iter().cycle());
            while at + 1 < bounds.len() {
                let next = (at + lengths.next().copied().unwrap_or(1)).min(bounds.len() - 1);
                pieces.push(&text[bounds[at]..bounds[next]]);
                at = next;
            }
            pieces
        }

        /// Whether `partial`, read from a prefix of `full`'s text, holds only
        /// what `full` holds: a prefix of its members or items, each equal
        /// but the last, which is itself partial; a string's prefix; any
        /// number, since a cut number reads as a shorter one.
        fn is_partial_of(partial: &Value, full: &Value) -> bool {
            match (partial, full) {
                (Value::Object(partial), Value::Object(full)) => {
                    partial.len() <= full.len()
                        && partial.iter().zip(full).enumerate().all(
                            |(index, ((key, value), (full_key, full_value)))| {
                                key == full_key
                                    && if index + 1 == partial.len() {
                                        is_partial_of(value, full_value)
                                    } else {
                                        value == full_value
                                    }
                            },
                        )
                }
                (Value::Array(partial), Value::Array(full)) => {
                    partial.len() <= full.len()
                        && partial
                            .iter()
                            .zip(full)
                            .enumerate()
                            .all(|(index, (item, full_item))| {
                                if index + 1 == partial.len() {
                                    is_partial_of(item, full_item)
                                } else {
                                    item == full_item
                                }
                            })
                }
                (Value::String(partial), Value::String(full)) => full.starts_with(partial.as_str()),
                (Value::Number(_), Value::Number(_)) => true,
                _ => partial == full,
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn the_partial_parser_reads_complete_json_as_json_does(
                full in value(),
                pretty in any::<bool>(),
            ) {
                let text = if pretty {
                    serde_json::to_string_pretty(&full).unwrap()
                } else {
                    serde_json::to_string(&full).unwrap()
                };
                prop_assert_eq!(parse_partial(&text), Ok(full));
            }

        }

        // Each case parses every prefix of its text.
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(96))]

            #[test]
            fn every_streamed_prefix_of_arguments_holds_only_what_the_arguments_hold(
                full in object(),
                pretty in any::<bool>(),
            ) {
                let text = if pretty {
                    serde_json::to_string_pretty(&full).unwrap()
                } else {
                    serde_json::to_string(&full).unwrap()
                };
                for (cut, _) in text.char_indices() {
                    let prefix = &text[..cut];
                    let partial = parse_streaming_json(Some(prefix));
                    prop_assert!(
                        is_partial_of(&partial, &full),
                        "{prefix:?} read as {partial}, not part of {full}"
                    );
                }
                prop_assert_eq!(parse_streaming_json(Some(&text)), full);
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn streamed_arguments_read_as_a_full_parse_of_their_text(
                full in object(),
                pretty in any::<bool>(),
                choices in prop::collection::vec(0u8..8, 1..16),
                lengths in prop::collection::vec(1usize..6, 1..8),
            ) {
                let text = if pretty {
                    serde_json::to_string_pretty(&full).unwrap()
                } else {
                    serde_json::to_string(&full).unwrap()
                };
                let text = escaped_differently(&text, &choices);
                let mut arguments = StreamingJsonObject::default();
                for delta in deltas(&text, &lengths) {
                    arguments.push(delta);
                    prop_assert_eq!(
                        arguments.object(),
                        &parse_streaming_json_object(Some(arguments.text()))
                    );
                }
                prop_assert_eq!(arguments.text(), text.as_str());
                prop_assert_eq!(Value::Object(arguments.object().clone()), full);
            }

            #[test]
            fn any_streamed_text_reads_as_a_full_parse_of_it(
                pieces in prop::collection::vec(
                    prop_oneof![
                        Just("{\"a\":\"".to_owned()),
                        Just("\"b\":[\"".to_owned()),
                        "[a-z \"\\\\{}\\[\\]:,0-9.eE+-]{0,6}",
                        "(\\PC|[\u{0}-\u{1f}\u{85}\u{FFFF}]){0,4}",
                        Just("\\u00e9".to_owned()),
                        Just("\\ud83d".to_owned()),
                        Just("\\ude00".to_owned()),
                        Just("NaN".to_owned()),
                    ],
                    0..16,
                ),
            ) {
                let mut arguments = StreamingJsonObject::default();
                for piece in &pieces {
                    arguments.push(piece);
                    prop_assert_eq!(
                        arguments.object(),
                        &parse_streaming_json_object(Some(arguments.text()))
                    );
                }
            }
        }
    }
}
