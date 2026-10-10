//! The OpenAI Responses protocol (`openai-responses`).
//!
//! Ported from Pi `packages/ai/src/api/openai-responses.ts` (v1.1.0):
//! compatibility defaults, request building, prompt-cache fields, reasoning
//! effort, service-tier pricing, and the stream lifecycle. The request goes to
//! `{baseUrl}/responses` as the `openai` SDK sends it. Not ported: GitHub
//! Copilot headers and grammar, additional-tools, and tool-search requests.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use crate::api::openai_completions::{openai_headers, parse_openai_sse};
use crate::api::openai_responses_shared::{
    ConvertResponsesMessagesOptions, ResponsesStreamProcessor, convert_responses_messages,
    convert_responses_tools,
};
use crate::api::simple_options::{build_base_options, resolve_sampling_params};
use crate::api::{error_message, spawn_stream};
use crate::http::{HttpRequest, SdkErrorShape, join_url, send};
use crate::models::clamp_thinking_level;
use crate::options::{
    ProviderHeaders, SimpleStreamOptions, StreamOptions, has_header, resolve_cache_retention,
};
use crate::stream::ApiProvider;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, CacheRetention, Model, ModelThinkingLevel,
    SessionAffinityFormat, StopReason, ThinkingLevel, TranscriptContext, Usage,
};
use crate::utils::abort::is_aborted;
use crate::utils::error_body::{ProviderError, format_provider_error, normalize_provider_error};
use crate::utils::event_stream::{AssistantMessageEventSender, AssistantMessageEventStream};
use crate::utils::transcript::{get_current_tools, resolve_transcript};

const OPENAI_TOOL_CALL_PROVIDERS: [&str; 3] = ["openai", "openai-codex", "opencode"];
/// OpenAI Responses rejects `max_output_tokens` below 16.
const MIN_OUTPUT_TOKENS: u64 = 16;
const CHATGPT_USAGE_URL: &str = "https://chatgpt.com/settings/usage";

/// Options of the Responses protocol.
#[derive(Debug, Clone, Default)]
pub struct OpenAIResponsesOptions {
    /// The shared options.
    pub base: StreamOptions,
    /// The reasoning effort; `None` is off.
    pub reasoning_effort: Option<ThinkingLevel>,
    /// `auto`, `detailed`, or `concise`.
    pub reasoning_summary: Option<String>,
    /// `service_tier`, such as `flex` or `priority`.
    pub service_tier: Option<String>,
    /// `tool_choice`, sent as given.
    pub tool_choice: Option<Value>,
}

/// Compatibility settings after defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct ResolvedResponsesCompat {
    pub supports_developer_role: bool,
    pub supports_mid_convo_system_messages: bool,
    pub session_affinity_format: SessionAffinityFormat,
    pub supports_long_cache_retention: bool,
    pub supports_strict_mode: bool,
    pub supports_explicit_prompt_cache_mode: bool,
    pub supports_max_output_tokens: bool,
}

/// Pi's `getCompat` for Responses.
pub fn get_compat(model: &Model) -> ResolvedResponsesCompat {
    let compat = model.compat.clone().unwrap_or_default();
    let detected_affinity =
        if model.provider == "openrouter" || model.base_url.contains("openrouter.ai") {
            SessionAffinityFormat::Openrouter
        } else {
            SessionAffinityFormat::Openai
        };
    ResolvedResponsesCompat {
        supports_developer_role: compat.supports_developer_role.unwrap_or(true),
        supports_mid_convo_system_messages: compat
            .supports_mid_convo_system_messages
            .unwrap_or(false),
        session_affinity_format: compat.session_affinity_format.unwrap_or(detected_affinity),
        supports_long_cache_retention: compat.supports_long_cache_retention.unwrap_or(true),
        supports_strict_mode: compat.supports_strict_mode.unwrap_or(false),
        supports_explicit_prompt_cache_mode: compat
            .supports_explicit_prompt_cache_mode
            .unwrap_or(false),
        supports_max_output_tokens: compat.supports_max_output_tokens.unwrap_or(true),
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

/// A non-`sk-` credential sent directly to OpenAI is a ChatGPT sign-in token.
fn is_chatgpt_sign_in(model: &Model, api_key: Option<&str>) -> bool {
    model.provider == "openai"
        && model.base_url == "https://api.openai.com/v1"
        && api_key.is_some_and(|key| !key.starts_with("sk-"))
}

/// Pi's `buildParams` for Responses.
pub fn build_params(
    model: &Model,
    context: &TranscriptContext,
    options: &OpenAIResponsesOptions,
    compat: &ResolvedResponsesCompat,
) -> Result<Value, String> {
    let request_tools = get_current_tools(context.messages());
    let allowed: BTreeSet<&str> = OPENAI_TOOL_CALL_PROVIDERS.into_iter().collect();
    let input = convert_responses_messages(
        model,
        context,
        &allowed,
        &ConvertResponsesMessagesOptions {
            include_system_prompt: None,
            supports_mid_convo_system_messages: compat.supports_mid_convo_system_messages,
        },
    )?;
    let retention = resolve_cache_retention(&options.base);
    let omit_unsupported = is_chatgpt_sign_in(model, options.base.api_key.as_deref());
    let mut params = Map::new();
    params.insert("model".into(), json!(model.id));
    params.insert("input".into(), Value::Array(input));
    params.insert("stream".into(), json!(true));
    if retention != CacheRetention::None
        && let Some(session_id) = &options.base.session_id
    {
        params.insert(
            "prompt_cache_key".into(),
            json!(session_id.chars().take(64).collect::<String>()),
        );
    }
    if !omit_unsupported {
        if retention == CacheRetention::Long
            && compat.supports_long_cache_retention
            && !compat.supports_explicit_prompt_cache_mode
        {
            params.insert("prompt_cache_retention".into(), json!("24h"));
        }
        if compat.supports_explicit_prompt_cache_mode {
            if retention == CacheRetention::None {
                params.insert("prompt_cache_options".into(), json!({ "mode": "explicit" }));
            } else if retention == CacheRetention::Long && compat.supports_long_cache_retention {
                params.insert("prompt_cache_options".into(), json!({ "ttl": "30m" }));
            }
        }
    }
    params.insert("store".into(), json!(false));
    if let Some(max_tokens) = options.base.max_tokens.filter(|max| *max > 0)
        && compat.supports_max_output_tokens
        && !omit_unsupported
    {
        params.insert(
            "max_output_tokens".into(),
            json!(max_tokens.max(MIN_OUTPUT_TOKENS)),
        );
    }
    if let Some(temperature) = options.base.temperature.filter(|_| !omit_unsupported) {
        params.insert("temperature".into(), json!(temperature));
    }
    if let Some(tier) = &options.service_tier {
        params.insert("service_tier".into(), json!(tier));
    }
    if !request_tools.is_empty() {
        params.insert(
            "tools".into(),
            Value::Array(convert_responses_tools(
                &request_tools,
                compat.supports_strict_mode,
            )?),
        );
    }
    if let Some(choice) = &options.tool_choice {
        params.insert("tool_choice".into(), choice.clone());
    }
    let summary = options
        .reasoning_summary
        .as_deref()
        .filter(|summary| !summary.is_empty());
    let effort = options
        .reasoning_effort
        .or(summary.map(|_| ThinkingLevel::Medium));
    if model.reasoning {
        if let Some(effort) = effort {
            let value = match options.reasoning_effort {
                Some(level) => model
                    .thinking_level_value(ModelThinkingLevel::from(level))
                    .flatten()
                    .unwrap_or(level.as_str())
                    .to_owned(),
                None => effort.as_str().to_owned(),
            };
            params.insert(
                "reasoning".into(),
                json!({ "effort": value, "summary": summary.unwrap_or("auto") }),
            );
            params.insert("include".into(), json!(["reasoning.encrypted_content"]));
        } else if model.provider != "github-copilot"
            && model.thinking_level_value(ModelThinkingLevel::Off) != Some(None)
        {
            let off = model
                .thinking_level_value(ModelThinkingLevel::Off)
                .flatten()
                .unwrap_or("none");
            params.insert("reasoning".into(), json!({ "effort": off }));
        }
        if model.provider == "xai" {
            params.insert("include".into(), json!(["reasoning.encrypted_content"]));
        }
    }
    let level = effort.map_or(ModelThinkingLevel::Off, ModelThinkingLevel::from);
    if let Some(sampling) =
        resolve_sampling_params(model, level, options.base.sampling_params.as_ref())
    {
        for (key, value) in sampling {
            params.insert(key, value);
        }
    }
    Ok(Value::Object(params))
}

fn service_tier_multiplier(model_id: &str, tier: Option<&str>) -> f64 {
    match tier {
        Some("flex") => 0.5,
        Some("priority" | "fast") => {
            if model_id == "gpt-5.5" {
                2.5
            } else {
                2.0
            }
        }
        _ => 1.0,
    }
}

/// Pi's `applyServiceTierPricing`.
pub fn apply_service_tier_pricing(usage: &mut Usage, tier: Option<&str>, model_id: &str) {
    let multiplier = service_tier_multiplier(model_id, tier);
    if multiplier == 1.0 {
        return;
    }
    let cost = &mut usage.cost;
    cost.input *= multiplier;
    cost.output *= multiplier;
    cost.cache_read *= multiplier;
    cost.cache_write *= multiplier;
    cost.total = cost.input + cost.output + cost.cache_read + cost.cache_write;
}

async fn run(
    sender: AssistantMessageEventSender,
    model: Model,
    context: TranscriptContext,
    options: OpenAIResponsesOptions,
) {
    let compat = get_compat(&model);
    let context = resolve_transcript(&context, compat.supports_mid_convo_system_messages);
    let model_id = model.id.clone();
    let pricing = Box::new(move |usage: &mut Usage, tier: Option<&str>| {
        apply_service_tier_pricing(usage, tier, &model_id)
    });
    let mut processor = ResponsesStreamProcessor::new(
        &model,
        AssistantMessage::pending(&model),
        options.service_tier.clone(),
        Some(pricing),
    );
    match run_inner(&sender, &model, &context, &options, &compat, &mut processor).await {
        Ok(()) => sender.finish(processor.output.clone()),
        Err(error) => {
            let mut output = processor.output.clone();
            output.stop_reason = if is_aborted(options.base.signal.as_ref()) {
                StopReason::Aborted
            } else {
                StopReason::Error
            };
            let prefix = format!(
                "{} API error",
                if model.provider == "openai" {
                    "OpenAI"
                } else {
                    &model.provider
                }
            );
            let message = format_provider_error(&normalize_provider_error(&error), Some(&prefix));
            output.error_message = Some(
                if message.contains("subscription_sharing_usage_limit_exceeded") {
                    format!("{message}\nCheck your ChatGPT usage: {CHATGPT_USAGE_URL}")
                } else {
                    message
                },
            );
            sender.finish(output);
        }
    }
}

async fn run_inner(
    sender: &AssistantMessageEventSender,
    model: &Model,
    context: &TranscriptContext,
    options: &OpenAIResponsesOptions,
    compat: &ResolvedResponsesCompat,
    processor: &mut ResponsesStreamProcessor,
) -> Result<(), ProviderError> {
    let api_key = client_api_key(model, &options.base).map_err(ProviderError::other)?;
    let retention = resolve_cache_retention(&options.base);
    let mut affinity: ProviderHeaders = Vec::new();
    if let Some(session_id) = options
        .base
        .session_id
        .as_deref()
        .filter(|_| retention != CacheRetention::None)
    {
        if compat.session_affinity_format == SessionAffinityFormat::Openrouter {
            affinity.push(("x-session-id".into(), Some(session_id.to_owned())));
        } else {
            if compat.session_affinity_format == SessionAffinityFormat::Openai {
                affinity.push(("session_id".into(), Some(session_id.to_owned())));
            }
            affinity.push(("x-client-request-id".into(), Some(session_id.to_owned())));
        }
    }
    let mut params = build_params(model, context, options, compat).map_err(ProviderError::other)?;
    if let Some(on_payload) = &options.base.on_payload
        && let Some(next) = on_payload(&params, model)
    {
        params = next;
    }
    let request = HttpRequest {
        url: join_url(&model.base_url, "/responses"),
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
            let Some(parsed) = parse_openai_sse(&event)? else {
                break 'read;
            };
            let Some(parsed) = parsed else { continue };
            if let Some(observer) = &options.base.on_provider_stream_event {
                observer(&parsed, model);
            }
            processor
                .handle_event(&parsed, sender)
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
            "OpenAI Responses stream ended without a stop reason",
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

/// Streams a request to an OpenAI Responses endpoint.
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: OpenAIResponsesOptions,
) -> AssistantMessageEventStream {
    let model = model.clone();
    let context = context.clone();
    spawn_stream(&model.clone(), move |sender| {
        run(sender, model, context, options)
    })
}

/// Maps simple options to Responses options and streams.
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
        OpenAIResponsesOptions {
            base,
            reasoning_effort: effort,
            tool_choice: options.tool_choice.map(|choice| json!(choice.as_str())),
            ..Default::default()
        },
    )
}

/// The built-in `openai-responses` API.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenAIResponsesApi;

impl ApiProvider for OpenAIResponsesApi {
    fn api(&self) -> &str {
        "openai-responses"
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
            OpenAIResponsesOptions {
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
