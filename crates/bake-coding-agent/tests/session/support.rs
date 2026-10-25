//! Shared helpers, after Pi `test/utilities.ts`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use bake_coding_agent::session::json::JsonObject;
use bake_coding_agent::session::{AgentMessage, SessionEntry};
use serde_json::{Value, json};

/// A temporary directory the test owns, removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(name: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "bake-session-{name}-{}-{unique}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    pub fn str(&self) -> &str {
        self.0.to_str().expect("UTF-8 temp dir")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn object(value: Value) -> JsonObject {
    match value {
        Value::Object(object) => object,
        other => panic!("not an object: {other}"),
    }
}

pub fn message(value: Value) -> AgentMessage {
    AgentMessage::from_json(object(value))
}

pub fn now() -> i64 {
    bake_ai::now_ms()
}

/// Pi's `userMsg`.
pub fn user_msg(text: &str) -> AgentMessage {
    message(json!({"role": "user", "content": text, "timestamp": now()}))
}

pub fn usage() -> Value {
    json!({
        "input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}
    })
}

/// Pi's `assistantMsg`.
pub fn assistant_msg(text: &str) -> AgentMessage {
    message(json!({
        "role": "assistant",
        "content": [{"type": "text", "text": text}],
        "api": "anthropic-messages",
        "provider": "anthropic",
        "model": "test",
        "usage": usage(),
        "stopReason": "stop",
        "timestamp": now(),
    }))
}

/// Pi's `readSessionFileRoles`: per line, the message role or entry type.
pub fn read_session_file_roles(file: &Path) -> Vec<String> {
    std::fs::read_to_string(file)
        .expect("read session file")
        .trim()
        .split('\n')
        .map(|line| {
            let record: Value = serde_json::from_str(line).expect("JSON line");
            record
                .get("message")
                .and_then(|message| message.get("role"))
                .or_else(|| record.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        })
        .collect()
}

pub fn ids(entries: &[&SessionEntry]) -> Vec<String> {
    entries.iter().map(|entry| entry.id().to_owned()).collect()
}

/// A message's member.
pub fn member<'a>(message: &'a AgentMessage, key: &str) -> Option<&'a Value> {
    message.as_json().get(key)
}

/// The first text block of a message's content, or its string content.
pub fn text_of(message: &AgentMessage) -> String {
    match member(message, "content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

pub fn roles(messages: &[AgentMessage]) -> Vec<String> {
    messages
        .iter()
        .map(|message| message.role().unwrap_or("").to_owned())
        .collect()
}

pub fn entry(value: Value) -> SessionEntry {
    SessionEntry::from_json(object(value)).expect("entry with id")
}
