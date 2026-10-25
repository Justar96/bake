//! Session file lines: the header and the entries of the tree.
//!
//! Ported from the types of Pi `packages/coding-agent/src/core/session-manager.ts`
//! (v1.1.0). Each line is kept as the JSON object it was read or written as,
//! so a rewrite reproduces it member for member, including members this
//! version does not know. Typed views ([`SessionEntry::typed`]) read the
//! object as Pi's entry interfaces describe it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use bake_ai::{SystemMessage, Usage, UserContent};

use crate::session::json::{JsonObject, js_stringify_object_held, str_member};
use crate::session::json_line::Verbatim;
use crate::session::messages::AgentMessage;

/// Pi's `CURRENT_SESSION_VERSION`.
pub const CURRENT_SESSION_VERSION: u64 = 3;

/// The `{"type":"session"}` line that opens a session file.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionHeader {
    object: JsonObject,
    verbatim: Verbatim,
}

impl SessionHeader {
    /// A current-version header. Members are in Pi's order, and
    /// `parentSession` is omitted when absent, as `JSON.stringify` omits
    /// `undefined`.
    pub fn new(id: &str, timestamp: &str, cwd: &str, parent_session: Option<&str>) -> Self {
        let mut object = JsonObject::new();
        object.insert("type".into(), "session".into());
        object.insert("version".into(), CURRENT_SESSION_VERSION.into());
        object.insert("id".into(), id.into());
        object.insert("timestamp".into(), timestamp.into());
        object.insert("cwd".into(), cwd.into());
        if let Some(parent) = parent_session {
            object.insert("parentSession".into(), parent.into());
        }
        Self {
            object,
            verbatim: Verbatim::default(),
        }
    }

    /// The `id` member; empty when it is not a string.
    pub fn id(&self) -> &str {
        str_member(&self.object, "id").unwrap_or("")
    }

    /// The `cwd` member, when it is a string. Version 1 headers may lack it.
    pub fn cwd(&self) -> Option<&str> {
        str_member(&self.object, "cwd")
    }

    /// The `timestamp` member, when it is a string.
    pub fn timestamp(&self) -> Option<&str> {
        str_member(&self.object, "timestamp")
    }

    /// The `parentSession` member, when it is a string.
    pub fn parent_session(&self) -> Option<&str> {
        str_member(&self.object, "parentSession")
    }

    /// The `version` member as stored; version 1 headers have none.
    pub fn version(&self) -> Option<&Value> {
        self.object.get("version")
    }

    /// The stored object.
    pub fn as_json(&self) -> &JsonObject {
        &self.object
    }
}

/// One entry of the session tree: any line other than a header whose `id`
/// is a string.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionEntry {
    id: String,
    raw: JsonObject,
    /// Text kept for values held differently from `JSON.parse`
    /// ([`crate::session::json_line`]).
    verbatim: Verbatim,
}

impl SessionEntry {
    /// Read an object as an entry; returns it unchanged when its `id` is not
    /// a string.
    pub fn from_json(raw: JsonObject) -> Result<Self, JsonObject> {
        match str_member(&raw, "id") {
            Some(id) => Ok(Self {
                id: id.to_owned(),
                raw,
                verbatim: Verbatim::default(),
            }),
            None => Err(raw),
        }
    }

    pub(crate) fn from_parts(id: String, raw: JsonObject) -> Self {
        Self {
            id,
            raw,
            verbatim: Verbatim::default(),
        }
    }

    /// The entry id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The parent id; `None` for a root (`null`), and also for a missing or
    /// non-string member, which no entry id can match.
    pub fn parent_id(&self) -> Option<&str> {
        str_member(&self.raw, "parentId")
    }

    /// The `type` member; empty when it is not a string.
    pub fn entry_type(&self) -> &str {
        str_member(&self.raw, "type").unwrap_or("")
    }

    /// The `timestamp` member, when it is a string.
    pub fn timestamp(&self) -> Option<&str> {
        str_member(&self.raw, "timestamp")
    }

    /// A member of the stored object.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.raw.get(key)
    }

    /// The stored object.
    pub fn as_json(&self) -> &JsonObject {
        &self.raw
    }

    /// The stored object, by value. Values deeper than
    /// [`crate::session::json_line::MAX_HELD_DEPTH`] are `null` in it.
    pub fn into_json(self) -> JsonObject {
        self.raw
    }

    /// The line Pi would write for this entry, without its newline.
    pub fn to_line(&self) -> String {
        js_stringify_object_held(&self.raw, &self.verbatim)
    }

    /// For a `message` entry, the message, when it is an object.
    pub fn message(&self) -> Option<AgentMessage> {
        if self.entry_type() != "message" {
            return None;
        }
        match self.raw.get("message") {
            Some(Value::Object(object)) => Some(AgentMessage::from_json(object.clone())),
            _ => None,
        }
    }

    /// For a `message` entry, the message's `role`, when it is a string.
    pub fn message_role(&self) -> Option<&str> {
        if self.entry_type() != "message" {
            return None;
        }
        self.raw
            .get("message")
            .and_then(Value::as_object)
            .and_then(|message| str_member(message, "role"))
    }

    /// Read the entry as Pi's entry interfaces describe it.
    pub fn typed(&self) -> Result<TypedEntry, serde_json::Error> {
        let value = Value::Object(self.raw.clone());
        Ok(match self.entry_type() {
            "message" => TypedEntry::Message(serde_json::from_value(value)?),
            "thinking_level_change" => {
                TypedEntry::ThinkingLevelChange(serde_json::from_value(value)?)
            }
            "model_change" => TypedEntry::ModelChange(serde_json::from_value(value)?),
            "usage" => TypedEntry::Usage(serde_json::from_value(value)?),
            "compaction" => TypedEntry::Compaction(serde_json::from_value(value)?),
            "branch_summary" => TypedEntry::BranchSummary(serde_json::from_value(value)?),
            "custom" => TypedEntry::Custom(serde_json::from_value(value)?),
            "custom_message" => TypedEntry::CustomMessage(serde_json::from_value(value)?),
            "context_edit" => TypedEntry::ContextEdit(serde_json::from_value(value)?),
            "label" => TypedEntry::Label(serde_json::from_value(value)?),
            "session_info" => TypedEntry::SessionInfo(serde_json::from_value(value)?),
            _ => TypedEntry::Unknown,
        })
    }

    pub(crate) fn set_member(&mut self, key: &str, value: Value) {
        if key == "id"
            && let Value::String(id) = &value
        {
            self.id = id.clone();
        }
        self.raw.insert(key.to_owned(), value);
    }
}

/// A parsed line of a session file.
#[derive(Debug, Clone, PartialEq)]
pub enum FileEntry {
    /// A `{"type":"session"}` line.
    Header(SessionHeader),
    /// An entry of the tree.
    Entry(SessionEntry),
    /// Another object: an entry without a string `id`. It is kept for a
    /// rewrite but takes no part in the tree. Pi indexes such a line under an
    /// `undefined` id; files Pi writes do not contain one.
    Unindexed(JsonObject),
}

impl FileEntry {
    /// Classify a parsed object.
    pub fn from_json(object: JsonObject) -> Self {
        Self::from_line(object, Verbatim::default())
    }

    /// Classify a parsed line with the text it keeps. An unindexed line
    /// drops it: Pi writes none, and it takes no part in the tree.
    pub(crate) fn from_line(object: JsonObject, verbatim: Verbatim) -> Self {
        if str_member(&object, "type") == Some("session") {
            return Self::Header(SessionHeader { object, verbatim });
        }
        match SessionEntry::from_json(object) {
            Ok(mut entry) => {
                entry.verbatim = verbatim;
                Self::Entry(entry)
            }
            Err(object) => Self::Unindexed(object),
        }
    }

    /// The stored object and its kept text.
    pub(crate) fn into_line(self) -> (JsonObject, Verbatim) {
        match self {
            Self::Header(header) => (header.object, header.verbatim),
            Self::Entry(entry) => (entry.raw, entry.verbatim),
            Self::Unindexed(object) => (object, Verbatim::default()),
        }
    }

    /// The stored object.
    pub fn as_json(&self) -> &JsonObject {
        match self {
            Self::Header(header) => header.as_json(),
            Self::Entry(entry) => entry.as_json(),
            Self::Unindexed(object) => object,
        }
    }

    /// The stored object, by value.
    pub fn into_json(self) -> JsonObject {
        match self {
            Self::Header(header) => header.object,
            Self::Entry(entry) => entry.raw,
            Self::Unindexed(object) => object,
        }
    }

    /// The line Pi would write, without its newline.
    pub fn to_line(&self) -> String {
        match self {
            Self::Header(header) => js_stringify_object_held(&header.object, &header.verbatim),
            Self::Entry(entry) => entry.to_line(),
            Self::Unindexed(object) => js_stringify_object_held(object, &Verbatim::default()),
        }
    }

    /// The entry, when this line is one.
    pub fn as_entry(&self) -> Option<&SessionEntry> {
        match self {
            Self::Entry(entry) => Some(entry),
            _ => None,
        }
    }
}

impl From<SessionEntry> for FileEntry {
    fn from(entry: SessionEntry) -> Self {
        Self::Entry(entry)
    }
}

impl From<SessionHeader> for FileEntry {
    fn from(header: SessionHeader) -> Self {
        Self::Header(header)
    }
}

/// An entry read as Pi's entry interfaces describe it.
// Unboxed variants keep matching plain; entries are read one at a time.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum TypedEntry {
    /// `message`
    Message(MessageEntry),
    /// `thinking_level_change`
    ThinkingLevelChange(ThinkingLevelChangeEntry),
    /// `model_change`
    ModelChange(ModelChangeEntry),
    /// `usage`
    Usage(UsageEntry),
    /// `compaction`
    Compaction(CompactionEntry),
    /// `branch_summary`
    BranchSummary(BranchSummaryEntry),
    /// `custom`
    Custom(CustomEntry),
    /// `custom_message`
    CustomMessage(CustomMessageEntry),
    /// `context_edit`
    ContextEdit(ContextEditEntry),
    /// `label`
    Label(LabelEntry),
    /// `session_info`
    SessionInfo(SessionInfoEntry),
    /// A type this version does not know.
    Unknown,
}

/// Pi's `SessionMessageEntry`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageEntry {
    /// The message.
    pub message: AgentMessage,
}

/// Pi's `ThinkingLevelChangeEntry`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelChangeEntry {
    /// The new level, such as `high` or `off`.
    pub thinking_level: String,
}

/// Pi's `ModelChangeEntry`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChangeEntry {
    /// Provider id.
    pub provider: String,
    /// Model id.
    pub model_id: String,
}

/// Pi's `UsageEntry`: usage outside model context, such as cache warming.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageEntry {
    /// Usage category, such as `cache_warm`.
    pub kind: String,
    /// Provider id.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// The usage.
    pub usage: Usage,
    /// A qualifier for usage notices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Pi's `CompactionEntry`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionEntry {
    /// The summary.
    pub summary: String,
    /// The first entry kept after the summary; the compaction's own id keeps
    /// none.
    pub first_kept_entry_id: String,
    /// Context tokens before compaction.
    pub tokens_before: u64,
    /// Extension data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// Usage of the summarizing calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// True when an extension wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_hook: Option<bool>,
    /// Prompt and tool state at the boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_message: Option<SystemMessage>,
}

/// Pi's `BranchSummaryEntry`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryEntry {
    /// The leaf the branch was left from, or `root`.
    pub from_id: String,
    /// The summary.
    pub summary: String,
    /// Extension data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// Usage of the summarizing call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// True when an extension wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_hook: Option<bool>,
}

/// Pi's `CustomEntry`: extension state outside model context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomEntry {
    /// The extension's type tag.
    pub custom_type: String,
    /// Extension data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Pi's `CustomMessageEntry`: extension content inside model context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessageEntry {
    /// The extension's type tag.
    pub custom_type: String,
    /// A string or text and image blocks.
    pub content: UserContent,
    /// Extension data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// Whether the interface shows it.
    pub display: bool,
}

/// Content a context edit may put in place of an entry's content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EditContent {
    /// A string; assistant and tool-result targets store it as one text block.
    Text(String),
    /// Content blocks of the target's kind, kept as JSON.
    Blocks(Vec<Value>),
}

/// The replacement of a context edit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextEditReplacement {
    /// The new content.
    pub content: EditContent,
}

/// Pi's `ContextEditEntry`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEditEntry {
    /// The edited entry.
    pub target_id: String,
    /// The new content, or `None` to omit the target from model context.
    pub replacement: Option<ContextEditReplacement>,
}

/// Pi's `LabelEntry`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelEntry {
    /// The labelled entry.
    pub target_id: String,
    /// The label; `None` or empty clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Pi's `SessionInfoEntry`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfoEntry {
    /// The display name; empty clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}
