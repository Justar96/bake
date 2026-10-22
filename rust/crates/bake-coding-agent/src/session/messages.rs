//! The coding agent's message types and their conversion to model messages.
//!
//! Ported from Pi `packages/coding-agent/src/core/messages.ts` (v1.1.0) and
//! the `AgentMessage` union of `packages/agent/src/types.ts`.
//!
//! An [`AgentMessage`] is the JSON object a session stores, kept whole: Pi
//! does not validate messages it reads, and a message written by another Pi
//! version may carry members this one does not know, so the object is the
//! message and every member survives a read and a rewrite in its order.
//! [`AgentMessage::to_agent`] reads it as Pi's `AgentMessage` union in its
//! one live representation, [`bake_agent::AgentMessage`]: one of `bake-ai`'s
//! model messages, or one of the four coding-agent kinds below, which
//! implement [`CustomAgentMessage`] as Pi's `CustomAgentMessages`
//! declaration merging adds them. The agent carries them, the session stores
//! them through `From<&bake_agent::AgentMessage>`, and they reach the model
//! only through [`convert_to_llm`].

use std::any::Any;
use std::sync::Arc;

use serde::de::Deserializer;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

use bake_agent::{AgentMessage as LiveMessage, CustomAgentMessage};
use bake_ai::{Message, TextContent, UserContent, UserContentBlock, UserMessage};

use crate::session::json::JsonObject;
use crate::session::time::js_date_ms;

/// Pi's `COMPACTION_SUMMARY_PREFIX`.
pub const COMPACTION_SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
/// Pi's `COMPACTION_SUMMARY_SUFFIX`.
pub const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";
/// Pi's `BRANCH_SUMMARY_PREFIX`.
pub const BRANCH_SUMMARY_PREFIX: &str =
    "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n";
/// Pi's `BRANCH_SUMMARY_SUFFIX`.
pub const BRANCH_SUMMARY_SUFFIX: &str = "</summary>";

/// A `!` command's execution (`role: "bashExecution"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashExecutionMessage {
    /// The command.
    pub command: String,
    /// Its output.
    pub output: String,
    /// The exit code; absent when the command did not exit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    /// Whether it was cancelled.
    pub cancelled: bool,
    /// Whether the output was truncated.
    pub truncated: bool,
    /// Where the full output was saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
    /// Unix milliseconds.
    pub timestamp: i64,
    /// True for `!!` commands, which stay out of model context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_from_context: Option<bool>,
}

/// An extension-injected message (`role: "custom"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessage {
    /// The extension's type tag.
    pub custom_type: String,
    /// A string or text and image blocks.
    pub content: UserContent,
    /// Whether the interface shows it.
    pub display: bool,
    /// Extension data, not sent to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// Unix milliseconds.
    pub timestamp: i64,
}

/// A summary of an abandoned branch (`role: "branchSummary"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryMessage {
    /// The summary.
    pub summary: String,
    /// The leaf the branch was left from.
    pub from_id: Option<String>,
    /// Unix milliseconds.
    pub timestamp: i64,
}

/// A summary of compacted history (`role: "compactionSummary"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSummaryMessage {
    /// The summary.
    pub summary: String,
    /// Context tokens before compaction.
    pub tokens_before: u64,
    /// Unix milliseconds.
    pub timestamp: i64,
}

fn tagged<T: Serialize>(role: &str, value: &T) -> JsonObject {
    let mut object = JsonObject::new();
    object.insert("role".to_owned(), Value::String(role.to_owned()));
    // These types hold only strings, numbers, and JSON values, whose
    // serialization cannot fail; a failure would leave just the role.
    if let Ok(Value::Object(members)) = serde_json::to_value(value) {
        object.extend(members);
    }
    object
}

macro_rules! custom_kind {
    ($type:ty, $role:literal) => {
        impl CustomAgentMessage for $type {
            fn role(&self) -> &str {
                $role
            }

            fn timestamp(&self) -> i64 {
                self.timestamp
            }

            fn to_json(&self) -> Value {
                Value::Object(tagged($role, self))
            }

            fn as_any(&self) -> &dyn Any {
                self
            }
        }
    };
}

custom_kind!(BashExecutionMessage, "bashExecution");
custom_kind!(CustomMessage, "custom");
custom_kind!(BranchSummaryMessage, "branchSummary");
custom_kind!(CompactionSummaryMessage, "compactionSummary");

/// A stored message the agent carries without reading it: a role this
/// crate does not know, or members of the wrong shape for their role. Pi
/// passes such objects on unvalidated; here they stay in the transcript and
/// round-trip through the session unchanged, and [`convert_to_llm`] drops
/// them.
#[derive(Debug, Clone, PartialEq)]
pub struct OpaqueMessage(pub JsonObject);

impl CustomAgentMessage for OpaqueMessage {
    fn role(&self) -> &str {
        self.0.get("role").and_then(Value::as_str).unwrap_or("")
    }

    fn timestamp(&self) -> i64 {
        self.0
            .get("timestamp")
            .and_then(|value| value.as_i64().or_else(|| value.as_f64().map(|n| n as i64)))
            .unwrap_or(0)
    }

    fn to_json(&self) -> Value {
        Value::Object(self.0.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn custom<T: CustomAgentMessage>(message: T) -> LiveMessage {
    LiveMessage::Custom(Arc::new(message))
}

/// Read a stored object by its `role` as Pi's `AgentMessage` union: one of
/// `bake-ai`'s model messages, or one of the four coding-agent kinds as a
/// [`CustomAgentMessage`]. Anything else is an [`OpaqueMessage`].
fn live_from_json(object: &JsonObject) -> LiveMessage {
    let role = object.get("role").and_then(Value::as_str).unwrap_or("");
    let untagged = || {
        let mut untagged = object.clone();
        untagged.remove("role");
        Value::Object(untagged)
    };
    let read = match role {
        "system" | "user" | "assistant" | "toolResult" => {
            serde_json::from_value::<Message>(Value::Object(object.clone()))
                .map(LiveMessage::Llm)
                .ok()
        }
        "bashExecution" => serde_json::from_value::<BashExecutionMessage>(untagged())
            .map(custom)
            .ok(),
        "custom" => serde_json::from_value::<CustomMessage>(untagged())
            .map(custom)
            .ok(),
        "branchSummary" => serde_json::from_value::<BranchSummaryMessage>(untagged())
            .map(custom)
            .ok(),
        "compactionSummary" => serde_json::from_value::<CompactionSummaryMessage>(untagged())
            .map(custom)
            .ok(),
        _ => None,
    };
    read.unwrap_or_else(|| custom(OpaqueMessage(object.clone())))
}

/// The JSON object Pi stores for a live message.
fn live_to_json(message: &LiveMessage) -> JsonObject {
    let value = match message {
        LiveMessage::Llm(message) => serde_json::to_value(message).unwrap_or(Value::Null),
        LiveMessage::Custom(message) => message.to_json(),
    };
    match value {
        Value::Object(object) => object,
        _ => JsonObject::new(),
    }
}

/// A stored message: the JSON object itself, every member kept.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentMessage(JsonObject);

impl AgentMessage {
    /// Wrap a stored object.
    pub fn from_json(object: JsonObject) -> Self {
        Self(object)
    }

    /// The stored object.
    pub fn as_json(&self) -> &JsonObject {
        &self.0
    }

    /// The stored object, by value.
    pub fn into_json(self) -> JsonObject {
        self.0
    }

    pub(crate) fn as_json_mut(&mut self) -> &mut JsonObject {
        &mut self.0
    }

    /// The `role` member, when it is a string.
    pub fn role(&self) -> Option<&str> {
        self.0.get("role").and_then(Value::as_str)
    }

    /// The message as Pi's `AgentMessage` union, the one representation the
    /// agent and the session share: a model message, one of the four
    /// coding-agent kinds, or an [`OpaqueMessage`] for anything else. It
    /// never fails; the stored object is unchanged.
    pub fn to_agent(&self) -> LiveMessage {
        live_from_json(&self.0)
    }
}

impl Serialize for AgentMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AgentMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self(JsonObject::deserialize(deserializer)?))
    }
}

impl From<&LiveMessage> for AgentMessage {
    fn from(message: &LiveMessage) -> Self {
        Self(live_to_json(message))
    }
}

impl From<LiveMessage> for AgentMessage {
    fn from(message: LiveMessage) -> Self {
        Self(live_to_json(&message))
    }
}

impl From<Message> for AgentMessage {
    fn from(message: Message) -> Self {
        LiveMessage::Llm(message).into()
    }
}

impl From<UserMessage> for AgentMessage {
    fn from(message: UserMessage) -> Self {
        Message::User(message).into()
    }
}

impl From<bake_ai::AssistantMessage> for AgentMessage {
    fn from(message: bake_ai::AssistantMessage) -> Self {
        Message::Assistant(message).into()
    }
}

impl From<bake_ai::ToolResultMessage> for AgentMessage {
    fn from(message: bake_ai::ToolResultMessage) -> Self {
        Message::ToolResult(message).into()
    }
}

impl From<bake_ai::SystemMessage> for AgentMessage {
    fn from(message: bake_ai::SystemMessage) -> Self {
        Message::System(message).into()
    }
}

impl From<BashExecutionMessage> for AgentMessage {
    fn from(message: BashExecutionMessage) -> Self {
        custom(message).into()
    }
}

impl From<CustomMessage> for AgentMessage {
    fn from(message: CustomMessage) -> Self {
        custom(message).into()
    }
}

impl From<BranchSummaryMessage> for AgentMessage {
    fn from(message: BranchSummaryMessage) -> Self {
        custom(message).into()
    }
}

impl From<CompactionSummaryMessage> for AgentMessage {
    fn from(message: CompactionSummaryMessage) -> Self {
        custom(message).into()
    }
}

/// Pi's `bashExecutionToText`: a `!` command as user-message text.
pub fn bash_execution_to_text(message: &BashExecutionMessage) -> String {
    let mut text = format!("Ran `{}`\n", message.command);
    if message.output.is_empty() {
        text.push_str("(no output)");
    } else {
        text.push_str(&format!("```\n{}\n```", message.output));
    }
    if message.cancelled {
        text.push_str("\n\n(command cancelled)");
    } else if let Some(code) = message.exit_code.filter(|code| *code != 0) {
        text.push_str(&format!("\n\nCommand exited with code {code}"));
    }
    if message.truncated
        && let Some(path) = &message.full_output_path
        && !path.is_empty()
    {
        text.push_str(&format!("\n\n[Output truncated. Full output: {path}]"));
    }
    text
}

fn date_ms(timestamp: &str) -> i64 {
    // An unparseable timestamp is `NaN` in Pi, which no `i64` holds; the
    // typed helpers use 0. The session projection keeps Pi's `null`.
    js_date_ms(Some(&Value::String(timestamp.to_owned()))).unwrap_or(0)
}

/// Pi's `createBranchSummaryMessage`.
pub fn create_branch_summary_message(
    summary: &str,
    from_id: &str,
    timestamp: &str,
) -> BranchSummaryMessage {
    BranchSummaryMessage {
        summary: summary.to_owned(),
        from_id: Some(from_id.to_owned()),
        timestamp: date_ms(timestamp),
    }
}

/// Pi's `createCompactionSummaryMessage`.
pub fn create_compaction_summary_message(
    summary: &str,
    tokens_before: u64,
    timestamp: &str,
) -> CompactionSummaryMessage {
    CompactionSummaryMessage {
        summary: summary.to_owned(),
        tokens_before,
        timestamp: date_ms(timestamp),
    }
}

/// Pi's `createCustomMessage`: a custom message entry as a message.
pub fn create_custom_message(
    custom_type: &str,
    content: UserContent,
    display: bool,
    details: Option<Value>,
    timestamp: &str,
) -> CustomMessage {
    CustomMessage {
        custom_type: custom_type.to_owned(),
        content,
        display,
        details,
        timestamp: date_ms(timestamp),
    }
}

fn text_message(text: String, timestamp: i64) -> Message {
    Message::User(UserMessage {
        content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))]),
        timestamp,
    })
}

/// Pi's `convertToLlm`: the model messages for a transcript. `bashExecution`
/// becomes user text (or is dropped when excluded from context), `custom`
/// becomes a user message with its content, and the summaries become user
/// text inside their prefix and suffix. A message of an unknown role is
/// dropped, as in Pi; so is an [`OpaqueMessage`] whose members do not read
/// as its role's type, which Pi would pass on unvalidated.
pub fn convert_to_llm(messages: &[LiveMessage]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(|message| {
            let custom = match message {
                LiveMessage::Llm(message) => return Some(message.clone()),
                LiveMessage::Custom(custom) => custom.as_any(),
            };
            if let Some(message) = custom.downcast_ref::<BashExecutionMessage>() {
                if message.exclude_from_context == Some(true) {
                    return None;
                }
                return Some(text_message(
                    bash_execution_to_text(message),
                    message.timestamp,
                ));
            }
            if let Some(message) = custom.downcast_ref::<CustomMessage>() {
                let content = match &message.content {
                    UserContent::Text(text) => UserContent::Blocks(vec![UserContentBlock::Text(
                        TextContent::new(text.clone()),
                    )]),
                    blocks => blocks.clone(),
                };
                return Some(Message::User(UserMessage {
                    content,
                    timestamp: message.timestamp,
                }));
            }
            if let Some(message) = custom.downcast_ref::<BranchSummaryMessage>() {
                return Some(text_message(
                    format!(
                        "{BRANCH_SUMMARY_PREFIX}{}{BRANCH_SUMMARY_SUFFIX}",
                        message.summary
                    ),
                    message.timestamp,
                ));
            }
            if let Some(message) = custom.downcast_ref::<CompactionSummaryMessage>() {
                return Some(text_message(
                    format!(
                        "{COMPACTION_SUMMARY_PREFIX}{}{COMPACTION_SUMMARY_SUFFIX}",
                        message.summary
                    ),
                    message.timestamp,
                ));
            }
            None
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> JsonObject {
        match value {
            Value::Object(object) => object,
            _ => JsonObject::new(),
        }
    }

    #[test]
    fn unknown_members_and_order_survive() {
        let stored = object(json!({
            "role": "user", "zeta": 1, "content": "hi", "timestamp": 3, "alpha": {"x": [1]}
        }));
        let message = AgentMessage::from_json(stored.clone());
        let text = serde_json::to_string(&message).unwrap_or_default();
        assert_eq!(
            text,
            r#"{"role":"user","zeta":1,"content":"hi","timestamp":3,"alpha":{"x":[1]}}"#
        );
        assert!(matches!(
            message.to_agent(),
            LiveMessage::Llm(Message::User(_))
        ));
        assert_eq!(message.into_json(), stored);
    }

    #[test]
    fn typed_messages_take_pi_shapes() {
        let bash = BashExecutionMessage {
            command: "ls".into(),
            output: String::new(),
            exit_code: Some(2),
            cancelled: false,
            truncated: true,
            full_output_path: Some("/tmp/out".into()),
            timestamp: 5,
            exclude_from_context: None,
        };
        let stored = AgentMessage::from(bash.clone());
        assert_eq!(
            serde_json::to_string(&stored).unwrap_or_default(),
            r#"{"role":"bashExecution","command":"ls","output":"","exitCode":2,"cancelled":false,"truncated":true,"fullOutputPath":"/tmp/out","timestamp":5}"#
        );
        let live = stored.to_agent();
        assert_eq!(
            live.as_custom()
                .and_then(|custom| custom.as_any().downcast_ref::<BashExecutionMessage>()),
            Some(&bash)
        );
        assert_eq!(AgentMessage::from(&live), stored);
        assert_eq!(
            bash_execution_to_text(&bash),
            "Ran `ls`\n(no output)\n\nCommand exited with code 2\n\n[Output truncated. Full output: /tmp/out]"
        );
        let unknown = AgentMessage::from_json(object(json!({"role": "future", "x": 1})));
        let live = unknown.to_agent();
        assert_eq!(live.role(), "future");
        assert_eq!(AgentMessage::from(&live), unknown);
        // A known role with the wrong shape is carried, not read.
        let malformed = AgentMessage::from_json(object(json!({"role": "custom", "content": 1})));
        assert_eq!(AgentMessage::from(malformed.to_agent()), malformed);
    }

    #[test]
    fn convert_to_llm_follows_pi() {
        let messages: Vec<AgentMessage> = vec![
            create_branch_summary_message("b", "x", "1970-01-01T00:00:00.002Z").into(),
            create_compaction_summary_message("c", 10, "1970-01-01T00:00:00.003Z").into(),
            create_custom_message("t", "hi".into(), true, None, "1970-01-01T00:00:00.004Z").into(),
            BashExecutionMessage {
                command: "x".into(),
                output: "o".into(),
                exit_code: None,
                cancelled: true,
                truncated: false,
                full_output_path: None,
                timestamp: 6,
                exclude_from_context: Some(true),
            }
            .into(),
            AgentMessage::from_json(object(json!({"role": "future"}))),
        ];
        let live: Vec<LiveMessage> = messages.iter().map(AgentMessage::to_agent).collect();
        let converted = convert_to_llm(&live);
        let texts: Vec<String> = converted
            .iter()
            .map(|message| match message {
                Message::User(user) => bake_ai::utils::text::user_content_text(&user.content, ""),
                _ => String::new(),
            })
            .collect();
        assert_eq!(
            texts,
            [
                format!("{BRANCH_SUMMARY_PREFIX}b{BRANCH_SUMMARY_SUFFIX}"),
                format!("{COMPACTION_SUMMARY_PREFIX}c{COMPACTION_SUMMARY_SUFFIX}"),
                "hi".to_owned(),
            ]
        );
        assert_eq!(
            converted.iter().map(Message::timestamp).collect::<Vec<_>>(),
            [2, 3, 4]
        );
    }
}
