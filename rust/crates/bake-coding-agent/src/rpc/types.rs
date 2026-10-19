//! The RPC protocol's records, ported from Pi v1.3.0
//! `packages/coding-agent/src/modes/rpc/rpc-types.ts`
//! (`71023aec1546d6d677a65669c5665c0927ce0dd0`): the commands stdin
//! carries, the responses and session state stdout carries, and the
//! extension UI records both carry.
//!
//! Pi's TypeScript union is read here as one tagged enum. A command's
//! unknown members are ignored, as they are in TypeScript; a member of the
//! wrong type, or a missing required member, is a failed response for that
//! command, where Pi would pass `undefined` on to the session. The reason
//! is the reader's, so it states what could not be read rather than naming
//! the member, which an internally tagged enum does not keep.

use serde::Deserialize;
use serde_json::{Map, Value};

use bake_agent::QueueMode;
use bake_ai::{ImageContent, ModelThinkingLevel};

/// How a `prompt` command behaves while the agent is streaming
/// (`streamingBehavior`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamingBehavior {
    /// Queue it for the running turn's next step.
    Steer,
    /// Queue it for when the turn would otherwise stop.
    FollowUp,
}

/// A queue mode on the wire (`"all" | "one-at-a-time"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum WireQueueMode {
    /// Every queued message.
    #[serde(rename = "all")]
    All,
    /// Only the oldest queued message.
    #[serde(rename = "one-at-a-time")]
    OneAtATime,
}

impl From<WireQueueMode> for QueueMode {
    fn from(mode: WireQueueMode) -> Self {
        match mode {
            WireQueueMode::All => QueueMode::All,
            WireQueueMode::OneAtATime => QueueMode::OneAtATime,
        }
    }
}

/// Pi's `RpcCommand` without its `id`, which [`Envelope`] reads.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RpcCommand {
    /// Send a prompt.
    Prompt {
        /// The prompt's text.
        message: String,
        /// Images for the prompt.
        #[serde(default)]
        images: Vec<ImageContent>,
        /// What to do while a turn runs.
        #[serde(default)]
        streaming_behavior: Option<StreamingBehavior>,
    },
    /// Queue a prompt for the running turn's next step.
    Steer {
        /// The prompt's text.
        message: String,
        /// Images for the prompt.
        #[serde(default)]
        images: Vec<ImageContent>,
    },
    /// Queue a prompt for when the turn would otherwise stop.
    FollowUp {
        /// The prompt's text.
        message: String,
        /// Images for the prompt.
        #[serde(default)]
        images: Vec<ImageContent>,
    },
    /// Stop the running turn.
    Abort,
    /// Empty both queues and return what they held.
    ClearQueue,
    /// Start a new session.
    NewSession {
        /// The session this one is forked from.
        #[serde(default)]
        parent_session: Option<String>,
    },
    /// The session's state.
    GetState,
    /// Select a model.
    SetModel {
        /// The model's provider.
        provider: String,
        /// The model's id.
        model_id: String,
    },
    /// Select the next available model.
    CycleModel,
    /// Every model the session can select.
    GetAvailableModels,
    /// Set the thinking level.
    SetThinkingLevel {
        /// The level.
        level: ModelThinkingLevel,
    },
    /// Select the next thinking level.
    CycleThinkingLevel,
    /// The thinking levels the selected model supports.
    GetAvailableThinkingLevels,
    /// How many steering messages a drain point takes.
    SetSteeringMode {
        /// The mode.
        mode: WireQueueMode,
    },
    /// How many follow-up messages a drain point takes.
    SetFollowUpMode {
        /// The mode.
        mode: WireQueueMode,
    },
    /// Summarize the conversation now.
    Compact {
        /// Instructions for the summary.
        #[serde(default)]
        custom_instructions: Option<String>,
    },
    /// Turn automatic compaction on or off.
    SetAutoCompaction {
        /// Whether it runs.
        enabled: bool,
    },
    /// Turn automatic retry on or off.
    SetAutoRetry {
        /// Whether it runs.
        enabled: bool,
    },
    /// Stop waiting before a retry.
    AbortRetry,
    /// Run a shell command outside the model's tools.
    Bash {
        /// The command line.
        command: String,
        /// Whether its output stays out of the context.
        #[serde(default)]
        exclude_from_context: Option<bool>,
    },
    /// Stop the running shell command.
    AbortBash,
    /// The session's counts and tokens.
    GetSessionStats,
    /// Write the session as an HTML page.
    ExportHtml {
        /// Where to write it.
        #[serde(default)]
        output_path: Option<String>,
    },
    /// Switch to the session at this path.
    SwitchSession {
        /// The session file.
        session_path: String,
    },
    /// Start a new session from an earlier message.
    Fork {
        /// The entry to fork at.
        entry_id: String,
    },
    /// Start a new session holding this whole conversation.
    Clone,
    /// The user messages a fork can start from.
    GetForkMessages,
    /// The session file's entries.
    GetEntries {
        /// Only the entries after this one.
        #[serde(default)]
        since: Option<String>,
    },
    /// The session file's entry tree.
    GetTree,
    /// The last assistant message's text.
    GetLastAssistantText,
    /// Name the session.
    SetSessionName {
        /// The display name.
        name: String,
    },
    /// The session's messages.
    GetMessages,
    /// The commands a prompt can invoke.
    GetCommands,
}

impl RpcCommand {
    /// The command's wire `type`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Prompt { .. } => "prompt",
            Self::Steer { .. } => "steer",
            Self::FollowUp { .. } => "follow_up",
            Self::Abort => "abort",
            Self::ClearQueue => "clear_queue",
            Self::NewSession { .. } => "new_session",
            Self::GetState => "get_state",
            Self::SetModel { .. } => "set_model",
            Self::CycleModel => "cycle_model",
            Self::GetAvailableModels => "get_available_models",
            Self::SetThinkingLevel { .. } => "set_thinking_level",
            Self::CycleThinkingLevel => "cycle_thinking_level",
            Self::GetAvailableThinkingLevels => "get_available_thinking_levels",
            Self::SetSteeringMode { .. } => "set_steering_mode",
            Self::SetFollowUpMode { .. } => "set_follow_up_mode",
            Self::Compact { .. } => "compact",
            Self::SetAutoCompaction { .. } => "set_auto_compaction",
            Self::SetAutoRetry { .. } => "set_auto_retry",
            Self::AbortRetry => "abort_retry",
            Self::Bash { .. } => "bash",
            Self::AbortBash => "abort_bash",
            Self::GetSessionStats => "get_session_stats",
            Self::ExportHtml { .. } => "export_html",
            Self::SwitchSession { .. } => "switch_session",
            Self::Fork { .. } => "fork",
            Self::Clone => "clone",
            Self::GetForkMessages => "get_fork_messages",
            Self::GetEntries { .. } => "get_entries",
            Self::GetTree => "get_tree",
            Self::GetLastAssistantText => "get_last_assistant_text",
            Self::SetSessionName { .. } => "set_session_name",
            Self::GetMessages => "get_messages",
            Self::GetCommands => "get_commands",
        }
    }
}

/// What every record on stdin carries: its optional correlation `id` and
/// its `type`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Envelope {
    /// The client's correlation id, repeated in the response.
    #[serde(default)]
    pub id: Option<String>,
    /// The record's kind.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
}

/// One record read from stdin.
#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    /// A command to handle.
    Command(Option<String>, RpcCommand),
    /// An answer to an `extension_ui_request`.
    ExtensionUiResponse(ExtensionUiResponse),
    /// A record that cannot be handled, with the failed response to send.
    Invalid(Value),
}

/// Pi's `RpcExtensionUIResponse`: the answer to one request.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ExtensionUiResponse {
    /// The id its request carried.
    pub id: String,
    /// A `select`, `input`, or `editor` answer.
    #[serde(default)]
    pub value: Option<String>,
    /// A `confirm` answer.
    #[serde(default)]
    pub confirmed: Option<bool>,
    /// Whether the person dismissed the request.
    #[serde(default)]
    pub cancelled: Option<bool>,
}

/// Reads one line as a record, as Pi's `handleInputLine` does: JSON that
/// does not parse is a `parse` failure without an id, an
/// `extension_ui_response` is routed to its request, and anything else is a
/// command.
pub fn read_record(line: &str) -> Record {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(error) => {
            return Record::Invalid(error_response(
                None,
                "parse",
                &format!("Failed to parse command: {error}"),
            ));
        }
    };
    if !value.is_object() {
        // Pi reads the record's `type` from a non-object too, and reports
        // the `undefined` it finds.
        return Record::Invalid(error_response(
            None,
            UNDEFINED,
            &format!("Unknown command: {UNDEFINED}"),
        ));
    }
    let envelope: Envelope = serde_json::from_value(value.clone()).unwrap_or_default();
    if envelope.kind.as_deref() == Some("extension_ui_response") {
        return match serde_json::from_value::<ExtensionUiResponse>(value) {
            Ok(response) => Record::ExtensionUiResponse(response),
            // Pi drops an answer it cannot route; so does this.
            Err(error) => Record::Invalid(error_response(
                envelope.id,
                "extension_ui_response",
                &format!("Failed to parse command: {error}"),
            )),
        };
    }
    let kind = envelope
        .kind
        .clone()
        .unwrap_or_else(|| UNDEFINED.to_owned());
    match serde_json::from_value::<RpcCommand>(value) {
        Ok(command) => Record::Command(envelope.id, command),
        Err(error) if is_unknown_command(&error, &kind) => Record::Invalid(error_response(
            envelope.id,
            &kind,
            &format!("Unknown command: {kind}"),
        )),
        Err(error) => Record::Invalid(error_response(
            envelope.id,
            &kind,
            &format!("Failed to parse command: {error}"),
        )),
    }
}

/// What Pi's message calls a record whose `type` is missing.
const UNDEFINED: &str = "undefined";

/// Whether the reader refused the record's `type`, which is Pi's `Unknown
/// command`, rather than one of the command's members, whose own unknown
/// value names itself instead.
fn is_unknown_command(error: &serde_json::Error, kind: &str) -> bool {
    let message = error.to_string();
    message.starts_with(&format!("unknown variant `{kind}`"))
        || message.starts_with("missing field `type`")
}

/// Pi's `success`: a response with `data`, or without it when `data` is
/// `None`.
pub fn success_response(id: Option<String>, command: &str, data: Option<Value>) -> Value {
    let mut object = Map::new();
    if let Some(id) = id {
        object.insert("id".into(), id.into());
    }
    object.insert("type".into(), "response".into());
    object.insert("command".into(), command.into());
    object.insert("success".into(), true.into());
    if let Some(data) = data {
        object.insert("data".into(), data);
    }
    Value::Object(object)
}

/// Pi's `error`: the one failed-response shape every command shares.
pub fn error_response(id: Option<String>, command: &str, message: &str) -> Value {
    let mut object = Map::new();
    if let Some(id) = id {
        object.insert("id".into(), id.into());
    }
    object.insert("type".into(), "response".into());
    object.insert("command".into(), command.into());
    object.insert("success".into(), false.into());
    object.insert("error".into(), message.into());
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_pis_commands_with_their_field_names() {
        assert_eq!(
            read_record(
                r#"{"id":"1","type":"prompt","message":"hi","streamingBehavior":"followUp"}"#
            ),
            Record::Command(
                Some("1".into()),
                RpcCommand::Prompt {
                    message: "hi".into(),
                    images: Vec::new(),
                    streaming_behavior: Some(StreamingBehavior::FollowUp),
                }
            )
        );
        assert_eq!(
            read_record(r#"{"type":"set_model","provider":"p","modelId":"m"}"#),
            Record::Command(
                None,
                RpcCommand::SetModel {
                    provider: "p".into(),
                    model_id: "m".into()
                }
            )
        );
        assert_eq!(
            read_record(r#"{"type":"set_steering_mode","mode":"one-at-a-time"}"#),
            Record::Command(
                None,
                RpcCommand::SetSteeringMode {
                    mode: WireQueueMode::OneAtATime
                }
            )
        );
        assert_eq!(
            read_record(r#"{"type":"get_entries","since":"e1","extra":true}"#),
            Record::Command(
                None,
                RpcCommand::GetEntries {
                    since: Some("e1".into())
                }
            )
        );
    }

    // Pi `docs/rpc.md`, "Errors": malformed JSON is a `parse` failure
    // without a request id, and an unknown type names itself.
    #[test]
    fn reports_pis_parse_and_unknown_command_failures() {
        let Record::Invalid(response) = read_record("{") else {
            panic!("expected a failed response");
        };
        assert_eq!(response["type"], json!("response"));
        assert_eq!(response["command"], json!("parse"));
        assert_eq!(response["success"], json!(false));
        assert!(
            response["error"]
                .as_str()
                .is_some_and(|error| error.starts_with("Failed to parse command: ")),
            "{response}"
        );
        assert_eq!(
            read_record(r#"{"id":"7","type":"telepathy"}"#),
            Record::Invalid(json!({
                "id": "7",
                "type": "response",
                "command": "telepathy",
                "success": false,
                "error": "Unknown command: telepathy",
            }))
        );
        // Pi reports a record without a `type`, and one that is not an
        // object, as `Unknown command: undefined`.
        for line in ["{}", "7", "[]", "\"prompt\""] {
            let Record::Invalid(response) = read_record(line) else {
                panic!("expected a failed response for {line}");
            };
            assert_eq!(
                response["error"],
                json!("Unknown command: undefined"),
                "{line}"
            );
        }
    }

    #[test]
    fn a_member_of_the_wrong_type_fails_its_command() {
        for line in [
            r#"{"type":"prompt","message":7}"#,
            r#"{"type":"prompt"}"#,
            r#"{"type":"set_thinking_level","level":"loud"}"#,
        ] {
            let Record::Invalid(response) = read_record(line) else {
                panic!("expected a failed response for {line}");
            };
            assert_eq!(response["success"], json!(false), "{line}");
            assert!(
                response["error"]
                    .as_str()
                    .is_some_and(|error| error.starts_with("Failed to parse command: ")),
                "{response}"
            );
        }
        let Record::Invalid(response) = read_record(r#"{"type":"prompt","message":7}"#) else {
            panic!("expected a failed response");
        };
        assert_eq!(response["command"], json!("prompt"));
    }

    #[test]
    fn an_extension_ui_response_is_routed_to_its_request() {
        assert_eq!(
            read_record(r#"{"type":"extension_ui_response","id":"a","confirmed":true}"#),
            Record::ExtensionUiResponse(ExtensionUiResponse {
                id: "a".into(),
                value: None,
                confirmed: Some(true),
                cancelled: None,
            })
        );
    }

    #[test]
    fn responses_keep_pis_member_order() {
        assert_eq!(
            serialize(&success_response(Some("3".into()), "abort", None)),
            r#"{"id":"3","type":"response","command":"abort","success":true}"#
        );
        assert_eq!(
            serialize(&success_response(
                None,
                "get_state",
                Some(json!({ "sessionId": "s" }))
            )),
            r#"{"type":"response","command":"get_state","success":true,"data":{"sessionId":"s"}}"#
        );
        assert_eq!(
            serialize(&error_response(Some("4".into()), "set_model", "nope")),
            r#"{"id":"4","type":"response","command":"set_model","success":false,"error":"nope"}"#
        );
    }

    fn serialize(value: &Value) -> String {
        serde_json::to_string(value).expect("response")
    }
}
