//! The session manager.
//!
//! Ported from Pi `packages/coding-agent/src/core/session-manager.ts`
//! (v1.1.0), class `SessionManager`.
//!
//! A session is an append-only tree. Every entry has an `id` and a
//! `parentId`; the leaf pointer marks the current position, each append
//! adds a child of the leaf and moves the leaf to it, and branching moves
//! the leaf to an earlier entry so the next append starts a new branch
//! without changing history. [`SessionManager::build_session_context`]
//! resolves the messages on the path from the root to the leaf.
//!
//! A persisted session's file is created only once the session holds a user
//! or assistant message, so opening and closing without a conversation
//! leaves no file; from then on each entry is appended as it is added.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use indexmap::IndexMap;
use serde_json::Value;

use bake_ai::{Usage, UserContent};

use crate::home::{bake_home, sessions_dir};
use crate::session::SessionError;
use crate::session::context::{
    LeafSelector, SessionContext, SessionProjection, context_entries, index_entries, projection,
    session_path, text_blocks,
};
use crate::session::entry::{EditContent, FileEntry, SessionEntry, SessionHeader};
use crate::session::file::{
    HeaderError, Line, default_session_dir, default_session_dir_path, find_most_recent_session,
    is_jsonl, load_objects, migrate_lines, read_session_header, read_session_header_for_discovery,
    session_cwd_matches,
};
use crate::session::id::{assert_valid_session_id, generate_entry_id, uuid_v7};
use crate::session::json::{JsonObject, js_to_string, js_trim, js_truthy, str_member};
use crate::session::list::{
    ListProgress, SessionInfo, list_all_from_sessions_dir, list_sessions_from_dir,
    sort_session_infos,
};
use crate::session::messages::AgentMessage;
use crate::session::paths::{normalize_path_buf, resolve_path, resolve_path_string};
use crate::session::time::{js_date_ms, now_iso};

/// Pi's `NewSessionOptions`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewSessionOptions {
    /// The session id; a UUIDv7 when absent. Must pass Pi's id check.
    pub id: Option<String>,
    /// The session this one was forked from.
    pub parent_session: Option<String>,
}

impl NewSessionOptions {
    /// Options with a session id.
    pub fn with_id(id: impl Into<String>) -> Self {
        Self {
            id: Some(id.into()),
            parent_session: None,
        }
    }
}

/// One node of [`SessionTree`].
#[derive(Debug, Clone, PartialEq)]
pub struct SessionTreeNode {
    /// The entry.
    pub entry: SessionEntry,
    /// Indexes of its children in [`SessionTree::nodes`], oldest first.
    pub children: Vec<usize>,
    /// Its label, if any.
    pub label: Option<String>,
    /// When its label last changed.
    pub label_timestamp: Option<String>,
}

/// Pi's `getTree()` result as an arena: every entry is a node, and
/// `roots` lists the entries without a parent in the session, including
/// orphans whose parent is missing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionTree {
    /// The nodes, in file order.
    pub nodes: Vec<SessionTreeNode>,
    /// Indexes of the roots, in file order.
    pub roots: Vec<usize>,
}

impl SessionTree {
    /// The node at `index`.
    pub fn node(&self, index: usize) -> Option<&SessionTreeNode> {
        self.nodes.get(index)
    }

    /// The root nodes.
    pub fn root_nodes(&self) -> impl Iterator<Item = &SessionTreeNode> {
        self.roots.iter().filter_map(|index| self.nodes.get(*index))
    }

    /// The children of `node`, oldest first.
    pub fn children<'a>(
        &'a self,
        node: &'a SessionTreeNode,
    ) -> impl Iterator<Item = &'a SessionTreeNode> {
        node.children
            .iter()
            .filter_map(|index| self.nodes.get(*index))
    }

    /// The node of the entry with `id`.
    pub fn find(&self, id: &str) -> Option<&SessionTreeNode> {
        self.nodes.iter().find(|node| node.entry.id() == id)
    }
}

/// Pi's `SessionManager`.
#[derive(Debug)]
pub struct SessionManager {
    session_id: String,
    session_file: Option<PathBuf>,
    session_dir: PathBuf,
    cwd: String,
    persist: bool,
    flushed: bool,
    file_entries: Vec<FileEntry>,
    /// Entry ids to their index in `file_entries`, in first-seen order as a
    /// JavaScript `Map` keeps them.
    by_id: IndexMap<String, usize>,
    labels_by_id: IndexMap<String, String>,
    label_timestamps_by_id: HashMap<String, String>,
    leaf_id: Option<String>,
}

fn home() -> Result<PathBuf, SessionError> {
    bake_home().ok_or(SessionError::NoHome)
}

fn session_file_name(timestamp: &str, id: &str) -> String {
    format!("{}_{id}.jsonl", timestamp.replace([':', '.'], "-"))
}

fn lines_of<'a>(entries: impl IntoIterator<Item = &'a FileEntry>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for entry in entries {
        bytes.extend_from_slice(entry.to_line().as_bytes());
        bytes.push(b'\n');
    }
    bytes
}

fn append_bytes(path: &Path, bytes: &[u8]) -> Result<(), SessionError> {
    OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .and_then(|mut file| file.write_all(bytes))
        .map_err(|error| SessionError::io(path, error))
}

fn create_new_with(path: &Path, bytes: &[u8]) -> Result<(), SessionError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut file| file.write_all(bytes))
        .map_err(|error| SessionError::io(path, error))
}

fn optional(object: &mut JsonObject, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        object.insert(key.to_owned(), value);
    }
}

fn usage_value(usage: &Usage) -> Value {
    serde_json::to_value(usage).unwrap_or(Value::Null)
}

/// Pi's `getCurrentSystemMessage` over stored messages, kept as JSON so the
/// recorded tools keep every member they were declared with.
fn current_system_message(messages: &[AgentMessage]) -> Option<JsonObject> {
    let mut content: Vec<String> = Vec::new();
    let mut sections: IndexMap<String, Value> = IndexMap::new();
    let mut tools: IndexMap<String, Value> = IndexMap::new();
    let mut timestamp: Option<Value> = None;
    for message in messages
        .iter()
        .filter(|message| message.role() == Some("system"))
    {
        let message = message.as_json();
        if matches!(timestamp, None | Some(Value::Null)) {
            timestamp = message.get("timestamp").cloned();
        }
        let text = match message.get("content") {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Array(blocks)) => blocks
                .iter()
                .filter_map(Value::as_object)
                .filter(|block| str_member(block, "type") == Some("text"))
                .map(|block| js_to_string(block.get("text")))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if !text.is_empty() {
            content.push(text);
        }
        if let Some(Value::Object(patch)) = message.get("sections") {
            for (name, value) in patch {
                if value.is_null() {
                    sections.shift_remove(name);
                } else {
                    sections.insert(name.clone(), value.clone());
                }
            }
        }
        for removed in message
            .get("toolsRemoved")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(name) = removed
                .as_object()
                .and_then(|tool| str_member(tool, "name"))
            {
                tools.shift_remove(name);
            }
        }
        for added in message
            .get("toolsAdded")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(name) = added.as_object().and_then(|tool| str_member(tool, "name")) {
                tools.insert(name.to_owned(), added.clone());
            }
        }
    }
    // `timestamp === undefined`: a `null` timestamp still yields a message.
    if timestamp.is_none() && tools.is_empty() {
        return None;
    }
    let mut system = JsonObject::new();
    system.insert("role".into(), "system".into());
    system.insert("content".into(), Value::String(content.join("\n\n")));
    if !sections.is_empty() {
        system.insert(
            "sections".into(),
            Value::Object(sections.into_iter().collect()),
        );
    }
    if !tools.is_empty() {
        system.insert(
            "toolsAdded".into(),
            Value::Array(tools.into_values().collect()),
        );
    }
    system.insert(
        "timestamp".into(),
        match timestamp {
            None | Some(Value::Null) => Value::from(0),
            Some(value) => value,
        },
    );
    Some(system)
}

/// A stable merge sort with a comparator that may not be a total order, as
/// JavaScript's `sort` tolerates one. `less(a, b)` is true when `b` must
/// come before `a`.
fn stable_sort(items: &mut Vec<usize>, before: impl Fn(usize, usize) -> bool) {
    let mut width = 1;
    let len = items.len();
    let mut buffer: Vec<usize> = Vec::with_capacity(len);
    while width < len {
        buffer.clear();
        let mut start = 0;
        while start < len {
            let middle = (start + width).min(len);
            let end = (start + 2 * width).min(len);
            let (mut left, mut right) = (start, middle);
            while left < middle && right < end {
                let (Some(&a), Some(&b)) = (items.get(left), items.get(right)) else {
                    break;
                };
                if before(b, a) {
                    buffer.push(b);
                    right += 1;
                } else {
                    buffer.push(a);
                    left += 1;
                }
            }
            buffer.extend(items.get(left..middle).unwrap_or(&[]));
            buffer.extend(items.get(right..end).unwrap_or(&[]));
            start = end;
        }
        std::mem::swap(items, &mut buffer);
        width *= 2;
    }
}

impl SessionManager {
    fn construct(
        cwd: &str,
        session_dir: PathBuf,
        session_file: Option<&Path>,
        persist: bool,
        options: NewSessionOptions,
        preloaded: Option<Vec<Line>>,
    ) -> Result<Self, SessionError> {
        let mut manager = Self {
            session_id: String::new(),
            session_file: None,
            session_dir: normalize_path_buf(&session_dir),
            cwd: resolve_path_string(cwd),
            persist,
            flushed: false,
            file_entries: Vec::new(),
            by_id: IndexMap::new(),
            labels_by_id: IndexMap::new(),
            label_timestamps_by_id: HashMap::new(),
            leaf_id: None,
        };
        if persist && !manager.session_dir.as_os_str().is_empty() && !manager.session_dir.exists() {
            fs::create_dir_all(&manager.session_dir)
                .map_err(|error| SessionError::io(&manager.session_dir, error))?;
        }
        if let Some(file) = session_file {
            manager.load_session_file(file, preloaded)?;
        } else if let Some(entries) = preloaded.filter(|entries| !entries.is_empty()) {
            manager.load_entries(entries, options)?;
        } else {
            manager.new_session(options)?;
        }
        Ok(manager)
    }

    /// Switch to another session file, as resuming does: an existing session
    /// is loaded and migrated, an empty file gets a fresh header, and a
    /// missing file becomes the path of a new session.
    pub fn set_session_file(&mut self, session_file: &Path) -> Result<(), SessionError> {
        self.load_session_file(session_file, None)
    }

    fn load_session_file(
        &mut self,
        session_file: &Path,
        preloaded: Option<Vec<Line>>,
    ) -> Result<(), SessionError> {
        let path = resolve_path(session_file);
        self.session_file = Some(path.clone());
        if !path.exists() {
            self.new_session(NewSessionOptions::default())?;
            self.session_file = Some(path);
            return Ok(());
        }
        let entries = match preloaded {
            Some(entries) => entries,
            None => load_objects(&path).map_err(|error| SessionError::io(&path, error))?,
        };
        // An empty file is initialized with a header; a non-empty file that
        // did not read as a session fails without being modified.
        if entries.is_empty() {
            let size = fs::metadata(&path)
                .map_err(|error| SessionError::io(&path, error))?
                .len();
            if size > 0 {
                return Err(SessionError::NotASession(path));
            }
            self.new_session(NewSessionOptions::default())?;
            self.session_file = Some(path);
            self.rewrite_file()?;
            self.flushed = true;
            return Ok(());
        }
        self.load_entries(entries, NewSessionOptions::default())?;
        self.flushed = true;
        Ok(())
    }

    /// Start a new session in this manager, replacing its entries. Returns
    /// the new session's file path when persisting; the file is written once
    /// the session holds a user or assistant message.
    pub fn new_session(
        &mut self,
        options: NewSessionOptions,
    ) -> Result<Option<PathBuf>, SessionError> {
        if let Some(id) = &options.id {
            assert_valid_session_id(id)?;
        }
        let id = options.id.unwrap_or_else(uuid_v7);
        let timestamp = now_iso();
        let header = SessionHeader::new(
            &id,
            &timestamp,
            &self.cwd,
            options.parent_session.as_deref(),
        );
        self.session_id = id;
        self.file_entries = vec![FileEntry::Header(header)];
        self.by_id.clear();
        self.labels_by_id.clear();
        self.label_timestamps_by_id.clear();
        self.leaf_id = None;
        self.flushed = false;
        if self.persist {
            self.session_file = Some(
                self.session_dir
                    .join(session_file_name(&timestamp, &self.session_id)),
            );
        }
        Ok(self.session_file.clone())
    }

    fn load_entries(
        &mut self,
        entries: Vec<Line>,
        options: NewSessionOptions,
    ) -> Result<(), SessionError> {
        let header = entries
            .iter()
            .find(|(entry, _)| str_member(entry, "type") == Some("session"))
            .map(|(header, _)| str_member(header, "id").unwrap_or("").to_owned());
        let from_line = |(object, verbatim): Line| FileEntry::from_line(object, verbatim);
        let rewrite = match header {
            Some(id) => {
                let (entries, migrated) = migrate_lines(entries);
                self.session_id = id;
                self.file_entries = entries.into_iter().map(from_line).collect();
                migrated
            }
            None => {
                self.new_session(options)?;
                self.file_entries.extend(entries.into_iter().map(from_line));
                false
            }
        };
        self.build_index();
        if rewrite {
            self.rewrite_file()?;
        }
        Ok(())
    }

    fn build_index(&mut self) {
        self.by_id.clear();
        self.labels_by_id.clear();
        self.label_timestamps_by_id.clear();
        self.leaf_id = None;
        for (index, entry) in self.file_entries.iter().enumerate() {
            let FileEntry::Entry(entry) = entry else {
                continue;
            };
            self.by_id.insert(entry.id().to_owned(), index);
            self.leaf_id = Some(entry.id().to_owned());
            if entry.entry_type() != "label" {
                continue;
            }
            let Some(target) = str_member(entry.as_json(), "targetId") else {
                continue;
            };
            if js_truthy(entry.get("label")) {
                self.labels_by_id
                    .insert(target.to_owned(), js_to_string(entry.get("label")));
                self.label_timestamps_by_id.insert(
                    target.to_owned(),
                    entry.timestamp().unwrap_or("").to_owned(),
                );
            } else {
                self.labels_by_id.shift_remove(target);
                self.label_timestamps_by_id.remove(target);
            }
        }
    }

    fn rewrite_file(&self) -> Result<(), SessionError> {
        let (true, Some(path)) = (self.persist, &self.session_file) else {
            return Ok(());
        };
        fs::write(path, lines_of(&self.file_entries)).map_err(|error| SessionError::io(path, error))
    }

    /// Whether this session writes to a file.
    pub fn is_persisted(&self) -> bool {
        self.persist
    }

    /// The working directory, resolved.
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// The session directory; empty for an in-memory session.
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    /// Whether the session directory is the Bake home's default for the
    /// working directory.
    pub fn uses_default_session_dir(&self) -> bool {
        bake_home()
            .is_some_and(|home| self.session_dir == default_session_dir_path(&self.cwd, &home))
    }

    /// The session id.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The session file; `None` for an in-memory session.
    pub fn session_file(&self) -> Option<&Path> {
        self.session_file.as_deref()
    }

    fn has_conversation(&self) -> bool {
        self.file_entries.iter().any(|entry| {
            entry
                .as_entry()
                .and_then(SessionEntry::message_role)
                .is_some_and(|role| role == "user" || role == "assistant")
        })
    }

    fn persist_last(&mut self) -> Result<(), SessionError> {
        let Some(path) = self.session_file.clone().filter(|_| self.persist) else {
            return Ok(());
        };
        if !self.flushed {
            if !self.has_conversation() {
                return Ok(());
            }
            create_new_with(&path, &lines_of(&self.file_entries))?;
            self.flushed = true;
            return Ok(());
        }
        match self.file_entries.last() {
            Some(entry) => append_bytes(&path, &lines_of([entry])),
            None => Ok(()),
        }
    }

    fn next_id(&self) -> String {
        generate_entry_id(|candidate| self.by_id.contains_key(candidate))
    }

    fn base(&self, entry_type: &str, id: &str) -> JsonObject {
        let mut object = JsonObject::new();
        object.insert("type".into(), Value::String(entry_type.to_owned()));
        object.insert("id".into(), Value::String(id.to_owned()));
        object.insert(
            "parentId".into(),
            self.leaf_id.clone().map_or(Value::Null, Value::String),
        );
        object.insert("timestamp".into(), Value::String(now_iso()));
        object
    }

    fn append_entry(&mut self, id: String, raw: JsonObject) -> Result<String, SessionError> {
        self.file_entries
            .push(FileEntry::Entry(SessionEntry::from_parts(id.clone(), raw)));
        self.by_id.insert(id.clone(), self.file_entries.len() - 1);
        self.leaf_id = Some(id.clone());
        self.persist_last()?;
        Ok(id)
    }

    /// Append a message as a child of the leaf and move the leaf to it.
    /// Returns the entry id. Compaction and branch summaries are entries of
    /// their own; use [`Self::append_compaction`] and
    /// [`Self::branch_with_summary`] for them.
    ///
    /// Pi's signature (`Message | CustomMessage | BashExecutionMessage`)
    /// excludes the `branchSummary` and `compactionSummary` roles; here they
    /// are refused with [`SessionError::SummaryMessage`] before anything
    /// changes.
    ///
    /// When writing fails, the entry stays in memory, as in Pi, and the
    /// error is returned; every append behaves this way.
    pub fn append_message(
        &mut self,
        message: impl Into<AgentMessage>,
    ) -> Result<String, SessionError> {
        let message = message.into();
        if let Some(role @ ("branchSummary" | "compactionSummary")) = message.role() {
            return Err(SessionError::SummaryMessage(role.to_owned()));
        }
        let id = self.next_id();
        let mut raw = self.base("message", &id);
        raw.insert("message".into(), Value::Object(message.into_json()));
        self.append_entry(id, raw)
    }

    /// Append a thinking-level change. Returns the entry id.
    pub fn append_thinking_level_change(
        &mut self,
        thinking_level: &str,
    ) -> Result<String, SessionError> {
        let id = self.next_id();
        let mut raw = self.base("thinking_level_change", &id);
        raw.insert("thinkingLevel".into(), thinking_level.into());
        self.append_entry(id, raw)
    }

    /// Append a model change. Returns the entry id.
    pub fn append_model_change(
        &mut self,
        provider: &str,
        model_id: &str,
    ) -> Result<String, SessionError> {
        let id = self.next_id();
        let mut raw = self.base("model_change", &id);
        raw.insert("provider".into(), provider.into());
        raw.insert("modelId".into(), model_id.into());
        self.append_entry(id, raw)
    }

    /// Append usage outside model context, such as cache warming. An empty
    /// `note` is omitted. Returns the entry.
    pub fn append_usage(
        &mut self,
        kind: &str,
        provider: &str,
        model: &str,
        usage: &Usage,
        note: Option<&str>,
    ) -> Result<SessionEntry, SessionError> {
        let id = self.next_id();
        let mut raw = self.base("usage", &id);
        raw.insert("kind".into(), kind.into());
        raw.insert("provider".into(), provider.into());
        raw.insert("model".into(), model.into());
        raw.insert("usage".into(), usage_value(usage));
        optional(
            &mut raw,
            "note",
            note.filter(|note| !note.is_empty()).map(Value::from),
        );
        let entry = SessionEntry::from_parts(id.clone(), raw.clone());
        self.append_entry(id, raw)?;
        Ok(entry)
    }

    /// Append a compaction. `first_kept_entry_id` of `None` keeps no earlier
    /// entry: the compaction names itself. The prompt and tool state of the
    /// current context is recorded with it. Returns the entry id.
    pub fn append_compaction(
        &mut self,
        summary: &str,
        first_kept_entry_id: Option<&str>,
        tokens_before: u64,
        details: Option<Value>,
        from_hook: Option<bool>,
        usage: Option<&Usage>,
    ) -> Result<String, SessionError> {
        let timestamp = now_iso();
        let system_message = current_system_message(&self.build_session_projection().messages);
        let id = self.next_id();
        let mut raw = JsonObject::new();
        raw.insert("type".into(), "compaction".into());
        raw.insert("id".into(), Value::String(id.clone()));
        raw.insert(
            "parentId".into(),
            self.leaf_id.clone().map_or(Value::Null, Value::String),
        );
        raw.insert("timestamp".into(), Value::String(timestamp.clone()));
        raw.insert("summary".into(), summary.into());
        raw.insert(
            "firstKeptEntryId".into(),
            Value::String(first_kept_entry_id.unwrap_or(&id).to_owned()),
        );
        raw.insert("tokensBefore".into(), tokens_before.into());
        optional(&mut raw, "details", details);
        optional(&mut raw, "usage", usage.map(usage_value));
        optional(&mut raw, "fromHook", from_hook.map(Value::from));
        if let Some(mut system) = system_message {
            let ms = js_date_ms(Some(&Value::String(timestamp))).map_or(Value::Null, Value::from);
            system.insert("timestamp".into(), ms);
            raw.insert("systemMessage".into(), Value::Object(system));
        }
        self.append_entry(id, raw)
    }

    /// Append extension state outside model context. Returns the entry id.
    pub fn append_custom_entry(
        &mut self,
        custom_type: &str,
        data: Option<Value>,
    ) -> Result<String, SessionError> {
        let id = self.next_id();
        let base = self.base("custom", &id);
        let mut raw = JsonObject::new();
        raw.insert("type".into(), "custom".into());
        raw.insert("customType".into(), custom_type.into());
        optional(&mut raw, "data", data);
        raw.extend(base.into_iter().filter(|(key, _)| key != "type"));
        self.append_entry(id, raw)
    }

    /// Append a display name; line breaks become spaces and the name is
    /// trimmed. Returns the entry id.
    pub fn append_session_info(&mut self, name: &str) -> Result<String, SessionError> {
        let mut sanitized = String::with_capacity(name.len());
        let mut in_break = false;
        for ch in name.chars() {
            if ch == '\r' || ch == '\n' {
                if !in_break {
                    sanitized.push(' ');
                }
                in_break = true;
            } else {
                sanitized.push(ch);
                in_break = false;
            }
        }
        let id = self.next_id();
        let mut raw = self.base("session_info", &id);
        raw.insert("name".into(), js_trim(&sanitized).into());
        self.append_entry(id, raw)
    }

    /// The display name from the latest `session_info` entry; an empty name
    /// clears it.
    pub fn session_name(&self) -> Option<String> {
        self.file_entries
            .iter()
            .rev()
            .filter_map(FileEntry::as_entry)
            .find(|entry| entry.entry_type() == "session_info")
            .and_then(|entry| entry.get("name").and_then(Value::as_str))
            .map(|name| js_trim(name).to_owned())
            .filter(|name| !name.is_empty())
    }

    /// Append extension content that takes part in model context as a user
    /// message. Returns the entry id.
    pub fn append_custom_message_entry(
        &mut self,
        custom_type: &str,
        content: UserContent,
        display: bool,
        details: Option<Value>,
    ) -> Result<String, SessionError> {
        let id = self.next_id();
        let base = self.base("custom_message", &id);
        let mut raw = JsonObject::new();
        raw.insert("type".into(), "custom_message".into());
        raw.insert("customType".into(), custom_type.into());
        raw.insert(
            "content".into(),
            serde_json::to_value(&content).unwrap_or(Value::Null),
        );
        raw.insert("display".into(), display.into());
        optional(&mut raw, "details", details);
        raw.extend(base.into_iter().filter(|(key, _)| key != "type"));
        self.append_entry(id, raw)
    }

    /// Append a branch-local edit of an earlier entry's model content:
    /// `None` omits it from context, content replaces its content. The
    /// target must be a user, assistant, tool-result, or custom message on
    /// the active branch. A string for an assistant or tool-result target is
    /// stored as one text block. Returns the entry id.
    pub fn append_context_edit(
        &mut self,
        target_id: &str,
        replacement: Option<EditContent>,
    ) -> Result<String, SessionError> {
        let Some(target) = self.entry(target_id) else {
            return Err(SessionError::EntryNotFound(target_id.to_owned()));
        };
        let role = if target.entry_type() == "custom_message" {
            Some("custom")
        } else {
            target
                .message_role()
                .filter(|role| matches!(*role, "user" | "assistant" | "toolResult"))
        };
        let role = role.map(str::to_owned);
        if !self
            .branch_entries(None)
            .iter()
            .any(|entry| entry.id() == target_id)
        {
            return Err(SessionError::NotOnActiveBranch(target_id.to_owned()));
        }
        let Some(role) = role else {
            return Err(SessionError::NotEditable(target_id.to_owned()));
        };
        let replacement = replacement.map(|content| {
            let content = match content {
                EditContent::Text(text) if role == "assistant" || role == "toolResult" => {
                    text_blocks(&text)
                }
                EditContent::Text(text) => Value::String(text),
                EditContent::Blocks(blocks) => Value::Array(blocks),
            };
            let mut object = JsonObject::new();
            object.insert("content".into(), content);
            Value::Object(object)
        });
        let id = self.next_id();
        let mut raw = self.base("context_edit", &id);
        raw.insert("targetId".into(), target_id.into());
        raw.insert("replacement".into(), replacement.unwrap_or(Value::Null));
        self.append_entry(id, raw)
    }

    /// The leaf id; `None` before the first entry or after
    /// [`Self::reset_leaf`].
    pub fn leaf_id(&self) -> Option<&str> {
        self.leaf_id.as_deref()
    }

    /// The leaf entry.
    pub fn leaf_entry(&self) -> Option<&SessionEntry> {
        // `this.leafId ? ... : undefined`: an empty id is falsy.
        self.leaf_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .and_then(|id| self.entry(id))
    }

    /// The entry with `id`; for a repeated id, its last entry.
    pub fn entry(&self, id: &str) -> Option<&SessionEntry> {
        let index = *self.by_id.get(id)?;
        self.file_entries.get(index).and_then(FileEntry::as_entry)
    }

    /// The direct children of an entry.
    pub fn children(&self, parent_id: &str) -> Vec<&SessionEntry> {
        self.by_id
            .values()
            .filter_map(|index| self.file_entries.get(*index).and_then(FileEntry::as_entry))
            .filter(|entry| entry.parent_id() == Some(parent_id))
            .collect()
    }

    /// The label of an entry.
    pub fn label(&self, id: &str) -> Option<&str> {
        self.labels_by_id.get(id).map(String::as_str)
    }

    /// Set a label on an entry, or clear it with `None` or an empty label.
    /// Returns the label entry's id.
    pub fn append_label_change(
        &mut self,
        target_id: &str,
        label: Option<&str>,
    ) -> Result<String, SessionError> {
        if !self.by_id.contains_key(target_id) {
            return Err(SessionError::EntryNotFound(target_id.to_owned()));
        }
        let id = self.next_id();
        let mut raw = self.base("label", &id);
        raw.insert("targetId".into(), target_id.into());
        optional(&mut raw, "label", label.map(Value::from));
        let timestamp = raw
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let id = self.append_entry(id, raw)?;
        match label.filter(|label| !label.is_empty()) {
            Some(label) => {
                self.labels_by_id
                    .insert(target_id.to_owned(), label.to_owned());
                self.label_timestamps_by_id
                    .insert(target_id.to_owned(), timestamp);
            }
            None => {
                self.labels_by_id.shift_remove(target_id);
                self.label_timestamps_by_id.remove(target_id);
            }
        }
        Ok(id)
    }

    /// Pi's `getBranch`: the entries from the root to `from_id` (the leaf
    /// when `None`), of every type.
    pub fn branch_entries(&self, from_id: Option<&str>) -> Vec<&SessionEntry> {
        let start = from_id.or(self.leaf_id.as_deref());
        let mut path = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        // Empty ids are falsy in JavaScript and link nowhere.
        let mut current = start
            .filter(|id| !id.is_empty())
            .and_then(|id| self.entry(id));
        while let Some(entry) = current {
            // Pi loops forever on a parent cycle; stop at the repeat.
            if !seen.insert(entry.id()) {
                break;
            }
            path.push(entry);
            current = entry
                .parent_id()
                .filter(|parent| !parent.is_empty())
                .and_then(|parent| self.entry(parent));
        }
        path.reverse();
        path
    }

    /// The compaction-aware entries on the path to the leaf.
    pub fn build_context_entries(&self) -> Vec<SessionEntry> {
        let refs = self.entries();
        let index = index_entries(&refs);
        let leaf = LeafSelector::from(self.leaf_id.as_deref());
        context_entries(session_path(&refs, leaf, &index))
            .into_iter()
            .cloned()
            .collect()
    }

    /// The context entries with the messages each contributes.
    pub fn build_session_projection(&self) -> SessionProjection {
        let refs = self.entries();
        let index = index_entries(&refs);
        projection(&refs, LeafSelector::from(self.leaf_id.as_deref()), &index)
    }

    /// What the agent resumes with: the context messages, thinking level, and
    /// model.
    pub fn build_session_context(&self) -> SessionContext {
        let projection = self.build_session_projection();
        SessionContext {
            messages: projection.messages,
            thinking_level: projection.thinking_level,
            model: projection.model,
        }
    }

    /// The first header.
    pub fn header(&self) -> Option<&SessionHeader> {
        self.file_entries.iter().find_map(|entry| match entry {
            FileEntry::Header(header) => Some(header),
            _ => None,
        })
    }

    /// The number of distinct entry ids.
    pub fn entry_count(&self) -> usize {
        self.by_id.len()
    }

    /// Every entry, in file order, headers excluded.
    pub fn entries(&self) -> Vec<&SessionEntry> {
        self.file_entries
            .iter()
            .filter_map(FileEntry::as_entry)
            .collect()
    }

    /// Every parsed line, in file order.
    pub fn file_entries(&self) -> &[FileEntry] {
        &self.file_entries
    }

    /// The session as a tree. Each node's children are ordered by timestamp,
    /// oldest first; an entry whose parent is missing or itself is a root.
    pub fn tree(&self) -> SessionTree {
        let entries = self.entries();
        let mut node_of: HashMap<&str, usize> = HashMap::with_capacity(entries.len());
        let mut nodes: Vec<SessionTreeNode> = Vec::with_capacity(entries.len());
        for (index, entry) in entries.iter().enumerate() {
            node_of.insert(entry.id(), index);
            nodes.push(SessionTreeNode {
                entry: (*entry).clone(),
                children: Vec::new(),
                label: self.labels_by_id.get(entry.id()).cloned(),
                label_timestamp: self.label_timestamps_by_id.get(entry.id()).cloned(),
            });
        }
        let mut roots = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let parent = entry
                .parent_id()
                .filter(|parent| *parent != entry.id())
                .and_then(|parent| node_of.get(parent).copied());
            match parent.and_then(|parent| nodes.get_mut(parent)) {
                Some(parent) => parent.children.push(index),
                None => roots.push(index),
            }
        }
        let times: Vec<Option<i64>> = entries
            .iter()
            .map(|entry| js_date_ms(entry.get("timestamp")))
            .collect();
        let time = |index: usize| times.get(index).copied().flatten();
        for node in &mut nodes {
            stable_sort(&mut node.children, |a, b| match (time(a), time(b)) {
                (Some(a), Some(b)) => a < b,
                _ => false,
            });
        }
        SessionTree { nodes, roots }
    }

    /// Move the leaf to an earlier entry; the next append starts a branch
    /// there.
    pub fn branch(&mut self, branch_from_id: &str) -> Result<(), SessionError> {
        if !self.by_id.contains_key(branch_from_id) {
            return Err(SessionError::EntryNotFound(branch_from_id.to_owned()));
        }
        self.leaf_id = Some(branch_from_id.to_owned());
        Ok(())
    }

    /// Move the leaf before the first entry; the next append is a new root.
    pub fn reset_leaf(&mut self) {
        self.leaf_id = None;
    }

    /// Branch from `branch_from_id` (before the first entry for `None`) and
    /// record a summary of the branch left behind, whose leaf becomes the
    /// summary's `fromId` (`root` when there was none). Returns the summary's
    /// entry id, the new leaf.
    pub fn branch_with_summary(
        &mut self,
        branch_from_id: Option<&str>,
        summary: &str,
        details: Option<Value>,
        from_hook: Option<bool>,
        usage: Option<&Usage>,
    ) -> Result<String, SessionError> {
        if let Some(id) = branch_from_id
            && !self.by_id.contains_key(id)
        {
            return Err(SessionError::EntryNotFound(id.to_owned()));
        }
        let from_id = self.leaf_id.clone().unwrap_or_else(|| "root".to_owned());
        self.leaf_id = branch_from_id.map(str::to_owned);
        let id = self.next_id();
        let mut raw = self.base("branch_summary", &id);
        raw.insert("fromId".into(), Value::String(from_id));
        raw.insert("summary".into(), summary.into());
        optional(&mut raw, "details", details);
        optional(&mut raw, "usage", usage.map(usage_value));
        optional(&mut raw, "fromHook", from_hook.map(Value::from));
        self.append_entry(id, raw)
    }

    /// Replace this session with a new one holding only the path from the
    /// root to `leaf_id`, labels on that path carried over as new label
    /// entries at its end. A persisted session moves to a new file, written
    /// now when the path holds a conversation, and its path is returned; an
    /// in-memory session returns `None`.
    pub fn create_branched_session(
        &mut self,
        leaf_id: &str,
    ) -> Result<Option<PathBuf>, SessionError> {
        let previous_file = self.session_file.clone();
        let path: Vec<SessionEntry> = self
            .branch_entries(Some(leaf_id))
            .into_iter()
            .cloned()
            .collect();
        if path.is_empty() {
            return Err(SessionError::EntryNotFound(leaf_id.to_owned()));
        }
        // Labels are entries of the tree: dropping them re-chains the path,
        // and a compaction that kept from a label keeps from the entry after.
        let mut kept: Vec<SessionEntry> = Vec::new();
        let mut replacement_by_label: HashMap<String, String> = HashMap::new();
        let mut pending_labels: Vec<String> = Vec::new();
        let mut parent: Option<String> = None;
        for entry in path {
            if entry.entry_type() == "label" {
                pending_labels.push(entry.id().to_owned());
                continue;
            }
            for label in pending_labels.drain(..) {
                replacement_by_label.insert(label, entry.id().to_owned());
            }
            let id = entry.id().to_owned();
            let mut copy = entry;
            copy.set_member(
                "parentId",
                parent.clone().map_or(Value::Null, Value::String),
            );
            if copy.entry_type() == "compaction"
                && let Some(first_kept) = copy.get("firstKeptEntryId").cloned()
            {
                let replaced = match first_kept.as_str() {
                    Some(kept_id) if kept_id != id => replacement_by_label
                        .get(kept_id)
                        .map_or(first_kept.clone(), |kept| Value::String(kept.clone())),
                    _ => first_kept,
                };
                copy.set_member("firstKeptEntryId", replaced);
            }
            parent = Some(id);
            kept.push(copy);
        }

        let new_session_id = uuid_v7();
        let timestamp = now_iso();
        let new_file = self
            .session_dir
            .join(session_file_name(&timestamp, &new_session_id));
        let parent_session = if self.persist {
            previous_file.map(|file| file.to_string_lossy().into_owned())
        } else {
            None
        };
        let header = SessionHeader::new(
            &new_session_id,
            &timestamp,
            &self.cwd,
            parent_session.as_deref(),
        );

        let mut ids: HashSet<String> = kept.iter().map(|entry| entry.id().to_owned()).collect();
        let mut label_parent = kept
            .last()
            .map(|entry| entry.id().to_owned())
            .filter(|id| !id.is_empty());
        let mut label_entries: Vec<SessionEntry> = Vec::new();
        for (target, label) in &self.labels_by_id {
            if !kept.iter().any(|entry| entry.id() == target) {
                continue;
            }
            let id = generate_entry_id(|candidate| ids.contains(candidate));
            ids.insert(id.clone());
            let mut raw = JsonObject::new();
            raw.insert("type".into(), "label".into());
            raw.insert("id".into(), Value::String(id.clone()));
            raw.insert(
                "parentId".into(),
                label_parent.clone().map_or(Value::Null, Value::String),
            );
            raw.insert(
                "timestamp".into(),
                self.label_timestamps_by_id
                    .get(target)
                    .map_or(Value::Null, |time| Value::String(time.clone())),
            );
            raw.insert("targetId".into(), Value::String(target.clone()));
            raw.insert("label".into(), Value::String(label.clone()));
            label_parent = Some(id.clone());
            label_entries.push(SessionEntry::from_parts(id, raw));
        }

        self.file_entries = std::iter::once(FileEntry::Header(header))
            .chain(kept.into_iter().map(FileEntry::Entry))
            .chain(label_entries.into_iter().map(FileEntry::Entry))
            .collect();
        self.session_id = new_session_id;
        if !self.persist {
            self.build_index();
            return Ok(None);
        }
        self.session_file = Some(new_file.clone());
        self.build_index();
        // As in a first append: write now only when there is a conversation.
        if self.has_conversation() {
            self.rewrite_file()?;
            self.flushed = true;
        } else {
            self.flushed = false;
        }
        Ok(Some(new_file))
    }

    /// Create a persisted session. Without `session_dir`, it lives in the
    /// Bake home's directory for `cwd`, which is created. The file is
    /// written once the session holds a user or assistant message.
    pub fn create(
        cwd: &str,
        session_dir: Option<&Path>,
        options: NewSessionOptions,
    ) -> Result<Self, SessionError> {
        let dir = match session_dir {
            Some(dir) => normalize_path_buf(dir),
            None => {
                let home = home()?;
                default_session_dir(cwd, &home).map_err(|error| {
                    SessionError::io(default_session_dir_path(cwd, &home), error)
                })?
            }
        };
        Self::construct(cwd, dir, None, true, options, None)
    }

    /// Open a session file. Without `session_dir`, new and branched
    /// sessions go beside it. The working directory is `cwd_override`, else
    /// the header's, else the process's.
    pub fn open(
        path: &Path,
        session_dir: Option<&Path>,
        cwd_override: Option<&str>,
    ) -> Result<Self, SessionError> {
        let resolved = resolve_path(path);
        let mut header_cwd: Option<String> = None;
        let mut preloaded: Option<Vec<Line>> = None;
        if cwd_override.is_none() && resolved.exists() {
            match read_session_header(&resolved) {
                Ok(header) => {
                    header_cwd = header.and_then(|header| header.cwd().map(str::to_owned))
                }
                Err(HeaderError::Io(error)) => return Err(SessionError::io(&resolved, error)),
                // The bounded scan only serves discovery; a full load decides
                // for files with very large headers or prefixes.
                Err(HeaderError::ScanLimit) => {
                    let entries = load_objects(&resolved)
                        .map_err(|error| SessionError::io(&resolved, error))?;
                    header_cwd = entries
                        .first()
                        .filter(|(first, _)| str_member(first, "type") == Some("session"))
                        .and_then(|(first, _)| str_member(first, "cwd"))
                        .map(str::to_owned);
                    preloaded = Some(entries);
                }
            }
        }
        let cwd = match (cwd_override, header_cwd) {
            (Some(cwd), _) => cwd.to_owned(),
            (None, Some(cwd)) => cwd,
            (None, None) => std::env::current_dir()
                .map(|dir| dir.to_string_lossy().into_owned())
                .unwrap_or_else(|_| ".".to_owned()),
        };
        let dir = match session_dir {
            Some(dir) => normalize_path_buf(dir),
            None => resolved
                .parent()
                .map_or_else(|| resolved.clone(), Path::to_path_buf),
        };
        Self::construct(
            &cwd,
            dir,
            Some(&resolved),
            true,
            NewSessionOptions::default(),
            preloaded,
        )
    }

    /// Continue the most recently modified session, or create one. In a
    /// session directory other than the default for `cwd`, only sessions
    /// started in `cwd` count.
    pub fn continue_recent(cwd: &str, session_dir: Option<&Path>) -> Result<Self, SessionError> {
        let home = bake_home();
        let dir = match session_dir {
            Some(dir) => normalize_path_buf(dir),
            None => {
                let home = home.as_deref().ok_or(SessionError::NoHome)?;
                default_session_dir(cwd, home)
                    .map_err(|error| SessionError::io(default_session_dir_path(cwd, home), error))?
            }
        };
        let filter_cwd = session_dir.is_some()
            && home.is_none_or(|home| dir != default_session_dir_path(cwd, &home));
        let most_recent = find_most_recent_session(&dir, filter_cwd.then_some(cwd));
        Self::construct(
            cwd,
            dir,
            most_recent.as_deref(),
            true,
            NewSessionOptions::default(),
            None,
        )
    }

    /// An in-memory session that writes no file, optionally adopting
    /// entries held elsewhere. Entries with a header take its identity and
    /// are migrated; entries without one get a new header from `options` and
    /// are adopted as current-version entries.
    pub fn in_memory(
        cwd: Option<&str>,
        options: NewSessionOptions,
        entries: Vec<FileEntry>,
    ) -> Result<Self, SessionError> {
        let cwd = match cwd {
            Some(cwd) => cwd.to_owned(),
            None => std::env::current_dir()
                .map(|dir| dir.to_string_lossy().into_owned())
                .unwrap_or_else(|_| ".".to_owned()),
        };
        let objects: Vec<Line> = entries.into_iter().map(FileEntry::into_line).collect();
        Self::construct(&cwd, PathBuf::new(), None, false, options, Some(objects))
    }

    /// Fork a session file, possibly from another project, into a new
    /// session for `target_cwd` holding all of its entries, with a header
    /// naming the source as its parent. Without `session_dir`, it lives in
    /// the Bake home's directory for `target_cwd`.
    pub fn fork_from(
        source_path: &Path,
        target_cwd: &str,
        session_dir: Option<&Path>,
        options: NewSessionOptions,
    ) -> Result<Self, SessionError> {
        let source = resolve_path(source_path);
        let target_cwd = resolve_path_string(target_cwd);
        let entries = load_objects(&source).map_err(|error| SessionError::io(&source, error))?;
        if entries.is_empty() {
            return Err(SessionError::InvalidForkSource(source));
        }
        let dir = match session_dir {
            Some(dir) => normalize_path_buf(dir),
            None => {
                let home = home()?;
                default_session_dir(&target_cwd, &home).map_err(|error| {
                    SessionError::io(default_session_dir_path(&target_cwd, &home), error)
                })?
            }
        };
        fs::create_dir_all(&dir).map_err(|error| SessionError::io(&dir, error))?;
        if let Some(id) = &options.id {
            assert_valid_session_id(id)?;
        }
        let id = options.id.unwrap_or_else(uuid_v7);
        let timestamp = now_iso();
        let file = dir.join(session_file_name(&timestamp, &id));
        let header = SessionHeader::new(
            &id,
            &timestamp,
            &target_cwd,
            Some(&source.to_string_lossy()),
        );
        create_new_with(&file, &lines_of([&FileEntry::Header(header)]))?;
        let body: Vec<FileEntry> = entries
            .into_iter()
            .filter(|(entry, _)| str_member(entry, "type") != Some("session"))
            .map(|(object, verbatim)| FileEntry::from_line(object, verbatim))
            .collect();
        if !body.is_empty() {
            append_bytes(&file, &lines_of(&body))?;
        }
        Self::construct(
            &target_cwd,
            dir,
            Some(&file),
            true,
            NewSessionOptions::default(),
            None,
        )
    }

    /// The file of the session with exactly `id`, reading only headers. In a
    /// session directory other than the default for `cwd`, only sessions
    /// started in `cwd` count.
    pub fn find_by_id(
        cwd: &str,
        id: &str,
        session_dir: Option<&Path>,
    ) -> Result<Option<PathBuf>, SessionError> {
        let home = bake_home();
        let dir = match session_dir {
            Some(dir) => normalize_path_buf(dir),
            None => {
                let home = home.as_deref().ok_or(SessionError::NoHome)?;
                default_session_dir(cwd, home)
                    .map_err(|error| SessionError::io(default_session_dir_path(cwd, home), error))?
            }
        };
        let filter_cwd = session_dir.is_some()
            && home.is_none_or(|home| dir != default_session_dir_path(cwd, &home));
        let resolved_cwd = resolve_path_string(cwd);
        let Ok(read_dir) = fs::read_dir(&dir) else {
            return Ok(None);
        };
        for entry in read_dir {
            let Ok(entry) = entry else {
                return Ok(None);
            };
            if !is_jsonl(&entry.file_name()) {
                continue;
            }
            let path = dir.join(entry.file_name());
            let Some(header) = read_session_header_for_discovery(&path) else {
                continue;
            };
            if header.id() != id {
                continue;
            }
            if filter_cwd && !session_cwd_matches(header.cwd(), &resolved_cwd) {
                continue;
            }
            return Ok(Some(path));
        }
        Ok(None)
    }

    /// The sessions of one directory, newest first. Without `session_dir`,
    /// the Bake home's directory for `cwd`; in another directory, only
    /// sessions started in `cwd`. `progress` reports each file loaded;
    /// setting `cancel` stops the listing with [`SessionError::Aborted`].
    pub fn list(
        cwd: &str,
        session_dir: Option<&Path>,
        progress: Option<&mut ListProgress<'_>>,
        cancel: Option<&AtomicBool>,
    ) -> Result<Vec<SessionInfo>, SessionError> {
        let home = bake_home();
        let dir = match session_dir {
            Some(dir) => normalize_path_buf(dir),
            None => {
                let home = home.as_deref().ok_or(SessionError::NoHome)?;
                default_session_dir(cwd, home)
                    .map_err(|error| SessionError::io(default_session_dir_path(cwd, home), error))?
            }
        };
        let filter_cwd = session_dir.is_some()
            && home.is_none_or(|home| dir != default_session_dir_path(cwd, &home));
        let resolved_cwd = resolve_path_string(cwd);
        let include = |session: &SessionInfo| {
            !filter_cwd || session_cwd_matches(Some(&session.cwd), &resolved_cwd)
        };
        let mut sessions = match progress {
            Some(progress) => {
                let mut filtered =
                    |loaded: usize, total: usize, partial: Option<&[SessionInfo]>| {
                        let partial: Option<Vec<SessionInfo>> = partial.map(|partial| {
                            partial
                                .iter()
                                .filter(|session| include(session))
                                .cloned()
                                .collect()
                        });
                        progress(loaded, total, partial.as_deref());
                    };
                list_sessions_from_dir(&dir, Some(&mut filtered), cancel)?
            }
            None => list_sessions_from_dir(&dir, None, cancel)?,
        };
        sessions.retain(|session| include(session));
        sort_session_infos(&mut sessions);
        Ok(sessions)
    }

    /// Every session, newest first: in `session_dir` when given, else in
    /// every directory under the Bake home's `sessions` directory.
    pub fn list_all(
        session_dir: Option<&Path>,
        progress: Option<&mut ListProgress<'_>>,
        cancel: Option<&AtomicBool>,
    ) -> Result<Vec<SessionInfo>, SessionError> {
        if cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst)) {
            return Err(SessionError::Aborted);
        }
        if let Some(dir) = session_dir {
            let mut sessions = list_sessions_from_dir(&normalize_path_buf(dir), progress, cancel)?;
            sort_session_infos(&mut sessions);
            return Ok(sessions);
        }
        let Some(home) = bake_home() else {
            return Ok(Vec::new());
        };
        list_all_from_sessions_dir(&sessions_dir(&home), progress, cancel)
    }

    /// [`Self::list_all`] over an explicit sessions root, as `listAll` reads
    /// Pi's agent directory: every directory or directory link under
    /// `sessions_root`.
    pub fn list_all_in(
        sessions_root: &Path,
        progress: Option<&mut ListProgress<'_>>,
        cancel: Option<&AtomicBool>,
    ) -> Result<Vec<SessionInfo>, SessionError> {
        if cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst)) {
            return Err(SessionError::Aborted);
        }
        list_all_from_sessions_dir(sessions_root, progress, cancel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_sort_keeps_equal_order_and_tolerates_nan() {
        let keys = [Some(3), None, Some(1), Some(3), None, Some(2)];
        let mut items: Vec<usize> = (0..keys.len()).collect();
        stable_sort(&mut items, |a, b| match (keys[a], keys[b]) {
            (Some(a), Some(b)) => a < b,
            _ => false,
        });
        assert_eq!(items.len(), keys.len());
        let mut sorted: Vec<usize> = (0..4).collect();
        let plain = [5, 1, 5, 0];
        stable_sort(&mut sorted, |a, b| plain[a] < plain[b]);
        assert_eq!(sorted, [3, 1, 0, 2]);
    }

    #[test]
    fn file_names_follow_pi() {
        assert_eq!(
            session_file_name("2025-01-02T03:04:05.678Z", "abc"),
            "2025-01-02T03-04-05-678Z_abc.jsonl"
        );
    }
}
