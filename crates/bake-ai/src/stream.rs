//! `stream` and `complete` entry points dispatched by the model's API.
//!
//! Ported from the API registry and entry points of Pi
//! `packages/ai/src/compat.ts` (v1.1.0), limited to the three protocols this
//! crate implements and providers such as [`crate::providers::faux`] that a
//! caller registers. Pi keeps one global registry; here each
//! [`ApiRegistry`] is a value, so tests and agents own theirs. Environment API
//! keys and model catalogs are not consulted: callers pass the key.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use crate::api::anthropic_messages::AnthropicMessagesApi;
use crate::api::openai_completions::OpenAICompletionsApi;
use crate::api::openai_responses::OpenAIResponsesApi;
use crate::options::{SimpleStreamOptions, StreamOptions};
use crate::transcript::normalize_context;
use crate::types::{AssistantMessage, Context, Model, TranscriptContext};
use crate::utils::event_stream::AssistantMessageEventStream;

/// One API implementation, Pi's `ProviderStreams`.
pub trait ApiProvider: Send + Sync {
    /// The API id models name in `api`.
    fn api(&self) -> &str;

    /// Streams with the shared options.
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream;

    /// Streams with provider-neutral reasoning options.
    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream;
}

/// Why a registry call failed before streaming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// No provider is registered for the model's API.
    NoProvider(String),
    /// The stream ended without a final message.
    NoResult,
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoProvider(api) => write!(f, "No API provider registered for api: {api}"),
            Self::NoResult => f.write_str("The stream ended without a final message"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// API providers by API id.
#[derive(Default)]
pub struct ApiRegistry {
    providers: RwLock<BTreeMap<String, Arc<dyn ApiProvider>>>,
}

impl ApiRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry with `openai-completions`, `openai-responses`, and
    /// `anthropic-messages`.
    pub fn with_builtins() -> Self {
        let registry = Self::new();
        registry.register(Arc::new(OpenAICompletionsApi));
        registry.register(Arc::new(OpenAIResponsesApi));
        registry.register(Arc::new(AnthropicMessagesApi));
        registry
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, Arc<dyn ApiProvider>>> {
        self.providers
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Registers `provider`, replacing one with the same API id.
    pub fn register(&self, provider: Arc<dyn ApiProvider>) {
        let api = provider.api().to_owned();
        self.providers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(api, provider);
    }

    /// Removes the provider for `api`; whether one was registered.
    pub fn unregister(&self, api: &str) -> bool {
        self.providers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(api)
            .is_some()
    }

    /// The provider for `api`.
    pub fn get(&self, api: &str) -> Option<Arc<dyn ApiProvider>> {
        self.read().get(api).cloned()
    }

    fn resolve(&self, api: &str) -> Result<Arc<dyn ApiProvider>, RegistryError> {
        self.get(api)
            .ok_or_else(|| RegistryError::NoProvider(api.to_owned()))
    }

    /// Normalizes `context` and streams through the model's API.
    pub fn stream(
        &self,
        model: &Model,
        context: Context,
        options: StreamOptions,
    ) -> Result<AssistantMessageEventStream, RegistryError> {
        Ok(self
            .resolve(&model.api)?
            .stream(model, &normalize_context(context), options))
    }

    /// [`ApiRegistry::stream`] to its final message.
    pub async fn complete(
        &self,
        model: &Model,
        context: Context,
        options: StreamOptions,
    ) -> Result<AssistantMessage, RegistryError> {
        self.stream(model, context, options)?
            .result()
            .await
            .ok_or(RegistryError::NoResult)
    }

    /// Normalizes `context` and streams with simple options.
    pub fn stream_simple(
        &self,
        model: &Model,
        context: Context,
        options: SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, RegistryError> {
        Ok(self
            .resolve(&model.api)?
            .stream_simple(model, &normalize_context(context), options))
    }

    /// [`ApiRegistry::stream_simple`] to its final message.
    pub async fn complete_simple(
        &self,
        model: &Model,
        context: Context,
        options: SimpleStreamOptions,
    ) -> Result<AssistantMessage, RegistryError> {
        self.stream_simple(model, context, options)?
            .result()
            .await
            .ok_or(RegistryError::NoResult)
    }
}

fn builtins() -> &'static ApiRegistry {
    static BUILTINS: std::sync::OnceLock<ApiRegistry> = std::sync::OnceLock::new();
    BUILTINS.get_or_init(ApiRegistry::with_builtins)
}

/// Streams through the built-in protocol named by `model.api`.
pub fn stream(
    model: &Model,
    context: Context,
    options: StreamOptions,
) -> Result<AssistantMessageEventStream, RegistryError> {
    builtins().stream(model, context, options)
}

/// [`stream`] to its final message.
pub async fn complete(
    model: &Model,
    context: Context,
    options: StreamOptions,
) -> Result<AssistantMessage, RegistryError> {
    builtins().complete(model, context, options).await
}

/// Streams with simple options through the built-in protocol.
pub fn stream_simple(
    model: &Model,
    context: Context,
    options: SimpleStreamOptions,
) -> Result<AssistantMessageEventStream, RegistryError> {
    builtins().stream_simple(model, context, options)
}

/// [`stream_simple`] to its final message.
pub async fn complete_simple(
    model: &Model,
    context: Context,
    options: SimpleStreamOptions,
) -> Result<AssistantMessage, RegistryError> {
    builtins().complete_simple(model, context, options).await
}
