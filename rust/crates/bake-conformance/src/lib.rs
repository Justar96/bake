//! Synthetic conformance runner for the scope-01 comparison harness.
//!
//! The runner reads one bounded `bake/synthetic-conformance/input` document,
//! validates all of it, applies only the writes whose fixture-supplied
//! permission allows them, and relays prompts, events, and permissions as one
//! observation. Permission decisions are fixture inputs: this crate implements
//! no product policy, session protocol, approver, or runtime behavior. The
//! TypeScript arm in `scripts/rust-conformance/` implements the same contract
//! independently.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write as _};
use std::path::Path;

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

pub const INPUT_SCHEMA: &str = "bake/synthetic-conformance/input";
pub const OBSERVATION_SCHEMA: &str = "bake/synthetic-conformance/observation";
pub const VERSION: u64 = 1;
pub const MAX_INPUT_BYTES: usize = 256 * 1024;
pub const MAX_ITEMS: usize = 64;
pub const MAX_STRING_BYTES: usize = 64 * 1024;
pub const MAX_PATH_BYTES: usize = 200;
/// Event nesting limit, counting the event object itself as depth 1.
pub const MAX_EVENT_DEPTH: usize = 32;
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// Why a run failed; each variant maps to one process exit code.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    /// The input was rejected before any write. Exit code 2.
    Invalid(String),
    /// Reading stdin, writing a file, or writing stdout failed. Exit code 1.
    Io(String),
}

impl Failure {
    pub fn exit_code(&self) -> u8 {
        match self {
            Failure::Invalid(_) => 2,
            Failure::Io(_) => 1,
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Invalid(message) => write!(f, "invalid input: {message}"),
            Failure::Io(message) => write!(f, "I/O failure: {message}"),
        }
    }
}

fn invalid(message: impl Into<String>) -> Failure {
    Failure::Invalid(message.into())
}

/// A validated input document.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    schema: String,
    version: u64,
    prompts: Vec<String>,
    events: Vec<Event>,
    permissions: Vec<Permission>,
    writes: Vec<WriteRequest>,
}

/// One relayed event object. Every field is retained; key order is not.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct Event(Map<String, Value>);

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Permission {
    id: String,
    path: String,
    decision: Decision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Deny,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteRequest {
    path: String,
    text: String,
    permission: String,
}

#[derive(Serialize)]
struct Observation<'a> {
    schema: &'static str,
    version: u64,
    prompts: &'a [String],
    events: &'a [Event],
    permissions: &'a [Permission],
}

/// Reads, validates, executes, and reports one input.
///
/// Writes are resolved against `root`. Nothing is written to `root` or
/// `output` unless the whole input is valid, and `output` receives nothing
/// when a file write fails, so a caller never sees a partial observation.
pub fn run(input: impl Read, mut output: impl io::Write, root: &Path) -> Result<(), Failure> {
    let bytes = read_bounded(input)?;
    let input = parse(&bytes)?;
    let mut line = observe(&input);
    line.push('\n');
    execute(&input, root)?;
    output
        .write_all(line.as_bytes())
        .and_then(|()| output.flush())
        .map_err(|error| Failure::Io(format!("cannot write stdout: {error}")))
}

fn read_bounded(input: impl Read) -> Result<Vec<u8>, Failure> {
    let mut bytes = Vec::new();
    // One byte past the bound is enough for `parse` to reject the input.
    input
        .take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| Failure::Io(format!("cannot read stdin: {error}")))?;
    Ok(bytes)
}

/// Parses and fully validates an input document without touching the filesystem.
pub fn parse(bytes: &[u8]) -> Result<Input, Failure> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(invalid(format!("input exceeds {MAX_INPUT_BYTES} bytes")));
    }
    let input: Input = serde_json::from_slice(bytes).map_err(|error| invalid(error.to_string()))?;
    input.validate()?;
    Ok(input)
}

/// Serializes the observation that relays the input's prompts, events, and permissions.
pub fn observe(input: &Input) -> String {
    let observation = Observation {
        schema: OBSERVATION_SCHEMA,
        version: VERSION,
        prompts: &input.prompts,
        events: &input.events,
        permissions: &input.permissions,
    };
    serde_json::to_string(&observation).expect("observation values are plain JSON")
}

impl Input {
    fn validate(&self) -> Result<(), Failure> {
        if self.schema != INPUT_SCHEMA {
            return Err(invalid(format!("schema must be \"{INPUT_SCHEMA}\"")));
        }
        if self.version != VERSION {
            return Err(invalid(format!("version must be {VERSION}")));
        }
        for (name, len) in [
            ("prompts", self.prompts.len()),
            ("events", self.events.len()),
            ("permissions", self.permissions.len()),
            ("writes", self.writes.len()),
        ] {
            if len > MAX_ITEMS {
                return Err(invalid(format!("{name} has more than {MAX_ITEMS} entries")));
            }
        }
        for (index, prompt) in self.prompts.iter().enumerate() {
            check_string(&format!("prompts[{index}]"), prompt)?;
        }

        let mut ids = BTreeSet::new();
        for (index, permission) in self.permissions.iter().enumerate() {
            let at = format!("permissions[{index}]");
            check_string(&format!("{at}.id"), &permission.id)?;
            if permission.id.is_empty() {
                return Err(invalid(format!("{at}.id is empty")));
            }
            if !ids.insert(permission.id.as_str()) {
                return Err(invalid(format!("{at}.id duplicates an earlier permission")));
            }
            check_path(&format!("{at}.path"), &permission.path)?;
        }

        for (index, write) in self.writes.iter().enumerate() {
            let at = format!("writes[{index}]");
            check_path(&format!("{at}.path"), &write.path)?;
            check_string(&format!("{at}.text"), &write.text)?;
            let permission = self
                .permissions
                .iter()
                .find(|permission| permission.id == write.permission)
                .ok_or_else(|| invalid(format!("{at}.permission names no permission")))?;
            if permission.path != write.path {
                return Err(invalid(format!(
                    "{at}.path differs from its permission's path"
                )));
            }
        }
        Ok(())
    }
}

fn check_string(at: &str, value: &str) -> Result<(), Failure> {
    if value.len() > MAX_STRING_BYTES {
        return Err(invalid(format!("{at} exceeds {MAX_STRING_BYTES} bytes")));
    }
    Ok(())
}

fn check_path(at: &str, path: &str) -> Result<(), Failure> {
    match path_problem(path) {
        Some(problem) => Err(invalid(format!("{at} {problem}"))),
        None => Ok(()),
    }
}

/// Returns why `path` is not a portable relative workspace path, if it is not.
///
/// Accepted paths are relative POSIX slash paths that name the same file on
/// Linux, macOS, and Windows: no absolute, drive, or UNC forms, no empty, `.`,
/// or `..` segment, no characters Windows rejects, no reserved device
/// basename, and no segment ending in a dot or space.
pub fn path_problem(path: &str) -> Option<&'static str> {
    if path.is_empty() {
        return Some("is empty");
    }
    if path.len() > MAX_PATH_BYTES {
        return Some("exceeds 200 bytes");
    }
    if path.contains('\\') {
        return Some("contains a backslash");
    }
    if path.contains(':') {
        return Some("contains a colon");
    }
    if path.chars().any(|c| c.is_ascii_control()) {
        return Some("contains an ASCII control character");
    }
    if path.contains(['<', '>', '"', '|', '?', '*']) {
        return Some("contains a character Windows rejects");
    }
    for segment in path.split('/') {
        if segment.is_empty() {
            return Some("has an empty segment (leading, trailing, or repeated slash)");
        }
        if segment == "." || segment == ".." {
            return Some("has a dot segment");
        }
        if segment.ends_with(['.', ' ']) {
            return Some("has a segment ending in a dot or space");
        }
        if is_reserved_basename(segment) {
            return Some("has a Windows reserved device name");
        }
    }
    None
}

/// Windows reserves device names regardless of extension or trailing spaces
/// before it, so `nul.txt` and `CON .log` are both reserved.
fn is_reserved_basename(segment: &str) -> bool {
    let stem = segment
        .split('.')
        .next()
        .unwrap_or(segment)
        .trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    match upper
        .strip_prefix("COM")
        .or_else(|| upper.strip_prefix("LPT"))
    {
        Some(suffix) => {
            matches!(suffix, "¹" | "²" | "³")
                || (suffix.len() == 1 && suffix.as_bytes()[0].is_ascii_digit())
        }
        None => false,
    }
}

fn execute(input: &Input, root: &Path) -> Result<(), Failure> {
    for write in &input.writes {
        let allowed = input.permissions.iter().any(|p| {
            p.id == write.permission && p.path == write.path && p.decision == Decision::Allow
        });
        if allowed {
            write_file(root, &write.path, write.text.as_bytes()).map_err(|error| {
                Failure::Io(format!("cannot write \"{}\": {error}", write.path))
            })?;
        }
    }
    Ok(())
}

/// Creates missing parent directories one level at a time and refuses to
/// write through a pre-existing symlink, junction, or non-regular file. The
/// checks and the writes are separate steps, so this is not race-safe
/// sandboxing: the driver runs only trusted arms in private roots it owns.
fn write_file(root: &Path, path: &str, bytes: &[u8]) -> io::Result<()> {
    let mut target = root.to_path_buf();
    let mut segments = path.split('/').peekable();
    while let Some(segment) = segments.next() {
        target.push(segment);
        let is_last = segments.peek().is_none();
        match fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(io::Error::other("path crosses a symbolic link"));
            }
            Ok(meta) if is_last && !meta.is_file() => {
                return Err(io::Error::other("target is not a regular file"));
            }
            Ok(meta) if !is_last && !meta.is_dir() => {
                return Err(io::Error::other("parent is not a directory"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if !is_last {
                    fs::create_dir(&target)?;
                }
            }
            Err(error) => return Err(error),
        }
    }
    let mut file = fs::File::create(&target)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Generic event data: rejects duplicate object keys, numbers that are not
/// safe integers, strings over the byte bound, and containers nested deeper
/// than [`MAX_EVENT_DEPTH`], all of which `Value` would otherwise collapse,
/// round, or accept. serde_json's own recursion limit rejects far deeper
/// documents before this check runs.
struct StrictValue(Value);

/// Deserializes one value whose containers, if any, sit at `depth`.
#[derive(Clone, Copy)]
struct StrictVisitor {
    depth: usize,
}

impl StrictVisitor {
    fn enter<E: de::Error>(self) -> Result<(), E> {
        if self.depth > MAX_EVENT_DEPTH {
            return Err(E::custom(format!(
                "event nests more than {MAX_EVENT_DEPTH} containers"
            )));
        }
        Ok(())
    }

    fn child(self) -> Self {
        StrictVisitor {
            depth: self.depth + 1,
        }
    }
}

impl<'de> DeserializeSeed<'de> for StrictVisitor {
    type Value = StrictValue;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<StrictValue, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Deserialize<'de> for Event {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match (StrictVisitor { depth: 1 }).deserialize(deserializer)?.0 {
            Value::Object(fields) => Ok(Event(fields)),
            _ => Err(de::Error::custom("event must be an object")),
        }
    }
}

fn safe_integer<E: de::Error>(magnitude: u64, value: Number) -> Result<StrictValue, E> {
    if magnitude > MAX_SAFE_INTEGER {
        return Err(E::custom("event number is not a safe integer"));
    }
    Ok(StrictValue(Value::Number(value)))
}

fn bounded_string<E: de::Error>(value: String) -> Result<String, E> {
    if value.len() > MAX_STRING_BYTES {
        return Err(E::custom(format!(
            "event string exceeds {MAX_STRING_BYTES} bytes"
        )));
    }
    Ok(value)
}

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = StrictValue;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_unit<E: de::Error>(self) -> Result<StrictValue, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<StrictValue, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<StrictValue, E> {
        safe_integer(value, value.into())
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<StrictValue, E> {
        safe_integer(value.unsigned_abs(), value.into())
    }

    // serde_json reports fractions, exponents, `-0`, and integers beyond 64
    // bits as floats; none of them is a safe integer that relays exactly.
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<StrictValue, E> {
        Err(E::custom("event number is not a safe integer"))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<StrictValue, E> {
        self.visit_string(value.to_owned())
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<StrictValue, E> {
        Ok(StrictValue(Value::String(bounded_string(value)?)))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<StrictValue, A::Error> {
        self.enter()?;
        let mut items = Vec::new();
        while let Some(StrictValue(item)) = seq.next_element_seed(self.child())? {
            items.push(item);
        }
        Ok(StrictValue(Value::Array(items)))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<StrictValue, A::Error> {
        self.enter()?;
        let mut fields = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            let key = bounded_string(key)?;
            if fields.contains_key(&key) {
                return Err(de::Error::custom(format!("duplicate event key \"{key}\"")));
            }
            let StrictValue(value) = map.next_value_seed(self.child())?;
            fields.insert(key, value);
        }
        Ok(StrictValue(Value::Object(fields)))
    }
}

#[cfg(test)]
mod tests;
