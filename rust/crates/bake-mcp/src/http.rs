//! The Streamable HTTP transport, as owned [`MessageReceiver`] and
//! [`MessageSender`] halves for [`McpClient`](crate::McpClient).
//!
//! Ported from Pi's `StreamableHttpTransport`
//! (`packages/mcp/src/transports/streamable-http.ts`, v1.3.0) for the
//! Streamable HTTP binding of MCP 2025-11-25, which the handshake revisions
//! the client negotiates share. Each client message is one HTTP POST to the
//! endpoint; a request is answered with a JSON body or a server-sent events
//! stream; `notifications/initialized` accepted opens the server's GET
//! stream; the `MCP-Session-Id` from the initialize response and the
//! negotiated `MCP-Protocol-Version` are sent on every later request; and a
//! close sends `DELETE` for the session.
//!
//! # Use
//!
//! [`HttpTransport::start`] validates the options, builds the HTTP client,
//! and starts one supervisor task without any I/O. It returns the owner
//! handle and the two halves for [`McpClient::start`](crate::McpClient::start).
//! [`HttpTransport::start_with_client`] takes the HTTP client from the
//! caller instead, which then owns resolution, pinning, redirects, proxies,
//! and TLS, as Bake's MCP egress admission does:
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use bake_mcp::{ClientOptions, HttpOptions, HttpTransport, Implementation, McpClient};
//!
//! let options = HttpOptions::new("https://example.com/mcp");
//! let (http, receiver, sender) = HttpTransport::start(options)?;
//! let mut connecting = McpClient::start(
//!     receiver,
//!     sender,
//!     ClientOptions::new(Implementation::new("bake", "0.4.0")),
//! )?;
//! match connecting.initialize().await {
//!     Ok((client, _events)) => {
//!         client.list_tools().await?;
//!         client.shutdown().await;
//!     }
//!     Err(error) => {
//!         // `http.failure()` says whether authorization is needed.
//!         let _ = (error, http.failure());
//!     }
//! }
//! http.shutdown().await;
//! # Ok(())
//! # }
//! ```
//!
//! # Ownership
//!
//! The supervisor task owns every HTTP exchange: each admitted POST, each
//! response stream, the GET stream, and their reconnection waits run as
//! tasks in its `JoinSet`, and no task is detached. The halves reach it
//! through bounded channels. [`HttpSender::send`] returns once the
//! supervisor holds the message, not when the server answers, so a slow
//! request does not hold up the client's other messages; responses and
//! server messages arrive through [`HttpReceiver::receive`].
//!
//! Shutdown first refuses new work, then ends owned tasks, then sends
//! `DELETE`:
//!
//! - [`HttpSender::close`], which the client calls once under its shutdown
//!   deadline, stops taking messages, aborts request and stream tasks, lets
//!   notification and answer POSTs already admitted finish (each within
//!   [`HttpOptions::notification_timeout`]), and waits for the supervisor.
//!   If the client drops `close` at its deadline, the supervisor goes on
//!   alone.
//! - [`HttpTransport::shutdown`] aborts every task at once, awaits each, sends
//!   `DELETE`, and returns once the supervisor task has ended and dropped
//!   the HTTP client. It is the owner's quiescence point; call it after the
//!   client's own shutdown, also when initialization failed or was
//!   cancelled. Dropping the handle only signals the same abort.
//! - A fatal failure (below) aborts as `shutdown` does.
//!
//! The HTTP client's connection pool belongs to `reqwest`: its per-connection
//! tasks end once their connection and the client are dropped, which happens
//! when the supervisor ends, but they are not awaited.
//!
//! # Failures
//!
//! A failure that leaves the connection usable fails one request: the
//! transport delivers a JSON-RPC error response for it (code `-32603`, a
//! message from [`HttpError`]'s text), as Pi does for a broken response
//! stream, so the client's call returns [`RequestError::Server`]. A failed
//! notification or answer becomes an error message without an id, which the
//! client reports as [`Diagnostic::ErrorWithoutId`]; when the client's event
//! queue is full it is dropped and counted
//! ([`HttpTransport::dropped_diagnostics`]).
//!
//! Two failures end the connection, since every later request would fail the
//! same way: authorization required (HTTP 401, or 403 with an
//! `insufficient_scope` bearer challenge) and an expired session (HTTP 404
//! to a request that carried a session ID). The transport records the
//! failure, which [`HttpTransport::failure`] returns typed, aborts its work,
//! and the receiver then fails with an [`io::Error`] whose text is the
//! failure's, so the client closes with [`CloseReason::Receive`].
//! Authorization and a new session are a later owner's: this transport
//! never obtains, refreshes, or stores credentials and never
//! re-initializes.
//!
//! [`RequestError::Server`]: crate::RequestError::Server
//! [`Diagnostic::ErrorWithoutId`]: crate::Diagnostic::ErrorWithoutId
//! [`CloseReason::Receive`]: crate::CloseReason::Receive
//!
//! # No retried request, no inferred answer
//!
//! A POST is sent once. A network error, a stream that ends, or any other
//! failure before the request's response arrives fails the request; it is
//! never retried, since the server may have run it already, and a stream's
//! end is never taken as an answer. The client of
//! [`HttpTransport::start`] follows no redirect, so nothing resends a POST
//! behind the transport's back; a client a caller supplies
//! ([`HttpTransport::start_with_client`]) whose policy follows one does:
//! `reqwest` replays the POST body on 307 and 308 and rewrites it into a
//! GET on 301, 302, and 303, so a POST that crosses a redirect reaches the
//! server twice. A response stream that breaks after the
//! server gave an event ID is resumed with GET and `Last-Event-ID`, which
//! replays the stream without running the request again; the GET stream is
//! reopened the same way. Both use Pi's bounded backoff ([`Reconnect`]) and
//! the server's `retry` field, and retry only network errors and HTTP 408,
//! 429, and 5xx. A request whose response stream cannot be resumed fails.
//!
//! # Bounds
//!
//! - Requests in flight: [`HttpLimits::max_in_flight_requests`] POSTs and
//!   their response streams run at once, and as many more wait in the
//!   supervisor; notifications and answers have their own
//!   [`HttpLimits::max_in_flight_notifications`]. Requests and notifications
//!   reach the supervisor through separate one-message channels, each taken
//!   only while its own queue has room, so waiting requests never hold back
//!   a `notifications/cancelled`. A cancellation acts when it is taken,
//!   before its own POST: it withdraws a waiting request, aborts a running
//!   request's POST, or, for a request still in its channel, is remembered
//!   (at most `max_in_flight_requests` ids, oldest forgotten first) so the
//!   request is dropped when it arrives. Notification admission always
//!   progresses: notification POSTs end within
//!   [`HttpOptions::notification_timeout`] and never wait for the receive
//!   queue (a diagnostic for a full queue is dropped and counted).
//! - Request admission: [`HttpSender::send`] waits for a request only while
//!   `max_in_flight_requests` run and as many wait. The client writes one
//!   message at a time, so meanwhile its later messages, cancellations
//!   included, wait in its own queue. That cannot happen while
//!   `max_in_flight_requests` is at least the client's
//!   [`Limits::max_pending_requests`](crate::Limits::max_pending_requests)
//!   (both default to 64): a request holds a client permit until its
//!   response is read or its cancellation processed, the client queues the
//!   cancellation before any request admitted with the released permit, and
//!   a request task ends once its response is read and is aborted at its
//!   cancellation, so the transport holds at most `max_pending_requests`
//!   live requests. A smaller value makes requests wait for running ones,
//!   and a cancellation queued behind a waiting request waits with it.
//! - Incoming messages: [`HttpLimits::max_queued_messages`]. A task with a
//!   message for a full queue waits, and stops reading its HTTP body until
//!   the client takes one, so a slow consumer slows the server.
//! - Message size: [`HttpLimits::max_message_bytes`] for a JSON body (refused
//!   by `Content-Length` before reading, or as soon as the bytes read pass
//!   it) and for each event's data; an SSE line may hold 64 bytes more for
//!   its field name. Each stream holds at most one line and one event.
//!   Error bodies are read up to 8 KiB, within 5 seconds.
//! - Memory: messages handed to [`HttpSender::send`] are copied from the
//!   client's, which its own message limit bounds; the transport holds up to
//!   `2 × (max_in_flight_requests + max_in_flight_notifications) + 2` of
//!   them (running, waiting, and one per channel). Each running request or
//!   stream holds at most one JSON body or one SSE line and event, and the
//!   receive queue `max_queued_messages` messages, each within
//!   `max_message_bytes`. The chunks `reqwest` reads are sized by its HTTP/1
//!   read buffer, before the transport sees them.
//!
//! # Security
//!
//! The client [`HttpTransport::start`] builds follows no redirect: a 3xx
//! response fails its request without contacting the target, so caller
//! headers cannot reach another origin. It reads no proxy environment. With
//! [`HttpTransport::start_with_client`] those two are the supplied client's
//! to decide, a followed redirect resends the request as described above,
//! and a 3xx it does not follow still fails its request. A URL
//! with a user name or password is refused either way. `Debug` output of the options, the transport, and its halves
//! shows the URL's origin only, and no header value or session ID; caller
//! header values are also marked sensitive for `reqwest`.
//!
//! Before an [`HttpError`] leaves the transport (through
//! [`HttpTransport::failure`], an error response or diagnostic, a receive
//! error, or [`SessionClose::Failed`]), each exact occurrence of a
//! configured header value, the URL, its path and query, and the session ID
//! in its text is replaced with `[redacted]`. The rest of that text is the
//! server's (bodies, `WWW-Authenticate` challenges, content types) or
//! `reqwest`'s and the system's (network causes, with `reqwest`'s URL
//! removed), passed on cut to its limits but otherwise as received: a
//! secret the server encodes, splits, or transforms, and any other
//! sensitive text it sends, is not filtered.
//!
//! # Not implemented
//!
//! OAuth and any credential source other than the static headers given at
//! start; re-initialization after an expired session, which the owner of the
//! transport does by starting a new one (Bake's `mcp::http`); the deprecated
//! 2024-11-05 HTTP+SSE transport fallback; the stateless 2026-07-28 binding;
//! and HTTP/2. The client built here uses no proxy; a supplied one may.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use reqwest::header::{self, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, Response, StatusCode, Url};
use serde_json::{Map, Value};
use tokio::sync::{mpsc, watch};
use tokio::task::{AbortHandle, Id, JoinError, JoinHandle, JoinSet};
use tokio::time::Instant;

use crate::jsonrpc::{error_codes, preview};
use crate::protocol::ProtocolVersion;
use crate::sse::{SseDecoder, SseItem, TooLarge};
use crate::transport::{DEFAULT_MAX_MESSAGE_BYTES, MessageReceiver, MessageSender, ReceiveError};

/// Pi's `MAX_ERROR_BODY_BYTES`.
const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;
/// Pi's `ERROR_MESSAGE_BODY_CHARS`.
const ERROR_MESSAGE_BODY_CHARS: usize = 500;
/// How long an error body may take to arrive.
const ERROR_BODY_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest `WWW-Authenticate` challenge kept, in characters.
const MAX_CHALLENGE_CHARS: usize = 2_048;
const SESSION_HEADER: &str = "mcp-session-id";
const VERSION_HEADER: &str = "mcp-protocol-version";
const LAST_EVENT_ID_HEADER: &str = "last-event-id";
const POST_ACCEPT: &str = "application/json, text/event-stream";
const EVENT_STREAM: &str = "text/event-stream";
const JSON: &str = "application/json";

/// Headers the transport sets itself, which caller headers may not replace.
const RESERVED_HEADERS: [&str; 8] = [
    "accept",
    "content-type",
    "content-length",
    "transfer-encoding",
    "host",
    SESSION_HEADER,
    VERSION_HEADER,
    LAST_EVENT_ID_HEADER,
];

const fn nonzero(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    }
}

/// The transport's bounds; see the [module documentation](self).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HttpLimits {
    /// The largest JSON body and SSE event data, in bytes. Default: 16 MiB,
    /// as Pi.
    pub max_message_bytes: NonZeroUsize,
    /// Request POSTs and their response streams at once. Default: 64.
    pub max_in_flight_requests: NonZeroUsize,
    /// Notification and answer POSTs at once. Default: 16.
    pub max_in_flight_notifications: NonZeroUsize,
    /// Messages waiting for [`HttpReceiver::receive`]. Default: 64.
    pub max_queued_messages: NonZeroUsize,
}

impl Default for HttpLimits {
    fn default() -> Self {
        Self {
            max_message_bytes: nonzero(DEFAULT_MAX_MESSAGE_BYTES),
            max_in_flight_requests: nonzero(64),
            max_in_flight_notifications: nonzero(16),
            max_queued_messages: nonzero(64),
        }
    }
}

/// Reconnection of a dropped SSE stream, as Pi's
/// `StreamableHttpReconnectOptions`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reconnect {
    /// The delay before the first attempt unless the server sent `retry`.
    /// Default: 1 s.
    pub initial_delay: Duration,
    /// The backoff's upper bound; a stream that stayed open this long counts
    /// as healthy. Default: 30 s. A server's `retry` is honored as sent.
    pub max_delay: Duration,
    /// Consecutive failed attempts before giving up on a stream. Default: 5.
    pub max_retries: u32,
}

impl Default for Reconnect {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            max_retries: 5,
        }
    }
}

/// How a transport connects.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpOptions {
    /// The MCP endpoint, `http` or `https`, without a user name or password.
    pub url: String,
    /// Headers sent with every request, such as `Authorization`. Values are
    /// treated as secrets. Names the transport sets (`Accept`,
    /// `Content-Type`, `Content-Length`, `Transfer-Encoding`, `Host`,
    /// `MCP-Session-Id`, `MCP-Protocol-Version`, `Last-Event-ID`) are
    /// refused.
    pub headers: Vec<(String, String)>,
    /// Open the server-to-client GET stream after initialization. Default:
    /// `true`, as Pi.
    pub open_server_stream: bool,
    /// The transport's bounds.
    pub limits: HttpLimits,
    /// SSE stream reconnection.
    pub reconnect: Reconnect,
    /// The TCP and TLS connection deadline of each request. Default: 10 s.
    pub connect_timeout: Duration,
    /// Each notification or answer POST's deadline, through its response.
    /// Default: 30 s. A request's POST has none: the client's request
    /// timeout cancels it.
    pub notification_timeout: Duration,
    /// The session `DELETE`'s deadline. Default: 1 s, as Pi.
    pub delete_timeout: Duration,
}

impl HttpOptions {
    /// The defaults for `url`.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            headers: Vec::new(),
            open_server_stream: true,
            limits: HttpLimits::default(),
            reconnect: Reconnect::default(),
            connect_timeout: Duration::from_secs(10),
            notification_timeout: Duration::from_secs(30),
            delete_timeout: Duration::from_secs(1),
        }
    }

    /// Add a header sent with every request.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// Shows the URL's origin and the header names only.
impl fmt::Debug for HttpOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let origin = Url::parse(&self.url)
            .map(|url| url.origin().ascii_serialization())
            .unwrap_or_else(|_| "<invalid>".into());
        f.debug_struct("HttpOptions")
            .field("origin", &origin)
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>(),
            )
            .field("open_server_stream", &self.open_server_stream)
            .field("limits", &self.limits)
            .field("reconnect", &self.reconnect)
            .field("connect_timeout", &self.connect_timeout)
            .field("notification_timeout", &self.notification_timeout)
            .field("delete_timeout", &self.delete_timeout)
            .finish()
    }
}

/// Why [`HttpTransport::start`] refused its options. Names a header at most;
/// never contains the URL or a header value.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HttpOptionsError {
    /// The URL does not parse.
    InvalidUrl,
    /// The URL's scheme is not `http` or `https`.
    UnsupportedScheme,
    /// The URL has a user name or password; pass credentials as headers.
    CredentialsInUrl,
    /// A header name is not a valid HTTP header name.
    InvalidHeaderName {
        /// The header's position in [`HttpOptions::headers`].
        index: usize,
    },
    /// A header value is not a valid HTTP header value.
    InvalidHeaderValue {
        /// The header's name.
        name: String,
    },
    /// A header the transport sets itself.
    ReservedHeader {
        /// The header's name.
        name: String,
    },
    /// A capacity is larger than Tokio's channels support.
    CapacityTooLarge {
        /// The option's path.
        field: &'static str,
    },
    /// A duration reaches past what the clock can represent.
    DurationTooLong {
        /// The option's name.
        field: &'static str,
    },
    /// The HTTP client could not be built.
    Client {
        /// Why.
        message: String,
    },
    /// No Tokio runtime is running on this thread.
    NoRuntime,
}

impl fmt::Display for HttpOptionsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl => f.write_str("the MCP server URL is not a valid URL"),
            Self::UnsupportedScheme => f.write_str("the MCP server URL must use http or https"),
            Self::CredentialsInUrl => f.write_str(
                "the MCP server URL contains a user name or password; pass credentials as headers",
            ),
            Self::InvalidHeaderName { index } => {
                write!(f, "MCP HTTP header {index} has an invalid name")
            }
            Self::InvalidHeaderValue { name } => {
                write!(f, "MCP HTTP header {name:?} has an invalid value")
            }
            Self::ReservedHeader { name } => write!(
                f,
                "MCP HTTP header {name:?} is set by the transport and cannot be configured"
            ),
            Self::CapacityTooLarge { field } => {
                write!(f, "{field} is over the largest supported capacity")
            }
            Self::DurationTooLong { field } => {
                write!(f, "{field} is too long for the clock to represent")
            }
            Self::Client { message } => write!(f, "could not create the HTTP client: {message}"),
            Self::NoRuntime => f.write_str("an MCP HTTP transport needs a running Tokio runtime"),
        }
    }
}

impl std::error::Error for HttpOptionsError {}

/// Why an HTTP exchange failed. Server bodies are kept cut to 8 KiB and
/// shown cut to 500 characters. Text from the server and the network is
/// passed on after the transport removes its configured secrets; see the
/// [module documentation](self#security) for exactly what is removed.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HttpError {
    /// HTTP 401, or 403 with an `insufficient_scope` bearer challenge. Ends
    /// the connection; an authorization owner acts on `challenge`.
    AuthorizationRequired {
        /// 401 or 403.
        status: u16,
        /// The `WWW-Authenticate` header, cut to 2,048 characters.
        challenge: Option<String>,
        /// The response body, cut to 8 KiB.
        body: String,
    },
    /// HTTP 404 to a request that carried the session ID. Ends the
    /// connection; the specification asks the client to initialize a new
    /// session, which is a later owner's.
    SessionExpired {
        /// The response body, cut to 8 KiB.
        body: String,
    },
    /// Another unsuccessful status.
    Status {
        /// The status code.
        status: u16,
        /// The response body, cut to 8 KiB.
        body: String,
    },
    /// A 3xx response, which is not followed.
    Redirect {
        /// The status code.
        status: u16,
    },
    /// A response with a content type the transport cannot read.
    UnexpectedContentType {
        /// The status code.
        status: u16,
        /// The content type, cut to 100 characters.
        content_type: Option<String>,
    },
    /// The server accepted a request (202 or 204) without answering it.
    AcceptedWithoutResponse {
        /// The status code.
        status: u16,
    },
    /// A JSON body that does not answer the request it was sent for.
    NoResponse,
    /// A JSON body that is not JSON.
    InvalidJson {
        /// The parser's message.
        detail: String,
    },
    /// The initialize response's `MCP-Session-Id` is not visible ASCII.
    InvalidSessionId,
    /// A body or event over the message limit.
    MessageTooLarge {
        /// The limit in bytes.
        limit: usize,
    },
    /// A response stream ended before the request's response.
    StreamEnded,
    /// A response stream broke and the server does not offer GET streams,
    /// so it cannot be resumed.
    ResumptionUnsupported,
    /// The server-to-client stream dropped and could not be reopened.
    ServerStreamLost,
    /// The request did not complete within its deadline.
    Timeout,
    /// Connecting, sending, or reading failed.
    Network {
        /// The error and its causes, without the URL.
        message: String,
    },
    /// A transport task panicked.
    Internal,
}

impl HttpError {
    /// Whether the failure ends the connection.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::AuthorizationRequired { .. } | Self::SessionExpired { .. } | Self::Internal
        )
    }

    /// Network failures and transient statuses (408, 429, 5xx) are retried
    /// when reopening a stream; nothing else is.
    fn is_retryable(&self) -> bool {
        match self {
            Self::Network { .. } => true,
            Self::Status { status, .. } => matches!(status, 408 | 429 | 500..),
            _ => false,
        }
    }

    fn io_kind(&self) -> io::ErrorKind {
        match self {
            Self::AuthorizationRequired { .. } => io::ErrorKind::PermissionDenied,
            Self::SessionExpired { .. } => io::ErrorKind::NotConnected,
            _ => io::ErrorKind::Other,
        }
    }

    fn network(error: reqwest::Error) -> Self {
        let error = error.without_url();
        let mut message = error.to_string();
        let mut source = std::error::Error::source(&error);
        while let Some(cause) = source {
            let text = cause.to_string();
            if !message.contains(&text) {
                message.push_str(": ");
                message.push_str(&text);
            }
            source = cause.source();
        }
        Self::Network { message }
    }
}

fn snippet(body: &str) -> String {
    let text = body.trim();
    if text.is_empty() {
        String::new()
    } else {
        format!(": {}", preview(text, ERROR_MESSAGE_BODY_CHARS))
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthorizationRequired { status, .. } => {
                write!(f, "MCP server requires authorization (HTTP {status})")
            }
            Self::SessionExpired { body } => write!(f, "MCP session expired{}", snippet(body)),
            Self::Status { status, body } => write!(
                f,
                "MCP HTTP request failed with status {status}{}",
                snippet(body)
            ),
            Self::Redirect { status } => write!(
                f,
                "MCP server answered with a redirect (HTTP {status}), which is not followed"
            ),
            Self::UnexpectedContentType {
                status,
                content_type,
            } => write!(
                f,
                "Unsupported MCP response content type: {} (HTTP {status})",
                content_type.as_deref().unwrap_or("missing")
            ),
            Self::AcceptedWithoutResponse { status } => write!(
                f,
                "MCP server accepted the request without a response (HTTP {status})"
            ),
            Self::NoResponse => f.write_str("MCP HTTP response did not answer the request"),
            Self::InvalidJson { detail } => write!(f, "MCP HTTP response is not JSON: {detail}"),
            Self::InvalidSessionId => {
                f.write_str("MCP server sent a session ID that is not visible ASCII")
            }
            Self::MessageTooLarge { limit } => {
                write!(f, "MCP HTTP message exceeds {limit} bytes")
            }
            Self::StreamEnded => f.write_str("stream ended without a response"),
            Self::ResumptionUnsupported => {
                f.write_str("stream broke and the server does not offer resumption")
            }
            Self::ServerStreamLost => {
                f.write_str("MCP server-to-client stream dropped and could not be reopened")
            }
            Self::Timeout => f.write_str("MCP HTTP request timed out"),
            Self::Network { message } => write!(f, "MCP HTTP request failed: {message}"),
            Self::Internal => f.write_str("an MCP HTTP transport task failed"),
        }
    }
}

impl std::error::Error for HttpError {}

/// What happened to the session at shutdown.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionClose {
    /// No session ID was assigned, so no `DELETE` was sent.
    NoSession,
    /// The session had expired, so no `DELETE` was sent.
    Expired,
    /// The server answered `DELETE` with a success status.
    Deleted {
        /// The status code.
        status: u16,
    },
    /// The server answered 405: it does not let clients end sessions, and
    /// the session ends when the server expires it.
    NotAllowed,
    /// The server answered with another status; the session may stay open
    /// until the server expires it.
    Rejected {
        /// The status code.
        status: u16,
    },
    /// The `DELETE` failed or passed its deadline; the session may stay
    /// open until the server expires it.
    Failed(HttpError),
}

/// The owner of a Streamable HTTP connection's tasks.
///
/// Call [`shutdown`](Self::shutdown) after the client's shutdown to wait
/// until every HTTP exchange has ended. Dropping it signals the same abort
/// without waiting.
#[derive(Debug)]
pub struct HttpTransport {
    shared: Arc<Shared>,
    supervisor: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

impl HttpTransport {
    /// Validate `options`, build the HTTP client, and start the supervisor
    /// task. Sends nothing; the first POST is the client's `initialize`.
    ///
    /// The client connects directly: it reads no proxy environment, follows
    /// no redirect, resolves each request's host itself, and applies
    /// [`HttpOptions::connect_timeout`].
    ///
    /// Must be called inside a Tokio runtime.
    pub fn start(
        options: HttpOptions,
    ) -> Result<(Self, HttpReceiver, HttpSender), HttpOptionsError> {
        let config = Config::new(&options)?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .http1_only()
            .connect_timeout(options.connect_timeout)
            .build()
            .map_err(|error| HttpOptionsError::Client {
                message: HttpError::network(error).to_string(),
            })?;
        Self::spawn(options, config, client)
    }

    /// [`start`](Self::start) over an HTTP client the caller built, which
    /// then owns every transport-level network decision this transport does
    /// not make itself: address resolution and pinning, the redirect policy,
    /// proxies, TLS, and [`HttpOptions::connect_timeout`], which this call
    /// does not apply. The options are validated as `start` validates them,
    /// and the URL, headers, limits, and timeouts still hold.
    ///
    /// A caller that admits network egress (Bake's
    /// `mcp::egress`) supplies a client whose resolver answers with the
    /// addresses its policy pinned and whose redirect policy refuses a
    /// cross-origin hop, so the transport reaches only the admitted origin.
    /// A 3xx response the client does not follow is still
    /// [`HttpError::Redirect`]; one it does follow resends the request,
    /// which is the caller's exception to this module's "a POST is sent
    /// once".
    ///
    /// Must be called inside a Tokio runtime.
    pub fn start_with_client(
        options: HttpOptions,
        client: reqwest::Client,
    ) -> Result<(Self, HttpReceiver, HttpSender), HttpOptionsError> {
        let config = Config::new(&options)?;
        Self::spawn(options, config, client)
    }

    /// Start the supervisor task for validated `options` and `config`.
    fn spawn(
        options: HttpOptions,
        config: Config,
        client: reqwest::Client,
    ) -> Result<(Self, HttpReceiver, HttpSender), HttpOptionsError> {
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| HttpOptionsError::NoRuntime)?;
        let limits = options.limits;
        let (requests, requests_in) = mpsc::channel(1);
        let (notices, notices_in) = mpsc::channel(1);
        let (incoming, incoming_receiver) = mpsc::channel(limits.max_queued_messages.get());
        let (phase, phase_receiver) = watch::channel(Phase::Open);
        let (done, done_receiver) = watch::channel(false);
        let shared = Arc::new(Shared {
            failure: Mutex::new(None),
            session: Mutex::new(None),
            version: Mutex::new(None),
            session_close: Mutex::new(None),
            phase,
            done,
            dropped_diagnostics: AtomicU64::new(0),
            secrets: config.secrets(),
        });
        let context = Context {
            client,
            config: Arc::new(config),
            shared: shared.clone(),
            incoming,
        };
        let supervisor = Supervisor {
            context,
            requests_in,
            notices_in,
            cancelled_early: VecDeque::new(),
            phase: phase_receiver,
            tasks: JoinSet::new(),
            kinds: HashMap::new(),
            requests: HashMap::new(),
            waiting_requests: VecDeque::new(),
            waiting_notices: VecDeque::new(),
            running_requests: 0,
            running_notices: 0,
            server_stream: None,
            server_stream_started: false,
        };
        let task = runtime.spawn(supervisor.run());
        Ok((
            Self {
                shared: shared.clone(),
                supervisor: tokio::sync::Mutex::new(Some(task)),
            },
            HttpReceiver {
                incoming: incoming_receiver,
                shared: shared.clone(),
            },
            HttpSender {
                requests,
                notices,
                phase: shared.phase.subscribe(),
                done: done_receiver,
                shared,
            },
        ))
    }

    /// The failure that ended the connection, if one did.
    pub fn failure(&self) -> Option<HttpError> {
        lock(&self.shared.failure).clone()
    }

    /// The protocol version sent in `MCP-Protocol-Version`, once the server's
    /// initialize result named a supported one.
    pub fn protocol_version(&self) -> Option<ProtocolVersion> {
        *lock(&self.shared.version)
    }

    /// Whether the server assigned a session ID. The ID itself is not
    /// exposed: it authenticates the session.
    pub fn has_session(&self) -> bool {
        lock(&self.shared.session).is_some()
    }

    /// Transport diagnostics dropped because the receive queue was full.
    pub fn dropped_diagnostics(&self) -> u64 {
        self.shared.dropped_diagnostics.load(Ordering::Relaxed)
    }

    /// Abort every HTTP exchange, wait for each task to end, send `DELETE`
    /// for the session within [`HttpOptions::delete_timeout`], and return
    /// once the supervisor task has ended. Safe to call more than once and
    /// from several tasks; every call returns after the task has ended.
    pub async fn shutdown(&self) -> SessionClose {
        self.shared.phase.send_if_modified(|phase| {
            let changed = *phase != Phase::Abort;
            *phase = Phase::Abort;
            changed
        });
        let mut supervisor = self.supervisor.lock().await;
        if let Some(task) = supervisor.as_mut() {
            // Awaited in place and cleared without an await in between, so a
            // shutdown future dropped while waiting leaves the handle for the
            // next call.
            if task.await.is_err() {
                record_failure(&self.shared, HttpError::Internal);
            }
            *supervisor = None;
        }
        lock(&self.shared.session_close)
            .clone()
            .unwrap_or(SessionClose::Failed(HttpError::Internal))
    }
}

impl Drop for HttpTransport {
    fn drop(&mut self) {
        self.shared.phase.send_replace(Phase::Abort);
    }
}

/// The receiving half: responses and server messages, in arrival order.
#[derive(Debug)]
pub struct HttpReceiver {
    incoming: mpsc::Receiver<Vec<u8>>,
    shared: Arc<Shared>,
}

impl MessageReceiver for HttpReceiver {
    /// The next message. Returns `None` once the transport has shut down
    /// and every message received before is read, and fails with the
    /// failure's text (kind `PermissionDenied` for authorization,
    /// `NotConnected` for an expired session) once a fatal failure has
    /// ended the transport. Cancel-safe: streams are decoded by the
    /// transport's tasks, so dropping this future loses nothing.
    async fn receive(&mut self) -> Result<Option<Vec<u8>>, ReceiveError> {
        match self.incoming.recv().await {
            Some(message) => Ok(Some(message)),
            None => match lock(&self.shared.failure).clone() {
                Some(failure) => Err(ReceiveError::Io(io::Error::new(
                    failure.io_kind(),
                    failure.to_string(),
                ))),
                None => Ok(None),
            },
        }
    }
}

/// The sending half: one POST per message.
#[derive(Debug)]
pub struct HttpSender {
    requests: mpsc::Sender<Job>,
    notices: mpsc::Sender<Job>,
    phase: watch::Receiver<Phase>,
    done: watch::Receiver<bool>,
    shared: Arc<Shared>,
}

impl HttpSender {
    fn closed_error(&self) -> io::Error {
        match lock(&self.shared.failure).clone() {
            Some(failure) => io::Error::new(failure.io_kind(), failure.to_string()),
            None => io::Error::new(
                io::ErrorKind::NotConnected,
                "the MCP HTTP transport is closed",
            ),
        }
    }
}

impl MessageSender for HttpSender {
    /// Hand the message to the supervisor, waiting while its queues are
    /// full. Returns before the POST completes; its outcome arrives through
    /// the receiver. Fails once the transport is closing or has failed.
    async fn send(&mut self, message: &[u8]) -> io::Result<()> {
        if *self.phase.borrow_and_update() != Phase::Open {
            return Err(self.closed_error());
        }
        let job = Job::new(message.to_vec());
        let channel = match job.kind {
            JobKind::Request { .. } => &self.requests,
            JobKind::Notice { .. } => &self.notices,
        };
        let sent = tokio::select! {
            biased;
            sent = channel.send(job) => sent.is_ok(),
            _ = self.phase.wait_for(|phase| *phase != Phase::Open) => false,
        };
        if sent {
            Ok(())
        } else {
            Err(self.closed_error())
        }
    }

    /// Stop taking messages, let admitted notification and answer POSTs
    /// finish, end every other exchange, send `DELETE`, and wait for the
    /// supervisor to end. If this future is dropped, the supervisor goes
    /// on; [`HttpTransport::shutdown`] aborts and awaits it.
    async fn close(&mut self) -> io::Result<()> {
        self.shared.phase.send_if_modified(|phase| {
            let open = *phase == Phase::Open;
            if open {
                *phase = Phase::Drain;
            }
            open
        });
        let _ = self.done.wait_for(|done| *done).await;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Open,
    /// Finish admitted notifications, abort the rest.
    Drain,
    /// Abort everything.
    Abort,
}

struct Shared {
    failure: Mutex<Option<HttpError>>,
    session: Mutex<Option<HeaderValue>>,
    version: Mutex<Option<ProtocolVersion>>,
    session_close: Mutex<Option<SessionClose>>,
    phase: watch::Sender<Phase>,
    done: watch::Sender<bool>,
    dropped_diagnostics: AtomicU64,
    /// The configured header values and the URL, its path, and its query,
    /// removed from text that leaves the transport.
    secrets: Vec<String>,
}

/// Omits the session ID and the secrets.
impl fmt::Debug for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shared")
            .field("failure", &*lock(&self.failure))
            .field("has_session", &lock(&self.session).is_some())
            .field("version", &*lock(&self.version))
            .field("phase", &*self.phase.borrow())
            .finish_non_exhaustive()
    }
}

impl Shared {
    /// Replace each configured secret and the session ID in the error's
    /// text with `[redacted]`. Exact matches only: a server body that
    /// encodes or splits a secret is passed on as the server sent it.
    fn redact(&self, error: HttpError) -> HttpError {
        let session = lock(&self.session)
            .as_ref()
            .and_then(|value| value.to_str().ok().map(str::to_owned));
        let scrub = |text: &mut String| {
            for secret in self.secrets.iter().chain(session.iter()) {
                if !secret.is_empty() && text.contains(secret.as_str()) {
                    *text = text.replace(secret.as_str(), "[redacted]");
                }
            }
        };
        let mut error = error;
        match &mut error {
            HttpError::AuthorizationRequired {
                challenge, body, ..
            } => {
                if let Some(challenge) = challenge {
                    scrub(challenge);
                }
                scrub(body);
            }
            HttpError::SessionExpired { body } | HttpError::Status { body, .. } => scrub(body),
            HttpError::InvalidJson { detail } => scrub(detail),
            HttpError::Network { message } => scrub(message),
            HttpError::UnexpectedContentType {
                content_type: Some(content_type),
                ..
            } => scrub(content_type),
            _ => {}
        }
        error
    }
}

/// A poisoned lock still holds consistent data here: every critical section
/// is a single read or write.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Record the first fatal failure and abort the transport.
fn record_failure(shared: &Shared, failure: HttpError) {
    let failure = shared.redact(failure);
    let mut slot = lock(&shared.failure);
    if slot.is_none() {
        *slot = Some(failure);
    }
    drop(slot);
    shared.phase.send_replace(Phase::Abort);
}

struct Config {
    url: Url,
    headers: HeaderMap,
    limits: HttpLimits,
    reconnect: Reconnect,
    open_server_stream: bool,
    notification_timeout: Duration,
    delete_timeout: Duration,
}

/// Shows the URL's origin; header values are sensitive and print as such.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("origin", &self.url.origin().ascii_serialization())
            .field("headers", &self.headers)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl Config {
    fn secrets(&self) -> Vec<String> {
        let mut secrets: Vec<String> = self
            .headers
            .values()
            .filter_map(|value| value.to_str().ok().map(str::to_owned))
            .collect();
        secrets.push(self.url.as_str().to_owned());
        if let Some(query) = self.url.query() {
            secrets.push(query.to_owned());
        }
        let path = self.url.path();
        if path.len() > 1 {
            secrets.push(path.to_owned());
        }
        // Longest first, so a secret that contains another is removed whole.
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        secrets.dedup();
        secrets
    }

    fn new(options: &HttpOptions) -> Result<Self, HttpOptionsError> {
        let url = Url::parse(&options.url).map_err(|_| HttpOptionsError::InvalidUrl)?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(HttpOptionsError::UnsupportedScheme);
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(HttpOptionsError::CredentialsInUrl);
        }
        let mut headers = HeaderMap::new();
        for (index, (name, value)) in options.headers.iter().enumerate() {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| HttpOptionsError::InvalidHeaderName { index })?;
            if RESERVED_HEADERS.contains(&name.as_str()) {
                return Err(HttpOptionsError::ReservedHeader {
                    name: name.as_str().into(),
                });
            }
            let mut value =
                HeaderValue::from_str(value).map_err(|_| HttpOptionsError::InvalidHeaderValue {
                    name: name.as_str().into(),
                })?;
            value.set_sensitive(true);
            headers.append(name, value);
        }
        let limits = options.limits;
        let capacities = [
            ("limits.max_queued_messages", limits.max_queued_messages),
            (
                "limits.max_in_flight_requests",
                limits.max_in_flight_requests,
            ),
            (
                "limits.max_in_flight_notifications",
                limits.max_in_flight_notifications,
            ),
        ];
        for (field, value) in capacities {
            if value.get() > tokio::sync::Semaphore::MAX_PERMITS {
                return Err(HttpOptionsError::CapacityTooLarge { field });
            }
        }
        let now = Instant::now();
        let durations = [
            ("connect_timeout", options.connect_timeout),
            ("notification_timeout", options.notification_timeout),
            ("delete_timeout", options.delete_timeout),
            ("reconnect.initial_delay", options.reconnect.initial_delay),
            ("reconnect.max_delay", options.reconnect.max_delay),
        ];
        for (field, value) in durations {
            if now.checked_add(value).is_none() {
                return Err(HttpOptionsError::DurationTooLong { field });
            }
        }
        Ok(Self {
            url,
            headers,
            limits,
            reconnect: options.reconnect,
            open_server_stream: options.open_server_stream,
            notification_timeout: options.notification_timeout,
            delete_timeout: options.delete_timeout,
        })
    }
}

/// One message from the client, classified once.
#[derive(Debug)]
struct Job {
    bytes: Vec<u8>,
    kind: JobKind,
}

#[derive(Debug)]
enum JobKind {
    /// A request; `key` is its id's JSON text.
    Request {
        id: Value,
        key: String,
        initialize: bool,
    },
    /// A notification or an answer. `cancels` is the request a
    /// `notifications/cancelled` names.
    Notice {
        initialized: bool,
        cancels: Option<String>,
    },
}

impl Job {
    fn new(bytes: Vec<u8>) -> Self {
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let method = value.get("method").and_then(Value::as_str);
        let kind = match (method, value.get("id")) {
            (Some(method), Some(id)) => JobKind::Request {
                key: id.to_string(),
                id: id.clone(),
                initialize: method == "initialize",
            },
            (method, _) => JobKind::Notice {
                initialized: method == Some("notifications/initialized"),
                cancels: (method == Some("notifications/cancelled"))
                    .then(|| value.pointer("/params/requestId").map(Value::to_string))
                    .flatten(),
            },
        };
        Self { bytes, kind }
    }
}

#[derive(Debug)]
enum TaskKind {
    Request { key: String },
    Notice,
    ServerStream,
}

/// How a task ended, for the supervisor.
#[derive(Debug)]
enum TaskEnd {
    Done,
    /// `notifications/initialized` was accepted.
    Initialized,
}

struct Supervisor {
    context: Context,
    /// Requests, taken while fewer than `max_in_flight_requests` wait.
    requests_in: mpsc::Receiver<Job>,
    /// Notifications and answers, taken while fewer than
    /// `max_in_flight_notifications` wait. A separate channel, so a
    /// `notifications/cancelled` never waits behind a request.
    notices_in: mpsc::Receiver<Job>,
    /// Cancelled ids that named no request the supervisor held: the request
    /// may still be in `requests_in`. Bounded by `max_in_flight_requests`;
    /// the oldest is forgotten first.
    cancelled_early: VecDeque<String>,
    phase: watch::Receiver<Phase>,
    tasks: JoinSet<TaskEnd>,
    kinds: HashMap<Id, TaskKind>,
    /// Running request tasks by request id.
    requests: HashMap<String, AbortHandle>,
    waiting_requests: VecDeque<Job>,
    waiting_notices: VecDeque<Job>,
    running_requests: usize,
    running_notices: usize,
    /// The GET stream's task, once started; `None` again after teardown
    /// aborts it.
    server_stream: Option<AbortHandle>,
    server_stream_started: bool,
}

impl Supervisor {
    async fn run(mut self) {
        loop {
            if *self.phase.borrow_and_update() != Phase::Open {
                break;
            }
            self.spawn_ready(true);
            let limits = self.context.config.limits;
            let request_room = self.waiting_requests.len() < limits.max_in_flight_requests.get();
            let notice_room = self.waiting_notices.len() < limits.max_in_flight_notifications.get();
            tokio::select! {
                biased;
                changed = self.phase.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
                Some(joined) = self.tasks.join_next_with_id(), if !self.tasks.is_empty() => {
                    self.reap(joined, true);
                }
                // Requests before notices: a request the sender handed over
                // is admitted before a cancellation sent after it.
                job = self.requests_in.recv(), if request_room => match job {
                    Some(job) => self.admit(job),
                    None => self.sender_dropped(),
                },
                job = self.notices_in.recv(), if notice_room => match job {
                    Some(job) => self.admit(job),
                    None => self.sender_dropped(),
                },
            }
        }
        self.teardown().await;
    }

    /// The sender was dropped without `close`: drain as `close` would.
    fn sender_dropped(&mut self) {
        self.context.shared.phase.send_if_modified(|phase| {
            let open = *phase == Phase::Open;
            if open {
                *phase = Phase::Drain;
            }
            open
        });
    }

    fn admit(&mut self, job: Job) {
        match &job.kind {
            JobKind::Request { key, .. } => {
                if let Some(index) = self.cancelled_early.iter().position(|early| early == key) {
                    // Cancelled before it was admitted: never sent.
                    self.cancelled_early.remove(index);
                    return;
                }
                self.waiting_requests.push_back(job);
            }
            JobKind::Notice { cancels, .. } => {
                if let Some(key) = cancels {
                    self.cancel(key.clone());
                }
                self.waiting_notices.push_back(job);
            }
        }
    }

    /// The client gave up on a request: withdraw it if it was never sent,
    /// or stop reading its answer. Applied when the cancellation is taken,
    /// before its own POST is sent, so it does not wait for a notification
    /// slot.
    fn cancel(&mut self, key: String) {
        if let Some(index) = self.waiting_requests.iter().position(
            |waiting| matches!(&waiting.kind, JobKind::Request { key: k, .. } if *k == key),
        ) {
            self.waiting_requests.remove(index);
        } else if let Some(handle) = self.requests.remove(&key) {
            handle.abort();
        } else {
            // Either still in `requests_in` or already finished. Request ids
            // are not reused, so a finished one's entry only ages out.
            if self.cancelled_early.len() >= self.context.config.limits.max_in_flight_requests.get()
            {
                self.cancelled_early.pop_front();
            }
            self.cancelled_early.push_back(key);
        }
    }

    fn spawn_ready(&mut self, requests: bool) {
        let limits = self.context.config.limits;
        while requests && self.running_requests < limits.max_in_flight_requests.get() {
            let Some(job) = self.waiting_requests.pop_front() else {
                break;
            };
            let JobKind::Request {
                id,
                key,
                initialize,
            } = job.kind
            else {
                continue;
            };
            let context = self.context.clone();
            let handle = self.tasks.spawn(context.request(job.bytes, id, initialize));
            self.kinds
                .insert(handle.id(), TaskKind::Request { key: key.clone() });
            self.requests.insert(key, handle);
            self.running_requests += 1;
        }
        while self.running_notices < limits.max_in_flight_notifications.get() {
            let Some(job) = self.waiting_notices.pop_front() else {
                break;
            };
            let initialized = matches!(
                job.kind,
                JobKind::Notice {
                    initialized: true,
                    ..
                }
            );
            let context = self.context.clone();
            let handle = self.tasks.spawn(context.notice(job.bytes, initialized));
            self.kinds.insert(handle.id(), TaskKind::Notice);
            self.running_notices += 1;
        }
    }

    fn reap(&mut self, joined: Result<(Id, TaskEnd), JoinError>, open: bool) {
        let (id, end) = match joined {
            Ok((id, end)) => (id, Some(end)),
            Err(error) => {
                if error.is_panic() {
                    record_failure(&self.context.shared, HttpError::Internal);
                }
                (error.id(), None)
            }
        };
        match self.kinds.remove(&id) {
            Some(TaskKind::Request { key }) => {
                self.running_requests -= 1;
                if self.requests.get(&key).map(AbortHandle::id) == Some(id) {
                    self.requests.remove(&key);
                }
            }
            Some(TaskKind::Notice) => self.running_notices -= 1,
            Some(TaskKind::ServerStream) => self.server_stream = None,
            None => {}
        }
        if open
            && matches!(end, Some(TaskEnd::Initialized))
            && self.context.config.open_server_stream
            && !self.server_stream_started
        {
            self.server_stream_started = true;
            let handle = self.tasks.spawn(self.context.clone().server_stream());
            self.kinds.insert(handle.id(), TaskKind::ServerStream);
            self.server_stream = Some(handle);
        }
    }

    async fn teardown(mut self) {
        // Refuse new work first: the sender sees the closed channels.
        self.requests_in.close();
        self.notices_in.close();
        self.waiting_requests.clear();
        for handle in self.requests.values() {
            handle.abort();
        }
        if let Some(handle) = self.server_stream.take() {
            handle.abort();
        }
        if *self.phase.borrow_and_update() == Phase::Drain {
            // Admitted notifications and answers are still sent; requests
            // that never started are dropped, since the client has failed
            // them.
            while let Ok(job) = self.notices_in.try_recv() {
                self.waiting_notices.push_back(job);
            }
            loop {
                self.spawn_ready(false);
                if self.running_notices == 0 && self.waiting_notices.is_empty() {
                    break;
                }
                tokio::select! {
                    biased;
                    changed = self.phase.changed() => {
                        if changed.is_err() || *self.phase.borrow_and_update() == Phase::Abort {
                            break;
                        }
                    }
                    Some(joined) = self.tasks.join_next_with_id() => self.reap(joined, false),
                }
            }
        }
        self.tasks.abort_all();
        while let Some(joined) = self.tasks.join_next_with_id().await {
            self.reap(joined, false);
        }
        let Context {
            client,
            config,
            shared,
            incoming,
        } = self.context;
        // No task is left to send, so the receiver ends once it has read the
        // queue.
        drop(incoming);
        let outcome = delete_session(&client, &config, &shared).await;
        let outcome = match outcome {
            SessionClose::Failed(error) => SessionClose::Failed(shared.redact(error)),
            outcome => outcome,
        };
        *lock(&shared.session_close) = Some(outcome);
        drop(client);
        shared.done.send_replace(true);
    }
}

async fn delete_session(
    client: &reqwest::Client,
    config: &Config,
    shared: &Shared,
) -> SessionClose {
    if matches!(
        *lock(&shared.failure),
        Some(HttpError::SessionExpired { .. })
    ) {
        return SessionClose::Expired;
    }
    let Some(session) = lock(&shared.session).clone() else {
        return SessionClose::NoSession;
    };
    // The headers given at start, never newly obtained credentials.
    let mut headers = config.headers.clone();
    headers.insert(SESSION_HEADER, session);
    if let Some(version) = *lock(&shared.version) {
        headers.insert(VERSION_HEADER, HeaderValue::from_static(version.as_str()));
    }
    let request = client
        .request(Method::DELETE, config.url.clone())
        .headers(headers)
        .send();
    match tokio::time::timeout(config.delete_timeout, request).await {
        Err(_) => SessionClose::Failed(HttpError::Timeout),
        Ok(Err(error)) => SessionClose::Failed(HttpError::network(error)),
        Ok(Ok(response)) => match response.status() {
            status if status.is_success() => SessionClose::Deleted {
                status: status.as_u16(),
            },
            StatusCode::METHOD_NOT_ALLOWED => SessionClose::NotAllowed,
            status => SessionClose::Rejected {
                status: status.as_u16(),
            },
        },
    }
}

/// What a task's exchange ended with, short of success.
enum Failure {
    /// The receiver is gone; nobody is waiting for anything.
    Gone,
    /// The exchange failed.
    Http(HttpError),
    /// The request's response stream failed.
    Stream(HttpError),
}

impl From<HttpError> for Failure {
    fn from(error: HttpError) -> Self {
        Self::Http(error)
    }
}

/// A stream's resumption state, as Pi's `StreamCursor`.
#[derive(Debug, Default)]
struct Cursor {
    last_id: Option<HeaderValue>,
    retry: Option<Duration>,
    /// Whether the stream delivered an event since it was (re)opened.
    received: bool,
}

/// What one task needs; each task holds a clone, and the supervisor drops
/// its own after every task has ended.
#[derive(Clone)]
struct Context {
    client: reqwest::Client,
    config: Arc<Config>,
    shared: Arc<Shared>,
    incoming: mpsc::Sender<Vec<u8>>,
}

impl Context {
    /// The caller's headers with the session and protocol version, and
    /// whether the session ID is among them.
    fn headers(&self) -> (HeaderMap, bool) {
        let mut headers = self.config.headers.clone();
        let session = lock(&self.shared.session).clone();
        let carried = session.is_some();
        if let Some(session) = session {
            headers.insert(SESSION_HEADER, session);
        }
        if let Some(version) = *lock(&self.shared.version) {
            headers.insert(VERSION_HEADER, HeaderValue::from_static(version.as_str()));
        }
        (headers, carried)
    }

    async fn post(&self, bytes: Vec<u8>) -> Result<Response, HttpError> {
        let (mut headers, carried) = self.headers();
        headers.insert(header::ACCEPT, HeaderValue::from_static(POST_ACCEPT));
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(JSON));
        let response = self
            .client
            .post(self.config.url.clone())
            .headers(headers)
            .body(bytes)
            .send()
            .await
            .map_err(HttpError::network)?;
        check(response, carried).await
    }

    /// Open a GET stream, resuming after `last_id`. `None` when the server
    /// answers 405: it offers no GET stream.
    async fn open_stream(
        &self,
        last_id: Option<&HeaderValue>,
    ) -> Result<Option<Response>, HttpError> {
        let (mut headers, carried) = self.headers();
        headers.insert(header::ACCEPT, HeaderValue::from_static(EVENT_STREAM));
        if let Some(last_id) = last_id {
            headers.insert(LAST_EVENT_ID_HEADER, last_id.clone());
        }
        let response = self
            .client
            .get(self.config.url.clone())
            .headers(headers)
            .send()
            .await
            .map_err(HttpError::network)?;
        if response.status() == StatusCode::METHOD_NOT_ALLOWED {
            return Ok(None);
        }
        let response = check(response, carried).await?;
        let content_type = content_type(&response);
        if content_type.as_deref() != Some(EVENT_STREAM) {
            return Err(HttpError::UnexpectedContentType {
                status: response.status().as_u16(),
                content_type,
            });
        }
        Ok(Some(response))
    }

    /// One request's POST, through its response.
    async fn request(self, bytes: Vec<u8>, id: Value, initialize: bool) -> TaskEnd {
        let failure = match self.exchange(bytes, &id, initialize).await {
            Ok(()) => return TaskEnd::Done,
            Err(failure) => failure,
        };
        let message = match failure {
            Failure::Gone => return TaskEnd::Done,
            Failure::Http(error) | Failure::Stream(error) if error.is_fatal() => {
                record_failure(&self.shared, error);
                return TaskEnd::Done;
            }
            Failure::Http(error) => self.shared.redact(error).to_string(),
            Failure::Stream(error) => {
                format!("MCP response stream failed: {}", self.shared.redact(error))
            }
        };
        let _ = self.incoming.send(error_message(&id, message)).await;
        TaskEnd::Done
    }

    async fn exchange(&self, bytes: Vec<u8>, id: &Value, initialize: bool) -> Result<(), Failure> {
        let response = self.post(bytes).await?;
        if initialize {
            self.capture_session(&response)?;
        }
        let status = response.status();
        if status == StatusCode::ACCEPTED || status == StatusCode::NO_CONTENT {
            return Err(HttpError::AcceptedWithoutResponse {
                status: status.as_u16(),
            }
            .into());
        }
        match content_type(&response).as_deref() {
            Some(JSON) => self.json_body(response, id, initialize).await,
            Some(EVENT_STREAM) => self.response_stream(response, id, initialize).await,
            other => Err(HttpError::UnexpectedContentType {
                status: status.as_u16(),
                content_type: other.map(str::to_owned),
            }
            .into()),
        }
    }

    /// Keep the session ID of the initialize response. The specification
    /// assigns it there only, so a later response cannot rebind the session
    /// (Pi takes it from any response).
    fn capture_session(&self, response: &Response) -> Result<(), HttpError> {
        let Some(value) = response.headers().get(SESSION_HEADER) else {
            return Ok(());
        };
        if value.is_empty()
            || !value
                .as_bytes()
                .iter()
                .all(|byte| (0x21..=0x7e).contains(byte))
        {
            return Err(HttpError::InvalidSessionId);
        }
        let mut value = value.clone();
        value.set_sensitive(true);
        *lock(&self.shared.session) = Some(value);
        Ok(())
    }

    async fn json_body(
        &self,
        mut response: Response,
        id: &Value,
        initialize: bool,
    ) -> Result<(), Failure> {
        let limit = self.config.limits.max_message_bytes.get();
        let too_large = HttpError::MessageTooLarge { limit };
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(too_large.into());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(HttpError::network)? {
            if body.len().saturating_add(chunk.len()) > limit {
                return Err(too_large.into());
            }
            body.extend_from_slice(&chunk);
        }
        drop(response);
        let value: Value =
            serde_json::from_slice(&body).map_err(|error| HttpError::InvalidJson {
                detail: error.to_string(),
            })?;
        let mut answered = false;
        // Pi accepts a batch body and passes each of its messages on.
        match value {
            Value::Array(items) => {
                for item in items {
                    answered |= self.observe(&item, id, initialize);
                    self.deliver(item.to_string().into_bytes()).await?;
                }
            }
            value => {
                answered = self.observe(&value, id, initialize);
                self.deliver(body).await?;
            }
        }
        if answered {
            Ok(())
        } else {
            Err(HttpError::NoResponse.into())
        }
    }

    /// Read the SSE stream answering one request. When it ends or breaks
    /// before the response and the server gave an event ID, resume it with
    /// GET and `Last-Event-ID`, as Pi does; otherwise the request fails.
    /// The task ends at the response (Pi reads on to the stream's end), so
    /// an answered request does not hold its place.
    async fn response_stream(
        &self,
        response: Response,
        id: &Value,
        initialize: bool,
    ) -> Result<(), Failure> {
        let reconnect = self.config.reconnect;
        let mut cursor = Cursor::default();
        let mut stream = Some(response);
        let mut failure: Option<HttpError> = None;
        let mut attempt = 0_u32;
        loop {
            if let Some(response) = stream.take() {
                match self
                    .read_events(response, &mut cursor, Some((id, initialize)))
                    .await
                {
                    Ok(true) => return Ok(()),
                    Ok(false) => failure = None,
                    Err(Failure::Http(error) | Failure::Stream(error)) => failure = Some(error),
                    Err(Failure::Gone) => return Err(Failure::Gone),
                }
            }
            if failure.as_ref().is_some_and(|error| !error.is_retryable()) {
                break;
            }
            let Some(last_id) = cursor.last_id.clone() else {
                break;
            };
            if attempt >= reconnect.max_retries {
                break;
            }
            if cursor.received {
                attempt = 0;
            }
            cursor.received = false;
            tokio::time::sleep(self.delay(attempt, cursor.retry)).await;
            attempt += 1;
            match self.open_stream(Some(&last_id)).await {
                Ok(Some(response)) => stream = Some(response),
                Ok(None) => {
                    failure = Some(HttpError::ResumptionUnsupported);
                    break;
                }
                Err(error) if error.is_fatal() => return Err(error.into()),
                Err(error) => {
                    let retry = error.is_retryable();
                    failure = Some(error);
                    if !retry {
                        break;
                    }
                }
            }
        }
        Err(Failure::Stream(failure.unwrap_or(HttpError::StreamEnded)))
    }

    /// Keep the server-to-client stream open, reconnecting with backoff
    /// when it drops, as Pi's `runGetStream`.
    async fn server_stream(self) -> TaskEnd {
        let reconnect = self.config.reconnect;
        let mut cursor = Cursor::default();
        let mut attempt = 0_u32;
        loop {
            match self.open_stream(cursor.last_id.as_ref()).await {
                Ok(None) => return TaskEnd::Done,
                Ok(Some(response)) => {
                    let opened = Instant::now();
                    match self.read_events(response, &mut cursor, None).await {
                        Ok(_) => {}
                        Err(Failure::Gone) => return TaskEnd::Done,
                        Err(Failure::Http(error) | Failure::Stream(error)) => {
                            if !error.is_retryable() {
                                self.diagnostic(&error);
                                return TaskEnd::Done;
                            }
                        }
                    }
                    // A stream that stayed up a while counts as healthy, even
                    // if it was idle.
                    if cursor.received || opened.elapsed() > reconnect.max_delay {
                        attempt = 0;
                    }
                }
                Err(error) if error.is_fatal() => {
                    record_failure(&self.shared, error);
                    return TaskEnd::Done;
                }
                Err(error) => {
                    if !error.is_retryable() {
                        self.diagnostic(&error);
                        return TaskEnd::Done;
                    }
                }
            }
            cursor.received = false;
            if attempt >= reconnect.max_retries {
                self.diagnostic(&HttpError::ServerStreamLost);
                return TaskEnd::Done;
            }
            tokio::time::sleep(self.delay(attempt, cursor.retry)).await;
            attempt += 1;
        }
    }

    /// Pass on a stream's events until it ends. Returns whether the
    /// response to `answer`'s request arrived; the stream is not read past
    /// it.
    async fn read_events(
        &self,
        mut response: Response,
        cursor: &mut Cursor,
        answer: Option<(&Value, bool)>,
    ) -> Result<bool, Failure> {
        let limit = self.config.limits.max_message_bytes.get();
        let too_large = |TooLarge { limit }| HttpError::MessageTooLarge { limit };
        let mut decoder = SseDecoder::new(limit);
        loop {
            let Some(chunk) = response.chunk().await.map_err(HttpError::network)? else {
                for item in decoder.finish().map_err(too_large)? {
                    if self.event(item, cursor, answer).await? {
                        return Ok(true);
                    }
                }
                return Ok(false);
            };
            let mut input = &chunk[..];
            while let Some(item) = decoder.next(&mut input).map_err(too_large)? {
                if self.event(item, cursor, answer).await? {
                    return Ok(true);
                }
            }
        }
    }

    async fn event(
        &self,
        item: SseItem,
        cursor: &mut Cursor,
        answer: Option<(&Value, bool)>,
    ) -> Result<bool, Failure> {
        match item {
            SseItem::Id(id) => cursor.last_id = Some(id),
            SseItem::Retry(delay) => cursor.retry = Some(Duration::from_millis(delay)),
            SseItem::Event { message, data } => {
                cursor.received = true;
                // Events without data prime resumption; other event types are
                // not JSON-RPC.
                if !message || data.iter().all(u8::is_ascii_whitespace) {
                    return Ok(false);
                }
                let answered = answer.is_some_and(|(id, initialize)| {
                    serde_json::from_slice::<Value>(&data)
                        .is_ok_and(|value| self.observe(&value, id, initialize))
                });
                self.deliver(data).await?;
                return Ok(answered);
            }
        }
        Ok(false)
    }

    /// Whether `message` answers request `id`. The initialize response's
    /// supported protocol version is recorded before the client can see the
    /// response, so every later request carries it.
    fn observe(&self, message: &Value, id: &Value, initialize: bool) -> bool {
        let answers = message.get("method").is_none()
            && message.get("id") == Some(id)
            && (message.get("result").is_some() || message.get("error").is_some());
        if answers
            && initialize
            && let Some(version) = message
                .pointer("/result/protocolVersion")
                .and_then(Value::as_str)
                .and_then(ProtocolVersion::parse)
        {
            *lock(&self.shared.version) = Some(version);
        }
        answers
    }

    /// Queue a message for the receiver, waiting while the queue is full.
    async fn deliver(&self, message: Vec<u8>) -> Result<(), Failure> {
        self.incoming.send(message).await.map_err(|_| Failure::Gone)
    }

    /// Report a failure that has no request to fail, without waiting.
    fn diagnostic(&self, error: &HttpError) {
        let message = error_message(&Value::Null, self.shared.redact(error.clone()).to_string());
        if let Err(mpsc::error::TrySendError::Full(_)) = self.incoming.try_send(message) {
            self.shared
                .dropped_diagnostics
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    /// One notification or answer POST, within the notification timeout.
    async fn notice(self, bytes: Vec<u8>, initialized: bool) -> TaskEnd {
        match tokio::time::timeout(self.config.notification_timeout, self.post(bytes)).await {
            Ok(Ok(response)) => {
                // Accepted; any body is ignored.
                drop(response);
                if initialized {
                    return TaskEnd::Initialized;
                }
            }
            Ok(Err(error)) if error.is_fatal() => record_failure(&self.shared, error),
            Ok(Err(error)) => self.diagnostic(&error),
            Err(_) => self.diagnostic(&HttpError::Timeout),
        }
        TaskEnd::Done
    }

    fn delay(&self, attempt: u32, server: Option<Duration>) -> Duration {
        backoff(self.config.reconnect, attempt, server)
    }
}

/// The server's `retry` if it sent one, else Pi's exponential backoff.
fn backoff(reconnect: Reconnect, attempt: u32, server: Option<Duration>) -> Duration {
    if let Some(server) = server {
        return server;
    }
    let factor = 1_u32.checked_shl(attempt).unwrap_or(u32::MAX);
    reconnect
        .initial_delay
        .saturating_mul(factor)
        .min(reconnect.max_delay)
}

/// A JSON-RPC error response for `id` (`null` for a diagnostic).
fn error_message(id: &Value, message: String) -> Vec<u8> {
    let mut error = Map::new();
    error.insert("code".into(), error_codes::INTERNAL_ERROR.into());
    error.insert("message".into(), message.into());
    let mut response = Map::new();
    response.insert("jsonrpc".into(), "2.0".into());
    response.insert("id".into(), id.clone());
    response.insert("error".into(), Value::Object(error));
    Value::Object(response).to_string().into_bytes()
}

fn content_type(response: &Response) -> Option<String> {
    let value = response.headers().get(header::CONTENT_TYPE)?;
    let text = String::from_utf8_lossy(value.as_bytes());
    let essence = text
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    Some(preview(&essence, 100))
}

/// Pass a successful response; classify the rest, reading at most 8 KiB of
/// their body.
async fn check(response: Response, carried_session: bool) -> Result<Response, HttpError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    if status.is_redirection() {
        return Err(HttpError::Redirect {
            status: status.as_u16(),
        });
    }
    let challenge = response
        .headers()
        .get(header::WWW_AUTHENTICATE)
        .map(|value| {
            preview(
                &String::from_utf8_lossy(value.as_bytes()),
                MAX_CHALLENGE_CHARS,
            )
        });
    let body = error_body(response).await;
    if status == StatusCode::UNAUTHORIZED
        || (status == StatusCode::FORBIDDEN && challenge.as_deref().is_some_and(insufficient_scope))
    {
        return Err(HttpError::AuthorizationRequired {
            status: status.as_u16(),
            challenge,
            body,
        });
    }
    if status == StatusCode::NOT_FOUND && carried_session {
        return Err(HttpError::SessionExpired { body });
    }
    Err(HttpError::Status {
        status: status.as_u16(),
        body,
    })
}

async fn error_body(mut response: Response) -> String {
    let mut body = Vec::new();
    let read = async {
        while body.len() < MAX_ERROR_BODY_BYTES {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    let room = MAX_ERROR_BODY_BYTES - body.len();
                    body.extend_from_slice(&chunk[..chunk.len().min(room)]);
                }
                _ => break,
            }
        }
    };
    let _ = tokio::time::timeout(ERROR_BODY_TIMEOUT, read).await;
    String::from_utf8_lossy(&body).into_owned()
}

/// Pi's `/(?:^|[\s,])error="?insufficient_scope"?/i` over a bearer
/// challenge.
fn insufficient_scope(challenge: &str) -> bool {
    let lower = challenge.to_ascii_lowercase();
    let mut from = 0;
    while let Some(offset) = lower[from..].find("error=") {
        let start = from + offset;
        let boundary = lower[..start]
            .chars()
            .next_back()
            .is_none_or(|before| before.is_whitespace() || before == ',');
        let value = &lower[start + "error=".len()..];
        let value = value.strip_prefix('"').unwrap_or(value);
        if boundary && value.starts_with("insufficient_scope") {
            return true;
        }
        from = start + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insufficient_scope_matches_pi() {
        assert!(insufficient_scope(
            r#"Bearer error="insufficient_scope", scope="admin""#
        ));
        assert!(insufficient_scope(
            "Bearer realm=x,error=insufficient_scope"
        ));
        assert!(insufficient_scope(r#"Bearer ERROR="INSUFFICIENT_SCOPE""#));
        assert!(!insufficient_scope(r#"Bearer error="invalid_token""#));
        assert!(!insufficient_scope(r#"Bearer xerror="insufficient_scope""#));
        assert!(!insufficient_scope("Bearer"));
    }

    #[test]
    fn options_refuse_credentials_reserved_headers_and_bad_urls() {
        let start = |options: HttpOptions| Config::new(&options).map(drop);
        assert_eq!(
            start(HttpOptions::new("https://user:secret@example.com/mcp")),
            Err(HttpOptionsError::CredentialsInUrl)
        );
        assert_eq!(
            start(HttpOptions::new("ftp://example.com/mcp")),
            Err(HttpOptionsError::UnsupportedScheme)
        );
        assert_eq!(
            start(HttpOptions::new("not a url")),
            Err(HttpOptionsError::InvalidUrl)
        );
        assert_eq!(
            start(HttpOptions::new("https://example.com/mcp").header("MCP-Session-Id", "x")),
            Err(HttpOptionsError::ReservedHeader {
                name: "mcp-session-id".into()
            })
        );
        assert_eq!(
            start(HttpOptions::new("https://example.com/mcp").header("x-key", "a\nb")),
            Err(HttpOptionsError::InvalidHeaderValue {
                name: "x-key".into()
            })
        );
        assert_eq!(
            start(HttpOptions::new("https://example.com/mcp").header("bad name", "v")),
            Err(HttpOptionsError::InvalidHeaderName { index: 0 })
        );
        assert!(
            start(HttpOptions::new("http://127.0.0.1:1/mcp").header("Authorization", "Bearer t"))
                .is_ok()
        );
    }

    #[test]
    fn debug_output_shows_neither_the_url_path_nor_header_values() {
        let options = HttpOptions::new("https://example.com/mcp?token=query-secret")
            .header("Authorization", "Bearer header-secret");
        let text = format!("{options:?}");
        assert!(text.contains("https://example.com"), "{text}");
        assert!(text.contains("Authorization"), "{text}");
        assert!(!text.contains("secret"), "{text}");
        let config = Config::new(&options).unwrap();
        let text = format!("{config:?}");
        assert!(!text.contains("header-secret"), "{text}");
    }

    #[test]
    fn jobs_are_classified_once() {
        let job = Job::new(br#"{"jsonrpc":"2.0","id":3,"method":"initialize"}"#.to_vec());
        assert!(
            matches!(job.kind, JobKind::Request { ref key, initialize: true, .. } if key == "3")
        );
        let job = Job::new(
            br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":3}}"#
                .to_vec(),
        );
        assert!(
            matches!(job.kind, JobKind::Notice { cancels: Some(ref key), initialized: false } if key == "3")
        );
        let job = Job::new(br#"{"jsonrpc":"2.0","id":"s1","result":{}}"#.to_vec());
        assert!(matches!(
            job.kind,
            JobKind::Notice {
                cancels: None,
                initialized: false
            }
        ));
    }

    #[test]
    fn backoff_doubles_up_to_the_cap_and_the_server_retry_wins() {
        let reconnect = Reconnect {
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(1_000),
            max_retries: 5,
        };
        let delay = |attempt, server| backoff(reconnect, attempt, server);
        assert_eq!(delay(0, None), Duration::from_millis(100));
        assert_eq!(delay(3, None), Duration::from_millis(800));
        assert_eq!(delay(4, None), Duration::from_millis(1_000));
        assert_eq!(delay(40, None), Duration::from_millis(1_000));
        assert_eq!(
            delay(0, Some(Duration::from_millis(5))),
            Duration::from_millis(5)
        );
    }
}
