//! Reading the proxy's catalog: `fetchCliProxyModels`, `CliProxyCheckFailure`,
//! `CliProxyCheckError`, and `cliProxyFailureText` in
//! `apps/tui/packages/app/src/cliproxyapi.ts`.
//!
//! `GET <root>/v1/models?client_version=pi` with the bearer key, no
//! redirects followed, a 15-second bound, a 4 MiB body cap, and the caller's
//! [`AbortSignal`]. The request runs on reqwest over rustls, the client
//! `bake-ai` uses, built here with redirects refused (`redirect: 'error'`).
//! Nothing is spawned: dropping or aborting the returned future drops the
//! connection, so no task outlives the caller.

use std::fmt;
use std::sync::OnceLock;
use std::time::Duration;

use bake_ai::AbortSignal;
use reqwest::Url;
use serde_json::Value;

use super::catalog::{CliProxyModel, cli_proxy_models};

/// The largest catalog body read (`MAX_CATALOG_BYTES`).
pub const MAX_CATALOG_BYTES: usize = 4 * 1024 * 1024;
/// How long a catalog request may take (`CATALOG_TIMEOUT_MS`).
pub const CATALOG_TIMEOUT: Duration = Duration::from_secs(15);

/// Why a proxy did not validate, and so which field a sign-in asks for
/// again: a refused key is the key's fault, everything else the address's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliProxyCheckFailure {
    /// The connection failed; `detail` is a Node-style error code where one
    /// applies, such as `ECONNREFUSED`.
    Unreachable {
        /// `host[:port]`.
        host: String,
        /// What failed.
        detail: String,
    },
    /// No answer within 15 seconds.
    Timeout {
        /// `host[:port]`.
        host: String,
    },
    /// The proxy answered with a redirect.
    Redirect,
    /// HTTP 401 or 403.
    Rejected {
        /// The status.
        status: u16,
    },
    /// HTTP 404.
    NotFound,
    /// Any other status but 200.
    Status {
        /// The status.
        status: u16,
    },
    /// The body is not a model list.
    NotProxy,
    /// The body exceeds [`MAX_CATALOG_BYTES`].
    TooLarge,
    /// The list has no chat model.
    Empty,
}

/// The field a sign-in asks for again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckField {
    /// The proxy address.
    Url,
    /// The API key.
    Key,
}

impl CliProxyCheckFailure {
    /// `CliProxyCheckError.field`.
    pub fn field(&self) -> CheckField {
        match self {
            Self::Rejected { .. } => CheckField::Key,
            _ => CheckField::Url,
        }
    }

    /// `cliProxyFailureText` with the English labels of
    /// `apps/tui/packages/ui/src/copy.ts`: one line naming what went wrong.
    pub fn text(&self) -> String {
        match self {
            Self::Unreachable { host, detail } => format!("Could not reach {host} ({detail})"),
            Self::Timeout { host } => format!("No answer within 15 seconds from {host}"),
            Self::Redirect => {
                "The address redirects elsewhere; enter the address it redirects to".into()
            }
            Self::Rejected { status } => {
                format!("The proxy rejected this API key (HTTP {status})")
            }
            Self::NotFound => "No CLIProxyAPI model list at this address (HTTP 404)".into(),
            Self::Status { status } => format!("The proxy answered HTTP {status}"),
            Self::NotProxy => "This address did not answer with a CLIProxyAPI model list".into(),
            Self::TooLarge => "The proxy's model list is too large to read".into(),
            Self::Empty => {
                "The proxy lists no chat models yet; add an upstream account to it, then try again"
                    .into()
            }
        }
    }
}

/// A proxy that did not validate (`CliProxyCheckError`). The message is
/// English for logs and never holds the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliProxyCheckError {
    /// Why.
    pub failure: CliProxyCheckFailure,
    /// The TypeScript's message.
    pub message: String,
}

impl CliProxyCheckError {
    fn new(failure: CliProxyCheckFailure, message: impl Into<String>) -> Self {
        Self {
            failure,
            message: message.into(),
        }
    }

    /// The field a sign-in asks for again.
    pub fn field(&self) -> CheckField {
        self.failure.field()
    }
}

impl fmt::Display for CliProxyCheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliProxyCheckError {}

/// How a catalog read ended without models.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchCliProxyModelsError {
    /// The caller's signal aborted. The TypeScript rethrows the caller's
    /// own abort instead of reporting a check failure.
    Aborted,
    /// The models URL does not parse (`new URL` throws).
    InvalidUrl,
    /// The proxy did not validate.
    Check(CliProxyCheckError),
}

impl fmt::Display for FetchCliProxyModelsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Aborted => f.write_str("The CLIProxyAPI model request was aborted"),
            Self::InvalidUrl => f.write_str("Invalid URL"),
            Self::Check(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for FetchCliProxyModelsError {}

impl From<CliProxyCheckError> for FetchCliProxyModelsError {
    fn from(error: CliProxyCheckError) -> Self {
        Self::Check(error)
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
                .map_err(|error| source_chain(&error))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// An error's sources joined, without reqwest's own line, which names the
/// URL.
fn source_chain(error: &dyn std::error::Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut source = error.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !parts.iter().any(|part| part.contains(&text)) {
            parts.push(text);
        }
        source = cause.source();
    }
    if parts.is_empty() {
        "fetch failed".into()
    } else {
        parts.join(": ")
    }
}

/// The transport failure's detail, as undici's `cause.code` names it where
/// the OS error has a Node code, else the cause's message.
fn transport_detail(error: &reqwest::Error) -> String {
    let mut source: Option<&dyn std::error::Error> = std::error::Error::source(error);
    while let Some(cause) = source {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            use std::io::ErrorKind;
            let code = match io.kind() {
                ErrorKind::ConnectionRefused => Some("ECONNREFUSED"),
                ErrorKind::ConnectionReset => Some("ECONNRESET"),
                ErrorKind::ConnectionAborted => Some("ECONNABORTED"),
                ErrorKind::TimedOut => Some("ETIMEDOUT"),
                ErrorKind::HostUnreachable => Some("EHOSTUNREACH"),
                ErrorKind::NetworkUnreachable => Some("ENETUNREACH"),
                ErrorKind::AddrNotAvailable => Some("EADDRNOTAVAIL"),
                _ => None,
            };
            if let Some(code) = code {
                return code.into();
            }
        }
        if cause.to_string().starts_with("dns error") {
            return "ENOTFOUND".into();
        }
        source = cause.source();
    }
    source_chain(error)
}

/// `fetchCliProxyModels`: validate a connection before changing either
/// credentials or settings.
///
/// `url` is the models URL ([`super::CliProxyEndpoints::models`]) and `root`
/// the proxy root, which each Anthropic Messages model is sent to.
/// Aborting `signal` ends the request at once with
/// [`FetchCliProxyModelsError::Aborted`].
///
/// Call it inside a Tokio runtime with the time driver enabled, as reqwest
/// itself requires: the time bound and the abort race on Tokio's timer and
/// panic without one. It spawns nothing, so dropping the future closes the
/// connection.
pub async fn fetch_cli_proxy_models(
    url: &str,
    api_key: &str,
    signal: Option<&AbortSignal>,
    root: Option<&str>,
) -> Result<Vec<CliProxyModel>, FetchCliProxyModelsError> {
    fetch_with_timeout(url, api_key, signal, root, CATALOG_TIMEOUT).await
}

/// [`fetch_cli_proxy_models`] with the time bound as a parameter, for tests.
pub(crate) async fn fetch_with_timeout(
    url: &str,
    api_key: &str,
    signal: Option<&AbortSignal>,
    root: Option<&str>,
    timeout: Duration,
) -> Result<Vec<CliProxyModel>, FetchCliProxyModelsError> {
    let parsed = Url::parse(url).map_err(|_| FetchCliProxyModelsError::InvalidUrl)?;
    let host = match (parsed.host_str(), parsed.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_owned(),
        (None, _) => String::new(),
    };
    if signal.is_some_and(AbortSignal::aborted) {
        return Err(FetchCliProxyModelsError::Aborted);
    }
    let cancelled = async {
        match signal {
            Some(signal) => signal.cancelled().await,
            None => std::future::pending::<()>().await,
        }
    };
    // The TypeScript's timeout signal covers the whole read; a timeout
    // during the body is reported as one too, rather than as a raw abort.
    tokio::select! {
        biased;
        () = cancelled => Err(FetchCliProxyModelsError::Aborted),
        result = tokio::time::timeout(timeout, read_catalog(parsed, &host, api_key, root)) => match result {
            Ok(result) => result,
            Err(_) => Err(CliProxyCheckError::new(
                CliProxyCheckFailure::Timeout { host: host.clone() },
                format!("CLIProxyAPI at {host} did not answer within 15 seconds"),
            )
            .into()),
        },
    }
}

async fn read_catalog(
    url: Url,
    host: &str,
    api_key: &str,
    root: Option<&str>,
) -> Result<Vec<CliProxyModel>, FetchCliProxyModelsError> {
    let unreachable = |detail: String| {
        CliProxyCheckError::new(
            CliProxyCheckFailure::Unreachable {
                host: host.to_owned(),
                detail: detail.clone(),
            },
            format!("CLIProxyAPI at {host} is unreachable: {detail}"),
        )
    };
    let client = client().map_err(unreachable)?;
    let mut authorization = reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}"))
        // A key no header can carry fails as `fetch` fails to build the
        // request; the key itself is never quoted.
        .map_err(|_| unreachable("the API key cannot be sent in an HTTP header".into()))?;
    authorization.set_sensitive(true);
    let mut response = client
        .get(url)
        .header(reqwest::header::AUTHORIZATION, authorization)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|error| unreachable(transport_detail(&error)))?;
    let status = response.status().as_u16();
    if matches!(status, 301 | 302 | 303 | 307 | 308) {
        return Err(CliProxyCheckError::new(
            CliProxyCheckFailure::Redirect,
            format!("CLIProxyAPI at {host} redirected the model request"),
        )
        .into());
    }
    if status != 200 {
        let failure = match status {
            401 | 403 => CliProxyCheckFailure::Rejected { status },
            404 => CliProxyCheckFailure::NotFound,
            _ => CliProxyCheckFailure::Status { status },
        };
        return Err(CliProxyCheckError::new(
            failure,
            format!("CLIProxyAPI model request failed (HTTP {status})"),
        )
        .into());
    }
    let too_large = || {
        CliProxyCheckError::new(
            CliProxyCheckFailure::TooLarge,
            "CLIProxyAPI model list is too large",
        )
    };
    if response
        .content_length()
        .is_some_and(|length| length > MAX_CATALOG_BYTES as u64)
    {
        return Err(too_large().into());
    }
    let mut body: Vec<u8> = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > MAX_CATALOG_BYTES {
                    return Err(too_large().into());
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(error) => return Err(unreachable(transport_detail(&error)).into()),
        }
    }
    // `TextDecoder` replaces invalid UTF-8 rather than failing, and drops
    // a leading byte order mark, which `JSON.parse` would refuse.
    let text = String::from_utf8_lossy(&body);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let payload: Value = serde_json::from_str(text).map_err(|_| {
        CliProxyCheckError::new(
            CliProxyCheckFailure::NotProxy,
            "CLIProxyAPI returned invalid model JSON",
        )
    })?;
    let models = cli_proxy_models(&payload, root).map_err(|_| {
        CliProxyCheckError::new(
            CliProxyCheckFailure::NotProxy,
            "CLIProxyAPI returned an invalid model list",
        )
    })?;
    if models.is_empty() {
        return Err(CliProxyCheckError::new(
            CliProxyCheckFailure::Empty,
            "CLIProxyAPI returned no selectable models",
        )
        .into());
    }
    Ok(models)
}

#[cfg(test)]
mod tests;
