//! Reading session files: lines, headers, migration, and discovery.
//!
//! Ported from Pi `packages/coding-agent/src/core/session-manager.ts`
//! (v1.1.0): `parseSessionEntries`, `parseSessionEntryLine`,
//! `loadEntriesFromFile`, `readSessionHeader`, `migrateV1ToV2`,
//! `migrateV2ToV3`, `migrateToCurrentVersion`, `findMostRecentSession`, and
//! the default session directory.
//!
//! A session file is JSON Lines: a header, then one entry per line, each
//! line ending in `\n`. Reading follows Pi:
//!
//! - Bytes are decoded as UTF-8 with U+FFFD for invalid sequences.
//! - A blank line is skipped. So is a line that does not parse, wherever it
//!   is, including a torn final line a crashed writer left behind; nothing
//!   about it is reported. A line whose value is falsy in JavaScript (`null`,
//!   `false`, `0`, `""`) is skipped too.
//! - The first parsed line must be a header with a string `id`; otherwise the
//!   file is not a session and is not modified.
//! - When the file does not end in a newline, one is appended once the
//!   header is valid, so the next append starts a line of its own and a
//!   torn fragment stays an unparseable line of its own.
//!
//! Lines are parsed as `JSON.parse` parses them
//! ([`crate::session::json_line`]): any nesting depth, lone UTF-16 surrogate
//! escapes, and out-of-range numbers all read, so no line Pi reads is
//! skipped and no parent chain breaks at one. In memory, values nested past
//! [`crate::session::json_line::MAX_HELD_DEPTH`] are `null`, a lone surrogate
//! is U+FFFD, and an out-of-range number is `null`; rewrites keep the bytes
//! Pi writes.
//!
//! Deviation: a line whose value is not an object (a number, string, or
//! array) is dropped after the header check, where Pi keeps it and indexes it
//! under an `undefined` id. An object without a string `id` keeps its
//! members but not the text kept for such values.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::session::entry::{CURRENT_SESSION_VERSION, FileEntry, SessionHeader};
use crate::session::id::generate_entry_id;
use crate::session::json::{JsonObject, js_to_number, js_trim, str_member};
use crate::session::json_line::{Verbatim, parse_json_line};
use crate::session::paths::{normalize_path_buf, resolve_path, resolve_path_string};

/// Pi's `MAX_SESSION_HEADER_SCAN_BYTES`: how far discovery reads for a
/// header.
pub const MAX_SESSION_HEADER_SCAN_BYTES: u64 = 1024 * 1024;

/// A parsed line: an object with the text it keeps, or a truthy value of
/// another kind.
pub(crate) enum ParsedLine {
    Object(JsonObject, Verbatim),
    Other,
}

/// A line's object and the text it keeps ([`crate::session::json_line`]).
pub(crate) type Line = (JsonObject, Verbatim);

/// Pi's `parseSessionEntryLine`: `None` for a blank, malformed, or falsy
/// line.
pub(crate) fn parse_line(line: &str) -> Option<ParsedLine> {
    if js_trim(line).is_empty() {
        return None;
    }
    let parsed = parse_json_line(line)?;
    if parsed.non_finite_root {
        return Some(ParsedLine::Other);
    }
    match parsed.value {
        Value::Object(object) => Some(ParsedLine::Object(object, parsed.verbatim)),
        Value::Null | Value::Bool(false) => None,
        Value::Number(number) if number.as_f64() == Some(0.0) => None,
        Value::String(text) if text.is_empty() => None,
        _ => Some(ParsedLine::Other),
    }
}

/// Pi's `parseSessionEntries`: every object line of `content`, without the
/// header check.
pub fn parse_session_entries(content: &str) -> Vec<FileEntry> {
    js_trim(content)
        .split('\n')
        .filter_map(|line| match parse_line(line)? {
            ParsedLine::Object(object, verbatim) => Some(FileEntry::from_line(object, verbatim)),
            ParsedLine::Other => None,
        })
        .collect()
}

fn is_valid_header(object: &JsonObject) -> bool {
    str_member(object, "type") == Some("session") && str_member(object, "id").is_some()
}

/// Read the objects of a session file, repairing a missing final newline.
/// Empty when the file does not exist or is not a session.
pub(crate) fn load_objects(path: &Path) -> io::Result<Vec<Line>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut objects = Vec::new();
    let mut first_is_header: Option<bool> = None;
    let mut line = Vec::new();
    let mut unterminated = false;
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        let terminated = line.last() == Some(&b'\n');
        if terminated {
            line.pop();
        } else {
            unterminated = true;
        }
        let text = String::from_utf8_lossy(&line);
        match parse_line(&text) {
            Some(ParsedLine::Object(object, verbatim)) => {
                first_is_header.get_or_insert_with(|| is_valid_header(&object));
                objects.push((object, verbatim));
            }
            Some(ParsedLine::Other) => {
                first_is_header.get_or_insert(false);
            }
            None => {}
        }
    }
    match first_is_header {
        None => return Ok(Vec::new()),
        Some(false) => return Ok(Vec::new()),
        Some(true) => {}
    }
    if unterminated {
        OpenOptions::new()
            .append(true)
            .open(path)?
            .write_all(b"\n")?;
    }
    Ok(objects)
}

/// Pi's `loadEntriesFromFile`: the lines of a session file, without
/// migration. Empty when the file does not exist or its first parsed line is
/// not a header with a string `id`; then the file is left as it is. When the
/// file does not end in a newline, one is appended.
pub fn load_entries_from_file(path: &Path) -> io::Result<Vec<FileEntry>> {
    let path = normalize_path_buf(path);
    Ok(load_objects(&path)?
        .into_iter()
        .map(|(object, verbatim)| FileEntry::from_line(object, verbatim))
        .collect())
}

/// Why a header could not be read.
#[derive(Debug)]
pub(crate) enum HeaderError {
    Io(io::Error),
    /// No header line ended within [`MAX_SESSION_HEADER_SCAN_BYTES`].
    ScanLimit,
}

impl From<io::Error> for HeaderError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// `undefined` to keep scanning, `Some(None)` for a parsed non-header line.
fn header_candidate(line: &[u8]) -> Option<Option<SessionHeader>> {
    match parse_line(&String::from_utf8_lossy(line))? {
        ParsedLine::Object(object, verbatim) if is_valid_header(&object) => {
            match FileEntry::from_line(object, verbatim) {
                FileEntry::Header(header) => Some(Some(header)),
                _ => Some(None),
            }
        }
        _ => Some(None),
    }
}

/// Pi's `readSessionHeader`: the first parsed line when it is a header,
/// reading at most [`MAX_SESSION_HEADER_SCAN_BYTES`].
pub(crate) fn read_session_header(path: &Path) -> Result<Option<SessionHeader>, HeaderError> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut pending: Vec<u8> = Vec::new();
    let mut scanned: u64 = 0;
    let mut buffer = [0u8; 4096];
    while scanned < MAX_SESSION_HEADER_SCAN_BYTES {
        let limit =
            usize::try_from((MAX_SESSION_HEADER_SCAN_BYTES - scanned).min(4096)).unwrap_or(4096);
        let Some(chunk) = buffer.get_mut(..limit) else {
            break;
        };
        let read = match reader.read(chunk) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        if read == 0 {
            return Ok(header_candidate(&pending).flatten());
        }
        scanned += read as u64;
        for byte in chunk.iter().take(read) {
            if *byte == b'\n' {
                if let Some(found) = header_candidate(&pending) {
                    return Ok(found);
                }
                pending.clear();
            } else {
                pending.push(*byte);
            }
        }
    }
    // A header ending exactly at the limit without a newline is allowed.
    let mut probe = [0u8; 1];
    loop {
        match reader.read(&mut probe) {
            Ok(0) => return Ok(header_candidate(&pending).flatten()),
            Ok(_) => return Err(HeaderError::ScanLimit),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
}

/// Discovery is best-effort: an unreadable or oversized file is not a
/// session.
pub(crate) fn read_session_header_for_discovery(path: &Path) -> Option<SessionHeader> {
    read_session_header(path).ok().flatten()
}

/// Whether a header's `cwd` names `resolved_cwd`.
pub(crate) fn session_cwd_matches(cwd: Option<&str>, resolved_cwd: &str) -> bool {
    cwd.is_some_and(|cwd| !cwd.is_empty() && resolve_path_string(cwd) == resolved_cwd)
}

/// Whether `name` ends in `.jsonl`.
pub(crate) fn is_jsonl(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|name| name.ends_with(".jsonl"))
}

/// Pi's `findMostRecentSession`: the most recently modified `.jsonl` file in
/// `session_dir` with a valid header, and when `cwd` is given, a header
/// `cwd` that resolves to it. `None` when the directory cannot be read or a
/// file cannot be stated, as in Pi.
pub fn find_most_recent_session(session_dir: &Path, cwd: Option<&str>) -> Option<PathBuf> {
    let dir = normalize_path_buf(session_dir);
    let resolved_cwd = cwd.map(resolve_path_string);
    let mut files: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    for entry in fs::read_dir(&dir).ok()? {
        let entry = entry.ok()?;
        if !is_jsonl(&entry.file_name()) {
            continue;
        }
        let path = dir.join(entry.file_name());
        let modified = fs::metadata(&path).ok()?.modified().ok()?;
        files.push((path, modified));
    }
    files.sort_by_key(|file| std::cmp::Reverse(file.1));
    files.into_iter().map(|(path, _)| path).find(|path| {
        read_session_header_for_discovery(path).is_some_and(|header| match &resolved_cwd {
            None => true,
            Some(resolved) => session_cwd_matches(header.cwd(), resolved),
        })
    })
}

/// JavaScript `Number(header?.version ?? 1)`: a missing or `null` version
/// is version 1.
fn header_version(object: &JsonObject) -> f64 {
    match object.get("version") {
        None | Some(Value::Null) => 1.0,
        version => js_to_number(version),
    }
}

/// Version 1 to 2: give every entry an id and a parent forming one chain,
/// and turn a compaction's `firstKeptEntryIndex` into `firstKeptEntryId`.
fn migrate_v1_to_v2(entries: &mut [JsonObject]) {
    let mut ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut previous: Option<String> = None;
    for index in 0..entries.len() {
        let Some(entry) = entries.get_mut(index) else {
            continue;
        };
        if str_member(entry, "type") == Some("session") {
            entry.insert("version".into(), Value::from(2));
            continue;
        }
        let id = generate_entry_id(|candidate| ids.contains(candidate));
        ids.insert(id.clone());
        entry.insert("id".into(), Value::String(id.clone()));
        entry.insert(
            "parentId".into(),
            previous.clone().map_or(Value::Null, Value::String),
        );
        previous = Some(id);
        if str_member(entry, "type") != Some("compaction") {
            continue;
        }
        let Some(Value::Number(kept)) = entry.get("firstKeptEntryIndex").cloned() else {
            continue;
        };
        // `entries[index]` for an integral index; a fractional one is
        // `undefined` in JavaScript. The outer `Option` is whether a target
        // exists, the inner one whether it has an `id` yet: an entry after
        // the compaction has none, and assigning `undefined` omits the member.
        let target_id: Option<Option<Value>> = kept
            .as_f64()
            .filter(|kept| kept.fract() == 0.0 && *kept >= 0.0)
            .and_then(|kept| usize::try_from(kept as u64).ok())
            .and_then(|kept| entries.get(kept))
            .filter(|target| str_member(target, "type") != Some("session"))
            .map(|target| target.get("id").cloned());
        let Some(entry) = entries.get_mut(index) else {
            continue;
        };
        match target_id {
            Some(Some(id)) => {
                entry.insert("firstKeptEntryId".into(), id);
            }
            Some(None) => {
                entry.shift_remove("firstKeptEntryId");
            }
            None => {}
        }
        entry.shift_remove("firstKeptEntryIndex");
    }
}

/// Version 2 to 3: the `hookMessage` role became `custom`.
fn migrate_v2_to_v3(entries: &mut [JsonObject]) {
    for entry in entries.iter_mut() {
        if str_member(entry, "type") == Some("session") {
            entry.insert("version".into(), Value::from(3));
            continue;
        }
        if str_member(entry, "type") != Some("message") {
            continue;
        }
        if let Some(Value::Object(message)) = entry.get_mut("message")
            && str_member(message, "role") == Some("hookMessage")
        {
            message.insert("role".into(), Value::String("custom".into()));
        }
    }
}

/// Pi's `migrateToCurrentVersion`: bring entries to the current version in
/// place; `true` when a migration ran. The version is the first header's.
pub(crate) fn migrate_objects(entries: &mut [JsonObject]) -> bool {
    let version = entries
        .iter()
        .find(|entry| str_member(entry, "type") == Some("session"))
        .map_or(1.0, header_version);
    if version >= CURRENT_SESSION_VERSION as f64 {
        return false;
    }
    if version < 2.0 {
        migrate_v1_to_v2(entries);
    }
    if version < 3.0 {
        migrate_v2_to_v3(entries);
    }
    true
}

/// Pi's `migrateSessionEntries`: migrate parsed lines to the current version
/// in place.
pub fn migrate_session_entries(entries: &mut Vec<FileEntry>) {
    let lines: Vec<Line> = std::mem::take(entries)
        .into_iter()
        .map(FileEntry::into_line)
        .collect();
    *entries = migrate_lines(lines)
        .0
        .into_iter()
        .map(|(object, verbatim)| FileEntry::from_line(object, verbatim))
        .collect();
}

/// [`migrate_objects`] over lines; the kept text stays with its line, since
/// migration neither reorders nor removes lines.
pub(crate) fn migrate_lines(lines: Vec<Line>) -> (Vec<Line>, bool) {
    let (mut objects, verbatims): (Vec<JsonObject>, Vec<Verbatim>) = lines.into_iter().unzip();
    let migrated = migrate_objects(&mut objects);
    (objects.into_iter().zip(verbatims).collect(), migrated)
}

/// Pi's encoding of a working directory as a directory name: the resolved
/// path without its leading separator, every `/`, `\`, and `:` as `-`,
/// between `--` and `--`.
pub fn session_dir_name(resolved_cwd: &str) -> String {
    let trimmed = resolved_cwd
        .strip_prefix(['/', '\\'])
        .unwrap_or(resolved_cwd);
    let encoded: String = trimmed
        .chars()
        .map(|ch| {
            if matches!(ch, '/' | '\\' | ':') {
                '-'
            } else {
                ch
            }
        })
        .collect();
    format!("--{encoded}--")
}

/// Pi's `getDefaultSessionDirPath` under a Bake home: `<home>/sessions/--<cwd>--`.
pub fn default_session_dir_path(cwd: &str, home: &Path) -> PathBuf {
    let resolved_cwd = resolve_path_string(cwd);
    resolve_path(&crate::home::sessions_dir(home)).join(session_dir_name(&resolved_cwd))
}

/// Pi's `getDefaultSessionDir`: [`default_session_dir_path`], created when
/// missing.
pub fn default_session_dir(cwd: &str, home: &Path) -> io::Result<PathBuf> {
    let dir = default_session_dir_path(cwd, home);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_names_follow_pi() {
        assert_eq!(session_dir_name("/home/u/proj"), "--home-u-proj--");
        assert_eq!(session_dir_name("C:\\work\\x"), "--C--work-x--");
        assert_eq!(session_dir_name("/"), "----");
    }

    #[test]
    fn falsy_and_malformed_lines_are_skipped() {
        for line in [
            "", "  ", "null", "false", "0", "\"\"", "{", "not json", "\u{feff}",
        ] {
            assert!(parse_line(line).is_none(), "{line:?}");
        }
        assert!(matches!(parse_line("1"), Some(ParsedLine::Other)));
        assert!(matches!(parse_line("[]"), Some(ParsedLine::Other)));
        assert!(matches!(parse_line("{}\r"), Some(ParsedLine::Object(..))));
        // `JSON.parse` reads these; see `json_line`.
        assert!(matches!(
            parse_line(r#"{"a":"\ud800"}"#),
            Some(ParsedLine::Object(..))
        ));
        assert!(matches!(parse_line("1e400"), Some(ParsedLine::Other)));
        assert!(parse_line("-1e-400").is_none());
    }

    #[test]
    fn header_versions_read_as_javascript_numbers() {
        let version = |value: Value| {
            let mut object = JsonObject::new();
            object.insert("version".into(), value);
            header_version(&object)
        };
        assert_eq!(version(Value::Null), 1.0);
        assert_eq!(version(Value::from("3")), 3.0);
        assert_eq!(version(Value::from("")), 0.0);
        assert!(version(Value::from("x")).is_nan());
        assert_eq!(version(Value::from(true)), 1.0);
        assert_eq!(version(serde_json::json!([3])), 3.0);
        assert!(version(Value::from("inf")).is_nan());
    }
}
