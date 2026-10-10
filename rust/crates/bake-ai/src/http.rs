//! The HTTP transport the three protocols share.
//!
//! Pi's adapters send through the OpenAI and Anthropic SDKs over `fetch`;
//! this module provides what those SDKs did for them: header merging (a
//! caller header overrides a default of the same name, case-insensitively,
//! and a `None` value removes it), a timeout until response headers (the
//! SDKs' 10-minute default), cancellation, retries through
//! [`retry_provider_request`], SDK-shaped errors for non-2xx responses, and a
//! streamed body read as server-sent events. HTTP/1.1 over rustls; no proxy
//! is read from the environment.

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

use crate::options::{ProviderHeaders, StreamOptions};
use crate::types::ProviderResponse;
use crate::utils::abort::{AbortSignal, cancelled, is_aborted};
use crate::utils::error_body::ProviderError;
use crate::utils::provider_retry::{ProviderRetryOptions, retry_provider_request};
use crate::utils::sse::{ServerSentEvent, SseDecoder};

const DEFAULT_TIMEOUT_MS: u64 = 600_000;

/// The `User-Agent` sent by default: crate version, OS, and architecture.
pub fn user_agent() -> String {
    format!(
        "bake/{} ({}; {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

fn client() -> Result<&'static reqwest::Client, ProviderError> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .no_proxy()
                .build()
                .map_err(|error| error_chain(&error))
        })
        .as_ref()
        .map_err(|error| ProviderError::other(format!("Could not create the HTTP client: {error}")))
}

/// An error's message followed by its sources.
pub(crate) fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}

/// Merges header layers in order; later names replace earlier ones
/// case-insensitively, keeping the later spelling, and `None` removes.
pub fn merge_headers(layers: &[&ProviderHeaders]) -> Vec<(String, String)> {
    let mut merged: Vec<(String, String)> = Vec::new();
    for layer in layers {
        for (name, value) in layer.iter() {
            merged.retain(|(known, _)| !known.eq_ignore_ascii_case(name));
            if let Some(value) = value {
                merged.push((name.clone(), value.clone()));
            }
        }
    }
    merged
}

/// Converts a plain map to a header layer.
pub fn header_layer<'a>(
    headers: impl IntoIterator<Item = (&'a String, &'a String)>,
) -> ProviderHeaders {
    headers
        .into_iter()
        .map(|(name, value)| (name.clone(), Some(value.clone())))
        .collect()
}

/// One POST with a JSON body.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    /// The full URL.
    pub url: String,
    /// Final headers.
    pub headers: Vec<(String, String)>,
    /// The serialized body.
    pub body: Vec<u8>,
    /// Which SDK's error shape a non-2xx response takes.
    pub error_shape: SdkErrorShape,
}

/// The SDK whose `APIError` a non-2xx response reproduces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SdkErrorShape {
    /// `openai`: the error is the body's `error` member.
    #[default]
    OpenAI,
    /// `@anthropic-ai/sdk`: the error is the whole parsed body.
    Anthropic,
}

fn lowercase_headers(headers: &reqwest::header::HeaderMap) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for (name, value) in headers {
        let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
        map.entry(name.as_str().to_owned())
            .and_modify(|existing: &mut String| {
                existing.push_str(", ");
                existing.push_str(&value);
            })
            .or_insert(value);
    }
    map
}

async fn send_once(
    request: &HttpRequest,
    timeout: Duration,
    signal: Option<&AbortSignal>,
) -> Result<reqwest::Response, ProviderError> {
    let client = client()?;
    let mut builder = client.post(&request.url).body(request.body.clone());
    for (name, value) in &request.headers {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| ProviderError::other(format!("Invalid request header name: {name:?}")))?;
        let value = reqwest::header::HeaderValue::from_str(value).map_err(|_| {
            ProviderError::other(format!("Invalid value for request header {name}"))
        })?;
        builder = builder.header(name, value);
    }
    let send = builder.send();
    let response = tokio::select! {
        result = tokio::time::timeout(timeout, send) => match result {
            Err(_) => return Err(ProviderError::timeout()),
            Ok(Err(error)) if error.is_timeout() => return Err(ProviderError::timeout()),
            Ok(Err(error)) if error.is_builder() => {
                return Err(ProviderError::other(format!("Invalid request: {}", error_chain(&error))));
            }
            Ok(Err(error)) => return Err(ProviderError::connection(&error_chain(&error))),
            Ok(Ok(response)) => response,
        },
        () = cancelled(signal) => return Err(ProviderError::aborted()),
    };
    let status = response.status().as_u16();
    if response.status().is_success() {
        return Ok(response);
    }
    let headers = lowercase_headers(response.headers());
    let body = tokio::select! {
        body = response.text() => body.unwrap_or_else(|error| error_chain(&error)),
        () = cancelled(signal) => return Err(ProviderError::aborted()),
    };
    Err(match request.error_shape {
        SdkErrorShape::OpenAI => ProviderError::http(status, headers, &body),
        SdkErrorShape::Anthropic => ProviderError::anthropic_http(status, headers, &body),
    })
}

/// Sends `request` with the options' timeout, signal, and retries, and
/// returns the 2xx response with its body unread.
pub async fn send(
    request: &HttpRequest,
    options: &StreamOptions,
) -> Result<SseResponse, ProviderError> {
    if is_aborted(options.signal.as_ref()) {
        return Err(ProviderError::aborted());
    }
    let timeout = Duration::from_millis(options.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let retry = ProviderRetryOptions {
        max_retries: options.max_retries,
        max_retry_delay_ms: options.max_retry_delay_ms,
        signal: options.signal.clone(),
        no_retry_statuses: Vec::new(),
    };
    let response = retry_provider_request(
        || send_once(request, timeout, options.signal.as_ref()),
        &retry,
    )
    .await?;
    Ok(SseResponse {
        info: ProviderResponse {
            status: response.status().as_u16(),
            headers: lowercase_headers(response.headers()),
        },
        response,
        decoder: SseDecoder::new(),
        finished: false,
        signal: options.signal.clone(),
    })
}

/// A 2xx response whose body is read as server-sent events.
pub struct SseResponse {
    /// Status and headers.
    pub info: ProviderResponse,
    response: reqwest::Response,
    decoder: SseDecoder,
    finished: bool,
    signal: Option<AbortSignal>,
}

impl SseResponse {
    /// The events the next body chunk completes; `Ok(None)` after the body
    /// ended and its trailing events were returned.
    pub async fn next_events(&mut self) -> Result<Option<Vec<ServerSentEvent>>, ProviderError> {
        if self.finished {
            return Ok(None);
        }
        let chunk = tokio::select! {
            chunk = self.response.chunk() => chunk,
            () = cancelled(self.signal.as_ref()) => return Err(ProviderError::aborted()),
        };
        match chunk {
            Ok(Some(bytes)) => Ok(Some(self.decoder.push(&bytes))),
            Ok(None) => {
                self.finished = true;
                Ok(Some(self.decoder.finish()))
            }
            Err(error) => Err(ProviderError::other(format!(
                "Network error: {}",
                error_chain(&error)
            ))),
        }
    }
}

/// Joins `path` onto `base_url` as the SDKs do: one slash between them.
pub fn join_url(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_headers_case_insensitively_and_removes_nulls() {
        let defaults: ProviderHeaders = vec![
            ("Accept".into(), Some("application/json".into())),
            ("User-Agent".into(), Some("ua".into())),
            ("x-remove".into(), Some("1".into())),
        ];
        let overrides: ProviderHeaders = vec![
            ("user-agent".into(), Some("custom".into())),
            ("X-Remove".into(), None),
        ];
        assert_eq!(
            merge_headers(&[&defaults, &overrides]),
            vec![
                ("Accept".to_owned(), "application/json".to_owned()),
                ("user-agent".to_owned(), "custom".to_owned())
            ]
        );
    }

    #[test]
    fn joins_urls() {
        assert_eq!(
            join_url("http://h/v1/", "/chat/completions"),
            "http://h/v1/chat/completions"
        );
        assert_eq!(
            join_url("http://h", "v1/messages?beta=true"),
            "http://h/v1/messages?beta=true"
        );
    }
}
