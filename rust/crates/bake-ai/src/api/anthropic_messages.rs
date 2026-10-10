//! The Anthropic Messages protocol (`anthropic-messages`).
//!
//! Ported from Pi `packages/ai/src/api/anthropic-messages.ts` (v1.1.0):
//! request building, message and tool conversion, beta features, prompt
//! caching, thinking, the raw SSE event decoder, event assembly, usage, stop
//! reasons, and errors. The request goes to `{baseUrl}/v1/messages?beta=true`
//! with the headers `@anthropic-ai/sdk`'s `beta.messages.create` sends.
//!
//! Not ported: GitHub Copilot headers, workload identity federation, managed
//! mid-conversation effort, server-side fallback models, native tool changes
//! (`tool_addition`/`tool_removal`), and the input-transformation diagnostic.
//! OAuth tokens (`sk-ant-oat`) keep Pi's bearer auth, Claude Code identity,
//! and tool-name casing.

use serde_json::{Map, Value, json};

use crate::api::constrained_sampling::{
    get_json_schema_tool_parameters, resolve_json_schema_strict_sampling,
};
use crate::api::simple_options::{
    adjust_max_tokens_for_thinking, build_base_options, clamp_max_tokens_to_context,
};
use crate::api::transform_messages::transform_messages;
use crate::api::{count, error_message, sanitize_id_chars, spawn_stream, truncate_units};
use crate::http::{
    HttpRequest, SdkErrorShape, header_layer, join_url, merge_headers, send, user_agent,
};
use crate::models::calculate_cost;
use crate::options::{
    ProviderHeaders, SimpleStreamOptions, StreamOptions, has_header, resolve_cache_retention,
};
use crate::stream::ApiProvider;
use crate::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, CacheRetention, Message, Model,
    ModelThinkingLevel, SessionAffinityFormat, StopReason, TextContent, ThinkingContent, Tool,
    ToolCall, TranscriptContext, UserContent, UserContentBlock,
};
use crate::utils::abort::is_aborted;
use crate::utils::error_body::ProviderError;
use crate::utils::event_stream::{AssistantMessageEventSender, AssistantMessageEventStream};
use crate::utils::json_parse::{parse_json_with_repair, parse_streaming_json_object};
use crate::utils::sse::ServerSentEvent;
use crate::utils::text::{get_system_message_text, render_system_message_update};
use crate::utils::transcript::{get_current_tools, get_initial_system_message, resolve_transcript};

const CLAUDE_CODE_VERSION: &str = "2.1.280";
const CLAUDE_CODE_TOOLS: [&str; 17] = [
    "Read",
    "Write",
    "Edit",
    "Bash",
    "Grep",
    "Glob",
    "AskUserQuestion",
    "EnterPlanMode",
    "ExitPlanMode",
    "KillShell",
    "NotebookEdit",
    "Skill",
    "Task",
    "TaskOutput",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
];
const FINE_GRAINED_TOOL_STREAMING_BETA: &str = "fine-grained-tool-streaming-2025-05-14";
const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";
const MESSAGE_EVENTS: [&str; 6] = [
    "message_start",
    "message_delta",
    "message_stop",
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
];
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Claude Code's casing of `name`, when it is one of its tools.
pub fn to_claude_code_name(name: &str) -> String {
    CLAUDE_CODE_TOOLS
        .iter()
        .find(|tool| tool.eq_ignore_ascii_case(name))
        .map_or_else(|| name.to_owned(), |tool| (*tool).to_owned())
}

/// The declared tool whose name matches `name` case-insensitively.
pub fn from_claude_code_name(name: &str, tools: &[Tool]) -> String {
    let lower = name.to_lowercase();
    tools
        .iter()
        .find(|tool| tool.name.to_lowercase() == lower)
        .map_or_else(|| name.to_owned(), |tool| tool.name.clone())
}

/// Adaptive-thinking effort levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum AnthropicEffort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl AnthropicEffort {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "xhigh" => Self::Xhigh,
            "max" => Self::Max,
            _ => return None,
        })
    }
}

/// Options of the Anthropic protocol.
#[derive(Debug, Clone, Default)]
pub struct AnthropicOptions {
    /// The shared options.
    pub base: StreamOptions,
    /// Extended thinking: `Some(false)` disables it explicitly.
    pub thinking_enabled: Option<bool>,
    /// Budget for budget-based thinking; 1024 when unset.
    pub thinking_budget_tokens: Option<u64>,
    /// Effort for adaptive thinking.
    pub effort: Option<AnthropicEffort>,
    /// `summarized` (the default) or `omitted`.
    pub thinking_display: Option<String>,
    /// Whether to request the interleaved-thinking beta; true when unset.
    pub interleaved_thinking: Option<bool>,
    /// `tool_choice`: a string becomes `{ "type": choice }`.
    pub tool_choice: Option<Value>,
}

/// Compatibility settings after defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct AnthropicCompat {
    pub supports_eager_tool_input_streaming: bool,
    pub supports_long_cache_retention: bool,
    pub send_session_affinity_headers: bool,
    pub session_affinity_format: Option<SessionAffinityFormat>,
    pub supports_cache_control_on_tools: bool,
    pub supports_temperature: bool,
    pub allow_empty_signature: bool,
    pub supports_strict_tools: bool,
    pub supports_mid_convo_system_messages: bool,
}

/// Pi's `getAnthropicCompat`.
pub fn get_compat(model: &Model) -> AnthropicCompat {
    let compat = model.compat.clone().unwrap_or_default();
    let is_open_router = model.provider == "openrouter" || model.base_url.contains("openrouter.ai");
    AnthropicCompat {
        supports_eager_tool_input_streaming: compat
            .supports_eager_tool_input_streaming
            .unwrap_or(true),
        supports_long_cache_retention: compat.supports_long_cache_retention.unwrap_or(true),
        send_session_affinity_headers: compat
            .send_session_affinity_headers
            .unwrap_or(is_open_router),
        session_affinity_format: compat
            .session_affinity_format
            .or(is_open_router.then_some(SessionAffinityFormat::Openrouter)),
        supports_cache_control_on_tools: compat.supports_cache_control_on_tools.unwrap_or(true),
        supports_temperature: compat.supports_temperature.unwrap_or(true),
        allow_empty_signature: compat.allow_empty_signature.unwrap_or(false),
        supports_strict_tools: compat.supports_strict_tools.unwrap_or(false),
        supports_mid_convo_system_messages: compat
            .supports_mid_convo_system_messages
            .unwrap_or(false),
    }
}

fn cache_control(model: &Model, options: &StreamOptions) -> Option<Value> {
    match resolve_cache_retention(options) {
        CacheRetention::None => None,
        CacheRetention::Long if get_compat(model).supports_long_cache_retention => {
            Some(json!({ "type": "ephemeral", "ttl": "1h" }))
        }
        _ => Some(json!({ "type": "ephemeral" })),
    }
}

fn has_request_auth(options: &StreamOptions) -> bool {
    let headers = options.headers.as_ref();
    options
        .api_key
        .as_deref()
        .is_some_and(|key| !key.is_empty())
        || has_header(headers, "authorization")
        || has_header(headers, "x-api-key")
        || has_header(headers, "cf-aig-authorization")
}

fn assert_request_auth(model: &Model, options: &StreamOptions) -> Result<(), String> {
    if has_request_auth(options) {
        Ok(())
    } else {
        Err(format!("No API key for provider: {}", model.provider))
    }
}

/// Whether `api_key` is a Claude subscription OAuth token.
pub fn is_oauth_token(api_key: &str) -> bool {
    api_key.contains("sk-ant-oat")
}

fn convert_content_blocks(content: &[UserContentBlock]) -> Value {
    if !content
        .iter()
        .any(|block| matches!(block, UserContentBlock::Image(_)))
    {
        let text = content
            .iter()
            .filter_map(|block| match block {
                UserContentBlock::Text(text) => Some(text.text.as_str()),
                UserContentBlock::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        return json!(text);
    }
    let mut blocks: Vec<Value> = content
        .iter()
        .map(|block| match block {
            UserContentBlock::Text(text) => json!({ "type": "text", "text": text.text }),
            UserContentBlock::Image(image) => json!({
                "type": "image",
                "source": { "type": "base64", "media_type": image.mime_type, "data": image.data }
            }),
        })
        .collect();
    if !content
        .iter()
        .any(|block| matches!(block, UserContentBlock::Text(_)))
    {
        blocks.insert(0, json!({ "type": "text", "text": "(see attached image)" }));
    }
    Value::Array(blocks)
}

fn normalize_tool_call_id(id: &str) -> String {
    truncate_units(&sanitize_id_chars(id), 64)
}

/// Pi's `convertMessages` without native tool changes or managed effort.
pub fn convert_messages(
    messages: &[Message],
    is_oauth: bool,
    cache_control: Option<&Value>,
    allow_empty_signature: bool,
) -> Vec<Value> {
    let mut params: Vec<Value> = Vec::new();
    let mut pending_system: Vec<Value> = Vec::new();
    let mut index = 0;
    while let Some(message) = messages.get(index) {
        match message {
            Message::System(system) => {
                let text = render_system_message_update(system);
                if !text.is_empty() {
                    pending_system.push(
                        json!({ "role": "system", "content": [{ "type": "text", "text": text }] }),
                    );
                }
            }
            Message::User(user) => match &user.content {
                UserContent::Text(text) => {
                    if !text.trim().is_empty() {
                        params.push(json!({ "role": "user", "content": text }));
                    }
                }
                UserContent::Blocks(blocks) => {
                    let content: Vec<Value> = blocks
                        .iter()
                        .filter(|block| !matches!(block, UserContentBlock::Text(text) if text.text.trim().is_empty()))
                        .map(|block| match block {
                            UserContentBlock::Text(text) => json!({ "type": "text", "text": text.text }),
                            UserContentBlock::Image(image) => json!({
                                "type": "image",
                                "source": { "type": "base64", "media_type": image.mime_type, "data": image.data }
                            }),
                        })
                        .collect();
                    if !content.is_empty() {
                        params.push(json!({ "role": "user", "content": content }));
                    }
                }
            },
            Message::Assistant(assistant) => {
                params.append(&mut pending_system);
                let mut blocks: Vec<Value> = Vec::new();
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Text(text) => {
                            if !text.text.trim().is_empty() {
                                blocks.push(json!({ "type": "text", "text": text.text }));
                            }
                        }
                        AssistantContentBlock::Thinking(thinking) => {
                            if thinking.redacted == Some(true) {
                                blocks.push(json!({
                                    "type": "redacted_thinking",
                                    "data": thinking.thinking_signature.clone().unwrap_or_default()
                                }));
                                continue;
                            }
                            let signature = thinking
                                .thinking_signature
                                .as_deref()
                                .filter(|signature| !signature.trim().is_empty());
                            if thinking.thinking.trim().is_empty() && signature.is_none() {
                                continue;
                            }
                            blocks.push(match signature {
                                Some(signature) => {
                                    json!({ "type": "thinking", "thinking": thinking.thinking, "signature": signature })
                                }
                                None if allow_empty_signature => {
                                    json!({ "type": "thinking", "thinking": thinking.thinking, "signature": "" })
                                }
                                None => json!({ "type": "text", "text": thinking.thinking }),
                            });
                        }
                        AssistantContentBlock::ToolCall(call) => {
                            let name = if is_oauth {
                                to_claude_code_name(&call.name)
                            } else {
                                call.name.clone()
                            };
                            blocks.push(json!({ "type": "tool_use", "id": call.id, "name": name, "input": call.arguments }));
                        }
                    }
                }
                if !blocks.is_empty() {
                    params.push(json!({ "role": "assistant", "content": blocks }));
                }
            }
            Message::ToolResult(_) => {
                let mut results = Vec::new();
                while let Some(Message::ToolResult(result)) = messages.get(index) {
                    results.push(json!({
                        "type": "tool_result",
                        "tool_use_id": result.tool_call_id,
                        "content": convert_content_blocks(&result.content),
                        "is_error": result.is_error
                    }));
                    index += 1;
                }
                params.push(json!({ "role": "user", "content": results }));
                continue;
            }
        }
        index += 1;
    }
    params.append(&mut pending_system);
    if let (Some(control), Some(last)) = (cache_control, params.last_mut()) {
        let role = last.get("role").and_then(Value::as_str);
        if matches!(role, Some("user" | "system"))
            && let Some(object) = last.as_object_mut()
        {
            match object.get_mut("content") {
                Some(Value::Array(blocks)) => {
                    if let Some(Value::Object(block)) = blocks.last_mut()
                        && matches!(
                            block.get("type").and_then(Value::as_str),
                            Some(
                                "text" | "image" | "tool_result" | "tool_addition" | "tool_removal"
                            )
                        )
                    {
                        block.insert("cache_control".into(), control.clone());
                    }
                }
                Some(Value::String(text)) => {
                    let text = std::mem::take(text);
                    object.insert(
                        "content".into(),
                        json!([{ "type": "text", "text": text, "cache_control": control }]),
                    );
                }
                _ => {}
            }
        }
    }
    params
}

fn is_strict_unsupported_keyword(key: &str, value: &Value) -> bool {
    const UNSUPPORTED: [&str; 11] = [
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
        "maxItems",
        "uniqueItems",
        "minContains",
        "maxContains",
        "minProperties",
        "maxProperties",
    ];
    const FORMATS: [&str; 10] = [
        "date-time",
        "time",
        "date",
        "duration",
        "email",
        "hostname",
        "uri",
        "ipv4",
        "ipv6",
        "uuid",
    ];
    if UNSUPPORTED.contains(&key) {
        return true;
    }
    match key {
        "minItems" => value
            .as_f64()
            .is_none_or(|count| count != 0.0 && count != 1.0),
        "format" => value
            .as_str()
            .is_none_or(|format| !FORMATS.contains(&format)),
        _ => false,
    }
}

/// Pi's `convertTools`.
pub fn convert_tools(
    tools: &[Tool],
    is_oauth: bool,
    compat: &AnthropicCompat,
    cache_control: Option<&Value>,
) -> Result<Vec<Value>, String> {
    let last = tools.len().saturating_sub(1);
    tools
        .iter()
        .enumerate()
        .map(|(index, tool)| {
            let strict = resolve_json_schema_strict_sampling(
                tool,
                compat.supports_strict_tools,
                Some(is_strict_unsupported_keyword),
            )?;
            let parameters = get_json_schema_tool_parameters(tool, strict)?;
            let mut input_schema = if strict == Some(true) {
                parameters.as_object().cloned().unwrap_or_default()
            } else {
                Map::new()
            };
            input_schema.insert("type".into(), json!("object"));
            input_schema.insert(
                "properties".into(),
                parameters
                    .get("properties")
                    .filter(|value| !value.is_null())
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            );
            input_schema.insert(
                "required".into(),
                parameters
                    .get("required")
                    .filter(|value| !value.is_null())
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            );
            let mut converted = Map::new();
            converted.insert(
                "name".into(),
                json!(if is_oauth {
                    to_claude_code_name(&tool.name)
                } else {
                    tool.name.clone()
                }),
            );
            converted.insert("description".into(), json!(tool.description));
            if compat.supports_eager_tool_input_streaming {
                converted.insert("eager_input_streaming".into(), json!(true));
            }
            if strict == Some(true) {
                converted.insert("strict".into(), json!(true));
            }
            converted.insert("input_schema".into(), Value::Object(input_schema));
            if let Some(control) = cache_control.filter(|_| index == last) {
                converted.insert("cache_control".into(), control.clone());
            }
            Ok(Value::Object(converted))
        })
        .collect()
}

fn configured_betas(model: &Model, options: &AnthropicOptions) -> Option<Option<String>> {
    let mut configured: Option<Option<String>> = None;
    if let Some(headers) = &model.headers {
        for (name, value) in headers {
            if name.eq_ignore_ascii_case("anthropic-beta") {
                configured = Some(Some(value.clone()));
            }
        }
    }
    for (name, value) in options.base.headers.iter().flatten() {
        if name.eq_ignore_ascii_case("anthropic-beta") {
            configured = Some(value.clone());
        }
    }
    configured
}

/// Pi's `getBetaFeatures`.
pub fn get_beta_features(
    model: &Model,
    context: &TranscriptContext,
    is_oauth: bool,
    options: &AnthropicOptions,
) -> Vec<String> {
    let mut features: Vec<String> = Vec::new();
    let mut add = |feature: &str| {
        if !features.iter().any(|existing| existing == feature) {
            features.push(feature.to_owned());
        }
    };
    match configured_betas(model, options) {
        Some(None) => return Vec::new(),
        Some(Some(configured)) => {
            for feature in configured
                .split(',')
                .map(str::trim)
                .filter(|feature| !feature.is_empty())
            {
                add(feature);
            }
            return features;
        }
        None => {}
    }
    if is_oauth {
        add("claude-code-20250219");
        add("oauth-2025-04-20");
    }
    if !get_current_tools(context.messages()).is_empty()
        && !get_compat(model).supports_eager_tool_input_streaming
    {
        add(FINE_GRAINED_TOOL_STREAMING_BETA);
    }
    let force_adaptive = model
        .compat
        .as_ref()
        .and_then(|compat| compat.force_adaptive_thinking)
        == Some(true);
    if model.reasoning
        && options.thinking_enabled == Some(true)
        && options.interleaved_thinking.unwrap_or(true)
        && !force_adaptive
    {
        add(INTERLEAVED_THINKING_BETA);
    }
    features
}

/// Pi's `buildParams`. Betas travel as the `betas` member, which the request
/// moves to the `anthropic-beta` header as the SDK does.
pub fn build_params(
    model: &Model,
    context: &TranscriptContext,
    is_oauth: bool,
    options: &AnthropicOptions,
) -> Result<Value, String> {
    let control = cache_control(model, &options.base);
    let compat = get_compat(model);
    let initial = get_initial_system_message(context.messages());
    let initial_text = initial.map(get_system_message_text).unwrap_or_default();
    let normalize = |id: &str, _: &Model, _: &AssistantMessage| normalize_tool_call_id(id);
    let transformed = transform_messages(context.messages(), model, Some(&normalize));
    let conversation = if initial.is_some() {
        transformed.get(1..).unwrap_or(&[])
    } else {
        &transformed[..]
    };
    let messages = convert_messages(
        conversation,
        is_oauth,
        control.as_ref(),
        compat.allow_empty_signature,
    );
    let betas = get_beta_features(model, context, is_oauth, options);
    let mut params = Map::new();
    params.insert("model".into(), json!(model.id));
    params.insert("messages".into(), Value::Array(messages));
    params.insert(
        "max_tokens".into(),
        json!(options.base.max_tokens.unwrap_or(model.max_tokens)),
    );
    params.insert("stream".into(), json!(true));
    if !betas.is_empty() {
        params.insert("betas".into(), json!(betas));
    }
    let system_block = |text: &str| {
        let mut block = json!({ "type": "text", "text": text });
        if let (Some(control), Some(object)) = (&control, block.as_object_mut()) {
            object.insert("cache_control".into(), control.clone());
        }
        block
    };
    if is_oauth {
        let mut system = vec![system_block(
            "You are Claude Code, Anthropic's official CLI for Claude.",
        )];
        if !initial_text.is_empty() {
            system.push(system_block(&initial_text));
        }
        params.insert("system".into(), Value::Array(system));
    } else if !initial_text.is_empty() {
        params.insert("system".into(), json!([system_block(&initial_text)]));
    }
    if let Some(temperature) = options.base.temperature
        && options.thinking_enabled != Some(true)
        && compat.supports_temperature
    {
        params.insert("temperature".into(), json!(temperature));
    }
    let tools = get_current_tools(context.messages());
    if !tools.is_empty() {
        let tool_control = control
            .as_ref()
            .filter(|_| compat.supports_cache_control_on_tools);
        params.insert(
            "tools".into(),
            Value::Array(convert_tools(&tools, is_oauth, &compat, tool_control)?),
        );
    }
    if model.reasoning {
        if options.thinking_enabled == Some(true) {
            let display = options
                .thinking_display
                .clone()
                .unwrap_or_else(|| "summarized".to_owned());
            if model
                .compat
                .as_ref()
                .and_then(|compat| compat.force_adaptive_thinking)
                == Some(true)
            {
                params.insert(
                    "thinking".into(),
                    json!({ "type": "adaptive", "display": display }),
                );
                if let Some(effort) = options.effort {
                    params.insert("output_config".into(), json!({ "effort": effort.as_str() }));
                }
            } else {
                let budget = options
                    .thinking_budget_tokens
                    .filter(|budget| *budget > 0)
                    .unwrap_or(1024);
                params.insert(
                    "thinking".into(),
                    json!({ "type": "enabled", "budget_tokens": budget, "display": display }),
                );
            }
        } else if options.thinking_enabled == Some(false)
            && model.thinking_level_value(ModelThinkingLevel::Off) != Some(None)
        {
            params.insert("thinking".into(), json!({ "type": "disabled" }));
        }
    }
    if let Some(user_id) = options
        .base
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("user_id"))
        .and_then(Value::as_str)
    {
        params.insert("metadata".into(), json!({ "user_id": user_id }));
    }
    match &options.tool_choice {
        Some(Value::String(choice)) if !choice.is_empty() => {
            params.insert("tool_choice".into(), json!({ "type": choice }));
        }
        Some(choice @ Value::Object(_)) => {
            params.insert("tool_choice".into(), choice.clone());
        }
        _ => {}
    }
    Ok(Value::Object(params))
}

/// Pi's `mapStopReason`.
pub fn map_stop_reason(
    reason: &str,
    stop_details: Option<&Value>,
) -> Result<(StopReason, Option<String>), String> {
    Ok(match reason {
        "end_turn" | "pause_turn" | "stop_sequence" => (StopReason::Stop, None),
        "max_tokens" => (StopReason::Length, None),
        "tool_use" => (StopReason::ToolUse, None),
        "refusal" => {
            let explanation = stop_details
                .and_then(|details| details.get("explanation"))
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .unwrap_or("The model refused to complete the request");
            (StopReason::Error, Some(explanation.to_owned()))
        }
        "sensitive" => (
            StopReason::Error,
            Some("Provider stopped with: sensitive".to_owned()),
        ),
        other => return Err(format!("Unhandled stop reason: {other}")),
    })
}

/// Pi's `iterateAnthropicEvents` for one decoded event: `Ok(None)` skips it.
pub fn decode_event(sse: &ServerSentEvent) -> Result<Option<Value>, String> {
    if sse.event.as_deref() == Some("error") {
        return Err(sse.data.clone());
    }
    if !MESSAGE_EVENTS.contains(&sse.event.as_deref().unwrap_or("")) {
        return Ok(None);
    }
    parse_json_with_repair(&sse.data)
        .map(Some)
        .map_err(|error| {
            format!(
                "Could not parse Anthropic SSE event {}: {error}; data={}; raw={}",
                sse.event.as_deref().unwrap_or("null"),
                sse.data,
                sse.raw.join("\\n")
            )
        })
}

/// Assembles Anthropic stream events into an assistant message.
pub struct AnthropicStreamProcessor {
    model: Model,
    /// The message being built.
    pub output: AssistantMessage,
    is_oauth: bool,
    tools: Vec<Tool>,
    /// `(content index, provider index, partial JSON)` of open blocks.
    open: Vec<(usize, i64, String)>,
    saw_start: bool,
    saw_stop: bool,
}

impl AnthropicStreamProcessor {
    /// A processor for `model`; `tools` restore OAuth tool-name casing.
    pub fn new(model: &Model, output: AssistantMessage, is_oauth: bool, tools: Vec<Tool>) -> Self {
        Self {
            model: model.clone(),
            output,
            is_oauth,
            tools,
            open: Vec::new(),
            saw_start: false,
            saw_stop: false,
        }
    }

    fn emit(
        &self,
        sender: &AssistantMessageEventSender,
        make: impl FnOnce(AssistantMessage) -> AssistantMessageEvent,
    ) {
        sender.push(make(self.output.clone()));
    }

    fn find(&self, provider_index: i64) -> Option<usize> {
        self.open
            .iter()
            .position(|(_, index, _)| *index == provider_index)
    }

    fn update_total(&mut self) {
        let usage = &mut self.output.usage;
        usage.total_tokens = usage.component_sum();
        calculate_cost(&self.model, &mut self.output.usage);
    }

    /// Applies one decoded event.
    pub fn handle_event(
        &mut self,
        event: &Value,
        sender: &AssistantMessageEventSender,
    ) -> Result<(), String> {
        let provider_index = event.get("index").and_then(Value::as_i64).unwrap_or(-1);
        match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start" => {
                self.saw_start = true;
                let message = event.get("message").cloned().unwrap_or(Value::Null);
                self.output.response_id =
                    message.get("id").and_then(Value::as_str).map(str::to_owned);
                if let Some(response_model) = message.get("model").and_then(Value::as_str)
                    && response_model != self.model.id
                {
                    self.output.response_model = Some(response_model.to_owned());
                }
                let usage = message.get("usage");
                let field =
                    |name: &str| count(usage.and_then(|usage| usage.get(name))).unwrap_or(0);
                self.output.usage.input = field("input_tokens");
                self.output.usage.output = field("output_tokens");
                self.output.usage.cache_read = field("cache_read_input_tokens");
                self.output.usage.cache_write = field("cache_creation_input_tokens");
                self.output.usage.cache_write_1h = Some(
                    count(
                        usage
                            .and_then(|usage| usage.get("cache_creation"))
                            .and_then(|c| c.get("ephemeral_1h_input_tokens")),
                    )
                    .unwrap_or(0),
                );
                self.update_total();
            }
            "content_block_start" => {
                let block = event.get("content_block").cloned().unwrap_or(Value::Null);
                let text_of = |name: &str| {
                    block
                        .get(name)
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned()
                };
                let (content, kind) = match block.get("type").and_then(Value::as_str) {
                    Some("fallback") => {
                        if !self.output.content.is_empty() {
                            return Err(
                                "Anthropic performed an unsupported mid-output model fallback"
                                    .to_owned(),
                            );
                        }
                        return Ok(());
                    }
                    Some("text") => (
                        AssistantContentBlock::Text(TextContent::new(text_of("text"))),
                        0,
                    ),
                    Some("thinking") => (
                        AssistantContentBlock::Thinking(ThinkingContent {
                            thinking: text_of("thinking"),
                            thinking_signature: Some(text_of("signature")),
                            redacted: None,
                        }),
                        1,
                    ),
                    Some("redacted_thinking") => (
                        AssistantContentBlock::Thinking(ThinkingContent {
                            thinking: "[Reasoning redacted]".to_owned(),
                            thinking_signature: block
                                .get("data")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            redacted: Some(true),
                        }),
                        1,
                    ),
                    Some("tool_use") => {
                        let name = text_of("name");
                        (
                            AssistantContentBlock::ToolCall(ToolCall {
                                id: text_of("id"),
                                name: if self.is_oauth {
                                    from_claude_code_name(&name, &self.tools)
                                } else {
                                    name
                                },
                                arguments: block
                                    .get("input")
                                    .and_then(Value::as_object)
                                    .cloned()
                                    .unwrap_or_default(),
                                ..ToolCall::default()
                            }),
                            2,
                        )
                    }
                    _ => return Ok(()),
                };
                self.output.content.push(content);
                let index = self.output.content.len() - 1;
                self.open.push((index, provider_index, String::new()));
                match kind {
                    0 => self.emit(sender, |partial| AssistantMessageEvent::TextStart {
                        content_index: index,
                        partial,
                    }),
                    1 => self.emit(sender, |partial| AssistantMessageEvent::ThinkingStart {
                        content_index: index,
                        partial,
                    }),
                    _ => self.emit(sender, |partial| AssistantMessageEvent::ToolCallStart {
                        content_index: index,
                        partial,
                    }),
                }
            }
            "content_block_delta" => {
                let Some(slot) = self.find(provider_index) else {
                    return Ok(());
                };
                let index = self.open.get(slot).map_or(0, |(index, _, _)| *index);
                let delta = event.get("delta").cloned().unwrap_or(Value::Null);
                let text_of = |name: &str| {
                    delta
                        .get(name)
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned()
                };
                match (
                    delta.get("type").and_then(Value::as_str),
                    self.output.content.get_mut(index),
                ) {
                    (Some("text_delta"), Some(AssistantContentBlock::Text(block))) => {
                        let text = text_of("text");
                        block.text.push_str(&text);
                        self.emit(sender, |partial| AssistantMessageEvent::TextDelta {
                            content_index: index,
                            delta: text,
                            partial,
                        });
                    }
                    (Some("thinking_delta"), Some(AssistantContentBlock::Thinking(block))) => {
                        let text = text_of("thinking");
                        block.thinking.push_str(&text);
                        self.emit(sender, |partial| AssistantMessageEvent::ThinkingDelta {
                            content_index: index,
                            delta: text,
                            partial,
                        });
                    }
                    (Some("input_json_delta"), Some(AssistantContentBlock::ToolCall(_))) => {
                        let text = text_of("partial_json");
                        let Some((_, _, partial_json)) = self.open.get_mut(slot) else {
                            return Ok(());
                        };
                        partial_json.push_str(&text);
                        let arguments = parse_streaming_json_object(Some(partial_json));
                        if let Some(AssistantContentBlock::ToolCall(call)) =
                            self.output.content.get_mut(index)
                        {
                            call.arguments = arguments;
                        }
                        self.emit(sender, |partial| AssistantMessageEvent::ToolCallDelta {
                            content_index: index,
                            delta: text,
                            partial,
                        });
                    }
                    (Some("signature_delta"), Some(AssistantContentBlock::Thinking(block))) => {
                        block
                            .thinking_signature
                            .get_or_insert_with(String::new)
                            .push_str(&text_of("signature"));
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let Some(slot) = self.find(provider_index) else {
                    return Ok(());
                };
                let (index, _, partial_json) = self.open.remove(slot);
                match self.output.content.get_mut(index) {
                    Some(AssistantContentBlock::Text(block)) => {
                        let content = block.text.clone();
                        self.emit(sender, |partial| AssistantMessageEvent::TextEnd {
                            content_index: index,
                            content,
                            partial,
                        });
                    }
                    Some(AssistantContentBlock::Thinking(block)) => {
                        let content = block.thinking.clone();
                        self.emit(sender, |partial| AssistantMessageEvent::ThinkingEnd {
                            content_index: index,
                            content,
                            partial,
                        });
                    }
                    Some(AssistantContentBlock::ToolCall(call)) => {
                        call.arguments = parse_streaming_json_object(Some(&partial_json));
                        let tool_call = call.clone();
                        self.emit(sender, |partial| AssistantMessageEvent::ToolCallEnd {
                            content_index: index,
                            tool_call,
                            partial,
                        });
                    }
                    None => {}
                }
            }
            "message_delta" => {
                let delta = event.get("delta");
                if let Some(reason) = delta
                    .and_then(|delta| delta.get("stop_reason"))
                    .and_then(Value::as_str)
                    .filter(|r| !r.is_empty())
                {
                    self.output.raw_stop_reason = Some(reason.to_owned());
                    let (stop_reason, error) =
                        map_stop_reason(reason, delta.and_then(|delta| delta.get("stop_details")))?;
                    self.output.stop_reason = stop_reason;
                    if error.is_some() {
                        self.output.error_message = error;
                    }
                }
                if let Some(usage) = event.get("usage").filter(|usage| !usage.is_null()) {
                    let field =
                        |name: &str| count(usage.get(name).filter(|value| !value.is_null()));
                    if let Some(value) = field("input_tokens") {
                        self.output.usage.input = value;
                    }
                    if let Some(value) = field("output_tokens") {
                        self.output.usage.output = value;
                    }
                    if let Some(value) = field("cache_read_input_tokens") {
                        self.output.usage.cache_read = value;
                    }
                    if let Some(value) = field("cache_creation_input_tokens") {
                        self.output.usage.cache_write = value;
                    }
                    if let Some(value) = count(
                        usage
                            .get("cache_creation")
                            .and_then(|c| c.get("ephemeral_1h_input_tokens")),
                    ) {
                        self.output.usage.cache_write_1h = Some(value);
                    }
                    if let Some(value) = count(
                        usage
                            .get("output_tokens_details")
                            .and_then(|d| d.get("thinking_tokens")),
                    ) {
                        self.output.usage.reasoning = Some(value);
                    }
                }
                self.update_total();
            }
            "message_stop" => self.saw_stop = true,
            _ => {}
        }
        Ok(())
    }

    /// Checks the stream after its last event.
    pub fn finish(&self) -> Result<(), String> {
        if self.saw_start && !self.saw_stop {
            return Err("Anthropic stream ended before message_stop".to_owned());
        }
        Ok(())
    }
}

fn request_headers(
    model: &Model,
    options: &AnthropicOptions,
    api_key: Option<&str>,
    betas: &[String],
) -> Vec<(String, String)> {
    let oauth = api_key.is_some_and(is_oauth_token);
    let mut base: ProviderHeaders = vec![
        ("Accept".into(), Some("application/json".into())),
        ("User-Agent".into(), Some(user_agent())),
        (
            "anthropic-dangerous-direct-browser-access".into(),
            Some("true".into()),
        ),
        ("anthropic-version".into(), Some(ANTHROPIC_VERSION.into())),
    ];
    if let Some(key) = api_key {
        if oauth {
            base.push(("Authorization".into(), Some(format!("Bearer {key}"))));
        } else {
            base.push(("X-Api-Key".into(), Some(key.to_owned())));
        }
    }
    let mut defaults: ProviderHeaders = vec![
        ("User-Agent".into(), Some(user_agent())),
        ("accept".into(), Some("application/json".into())),
        (
            "anthropic-dangerous-direct-browser-access".into(),
            Some("true".into()),
        ),
    ];
    if oauth {
        defaults.push((
            "user-agent".into(),
            Some(format!("claude-cli/{CLAUDE_CODE_VERSION}")),
        ));
        defaults.push(("x-app".into(), Some("cli".into())));
    } else {
        let compat = get_compat(model);
        let retention = resolve_cache_retention(&options.base);
        if let Some(session_id) = options
            .base
            .session_id
            .as_deref()
            .filter(|_| retention != CacheRetention::None)
            && compat.send_session_affinity_headers
        {
            let name = if compat.session_affinity_format == Some(SessionAffinityFormat::Openrouter)
            {
                "x-session-id"
            } else {
                "x-session-affinity"
            };
            defaults.push((name.into(), Some(session_id.to_owned())));
        }
    }
    if let Some(headers) = &model.headers {
        defaults.extend(header_layer(headers));
    }
    if let Some(headers) = &options.base.headers {
        defaults.extend(headers.iter().cloned());
    }
    let body: ProviderHeaders = vec![("content-type".into(), Some("application/json".into()))];
    let request: ProviderHeaders = if betas.is_empty() {
        Vec::new()
    } else {
        vec![("anthropic-beta".into(), Some(betas.join(",")))]
    };
    merge_headers(&[&base, &defaults, &body, &request])
}

async fn run(
    sender: AssistantMessageEventSender,
    model: Model,
    context: TranscriptContext,
    options: AnthropicOptions,
) {
    let context = resolve_transcript(
        &context,
        get_compat(&model).supports_mid_convo_system_messages,
    );
    let api_key = options.base.api_key.clone().filter(|key| !key.is_empty());
    let is_oauth = api_key.as_deref().is_some_and(is_oauth_token);
    let tools = get_current_tools(context.messages());
    let mut processor =
        AnthropicStreamProcessor::new(&model, AssistantMessage::pending(&model), is_oauth, tools);
    match run_inner(
        &sender,
        &model,
        &context,
        &options,
        api_key.as_deref(),
        &mut processor,
    )
    .await
    {
        Ok(()) => sender.finish(processor.output.clone()),
        Err(error) => {
            let mut output = processor.output.clone();
            output.stop_reason = if is_aborted(options.base.signal.as_ref()) {
                StopReason::Aborted
            } else {
                StopReason::Error
            };
            output.error_message = Some(error.message.clone());
            sender.finish(output);
        }
    }
}

async fn run_inner(
    sender: &AssistantMessageEventSender,
    model: &Model,
    context: &TranscriptContext,
    options: &AnthropicOptions,
    api_key: Option<&str>,
    processor: &mut AnthropicStreamProcessor,
) -> Result<(), ProviderError> {
    assert_request_auth(model, &options.base).map_err(ProviderError::other)?;
    let is_oauth = api_key.is_some_and(is_oauth_token);
    let mut params =
        build_params(model, context, is_oauth, options).map_err(ProviderError::other)?;
    if let Some(on_payload) = &options.base.on_payload
        && let Some(mut next) = on_payload(&params, model)
    {
        if let Some(object) = next.as_object_mut() {
            object.insert("stream".into(), json!(true));
        }
        params = next;
    }
    let betas: Vec<String> = match params
        .as_object_mut()
        .and_then(|object| object.remove("betas"))
    {
        Some(Value::Array(betas)) => betas
            .iter()
            .filter_map(|beta| beta.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    };
    let request = HttpRequest {
        url: join_url(&model.base_url, "/v1/messages?beta=true"),
        headers: request_headers(model, options, api_key, &betas),
        body: serde_json::to_vec(&params)
            .map_err(|error| ProviderError::other(error.to_string()))?,
        error_shape: SdkErrorShape::Anthropic,
    };
    let mut response = send(&request, &options.base).await?;
    if let Some(on_response) = &options.base.on_response {
        on_response(&response.info, model);
    }
    sender.push(AssistantMessageEvent::Start {
        partial: processor.output.clone(),
    });
    while let Some(events) = response.next_events().await? {
        for sse in events {
            if is_aborted(options.base.signal.as_ref()) {
                return Err(ProviderError::aborted());
            }
            let Some(event) = decode_event(&sse).map_err(ProviderError::other)? else {
                continue;
            };
            if let Some(observer) = &options.base.on_provider_stream_event {
                observer(&event, model);
            }
            processor
                .handle_event(&event, sender)
                .map_err(ProviderError::other)?;
        }
        if sender.is_closed() {
            return Err(ProviderError::aborted());
        }
    }
    processor.finish().map_err(ProviderError::other)?;
    if is_aborted(options.base.signal.as_ref()) {
        return Err(ProviderError::aborted());
    }
    match processor.output.stop_reason {
        StopReason::Pending => Err(ProviderError::other(
            "Anthropic stream ended without a stop reason",
        )),
        StopReason::Aborted | StopReason::Error => Err(ProviderError::other(
            processor
                .output
                .error_message
                .clone()
                .filter(|error| !error.is_empty())
                .unwrap_or_else(|| "An unknown error occurred".to_owned()),
        )),
        _ => Ok(()),
    }
}

/// Streams a request to an Anthropic Messages endpoint.
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: AnthropicOptions,
) -> AssistantMessageEventStream {
    let model = model.clone();
    let context = context.clone();
    spawn_stream(&model.clone(), move |sender| {
        run(sender, model, context, options)
    })
}

fn map_thinking_level_to_effort(
    model: &Model,
    level: crate::types::ThinkingLevel,
) -> AnthropicEffort {
    if let Some(Some(mapped)) = model.thinking_level_value(ModelThinkingLevel::from(level))
        && let Some(effort) = AnthropicEffort::parse(mapped)
    {
        return effort;
    }
    match level.as_str() {
        "minimal" | "low" => AnthropicEffort::Low,
        "medium" => AnthropicEffort::Medium,
        _ => AnthropicEffort::High,
    }
}

/// Maps simple options to Anthropic options and streams.
pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> AssistantMessageEventStream {
    if let Err(error) = assert_request_auth(model, &options.base) {
        let failed = error_message(model, &error);
        return spawn_stream(model, move |sender| async move { sender.finish(failed) });
    }
    let base = build_base_options(model, context, &options);
    let tool_choice = options.tool_choice.map(|choice| json!(choice.as_str()));
    let Some(level) = options.reasoning else {
        return stream(
            model,
            context,
            AnthropicOptions {
                base,
                thinking_enabled: Some(false),
                tool_choice,
                ..Default::default()
            },
        );
    };
    if model
        .compat
        .as_ref()
        .and_then(|compat| compat.force_adaptive_thinking)
        == Some(true)
    {
        let effort = map_thinking_level_to_effort(model, level);
        return stream(
            model,
            context,
            AnthropicOptions {
                base,
                thinking_enabled: Some(true),
                effort: Some(effort),
                tool_choice,
                ..Default::default()
            },
        );
    }
    let (adjusted, thinking_budget) = adjust_max_tokens_for_thinking(
        base.max_tokens,
        model.max_tokens,
        level,
        options.thinking_budgets.as_ref(),
    );
    let max_tokens = clamp_max_tokens_to_context(model, context, adjusted);
    stream(
        model,
        context,
        AnthropicOptions {
            base: StreamOptions {
                max_tokens: Some(max_tokens),
                ..base
            },
            thinking_enabled: Some(true),
            thinking_budget_tokens: Some(thinking_budget.min(max_tokens.saturating_sub(1024))),
            tool_choice,
            ..Default::default()
        },
    )
}

/// The built-in `anthropic-messages` API.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnthropicMessagesApi;

impl ApiProvider for AnthropicMessagesApi {
    fn api(&self) -> &str {
        "anthropic-messages"
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        stream(
            model,
            context,
            AnthropicOptions {
                base: options,
                ..Default::default()
            },
        )
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        stream_simple(model, context, options)
    }
}
