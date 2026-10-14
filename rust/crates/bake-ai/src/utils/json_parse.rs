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
}
