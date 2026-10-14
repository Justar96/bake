//! Messages, content, tools, models, usage, and stream events.
//!
//! Ported from Pi `packages/ai/src/types.ts` and `src/utils/diagnostics.ts`
//! (v1.1.0). Every type that a session stores serializes to the JSON Pi
//! writes: camelCase members, `role` and `type` tags, and absent optional
//! members omitted. Provider catalogs, OAuth, images, classifiers, and
//! deferred-response fetching are not ported.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON object, Pi's `JsonObject`. Members keep their input order.
pub type JsonObject = serde_json::Map<String, Value>;

/// Pi's `ThinkingLevel`: the reasoning levels a request can ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    /// `minimal`
    Minimal,
    /// `low`
    Low,
    /// `medium`
    Medium,
    /// `high`
    High,
    /// `xhigh`
    Xhigh,
    /// `max`
    Max,
}

impl ThinkingLevel {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Pi's `ModelThinkingLevel`: `off` or a [`ThinkingLevel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelThinkingLevel {
    /// `off`
    Off,
    /// `minimal`
    Minimal,
    /// `low`
    Low,
    /// `medium`
    Medium,
    /// `high`
    High,
    /// `xhigh`
    Xhigh,
    /// `max`
    Max,
}

impl ModelThinkingLevel {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// The thinking level, or `None` for `off`.
    pub fn level(self) -> Option<ThinkingLevel> {
        match self {
            Self::Off => None,
            Self::Minimal => Some(ThinkingLevel::Minimal),
            Self::Low => Some(ThinkingLevel::Low),
            Self::Medium => Some(ThinkingLevel::Medium),
            Self::High => Some(ThinkingLevel::High),
            Self::Xhigh => Some(ThinkingLevel::Xhigh),
            Self::Max => Some(ThinkingLevel::Max),
        }
    }
}

impl From<ThinkingLevel> for ModelThinkingLevel {
    fn from(level: ThinkingLevel) -> Self {
        match level {
            ThinkingLevel::Minimal => Self::Minimal,
            ThinkingLevel::Low => Self::Low,
            ThinkingLevel::Medium => Self::Medium,
            ThinkingLevel::High => Self::High,
            ThinkingLevel::Xhigh => Self::Xhigh,
            ThinkingLevel::Max => Self::Max,
        }
    }
}

/// Pi's `ThinkingLevelMap`: provider values per thinking level. A missing
/// key uses the provider default; a `null` value marks the level unsupported.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThinkingLevelMap(pub BTreeMap<String, Option<String>>);

impl ThinkingLevelMap {
    /// `None` when the level is missing, `Some(None)` when it is `null`.
    pub fn get(&self, level: ModelThinkingLevel) -> Option<Option<&str>> {
        self.0.get(level.as_str()).map(Option::as_deref)
    }
}

/// Pi's `ToolChoice` for simple requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoice {
    /// `auto`
    Auto,
    /// `none`
    None,
}

impl ToolChoice {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
        }
    }
}

/// Pi's `CacheRetention`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheRetention {
    /// `none`
    None,
    /// `short`
    Short,
    /// `long`
    Long,
}

/// Pi's `ThinkingBudgets`: token budgets for token-based providers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingBudgets {
    /// `minimal`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimal: Option<u64>,
    /// `low`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub low: Option<u64>,
    /// `medium`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub medium: Option<u64>,
    /// `high`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub high: Option<u64>,
}

/// `{ type: "text" }` content.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    /// The text.
    pub text: String,
    /// Provider message metadata, such as an OpenAI Responses message id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_signature: Option<String>,
}

impl TextContent {
    /// Text content without a signature.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            text_signature: None,
        }
    }
}

/// `{ type: "thinking" }` content.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingContent {
    /// The reasoning text.
    pub thinking: String,
    /// Provider-specific opaque or serialized reasoning replay data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
    /// True when safety filters redacted the reasoning; the payload is then in
    /// `thinking_signature`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redacted: Option<bool>,
}

impl ThinkingContent {
    /// Thinking content without a signature.
    pub fn new(thinking: impl Into<String>) -> Self {
        Self {
            thinking: thinking.into(),
            thinking_signature: None,
            redacted: None,
        }
    }

    /// Whether the block is redacted.
    pub fn is_redacted(&self) -> bool {
        self.redacted == Some(true)
    }
}

/// `{ type: "image" }` content: base64 data and its MIME type.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageContent {
    /// Base64-encoded image data.
    pub data: String,
    /// For example `image/png`.
    pub mime_type: String,
}

/// `{ type: "toolCall" }` content.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    /// The provider's call id.
    pub id: String,
    /// The tool name.
    pub name: String,
    /// The parsed arguments.
    pub arguments: JsonObject,
    /// Google's opaque thought signature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
    /// OpenAI Responses namespace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

/// Text or image content, as user messages and tool results carry it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UserContentBlock {
    /// `text`
    Text(TextContent),
    /// `image`
    Image(ImageContent),
}

/// Content of an assistant message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AssistantContentBlock {
    /// `text`
    Text(TextContent),
    /// `thinking`
    Thinking(ThinkingContent),
    /// `toolCall`
    ToolCall(ToolCall),
}

/// A user message's content: a string or blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    /// A plain string.
    Text(String),
    /// Text and image blocks.
    Blocks(Vec<UserContentBlock>),
}

impl Default for UserContent {
    fn default() -> Self {
        Self::Blocks(Vec::new())
    }
}

impl From<&str> for UserContent {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

impl From<String> for UserContent {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

/// A system message's content: a string or text blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemContent {
    /// A plain string.
    Text(String),
    /// Text blocks.
    Blocks(Vec<SystemTextBlock>),
}

impl Default for SystemContent {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

impl From<&str> for SystemContent {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

/// The `{ type: "text" }` block a system message may hold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SystemTextBlock {
    /// `text`
    Text(TextContent),
}

/// Named prompt sections in declaration order; `None` removes a section.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sections(pub Vec<(String, Option<String>)>);

impl Sections {
    /// Whether no section is named.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Serialize for Sections {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (name, value) in &self.0 {
            map.serialize_entry(name, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Sections {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SectionsVisitor;
        impl<'de> Visitor<'de> for SectionsVisitor {
            type Value = Sections;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object of section names to strings or null")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Sections, A::Error> {
                let mut sections: Vec<(String, Option<String>)> = Vec::new();
                while let Some((name, value)) = access.next_entry::<String, Option<String>>()? {
                    // A repeated key keeps its first position, as an object does.
                    if let Some(entry) = sections.iter_mut().find(|(known, _)| *known == name) {
                        entry.1 = value;
                    } else {
                        sections.push((name, value));
                    }
                }
                Ok(Sections(sections))
            }
        }
        deserializer.deserialize_map(SectionsVisitor)
    }
}

/// Pi's `ConstrainedSamplingConfig` and its `false` opt-out.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstrainedSampling {
    /// `false`
    Disabled,
    /// `{ type: "json_schema", strict }`
    JsonSchema {
        /// Whether strict sampling is preferred or required.
        strict: StrictPreference,
    },
    /// `{ type: "grammar", variants }`; this crate does not send grammar tools.
    Grammar {
        /// Grammar variants by format, kept as given.
        variants: JsonObject,
    },
}

/// The `strict` member of a JSON-schema constrained-sampling config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StrictPreference {
    /// Use strict sampling when the schema allows it.
    Prefer,
    /// Fail the request when strict sampling is impossible.
    Require,
}

impl Serialize for ConstrainedSampling {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Disabled => serializer.serialize_bool(false),
            Self::JsonSchema { strict } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "json_schema")?;
                map.serialize_entry("strict", strict)?;
                map.end()
            }
            Self::Grammar { variants } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "grammar")?;
                map.serialize_entry("variants", variants)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ConstrainedSampling {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        match &value {
            Value::Bool(false) => Ok(Self::Disabled),
            Value::Object(object) => match object.get("type").and_then(Value::as_str) {
                Some("json_schema") => {
                    let strict = object
                        .get("strict")
                        .cloned()
                        .ok_or_else(|| de::Error::missing_field("strict"))?;
                    let strict =
                        StrictPreference::deserialize(strict).map_err(de::Error::custom)?;
                    Ok(Self::JsonSchema { strict })
                }
                Some("grammar") => {
                    let variants = match object.get("variants") {
                        Some(Value::Object(variants)) => variants.clone(),
                        _ => return Err(de::Error::missing_field("variants")),
                    };
                    Ok(Self::Grammar { variants })
                }
                _ => Err(de::Error::custom("unknown constrained sampling type")),
            },
            _ => Err(de::Error::custom(
                "expected false or a constrained sampling config",
            )),
        }
    }
}

/// A tool declaration: name, description, and JSON-schema parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    /// The tool name.
    pub name: String,
    /// What the tool does, for the model.
    pub description: String,
    /// The JSON schema of the arguments.
    pub parameters: Value,
    /// Provider-side constrained sampling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constrained_sampling: Option<ConstrainedSampling>,
}

/// A reference to a tool by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolReference {
    /// The tool name.
    pub name: String,
}

/// Cost in dollars per usage component.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCost {
    /// Input cost.
    pub input: f64,
    /// Output cost.
    pub output: f64,
    /// Cache-read cost.
    pub cache_read: f64,
    /// Cache-write cost.
    pub cache_write: f64,
    /// The sum.
    pub total: f64,
}

/// Token usage and its cost.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// Uncached input tokens.
    pub input: u64,
    /// Output tokens, reasoning included.
    pub output: u64,
    /// Cache-read input tokens.
    pub cache_read: u64,
    /// Cache-write input tokens.
    pub cache_write: u64,
    /// The part of `cache_write` written with 1h retention (Anthropic only).
    #[serde(rename = "cacheWrite1h", skip_serializing_if = "Option::is_none")]
    pub cache_write_1h: Option<u64>,
    /// Reasoning tokens, a subset of `output`, when the provider reports them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    /// The total the provider reports or the sum of the components.
    pub total_tokens: u64,
    /// The cost.
    pub cost: UsageCost,
}

impl Usage {
    /// `input + output + cache_read + cache_write`, saturating at
    /// `u64::MAX` because providers report these counts.
    pub fn component_sum(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }

    /// The input side, `input + cache_read + cache_write`, saturating.
    pub fn input_sum(&self) -> u64 {
        self.input
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }
}

/// Pi's `StopReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    /// Still streaming.
    Pending,
    /// The model finished.
    Stop,
    /// The output limit was reached.
    Length,
    /// The model called tools.
    ToolUse,
    /// The request failed.
    Error,
    /// The request was aborted.
    Aborted,
    /// The provider continues asynchronously behind a handle.
    Deferred,
}

impl StopReason {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Stop => "stop",
            Self::Length => "length",
            Self::ToolUse => "toolUse",
            Self::Error => "error",
            Self::Aborted => "aborted",
            Self::Deferred => "deferred",
        }
    }
}

/// A provider's handle to a deferred response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferredHandle {
    /// Provider id.
    pub provider: String,
    /// Model id.
    pub model_id: String,
    /// API id.
    pub api: String,
    /// Provider token.
    pub id: String,
    /// Expiry in Unix milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// Suggested poll delay.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll_after_ms: Option<u64>,
    /// Provider conversion data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Error details in a diagnostic (`utils/diagnostics.ts`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticErrorInfo {
    /// Error name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Error message.
    pub message: String,
    /// Stack trace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    /// A string or number code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<Value>,
}

/// A redacted provider or runtime diagnostic on an assistant message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantMessageDiagnostic {
    /// Diagnostic type.
    #[serde(rename = "type")]
    pub kind: String,
    /// Unix milliseconds.
    pub timestamp: i64,
    /// Error details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<DiagnosticErrorInfo>,
    /// Extra details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonObject>,
}

/// System instructions and tool declarations at one point in the transcript.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemMessage {
    /// Base prompt on the leading message; additional instructions later.
    #[serde(default, deserialize_with = "lax_system_content")]
    pub content: SystemContent,
    /// Named sections; later messages replace them by name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sections: Option<Sections>,
    /// Tools that become available here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_added: Option<Vec<Tool>>,
    /// Tools that stop being available here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_removed: Option<Vec<ToolReference>>,
    /// Unix milliseconds.
    pub timestamp: i64,
}

/// A user message.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserMessage {
    /// A string or text and image blocks.
    #[serde(default, deserialize_with = "lax_user_content")]
    pub content: UserContent,
    /// Unix milliseconds.
    pub timestamp: i64,
}

/// A model response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    /// Text, thinking, and tool-call blocks.
    #[serde(default, deserialize_with = "lax_vec")]
    pub content: Vec<AssistantContentBlock>,
    /// The API that produced it.
    pub api: String,
    /// The provider id.
    pub provider: String,
    /// The requested model id.
    pub model: String,
    /// The model the provider reported, when different.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    /// The provider's response id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    /// The provider-native effort level used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_thinking_level: Option<String>,
    /// The thinking level the agent loop requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ModelThinkingLevel>,
    /// Redacted provider and runtime diagnostics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<AssistantMessageDiagnostic>>,
    /// Token usage.
    pub usage: Usage,
    /// Why the response ended.
    pub stop_reason: StopReason,
    /// The deferred-response handle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deferred: Option<DeferredHandle>,
    /// The failure, for `error` and `aborted`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// The provider's own stop reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_stop_reason: Option<String>,
    /// Whether the provider said the model ended its turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_turn: Option<bool>,
    /// Unix milliseconds when the request started.
    pub timestamp: i64,
    /// Monotonic milliseconds until the response ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

impl AssistantMessage {
    /// An empty pending message for `model`, timestamped now.
    pub fn pending(model: &Model) -> Self {
        Self {
            content: Vec::new(),
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Pending,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: crate::now_ms(),
            duration_ms: None,
        }
    }

    /// The tool calls, in order.
    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.content.iter().filter_map(|block| match block {
            AssistantContentBlock::ToolCall(call) => Some(call),
            _ => None,
        })
    }
}

/// One call another tool made while it ran.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedToolCallRecord {
    /// Call id.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Arguments, omitted over the size limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<JsonObject>,
    /// UTF-8 size of omitted arguments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments_bytes: Option<u64>,
    /// `ok`, `error`, or `unfinished`.
    pub status: String,
    /// Duration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Truncated error text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The bounded record of nested calls a tool made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NestedToolCalls {
    /// The calls.
    pub calls: Vec<NestedToolCallRecord>,
    /// False when calls were dropped or unfinished.
    pub complete: bool,
}

/// A tool's result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage {
    /// The call this answers.
    pub tool_call_id: String,
    /// The tool name.
    pub tool_name: String,
    /// Text and image blocks.
    #[serde(default, deserialize_with = "lax_vec")]
    pub content: Vec<UserContentBlock>,
    /// JSON details for the session record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// Usage of the tool execution itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Nested calls, kept for the record and not sent to the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nested_calls: Option<NestedToolCalls>,
    /// Whether the call failed.
    pub is_error: bool,
    /// Unix milliseconds.
    pub timestamp: i64,
    /// Execution duration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// A transcript message, tagged by `role`.
// Unboxed variants keep matching and construction plain; transcripts hold
// few enough messages that the padding does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum Message {
    /// `system`
    System(SystemMessage),
    /// `user`
    User(UserMessage),
    /// `assistant`
    Assistant(AssistantMessage),
    /// `toolResult`
    ToolResult(ToolResultMessage),
}

impl Message {
    /// The `role` spelling.
    pub fn role(&self) -> &'static str {
        match self {
            Self::System(_) => "system",
            Self::User(_) => "user",
            Self::Assistant(_) => "assistant",
            Self::ToolResult(_) => "toolResult",
        }
    }

    /// The message timestamp.
    pub fn timestamp(&self) -> i64 {
        match self {
            Self::System(message) => message.timestamp,
            Self::User(message) => message.timestamp,
            Self::Assistant(message) => message.timestamp,
            Self::ToolResult(message) => message.timestamp,
        }
    }
}

/// Request input for the public entry points: `system_prompt` and `tools`
/// are shorthand for a leading system message.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    /// The system prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// The messages.
    pub messages: Vec<Message>,
    /// The tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
}

/// The normalized request context providers receive; prompt and tools travel
/// in system messages. Only [`crate::transcript::normalize_context`] and the
/// transcript helpers build one.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TranscriptContext {
    pub(crate) messages: Vec<Message>,
}

impl TranscriptContext {
    /// The messages.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }
}

/// Input modality of a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputModality {
    /// `text`
    Text,
    /// `image`
    Image,
}

/// Rates in dollars per million tokens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostRates {
    /// Input.
    pub input: f64,
    /// Output.
    pub output: f64,
    /// Cache read.
    pub cache_read: f64,
    /// Cache write.
    pub cache_write: f64,
}

/// A request-wide pricing tier.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostTier {
    /// The tier's rates.
    #[serde(flatten)]
    pub rates: ModelCostRates,
    /// Applies when total input exceeds this token count.
    pub input_tokens_above: u64,
}

/// A model's pricing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    /// Base rates.
    #[serde(flatten)]
    pub rates: ModelCostRates,
    /// Pricing tiers; the highest matching threshold applies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<ModelCostTier>>,
}

/// `thinkingFormat` of the OpenAI completions compatibility settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThinkingFormat {
    /// `reasoning_effort`
    Openai,
    /// `reasoning: { effort }`
    Openrouter,
    /// `thinking: { type }` plus `reasoning_effort`
    Deepseek,
    /// `reasoning: { enabled }` plus `reasoning_effort`
    Together,
    /// `chat_template_args` plus `reasoning_effort`
    Baseten,
    /// `thinking: { type }`
    Zai,
    /// top-level `enable_thinking`
    Qwen,
    /// configurable `chat_template_kwargs`
    ChatTemplate,
    /// `chat_template_kwargs.enable_thinking` and `preserve_thinking`
    QwenChatTemplate,
    /// top-level `thinking: string`
    StringThinking,
    /// `reasoning: { effort }` when mapped
    AntLing,
}

/// `maxTokensField` of the OpenAI completions compatibility settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensField {
    /// `max_completion_tokens`
    MaxCompletionTokens,
    /// `max_tokens`
    MaxTokens,
}

/// `thinkingTokenBudgetField` of the OpenAI completions compatibility settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingTokenBudgetField {
    /// vLLM
    ThinkingTokenBudget,
    /// Qwen, DashScope, SGLang
    ThinkingBudget,
    /// llama.cpp
    ThinkingBudgetTokens,
}

impl ThinkingTokenBudgetField {
    /// The request field name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ThinkingTokenBudget => "thinking_token_budget",
            Self::ThinkingBudget => "thinking_budget",
            Self::ThinkingBudgetTokens => "thinking_budget_tokens",
        }
    }
}

/// Session-affinity header format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionAffinityFormat {
    /// `session_id`, `x-client-request-id`, and (completions) `x-session-affinity`
    Openai,
    /// `x-client-request-id` and (completions) `x-session-affinity`
    OpenaiNosession,
    /// `x-session-id`
    Openrouter,
}

/// The compatibility settings of the three ported protocols. Pi types them
/// per API (`OpenAICompletionsCompat`, `OpenAIResponsesCompat`,
/// `AnthropicMessagesCompat`); their JSON is one object of optional members,
/// and each protocol reads the members it knows. Members of settings this
/// crate does not implement are ignored when read.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub struct ModelCompat {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_store: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_developer_role: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_reasoning_effort: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_usage_in_streaming: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_finish_reason: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens_field: Option<MaxTokensField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_tool_result_name: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_assistant_after_tool_result: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_thinking_as_text: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_reasoning_content_on_assistant_messages: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_format: Option<ThinkingFormat>,
    /// Values may be a scalar or `{ "$var": ..., "omitWhenOff"?: bool }`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_template_kwargs: Option<JsonObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_template_args: Option<JsonObject>,
    /// Sent as the `provider` request member.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_router_routing: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vercel_gateway_routing: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zai_tool_stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_token_budget_field: Option<ThinkingTokenBudgetField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_thinking_token_budget: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_system_messages: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_tool_additions: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_strict_mode: Option<bool>,
    /// Only `"anthropic"` is defined.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_session_affinity_headers: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_affinity_format: Option<SessionAffinityFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_long_cache_retention: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vllm_priority: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_explicit_prompt_cache_mode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_max_output_tokens: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_eager_tool_input_streaming: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_cache_control_on_tools: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_temperature: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force_adaptive_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_empty_signature: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_strict_tools: Option<bool>,
}

/// A chat model, Pi's `Model<Api>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    /// Model id sent to the provider.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Wire protocol, such as `openai-responses`.
    pub api: String,
    /// Provider id.
    pub provider: String,
    /// Base URL.
    pub base_url: String,
    /// Input modalities.
    pub input: Vec<InputModality>,
    /// Provider input limits, kept as given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<Value>,
    /// Pricing.
    pub cost: ModelCost,
    /// Extra request headers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    /// Whether the model reasons.
    pub reasoning: bool,
    /// Provider values per thinking level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    /// Prompt cache lifetimes, kept as given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<Value>,
    /// Context window in tokens.
    pub context_window: u64,
    /// Output limit in tokens.
    pub max_tokens: u64,
    /// Default sampling parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<JsonObject>,
    /// Sampling parameters per effective thinking level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling_params_by_thinking_level: Option<BTreeMap<String, JsonObject>>,
    /// Compatibility overrides.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<ModelCompat>,
}

impl Model {
    /// Whether the model accepts images.
    pub fn accepts_images(&self) -> bool {
        self.input.contains(&InputModality::Image)
    }

    /// The thinking-level map entry: `None` when missing, `Some(None)` for `null`.
    pub fn thinking_level_value(&self, level: ModelThinkingLevel) -> Option<Option<&str>> {
        self.thinking_level_map
            .as_ref()
            .and_then(|map| map.get(level))
    }
}

/// HTTP status and headers of a provider response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderResponse {
    /// Status code.
    pub status: u16,
    /// Headers, names lowercased.
    pub headers: BTreeMap<String, String>,
}

/// Pi's `AssistantMessageEvent`. `partial` is a snapshot of the response so
/// far when the event was pushed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantMessageEvent {
    /// The response started.
    Start {
        /// The response so far.
        partial: AssistantMessage,
    },
    /// A text block started.
    TextStart {
        /// Index into `partial.content`.
        #[serde(rename = "contentIndex")]
        content_index: usize,
        /// The response so far.
        partial: AssistantMessage,
    },
    /// Text arrived.
    TextDelta {
        /// Index into `partial.content`.
        #[serde(rename = "contentIndex")]
        content_index: usize,
        /// The new text.
        delta: String,
        /// The response so far.
        partial: AssistantMessage,
    },
    /// A text block ended.
    TextEnd {
        /// Index into `partial.content`.
        #[serde(rename = "contentIndex")]
        content_index: usize,
        /// The final text.
        content: String,
        /// The response so far.
        partial: AssistantMessage,
    },
    /// A thinking block started.
    ThinkingStart {
        /// Index into `partial.content`.
        #[serde(rename = "contentIndex")]
        content_index: usize,
        /// The response so far.
        partial: AssistantMessage,
    },
    /// Reasoning arrived.
    ThinkingDelta {
        /// Index into `partial.content`.
        #[serde(rename = "contentIndex")]
        content_index: usize,
        /// The new reasoning.
        delta: String,
        /// The response so far.
        partial: AssistantMessage,
    },
    /// A thinking block ended.
    ThinkingEnd {
        /// Index into `partial.content`.
        #[serde(rename = "contentIndex")]
        content_index: usize,
        /// The final reasoning.
        content: String,
        /// The response so far.
        partial: AssistantMessage,
    },
    /// A tool call started.
    #[serde(rename = "toolcall_start")]
    ToolCallStart {
        /// Index into `partial.content`.
        #[serde(rename = "contentIndex")]
        content_index: usize,
        /// The response so far.
        partial: AssistantMessage,
    },
    /// Argument JSON arrived.
    #[serde(rename = "toolcall_delta")]
    ToolCallDelta {
        /// Index into `partial.content`.
        #[serde(rename = "contentIndex")]
        content_index: usize,
        /// The new JSON text.
        delta: String,
        /// The response so far.
        partial: AssistantMessage,
    },
    /// A tool call ended.
    #[serde(rename = "toolcall_end")]
    ToolCallEnd {
        /// Index into `partial.content`.
        #[serde(rename = "contentIndex")]
        content_index: usize,
        /// The final call.
        #[serde(rename = "toolCall")]
        tool_call: ToolCall,
        /// The response so far.
        partial: AssistantMessage,
    },
    /// The response completed (`stop`, `length`, `toolUse`, or `deferred`).
    Done {
        /// The stop reason.
        reason: StopReason,
        /// The final message.
        message: AssistantMessage,
    },
    /// The response failed (`error` or `aborted`).
    Error {
        /// The stop reason.
        reason: StopReason,
        /// The final message.
        error: AssistantMessage,
    },
}

impl AssistantMessageEvent {
    /// The `type` spelling.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Start { .. } => "start",
            Self::TextStart { .. } => "text_start",
            Self::TextDelta { .. } => "text_delta",
            Self::TextEnd { .. } => "text_end",
            Self::ThinkingStart { .. } => "thinking_start",
            Self::ThinkingDelta { .. } => "thinking_delta",
            Self::ThinkingEnd { .. } => "thinking_end",
            Self::ToolCallStart { .. } => "toolcall_start",
            Self::ToolCallDelta { .. } => "toolcall_delta",
            Self::ToolCallEnd { .. } => "toolcall_end",
            Self::Done { .. } => "done",
            Self::Error { .. } => "error",
        }
    }
}

/// Accepts `null` or a missing member as an empty list, as Pi's
/// `transformMessages` does for untyped callers and old session files.
fn lax_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

fn lax_user_content<'de, D: Deserializer<'de>>(deserializer: D) -> Result<UserContent, D::Error> {
    Ok(Option::<UserContent>::deserialize(deserializer)?.unwrap_or_default())
}

fn lax_system_content<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<SystemContent, D::Error> {
    Ok(Option::<SystemContent>::deserialize(deserializer)?
        .unwrap_or(SystemContent::Blocks(Vec::new())))
}
