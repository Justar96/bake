//! The coding agent's message types and their conversion to model messages.
//!
//! Ported from Pi `packages/coding-agent/src/core/messages.ts` (v1.1.0) and
//! the `AgentMessage` union of `packages/agent/src/types.ts`.
//!
//! An [`AgentMessage`] is the JSON object a session stores, kept whole: Pi
//! does not validate messages it reads, and a message written by another Pi
//! version may carry members this one does not know, so the object is the
//! message and every member survives a read and a rewrite in its order.
//! [`AgentMessage::to_typed`] reads it as a [`TypedAgentMessage`]: one of
//! `bake-ai`'s model messages, or one of the four coding-agent roles below.

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

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

/// A message read by its `role`.
// Unboxed variants keep matching plain, as `bake_ai::Message` does.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum TypedAgentMessage {
    /// `system`, `user`, `assistant`, or `toolResult`.
    Llm(Message),
    /// `bashExecution`
    BashExecution(BashExecutionMessage),
    /// `custom`
    Custom(CustomMessage),
    /// `branchSummary`
    BranchSummary(BranchSummaryMessage),
    /// `compactionSummary`
    CompactionSummary(CompactionSummaryMessage),
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

impl TypedAgentMessage {
    /// The message as the JSON object Pi stores.
    pub fn to_json(&self) -> JsonObject {
        match self {
            Self::Llm(message) => match serde_json::to_value(message) {
                Ok(Value::Object(object)) => object,
                _ => JsonObject::new(),
            },
            Self::BashExecution(message) => tagged("bashExecution", message),
            Self::Custom(message) => tagged("custom", message),
            Self::BranchSummary(message) => tagged("branchSummary", message),
            Self::CompactionSummary(message) => tagged("compactionSummary", message),
        }
    }

    /// Read a stored object by its `role`.
    pub fn from_json(object: &JsonObject) -> Result<Self, serde_json::Error> {
        let value = Value::Object(object.clone());
        let role = object.get("role").and_then(Value::as_str).unwrap_or("");
        let mut untagged = object.clone();
        untagged.remove("role");
        let untagged = Value::Object(untagged);
        Ok(match role {
            "system" | "user" | "assistant" | "toolResult" => {
                Self::Llm(serde_json::from_value(value)?)
            }
            "bashExecution" => Self::BashExecution(serde_json::from_value(untagged)?),
            "custom" => Self::Custom(serde_json::from_value(untagged)?),
            "branchSummary" => Self::BranchSummary(serde_json::from_value(untagged)?),
            "compactionSummary" => Self::CompactionSummary(serde_json::from_value(untagged)?),
            other => {
                return Err(de::Error::custom(format!("unknown message role `{other}`")));
            }
        })
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

    /// Read the message by its `role`. Fails for a role this crate does not
    /// know or members of the wrong shape; the stored object is unchanged.
    pub fn to_typed(&self) -> Result<TypedAgentMessage, serde_json::Error> {
        TypedAgentMessage::from_json(&self.0)
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

impl From<TypedAgentMessage> for AgentMessage {
    fn from(message: TypedAgentMessage) -> Self {
        Self(message.to_json())
    }
}

impl From<Message> for AgentMessage {
    fn from(message: Message) -> Self {
        TypedAgentMessage::Llm(message).into()
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
        TypedAgentMessage::BashExecution(message).into()
    }
}

impl From<CustomMessage> for AgentMessage {
    fn from(message: CustomMessage) -> Self {
        TypedAgentMessage::Custom(message).into()
    }
}

impl From<BranchSummaryMessage> for AgentMessage {
    fn from(message: BranchSummaryMessage) -> Self {
        TypedAgentMessage::BranchSummary(message).into()
    }
}

impl From<CompactionSummaryMessage> for AgentMessage {
    fn from(message: CompactionSummaryMessage) -> Self {
        TypedAgentMessage::CompactionSummary(message).into()
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
/// dropped, as in Pi; so is one whose members do not read as its role's
/// type, which Pi would pass on unvalidated.
pub fn convert_to_llm(messages: &[AgentMessage]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(|message| match message.to_typed().ok()? {
            TypedAgentMessage::Llm(message) => Some(message),
            TypedAgentMessage::BashExecution(message) => {
                if message.exclude_from_context == Some(true) {
                    None
                } else {
                    Some(text_message(
                        bash_execution_to_text(&message),
                        message.timestamp,
                    ))
                }
            }
            TypedAgentMessage::Custom(message) => {
                let content = match message.content {
                    UserContent::Text(text) => {
                        UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))])
                    }
                    blocks => blocks,
                };
                Some(Message::User(UserMessage {
                    content,
                    timestamp: message.timestamp,
                }))
            }
            TypedAgentMessage::BranchSummary(message) => Some(text_message(
                format!(
                    "{BRANCH_SUMMARY_PREFIX}{}{BRANCH_SUMMARY_SUFFIX}",
                    message.summary
                ),
                message.timestamp,
            )),
            TypedAgentMessage::CompactionSummary(message) => Some(text_message(
                format!(
                    "{COMPACTION_SUMMARY_PREFIX}{}{COMPACTION_SUMMARY_SUFFIX}",
                    message.summary
                ),
                message.timestamp,
            )),
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
            message.to_typed(),
            Ok(TypedAgentMessage::Llm(Message::User(_)))
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
        assert_eq!(
            stored.to_typed().ok(),
            Some(TypedAgentMessage::BashExecution(bash.clone()))
        );
        assert_eq!(
            bash_execution_to_text(&bash),
            "Ran `ls`\n(no output)\n\nCommand exited with code 2\n\n[Output truncated. Full output: /tmp/out]"
        );
        let unknown = AgentMessage::from_json(object(json!({"role": "future"})));
        assert!(unknown.to_typed().is_err());
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
        let converted = convert_to_llm(&messages);
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
