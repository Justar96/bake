//! Anonymous public HTTP(S) retrieval for `web_fetch`: validated and
//! pinned public destinations, same-origin redirects only, time and size
//! bounds, and text decoding. Requests carry no cookies or ambient
//! credentials.
//!
//! Ported from `packages/web/web-fetch-http/src/{provider,network,index}.ts`
//! (Bake 0.3): `HttpFetchProvider`, `resolvePublicAddresses`, and the
//! plugin's default limits. Every message is the TypeScript's.
//!
//! Each hostname is resolved once, on Tokio's blocking pool and owned by
//! the session's [`WorkGate`] when one is given, and the whole answer set
//! is refused when any address is not public, including an IPv6 answer
//! that an active DNS64 prefix translates to a non-public IPv4. The
//! connection is then made by a client of this request's own whose
//! resolver returns only those addresses, so the hostname is never
//! resolved again and DNS rebinding cannot reach another address. Every
//! redirect hop is validated and resolved the same way; a cross-origin
//! redirect is refused, so a new origin needs a new tool call.
//!
//! Differences from the TypeScript: no proxy route (Bake's Rust HTTP
//! clients connect directly, `no_proxy`); a failed DNS64 discovery lookup
//! counts as no active prefix instead of failing the fetch; and the
//! character cap counts UTF-16 code units, as the TypeScript did, cutting
//! only between characters.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use bake_ai::AbortSignal;
use reqwest::Url;

use super::policy::{
    FetchableKind, IPV4ONLY_DISCOVERY_HOST, classify_content_type, encoding_for_charset,
    is_public_ip, is_same_origin, nat64_prefixes, parse_charset, strip_ipv6_brackets,
    translated_ipv4, validate_fetch_url,
};
use super::{
    WEB_ABORTED, WEB_BLOCKED_URL, WEB_FETCH_TIMEOUT, WEB_FETCH_TOO_LARGE, WEB_PROVIDER_ERROR,
    WEB_REDIRECT_BLOCKED, WEB_UNSUPPORTED_CONTENT_TYPE, WebError,
};
use crate::owned_work::WorkGate;

/// The `User-Agent` every request sends: the product, never a browser.
pub const DEFAULT_USER_AGENT: &str = concat!("bake/", env!("CARGO_PKG_VERSION"));

/// The `Accept` header every request sends.
const ACCEPT: &str = "text/html,application/xhtml+xml,text/*;q=0.9,application/json;q=0.8";

/// The fetcher's limits (`HttpFetchLimits`), with the plugin's defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpFetchLimits {
    /// The largest response body read, in bytes; a larger declared length
    /// is refused and a longer stream cut.
    pub max_response_bytes: u64,
    /// The longest decoded body kept, in UTF-16 code units.
    pub max_body_chars: usize,
    /// The bound on one fetch, redirects and body included.
    pub timeout: Duration,
    /// The most same-origin redirects followed.
    pub max_redirects: u32,
    /// The `User-Agent` header.
    pub user_agent: String,
}

impl Default for HttpFetchLimits {
    fn default() -> Self {
        Self {
            max_response_bytes: 5_000_000,
            max_body_chars: 100_000,
            timeout: Duration::from_secs(30),
            max_redirects: 5,
            user_agent: DEFAULT_USER_AGENT.to_owned(),
        }
    }
}

/// The decoded body of a fetched resource (`WebFetchBody`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebFetchBody {
    /// An HTML document.
    Html(String),
    /// Any other text.
    Text(String),
}

impl WebFetchBody {
    /// `html` or `text`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Html(_) => "html",
            Self::Text(_) => "text",
        }
    }

    /// The decoded text.
    pub fn content(&self) -> &str {
        match self {
            Self::Html(content) | Self::Text(content) => content,
        }
    }
}

/// One fetch's outcome (`WebFetchResult`). A non-2xx status is a result,
/// not an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebFetchResult {
    /// The final URL, after the redirects followed.
    pub url: String,
    /// The final response's status.
    pub status_code: u16,
    /// The decoded body.
    pub body: WebFetchBody,
    /// Whether the byte or character cap cut the body.
    pub truncated: bool,
}

/// Resolves a URL host to the validated addresses a connection may use
/// (`HttpFetchResolver`). It runs on the blocking pool.
pub type AddressResolver = Arc<dyn Fn(&str) -> Result<Vec<IpAddr>, WebError> + Send + Sync>;

/// System DNS through `getaddrinfo`, in its answer order.
pub fn system_lookup(host: &str) -> Result<Vec<IpAddr>, String> {
    (host, 0u16)
        .to_socket_addrs()
        .map(|addresses| addresses.map(|address| address.ip()).collect())
        .map_err(|error| error.to_string())
}

/// `resolvePublicAddresses`: resolve `host` once with `lookup`, a literal
/// without asking it, and refuse the whole answer set when any address is
/// not public. The addresses returned are the only ones a connection may
/// use.
pub fn resolve_public_addresses(
    host: &str,
    lookup: &dyn Fn(&str) -> Result<Vec<IpAddr>, String>,
) -> Result<Vec<IpAddr>, WebError> {
    let unbracketed = strip_ipv6_brackets(host);
    let resolved = match unbracketed.parse::<IpAddr>() {
        Ok(literal) => vec![literal],
        Err(_) => lookup(unbracketed).map_err(|error| {
            WebError::new(WEB_PROVIDER_ERROR, format!("web fetch failed: {error}"))
        })?,
    };
    if resolved.is_empty() {
        return Err(WebError::new(
            WEB_PROVIDER_ERROR,
            format!("hostname \"{host}\" resolved to no addresses"),
        ));
    }
    let prefixes = if resolved.iter().any(IpAddr::is_ipv6) {
        nat64_prefixes(&lookup(IPV4ONLY_DISCOVERY_HOST).unwrap_or_default())
    } else {
        Vec::new()
    };
    for address in &resolved {
        if !is_public_ip(*address) {
            return Err(WebError::new(
                WEB_BLOCKED_URL,
                format!("URL hostname \"{host}\" resolves to a non-public IP address"),
            ));
        }
        if translated_ipv4(*address, &prefixes).is_some_and(|ipv4| !is_public_ip(ipv4.into())) {
            return Err(WebError::new(
                WEB_BLOCKED_URL,
                format!(
                    "URL hostname \"{host}\" resolves through NAT64 to a non-public IPv4 address"
                ),
            ));
        }
    }
    Ok(resolved)
}

/// A resolver that answers every name with one validated address set.
struct PinnedResolver(Vec<SocketAddr>);

impl reqwest::dns::Resolve for PinnedResolver {
    fn resolve(&self, _name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let addresses: reqwest::dns::Addrs = Box::new(self.0.clone().into_iter());
        Box::pin(std::future::ready(Ok(addresses)))
    }
}

/// The anonymous public HTTP(S) fetcher (`HttpFetchProvider`).
#[derive(Clone)]
pub struct HttpFetcher {
    limits: HttpFetchLimits,
    resolver: AddressResolver,
    owner: Option<WorkGate>,
}

impl std::fmt::Debug for HttpFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpFetcher")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

fn aborted() -> WebError {
    WebError::new(WEB_ABORTED, "web fetch aborted")
}

fn failed(error: &dyn std::error::Error) -> WebError {
    WebError::new(
        WEB_PROVIDER_ERROR,
        format!("web fetch failed: {}", super::source_chain(error)),
    )
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

impl HttpFetcher {
    /// A fetcher over system DNS whose lookups `owner`, the session's
    /// gate, owns.
    pub fn new(limits: HttpFetchLimits, owner: Option<WorkGate>) -> Self {
        Self::with_resolver(
            limits,
            Arc::new(|host: &str| resolve_public_addresses(host, &system_lookup)),
            owner,
        )
    }

    /// A fetcher whose `resolver` supplies the validated addresses.
    pub fn with_resolver(
        limits: HttpFetchLimits,
        resolver: AddressResolver,
        owner: Option<WorkGate>,
    ) -> Self {
        Self {
            limits,
            resolver,
            owner,
        }
    }

    /// The limits.
    pub fn limits(&self) -> &HttpFetchLimits {
        &self.limits
    }

    /// Fetch `url`. Aborting `signal` ends the fetch at once with
    /// `web fetch aborted`; the time bound ends it with `web fetch timed
    /// out`. Dropping the future closes its connection.
    pub async fn fetch(
        &self,
        url: &str,
        signal: Option<&AbortSignal>,
    ) -> Result<WebFetchResult, WebError> {
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
            result = tokio::time::timeout(self.limits.timeout, self.follow_and_read(url)) => {
                result.unwrap_or_else(|_| Err(WebError::new(WEB_FETCH_TIMEOUT, "web fetch timed out")))
            }
        }
    }

    async fn follow_and_read(&self, initial: &str) -> Result<WebFetchResult, WebError> {
        let mut current = validate_fetch_url(initial)?;
        let mut redirects = 0u32;
        loop {
            let response = self.request_once(&current).await?;
            let status = response.status().as_u16();
            if !is_redirect(status) {
                return self.read_body(response, current).await;
            }
            // The budget is checked before the next hop is looked at.
            if redirects >= self.limits.max_redirects {
                return Err(WebError::new(
                    WEB_REDIRECT_BLOCKED,
                    format!(
                        "exceeded the maximum of {} redirects",
                        self.limits.max_redirects
                    ),
                ));
            }
            let Some(location) = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
            else {
                return Err(WebError::new(
                    WEB_PROVIDER_ERROR,
                    format!("redirect response (HTTP {status}) without a Location header"),
                ));
            };
            let target = current.join(location).map_err(|_| {
                WebError::new(
                    WEB_PROVIDER_ERROR,
                    format!("invalid redirect Location \"{location}\""),
                )
            })?;
            // A redirect gets the checks a direct request gets.
            let target = validate_fetch_url(target.as_str())?;
            if !is_same_origin(&target, &current) {
                return Err(WebError::new(
                    WEB_REDIRECT_BLOCKED,
                    format!(
                        "cross-origin redirect to {} is not followed automatically; retry against that URL directly",
                        target.origin().ascii_serialization()
                    ),
                ));
            }
            drop(response);
            current = target;
            redirects += 1;
        }
    }

    /// Resolve and validate `host` on the blocking pool, owned by the
    /// session's gate: shutdown waits for a lookup a cancelled fetch left
    /// running.
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, WebError> {
        let permit = match &self.owner {
            Some(owner) => Some(owner.enter().ok_or_else(aborted)?),
            None => None,
        };
        let resolver = Arc::clone(&self.resolver);
        let host = host.to_owned();
        tokio::task::spawn_blocking(move || {
            let result = resolver(&host);
            drop(permit);
            result
        })
        .await
        .map_err(|error| failed(&error))?
    }

    async fn request_once(&self, url: &Url) -> Result<reqwest::Response, WebError> {
        let host = url.host_str().unwrap_or_default();
        let addresses = self.resolve(host).await?;
        let pinned = addresses
            .into_iter()
            .map(|address| SocketAddr::new(address, 0))
            .collect();
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .dns_resolver(Arc::new(PinnedResolver(pinned)))
            .build()
            .map_err(|error| failed(&error))?;
        client
            .get(url.clone())
            .header(reqwest::header::USER_AGENT, &self.limits.user_agent)
            .header(reqwest::header::ACCEPT, ACCEPT)
            .send()
            .await
            .map_err(|error| failed(&error))
    }

    /// Classify, byte-cap, decode, and character-cap the final response.
    async fn read_body(
        &self,
        mut response: reqwest::Response,
        url: Url,
    ) -> Result<WebFetchResult, WebError> {
        let status_code = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned());
        let Some(kind) = classify_content_type(content_type.as_deref()) else {
            return Err(WebError::new(
                WEB_UNSUPPORTED_CONTENT_TYPE,
                format!(
                    "unsupported content type \"{}\"",
                    content_type.as_deref().unwrap_or("unknown")
                ),
            ));
        };
        // Before reading, so an unsupported charset costs no body.
        let encoding = encoding_for_charset(parse_charset(content_type.as_deref()).as_deref())?;
        let max = self.limits.max_response_bytes;
        if response.content_length().is_some_and(|length| length > max) {
            return Err(WebError::new(
                WEB_FETCH_TOO_LARGE,
                format!("response exceeds the maximum of {max} bytes"),
            ));
        }
        let cap = usize::try_from(max).unwrap_or(usize::MAX);
        let mut bytes: Vec<u8> = Vec::new();
        let mut truncated_by_bytes = false;
        // Only dropped bytes count as truncation: a body that exactly
        // fills the cap is read to its end and kept whole.
        while let Some(chunk) = response.chunk().await.map_err(|error| failed(&error))? {
            let remaining = cap - bytes.len();
            if chunk.len() > remaining {
                bytes.extend_from_slice(&chunk[..remaining]);
                truncated_by_bytes = true;
                break;
            }
            bytes.extend_from_slice(&chunk);
        }
        drop(response);
        let (decoded, _) = encoding.decode_with_bom_removal(&bytes);
        let (content, truncated_by_chars) =
            super::truncate_utf16(&decoded, self.limits.max_body_chars);
        let content = content.to_owned();
        Ok(WebFetchResult {
            url: url.to_string(),
            status_code,
            body: match kind {
                FetchableKind::Html => WebFetchBody::Html(content),
                FetchableKind::Text => WebFetchBody::Text(content),
            },
            truncated: truncated_by_bytes || truncated_by_chars,
        })
    }
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
