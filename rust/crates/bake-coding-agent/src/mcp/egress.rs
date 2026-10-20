//! D24 network admission for one Streamable HTTP MCP server: the URL and
//! headers `mcp.json` configured, the addresses one connection may use, and
//! the HTTP client that reaches only them.
//!
//! The owner's `mcp.json` authorizes exactly the configured origin (scheme,
//! host, port), as a stdio entry's `command` authorizes that program, so a
//! loopback or private address is allowed: a local MCP server is the common
//! case, and both Pi v1.3.0 (`packages/mcp/src/transports/streamable-http.ts`,
//! which checks no address) and Bake 0.3 (`packages/mcp/mcp-client`, whose
//! `url` is any string) connect to one. What the admission adds around that
//! origin is Bake's own:
//!
//! - **One resolution per connection, pinned.** The host is resolved once,
//!   on Tokio's blocking pool under the session's [`WorkGate`], and the
//!   connection's client answers every name with those addresses
//!   ([`pinned_client_builder`]), so no later lookup can move it: DNS
//!   rebinding reaches nothing. A reconnect is a new connection and resolves
//!   again (`super::http`).
//! - **Same-origin redirects only, within a small bound.** A hop to another
//!   origin is refused before it is sent, so the configured headers cannot
//!   reach an origin the owner did not authorize, and at most
//!   `MAX_REDIRECTS` (three) same-origin hops are followed.
//! - **`https` for a public destination.** Plain `http` is allowed to a
//!   loopback or otherwise non-public address, which is what Pi and Bake 0.3
//!   allow and what a local server needs, and refused when the host resolves
//!   to a globally reachable address, where the configured `Authorization`
//!   header would cross the network in clear text. The classification is the
//!   web tools' ([`crate::web::policy::is_public_ip`]).
//! - **No proxy.** The client connects directly and reads neither
//!   `HTTPS_PROXY` nor `NO_PROXY`. Bake 0.3 sent MCP HTTP through undici's
//!   global dispatcher, which `installProxyFromEnvironment` built from those
//!   variables (`packages/util/http-proxy/src/install.ts`); a proxy resolves
//!   the origin itself, so a proxied request cannot be pinned, and Bake's
//!   Rust HTTP clients, the web tools included
//!   (`packages/web/web-fetch-http` in Rust), connect directly for that
//!   reason. 0.4 therefore does not read the proxy environment for MCP.
//! - **No ambient credentials.** Only the entry's headers are sent, with
//!   their values resolved as other configuration values are: `${NAME}`
//!   references the environment and `!cmd` is refused, as for a stdio
//!   entry's `env`. Header values never appear in an error, a log, or
//!   `Debug` output: a refusal names a header, and `bake-mcp` redacts the
//!   configured values, the URL, and the session ID from server text.
//!
//! The connection is made by the `bake-rs` process, not by a sandboxed
//! child, so it is allowed in every sandbox mode, as the web tools are
//! (D40): the owner configured the origin, and `bake-sandbox` has no network
//! policy for it to bypass.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use bake_mcp::{HttpOptions, Reconnect};

use super::McpHttpOptions;
use super::config::McpHttpConfig;
use crate::config_value::{
    is_command_config_value, missing_config_value_env_var_names, resolve_config_value_uncached,
};
use crate::owned_work::WorkGate;
use crate::web::fetch::{ResolveFailure, pinned_client_builder, resolve_addresses, system_lookup};
use crate::web::policy::{is_public_ip, is_same_origin, parse_fetch_url};

/// The most same-origin redirects one request follows.
pub(crate) const MAX_REDIRECTS: usize = 3;

/// How long [`admit`] waits for one host resolution: the system lookup has
/// no deadline of its own, and a connection attempt that cannot resolve
/// must end so the next one can be tried. The lookup itself cannot be
/// cancelled, so the session's gate still waits for it, as the web
/// fetcher's does.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(20);

/// Resolves a host to the addresses a connection may use, as
/// [`system_lookup`] does. A session may supply its own
/// ([`super::McpOptions::resolver`]); tests inject one to change what a
/// name answers between connections.
pub type HostResolver = Arc<dyn Fn(&str) -> Result<Vec<IpAddr>, String> + Send + Sync>;

/// The system resolver every session uses unless it supplies another.
pub fn system_resolver() -> HostResolver {
    Arc::new(|host: &str| system_lookup(host))
}

/// Why a server was not admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AdmissionError {
    /// The configuration itself is not admissible; nothing is retried.
    Refused(String),
    /// The host could not be resolved now; a later attempt may succeed.
    Unresolved(String),
}

/// What one admitted connection runs with. `Debug` shows the transport's
/// options, which show the URL's origin and the header names only.
#[derive(Debug)]
pub(crate) struct Admitted {
    /// The transport's options, with the resolved headers.
    pub(crate) options: HttpOptions,
    /// The client that reaches only the pinned addresses of the configured
    /// origin.
    pub(crate) client: reqwest::Client,
}

/// Why a redirect was not followed. Names the target's origin only, never a
/// path, a query, or a header.
#[derive(Debug)]
enum RedirectRefused {
    /// The hop leaves the origin `mcp.json` authorized.
    CrossOrigin(String),
    /// More hops than [`MAX_REDIRECTS`].
    TooMany,
}

impl std::fmt::Display for RedirectRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CrossOrigin(origin) => write!(
                f,
                "a redirect to {origin} is not followed: mcp.json authorizes only the configured origin"
            ),
            Self::TooMany => write!(f, "more than {MAX_REDIRECTS} redirects"),
        }
    }
}

impl std::error::Error for RedirectRefused {}

/// Admit one connection to `config`: resolve and pin its host, check its
/// scheme against what that resolution reaches, resolve its headers, and
/// build the client and transport options. `work` owns the lookup, so a
/// session's shutdown waits for it.
pub(crate) async fn admit(
    name: &str,
    config: &McpHttpConfig,
    settings: &McpHttpOptions,
    resolver: &HostResolver,
    work: &WorkGate,
) -> Result<Admitted, AdmissionError> {
    let refused = |reason: String| AdmissionError::Refused(reason);
    let url = parse_fetch_url(&config.url)
        .map_err(|error| refused(format!("MCP server \"{name}\": {}", error.message)))?;
    let Some(host) = url.host_str() else {
        return Err(refused(format!(
            "MCP server \"{name}\": the URL names no host"
        )));
    };
    let headers = headers(name, config).map_err(refused)?;
    let addresses = resolve(host, resolver, work).await.map_err(|failure| {
        let detail = match failure {
            ResolveFailure::Lookup(error) => error,
            ResolveFailure::NoAddresses => "it resolved to no addresses".to_owned(),
        };
        AdmissionError::Unresolved(format!(
            "MCP server \"{name}\" could not resolve its host: {detail}"
        ))
    })?;
    if url.scheme() == "http" && addresses.iter().copied().any(is_public_ip) {
        return Err(refused(format!(
            "MCP server \"{name}\" uses http to a public address, which would send its headers in clear text; use https, or a loopback or private address"
        )));
    }
    let authorized = url.clone();
    let client = pinned_client_builder(&addresses)
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() > MAX_REDIRECTS {
                return attempt.error(RedirectRefused::TooMany);
            }
            if is_same_origin(attempt.url(), &authorized) {
                attempt.follow()
            } else {
                let origin = attempt.url().origin().ascii_serialization();
                attempt.error(RedirectRefused::CrossOrigin(origin))
            }
        }))
        .referer(false)
        .http1_only()
        .connect_timeout(settings.connect_timeout)
        .build()
        .map_err(|error| {
            refused(format!(
                "MCP server \"{name}\" could not start its HTTP client: {}",
                crate::web::source_chain(&error)
            ))
        })?;
    let mut options = HttpOptions::new(url.to_string());
    options.headers = headers;
    options.limits.max_message_bytes = settings.max_message_bytes;
    options.connect_timeout = settings.connect_timeout;
    // The transport's own stream resumption keeps Pi's bounds; the
    // connection's budget is `super::http`'s.
    options.reconnect = Reconnect::default();
    Ok(Admitted { options, client })
}

/// The entry's headers with their values resolved. A value that runs a
/// command or names an unset variable is refused; neither the refusal nor
/// any later error carries a value.
fn headers(name: &str, config: &McpHttpConfig) -> Result<Vec<(String, String)>, String> {
    let mut resolved = Vec::with_capacity(config.headers.len());
    for (key, value) in &config.headers {
        if is_command_config_value(value) {
            return Err(format!(
                "MCP server \"{name}\" headers \"{key}\" runs a command, which Bake does not run"
            ));
        }
        let missing = missing_config_value_env_var_names(value, None);
        if !missing.is_empty() {
            return Err(format!(
                "MCP server \"{name}\" headers \"{key}\" references unset variables: {}",
                missing.join(", ")
            ));
        }
        resolved.push((
            key.clone(),
            resolve_config_value_uncached(value, None).unwrap_or_default(),
        ));
    }
    Ok(resolved)
}

/// Resolve `host` on the blocking pool, under a permit of the session's
/// gate: shutdown waits for a lookup its connection no longer needs.
async fn resolve(
    host: &str,
    resolver: &HostResolver,
    work: &WorkGate,
) -> Result<Vec<IpAddr>, ResolveFailure> {
    let permit = work
        .enter()
        .ok_or_else(|| ResolveFailure::Lookup("the session is shutting down".to_owned()))?;
    let resolver = Arc::clone(resolver);
    let owned = host.to_owned();
    let lookup = tokio::task::spawn_blocking(move || {
        let resolved = resolve_addresses(&owned, &|host| resolver(host));
        drop(permit);
        resolved
    });
    match tokio::time::timeout(RESOLVE_TIMEOUT, lookup).await {
        Ok(Ok(resolved)) => resolved,
        Ok(Err(error)) => Err(ResolveFailure::Lookup(error.to_string())),
        Err(_) => Err(ResolveFailure::Lookup(format!(
            "the lookup did not answer within {RESOLVE_TIMEOUT:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroUsize;

    fn config(url: &str, headers: &[(&str, &str)]) -> McpHttpConfig {
        McpHttpConfig {
            url: url.to_owned(),
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        }
    }

    fn resolver(addresses: &'static [&'static str]) -> HostResolver {
        Arc::new(move |_host: &str| {
            Ok(addresses
                .iter()
                .map(|address| address.parse().expect("address"))
                .collect())
        })
    }

    async fn admit_url(
        url: &str,
        headers: &[(&str, &str)],
        resolver: &HostResolver,
    ) -> Result<Admitted, AdmissionError> {
        admit(
            "s",
            &config(url, headers),
            &McpHttpOptions::default(),
            resolver,
            &WorkGate::default(),
        )
        .await
    }

    fn reason(error: AdmissionError) -> String {
        match error {
            AdmissionError::Refused(reason) | AdmissionError::Unresolved(reason) => reason,
        }
    }

    // The configured origin is authorized, whatever address it names: a
    // loopback or private server is admitted over plain http, as Pi and
    // Bake 0.3 admit one.
    #[tokio::test]
    async fn a_private_destination_is_admitted_over_http() {
        let loopback = resolver(&["127.0.0.1"]);
        let admitted = admit_url("http://localhost:7331/mcp", &[], &loopback)
            .await
            .expect("admitted");
        assert_eq!(admitted.options.url, "http://localhost:7331/mcp");
        let private = resolver(&["10.1.2.3"]);
        admit_url("http://mcp.internal/mcp", &[], &private)
            .await
            .expect("private");
        let public = resolver(&["93.184.216.34"]);
        admit_url("https://mcp.example.com/mcp", &[], &public)
            .await
            .expect("https to a public address");
    }

    // Bake's own rule, stricter than Pi and 0.3: clear text to a globally
    // reachable address would expose the configured headers.
    #[tokio::test]
    async fn http_to_a_public_address_is_refused() {
        let public = resolver(&["93.184.216.34"]);
        let error = admit_url("http://mcp.example.com/mcp", &[], &public)
            .await
            .expect_err("refused");
        assert!(reason(error).contains("would send its headers in clear text"));
        // A public address among private ones is still public.
        let mixed = resolver(&["127.0.0.1", "93.184.216.34"]);
        assert!(
            admit_url("http://mcp.example.com/mcp", &[], &mixed)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn an_unusable_url_is_refused_and_a_failed_lookup_is_not() {
        let loopback = resolver(&["127.0.0.1"]);
        for (url, expected) in [
            ("ftp://example.com/mcp", "unsupported URL scheme"),
            ("not a url", "invalid URL"),
            (
                "http://user:secret@127.0.0.1/mcp",
                "credentials in URLs are not allowed",
            ),
        ] {
            let error = admit_url(url, &[], &loopback).await.expect_err(url);
            assert!(matches!(error, AdmissionError::Refused(_)), "{url}");
            assert!(reason(error).contains(expected), "{url}");
        }
        let failing: HostResolver = Arc::new(|_| Err("no such host".to_owned()));
        let error = admit_url("https://mcp.example.com/mcp", &[], &failing)
            .await
            .expect_err("unresolved");
        assert!(matches!(error, AdmissionError::Unresolved(_)));
        assert!(reason(error).contains("no such host"));
        let empty: HostResolver = Arc::new(|_| Ok(Vec::new()));
        let error = admit_url("https://mcp.example.com/mcp", &[], &empty)
            .await
            .expect_err("no addresses");
        assert!(reason(error).contains("resolved to no addresses"));
    }

    // Header values are configuration values, as a stdio entry's `env` is:
    // `!cmd` and an unset reference are refused, and no refusal shows a
    // value.
    #[tokio::test]
    async fn header_values_resolve_and_never_appear_in_a_refusal() {
        let loopback = resolver(&["127.0.0.1"]);
        // SAFETY: the test process sets its own variable before the lookup.
        unsafe { std::env::set_var("BAKE_MCP_EGRESS_TEST_TOKEN", "secret-token") };
        let admitted = admit_url(
            "http://127.0.0.1:7331/mcp",
            &[("Authorization", "Bearer ${BAKE_MCP_EGRESS_TEST_TOKEN}")],
            &loopback,
        )
        .await
        .expect("admitted");
        assert_eq!(
            admitted.options.headers,
            [("Authorization".to_owned(), "Bearer secret-token".to_owned())]
        );
        let shown = format!("{:?}", admitted.options);
        assert!(!shown.contains("secret-token"), "{shown}");
        let error = refusal(&[("Authorization", "!print-the-token")], &loopback).await;
        assert!(
            error.contains("headers \"Authorization\" runs a command"),
            "{error}"
        );
        assert!(!error.contains("print-the-token"), "{error}");
        let error = refusal(
            &[("Authorization", "Bearer ${BAKE_MCP_EGRESS_TEST_UNSET}")],
            &loopback,
        )
        .await;
        assert!(
            error.contains("references unset variables: BAKE_MCP_EGRESS_TEST_UNSET"),
            "{error}"
        );
    }

    async fn refusal(headers: &[(&str, &str)], resolver: &HostResolver) -> String {
        reason(
            admit_url("http://127.0.0.1:7331/mcp", headers, resolver)
                .await
                .expect_err("refused"),
        )
    }

    #[tokio::test]
    async fn the_message_limit_comes_from_the_session() {
        let settings = McpHttpOptions {
            max_message_bytes: NonZeroUsize::new(4_096).expect("limit"),
            ..McpHttpOptions::default()
        };
        let admitted = admit(
            "s",
            &config("http://127.0.0.1:7331/mcp", &[]),
            &settings,
            &resolver(&["127.0.0.1"]),
            &WorkGate::default(),
        )
        .await
        .expect("admitted");
        assert_eq!(admitted.options.limits.max_message_bytes.get(), 4_096);
    }
}
