//! The OpenAI Chat Completions protocol (`openai-completions`).
//!
//! Ported from Pi `packages/ai/src/api/openai-completions.ts` (v1.1.0):
//! compatibility detection, request building, message and tool conversion,
//! streamed chunk assembly, usage, stop reasons, and errors. The request goes
//! to `{baseUrl}/chat/completions` as the `openai` SDK sends it. Not ported:
//! GitHub Copilot headers and grammar-constrained (custom) tools.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use crate::api::constrained_sampling::{
    get_json_schema_tool_parameters, resolve_json_schema_strict_sampling,
};
use crate::api::simple_options::{
    build_base_options, clamp_thinking_budget_to_answer_room, resolve_sampling_params,
    thinking_budget_for_level,
};
use crate::api::transform_messages::transform_messages;
use crate::api::{
    count, error_message, non_empty_str, sanitize_id_chars, spawn_stream, truncate_units, truthy,
};
use crate::http::{
    HttpRequest, SdkErrorShape, header_layer, join_url, merge_headers, send, user_agent,
};
use crate::models::{calculate_cost, clamp_thinking_level};
use crate::options::{
    ProviderHeaders, SimpleStreamOptions, StreamOptions, has_header, resolve_cache_retention,
};
use crate::stream::ApiProvider;
use crate::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, CacheRetention, MaxTokensField,
    Message, Model, ModelThinkingLevel, SessionAffinityFormat, StopReason, TextContent,
    ThinkingBudgets, ThinkingContent, ThinkingFormat, ThinkingLevel, ThinkingTokenBudgetField,
    Tool, ToolCall, TranscriptContext, Usage, UserContent, UserContentBlock,
};
use crate::utils::abort::is_aborted;
use crate::utils::error_body::{ProviderError, format_provider_error, normalize_provider_error};
use crate::utils::event_stream::{AssistantMessageEventSender, AssistantMessageEventStream};
use crate::utils::hash::short_hash;
use crate::utils::json_parse::{parse_json_strict, parse_streaming_json_object};
use crate::utils::text::{get_system_message_text, render_system_message_update};
use crate::utils::transcript::{resolve_transcript, resolve_transcript_tools};

/// Options of the completions protocol.
#[derive(Debug, Clone, Default)]
pub struct OpenAICompletionsOptions {
    /// The shared options.
    pub base: StreamOptions,
    /// `tool_choice`, sent as given.
    pub tool_choice: Option<Value>,
    /// The reasoning effort; `None` is off.
    pub reasoning_effort: Option<ThinkingLevel>,
    /// Token budgets for `thinkingTokenBudgetField` and `thinking.budget`.
    pub thinking_budgets: Option<ThinkingBudgets>,
}

/// Compatibility settings after detection and overrides.
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs)]
pub struct ResolvedCompletionsCompat {
    pub supports_store: bool,
    pub supports_developer_role: bool,
    pub supports_reasoning_effort: bool,
    pub supports_usage_in_streaming: bool,
    pub supports_finish_reason: bool,
    pub max_tokens_field: MaxTokensField,
    pub requires_tool_result_name: bool,
    pub requires_assistant_after_tool_result: bool,
    pub requires_thinking_as_text: bool,
    pub requires_reasoning_content_on_assistant_messages: bool,
    pub thinking_format: ThinkingFormat,
    pub open_router_routing: Option<Value>,
    pub vercel_gateway_routing: Option<Value>,
    pub chat_template_kwargs: Map<String, Value>,
    pub chat_template_args: Map<String, Value>,
    pub zai_tool_stream: bool,
    pub supports_thinking_token_budget: bool,
    pub thinking_token_budget_field: Option<ThinkingTokenBudgetField>,
    pub supports_strict_mode: bool,
    pub supports_mid_convo_system_messages: bool,
    pub supports_mid_convo_tool_additions: bool,
    pub cache_control_format: Option<String>,
    pub send_session_affinity_headers: bool,
    pub session_affinity_format: SessionAffinityFormat,
    pub supports_long_cache_retention: bool,
    pub vllm_priority: Option<i64>,
}

/// Pi's `detectCompat`: settings from the provider id and base URL.
pub fn detect_compat(model: &Model) -> ResolvedCompletionsCompat {
    let provider = model.provider.as_str();
    let base_url = model.base_url.as_str();
    let is_zai = provider == "zai"
        || provider == "zai-coding-cn"
        || base_url.contains("api.z.ai")
        || base_url.contains("open.bigmodel.cn");
    let is_together = provider == "together"
        || base_url.contains("api.together.ai")
        || base_url.contains("api.together.xyz");
    let is_moonshot = provider == "moonshotai"
        || provider == "moonshotai-cn"
        || base_url.contains("api.moonshot.");
    let is_open_router = provider == "openrouter" || base_url.contains("openrouter.ai");
    let is_cloudflare_workers_ai =
        provider == "cloudflare-workers-ai" || base_url.contains("api.cloudflare.com");
    let is_cloudflare_ai_gateway =
        provider == "cloudflare-ai-gateway" || base_url.contains("gateway.ai.cloudflare.com");
    let is_nvidia = provider == "nvidia" || base_url.contains("integrate.api.nvidia.com");
    let is_ant_ling = provider == "ant-ling" || base_url.contains("api.ant-ling.com");
    let is_cerebras = provider == "cerebras" || base_url.contains("cerebras.ai");
    let is_deepseek = provider == "deepseek" || base_url.to_lowercase().contains("deepseek.com");
    let is_grok = provider == "xai" || base_url.contains("api.x.ai");
    let is_non_standard = is_nvidia
        || is_cerebras
        || is_grok
        || is_together
        || base_url.contains("chutes.ai")
        || is_deepseek
        || is_zai
        || is_moonshot
        || provider == "opencode"
        || base_url.contains("opencode.ai")
        || is_cloudflare_workers_ai
        || is_cloudflare_ai_gateway
        || is_ant_ling;
    let use_max_tokens = base_url.contains("chutes.ai")
        || is_deepseek
        || is_moonshot
        || is_cloudflare_ai_gateway
        || is_together
        || is_nvidia
        || is_ant_ling
        || is_zai;
    let open_router_developer_role =
        is_open_router && (model.id.starts_with("anthropic/") || model.id.starts_with("openai/"));
    let cache_control_format = (provider == "openrouter" && model.id.starts_with("anthropic/"))
        .then(|| "anthropic".to_owned());
    ResolvedCompletionsCompat {
        supports_store: !is_non_standard,
        supports_developer_role: open_router_developer_role
            || (!is_non_standard && !is_open_router),
        supports_reasoning_effort: !is_grok
            && !is_zai
            && !is_moonshot
            && !is_together
            && !is_cloudflare_ai_gateway
            && !is_nvidia
            && !is_ant_ling,
        supports_usage_in_streaming: true,
        supports_finish_reason: true,
        max_tokens_field: if use_max_tokens {
            MaxTokensField::MaxTokens
        } else {
            MaxTokensField::MaxCompletionTokens
        },
        requires_tool_result_name: false,
        requires_assistant_after_tool_result: false,
        requires_thinking_as_text: false,
        requires_reasoning_content_on_assistant_messages: is_deepseek,
        thinking_format: if is_deepseek {
            ThinkingFormat::Deepseek
        } else if is_zai {
            ThinkingFormat::Zai
        } else if is_together {
            ThinkingFormat::Together
        } else if is_ant_ling {
            ThinkingFormat::AntLing
        } else if is_open_router {
            ThinkingFormat::Openrouter
        } else {
            ThinkingFormat::Openai
        },
        open_router_routing: None,
        vercel_gateway_routing: None,
        chat_template_kwargs: Map::new(),
        chat_template_args: Map::new(),
        zai_tool_stream: false,
        supports_thinking_token_budget: false,
        thinking_token_budget_field: None,
        supports_strict_mode: false,
        supports_mid_convo_system_messages: false,
        supports_mid_convo_tool_additions: false,
        cache_control_format,
        send_session_affinity_headers: is_open_router,
        session_affinity_format: if is_open_router {
            SessionAffinityFormat::Openrouter
        } else {
            SessionAffinityFormat::Openai
        },
        supports_long_cache_retention: !(is_together
            || is_cloudflare_workers_ai
            || is_cloudflare_ai_gateway
            || is_nvidia
            || is_ant_ling),
        vllm_priority: None,
    }
}

/// Pi's `getCompat`: detection overridden by `model.compat`.
pub fn get_compat(model: &Model) -> ResolvedCompletionsCompat {
    let detected = detect_compat(model);
    let Some(compat) = &model.compat else {
        return detected;
    };
    ResolvedCompletionsCompat {
        supports_store: compat.supports_store.unwrap_or(detected.supports_store),
        supports_developer_role: compat
            .supports_developer_role
            .unwrap_or(detected.supports_developer_role),
        supports_reasoning_effort: compat
            .supports_reasoning_effort
            .unwrap_or(detected.supports_reasoning_effort),
        supports_usage_in_streaming: compat
            .supports_usage_in_streaming
            .unwrap_or(detected.supports_usage_in_streaming),
        supports_finish_reason: compat
            .supports_finish_reason
            .unwrap_or(detected.supports_finish_reason),
        max_tokens_field: compat.max_tokens_field.unwrap_or(detected.max_tokens_field),
        requires_tool_result_name: compat
            .requires_tool_result_name
            .unwrap_or(detected.requires_tool_result_name),
        requires_assistant_after_tool_result: compat
            .requires_assistant_after_tool_result
            .unwrap_or(detected.requires_assistant_after_tool_result),
        requires_thinking_as_text: compat
            .requires_thinking_as_text
            .unwrap_or(detected.requires_thinking_as_text),
        requires_reasoning_content_on_assistant_messages: compat
            .requires_reasoning_content_on_assistant_messages
            .unwrap_or(detected.requires_reasoning_content_on_assistant_messages),
        thinking_format: compat.thinking_format.unwrap_or(detected.thinking_format),
        open_router_routing: compat.open_router_routing.clone(),
        vercel_gateway_routing: compat.vercel_gateway_routing.clone(),
        chat_template_kwargs: compat.chat_template_kwargs.clone().unwrap_or_default(),
        chat_template_args: compat.chat_template_args.clone().unwrap_or_default(),
        zai_tool_stream: compat.zai_tool_stream.unwrap_or(detected.zai_tool_stream),
        supports_thinking_token_budget: compat
            .supports_thinking_token_budget
            .unwrap_or(detected.supports_thinking_token_budget),
        thinking_token_budget_field: compat
            .thinking_token_budget_field
            .or(detected.thinking_token_budget_field),
        supports_strict_mode: compat
            .supports_strict_mode
            .unwrap_or(detected.supports_strict_mode),
        supports_mid_convo_system_messages: compat
            .supports_mid_convo_system_messages
            .unwrap_or(detected.supports_mid_convo_system_messages),
        supports_mid_convo_tool_additions: compat
            .supports_mid_convo_tool_additions
            .unwrap_or(detected.supports_mid_convo_tool_additions),
        cache_control_format: compat
            .cache_control_format
            .clone()
            .or(detected.cache_control_format),
        send_session_affinity_headers: compat
            .send_session_affinity_headers
            .unwrap_or(detected.send_session_affinity_headers),
        session_affinity_format: compat
            .session_affinity_format
            .unwrap_or(detected.session_affinity_format),
        supports_long_cache_retention: compat
            .supports_long_cache_retention
            .unwrap_or(detected.supports_long_cache_retention),
        vllm_priority: compat.vllm_priority,
    }
}

fn client_api_key(model: &Model, options: &StreamOptions) -> Result<String, String> {
    if let Some(key) = options.api_key.as_deref().filter(|key| !key.is_empty()) {
        return Ok(key.to_owned());
    }
    let headers = options.headers.as_ref();
    if has_header(headers, "authorization") || has_header(headers, "cf-aig-authorization") {
        return Ok("unused".to_owned());
    }
    Err(format!("No API key for provider: {}", model.provider))
}

fn has_tool_history(messages: &[Message]) -> bool {
    messages.iter().any(|message| match message {
        Message::ToolResult(_) => true,
        Message::Assistant(assistant) => assistant.tool_calls().next().is_some(),
        _ => false,
    })
}

const REASONING_FIELDS: [&str; 3] = ["reasoning", "reasoning_content", "reasoning_text"];

fn is_reasoning_detail(detail: &Value) -> bool {
    let Some(object) = detail.as_object() else {
        return false;
    };
    let id_ok = matches!(
        object.get("id"),
        None | Some(Value::Null) | Some(Value::String(_))
    );
    let format_ok = matches!(object.get("format"), None | Some(Value::String(_)));
    let index_ok = matches!(object.get("index"), None | Some(Value::Number(_)));
    if !(id_ok && format_ok && index_ok) {
        return false;
    }
    match object.get("type").and_then(Value::as_str) {
        Some("reasoning.summary") => object.get("summary").is_some_and(Value::is_string),
        Some("reasoning.encrypted") => object.get("data").is_some_and(Value::is_string),
        Some("reasoning.text") => {
            object.get("text").is_some_and(Value::is_string)
                && matches!(
                    object.get("signature"),
                    None | Some(Value::Null) | Some(Value::String(_))
                )
        }
        _ => false,
    }
}

fn parse_reasoning_details(signature: Option<&str>) -> Option<Vec<Value>> {
    let parsed: Value = serde_json::from_str(signature.filter(|s| !s.is_empty())?).ok()?;
    let details = parsed.as_array()?;
    (!details.is_empty() && details.iter().all(is_reasoning_detail)).then(|| details.clone())
}

fn parse_legacy_encrypted_detail(signature: Option<&str>) -> Option<Value> {
    let parsed: Value = serde_json::from_str(signature.filter(|s| !s.is_empty())?).ok()?;
    let ok = is_reasoning_detail(&parsed)
        && parsed.get("type").and_then(Value::as_str) == Some("reasoning.encrypted")
        && non_empty_str(parsed.get("id")).is_some()
        && non_empty_str(parsed.get("data")).is_some();
    ok.then_some(parsed)
}

fn fill_missing_common_fields(target: &mut Map<String, Value>, source: &Map<String, Value>) {
    if target.get("id").is_none_or(Value::is_null)
        && let Some(id) = source.get("id")
    {
        target.insert("id".into(), id.clone());
    }
    if !truthy(target.get("format"))
        && let Some(format) = source.get("format")
    {
        target.insert("format".into(), format.clone());
    }
    if target.get("index").is_none_or(Value::is_null)
        && let Some(index) = source.get("index")
    {
        target.insert("index".into(), index.clone());
    }
}

fn append_reasoning_detail(details: &mut Vec<Value>, detail: &Value) {
    let kind = detail.get("type").and_then(Value::as_str);
    if let (Some(Value::Object(last)), Some(source)) = (details.last_mut(), detail.as_object()) {
        let last_kind = last.get("type").and_then(Value::as_str).map(str::to_owned);
        let merge_field = match (kind, last_kind.as_deref()) {
            (Some("reasoning.text"), Some("reasoning.text")) => Some("text"),
            (Some("reasoning.summary"), Some("reasoning.summary")) => Some("summary"),
            _ => None,
        };
        if let Some(field) = merge_field {
            let addition = source.get(field).and_then(Value::as_str).unwrap_or("");
            let mut merged = last
                .get(field)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            merged.push_str(addition);
            last.insert(field.into(), Value::String(merged));
            if field == "text"
                && !truthy(last.get("signature"))
                && let Some(signature) = source.get("signature")
            {
                last.insert("signature".into(), signature.clone());
            }
            fill_missing_common_fields(last, source);
            return;
        }
    }
    details.push(detail.clone());
}

fn cache_control(compat: &ResolvedCompletionsCompat, retention: CacheRetention) -> Option<Value> {
    if compat.cache_control_format.as_deref() != Some("anthropic")
        || retention == CacheRetention::None
    {
        return None;
    }
    let mut control = Map::new();
    control.insert("type".into(), json!("ephemeral"));
    if retention == CacheRetention::Long && compat.supports_long_cache_retention {
        control.insert("ttl".into(), json!("1h"));
    }
    Some(Value::Object(control))
}

fn add_cache_control_to_text_content(message: &mut Value, control: &Value) -> bool {
    let Some(object) = message.as_object_mut() else {
        return false;
    };
    match object.get_mut("content") {
        Some(Value::String(text)) => {
            if text.is_empty() {
                return false;
            }
            let text = std::mem::take(text);
            object.insert(
                "content".into(),
                json!([{ "type": "text", "text": text, "cache_control": control }]),
            );
            true
        }
        Some(Value::Array(parts)) => {
            for part in parts.iter_mut().rev() {
                if part.get("type").and_then(Value::as_str) == Some("text")
                    && let Some(part) = part.as_object_mut()
                {
                    part.insert("cache_control".into(), control.clone());
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

fn apply_anthropic_cache_control(
    messages: &mut [Value],
    tools: Option<&mut Vec<Value>>,
    control: &Value,
) {
    for message in messages.iter_mut() {
        let role = message.get("role").and_then(Value::as_str);
        if matches!(role, Some("system" | "developer")) {
            add_cache_control_to_text_content(message, control);
            break;
        }
    }
    if let Some(Value::Object(last)) = tools.and_then(|tools| tools.last_mut()) {
        last.insert("cache_control".into(), control.clone());
    }
    for message in messages.iter_mut().rev() {
        let role = message.get("role").and_then(Value::as_str);
        if matches!(role, Some("user" | "assistant" | "tool"))
            && add_cache_control_to_text_content(message, control)
        {
            return;
        }
    }
}

fn normalize_completions_tool_call_id(model: &Model, id: &str) -> String {
    if let Some(separator) = id.find('|') {
        let call_id = sanitize_id_chars(id.get(..separator).unwrap_or(""));
        let item_id = sanitize_id_chars(id.get(separator + 1..).unwrap_or(""));
        let combined = if item_id.is_empty() {
            call_id.clone()
        } else {
            format!("{call_id}_{item_id}")
        };
        if combined.encode_utf16().count() <= 40 {
            return combined;
        }
        let hash: String = short_hash(id).chars().take(8).collect();
        let prefix = truncate_units(&call_id, (40 - hash.len() - 1).max(1));
        return format!("{prefix}_{hash}");
    }
    if model.provider == "openai" && id.encode_utf16().count() > 40 {
        return truncate_units(id, 40);
    }
    id.to_owned()
}

fn image_url(mime_type: &str, data: &str) -> Value {
    json!({ "type": "image_url", "image_url": { "url": format!("data:{mime_type};base64,{data}") } })
}

/// Pi's `convertMessages`: the transcript as Chat Completions messages.
pub fn convert_messages(
    model: &Model,
    context: &TranscriptContext,
    compat: &ResolvedCompletionsCompat,
) -> Result<Vec<Value>, String> {
    let normalized = resolve_transcript(context, compat.supports_mid_convo_system_messages);
    let normalize =
        |id: &str, _: &Model, _: &AssistantMessage| normalize_completions_tool_call_id(model, id);
    let transformed = transform_messages(normalized.messages(), model, Some(&normalize));
    let (_, anchors_additions) = resolve_transcript_tools(
        normalized.messages(),
        compat.supports_mid_convo_system_messages && compat.supports_mid_convo_tool_additions,
    );
    let instruction_role = if model.reasoning && compat.supports_developer_role {
        "developer"
    } else {
        "system"
    };
    let mut params: Vec<Value> = Vec::new();
    let mut last_role: Option<&str> = None;
    let mut index = 0;
    while let Some(message) = transformed.get(index) {
        if compat.requires_assistant_after_tool_result
            && last_role == Some("toolResult")
            && matches!(message, Message::User(_))
        {
            params.push(
                json!({ "role": "assistant", "content": "I have processed the tool results." }),
            );
        }
        match message {
            Message::System(system) => {
                let added = if index > 0 && anchors_additions {
                    system.tools_added.clone().unwrap_or_default()
                } else {
                    Vec::new()
                };
                if !added.is_empty() {
                    params
                        .push(json!({ "role": "system", "tools": convert_tools(&added, compat)? }));
                }
                let text = if index == 0 {
                    get_system_message_text(system)
                } else {
                    render_system_message_update(system)
                };
                if !text.is_empty() {
                    params.push(json!({ "role": instruction_role, "content": text }));
                }
            }
            Message::User(user) => match &user.content {
                UserContent::Text(text) => params.push(json!({ "role": "user", "content": text })),
                UserContent::Blocks(blocks) => {
                    let content: Vec<Value> = blocks
                        .iter()
                        .filter(|block| !matches!(block, UserContentBlock::Text(text) if text.text.is_empty()))
                        .map(|block| match block {
                            UserContentBlock::Text(text) => json!({ "type": "text", "text": text.text }),
                            UserContentBlock::Image(image) => image_url(&image.mime_type, &image.data),
                        })
                        .collect();
                    if content.is_empty() {
                        index += 1;
                        continue;
                    }
                    params.push(json!({ "role": "user", "content": content }));
                }
            },
            Message::Assistant(assistant) => {
                if let Some(converted) = convert_assistant(model, compat, assistant) {
                    params.push(converted);
                } else {
                    index += 1;
                    continue;
                }
            }
            Message::ToolResult(_) => {
                let mut images: Vec<Value> = Vec::new();
                let mut next = index;
                while let Some(Message::ToolResult(result)) = transformed.get(next) {
                    let text = result
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            UserContentBlock::Text(text) => Some(text.text.as_str()),
                            UserContentBlock::Image(_) => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let has_images = result
                        .content
                        .iter()
                        .any(|block| matches!(block, UserContentBlock::Image(_)));
                    let text = if !text.is_empty() {
                        text
                    } else if has_images {
                        "(see attached image)".to_owned()
                    } else {
                        "(no tool output)".to_owned()
                    };
                    let mut tool_message = json!({ "role": "tool", "content": text, "tool_call_id": result.tool_call_id });
                    if compat.requires_tool_result_name
                        && !result.tool_name.is_empty()
                        && let Some(object) = tool_message.as_object_mut()
                    {
                        object.insert("name".into(), json!(result.tool_name));
                    }
                    params.push(tool_message);
                    if has_images && model.accepts_images() {
                        for block in &result.content {
                            if let UserContentBlock::Image(image) = block {
                                images.push(image_url(&image.mime_type, &image.data));
                            }
                        }
                    }
                    next += 1;
                }
                if images.is_empty() {
                    last_role = Some("toolResult");
                } else {
                    if compat.requires_assistant_after_tool_result {
                        params.push(json!({ "role": "assistant", "content": "I have processed the tool results." }));
                    }
                    let mut content = vec![
                        json!({ "type": "text", "text": "Attached image(s) from tool result:" }),
                    ];
                    content.extend(images);
                    params.push(json!({ "role": "user", "content": content }));
                    last_role = Some("user");
                }
                index = next;
                continue;
            }
        }
        last_role = Some(message.role());
        index += 1;
    }
    Ok(params)
}

fn convert_assistant(
    model: &Model,
    compat: &ResolvedCompletionsCompat,
    assistant: &AssistantMessage,
) -> Option<Value> {
    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert(
        "content".into(),
        if compat.requires_assistant_after_tool_result {
            json!("")
        } else {
            Value::Null
        },
    );
    let text_parts: Vec<&str> = assistant
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::Text(text) if !text.text.trim().is_empty() => {
                Some(text.text.as_str())
            }
            _ => None,
        })
        .collect();
    let assistant_text = text_parts.concat();
    let thinking: Vec<&ThinkingContent> = assistant
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::Thinking(thinking) => Some(thinking),
            _ => None,
        })
        .collect();
    let tool_calls: Vec<&ToolCall> = assistant.tool_calls().collect();
    let signed = thinking
        .iter()
        .find_map(|block| parse_reasoning_details(block.thinking_signature.as_deref()));
    let legacy: Vec<Value> = tool_calls
        .iter()
        .filter_map(|call| parse_legacy_encrypted_detail(call.thought_signature.as_deref()))
        .collect();
    let preserved = signed.or_else(|| (!legacy.is_empty()).then_some(legacy));
    let non_empty_thinking: Vec<&&ThinkingContent> = thinking
        .iter()
        .filter(|block| !block.thinking.trim().is_empty())
        .collect();
    if !non_empty_thinking.is_empty() {
        if compat.requires_thinking_as_text {
            let thinking_text = non_empty_thinking
                .iter()
                .map(|block| block.thinking.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            let mut content = vec![json!({ "type": "text", "text": thinking_text })];
            content.extend(
                text_parts
                    .iter()
                    .map(|text| json!({ "type": "text", "text": text })),
            );
            message.insert("content".into(), Value::Array(content));
        } else {
            if !assistant_text.is_empty() {
                message.insert("content".into(), json!(assistant_text));
            }
            if preserved.is_none() {
                let mut signature = non_empty_thinking
                    .first()
                    .and_then(|block| block.thinking_signature.clone());
                if model.provider == "opencode-go" && signature.as_deref() == Some("reasoning") {
                    signature = Some("reasoning_content".to_owned());
                }
                if let Some(field) =
                    signature.filter(|field| REASONING_FIELDS.contains(&field.as_str()))
                {
                    let joined = non_empty_thinking
                        .iter()
                        .map(|block| block.thinking.as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    message.insert(field, json!(joined));
                }
            }
        }
    } else if !assistant_text.is_empty() {
        message.insert("content".into(), json!(assistant_text));
    }
    if !tool_calls.is_empty() {
        let calls: Vec<Value> = tool_calls
            .iter()
            .map(|call| {
                json!({
                    "id": call.id,
                    "type": "function",
                    "function": { "name": call.name, "arguments": serde_json::to_string(&call.arguments).unwrap_or_default() }
                })
            })
            .collect();
        message.insert("tool_calls".into(), Value::Array(calls));
    }
    if let Some(details) = preserved {
        message.insert("reasoning_details".into(), Value::Array(details));
    }
    if compat.requires_reasoning_content_on_assistant_messages
        && model.reasoning
        && !message.contains_key("reasoning_content")
    {
        message.insert("reasoning_content".into(), json!(""));
    }
    let has_content = match message.get("content") {
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(parts)) => !parts.is_empty(),
        _ => false,
    };
    if !has_content && !message.contains_key("tool_calls") {
        return None;
    }
    Some(Value::Object(message))
}

fn convert_tools(tools: &[Tool], compat: &ResolvedCompletionsCompat) -> Result<Vec<Value>, String> {
    tools
        .iter()
        .map(|tool| {
            let strict =
                resolve_json_schema_strict_sampling(tool, compat.supports_strict_mode, None)?;
            let mut function = Map::new();
            function.insert("name".into(), json!(tool.name));
            function.insert("description".into(), json!(tool.description));
            function.insert(
                "parameters".into(),
                get_json_schema_tool_parameters(tool, strict)?,
            );
            if compat.supports_strict_mode {
                function.insert("strict".into(), json!(strict.unwrap_or(false)));
            }
            Ok(json!({ "type": "function", "function": function }))
        })
        .collect()
}

fn clamp_prompt_cache_key(key: Option<&str>) -> Option<String> {
    key.map(|key| key.chars().take(64).collect())
}

fn thinking_value(model: &Model, level: ThinkingLevel) -> Option<Option<&str>> {
    model.thinking_level_value(ModelThinkingLevel::from(level))
}

/// `thinkingLevelMap?.[level] ?? level`.
fn mapped_or_level(model: &Model, level: ThinkingLevel) -> String {
    thinking_value(model, level)
        .flatten()
        .map_or_else(|| level.as_str().to_owned(), str::to_owned)
}

fn resolve_chat_template_value(
    model: &Model,
    options: &OpenAICompletionsOptions,
    value: &Value,
    thinking_budget: Option<u64>,
) -> Option<Value> {
    let Some(object) = value.as_object() else {
        return Some(value.clone());
    };
    let effort = options.reasoning_effort;
    if effort.is_none() && truthy(object.get("omitWhenOff")) {
        return None;
    }
    match object.get("$var").and_then(Value::as_str) {
        Some("thinking.enabled") => return Some(json!(effort.is_some())),
        Some("thinking.budget") => return thinking_budget.map(|budget| json!(budget)),
        _ => {}
    }
    let mapped = match effort {
        Some(level) => thinking_value(model, level),
        None => model.thinking_level_value(ModelThinkingLevel::Off),
    };
    match mapped {
        None => effort.map(|level| json!(level.as_str())),
        Some(Some(text)) => Some(json!(text)),
        Some(None) => None,
    }
}

fn build_chat_template_values(
    model: &Model,
    options: &OpenAICompletionsOptions,
    values: &Map<String, Value>,
    thinking_budget: Option<u64>,
) -> Option<Value> {
    let mut resolved = Map::new();
    for (key, value) in values {
        if let Some(value) = resolve_chat_template_value(model, options, value, thinking_budget) {
            resolved.insert(key.clone(), value);
        }
    }
    (!resolved.is_empty()).then_some(Value::Object(resolved))
}

/// Pi's `buildParams`: the request body.
pub fn build_params(
    model: &Model,
    context: &TranscriptContext,
    options: &OpenAICompletionsOptions,
    compat: &ResolvedCompletionsCompat,
    retention: CacheRetention,
) -> Result<Value, String> {
    let (request_tools, _) = resolve_transcript_tools(
        context.messages(),
        compat.supports_mid_convo_system_messages && compat.supports_mid_convo_tool_additions,
    );
    let mut messages = convert_messages(model, context, compat)?;
    let control = cache_control(compat, retention);
    let mut params = Map::new();
    params.insert("model".into(), json!(model.id));
    params.insert("messages".into(), Value::Null);
    params.insert("stream".into(), json!(true));
    let long = retention == CacheRetention::Long && compat.supports_long_cache_retention;
    if ((model.base_url.contains("api.openai.com") && retention != CacheRetention::None) || long)
        && let Some(key) = clamp_prompt_cache_key(options.base.session_id.as_deref())
    {
        params.insert("prompt_cache_key".into(), json!(key));
    }
    if long {
        params.insert("prompt_cache_retention".into(), json!("24h"));
    }
    if compat.supports_usage_in_streaming {
        params.insert("stream_options".into(), json!({ "include_usage": true }));
    }
    if compat.supports_store {
        params.insert("store".into(), json!(false));
    }
    if let Some(max_tokens) = options.base.max_tokens.filter(|max| *max > 0) {
        let field = match compat.max_tokens_field {
            MaxTokensField::MaxTokens => "max_tokens",
            MaxTokensField::MaxCompletionTokens => "max_completion_tokens",
        };
        params.insert(field.into(), json!(max_tokens));
    }
    if let Some(temperature) = options.base.temperature {
        params.insert("temperature".into(), json!(temperature));
    }
    let mut tools: Option<Vec<Value>> = None;
    if !request_tools.is_empty() {
        tools = Some(convert_tools(&request_tools, compat)?);
    } else if has_tool_history(context.messages()) {
        tools = Some(Vec::new());
    }
    if let Some(control) = &control {
        apply_anthropic_cache_control(&mut messages, tools.as_mut(), control);
    }
    params.insert("messages".into(), Value::Array(messages));
    if let Some(tools) = tools {
        let has_tools = !tools.is_empty();
        params.insert("tools".into(), Value::Array(tools));
        if has_tools && compat.zai_tool_stream {
            params.insert("tool_stream".into(), json!(true));
        }
    }
    if let Some(choice) = &options.tool_choice {
        params.insert("tool_choice".into(), choice.clone());
    }
    if let Some(priority) = compat.vllm_priority {
        params.insert("priority".into(), json!(priority));
    }
    let budget_field = compat.thinking_token_budget_field.or(compat
        .supports_thinking_token_budget
        .then_some(ThinkingTokenBudgetField::ThinkingTokenBudget));
    let thinking_budget = options
        .reasoning_effort
        .filter(|_| model.reasoning)
        .and_then(|effort| {
            let ceiling = count(params.get("max_tokens"))
                .or_else(|| count(params.get("max_completion_tokens")))
                .unwrap_or(model.max_tokens);
            let budget = clamp_thinking_budget_to_answer_room(
                thinking_budget_for_level(effort, options.thinking_budgets.as_ref()),
                ceiling,
            );
            (budget > 0).then_some(budget)
        });
    apply_thinking_format(&mut params, model, options, compat, thinking_budget);
    if let (Some(field), Some(budget)) = (budget_field, thinking_budget) {
        params.insert(field.as_str().into(), json!(budget));
    }
    if let Some(routing) = &compat.open_router_routing {
        params.insert("provider".into(), routing.clone());
    }
    if let Some(routing) = model
        .compat
        .as_ref()
        .and_then(|compat| compat.vercel_gateway_routing.as_ref())
    {
        let mut gateway = Map::new();
        for key in ["only", "order"] {
            if let Some(value) = routing.get(key).filter(|value| truthy(Some(value))) {
                gateway.insert(key.into(), value.clone());
            }
        }
        if !gateway.is_empty() {
            params.insert("providerOptions".into(), json!({ "gateway": gateway }));
        }
    }
    let level = options
        .reasoning_effort
        .map_or(ModelThinkingLevel::Off, ModelThinkingLevel::from);
    if let Some(sampling) =
        resolve_sampling_params(model, level, options.base.sampling_params.as_ref())
    {
        for (key, value) in sampling {
            params.insert(key, value);
        }
    }
    Ok(Value::Object(params))
}

fn apply_thinking_format(
    params: &mut Map<String, Value>,
    model: &Model,
    options: &OpenAICompletionsOptions,
    compat: &ResolvedCompletionsCompat,
    thinking_budget: Option<u64>,
) {
    let effort = options.reasoning_effort;
    let off = model.thinking_level_value(ModelThinkingLevel::Off);
    if !model.reasoning {
        return;
    }
    match compat.thinking_format {
        ThinkingFormat::Zai => {
            params.insert(
                "thinking".into(),
                if effort.is_some() {
                    json!({ "type": "enabled", "clear_thinking": false })
                } else {
                    json!({ "type": "disabled" })
                },
            );
            if let Some(level) = effort.filter(|_| compat.supports_reasoning_effort) {
                let value = match thinking_value(model, level) {
                    None => Some(level.as_str().to_owned()),
                    Some(mapped) => mapped.map(str::to_owned),
                };
                if let Some(value) = value {
                    params.insert("reasoning_effort".into(), json!(value));
                }
            }
        }
        ThinkingFormat::Qwen => {
            params.insert("enable_thinking".into(), json!(effort.is_some()));
            if let Some(level) = effort.filter(|_| compat.supports_reasoning_effort) {
                params.insert(
                    "reasoning_effort".into(),
                    json!(mapped_or_level(model, level)),
                );
            }
        }
        ThinkingFormat::QwenChatTemplate => {
            params.insert(
                "chat_template_kwargs".into(),
                json!({ "enable_thinking": effort.is_some(), "preserve_thinking": true }),
            );
        }
        ThinkingFormat::ChatTemplate => {
            if let Some(values) = build_chat_template_values(
                model,
                options,
                &compat.chat_template_kwargs,
                thinking_budget,
            ) {
                params.insert("chat_template_kwargs".into(), values);
            }
        }
        ThinkingFormat::Baseten => {
            if let Some(values) = build_chat_template_values(
                model,
                options,
                &compat.chat_template_args,
                thinking_budget,
            ) {
                params.insert("chat_template_args".into(), values);
            }
            if compat.supports_reasoning_effort {
                let mapped = match effort {
                    Some(level) => thinking_value(model, level),
                    None => off,
                };
                let value = match mapped {
                    None => effort.map(|level| level.as_str().to_owned()),
                    Some(mapped) => mapped.map(str::to_owned),
                };
                if let Some(value) = value {
                    params.insert("reasoning_effort".into(), json!(value));
                }
            }
        }
        ThinkingFormat::Deepseek => {
            if effort.is_some() {
                params.insert("thinking".into(), json!({ "type": "enabled" }));
            } else if off != Some(None) {
                params.insert("thinking".into(), json!({ "type": "disabled" }));
            }
            if let Some(level) = effort.filter(|_| compat.supports_reasoning_effort) {
                params.insert(
                    "reasoning_effort".into(),
                    json!(mapped_or_level(model, level)),
                );
            }
        }
        ThinkingFormat::Openrouter => {
            if let Some(level) = effort {
                params.insert(
                    "reasoning".into(),
                    json!({ "effort": mapped_or_level(model, level) }),
                );
            } else if off != Some(None) {
                params.insert(
                    "reasoning".into(),
                    json!({ "effort": off.flatten().unwrap_or("none") }),
                );
            }
        }
        ThinkingFormat::AntLing => {
            if let Some(Some(mapped)) = effort.and_then(|level| thinking_value(model, level)) {
                params.insert("reasoning".into(), json!({ "effort": mapped }));
            }
        }
        ThinkingFormat::Together => {
            params.insert("reasoning".into(), json!({ "enabled": effort.is_some() }));
            if let Some(level) = effort.filter(|_| compat.supports_reasoning_effort) {
                params.insert(
                    "reasoning_effort".into(),
                    json!(mapped_or_level(model, level)),
                );
            }
        }
        ThinkingFormat::StringThinking => {
            if let Some(level) = effort {
                params.insert("thinking".into(), json!(mapped_or_level(model, level)));
            } else if off != Some(None) {
                params.insert("thinking".into(), json!(off.flatten().unwrap_or("none")));
            }
        }
        ThinkingFormat::Openai => {
            if let Some(level) = effort {
                if compat.supports_reasoning_effort {
                    params.insert(
                        "reasoning_effort".into(),
                        json!(mapped_or_level(model, level)),
                    );
                }
            } else if compat.supports_reasoning_effort
                && let Some(Some(off)) = off
            {
                params.insert("reasoning_effort".into(), json!(off));
            }
        }
    }
}

fn parse_chunk_usage(raw: &Value, model: &Model) -> Usage {
    let prompt = count(raw.get("prompt_tokens")).unwrap_or(0);
    let details = raw.get("prompt_tokens_details");
    let cache_read = [
        details.and_then(|d| d.get("cached_tokens")),
        raw.get("prompt_cache_hit_tokens"),
        raw.get("cached_tokens"),
    ]
    .into_iter()
    .find(|value| value.is_some_and(|value| !value.is_null()))
    .and_then(count)
    .unwrap_or(0);
    let cache_write = count(details.and_then(|d| d.get("cache_write_tokens"))).unwrap_or(0);
    let input = prompt
        .saturating_sub(cache_read)
        .saturating_sub(cache_write);
    let output = count(raw.get("completion_tokens")).unwrap_or(0);
    let reasoning = count(
        raw.get("completion_tokens_details")
            .and_then(|d| d.get("reasoning_tokens")),
    )
    .unwrap_or(0);
    let mut usage = Usage {
        input,
        output,
        cache_read,
        cache_write,
        reasoning: Some(reasoning),
        ..Usage::default()
    };
    usage.total_tokens = usage.component_sum();
    calculate_cost(model, &mut usage);
    usage
}

fn map_stop_reason(reason: &str) -> (StopReason, Option<String>) {
    match reason {
        "stop" | "end" => (StopReason::Stop, None),
        "length" => (StopReason::Length, None),
        "function_call" | "tool_calls" => (StopReason::ToolUse, None),
        other => (
            StopReason::Error,
            Some(format!("Provider finish_reason: {other}")),
        ),
    }
}

#[derive(Debug, Default)]
struct ToolScratch {
    partial_args: String,
    stream_index: Option<i64>,
}

/// Assembles streamed chunks into an assistant message and its events.
pub struct CompletionsStreamProcessor {
    model: Model,
    /// The message being built.
    pub output: AssistantMessage,
    text_block: Option<usize>,
    thinking_block: Option<usize>,
    has_finish_reason: bool,
    tools_by_index: HashMap<i64, usize>,
    tools_by_id: HashMap<String, usize>,
    scratch: HashMap<usize, ToolScratch>,
    reasoning_details: Option<Vec<Value>>,
}

impl CompletionsStreamProcessor {
    /// A processor for `model` filling `output`.
    pub fn new(model: &Model, output: AssistantMessage) -> Self {
        Self {
            model: model.clone(),
            output,
            text_block: None,
            thinking_block: None,
            has_finish_reason: false,
            tools_by_index: HashMap::new(),
            tools_by_id: HashMap::new(),
            scratch: HashMap::new(),
            reasoning_details: None,
        }
    }

    fn emit(
        &self,
        sender: &AssistantMessageEventSender,
        make: impl FnOnce(AssistantMessage) -> AssistantMessageEvent,
    ) {
        sender.push(make(self.output.clone()));
    }

    fn ensure_text(&mut self, sender: &AssistantMessageEventSender) -> usize {
        if let Some(index) = self.text_block {
            return index;
        }
        self.output
            .content
            .push(AssistantContentBlock::Text(TextContent::new("")));
        let index = self.output.content.len() - 1;
        self.text_block = Some(index);
        self.emit(sender, |partial| AssistantMessageEvent::TextStart {
            content_index: index,
            partial,
        });
        index
    }

    fn ensure_thinking(&mut self, sender: &AssistantMessageEventSender, signature: &str) -> usize {
        if let Some(index) = self.thinking_block {
            return index;
        }
        self.output
            .content
            .push(AssistantContentBlock::Thinking(ThinkingContent {
                thinking: String::new(),
                thinking_signature: Some(signature.to_owned()),
                redacted: None,
            }));
        let index = self.output.content.len() - 1;
        self.thinking_block = Some(index);
        self.emit(sender, |partial| AssistantMessageEvent::ThinkingStart {
            content_index: index,
            partial,
        });
        index
    }

    fn ensure_tool_call(&mut self, sender: &AssistantMessageEventSender, delta: &Value) -> usize {
        let stream_index = delta.get("index").and_then(Value::as_i64);
        let id = non_empty_str(delta.get("id"));
        let name = delta
            .get("function")
            .and_then(|function| function.get("name"))
            .or_else(|| delta.get("custom").and_then(|custom| custom.get("name")))
            .and_then(Value::as_str)
            .unwrap_or("");
        let mut found = stream_index.and_then(|index| self.tools_by_index.get(&index).copied());
        if found.is_none() {
            found = id.and_then(|id| self.tools_by_id.get(id).copied());
        }
        let index = match found {
            Some(index) => index,
            None => {
                self.output
                    .content
                    .push(AssistantContentBlock::ToolCall(ToolCall {
                        id: id.unwrap_or("").to_owned(),
                        name: name.to_owned(),
                        ..ToolCall::default()
                    }));
                let index = self.output.content.len() - 1;
                self.scratch.insert(
                    index,
                    ToolScratch {
                        partial_args: String::new(),
                        stream_index,
                    },
                );
                if let Some(stream_index) = stream_index {
                    self.tools_by_index.insert(stream_index, index);
                }
                self.emit(sender, |partial| AssistantMessageEvent::ToolCallStart {
                    content_index: index,
                    partial,
                });
                index
            }
        };
        if let Some(stream_index) = stream_index
            && let Some(scratch) = self.scratch.get_mut(&index)
            && scratch.stream_index.is_none()
        {
            scratch.stream_index = Some(stream_index);
            self.tools_by_index.insert(stream_index, index);
        }
        if let Some(id) = id {
            self.tools_by_id.insert(id.to_owned(), index);
        }
        if let Some(AssistantContentBlock::ToolCall(call)) = self.output.content.get_mut(index) {
            if call.id.is_empty()
                && let Some(id) = id
            {
                call.id = id.to_owned();
            }
            if call.name.is_empty() && !name.is_empty() {
                call.name = name.to_owned();
            }
        }
        index
    }

    /// Applies one parsed chunk.
    pub fn handle_chunk(&mut self, chunk: &Value, sender: &AssistantMessageEventSender) {
        if !chunk.is_object() {
            return;
        }
        if self.output.response_id.as_deref().is_none_or(str::is_empty)
            && let Some(id) = non_empty_str(chunk.get("id"))
        {
            self.output.response_id = Some(id.to_owned());
        }
        if let Some(model) = non_empty_str(chunk.get("model"))
            && model != self.model.id
            && self
                .output
                .response_model
                .as_deref()
                .is_none_or(str::is_empty)
        {
            self.output.response_model = Some(model.to_owned());
        }
        let chunk_usage = chunk.get("usage").filter(|usage| truthy(Some(usage)));
        if let Some(usage) = chunk_usage {
            self.output.usage = parse_chunk_usage(usage, &self.model);
        }
        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            return;
        };
        if chunk_usage.is_none()
            && let Some(usage) = choice.get("usage").filter(|usage| truthy(Some(usage)))
        {
            self.output.usage = parse_chunk_usage(usage, &self.model);
        }
        if let Some(reason) = non_empty_str(choice.get("finish_reason")) {
            self.output.raw_stop_reason = Some(reason.to_owned());
            let (stop_reason, error) = map_stop_reason(reason);
            self.output.stop_reason = stop_reason;
            if let Some(error) = error {
                self.output.error_message = Some(error);
            }
            self.has_finish_reason = true;
        }
        let Some(delta) = choice.get("delta").filter(|delta| truthy(Some(delta))) else {
            return;
        };
        if let Some(text) = non_empty_str(delta.get("content")) {
            let index = self.ensure_text(sender);
            if let Some(AssistantContentBlock::Text(block)) = self.output.content.get_mut(index) {
                block.text.push_str(text);
            }
            self.emit(sender, |partial| AssistantMessageEvent::TextDelta {
                content_index: index,
                delta: text.to_owned(),
                partial,
            });
        }
        let reasoning = ["reasoning_content", "reasoning", "reasoning_text"]
            .into_iter()
            .find_map(|field| non_empty_str(delta.get(field)).map(|text| (field, text)));
        if let Some((field, text)) = reasoning {
            let signature = if self.model.provider == "opencode-go" && field == "reasoning" {
                "reasoning_content"
            } else {
                field
            };
            let index = self.ensure_thinking(sender, signature);
            if let Some(AssistantContentBlock::Thinking(block)) = self.output.content.get_mut(index)
            {
                block.thinking.push_str(text);
            }
            self.emit(sender, |partial| AssistantMessageEvent::ThinkingDelta {
                content_index: index,
                delta: text.to_owned(),
                partial,
            });
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let index = self.ensure_tool_call(sender, call);
                let mut text = String::new();
                if let Some(arguments) = non_empty_str(
                    call.get("function")
                        .and_then(|function| function.get("arguments")),
                ) {
                    text = arguments.to_owned();
                    if let Some(scratch) = self.scratch.get_mut(&index) {
                        scratch.partial_args.push_str(arguments);
                        let parsed = parse_streaming_json_object(Some(&scratch.partial_args));
                        if let Some(AssistantContentBlock::ToolCall(block)) =
                            self.output.content.get_mut(index)
                        {
                            block.arguments = parsed;
                        }
                    }
                }
                self.emit(sender, |partial| AssistantMessageEvent::ToolCallDelta {
                    content_index: index,
                    delta: text,
                    partial,
                });
            }
        }
        if let Some(details) = delta.get("reasoning_details").and_then(Value::as_array) {
            for detail in details.iter().filter(|detail| is_reasoning_detail(detail)) {
                self.ensure_thinking(sender, "");
                append_reasoning_detail(
                    self.reasoning_details.get_or_insert_with(Vec::new),
                    detail,
                );
            }
        }
    }

    fn apply_reasoning_details(&mut self, index: usize) {
        if let Some(details) = &self.reasoning_details
            && let Some(AssistantContentBlock::Thinking(block)) = self.output.content.get_mut(index)
        {
            block.thinking_signature = serde_json::to_string(details).ok();
        }
    }

    /// Emits the end events of every block, in content order.
    pub fn finish_blocks(&mut self, sender: &AssistantMessageEventSender) {
        for index in 0..self.output.content.len() {
            match self.output.content.get(index) {
                Some(AssistantContentBlock::Text(text)) => {
                    let content = text.text.clone();
                    self.emit(sender, |partial| AssistantMessageEvent::TextEnd {
                        content_index: index,
                        content,
                        partial,
                    });
                }
                Some(AssistantContentBlock::Thinking(_)) => {
                    self.apply_reasoning_details(index);
                    let content = match self.output.content.get(index) {
                        Some(AssistantContentBlock::Thinking(thinking)) => {
                            thinking.thinking.clone()
                        }
                        _ => String::new(),
                    };
                    self.emit(sender, |partial| AssistantMessageEvent::ThinkingEnd {
                        content_index: index,
                        content,
                        partial,
                    });
                }
                Some(AssistantContentBlock::ToolCall(_)) => {
                    let partial_args = self
                        .scratch
                        .remove(&index)
                        .map(|scratch| scratch.partial_args);
                    let arguments = parse_streaming_json_object(partial_args.as_deref());
                    let tool_call = match self.output.content.get_mut(index) {
                        Some(AssistantContentBlock::ToolCall(call)) => {
                            call.arguments = arguments;
                            call.clone()
                        }
                        _ => continue,
                    };
                    self.emit(sender, |partial| AssistantMessageEvent::ToolCallEnd {
                        content_index: index,
                        tool_call,
                        partial,
                    });
                }
                None => {}
            }
        }
    }

    /// Checks the final state after the stream ended.
    pub fn complete(&mut self, supports_finish_reason: bool) -> Result<(), ProviderError> {
        if self.output.stop_reason == StopReason::Aborted {
            return Err(ProviderError::aborted());
        }
        if !self.has_finish_reason && !supports_finish_reason {
            self.output.stop_reason = if self.output.tool_calls().next().is_some() {
                StopReason::ToolUse
            } else {
                StopReason::Stop
            };
        }
        if self.output.stop_reason == StopReason::Error {
            return Err(ProviderError::other(
                self.output
                    .error_message
                    .clone()
                    .filter(|error| !error.is_empty())
                    .unwrap_or_else(|| "Provider returned an error stop reason".to_owned()),
            ));
        }
        if (supports_finish_reason && !self.has_finish_reason)
            || self.output.stop_reason == StopReason::Pending
        {
            return Err(ProviderError::other("Stream ended without finish_reason"));
        }
        Ok(())
    }

    /// Marks the message failed after `error`, as Pi's catch block does.
    pub fn fail(&mut self, error: &ProviderError, aborted: bool) {
        for index in 0..self.output.content.len() {
            if matches!(
                self.output.content.get(index),
                Some(AssistantContentBlock::Thinking(_))
            ) {
                self.apply_reasoning_details(index);
            }
        }
        self.output.stop_reason = if aborted {
            StopReason::Aborted
        } else {
            StopReason::Error
        };
        let mut message = format_provider_error(&normalize_provider_error(error), None);
        if let Some(raw) = error
            .error
            .as_ref()
            .and_then(|body| body.get("metadata"))
            .and_then(|metadata| metadata.get("raw"))
            && truthy(Some(raw))
        {
            let raw = match raw {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            if !message.contains(&raw) {
                message.push('\n');
                message.push_str(&raw);
            }
        }
        self.output.error_message = Some(message);
    }
}

/// Parses one OpenAI SSE event as the SDK's stream does, with `JSON.parse`
/// strictness: `None` stops at `[DONE]`, an `error` event or payload fails,
/// `thread.*` events are skipped, and malformed JSON fails.
pub(crate) fn parse_openai_sse(
    event: &crate::utils::sse::ServerSentEvent,
) -> Result<Option<Option<Value>>, ProviderError> {
    if event.data == "[DONE]" {
        return Ok(None);
    }
    if event
        .event
        .as_deref()
        .is_some_and(|name| name.starts_with("thread."))
    {
        return Ok(Some(None));
    }
    let data = parse_json_strict(&event.data).map_err(|_| {
        ProviderError::other("Error reading response: malformed server-sent event JSON.")
    })?;
    if event.event.as_deref() == Some("error") {
        let error = data
            .get("error")
            .filter(|error| !error.is_null())
            .cloned()
            .unwrap_or_else(|| data.clone());
        return Err(ProviderError::stream_payload(error));
    }
    if let Some(error) = data.get("error").filter(|error| truthy(Some(error))) {
        return Err(ProviderError::stream_payload(error.clone()));
    }
    Ok(Some(Some(data)))
}

/// The request headers: SDK defaults, auth, then the model's and caller's.
pub(crate) fn openai_headers(
    model: &Model,
    api_key: &str,
    affinity: ProviderHeaders,
    caller: Option<&ProviderHeaders>,
) -> Vec<(String, String)> {
    let base: ProviderHeaders = vec![
        ("Accept".into(), Some("application/json".into())),
        ("Content-Type".into(), Some("application/json".into())),
        ("Authorization".into(), Some(format!("Bearer {api_key}"))),
    ];
    let mut defaults: ProviderHeaders = vec![("User-Agent".into(), Some(user_agent()))];
    if let Some(headers) = &model.headers {
        defaults.extend(header_layer(headers));
    }
    defaults.extend(affinity);
    let empty = Vec::new();
    merge_headers(&[&base, &defaults, caller.unwrap_or(&empty)])
}

async fn run(
    sender: AssistantMessageEventSender,
    model: Model,
    context: TranscriptContext,
    options: OpenAICompletionsOptions,
) {
    let compat = get_compat(&model);
    let context = resolve_transcript(&context, compat.supports_mid_convo_system_messages);
    let mut processor = CompletionsStreamProcessor::new(&model, AssistantMessage::pending(&model));
    let result = run_inner(&sender, &model, &context, &options, &compat, &mut processor).await;
    match result {
        Ok(()) => {
            let message = processor.output.clone();
            sender.finish(message);
        }
        Err(error) => {
            let aborted = is_aborted(options.base.signal.as_ref());
            processor.fail(&error, aborted);
            sender.finish(processor.output.clone());
        }
    }
}

async fn run_inner(
    sender: &AssistantMessageEventSender,
    model: &Model,
    context: &TranscriptContext,
    options: &OpenAICompletionsOptions,
    compat: &ResolvedCompletionsCompat,
    processor: &mut CompletionsStreamProcessor,
) -> Result<(), ProviderError> {
    let api_key = client_api_key(model, &options.base).map_err(ProviderError::other)?;
    let retention = resolve_cache_retention(&options.base);
    let session_id = options
        .base
        .session_id
        .as_deref()
        .filter(|_| retention != CacheRetention::None);
    let mut affinity: ProviderHeaders = Vec::new();
    if let Some(session_id) = session_id.filter(|_| compat.send_session_affinity_headers) {
        if compat.session_affinity_format == SessionAffinityFormat::Openrouter {
            affinity.push(("x-session-id".into(), Some(session_id.to_owned())));
        } else {
            if compat.session_affinity_format == SessionAffinityFormat::Openai {
                affinity.push(("session_id".into(), Some(session_id.to_owned())));
            }
            affinity.push(("x-client-request-id".into(), Some(session_id.to_owned())));
            affinity.push(("x-session-affinity".into(), Some(session_id.to_owned())));
        }
    }
    let mut params =
        build_params(model, context, options, compat, retention).map_err(ProviderError::other)?;
    if let Some(on_payload) = &options.base.on_payload
        && let Some(next) = on_payload(&params, model)
    {
        params = next;
    }
    let request = HttpRequest {
        url: join_url(&model.base_url, "/chat/completions"),
        headers: openai_headers(model, &api_key, affinity, options.base.headers.as_ref()),
        body: serde_json::to_vec(&params)
            .map_err(|error| ProviderError::other(error.to_string()))?,
        error_shape: SdkErrorShape::OpenAI,
    };
    let mut response = send(&request, &options.base).await?;
    if let Some(on_response) = &options.base.on_response {
        on_response(&response.info, model);
    }
    sender.push(AssistantMessageEvent::Start {
        partial: processor.output.clone(),
    });
    'read: while let Some(events) = response.next_events().await? {
        for event in events {
            let Some(chunk) = parse_openai_sse(&event)? else {
                break 'read;
            };
            let Some(chunk) = chunk else { continue };
            if let Some(observer) = &options.base.on_provider_stream_event {
                observer(&chunk, model);
            }
            processor.handle_chunk(&chunk, sender);
        }
        if sender.is_closed() {
            return Err(ProviderError::aborted());
        }
    }
    processor.finish_blocks(sender);
    if is_aborted(options.base.signal.as_ref()) {
        return Err(ProviderError::aborted());
    }
    processor.complete(compat.supports_finish_reason)
}

/// Streams a request to an OpenAI-compatible Chat Completions endpoint.
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: OpenAICompletionsOptions,
) -> AssistantMessageEventStream {
    let model = model.clone();
    let context = context.clone();
    spawn_stream(&model.clone(), move |sender| {
        run(sender, model, context, options)
    })
}

/// Maps simple options to completions options and streams.
pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> AssistantMessageEventStream {
    if let Err(error) = client_api_key(model, &options.base) {
        let failed = error_message(model, &error);
        return spawn_stream(model, move |sender| async move { sender.finish(failed) });
    }
    let base = build_base_options(model, context, &options);
    let effort = options
        .reasoning
        .map(|level| clamp_thinking_level(model, ModelThinkingLevel::from(level)))
        .and_then(ModelThinkingLevel::level);
    stream(
        model,
        context,
        OpenAICompletionsOptions {
            base,
            tool_choice: options.tool_choice.map(|choice| json!(choice.as_str())),
            reasoning_effort: effort,
            thinking_budgets: options.thinking_budgets,
        },
    )
}

/// The built-in `openai-completions` API.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenAICompletionsApi;

impl ApiProvider for OpenAICompletionsApi {
    fn api(&self) -> &str {
        "openai-completions"
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
            OpenAICompletionsOptions {
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
