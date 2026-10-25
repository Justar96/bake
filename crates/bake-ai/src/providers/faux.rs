//! A scripted provider for tests that need no network.
//!
//! Ported from Pi `packages/ai/src/providers/faux.ts` (v1.1.0). Queued
//! responses (messages or factories) are streamed back as deltas of a few
//! tokens each, usage is estimated from the serialized prompt at four
//! characters per token, and prompt caching is simulated per session id.
//! Text is chunked by Unicode scalar values rather than UTF-16 code units, so
//! a chunk never splits a surrogate pair. Deferred responses
//! (`fetchDeferred`, `cancelDeferred`) are not ported.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::api::spawn_stream;
use crate::options::{SimpleStreamOptions, StreamOptions};
use crate::stream::ApiProvider;
use crate::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, CacheRetention, InputModality,
    JsonObject, Message, Model, ModelCost, ModelCostRates, ProviderResponse, StopReason,
    TextContent, ThinkingContent, ToolCall, TranscriptContext, Usage, UserContent,
    UserContentBlock,
};
use crate::utils::abort::is_aborted;
use crate::utils::event_stream::{AssistantMessageEventSender, AssistantMessageEventStream};
use crate::utils::text::get_system_message_text;

const DEFAULT_API: &str = "faux";
const DEFAULT_PROVIDER: &str = "faux";
const DEFAULT_MODEL_ID: &str = "faux-1";
const DEFAULT_MODEL_NAME: &str = "Faux Model";
const DEFAULT_BASE_URL: &str = "http://localhost:0";
const DEFAULT_MIN_TOKEN_SIZE: usize = 3;
const DEFAULT_MAX_TOKEN_SIZE: usize = 5;

/// A text block.
pub fn faux_text(text: &str) -> AssistantContentBlock {
    AssistantContentBlock::Text(TextContent::new(text))
}

/// A thinking block.
pub fn faux_thinking(thinking: &str) -> AssistantContentBlock {
    AssistantContentBlock::Thinking(ThinkingContent::new(thinking))
}

/// A tool call; `id` defaults to a random `tool:` id.
pub fn faux_tool_call(
    name: &str,
    arguments: serde_json::Value,
    id: Option<&str>,
) -> AssistantContentBlock {
    AssistantContentBlock::ToolCall(ToolCall {
        id: id.map_or_else(|| random_id("tool"), str::to_owned),
        name: name.to_owned(),
        arguments: match arguments {
            serde_json::Value::Object(object) => object,
            _ => JsonObject::new(),
        },
        thought_signature: None,
        namespace: None,
    })
}

/// A completed faux response holding one text block.
pub fn faux_assistant_message(text: &str) -> AssistantMessage {
    faux_assistant_blocks(vec![faux_text(text)], StopReason::Stop)
}

/// A faux response with `content` and `stop_reason`.
pub fn faux_assistant_blocks(
    content: Vec<AssistantContentBlock>,
    stop_reason: StopReason,
) -> AssistantMessage {
    AssistantMessage {
        content,
        api: DEFAULT_API.to_owned(),
        provider: DEFAULT_PROVIDER.to_owned(),
        model: DEFAULT_MODEL_ID.to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: crate::now_ms(),
        duration_ms: None,
    }
}

/// A faux model definition.
#[derive(Debug, Clone, Default)]
pub struct FauxModelDefinition {
    /// Model id.
    pub id: String,
    /// Display name; the id when `None`.
    pub name: Option<String>,
    /// Whether the model reasons.
    pub reasoning: bool,
    /// Input modalities; text and image when `None`.
    pub input: Option<Vec<InputModality>>,
    /// Context window; 128000 when `None`.
    pub context_window: Option<u64>,
    /// Output limit; 16384 when `None`.
    pub max_tokens: Option<u64>,
}

/// A model with the faux defaults (`faux` API and provider).
pub fn faux_model(id: &str) -> Model {
    build_model(
        &FauxModelDefinition {
            id: id.to_owned(),
            ..FauxModelDefinition::default()
        },
        DEFAULT_API,
        DEFAULT_PROVIDER,
    )
}

fn build_model(definition: &FauxModelDefinition, api: &str, provider: &str) -> Model {
    Model {
        id: definition.id.clone(),
        name: definition
            .name
            .clone()
            .unwrap_or_else(|| definition.id.clone()),
        api: api.to_owned(),
        provider: provider.to_owned(),
        base_url: DEFAULT_BASE_URL.to_owned(),
        input: definition
            .input
            .clone()
            .unwrap_or_else(|| vec![InputModality::Text, InputModality::Image]),
        input_limits: None,
        cost: ModelCost {
            rates: ModelCostRates::default(),
            tiers: None,
        },
        headers: None,
        reasoning: definition.reasoning,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: definition.context_window.unwrap_or(128_000),
        max_tokens: definition.max_tokens.unwrap_or(16_384),
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    }
}

/// What a factory sees of the provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FauxProviderState {
    /// Requests so far, this one included.
    pub call_count: u64,
}

/// A response factory's future.
pub type FauxFuture = Pin<Box<dyn Future<Output = Result<AssistantMessage, String>> + Send>>;

/// Builds a response from the request; an `Err` streams as an error.
pub type FauxResponseFactory = Arc<
    dyn Fn(&TranscriptContext, &SimpleStreamOptions, FauxProviderState, &Model) -> FauxFuture
        + Send
        + Sync,
>;

/// One queued response.
// A short test queue; boxing the message would only complicate callers.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum FauxResponseStep {
    /// A fixed message.
    Message(AssistantMessage),
    /// A factory.
    Factory(FauxResponseFactory),
}

impl FauxResponseStep {
    /// A synchronous factory.
    pub fn from_fn<F>(factory: F) -> Self
    where
        F: Fn(
                &TranscriptContext,
                &SimpleStreamOptions,
                FauxProviderState,
                &Model,
            ) -> Result<AssistantMessage, String>
            + Send
            + Sync
            + 'static,
    {
        Self::Factory(Arc::new(move |context, options, state, model| {
            let result = factory(context, options, state, model);
            Box::pin(async move { result })
        }))
    }
}

impl From<AssistantMessage> for FauxResponseStep {
    fn from(message: AssistantMessage) -> Self {
        Self::Message(message)
    }
}

/// Options of [`FauxProvider::new`].
#[derive(Debug, Clone, Default)]
pub struct FauxProviderOptions {
    /// API id; a random `faux:` id when `None`.
    pub api: Option<String>,
    /// Provider id; `faux` when `None`.
    pub provider: Option<String>,
    /// Models; one `faux-1` model when empty.
    pub models: Vec<FauxModelDefinition>,
    /// Pacing; chunks only yield to the scheduler when `None` or 0.
    pub tokens_per_second: Option<f64>,
    /// Minimum chunk size in tokens.
    pub token_size_min: Option<usize>,
    /// Maximum chunk size in tokens.
    pub token_size_max: Option<usize>,
}

struct FauxState {
    call_count: u64,
    pending: VecDeque<FauxResponseStep>,
    prompt_cache: HashMap<String, Vec<String>>,
}

/// The faux provider. Register it in an [`crate::stream::ApiRegistry`].
pub struct FauxProvider {
    api: String,
    provider: String,
    models: Vec<Model>,
    min_token_size: usize,
    max_token_size: usize,
    tokens_per_second: Option<f64>,
    state: Arc<Mutex<FauxState>>,
}

fn lock(state: &Mutex<FauxState>) -> MutexGuard<'_, FauxState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn random_id(prefix: &str) -> String {
    let random = (crate::random_f64() * 2f64.powi(52)) as u64;
    format!("{prefix}:{}:{}", crate::now_ms(), base36(random))
}

fn base36(mut value: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
        if value == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

fn js_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn estimate_tokens(text: &str) -> u64 {
    js_len(text).div_ceil(4) as u64
}

fn json(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn blocks_to_text(blocks: &[UserContentBlock]) -> String {
    blocks
        .iter()
        .map(|block| match block {
            UserContentBlock::Text(text) => text.text.clone(),
            UserContentBlock::Image(image) => {
                format!("[image:{}:{}]", image.mime_type, js_len(&image.data))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn assistant_content_to_text(content: &[AssistantContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            AssistantContentBlock::Text(text) => text.text.clone(),
            AssistantContentBlock::Thinking(thinking) => thinking.thinking.clone(),
            AssistantContentBlock::ToolCall(call) => {
                format!("{}:{}", call.name, json(&call.arguments))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn message_to_text(message: &Message) -> String {
    match message {
        Message::System(system) => {
            let mut parts = vec![get_system_message_text(system)];
            parts.extend(
                system
                    .tools_removed
                    .iter()
                    .flatten()
                    .map(|tool| format!("tool-:{}", json(tool))),
            );
            parts.extend(
                system
                    .tools_added
                    .iter()
                    .flatten()
                    .map(|tool| format!("tool+:{}", json(tool))),
            );
            parts.retain(|part| !part.is_empty());
            parts.join("\n")
        }
        Message::User(user) => match &user.content {
            UserContent::Text(text) => text.clone(),
            UserContent::Blocks(blocks) => blocks_to_text(blocks),
        },
        Message::Assistant(assistant) => assistant_content_to_text(&assistant.content),
        Message::ToolResult(result) => {
            let mut parts = vec![result.tool_name.clone()];
            parts.extend(
                result
                    .content
                    .iter()
                    .map(|block| blocks_to_text(std::slice::from_ref(block))),
            );
            parts.join("\n")
        }
    }
}

fn joined_length(messages: &[String], count: usize) -> usize {
    let separators = if count > 0 { (count - 1) * 2 } else { 0 };
    separators
        + messages
            .iter()
            .take(count)
            .map(|message| js_len(message))
            .sum::<usize>()
}

fn common_prefix_length(a: &str, b: &str) -> usize {
    a.encode_utf16()
        .zip(b.encode_utf16())
        .take_while(|(x, y)| x == y)
        .count()
}

fn common_prompt_prefix_length(previous: &[String], current: &[String]) -> usize {
    let mut index = 0;
    while index < previous.len() && index < current.len() && previous[index] == current[index] {
        index += 1;
    }
    let rest = |messages: &[String]| {
        if index == messages.len() {
            String::new()
        } else {
            let tail = messages.get(index..).unwrap_or(&[]).join("\n\n");
            if index > 0 {
                format!("\n\n{tail}")
            } else {
                tail
            }
        }
    };
    joined_length(previous, index) + common_prefix_length(&rest(previous), &rest(current))
}

fn with_usage_estimate(
    mut message: AssistantMessage,
    context: &TranscriptContext,
    options: &StreamOptions,
    prompt_cache: &mut HashMap<String, Vec<String>>,
) -> AssistantMessage {
    let prompt: Vec<String> = context
        .messages()
        .iter()
        .map(|message| format!("{}:{}", message.role(), message_to_text(message)))
        .collect();
    let prompt_length = joined_length(&prompt, prompt.len());
    let prompt_tokens = prompt_length.div_ceil(4) as u64;
    let output_tokens = estimate_tokens(&assistant_content_to_text(&message.content));
    let mut input = prompt_tokens;
    let mut cache_read = 0;
    let mut cache_write = 0;
    if let Some(session_id) = options.session_id.as_deref().filter(|id| !id.is_empty())
        && options.cache_retention != Some(CacheRetention::None)
    {
        if let Some(previous) = prompt_cache.get(session_id) {
            let cached = common_prompt_prefix_length(previous, &prompt);
            cache_read = cached.div_ceil(4) as u64;
            cache_write = prompt_length.saturating_sub(cached).div_ceil(4) as u64;
            input = prompt_tokens.saturating_sub(cache_read);
        } else {
            cache_write = prompt_tokens;
        }
        prompt_cache.insert(session_id.to_owned(), prompt);
    }
    message.usage = Usage {
        input,
        output: output_tokens,
        cache_read,
        cache_write,
        ..Usage::default()
    };
    message.usage.total_tokens = message.usage.component_sum();
    message
}

fn split_by_token_size(text: &str, min: usize, max: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut chunks = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let span = max.saturating_sub(min) + 1;
        let token_size = min + ((crate::random_f64() * span as f64) as usize).min(span - 1);
        let size = (token_size * 4).max(1);
        let end = (index + size).min(chars.len());
        chunks.push(chars.get(index..end).unwrap_or(&[]).iter().collect());
        index = end;
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

fn error_message(error: &str, api: &str, provider: &str, model_id: &str) -> AssistantMessage {
    let mut message = faux_assistant_blocks(Vec::new(), StopReason::Error);
    message.api = api.to_owned();
    message.provider = provider.to_owned();
    message.model = model_id.to_owned();
    message.error_message = Some(error.to_owned());
    message
}

fn aborted_message(partial: &AssistantMessage) -> AssistantMessage {
    let mut message = partial.clone();
    message.stop_reason = StopReason::Aborted;
    message.error_message = Some("Request was aborted".to_owned());
    message.timestamp = crate::now_ms();
    message
}

struct Pacing {
    min: usize,
    max: usize,
    tokens_per_second: Option<f64>,
}

async fn schedule_chunk(chunk: &str, tokens_per_second: Option<f64>) {
    match tokens_per_second.filter(|rate| *rate > 0.0 && rate.is_finite()) {
        None => tokio::task::yield_now().await,
        Some(rate) => {
            let seconds = estimate_tokens(chunk) as f64 / rate;
            tokio::time::sleep(Duration::from_secs_f64(seconds.clamp(0.0, 3600.0))).await;
        }
    }
}

async fn stream_with_deltas(
    sender: &AssistantMessageEventSender,
    message: AssistantMessage,
    pacing: &Pacing,
    options: &StreamOptions,
) {
    let signal = options.signal.as_ref();
    let mut partial = message.clone();
    partial.content.clear();
    partial.stop_reason = StopReason::Pending;
    let abort = |partial: &AssistantMessage| {
        let aborted = aborted_message(partial);
        sender.push(AssistantMessageEvent::Error {
            reason: StopReason::Aborted,
            error: aborted.clone(),
        });
        sender.end(Some(aborted));
    };
    if is_aborted(signal) {
        abort(&partial);
        return;
    }
    sender.push(AssistantMessageEvent::Start {
        partial: partial.clone(),
    });
    for (index, block) in message.content.iter().enumerate() {
        if is_aborted(signal) {
            abort(&partial);
            return;
        }
        match block {
            AssistantContentBlock::Thinking(thinking) => {
                partial
                    .content
                    .push(AssistantContentBlock::Thinking(ThinkingContent::new("")));
                sender.push(AssistantMessageEvent::ThinkingStart {
                    content_index: index,
                    partial: partial.clone(),
                });
                for chunk in split_by_token_size(&thinking.thinking, pacing.min, pacing.max) {
                    schedule_chunk(&chunk, pacing.tokens_per_second).await;
                    if is_aborted(signal) {
                        abort(&partial);
                        return;
                    }
                    if let Some(AssistantContentBlock::Thinking(block)) =
                        partial.content.get_mut(index)
                    {
                        block.thinking.push_str(&chunk);
                    }
                    sender.push(AssistantMessageEvent::ThinkingDelta {
                        content_index: index,
                        delta: chunk,
                        partial: partial.clone(),
                    });
                }
                sender.push(AssistantMessageEvent::ThinkingEnd {
                    content_index: index,
                    content: thinking.thinking.clone(),
                    partial: partial.clone(),
                });
            }
            AssistantContentBlock::Text(text) => {
                partial
                    .content
                    .push(AssistantContentBlock::Text(TextContent::new("")));
                sender.push(AssistantMessageEvent::TextStart {
                    content_index: index,
                    partial: partial.clone(),
                });
                for chunk in split_by_token_size(&text.text, pacing.min, pacing.max) {
                    schedule_chunk(&chunk, pacing.tokens_per_second).await;
                    if is_aborted(signal) {
                        abort(&partial);
                        return;
                    }
                    if let Some(AssistantContentBlock::Text(block)) = partial.content.get_mut(index)
                    {
                        block.text.push_str(&chunk);
                    }
                    sender.push(AssistantMessageEvent::TextDelta {
                        content_index: index,
                        delta: chunk,
                        partial: partial.clone(),
                    });
                }
                sender.push(AssistantMessageEvent::TextEnd {
                    content_index: index,
                    content: text.text.clone(),
                    partial: partial.clone(),
                });
            }
            AssistantContentBlock::ToolCall(call) => {
                partial
                    .content
                    .push(AssistantContentBlock::ToolCall(ToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        ..ToolCall::default()
                    }));
                sender.push(AssistantMessageEvent::ToolCallStart {
                    content_index: index,
                    partial: partial.clone(),
                });
                for chunk in split_by_token_size(&json(&call.arguments), pacing.min, pacing.max) {
                    schedule_chunk(&chunk, pacing.tokens_per_second).await;
                    if is_aborted(signal) {
                        abort(&partial);
                        return;
                    }
                    sender.push(AssistantMessageEvent::ToolCallDelta {
                        content_index: index,
                        delta: chunk,
                        partial: partial.clone(),
                    });
                }
                if let Some(AssistantContentBlock::ToolCall(block)) = partial.content.get_mut(index)
                {
                    block.arguments = call.arguments.clone();
                }
                sender.push(AssistantMessageEvent::ToolCallEnd {
                    content_index: index,
                    tool_call: call.clone(),
                    partial: partial.clone(),
                });
            }
        }
    }
    match message.stop_reason {
        StopReason::Pending => {
            let failed = error_message(
                "Faux response ended without a stop reason",
                &message.api,
                &message.provider,
                &message.model,
            );
            sender.finish(failed);
        }
        _ => sender.finish(message),
    }
}

impl FauxProvider {
    /// A provider with `options`.
    pub fn new(options: FauxProviderOptions) -> Self {
        let api = options.api.unwrap_or_else(|| random_id(DEFAULT_API));
        let provider = options
            .provider
            .unwrap_or_else(|| DEFAULT_PROVIDER.to_owned());
        let max = options.token_size_max.unwrap_or(DEFAULT_MAX_TOKEN_SIZE);
        let min_token_size = options
            .token_size_min
            .unwrap_or(DEFAULT_MIN_TOKEN_SIZE)
            .min(max)
            .max(1);
        let max_token_size = max.max(min_token_size);
        let definitions = if options.models.is_empty() {
            vec![FauxModelDefinition {
                id: DEFAULT_MODEL_ID.to_owned(),
                name: Some(DEFAULT_MODEL_NAME.to_owned()),
                ..FauxModelDefinition::default()
            }]
        } else {
            options.models
        };
        let models = definitions
            .iter()
            .map(|definition| build_model(definition, &api, &provider))
            .collect();
        Self {
            api,
            provider,
            models,
            min_token_size,
            max_token_size,
            tokens_per_second: options.tokens_per_second,
            state: Arc::new(Mutex::new(FauxState {
                call_count: 0,
                pending: VecDeque::new(),
                prompt_cache: HashMap::new(),
            })),
        }
    }

    /// The models, first one the default.
    pub fn models(&self) -> &[Model] {
        &self.models
    }

    /// The first model.
    pub fn model(&self) -> &Model {
        // `new` always builds at least one model.
        &self.models[0]
    }

    /// The model with `id`.
    pub fn get_model(&self, id: &str) -> Option<&Model> {
        self.models.iter().find(|model| model.id == id)
    }

    /// The provider id.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Requests so far.
    pub fn call_count(&self) -> u64 {
        lock(&self.state).call_count
    }

    /// Replaces the queue.
    pub fn set_responses(&self, responses: Vec<FauxResponseStep>) {
        lock(&self.state).pending = responses.into();
    }

    /// Appends to the queue.
    pub fn append_responses(&self, responses: Vec<FauxResponseStep>) {
        lock(&self.state).pending.extend(responses);
    }

    /// Queued responses left.
    pub fn pending_response_count(&self) -> usize {
        lock(&self.state).pending.len()
    }

    fn run(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        let (step, call_count) = {
            let mut state = lock(&self.state);
            state.call_count += 1;
            (state.pending.pop_front(), state.call_count)
        };
        let api = self.api.clone();
        let provider = self.provider.clone();
        let model = model.clone();
        let context = context.clone();
        let state = Arc::clone(&self.state);
        let pacing = Pacing {
            min: self.min_token_size,
            max: self.max_token_size,
            tokens_per_second: self.tokens_per_second,
        };
        spawn_stream(&model.clone(), move |sender| async move {
            if let Some(on_response) = &options.base.on_response {
                on_response(
                    &ProviderResponse {
                        status: 200,
                        headers: Default::default(),
                    },
                    &model,
                );
            }
            let Some(step) = step else {
                let message =
                    error_message("No more faux responses queued", &api, &provider, &model.id);
                let message = with_usage_estimate(
                    message,
                    &context,
                    &options.base,
                    &mut lock(&state).prompt_cache,
                );
                sender.finish(message);
                return;
            };
            let resolved = match step {
                FauxResponseStep::Message(message) => Ok(message),
                FauxResponseStep::Factory(factory) => {
                    factory(&context, &options, FauxProviderState { call_count }, &model).await
                }
            };
            let message = match resolved {
                Ok(mut message) => {
                    message.api = api.clone();
                    message.provider = provider.clone();
                    message.model = model.id.clone();
                    with_usage_estimate(
                        message,
                        &context,
                        &options.base,
                        &mut lock(&state).prompt_cache,
                    )
                }
                Err(error) => {
                    sender.finish(error_message(&error, &api, &provider, &model.id));
                    return;
                }
            };
            stream_with_deltas(&sender, message, &pacing, &options.base).await;
        })
    }
}

impl ApiProvider for FauxProvider {
    fn api(&self) -> &str {
        &self.api
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        self.run(model, context, options.into())
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        self.run(model, context, options)
    }
}

#[cfg(test)]
mod tests {
    //! Cases from Pi `test/faux-provider.test.ts` (v1.1.0), named after the
    //! Pi test each follows.

    use std::sync::Arc;

    use serde_json::{Value, json};

    use super::*;
    use crate::stream::ApiRegistry;
    use crate::types::{Context, Message, UserMessage};
    use crate::utils::abort::AbortController;

    fn register(options: FauxProviderOptions) -> (ApiRegistry, Arc<FauxProvider>) {
        let registry = ApiRegistry::new();
        let faux = Arc::new(FauxProvider::new(options));
        registry.register(faux.clone());
        (registry, faux)
    }

    fn hi() -> Context {
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: "hi".into(),
                timestamp: 1,
            })],
            tools: None,
        }
    }

    fn names(events: &[AssistantMessageEvent]) -> Vec<String> {
        events
            .iter()
            .map(|event| {
                serde_json::to_value(event)
                    .ok()
                    .and_then(|value| value["type"].as_str().map(str::to_owned))
                    .unwrap_or_default()
            })
            .collect()
    }

    // "registers a custom provider and estimates usage"
    #[tokio::test]
    async fn estimates_usage() {
        let (registry, faux) = register(FauxProviderOptions::default());
        faux.set_responses(vec![faux_assistant_message("hello world").into()]);
        let context = Context {
            system_prompt: Some("Be concise.".into()),
            ..hi()
        };
        let response = registry
            .complete(faux.model(), context, StreamOptions::default())
            .await
            .unwrap();
        assert_eq!(response.content, vec![faux_text("hello world")]);
        assert!(response.usage.input > 0 && response.usage.output > 0);
        assert_eq!(
            response.usage.total_tokens,
            response.usage.input + response.usage.output
        );
        assert_eq!(faux.call_count(), 1);
    }

    // "rewrites api, provider, and model on returned messages"
    #[tokio::test]
    async fn rewrites_identity() {
        let (registry, faux) = register(FauxProviderOptions {
            api: Some("faux:test".into()),
            provider: Some("faux-provider".into()),
            models: vec![FauxModelDefinition {
                id: "faux-model".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        faux.set_responses(vec![faux_assistant_message("hello").into()]);
        let response = registry
            .complete(faux.model(), hi(), StreamOptions::default())
            .await
            .unwrap();
        assert_eq!(
            (
                response.api.as_str(),
                response.provider.as_str(),
                response.model.as_str()
            ),
            ("faux:test", "faux-provider", "faux-model")
        );
    }

    // "supports multiple models with per-model reasoning and model-aware factories"
    #[tokio::test]
    async fn model_aware_factories() {
        let (registry, faux) = register(FauxProviderOptions {
            models: vec![
                FauxModelDefinition {
                    id: "faux-fast".into(),
                    reasoning: false,
                    ..Default::default()
                },
                FauxModelDefinition {
                    id: "faux-thinker".into(),
                    reasoning: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        let factory = || {
            FauxResponseStep::from_fn(|_, _, _, model| {
                Ok(faux_assistant_message(&format!(
                    "{}:{}",
                    model.id, model.reasoning
                )))
            })
        };
        faux.set_responses(vec![factory(), factory()]);
        let fast = registry
            .complete(
                faux.get_model("faux-fast").unwrap(),
                hi(),
                StreamOptions::default(),
            )
            .await
            .unwrap();
        let thinker = registry
            .complete(
                faux.get_model("faux-thinker").unwrap(),
                hi(),
                StreamOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(fast.content, vec![faux_text("faux-fast:false")]);
        assert_eq!(thinker.content, vec![faux_text("faux-thinker:true")]);
    }

    // "consumes queued responses in order and errors when exhausted"
    #[tokio::test]
    async fn consumes_in_order_then_errors() {
        let (registry, faux) = register(FauxProviderOptions::default());
        faux.set_responses(vec![
            faux_assistant_message("first").into(),
            faux_assistant_message("second").into(),
        ]);
        let mut texts = Vec::new();
        for _ in 0..3 {
            texts.push(
                registry
                    .complete(faux.model(), hi(), StreamOptions::default())
                    .await
                    .unwrap(),
            );
        }
        assert_eq!(texts[0].content, vec![faux_text("first")]);
        assert_eq!(texts[1].content, vec![faux_text("second")]);
        assert_eq!(texts[2].stop_reason, StopReason::Error);
        assert_eq!(
            texts[2].error_message.as_deref(),
            Some("No more faux responses queued")
        );
        assert_eq!(faux.pending_response_count(), 0);
        assert_eq!(faux.call_count(), 3);
    }

    // "emits an error when a response factory throws"
    #[tokio::test]
    async fn factory_errors_end_the_stream() {
        let (registry, faux) = register(FauxProviderOptions::default());
        faux.set_responses(vec![FauxResponseStep::from_fn(|_, _, _, _| {
            Err("factory exploded".into())
        })]);
        let response = registry
            .complete(faux.model(), hi(), StreamOptions::default())
            .await
            .unwrap();
        assert_eq!(response.stop_reason, StopReason::Error);
        assert_eq!(response.error_message.as_deref(), Some("factory exploded"));
    }

    // "rejects a queued response without a terminal stop reason"
    #[tokio::test]
    async fn rejects_pending_responses() {
        let (registry, faux) = register(FauxProviderOptions::default());
        faux.set_responses(vec![
            faux_assistant_blocks(vec![faux_text("partial")], StopReason::Pending).into(),
        ]);
        let stream = registry
            .stream(faux.model(), hi(), StreamOptions::default())
            .unwrap();
        let events = stream.collect().await;
        assert!(!names(&events).contains(&"done".to_owned()));
        let result = stream.result().await.unwrap();
        assert_eq!(result.stop_reason, StopReason::Error);
        assert_eq!(
            result.error_message.as_deref(),
            Some("Faux response ended without a stop reason")
        );
    }

    // "counts cached characters up to the first difference in the joined prompt"
    #[tokio::test]
    async fn simulates_prompt_cache_by_prefix() {
        let (registry, faux) = register(FauxProviderOptions::default());
        faux.set_responses(vec![
            faux_assistant_message("a").into(),
            faux_assistant_message("b").into(),
            faux_assistant_message("c").into(),
        ]);
        let options = || StreamOptions {
            session_id: Some("session-1".into()),
            cache_retention: Some(crate::types::CacheRetention::Short),
            ..StreamOptions::default()
        };
        let user = |text: &str| {
            Message::User(UserMessage {
                content: text.into(),
                timestamp: 1,
            })
        };
        let context = |messages: Vec<Message>| Context {
            system_prompt: None,
            messages,
            tools: None,
        };
        registry
            .complete(faux.model(), context(vec![user("hello world")]), options())
            .await
            .unwrap();
        let extended = registry
            .complete(
                faux.model(),
                context(vec![user("hello world"), user("next")]),
                options(),
            )
            .await
            .unwrap();
        assert_eq!(
            (
                extended.usage.input,
                extended.usage.cache_read,
                extended.usage.cache_write
            ),
            (3, 4, 3)
        );
        let edited = registry
            .complete(
                faux.model(),
                context(vec![user("hello wide"), user("next")]),
                options(),
            )
            .await
            .unwrap();
        assert_eq!(
            (
                edited.usage.input,
                edited.usage.cache_read,
                edited.usage.cache_write
            ),
            (4, 3, 4)
        );
    }

    // "streams an exact event order for fixed-size chunks" and
    // "streams thinking, text, and partial tool call deltas"
    #[tokio::test]
    async fn streams_exact_event_order() {
        let (registry, faux) = register(FauxProviderOptions {
            token_size_min: Some(1),
            token_size_max: Some(1),
            ..Default::default()
        });
        faux.set_responses(vec![
            faux_assistant_blocks(
                vec![
                    faux_thinking("go"),
                    faux_text("ok"),
                    faux_tool_call("echo", json!({}), Some("tool-1")),
                ],
                StopReason::ToolUse,
            )
            .into(),
            faux_assistant_blocks(
                vec![faux_tool_call(
                    "echo",
                    json!({ "text": "hi", "count": 12 }),
                    Some("tool-2"),
                )],
                StopReason::ToolUse,
            )
            .into(),
        ]);
        let stream = registry
            .stream(faux.model(), hi(), StreamOptions::default())
            .unwrap();
        let events = stream.collect().await;
        assert_eq!(
            names(&events),
            [
                "start",
                "thinking_start",
                "thinking_delta",
                "thinking_end",
                "text_start",
                "text_delta",
                "text_end",
                "toolcall_start",
                "toolcall_delta",
                "toolcall_end",
                "done"
            ]
        );
        let stream = registry
            .stream(faux.model(), hi(), StreamOptions::default())
            .unwrap();
        let deltas: Vec<String> = stream
            .collect()
            .await
            .into_iter()
            .filter_map(|event| match event {
                AssistantMessageEvent::ToolCallDelta { delta, .. } => Some(delta),
                _ => None,
            })
            .collect();
        assert!(deltas.len() > 1);
        assert_eq!(
            serde_json::from_str::<Value>(&deltas.concat()).unwrap(),
            json!({ "text": "hi", "count": 12 })
        );
    }

    // "supports aborting before the first chunk"
    #[tokio::test]
    async fn aborts_before_the_first_chunk() {
        let (registry, faux) = register(FauxProviderOptions {
            tokens_per_second: Some(50.0),
            token_size_min: Some(3),
            token_size_max: Some(3),
            ..Default::default()
        });
        faux.set_responses(vec![
            faux_assistant_message("abcdefghijklmnopqrstuvwxyz").into(),
        ]);
        let controller = AbortController::new();
        controller.abort();
        let options = StreamOptions {
            signal: Some(controller.signal()),
            ..StreamOptions::default()
        };
        let stream = registry.stream(faux.model(), hi(), options).unwrap();
        let events = stream.collect().await;
        assert_eq!(names(&events), ["error"]);
        assert_eq!(
            stream.result().await.unwrap().stop_reason,
            StopReason::Aborted
        );
    }

    // "supports aborting mid-text stream when paced"
    #[tokio::test]
    async fn aborts_mid_text_when_paced() {
        let (registry, faux) = register(FauxProviderOptions {
            tokens_per_second: Some(100.0),
            token_size_min: Some(3),
            token_size_max: Some(3),
            ..Default::default()
        });
        faux.set_responses(vec![
            faux_assistant_message("abcdefghijklmnopqrstuvwxyz").into(),
        ]);
        let controller = AbortController::new();
        let options = StreamOptions {
            signal: Some(controller.signal()),
            ..StreamOptions::default()
        };
        let stream = registry.stream(faux.model(), hi(), options).unwrap();
        let mut seen = Vec::new();
        let mut text_deltas = 0;
        while let Some(event) = stream.next().await {
            if matches!(event, AssistantMessageEvent::TextDelta { .. }) {
                text_deltas += 1;
                controller.abort();
            }
            seen.extend(names(&[event]));
        }
        assert_eq!(text_deltas, 1);
        assert!(seen.contains(&"error".to_owned()));
        assert!(!seen.contains(&"text_end".to_owned()));
    }
}
