//! A bounded, panic-free YAML reader for the 0.3 home's `settings.yaml` and
//! `.credentials.yaml`, which are untrusted input.
//!
//! The TypeScript reads both with the `yaml` package's `parseDocument` and
//! `toJS`: YAML 1.2 core schema, unique keys, no merge keys. This reads them
//! with `serde-saphyr` set the same way (strict booleans, `<<` as an
//! ordinary key, duplicate keys refused) into a `serde_json::Value`, under
//! two sets of bounds:
//!
//! - the parser's own budget: nesting depth, events, nodes, anchors,
//!   aliases, and alias replay;
//! - this module's budget on the value it builds: total string bytes and
//!   nodes, counted as each node is materialized, so an alias of a large
//!   anchor replayed many times stops at the bound instead of filling
//!   memory.
//!
//! Errors carry only a kind and a position. The parser's own messages quote
//! the source, which in the credentials file is a secret, so they are never
//! surfaced.

use std::cell::Cell;
use std::fmt;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use serde_saphyr::options::{AliasLimits, DuplicateKeyPolicy, MergeKeyPolicy};
use serde_saphyr::{Budget, Options};

/// The deepest nesting either document may use. The settings shape needs
/// seven levels; the bound keeps the parser's recursion well inside a
/// 2 MiB thread stack even in a debug build, where each level costs tens of
/// kilobytes.
pub(crate) const MAX_DEPTH: usize = 32;
/// The most nodes the built value may hold.
pub(crate) const MAX_NODES: usize = 100_000;
/// The most alias references a document may make.
pub(crate) const MAX_ALIASES: usize = 1_000;

/// What kind of YAML failure occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YamlErrorKind {
    /// The bytes are not UTF-8.
    NotUtf8,
    /// The text is not one well-formed YAML document.
    Syntax,
    /// The document exceeds a depth, size, node, or alias bound.
    Limit,
    /// A mapping key is not a string.
    NonStringKey,
    /// A mapping repeats a key.
    DuplicateKey,
}

/// A YAML failure: its kind and, when the parser knows it, its position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YamlError {
    /// The failure.
    pub kind: YamlErrorKind,
    /// One-based line and column.
    pub position: Option<(u64, u64)>,
}

impl fmt::Display for YamlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.kind {
            YamlErrorKind::NotUtf8 => "is not UTF-8",
            YamlErrorKind::Syntax => "is not valid YAML",
            YamlErrorKind::Limit => "exceeds the YAML reader's depth, size, or alias limits",
            YamlErrorKind::NonStringKey => "has a mapping key that is not a string",
            YamlErrorKind::DuplicateKey => "repeats a mapping key",
        })?;
        if let Some((line, column)) = self.position {
            write!(f, " at line {line}, column {column}")?;
        }
        Ok(())
    }
}

impl std::error::Error for YamlError {}

/// What the value builder ran out of, or refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Breach {
    Limit,
    NonStringKey,
    DuplicateKey,
}

struct State {
    bytes: Cell<usize>,
    nodes: Cell<usize>,
    breach: Cell<Option<Breach>>,
}

impl State {
    fn fail<E: de::Error>(&self, breach: Breach) -> E {
        self.breach.set(Some(breach));
        E::custom("bounded YAML reader refused the document")
    }

    fn charge<E: de::Error>(&self, bytes: usize) -> Result<(), E> {
        let nodes = self.nodes.get();
        let remaining = self.bytes.get();
        if nodes == 0 || bytes > remaining {
            return Err(self.fail(Breach::Limit));
        }
        self.nodes.set(nodes - 1);
        self.bytes.set(remaining - bytes);
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct ValueSeed<'s> {
    state: &'s State,
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for ValueSeed<'_> {
    type Value = Value;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for ValueSeed<'_> {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a YAML value")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        self.state.charge(0)?;
        Ok(Value::Bool(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        self.state.charge(0)?;
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        self.state.charge(0)?;
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        self.state.charge(0)?;
        // JSON has no spelling for a non-finite number; `toJS` would give
        // `Infinity`, which no field this import reads accepts.
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| self.state.fail(Breach::Limit))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        self.state.charge(value.len())?;
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Value, E> {
        self.state.charge(value.len())?;
        Ok(Value::String(value))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        self.state.charge(0)?;
        Ok(Value::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        self.visit_unit()
    }

    fn visit_some<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        self.deserialize(deserializer)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let child = self.enter()?;
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(child)? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Value, A::Error> {
        let child = self.enter()?;
        let mut map = Map::new();
        while let Some(key) = access.next_key_seed(child)? {
            let Value::String(key) = key else {
                return Err(self.state.fail(Breach::NonStringKey));
            };
            if map.contains_key(&key) {
                return Err(self.state.fail(Breach::DuplicateKey));
            }
            let value = access.next_value_seed(child)?;
            map.insert(key, value);
        }
        Ok(Value::Object(map))
    }
}

impl ValueSeed<'_> {
    fn enter<E: de::Error>(self) -> Result<Self, E> {
        self.state.charge(0)?;
        if self.depth >= MAX_DEPTH {
            return Err(self.state.fail(Breach::Limit));
        }
        Ok(Self {
            state: self.state,
            depth: self.depth + 1,
        })
    }
}

fn options(max_bytes: usize) -> Options {
    let mut budget = Budget::default();
    budget.max_depth = MAX_DEPTH;
    budget.flow_nesting_limit = MAX_DEPTH;
    budget.max_nodes = MAX_NODES;
    budget.max_events = MAX_NODES * 4;
    budget.max_aliases = MAX_ALIASES;
    budget.max_anchors = MAX_ALIASES;
    budget.max_documents = 1;
    budget.max_total_scalar_bytes = max_bytes;
    budget.max_total_comment_bytes = max_bytes;
    budget.max_recorded_anchor_bytes = max_bytes;
    budget.max_recorded_anchor_events = MAX_NODES;
    budget.max_merge_keys = 0;
    let mut alias_limits = AliasLimits::default();
    alias_limits.max_total_replayed_events = MAX_NODES;
    alias_limits.max_replay_stack_depth = MAX_DEPTH;
    let mut options = Options::default();
    options.budget = Some(budget);
    options.alias_limits = alias_limits;
    options.emit_comments = false;
    options.duplicate_keys = DuplicateKeyPolicy::Error;
    options.merge_keys = MergeKeyPolicy::AsOrdinary;
    options.strict_booleans = true;
    options.legacy_octal_numbers = false;
    options.with_snippet = false;
    options
}

/// Parse one YAML document of at most `max_bytes` bytes into JSON values.
/// An empty or comment-only document is `null`, as `toJS` gives.
pub(crate) fn parse_document(bytes: &[u8], max_bytes: usize) -> Result<Value, YamlError> {
    parse_with(bytes, max_bytes, options(max_bytes))
}

fn parse_with(bytes: &[u8], max_bytes: usize, options: Options) -> Result<Value, YamlError> {
    let text = std::str::from_utf8(bytes).map_err(|_| YamlError {
        kind: YamlErrorKind::NotUtf8,
        position: None,
    })?;
    if is_blank_document(text) {
        return Ok(Value::Null);
    }
    let state = State {
        bytes: Cell::new(max_bytes),
        nodes: Cell::new(MAX_NODES),
        breach: Cell::new(None),
    };
    let seed = ValueSeed {
        state: &state,
        depth: 0,
    };
    serde_saphyr::with_deserializer_from_str_with_options(text, options, |de| seed.deserialize(de))
        .map_err(|error| {
            let kind = match state.breach.get() {
                Some(Breach::Limit) => YamlErrorKind::Limit,
                Some(Breach::NonStringKey) => YamlErrorKind::NonStringKey,
                Some(Breach::DuplicateKey) => YamlErrorKind::DuplicateKey,
                // A failure inside an alias's replay arrives as `AliasError`,
                // its cause stringified; the builder accepts every value, so
                // that cause is a parser budget.
                None => match error.without_snippet() {
                    serde_saphyr::Error::Budget { .. }
                    | serde_saphyr::Error::AliasError { .. }
                    | serde_saphyr::Error::AliasReplayCounterOverflow { .. }
                    | serde_saphyr::Error::AliasReplayLimitExceeded { .. }
                    | serde_saphyr::Error::AliasExpansionLimitExceeded { .. }
                    | serde_saphyr::Error::AliasReplayStackDepthExceeded { .. } => {
                        YamlErrorKind::Limit
                    }
                    serde_saphyr::Error::DuplicateMappingKey { .. } => YamlErrorKind::DuplicateKey,
                    _ => YamlErrorKind::Syntax,
                },
            };
            let position = error
                .location()
                .filter(|location| location.line() > 0)
                .map(|location| (location.line(), location.column()));
            YamlError { kind, position }
        })
}

/// Whether a document holds only whitespace, comments, and document
/// markers, which the parser reads as no value at all.
fn is_blank_document(text: &str) -> bool {
    text.trim_start_matches('\u{FEFF}').lines().all(|line| {
        let line = line.trim();
        line.is_empty() || line.starts_with('#') || line == "---" || line == "..."
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CAP: usize = 1 << 20;

    fn kind(text: &str) -> Option<YamlErrorKind> {
        parse_document(text.as_bytes(), CAP)
            .err()
            .map(|error| error.kind)
    }

    #[test]
    fn reads_the_settings_shape_as_the_core_schema_reads_it() {
        let value = parse_document(
            b"llm-pi-ai:\n  providers:\n    cliproxyapi:\n      models:\n        - id: a\n          contextWindow: 272000\n          input: [text, image]\n          reasoningEfforts: {low: low, off: }\n          flag: yes\n          quoted: '12'\n          octal: 0o17\n          on: true\n          '<<': merge\n",
            CAP,
        );
        assert_eq!(
            value,
            Ok(
                json!({ "llm-pi-ai": { "providers": { "cliproxyapi": { "models": [{
                "id": "a", "contextWindow": 272000, "input": ["text", "image"],
                "reasoningEfforts": { "low": "low", "off": null },
                "flag": "yes", "quoted": "12", "octal": 15, "on": true, "<<": "merge",
            }] } } } })
            )
        );
        assert_eq!(parse_document(b"", CAP), Ok(Value::Null));
        assert_eq!(parse_document(b"# only\n---\n", CAP), Ok(Value::Null));
    }

    #[test]
    fn refuses_hostile_documents_with_a_typed_error() {
        assert_eq!(kind("a: [1, 2"), Some(YamlErrorKind::Syntax));
        assert_eq!(kind("a: 1\na: 2\n"), Some(YamlErrorKind::DuplicateKey));
        assert_eq!(kind("1: x\n"), Some(YamlErrorKind::NonStringKey));
        // A second document breaches the one-document budget.
        assert_eq!(kind("a: 1\n---\nb: 2\n"), Some(YamlErrorKind::Limit));
        assert_eq!(
            parse_document(b"\xff\xfe", CAP).map_err(|error| error.kind),
            Err(YamlErrorKind::NotUtf8)
        );
        // Deep nesting, block and flow.
        let deep_flow = format!("a: {}{}", "[".repeat(10_000), "]".repeat(10_000));
        // The scanner refuses deep flow nesting before any node is built,
        // as a syntax error.
        assert!(matches!(
            kind(&deep_flow),
            Some(YamlErrorKind::Limit | YamlErrorKind::Syntax)
        ));
        let mut deep_block = String::new();
        for depth in 0..200 {
            deep_block.push_str(&" ".repeat(depth));
            deep_block.push_str("k:\n");
        }
        assert_eq!(kind(&deep_block), Some(YamlErrorKind::Limit));
        // A billion laughs.
        let mut laughs = String::from(
            "a: &a [\"lol\", \"lol\", \"lol\", \"lol\", \"lol\", \"lol\", \"lol\", \"lol\", \"lol\"]\n",
        );
        for (index, name) in ["b", "c", "d", "e", "f", "g", "h", "i"].iter().enumerate() {
            let previous = ["a", "b", "c", "d", "e", "f", "g", "h"][index];
            laughs.push_str(&format!(
                "{name}: &{name} [{}]\n",
                vec![format!("*{previous}"); 9].join(", ")
            ));
        }
        assert_eq!(kind(&laughs), Some(YamlErrorKind::Limit));
        // One large anchor replayed: bounded by the built value's bytes.
        let big = "x".repeat(200_000);
        let replayed = format!("a: &a \"{big}\"\nb: [{}]\n", vec!["*a"; 50].join(", "));
        assert_eq!(kind(&replayed), Some(YamlErrorKind::Limit));
        // Many scalars: bounded by nodes.
        let wide = format!("a: [{}]\n", vec!["1"; MAX_NODES + 10].join(","));
        assert_eq!(
            parse_document(wide.as_bytes(), wide.len() * 2).map_err(|error| error.kind),
            Err(YamlErrorKind::Limit)
        );
        // An unknown anchor and a cycle-shaped alias.
        assert_eq!(kind("a: *nope\n"), Some(YamlErrorKind::Syntax));
        assert!(kind("a: &a [*a]\n").is_some());
    }

    // The builder's own bounds hold with the parser's budget switched off,
    // so neither layer depends on the other.
    #[test]
    fn bounds_the_built_value_without_the_parser_budget() {
        let unbudgeted = || {
            let mut options = options(CAP);
            options.budget = None;
            let mut alias_limits = AliasLimits::default();
            alias_limits.max_total_replayed_events = usize::MAX;
            options.alias_limits = alias_limits;
            options
        };
        let kind = |text: &str| {
            parse_with(text.as_bytes(), CAP, unbudgeted())
                .err()
                .map(|error| error.kind)
        };
        let mut laughs = String::from("a: &a [lol, lol, lol, lol, lol, lol, lol, lol, lol, lol]\n");
        for (previous, name) in ["a", "b", "c", "d", "e", "f", "g", "h"]
            .iter()
            .zip(["b", "c", "d", "e", "f", "g", "h", "i"])
        {
            laughs.push_str(&format!(
                "{name}: &{name} [{}]\n",
                vec![format!("*{previous}"); 10].join(", ")
            ));
        }
        assert_eq!(kind(&laughs), Some(YamlErrorKind::Limit));
        let big = "x".repeat(200_000);
        let replayed = format!("a: &a \"{big}\"\nb: [{}]\n", vec!["*a"; 50].join(", "));
        assert_eq!(kind(&replayed), Some(YamlErrorKind::Limit));
        // A million numbers through aliases: no bytes, so only the node
        // bound stops it.
        let mut numbers = String::from("a: &a [1, 1, 1, 1, 1, 1, 1, 1, 1, 1]\n");
        for (previous, name) in ["a", "b", "c", "d", "e"]
            .iter()
            .zip(["b", "c", "d", "e", "f"])
        {
            numbers.push_str(&format!(
                "{name}: &{name} [{}]\n",
                vec![format!("*{previous}"); 10].join(", ")
            ));
        }
        assert_eq!(kind(&numbers), Some(YamlErrorKind::Limit));
        let mut deep_block = String::new();
        for depth in 0..100 {
            deep_block.push_str(&" ".repeat(depth));
            deep_block.push_str("k:\n");
        }
        assert_eq!(kind(&deep_block), Some(YamlErrorKind::Limit));
        assert_eq!(kind("a: [1, 2]\n"), None);
    }

    // The deepest accepted document parses on a thread with half of Tokio's
    // default worker stack, and Windows' default main-thread stack.
    #[test]
    fn parses_the_deepest_document_on_a_small_stack() {
        let mut deepest = String::new();
        for depth in 0..MAX_DEPTH - 1 {
            deepest.push_str(&" ".repeat(depth));
            deepest.push_str("k:\n");
        }
        deepest.push_str(&" ".repeat(MAX_DEPTH - 1));
        deepest.push_str("k: v\n");
        let parsed = std::thread::Builder::new()
            .stack_size(1024 * 1024)
            .spawn(move || parse_document(deepest.as_bytes(), CAP).map(|_| ()))
            .expect("a thread")
            .join()
            .expect("no stack overflow");
        assert_eq!(parsed, Ok(()));
    }

    #[test]
    fn never_quotes_the_document_in_an_error() {
        let secret = "sk-very-secret-value";
        for text in [
            format!("version: 1\nrefs: {{K: {secret}\n"),
            format!("refs:\n  K: {secret}\n  K: {secret}\n"),
            format!("refs: [{secret}\n"),
        ] {
            let error = parse_document(text.as_bytes(), CAP).expect_err("malformed");
            assert!(!format!("{error} {error:?}").contains(secret));
        }
    }
}
