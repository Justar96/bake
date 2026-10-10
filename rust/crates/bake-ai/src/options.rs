//! Request options shared by every provider.
//!
//! Ported from Pi `packages/ai/src/types.ts` (`ProviderRequestOptions`,
//! `StreamOptions`, `SimpleStreamOptions`). Pi's callbacks may be
//! asynchronous; these are synchronous. Not ported: `fetch`, `transport`,
//! `websocketConnectTimeoutMs`, `telemetryContext`, and deferred requests.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use serde_json::Value;

use crate::types::{
    CacheRetention, JsonObject, Model, ProviderResponse, ThinkingBudgets, ThinkingLevel, ToolChoice,
};
use crate::utils::abort::AbortSignal;

/// Replaces the request payload before it is sent; `None` keeps it.
pub type OnPayload = Arc<dyn Fn(&Value, &Model) -> Option<Value> + Send + Sync>;
/// Observes the HTTP response before its body is read.
pub type OnResponse = Arc<dyn Fn(&ProviderResponse, &Model) + Send + Sync>;
/// Observes each parsed provider stream event before normalization.
pub type OnProviderStreamEvent = Arc<dyn Fn(&Value, &Model) + Send + Sync>;

/// Request headers in order. A `None` value removes a default header of the
/// same name (compared case-insensitively).
pub type ProviderHeaders = Vec<(String, Option<String>)>;

/// Options every provider accepts.
#[derive(Clone, Default)]
pub struct StreamOptions {
    /// Cancels the request.
    pub signal: Option<AbortSignal>,
    /// The API key.
    pub api_key: Option<String>,
    /// Provider-scoped environment values, read before the process
    /// environment (for example `PI_CACHE_RETENTION`).
    pub env: Option<BTreeMap<String, String>>,
    /// Replaces the payload before it is sent.
    pub on_payload: Option<OnPayload>,
    /// Observes the response status and headers.
    pub on_response: Option<OnResponse>,
    /// Observes each parsed provider event.
    pub on_provider_stream_event: Option<OnProviderStreamEvent>,
    /// Extra headers; they override defaults.
    pub headers: Option<ProviderHeaders>,
    /// Timeout until response headers; 10 minutes when `None`.
    pub timeout_ms: Option<u64>,
    /// Request retries; 0 when `None`.
    pub max_retries: Option<u32>,
    /// Cap of a server-requested retry delay; 60 s when `None`, none when 0.
    pub max_retry_delay_ms: Option<u64>,
    /// Sampling temperature.
    pub temperature: Option<f64>,
    /// Extra sampling parameters merged into the body last.
    pub sampling_params: Option<JsonObject>,
    /// Output token limit.
    pub max_tokens: Option<u64>,
    /// Prompt cache retention; `short` when `None`.
    pub cache_retention: Option<CacheRetention>,
    /// Session id for caching and affinity.
    pub session_id: Option<String>,
    /// Request metadata, such as Anthropic's `user_id`.
    pub metadata: Option<JsonObject>,
}

impl fmt::Debug for StreamOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamOptions")
            .field("signal", &self.signal.as_ref().map(AbortSignal::aborted))
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("headers", &self.headers.as_ref().map(Vec::len))
            .field("timeout_ms", &self.timeout_ms)
            .field("max_retries", &self.max_retries)
            .field("temperature", &self.temperature)
            .field("max_tokens", &self.max_tokens)
            .field("cache_retention", &self.cache_retention)
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

/// Provider-neutral options with a reasoning level.
#[derive(Debug, Clone, Default)]
pub struct SimpleStreamOptions {
    /// The shared options.
    pub base: StreamOptions,
    /// Tool selection.
    pub tool_choice: Option<ToolChoice>,
    /// The reasoning level; `None` is off.
    pub reasoning: Option<ThinkingLevel>,
    /// Custom token budgets per level.
    pub thinking_budgets: Option<ThinkingBudgets>,
}

impl From<StreamOptions> for SimpleStreamOptions {
    fn from(base: StreamOptions) -> Self {
        Self {
            base,
            ..Self::default()
        }
    }
}

/// Pi's `getProviderEnvValue`: the scoped value, then the process
/// environment; empty values count as absent.
pub fn provider_env_value(name: &str, env: Option<&BTreeMap<String, String>>) -> Option<String> {
    env.and_then(|env| env.get(name))
        .filter(|value| !value.is_empty())
        .cloned()
        .or_else(|| std::env::var(name).ok().filter(|value| !value.is_empty()))
}

/// Pi's cache-retention default: the option, then `PI_CACHE_RETENTION=long`,
/// then `short`.
pub fn resolve_cache_retention(options: &StreamOptions) -> CacheRetention {
    if let Some(retention) = options.cache_retention {
        return retention;
    }
    if provider_env_value("PI_CACHE_RETENTION", options.env.as_ref()).as_deref() == Some("long") {
        return CacheRetention::Long;
    }
    CacheRetention::Short
}

/// Whether `headers` sets `name` (case-insensitively) to a non-blank value.
pub fn has_header(headers: Option<&ProviderHeaders>, name: &str) -> bool {
    headers.is_some_and(|headers| {
        headers.iter().any(|(key, value)| {
            key.eq_ignore_ascii_case(name)
                && value
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
        })
    })
}
