//! The session context: the messages an agent resumes with.
//!
//! Ported from Pi `packages/coding-agent/src/core/session-manager.ts`
//! (v1.1.0): `buildSessionPath`, `getSessionContextSettings`,
//! `sessionEntryToContextMessages`, `buildContextEntries`,
//! `projectContextEntry`, `buildSessionProjection`, and
//! `buildSessionContext`.
//!
//! The context follows the path from the root to the leaf. The latest
//! compaction on the path contributes its recorded system message and its
//! summary, followed by the entries from `firstKeptEntryId` up to it (system
//! messages excepted) and every entry after it. Branch summaries and custom
//! message entries become messages; context edits replace or omit an earlier
//! entry's content; the thinking level and model come from the whole path.
//!
//! Pi reads entries without validating them, and so does this module: a
//! message keeps every member it was stored with, and a summary takes its
//! members as stored. Where Pi would throw or loop, Bake does neither:
//!
//! - A `message` entry whose `message` is not an object contributes nothing
//!   (Pi throws or passes the value on).
//! - A context edit without a `replacement` member leaves its target as it is
//!   (Pi throws when the target is editable).
//! - A parent chain that returns to an entry already on the path stops there
//!   (Pi loops forever).
//! - A thinking level change whose `thinkingLevel` is not a string is
//!   skipped (Pi records `undefined`).
//! - A model change, or an assistant message, whose provider or model is not
//!   a string leaves no model (Pi records an object holding those values,
//!   `{}` once serialized when they are missing).

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::session::entry::SessionEntry;
use crate::session::json::{JsonObject, js_truthy, str_member};
use crate::session::messages::AgentMessage;
use crate::session::time::js_date_ms;

/// Which leaf a context is built for, Pi's `leafId` argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeafSelector<'a> {
    /// `undefined`: the last entry.
    Last,
    /// `null`: before the first entry, an empty path.
    Empty,
    /// An entry id; the last entry when no entry has it.
    Id(&'a str),
}

impl<'a> From<Option<&'a str>> for LeafSelector<'a> {
    /// A manager's leaf pointer: `None` is `null`.
    fn from(leaf: Option<&'a str>) -> Self {
        match leaf {
            Some(id) => Self::Id(id),
            None => Self::Empty,
        }
    }
}

/// The model a session last used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    /// Provider id.
    pub provider: String,
    /// Model id.
    pub model_id: String,
}

/// One context entry and the messages it contributes.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedSessionEntry {
    /// The entry.
    pub source_entry: SessionEntry,
    /// Its messages after context edits; empty for state entries and
    /// omissions.
    pub messages: Vec<AgentMessage>,
}

/// Pi's `SessionProjection`.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionProjection {
    /// The context entries.
    pub entries: Vec<ProjectedSessionEntry>,
    /// Their messages, in order.
    pub messages: Vec<AgentMessage>,
    /// The thinking level; `off` when none was set.
    pub thinking_level: String,
    /// The model, when one was chosen or answered.
    pub model: Option<ModelRef>,
}

/// Pi's `SessionContext`: what the agent resumes with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionContext {
    /// The messages.
    pub messages: Vec<AgentMessage>,
    /// The thinking level; `off` when none was set.
    pub thinking_level: String,
    /// The model, when one was chosen or answered.
    pub model: Option<ModelRef>,
}

impl SessionContext {
    /// The context as the JSON object Pi's `buildSessionContext()` returns.
    pub fn to_json(&self) -> Value {
        let mut object = JsonObject::new();
        object.insert(
            "messages".into(),
            Value::Array(
                self.messages
                    .iter()
                    .map(|message| Value::Object(message.as_json().clone()))
                    .collect(),
            ),
        );
        object.insert(
            "thinkingLevel".into(),
            Value::String(self.thinking_level.clone()),
        );
        object.insert(
            "model".into(),
            match &self.model {
                Some(model) => {
                    let mut value = JsonObject::new();
                    value.insert("provider".into(), Value::String(model.provider.clone()));
                    value.insert("modelId".into(), Value::String(model.model_id.clone()));
                    Value::Object(value)
                }
                None => Value::Null,
            },
        );
        Value::Object(object)
    }
}

/// Entries by id; a repeated id names its last entry, as Pi's `Map` does.
pub(crate) type EntryIndex<'a> = HashMap<&'a str, &'a SessionEntry>;

pub(crate) fn index_entries<'a>(entries: &[&'a SessionEntry]) -> EntryIndex<'a> {
    let mut index = HashMap::with_capacity(entries.len());
    for entry in entries {
        index.insert(entry.id(), *entry);
    }
    index
}

/// Walk from `start` to its root and return the path root first.
pub(crate) fn path_to_root<'a>(
    start: Option<&'a SessionEntry>,
    index: &EntryIndex<'a>,
) -> Vec<&'a SessionEntry> {
    let mut path = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut current = start;
    while let Some(entry) = current {
        if !seen.insert(entry.id()) {
            break;
        }
        path.push(entry);
        // An empty parent id is falsy in JavaScript and links nowhere.
        current = entry
            .parent_id()
            .filter(|parent| !parent.is_empty())
            .and_then(|parent| index.get(parent).copied());
    }
    path.reverse();
    path
}

pub(crate) fn session_path<'a>(
    entries: &[&'a SessionEntry],
    leaf: LeafSelector<'_>,
    index: &EntryIndex<'a>,
) -> Vec<&'a SessionEntry> {
    let start = match leaf {
        LeafSelector::Empty => return Vec::new(),
        LeafSelector::Last => entries.last().copied(),
        // An empty id is falsy: the last entry, as for `undefined`.
        LeafSelector::Id(id) => index
            .get(id)
            .copied()
            .filter(|_| !id.is_empty())
            .or_else(|| entries.last().copied()),
    };
    path_to_root(start, index)
}

fn context_settings(path: &[&SessionEntry]) -> (String, Option<ModelRef>) {
    let mut thinking_level = "off".to_owned();
    let mut model = None;
    for entry in path {
        match entry.entry_type() {
            "thinking_level_change" => {
                if let Some(level) = str_member(entry.as_json(), "thinkingLevel") {
                    thinking_level = level.to_owned();
                }
            }
            // Each replaces the model, as in Pi, even when its members are
            // not strings; then no model is known.
            "model_change" => {
                model = model_ref(
                    str_member(entry.as_json(), "provider"),
                    str_member(entry.as_json(), "modelId"),
                );
            }
            "message" if entry.message_role() == Some("assistant") => {
                let message = entry.get("message").and_then(Value::as_object);
                model = message.and_then(|message| {
                    model_ref(
                        str_member(message, "provider"),
                        str_member(message, "model"),
                    )
                });
            }
            _ => {}
        }
    }
    (thinking_level, model)
}

fn model_ref(provider: Option<&str>, model_id: Option<&str>) -> Option<ModelRef> {
    Some(ModelRef {
        provider: provider?.to_owned(),
        model_id: model_id?.to_owned(),
    })
}

fn copy_member(target: &mut JsonObject, source: &JsonObject, key: &str) {
    if let Some(value) = source.get(key) {
        target.insert(key.to_owned(), value.clone());
    }
}

fn timestamp_value(entry: &SessionEntry) -> Value {
    // `new Date(entry.timestamp).getTime()`; `NaN` serializes as `null`.
    match js_date_ms(entry.get("timestamp")) {
        Some(ms) => Value::from(ms),
        None => Value::Null,
    }
}

fn role_object(role: &str) -> JsonObject {
    let mut object = JsonObject::new();
    object.insert("role".into(), Value::String(role.to_owned()));
    object
}

/// Pi's `sessionEntryToContextMessages`: the messages one entry contributes.
pub fn session_entry_to_context_messages(entry: &SessionEntry) -> Vec<AgentMessage> {
    let raw = entry.as_json();
    match entry.entry_type() {
        "message" => {
            let Some(Value::Object(message)) = raw.get("message") else {
                return Vec::new();
            };
            let mut message = message.clone();
            let missing_content = matches!(message.get("content"), None | Some(Value::Null));
            match str_member(&message, "role") {
                Some("system") if missing_content => {
                    message.insert("content".into(), Value::String(String::new()));
                }
                Some("user" | "assistant" | "toolResult") if missing_content => {
                    message.insert("content".into(), Value::Array(Vec::new()));
                }
                _ => {}
            }
            vec![AgentMessage::from_json(message)]
        }
        "custom_message" => {
            let mut message = role_object("custom");
            copy_member(&mut message, raw, "customType");
            let content = match raw.get("content") {
                None | Some(Value::Null) => Value::Array(Vec::new()),
                Some(content) => content.clone(),
            };
            message.insert("content".into(), content);
            copy_member(&mut message, raw, "display");
            copy_member(&mut message, raw, "details");
            message.insert("timestamp".into(), timestamp_value(entry));
            vec![AgentMessage::from_json(message)]
        }
        "branch_summary" if js_truthy(raw.get("summary")) => {
            let mut message = role_object("branchSummary");
            copy_member(&mut message, raw, "summary");
            copy_member(&mut message, raw, "fromId");
            message.insert("timestamp".into(), timestamp_value(entry));
            vec![AgentMessage::from_json(message)]
        }
        "compaction" => {
            let mut summary = role_object("compactionSummary");
            copy_member(&mut summary, raw, "summary");
            copy_member(&mut summary, raw, "tokensBefore");
            summary.insert("timestamp".into(), timestamp_value(entry));
            let summary = AgentMessage::from_json(summary);
            match raw.get("systemMessage") {
                Some(Value::Object(system)) => {
                    vec![AgentMessage::from_json(system.clone()), summary]
                }
                _ => vec![summary],
            }
        }
        _ => Vec::new(),
    }
}

pub(crate) fn context_entries(path: Vec<&SessionEntry>) -> Vec<&SessionEntry> {
    let Some(compaction_index) = path
        .iter()
        .rposition(|entry| entry.entry_type() == "compaction")
    else {
        return path;
    };
    let Some(compaction) = path.get(compaction_index).copied() else {
        return path;
    };
    let first_kept = str_member(compaction.as_json(), "firstKeptEntryId");
    let mut entries = vec![compaction];
    let mut found_first_kept = false;
    for entry in path.iter().take(compaction_index) {
        if Some(entry.id()) == first_kept {
            found_first_kept = true;
        }
        if found_first_kept && entry.message_role() != Some("system") {
            entries.push(entry);
        }
    }
    entries.extend(path.iter().skip(compaction_index + 1));
    entries
}

fn project_entry(entry: &SessionEntry, edit: Option<&SessionEntry>) -> Vec<AgentMessage> {
    let messages = session_entry_to_context_messages(entry);
    let Some(edit) = edit else {
        return messages;
    };
    let Some(replacement) = edit.get("replacement") else {
        return messages;
    };
    if replacement.is_null() {
        return Vec::new();
    }
    let content = replacement
        .as_object()
        .and_then(|object| object.get("content"));
    messages
        .into_iter()
        .map(|mut message| {
            let role = message.role().unwrap_or("").to_owned();
            if !matches!(
                role.as_str(),
                "user" | "assistant" | "toolResult" | "custom"
            ) {
                return message;
            }
            let object = message.as_json_mut();
            match content {
                Some(Value::String(text)) if role == "assistant" || role == "toolResult" => {
                    object.insert("content".into(), text_blocks(text));
                }
                Some(content) => {
                    object.insert("content".into(), content.clone());
                }
                // `content: undefined`, which JSON omits.
                None => {
                    object.shift_remove("content");
                }
            }
            message
        })
        .collect()
}

/// `[{ type: "text", text }]`.
pub(crate) fn text_blocks(text: &str) -> Value {
    let mut block = JsonObject::new();
    block.insert("type".into(), Value::String("text".into()));
    block.insert("text".into(), Value::String(text.to_owned()));
    Value::Array(vec![Value::Object(block)])
}

pub(crate) fn projection(
    entries: &[&SessionEntry],
    leaf: LeafSelector<'_>,
    index: &EntryIndex<'_>,
) -> SessionProjection {
    let path = session_path(entries, leaf, index);
    let (thinking_level, model) = context_settings(&path);
    let context = context_entries(path);
    let mut edits: HashMap<&str, &SessionEntry> = HashMap::new();
    for entry in &context {
        if entry.entry_type() == "context_edit"
            && let Some(target) = str_member(entry.as_json(), "targetId")
        {
            edits.insert(target, entry);
        }
    }
    let projected: Vec<ProjectedSessionEntry> = context
        .iter()
        .enumerate()
        .map(|(index, entry)| ProjectedSessionEntry {
            source_entry: (*entry).clone(),
            // An older compaction can be retained inside the newest one's
            // kept range; only the newest, at index 0, contributes.
            messages: if entry.entry_type() == "compaction" && index > 0 {
                Vec::new()
            } else {
                project_entry(entry, edits.get(entry.id()).copied())
            },
        })
        .collect();
    let messages = projected
        .iter()
        .flat_map(|entry| entry.messages.iter().cloned())
        .collect();
    SessionProjection {
        entries: projected,
        messages,
        thinking_level,
        model,
    }
}

/// Pi's `buildContextEntries`: the compaction-aware entries on the path to
/// `leaf`.
pub fn build_context_entries(
    entries: &[SessionEntry],
    leaf: LeafSelector<'_>,
) -> Vec<SessionEntry> {
    let refs: Vec<&SessionEntry> = entries.iter().collect();
    let index = index_entries(&refs);
    context_entries(session_path(&refs, leaf, &index))
        .into_iter()
        .cloned()
        .collect()
}

/// Pi's `buildSessionProjection`.
pub fn build_session_projection(
    entries: &[SessionEntry],
    leaf: LeafSelector<'_>,
) -> SessionProjection {
    let refs: Vec<&SessionEntry> = entries.iter().collect();
    let index = index_entries(&refs);
    projection(&refs, leaf, &index)
}

/// Pi's `buildSessionContext`.
pub fn build_session_context(entries: &[SessionEntry], leaf: LeafSelector<'_>) -> SessionContext {
    let projection = build_session_projection(entries, leaf);
    SessionContext {
        messages: projection.messages,
        thinking_level: projection.thinking_level,
        model: projection.model,
    }
}

/// Pi's `getLatestCompactionEntry`.
pub fn get_latest_compaction_entry(entries: &[SessionEntry]) -> Option<&SessionEntry> {
    entries
        .iter()
        .rev()
        .find(|entry| entry.entry_type() == "compaction")
}
