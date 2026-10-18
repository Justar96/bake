//! Web search through the CLIProxyAPI route: one Anthropic Messages
//! request per query, carrying the server-side `web_search_20250305`
//! tool, whose sources come only from the structured
//! `web_search_tool_result` blocks. A response without one is an error,
//! never a source list scraped from prose.
//!
//! Ported from `packages/web/web-search-deepseek/src/{provider,types}.ts`
//! (Bake 0.3): the request body, its limits (`max_tokens` 4096, `max_uses`
//! 5, `anthropic-version` 2023-06-01), the `x-api-key` and bearer headers,
//! `citationSnippets`, and `mapAnthropicResponse`. By the owner's decision
//! the request goes to the session's CLIProxyAPI route, at
//! `<model base URL>/v1/messages` with the route's key and headers, rather
//! than to DeepSeek's endpoint; [`select_search_model`] picks the model.
//! The request a search sends, without its key, is returned with the
//! result ([`SearchRequest`]) so the tool records it in the session.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use bake_ai::{AbortSignal, Model};
use serde_json::{Value, json};

use super::{WEB_ABORTED, WEB_PROVIDER_ERROR, WEB_PROVIDER_UNAVAILABLE, WebError};
use crate::agent_session::SessionSelection;
use crate::cliproxyapi::CLIPROXYAPI_ID;
use crate::config_value::with_command_cancel;
use crate::model_registry::ModelRuntime;
use crate::owned_work::WorkGate;

/// The `anthropic-version` header value.
pub const SEARCH_API_VERSION: &str = "2023-06-01";
/// The Messages request's `max_tokens`.
pub const SEARCH_MAX_TOKENS: u64 = 4096;
/// The server tool's `max_uses`.
pub const SEARCH_MAX_USES: u64 = 5;
/// The `settings.json` member naming the search model.
pub const SEARCH_MODEL_SETTING: &str = "webSearch.model";
/// The protocol a search model must speak.
pub const ANTHROPIC_MESSAGES: &str = "anthropic-messages";
/// The largest search response read.
pub const MAX_SEARCH_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// One citeable source (`WebSearchSource`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSearchSource {
    /// The source's URL.
    pub url: String,
    /// Its title, when the provider gave one.
    pub title: Option<String>,
    /// A cited excerpt.
    pub snippet: Option<String>,
    /// The provider's page age or date.
    pub published_at: Option<String>,
}

impl WebSearchSource {
    /// The source with only its URL.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            title: None,
            snippet: None,
            published_at: None,
        }
    }

    /// The source as JSON, absent fields left out (`projectSource`).
    pub fn to_json(&self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("url".into(), json!(self.url));
        for (key, value) in [
            ("title", &self.title),
            ("snippet", &self.snippet),
            ("publishedAt", &self.published_at),
        ] {
            if let Some(value) = value {
                object.insert(key.into(), json!(value));
            }
        }
        Value::Object(object)
    }
}

/// A normalized search outcome (`WebSearchResult`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WebSearchResult {
    /// A provider-written answer, when any.
    pub content: Option<String>,
    /// The sources, in result order.
    pub sources: Vec<WebSearchSource>,
    /// Whether sources were dropped to honor the result cap.
    pub truncated: bool,
}

/// The secret-free request one search sent: its endpoint, its
/// `anthropic-version`, and its exact body
/// (`DeepSeekSearchLlmRequest`).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchRequest {
    /// The Messages endpoint.
    pub endpoint: String,
    /// The `anthropic-version` header.
    pub api_version: String,
    /// The JSON body.
    pub body: Value,
}

impl SearchRequest {
    /// The request as JSON.
    pub fn to_json(&self) -> Value {
        json!({ "endpoint": self.endpoint, "apiVersion": self.api_version, "body": self.body })
    }
}

/// The Messages body for one query.
pub fn search_request_body(model: &str, query: &str) -> Value {
    json!({
        "model": model,
        "max_tokens": SEARCH_MAX_TOKENS,
        "messages": [{
            "role": "user",
            "content": [{ "type": "text", "text": format!("Perform a web search for the query: {query}") }],
        }],
        "tools": [{ "type": "web_search_20250305", "name": "web_search", "max_uses": SEARCH_MAX_USES }],
    })
}

fn unavailable(message: impl Into<String>) -> WebError {
    WebError::new(WEB_PROVIDER_UNAVAILABLE, message)
}

/// The model searches run on, in this order: the model the
/// `webSearch.model` setting names (an id on the CLIProxyAPI route, with
/// or without the `cliproxyapi/` prefix); else the session's model when
/// it is an Anthropic Messages model on the route; else the last of the
/// route's Anthropic Messages models whose id contains `haiku`, in route
/// order; else its first Anthropic Messages model. `available` is the
/// registry's models with configured auth.
pub fn select_search_model(
    available: &[Model],
    current: Option<&Model>,
    configured: Option<&str>,
) -> Result<Model, WebError> {
    let on_route =
        |model: &&Model| model.provider == CLIPROXYAPI_ID && model.api == ANTHROPIC_MESSAGES;
    let route: Vec<&Model> = available.iter().filter(on_route).collect();
    if let Some(configured) = configured {
        let id = configured
            .strip_prefix(&format!("{CLIPROXYAPI_ID}/"))
            .unwrap_or(configured);
        return route
            .iter()
            .find(|model| model.id == id)
            .map(|model| (*model).clone())
            .ok_or_else(|| {
                unavailable(format!(
                    "The {SEARCH_MODEL_SETTING} setting names \"{configured}\", which is not an Anthropic Messages model on the CLIProxyAPI route. The user can set {SEARCH_MODEL_SETTING} in settings.json to one that is."
                ))
            });
    }
    if let Some(current) = current
        && let Some(model) = route
            .iter()
            .find(|model| model.provider == current.provider && model.id == current.id)
    {
        return Ok((*model).clone());
    }
    route
        .iter()
        .rev()
        .find(|model| model.id.contains("haiku"))
        .or_else(|| route.first())
        .map(|model| (*model).clone())
        .ok_or_else(|| {
            unavailable(format!(
                "Web search runs through the CLIProxyAPI route and needs one of its Anthropic Messages models, but none is available. The user can sign in with /login, or set {SEARCH_MODEL_SETTING} in settings.json to such a model."
            ))
        })
}

fn non_empty(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// `citationSnippets`: each URL's first cited excerpt, from the `text`
/// blocks' `citations`; result items carry no excerpt of their own.
pub fn citation_snippets(blocks: &[Value]) -> Vec<(String, String)> {
    let mut snippets: Vec<(String, String)> = Vec::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        let citations = block.get("citations").and_then(Value::as_array);
        for citation in citations.into_iter().flatten() {
            if let (Some(url), Some(cited)) = (
                non_empty(citation.get("url")),
                non_empty(citation.get("cited_text")),
            ) && !snippets.iter().any(|(known, _)| *known == url)
            {
                snippets.push((url, cited));
            }
        }
    }
    snippets
}

/// `mapAnthropicResponse`: the sources of every `web_search_tool_result`
/// block, joined to their excerpts, deduplicated by URL (the first wins).
/// No such block is an error, as is a response whose result blocks all
/// failed.
pub fn map_anthropic_response(response: &Value) -> Result<WebSearchResult, MapError> {
    let blocks: &[Value] = response
        .get("content")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);
    let results: Vec<&Value> = blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("web_search_tool_result"))
        .collect();
    if results.is_empty() {
        return Err(MapError::NoResultBlocks);
    }
    let snippets = citation_snippets(blocks);
    let mut sources: Vec<WebSearchSource> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for block in results {
        match block.get("content") {
            Some(Value::Array(items)) => {
                for item in items {
                    if item.get("type").and_then(Value::as_str) != Some("web_search_result") {
                        continue;
                    }
                    let Some(url) = non_empty(item.get("url")) else {
                        continue;
                    };
                    if sources.iter().any(|source| source.url == url) {
                        continue;
                    }
                    let snippet = snippets
                        .iter()
                        .find(|(known, _)| *known == url)
                        .map(|(_, cited)| cited.clone());
                    sources.push(WebSearchSource {
                        title: non_empty(item.get("title")),
                        snippet,
                        published_at: non_empty(item.get("page_age")),
                        url,
                    });
                }
            }
            // An Anthropic `web_search_tool_result_error`.
            Some(Value::Object(error)) => {
                if let Some(code) = non_empty(error.get("error_code")) {
                    failures.push(code);
                }
            }
            _ => {}
        }
    }
    if sources.is_empty() && !failures.is_empty() {
        return Err(MapError::ToolFailed(failures));
    }
    Ok(WebSearchResult {
        content: None,
        sources,
        truncated: false,
    })
}

/// Why a response yielded no result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapError {
    /// No `web_search_tool_result` block: the model did not search.
    NoResultBlocks,
    /// Every result block failed, with these error codes.
    ToolFailed(Vec<String>),
}

impl std::fmt::Display for MapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoResultBlocks => f.write_str("the model returned no web_search_tool_result blocks; the request may not have triggered native web search"),
            Self::ToolFailed(codes) => write!(f, "the web search tool failed: {}", codes.join(", ")),
        }
    }
}

fn client() -> Result<&'static reqwest::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|error| super::source_chain(&error))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// The detail of an error body: `error.message`, a string `error`, or
/// `message`.
fn error_detail(body: &[u8]) -> Option<String> {
    let parsed: Value = serde_json::from_slice(body).ok()?;
    let error = parsed.get("error");
    error
        .and_then(Value::as_str)
        .or_else(|| {
            error
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
        })
        .or_else(|| parsed.get("message").and_then(Value::as_str))
        .filter(|detail| !detail.is_empty())
        .map(str::to_owned)
}

/// Searches through the CLIProxyAPI route of a session's model registry.
#[derive(Clone)]
pub struct CliProxySearch {
    registry: ModelRuntime,
    selection: Option<SessionSelection>,
    configured_model: Option<String>,
    owner: Option<WorkGate>,
}

impl std::fmt::Debug for CliProxySearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CliProxySearch")
            .field("configured_model", &self.configured_model)
            .finish_non_exhaustive()
    }
}

impl CliProxySearch {
    /// Searches over `registry`, preferring the model `selection`
    /// publishes and the `webSearch.model` setting's `configured_model`
    /// ([`select_search_model`]). `owner`, the session's gate, owns the
    /// key lookups, so shutdown waits for a key command a search started.
    pub fn new(
        registry: ModelRuntime,
        selection: Option<SessionSelection>,
        configured_model: Option<String>,
        owner: Option<WorkGate>,
    ) -> Self {
        Self {
            registry,
            selection,
            configured_model,
            owner,
        }
    }

    /// One search for `query`, with the request it sent. Aborting `signal`
    /// ends it at once.
    pub async fn search(
        &self,
        query: &str,
        signal: Option<&AbortSignal>,
    ) -> Result<(WebSearchResult, SearchRequest), WebError> {
        let aborted = || WebError::new(WEB_ABORTED, "web search aborted");
        if signal.is_some_and(AbortSignal::aborted) {
            return Err(aborted());
        }
        let cancelled = async {
            match signal {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            () = cancelled => Err(aborted()),
            result = self.run(query) => result,
        }
    }

    async fn run(&self, query: &str) -> Result<(WebSearchResult, SearchRequest), WebError> {
        let registry = self.registry.snapshot();
        let current = self
            .selection
            .as_ref()
            .and_then(|selection| selection.get().model);
        let model = select_search_model(
            &registry.available(),
            current.as_ref(),
            self.configured_model.as_deref(),
        )?;
        // Resolving a key may run a configured command, so it blocks. As a
        // model request's does, the lookup holds a permit of the session's
        // gate, and a search that ends early (its signal aborted, or its
        // future dropped) kills the command; shutdown then waits for the
        // lookup's thread.
        struct CancelOnDrop(Arc<AtomicBool>);
        impl Drop for CancelOnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let permit = match &self.owner {
            Some(owner) => Some(
                owner
                    .enter()
                    .ok_or_else(|| WebError::new(WEB_ABORTED, "web search aborted"))?,
            ),
            None => None,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = CancelOnDrop(Arc::clone(&cancel));
        let auth = {
            let model = model.clone();
            tokio::task::spawn_blocking(move || {
                let auth = with_command_cancel(cancel, || registry.get_auth(&model));
                drop(permit);
                auth
            })
            .await
            .map_err(|error| WebError::new(WEB_PROVIDER_ERROR, error.to_string()))?
            .map_err(|error| WebError::new(WEB_PROVIDER_ERROR, error))?
        };
        let name = format!("{}/{}", model.provider, model.id);
        let Some(auth) = auth else {
            return Err(unavailable(format!(
                "Web search model \"{name}\" has no API key. The user can sign in with /login."
            )));
        };
        let Some(api_key) = auth.api_key.filter(|key| !key.is_empty()) else {
            return Err(unavailable(format!(
                "Web search model \"{name}\" has no API key. The user can sign in with /login."
            )));
        };
        let request = SearchRequest {
            endpoint: bake_ai::http::join_url(&model.base_url, "/v1/messages"),
            api_version: SEARCH_API_VERSION.to_owned(),
            body: search_request_body(&model.id, query),
        };
        let failed = |message: String| WebError::new(WEB_PROVIDER_ERROR, message);
        let client = client()
            .map_err(|error| failed(format!("CLIProxyAPI web search request failed: {error}")))?;
        let secret = |value: String| {
            let mut header = reqwest::header::HeaderValue::from_str(&value)
                .map_err(|_| failed("the API key cannot be sent in an HTTP header".into()))?;
            header.set_sensitive(true);
            Ok::<_, WebError>(header)
        };
        let mut builder = client
            .post(&request.endpoint)
            .header("x-api-key", secret(api_key.clone())?)
            .header(
                reqwest::header::AUTHORIZATION,
                secret(format!("Bearer {api_key}"))?,
            )
            .header("anthropic-version", SEARCH_API_VERSION)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .header(
                reqwest::header::USER_AGENT,
                super::fetch::DEFAULT_USER_AGENT,
            );
        for (header, value) in auth.headers.unwrap_or_default() {
            builder = builder.header(header, value);
        }
        let body = serde_json::to_vec(&request.body)
            .map_err(|error| failed(format!("CLIProxyAPI web search request failed: {error}")))?;
        let mut response = builder.body(body).send().await.map_err(|error| {
            failed(format!(
                "CLIProxyAPI web search request failed: {}",
                super::source_chain(&error)
            ))
        })?;
        let status = response.status().as_u16();
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            failed(format!(
                "CLIProxyAPI web search request failed: {}",
                super::source_chain(&error)
            ))
        })? {
            if bytes.len() + chunk.len() > MAX_SEARCH_RESPONSE_BYTES {
                return Err(failed(
                    "CLIProxyAPI returned an unprocessable response body: it is too large".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let unsupported = |detail: String| {
            failed(format!(
                "Web search model \"{name}\" could not search: {detail}. The user can set {SEARCH_MODEL_SETTING} in settings.json to an Anthropic Messages model on the CLIProxyAPI route that supports web search."
            ))
        };
        if !(200..300).contains(&status) {
            let mut message = format!("CLIProxyAPI web search error (HTTP {status})");
            if let Some(detail) = error_detail(&bytes) {
                message.push_str(&format!(": {detail}"));
            }
            return Err(match status {
                400 | 404 | 422 => unsupported(message),
                _ => failed(format!("{message} (model \"{name}\")")),
            });
        }
        let payload: Value = serde_json::from_slice(&bytes).map_err(|error| {
            failed(format!(
                "CLIProxyAPI returned an unprocessable response body: {error}"
            ))
        })?;
        let result = map_anthropic_response(&payload).map_err(|error| match error {
            MapError::NoResultBlocks => unsupported(error.to_string()),
            MapError::ToolFailed(_) => failed(format!("{error} (model \"{name}\")")),
        })?;
        Ok((result, request))
    }
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
