//! Session listings for the resume picker.
//!
//! Ported from Pi `packages/coding-agent/src/core/session-manager.ts`
//! (v1.1.0): `buildSessionInfo`, `listSessionsFromDir`, and the bodies of
//! `SessionManager.list` and `SessionManager.listAll`.
//!
//! Pi loads up to ten files at once on its event loop; Bake loads them one
//! after another on the caller's thread, so progress arrives in file order
//! and nothing outlives the call. Cancellation is checked before each file,
//! where Pi's workers check their `AbortSignal`. File names are ordered by
//! code point where Pi uses `localeCompare`, and lines are split at `\n`
//! with a trailing `\r` dropped, where Node's `readline` also ends a line at
//! a lone `\r`, which a JSON line holds only between tokens. A file whose
//! header `id` is not a string is not listed, where Pi lists it with that
//! value as its id ([`SessionInfo::id`] is a string); neither Pi nor Bake
//! can open such a file.

use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use serde_json::Value;

use crate::session::SessionError;
use crate::session::file::{ParsedLine, is_jsonl, parse_line};
use crate::session::json::{JsonObject, js_to_string, js_trim, str_member};
use crate::session::time::{js_date_ms, parse_date_ms, system_time_ms, time_clip};

/// Pi's `SessionInfo`.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionInfo {
    /// The session file.
    pub path: PathBuf,
    /// The session id.
    pub id: String,
    /// The working directory it started in; empty for old sessions.
    pub cwd: String,
    /// The display name from the latest `session_info` entry.
    pub name: Option<String>,
    /// The session it was forked from.
    pub parent_session_path: Option<String>,
    /// The header's timestamp in Unix milliseconds; `None` when invalid.
    pub created_ms: Option<i64>,
    /// The latest user or assistant message time, else the header's time,
    /// else the file's modification time, in Unix milliseconds.
    pub modified_ms: i64,
    /// The number of `message` entries.
    pub message_count: usize,
    /// The first user message's text, or `(no messages)`.
    pub first_message: String,
    /// The text of every user and assistant message, joined by spaces.
    pub all_messages_text: String,
}

/// Progress of a listing: files loaded, files in all, and on some updates
/// the sessions loaded so far, newest first.
pub type ListProgress<'a> = dyn FnMut(usize, usize, Option<&[SessionInfo]>) + 'a;

const CURRENT_SESSION_LIST_PUBLISH_INTERVAL: usize = 10;
const ALL_SESSION_LIST_PUBLISH_INTERVAL: usize = 100;

fn check(cancel: Option<&AtomicBool>) -> Result<(), SessionError> {
    if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
        Err(SessionError::Aborted)
    } else {
        Ok(())
    }
}

/// `extractTextContent`; `None` where Pi would throw.
fn message_text(message: &JsonObject) -> Option<String> {
    match message.get("content")? {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let mut texts = Vec::new();
            for block in blocks {
                match block {
                    Value::Null => return None,
                    Value::Object(block) if str_member(block, "type") == Some("text") => {
                        texts.push(js_to_string(block.get("text")));
                    }
                    _ => {}
                }
            }
            Some(texts.join(" "))
        }
        _ => None,
    }
}

/// `getMessageActivityTime`: a user or assistant message's own timestamp,
/// else its entry's.
fn activity_time(entry: &JsonObject, message: &JsonObject) -> Option<f64> {
    match message.get("timestamp") {
        Some(Value::Number(number)) => number.as_f64(),
        _ => js_date_ms(entry.get("timestamp")).map(|ms| ms as f64),
    }
}

/// Pi's `buildSessionInfo`; `None` for a file that is not a session or that
/// Pi would fail to read.
pub(crate) fn build_session_info(path: &Path, modified: Option<SystemTime>) -> Option<SessionInfo> {
    let modified = match modified {
        Some(modified) => modified,
        None => fs::metadata(path).ok()?.modified().ok()?,
    };
    let mut reader = BufReader::new(File::open(path).ok()?);
    let mut header: Option<JsonObject> = None;
    let mut message_count = 0usize;
    let mut first_message = String::new();
    let mut all_messages: Vec<String> = Vec::new();
    let mut name: Option<String> = None;
    let mut last_activity: Option<f64> = None;
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).ok()? == 0 {
            break;
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        let Some(parsed) = parse_line(&String::from_utf8_lossy(&line)) else {
            continue;
        };
        let ParsedLine::Object(entry, _) = parsed else {
            // A first value that is not a header is not a session.
            header.as_ref()?;
            continue;
        };
        if header.is_none() {
            if str_member(&entry, "type") != Some("session") {
                return None;
            }
            header = Some(entry);
            continue;
        }
        if str_member(&entry, "type") == Some("session_info") {
            // `entry.name?.trim() || undefined`; any other value throws.
            name = match entry.get("name") {
                None | Some(Value::Null) => None,
                Some(Value::String(text)) => {
                    Some(js_trim(text).to_owned()).filter(|name| !name.is_empty())
                }
                Some(_) => return None,
            };
        }
        if str_member(&entry, "type") != Some("message") {
            continue;
        }
        message_count += 1;
        let message = match entry.get("message") {
            None | Some(Value::Null) => return None,
            Some(Value::Object(message)) => message,
            Some(_) => continue,
        };
        let role = str_member(message, "role");
        if role.is_none() || !message.contains_key("content") {
            continue;
        }
        if !matches!(role, Some("user" | "assistant")) {
            continue;
        }
        if let Some(time) = activity_time(&entry, message) {
            last_activity = Some(last_activity.unwrap_or(0.0).max(time));
        }
        let text = message_text(message)?;
        if text.is_empty() {
            continue;
        }
        if first_message.is_empty() && role == Some("user") {
            first_message = text.clone();
        }
        all_messages.push(text);
    }
    let header = header?;
    let id = str_member(&header, "id")?.to_owned();
    let header_time = str_member(&header, "timestamp").and_then(parse_date_ms);
    let modified_ms = last_activity
        .filter(|time| *time > 0.0)
        .and_then(time_clip)
        .or(header_time)
        .unwrap_or_else(|| system_time_ms(modified) as i64);
    Some(SessionInfo {
        path: path.to_path_buf(),
        id,
        cwd: str_member(&header, "cwd").unwrap_or("").to_owned(),
        name,
        parent_session_path: str_member(&header, "parentSession").map(str::to_owned),
        created_ms: js_date_ms(header.get("timestamp")),
        modified_ms,
        message_count,
        first_message: if first_message.is_empty() {
            "(no messages)".to_owned()
        } else {
            first_message
        },
        all_messages_text: all_messages.join(" "),
    })
}

/// Newest first; a stable sort, as `Array.prototype.sort` is.
pub(crate) fn sort_session_infos(sessions: &mut [SessionInfo]) {
    sessions.sort_by_key(|session| std::cmp::Reverse(session.modified_ms));
}

fn sorted(sessions: &[SessionInfo]) -> Vec<SessionInfo> {
    let mut sessions = sessions.to_vec();
    sort_session_infos(&mut sessions);
    sessions
}

/// Pi's `listSessionsFromDir`: every session in one directory, in file-name
/// order, newest name first.
pub(crate) fn list_sessions_from_dir(
    dir: &Path,
    mut progress: Option<&mut ListProgress<'_>>,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<SessionInfo>, SessionError> {
    check(cancel)?;
    let Ok(read_dir) = fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut names: Vec<std::ffi::OsString> = Vec::new();
    for entry in read_dir {
        let Ok(entry) = entry else {
            return Ok(Vec::new());
        };
        if is_jsonl(&entry.file_name()) {
            names.push(entry.file_name());
        }
    }
    names.sort_by(|a, b| b.cmp(a));
    let total = names.len();
    let mut sessions = Vec::new();
    for (index, name) in names.iter().enumerate() {
        check(cancel)?;
        if let Some(info) = build_session_info(&dir.join(name), None) {
            sessions.push(info);
        }
        let loaded = index + 1;
        if let Some(progress) = progress.as_deref_mut() {
            let publish = loaded == 1
                || loaded % CURRENT_SESSION_LIST_PUBLISH_INTERVAL == 0
                || loaded == total;
            let partial = publish.then(|| sorted(&sessions));
            progress(loaded, total, partial.as_deref());
        }
    }
    Ok(sessions)
}

/// The body of Pi's `listAll` without a session directory: every session in
/// every directory (or directory link) under `sessions_dir`, most recently
/// modified file first.
pub(crate) fn list_all_from_sessions_dir(
    sessions_dir: &Path,
    mut progress: Option<&mut ListProgress<'_>>,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<SessionInfo>, SessionError> {
    let Ok(read_dir) = fs::read_dir(sessions_dir) else {
        return Ok(Vec::new());
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in read_dir {
        let Ok(entry) = entry else {
            return Ok(Vec::new());
        };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() || file_type.is_symlink() {
            dirs.push(sessions_dir.join(entry.file_name()));
        }
    }
    let mut candidates: Vec<(PathBuf, Option<SystemTime>)> = Vec::new();
    for dir in dirs {
        check(cancel)?;
        // A broken link or a link to a file lists nothing.
        let Ok(read_dir) = fs::read_dir(&dir) else {
            continue;
        };
        let mut files = Vec::new();
        let mut failed = false;
        for entry in read_dir {
            match entry {
                Ok(entry) if is_jsonl(&entry.file_name()) => {
                    files.push(dir.join(entry.file_name()))
                }
                Ok(_) => {}
                Err(_) => failed = true,
            }
        }
        if failed {
            continue;
        }
        for path in files {
            let modified = fs::metadata(&path).and_then(|meta| meta.modified()).ok();
            candidates.push((path, modified));
        }
    }
    candidates.sort_by(|a, b| {
        let time = |candidate: &(PathBuf, Option<SystemTime>)| {
            candidate.1.map_or(f64::NEG_INFINITY, system_time_ms)
        };
        time(b)
            .total_cmp(&time(a))
            .then_with(|| b.0.file_name().cmp(&a.0.file_name()))
    });
    let total = candidates.len();
    let mut sessions = Vec::new();
    for (index, (path, modified)) in candidates.iter().enumerate() {
        check(cancel)?;
        if let Some(info) = build_session_info(path, *modified) {
            sessions.push(info);
        }
        let loaded = index + 1;
        if let Some(progress) = progress.as_deref_mut() {
            let publish =
                index == 0 || loaded % ALL_SESSION_LIST_PUBLISH_INTERVAL == 0 || loaded == total;
            let partial = publish.then(|| sorted(&sessions));
            progress(loaded, total, partial.as_deref());
        }
    }
    sort_session_infos(&mut sessions);
    Ok(sessions)
}
