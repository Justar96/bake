//! The MCP client: one connection's requests, responses, notifications, and
//! shutdown, run by a driver task the client owns.
//!
//! Ported from Pi's `McpClient` (`packages/mcp/src/client.ts`, v1.3.0). Pi
//! keeps its pending requests in maps that transport callbacks update. Here a
//! single driver task owns every piece of connection state (the pending
//! requests, the outgoing queue, both transport halves) and the
//! [`McpClient`] handle reaches it only through bounded channels, so no lock
//! guards protocol state and no second state machine exists.
//!
//! # Bounds
//!
//! Every queue has a limit from [`Limits`]:
//!
//! - In-flight requests: a semaphore of `max_pending_requests` permits. A
//!   caller waits for a permit (inside its timeout), and a permit is released
//!   only when its response arrives or the driver has processed its
//!   cancellation, so the pending map and the cancellation channel together
//!   hold at most `max_pending_requests` entries.
//! - Outgoing messages: `max_queued_messages` is a threshold, not a
//!   capacity. The driver takes new requests and reads new incoming messages
//!   only while fewer than that many messages wait to be written, so a server
//!   that floods requests without reading answers is slowed by its own pipe
//!   instead of growing the queue. Cancellation notices are queued past the
//!   threshold, at most one per written request, so the queue holds at most
//!   `max_queued_messages + max_pending_requests` messages.
//! - Message size: `max_message_bytes` both ways; an incoming message over it
//!   closes the connection, and an outgoing one fails its request.
//! - Events: `max_events`; when the consumer falls behind, further events are
//!   dropped and counted ([`McpClient::dropped_events`]), and the driver never
//!   waits for the consumer.
//! - Tool lists: `max_list_pages`, `max_listed_tools`, and `max_list_bytes`,
//!   with repeated cursors refused.
//!
//! # Cancellation and shutdown
//!
//! Dropping a request's future cancels it. A request the driver has not yet
//! written is removed from the queue and never sent; one already written is
//! followed by `notifications/cancelled`, except `initialize`, which the
//! specification forbids cancelling. A response that arrives after its
//! request was cancelled is ignored, as the specification asks.
//!
//! [`McpClient::shutdown`] stops the driver and waits for its task to end:
//! pending requests fail with [`CloseReason::Shutdown`], queued notifications
//! and answers are written within [`ClientOptions::shutdown_timeout`],
//! queued requests nobody waits for are discarded, the sender is closed, and
//! both transport halves are dropped before it returns. Dropping the client
//! instead signals the same shutdown without waiting for it. The driver checks
//! for shutdown before each pass over pending cancellations and handles at
//! most `max_pending_requests` of them per pass, so callers that keep
//! cancelling requests cannot hold shutdown off.
//!
//! # Ownership during initialization
//!
//! [`McpClient::start`] validates the options and starts the driver, and the
//! [`Connecting`] it returns owns that task until
//! [`Connecting::initialize`] hands it to a client. A session that must
//! release the transport before it reaps the server process keeps the
//! `Connecting`: if initialization is cancelled, [`Connecting::shutdown`]
//! waits for the driver as [`McpClient::shutdown`] does. [`McpClient::connect`]
//! combines both steps and leaves the driver without an owner if its future
//! is dropped.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::future::Future;
use std::io;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::jsonrpc::{self, Incoming, JsonRpcError, Outcome, RequestId, error_codes, preview};
use crate::protocol::{
    self, CallToolResult, Implementation, InitializeRejection, InitializeResult, ProtocolVersion,
    Tool, ToolsPage,
};
use crate::transport::{DEFAULT_MAX_MESSAGE_BYTES, MessageReceiver, MessageSender, ReceiveError};

/// Pi's default request timeout (`DEFAULT_REQUEST_TIMEOUT_MS`).
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a shutdown may spend writing queued messages and closing.
const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const CANCELLED_REASON: &str = "Request cancelled";
/// Pi's reason for a timed-out request.
const TIMED_OUT_REASON: &str = "Request timed out";

const fn nonzero(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    }
}

/// The bounds a connection holds itself to; see the [module
/// documentation](self) for how each applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The largest message, in bytes, either way. Default: 16 MiB, as Pi.
    /// Give the same value to the [`LineReader`](crate::LineReader) so an
    /// oversized line is refused while it is read.
    pub max_message_bytes: NonZeroUsize,
    /// Requests in flight at once. Default: 64.
    pub max_pending_requests: NonZeroUsize,
    /// Messages waiting to be written before the driver stops taking
    /// requests and reading. Default: 64.
    pub max_queued_messages: NonZeroUsize,
    /// Events waiting for the consumer. Default: 256.
    pub max_events: NonZeroUsize,
    /// Pages one [`McpClient::list_tools`] call follows. Default: 1,000, as
    /// Pi's `MAX_LIST_PAGES`.
    pub max_list_pages: NonZeroUsize,
    /// Tools one [`McpClient::list_tools`] call collects. Default: 10,000.
    pub max_listed_tools: NonZeroUsize,
    /// The total size of one [`McpClient::list_tools`] call's pages, in
    /// bytes. Default: 64 MiB.
    pub max_list_bytes: NonZeroUsize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_message_bytes: nonzero(DEFAULT_MAX_MESSAGE_BYTES),
            max_pending_requests: nonzero(64),
            max_queued_messages: nonzero(64),
            max_events: nonzero(256),
            max_list_pages: nonzero(1_000),
            max_listed_tools: nonzero(10_000),
            max_list_bytes: nonzero(64 * 1024 * 1024),
        }
    }
}

/// How a client connects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientOptions {
    /// The client's name and version, sent as `clientInfo`.
    pub client_info: Implementation,
    /// The version requested in `initialize`. Default:
    /// [`ProtocolVersion::LATEST`].
    pub protocol_version: ProtocolVersion,
    /// Each request's deadline, covering the wait for a permit and for the
    /// response; `None` waits without one. Default: 30 s, as Pi.
    pub request_timeout: Option<Duration>,
    /// How long a shutdown may spend writing queued messages and closing the
    /// sender before it drops them. Default: 2 s.
    pub shutdown_timeout: Duration,
    /// The connection's bounds.
    pub limits: Limits,
}

impl ClientOptions {
    /// The defaults for a client with this name and version.
    pub fn new(client_info: Implementation) -> Self {
        Self {
            client_info,
            protocol_version: ProtocolVersion::LATEST,
            request_timeout: Some(DEFAULT_REQUEST_TIMEOUT),
            shutdown_timeout: DEFAULT_SHUTDOWN_TIMEOUT,
            limits: Limits::default(),
        }
    }

    /// Check that every capacity fits Tokio's channels and semaphores
    /// ([`Semaphore::MAX_PERMITS`]) and that every timeout gives a deadline
    /// the clock can represent from now. [`McpClient::start`] calls it before
    /// starting anything; options read from configuration can be checked
    /// with it earlier.
    pub fn validate(&self) -> Result<(), OptionsError> {
        let capacities = [
            (
                "limits.max_pending_requests",
                self.limits.max_pending_requests,
            ),
            ("limits.max_events", self.limits.max_events),
        ];
        for (field, value) in capacities {
            if value.get() > Semaphore::MAX_PERMITS {
                return Err(OptionsError::CapacityTooLarge {
                    field,
                    value: value.get(),
                    max: Semaphore::MAX_PERMITS,
                });
            }
        }
        let now = Instant::now();
        let durations = [
            ("request_timeout", self.request_timeout),
            ("shutdown_timeout", Some(self.shutdown_timeout)),
        ];
        for (field, value) in durations {
            if let Some(value) = value
                && now.checked_add(value).is_none()
            {
                return Err(OptionsError::DurationTooLong { field, value });
            }
        }
        Ok(())
    }
}

/// Why [`ClientOptions::validate`] refused options.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum OptionsError {
    /// A capacity is larger than Tokio supports.
    CapacityTooLarge {
        /// The option's path, such as `limits.max_events`.
        field: &'static str,
        /// The value given.
        value: usize,
        /// The largest value accepted.
        max: usize,
    },
    /// A timeout reaches past what the clock can represent.
    DurationTooLong {
        /// The option's name.
        field: &'static str,
        /// The value given.
        value: Duration,
    },
}

impl fmt::Display for OptionsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapacityTooLarge { field, value, max } => {
                write!(
                    f,
                    "{field} is {value}, over the largest supported capacity, {max}"
                )
            }
            Self::DurationTooLong { field, value } => {
                write!(
                    f,
                    "{field} is {value:?}, too long for the clock to represent"
                )
            }
        }
    }
}

impl std::error::Error for OptionsError {}

/// Why a connection closed.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CloseReason {
    /// [`McpClient::shutdown`] was called or the client was dropped.
    Shutdown,
    /// The server ended its output.
    EndOfStream,
    /// The server's output ended inside a message.
    IncompleteMessage {
        /// The unfinished message's bytes.
        bytes: usize,
    },
    /// The server sent a message over the limit.
    MessageTooLarge {
        /// The limit in bytes.
        limit: usize,
    },
    /// Reading from the server failed.
    Receive {
        /// The I/O error's kind.
        kind: io::ErrorKind,
        /// The I/O error's message.
        message: String,
    },
    /// Writing to the server failed.
    Send {
        /// The I/O error's kind.
        kind: io::ErrorKind,
        /// The I/O error's message.
        message: String,
    },
    /// The driver task ended without recording a reason: a transport
    /// implementation panicked.
    DriverFailed,
}

impl CloseReason {
    fn from_receive(error: ReceiveError) -> Self {
        match error {
            ReceiveError::TooLarge { limit } => Self::MessageTooLarge { limit },
            ReceiveError::Incomplete { bytes } => Self::IncompleteMessage { bytes },
            ReceiveError::Io(error) => Self::Receive {
                kind: error.kind(),
                message: error.to_string(),
            },
        }
    }
}

impl fmt::Display for CloseReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shutdown => f.write_str("the MCP client was shut down"),
            Self::EndOfStream => f.write_str("the MCP server closed the connection"),
            Self::IncompleteMessage { bytes } => write!(
                f,
                "the MCP server's output ended inside a message ({bytes} bytes without a newline)"
            ),
            Self::MessageTooLarge { limit } => {
                write!(
                    f,
                    "the MCP server sent a message over the {limit}-byte limit"
                )
            }
            Self::Receive { message, .. } => {
                write!(f, "reading from the MCP server failed: {message}")
            }
            Self::Send { message, .. } => write!(f, "writing to the MCP server failed: {message}"),
            Self::DriverFailed => f.write_str("the MCP client's driver task failed"),
        }
    }
}

/// Why a request failed.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum RequestError {
    /// The server answered with a JSON-RPC error.
    Server {
        /// The request's method.
        method: &'static str,
        /// The server's error.
        error: JsonRpcError,
    },
    /// No response arrived before the request's deadline. The server was
    /// sent `notifications/cancelled` if the request had been written.
    Timeout {
        /// The request's method.
        method: &'static str,
        /// The deadline's length.
        after: Duration,
    },
    /// The connection closed first.
    Closed(CloseReason),
    /// The request is larger than the message limit and was not sent.
    MessageTooLarge {
        /// The request's method.
        method: &'static str,
        /// The encoded request's bytes.
        size: usize,
        /// The limit.
        limit: usize,
    },
    /// The response did not have a JSON-RPC response's shape.
    InvalidResponse {
        /// The request's method.
        method: &'static str,
        /// What was wrong.
        detail: String,
    },
    /// The result did not have the method's result shape.
    InvalidResult {
        /// The request's method.
        method: &'static str,
        /// What was wrong.
        detail: String,
    },
    /// A paginated list returned a cursor it had returned before.
    RepeatedCursor {
        /// The list method.
        method: &'static str,
        /// The repeated cursor, shortened.
        cursor: String,
    },
    /// A paginated list did not end within the page limit.
    TooManyPages {
        /// The list method.
        method: &'static str,
        /// The limit.
        limit: usize,
    },
    /// A paginated list returned more items than the limit.
    TooManyItems {
        /// The list method.
        method: &'static str,
        /// The limit.
        limit: usize,
    },
    /// A paginated list's pages exceeded the byte limit in total.
    ListTooLarge {
        /// The list method.
        method: &'static str,
        /// The limit in bytes.
        limit: usize,
    },
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Server { method, error } => {
                write!(f, "the MCP server returned an error for {method}: {error}")
            }
            Self::Timeout { method, after } => write!(
                f,
                "the MCP {method} request timed out after {} ms",
                after.as_millis()
            ),
            Self::Closed(reason) => write!(f, "MCP connection closed: {reason}"),
            Self::MessageTooLarge {
                method,
                size,
                limit,
            } => write!(
                f,
                "the MCP {method} request is {size} bytes, over the {limit}-byte limit"
            ),
            Self::InvalidResponse { method, detail } => {
                write!(f, "invalid MCP {method} response: {detail}")
            }
            Self::InvalidResult { method, detail } => {
                write!(f, "invalid MCP {method} result: {detail}")
            }
            Self::RepeatedCursor { method, cursor } => {
                write!(f, "MCP {method} returned a repeated cursor: {cursor:?}")
            }
            Self::TooManyPages { method, limit } => {
                write!(f, "MCP {method} exceeded {limit} pages")
            }
            Self::TooManyItems { method, limit } => {
                write!(f, "MCP {method} listed more than {limit} items")
            }
            Self::ListTooLarge { method, limit } => {
                write!(f, "MCP {method} pages exceeded {limit} bytes in total")
            }
        }
    }
}

impl std::error::Error for RequestError {}

/// Why [`McpClient::connect`] failed. The driver task has ended and both
/// transport halves are dropped by the time it returns.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ConnectError {
    /// No Tokio runtime is running on this thread.
    NoRuntime,
    /// The options were refused before any task started.
    InvalidOptions(OptionsError),
    /// [`Connecting::initialize`] was already called on this connection, or
    /// the connection was already handed to a client.
    AlreadyAttempted,
    /// `initialize` or `notifications/initialized` failed. A server that
    /// speaks only MCP 2026-07-28 answers `initialize` with a
    /// [`RequestError::Server`] error, whose data names its versions.
    Initialize(RequestError),
    /// The server selected a protocol version this client does not support.
    UnsupportedProtocolVersion {
        /// The server's version, shortened.
        selected: String,
    },
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRuntime => f.write_str("an MCP client needs a running Tokio runtime"),
            Self::InvalidOptions(error) => write!(f, "invalid MCP client options: {error}"),
            Self::AlreadyAttempted => f.write_str(
                "MCP initialization was already attempted on this connection; start a new one",
            ),
            Self::Initialize(error) => write!(f, "MCP initialization failed: {error}"),
            Self::UnsupportedProtocolVersion { selected } => write!(
                f,
                "the MCP server selected unsupported protocol version {selected:?}; this client supports {}",
                ProtocolVersion::SUPPORTED
                    .map(ProtocolVersion::as_str)
                    .join(", ")
            ),
        }
    }
}

impl std::error::Error for ConnectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Initialize(error) => Some(error),
            Self::InvalidOptions(error) => Some(error),
            _ => None,
        }
    }
}

/// Something the server sent that is not a response.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ClientEvent {
    /// A notification other than `notifications/tools/list_changed` (see
    /// [`McpClient::tool_list_changes`]) and `notifications/cancelled`, for
    /// example `notifications/message` log entries.
    Notification {
        /// The notification's method.
        method: String,
        /// Its parameters, if any.
        params: Option<Value>,
    },
    /// A message the client ignored; the connection stays open.
    Diagnostic(Diagnostic),
}

/// A message the client ignored, as Pi reports through `onError` without
/// failing pending requests.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Diagnostic {
    /// A message that is not a JSON-RPC message, such as a stray log line on
    /// standard output.
    MalformedMessage {
        /// Why, without the message's text.
        detail: String,
    },
    /// A response whose id names no request in flight.
    UnknownResponse {
        /// The id, shortened.
        id: String,
    },
    /// A JSON-RPC error with a null id: the server could not read a message.
    ErrorWithoutId(JsonRpcError),
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedMessage { detail } => {
                write!(f, "ignored a malformed MCP message: {detail}")
            }
            Self::UnknownResponse { id } => {
                write!(f, "ignored an MCP response for unknown request {id}")
            }
            Self::ErrorWithoutId(error) => {
                write!(
                    f,
                    "the MCP server reported an error without a request id: {error}"
                )
            }
        }
    }
}

/// The events of one connection, in arrival order. Ends (returns `None`)
/// once the connection has closed and every queued event is read.
#[derive(Debug)]
pub struct ClientEvents {
    receiver: mpsc::Receiver<ClientEvent>,
}

impl ClientEvents {
    /// The next event.
    pub async fn recv(&mut self) -> Option<ClientEvent> {
        self.receiver.recv().await
    }

    /// The next event if one is queued.
    pub fn try_recv(&mut self) -> Option<ClientEvent> {
        self.receiver.try_recv().ok()
    }
}

/// Changes to the server's tool list, announced by
/// `notifications/tools/list_changed`. Notifications that arrive before the
/// consumer looks are coalesced: the consumer re-lists once for all of them.
#[derive(Clone, Debug)]
pub struct ToolListChanges {
    receiver: watch::Receiver<u64>,
}

impl ToolListChanges {
    /// How many change notifications have arrived since the connection opened.
    pub fn generation(&self) -> u64 {
        *self.receiver.borrow()
    }

    /// Wait for a change this handle has not seen, and return the new
    /// generation. Returns `None` once the connection has closed and every
    /// change has been seen. A handle from [`McpClient::tool_list_changes`]
    /// has seen none, so a change during the first listing is not lost.
    pub async fn changed(&mut self) -> Option<u64> {
        self.receiver.changed().await.ok()?;
        Some(*self.receiver.borrow_and_update())
    }
}

/// A connected MCP client.
///
/// Requests take `&self` and may run concurrently, up to
/// [`Limits::max_pending_requests`]; share the client with an `Arc` to call it
/// from several tasks. Call [`shutdown`](Self::shutdown) to close the
/// connection and wait for its task.
#[derive(Debug)]
pub struct McpClient {
    core: Core,
    server: InitializeResult,
}

impl McpClient {
    /// Validate `options` and start the connection's driver task, without
    /// sending anything. The returned [`Connecting`] owns the task: initialize
    /// through it, and keep it to [`shutdown`](Connecting::shutdown) the
    /// connection if initialization is cancelled.
    ///
    /// Must be called inside a Tokio runtime. Refused options and a missing
    /// runtime are reported before any task starts; both halves are dropped
    /// on return.
    pub fn start<R, S>(
        receiver: R,
        sender: S,
        options: ClientOptions,
    ) -> Result<Connecting, ConnectError>
    where
        R: MessageReceiver,
        S: MessageSender,
    {
        options.validate().map_err(ConnectError::InvalidOptions)?;
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| ConnectError::NoRuntime)?;
        let limits = options.limits;
        let (commands, command_receiver) = mpsc::channel(limits.max_pending_requests.get());
        // Unbounded only in type: each entry carries one of the
        // `max_pending_requests` permits.
        let (cancels, cancel_receiver) = mpsc::unbounded_channel();
        let (events, event_receiver) = mpsc::channel(limits.max_events.get());
        let (shutdown, shutdown_receiver) = watch::channel(false);
        let (close, closed) = watch::channel(None);
        let (tools, tool_receiver) = watch::channel(0);
        let permits = Arc::new(Semaphore::new(limits.max_pending_requests.get()));
        let dropped_events = Arc::new(AtomicU64::new(0));
        let driver = Driver {
            receiver,
            sender: Some(sender),
            writing: None,
            queue: VecDeque::new(),
            pending: HashMap::new(),
            cancelled: VecDeque::new(),
            commands: command_receiver,
            cancels: cancel_receiver,
            shutdown: shutdown_receiver,
            events,
            dropped_events: dropped_events.clone(),
            tools,
            close,
            permits: permits.clone(),
            limits,
            shutdown_timeout: options.shutdown_timeout,
        };
        let task = runtime.spawn(driver.run());
        let core = Core {
            commands,
            cancels,
            permits,
            next_id: AtomicU64::new(1),
            shutdown,
            closed,
            tools: tool_receiver,
            dropped_events,
            driver: Mutex::new(Some(task)),
            limits,
            request_timeout: options.request_timeout,
        };
        Ok(Connecting {
            core: Some(core),
            events: Some(ClientEvents {
                receiver: event_receiver,
            }),
            options,
            attempted: false,
        })
    }

    /// [`start`](Self::start) and [`Connecting::initialize`] in one call.
    ///
    /// A convenience with weaker ownership: if this future is dropped before
    /// it returns, the driver task is only signalled to stop. It goes on
    /// finishing its in-flight write and closing the sender for up to
    /// [`ClientOptions::shutdown_timeout`] with no owner that can wait for
    /// it. A caller that must know when the transport is released, such as a
    /// session that reaps the server process afterwards, uses `start` and
    /// keeps the [`Connecting`]. When this future returns an error, the
    /// driver has ended and both halves are dropped.
    pub async fn connect<R, S>(
        receiver: R,
        sender: S,
        options: ClientOptions,
    ) -> Result<(Self, ClientEvents), ConnectError>
    where
        R: MessageReceiver,
        S: MessageSender,
    {
        Self::start(receiver, sender, options)?.initialize().await
    }

    /// The server's initialize result.
    pub fn server(&self) -> &InitializeResult {
        &self.server
    }

    /// One page of `tools/list`, starting at `cursor`.
    pub async fn list_tools_page(&self, cursor: Option<&str>) -> Result<ToolsPage, RequestError> {
        self.tools_page(cursor).await.map(|(page, _)| page)
    }

    /// Every tool, following `nextCursor` through all pages within the list
    /// limits. Like Pi's client it does not check the `tools` capability;
    /// callers that honor it check [`ServerCapabilities::tools`]
    /// first.
    ///
    /// [`ServerCapabilities::tools`]: crate::ServerCapabilities::tools
    pub async fn list_tools(&self) -> Result<Vec<Tool>, RequestError> {
        let limits = self.core.limits;
        let mut tools = Vec::new();
        let mut cursors = HashSet::new();
        let mut cursor: Option<String> = None;
        let mut bytes = 0_usize;
        for _ in 0..limits.max_list_pages.get() {
            let (page, page_bytes) = self.tools_page(cursor.as_deref()).await?;
            bytes = bytes.saturating_add(page_bytes);
            if bytes > limits.max_list_bytes.get() {
                return Err(RequestError::ListTooLarge {
                    method: TOOLS_LIST,
                    limit: limits.max_list_bytes.get(),
                });
            }
            if tools.len().saturating_add(page.tools.len()) > limits.max_listed_tools.get() {
                return Err(RequestError::TooManyItems {
                    method: TOOLS_LIST,
                    limit: limits.max_listed_tools.get(),
                });
            }
            tools.extend(page.tools);
            let Some(next) = page.next_cursor else {
                return Ok(tools);
            };
            if !cursors.insert(next.clone()) {
                return Err(RequestError::RepeatedCursor {
                    method: TOOLS_LIST,
                    cursor: preview(&next, 64),
                });
            }
            cursor = Some(next);
        }
        Err(RequestError::TooManyPages {
            method: TOOLS_LIST,
            limit: limits.max_list_pages.get(),
        })
    }

    async fn tools_page(&self, cursor: Option<&str>) -> Result<(ToolsPage, usize), RequestError> {
        let params = cursor.map(|cursor| {
            let mut params = Map::new();
            params.insert("cursor".into(), cursor.into());
            params
        });
        let reply = self.core.request(TOOLS_LIST, params, true).await?;
        let page =
            protocol::tools_page(reply.result).map_err(|detail| RequestError::InvalidResult {
                method: TOOLS_LIST,
                detail,
            })?;
        Ok((page, reply.bytes))
    }

    /// Call a tool. A tool that fails reports
    /// [`CallToolResult::is_error`]; an `Err` is a protocol failure.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Option<Map<String, Value>>,
    ) -> Result<CallToolResult, RequestError> {
        let mut params = Map::new();
        params.insert("name".into(), name.into());
        if let Some(arguments) = arguments {
            params.insert("arguments".into(), Value::Object(arguments));
        }
        let reply = self.core.request(TOOLS_CALL, Some(params), true).await?;
        protocol::call_tool_result(reply.result).map_err(|detail| RequestError::InvalidResult {
            method: TOOLS_CALL,
            detail,
        })
    }

    /// Send `ping` and wait for the answer.
    pub async fn ping(&self) -> Result<(), RequestError> {
        self.core.request("ping", None, true).await.map(drop)
    }

    /// A handle that waits for tool list changes.
    pub fn tool_list_changes(&self) -> ToolListChanges {
        ToolListChanges {
            receiver: self.core.tools.clone(),
        }
    }

    /// How many events were dropped because the consumer fell behind.
    pub fn dropped_events(&self) -> u64 {
        self.core.dropped_events.load(Ordering::Relaxed)
    }

    /// Why the connection closed, once it has.
    pub fn close_reason(&self) -> Option<CloseReason> {
        self.core.close_reason()
    }

    /// Wait until the connection closes, and return why.
    pub async fn closed(&self) -> CloseReason {
        let mut closed = self.core.closed.clone();
        match closed.wait_for(Option::is_some).await {
            Ok(reason) => reason.clone().unwrap_or(CloseReason::DriverFailed),
            Err(_) => self
                .core
                .close_reason()
                .unwrap_or(CloseReason::DriverFailed),
        }
    }

    /// Close the connection and wait for the driver task to end. Returns why
    /// the connection closed: [`CloseReason::Shutdown`], or the earlier reason
    /// if it had already closed. Safe to call more than once and from several
    /// tasks; every call returns after the task has ended.
    pub async fn shutdown(&self) -> CloseReason {
        self.core.shutdown().await
    }
}

/// A started connection that has not finished initialization.
///
/// It owns the driver task until [`initialize`](Self::initialize) hands the
/// connection to an [`McpClient`]. If initialization is cancelled (its future
/// dropped), the `Connecting` still owns the task: call
/// [`shutdown`](Self::shutdown) to stop it and wait until it has dropped both
/// transport halves. Dropping a `Connecting` only signals the task.
#[derive(Debug)]
pub struct Connecting {
    /// `None` once handed to a client.
    core: Option<Core>,
    events: Option<ClientEvents>,
    options: ClientOptions,
    attempted: bool,
}

impl Connecting {
    /// Send `initialize`, check the result, and send
    /// `notifications/initialized`; on success, hand the connection to a new
    /// [`McpClient`].
    ///
    /// Runs at most once: `initialize` cannot be repeated on a connection
    /// (the server may have answered a cancelled attempt), so a second call
    /// fails with [`ConnectError::AlreadyAttempted`]. On error the driver has
    /// ended and both halves are dropped before this returns. If this future
    /// is dropped, call [`shutdown`](Self::shutdown).
    pub async fn initialize(&mut self) -> Result<(McpClient, ClientEvents), ConnectError> {
        let (Some(core), false) = (self.core.as_ref(), self.attempted) else {
            return Err(ConnectError::AlreadyAttempted);
        };
        self.attempted = true;
        match core.initialize(&self.options).await {
            Ok(server) => {
                let (Some(core), Some(events)) = (self.core.take(), self.events.take()) else {
                    return Err(ConnectError::AlreadyAttempted);
                };
                Ok((McpClient { core, server }, events))
            }
            Err(error) => {
                core.shutdown().await;
                Err(error)
            }
        }
    }

    /// Stop the driver task and wait until it has ended and dropped both
    /// transport halves. Returns why the connection closed, or `None` if
    /// [`initialize`](Self::initialize) already handed it to an
    /// [`McpClient`], whose own [`shutdown`](McpClient::shutdown) owns it
    /// then. Safe to call more than once.
    pub async fn shutdown(&self) -> Option<CloseReason> {
        match &self.core {
            Some(core) => Some(core.shutdown().await),
            None => None,
        }
    }
}

const TOOLS_LIST: &str = "tools/list";
const TOOLS_CALL: &str = "tools/call";

/// The handle side of a connection.
#[derive(Debug)]
struct Core {
    commands: mpsc::Sender<Command>,
    cancels: mpsc::UnboundedSender<Cancel>,
    permits: Arc<Semaphore>,
    next_id: AtomicU64,
    /// Dropping it (with the client) also stops the driver.
    shutdown: watch::Sender<bool>,
    closed: watch::Receiver<Option<CloseReason>>,
    tools: watch::Receiver<u64>,
    dropped_events: Arc<AtomicU64>,
    driver: Mutex<Option<JoinHandle<()>>>,
    limits: Limits,
    request_timeout: Option<Duration>,
}

impl Core {
    async fn initialize(&self, options: &ClientOptions) -> Result<InitializeResult, ConnectError> {
        let mut params = Map::new();
        params.insert(
            "protocolVersion".into(),
            options.protocol_version.as_str().into(),
        );
        // No client capability is declared: this client answers no
        // server-initiated request but `ping` (roots, sampling, and
        // elicitation are not implemented).
        params.insert("capabilities".into(), Value::Object(Map::new()));
        params.insert("clientInfo".into(), options.client_info.to_value());
        let reply = self
            .request("initialize", Some(params), false)
            .await
            .map_err(ConnectError::Initialize)?;
        let result =
            protocol::initialize_result(reply.result).map_err(|rejection| match rejection {
                InitializeRejection::Invalid(detail) => {
                    ConnectError::Initialize(RequestError::InvalidResult {
                        method: "initialize",
                        detail: detail.into(),
                    })
                }
                InitializeRejection::UnsupportedVersion(selected) => {
                    ConnectError::UnsupportedProtocolVersion { selected }
                }
            })?;
        let notification = jsonrpc::encode_notification("notifications/initialized", None);
        self.commands
            .send(Command::Notify {
                bytes: notification,
            })
            .await
            .map_err(|_| ConnectError::Initialize(self.closed_error()))?;
        Ok(result)
    }

    async fn request(
        &self,
        method: &'static str,
        params: Option<Map<String, Value>>,
        cancellable: bool,
    ) -> Result<Reply, RequestError> {
        // `validate` checked the deadline was representable at start; one
        // that has become unrepresentable since waits without a deadline.
        let deadline = self
            .request_timeout
            .and_then(|after| Instant::now().checked_add(after));
        let timeout = || RequestError::Timeout {
            method,
            after: self.request_timeout.unwrap_or(Duration::ZERO),
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let bytes = jsonrpc::encode_request(id, method, params);
        let limit = self.limits.max_message_bytes.get();
        if bytes.len() > limit {
            return Err(RequestError::MessageTooLarge {
                method,
                size: bytes.len(),
                limit,
            });
        }
        let permit = match until(deadline, self.permits.clone().acquire_owned()).await {
            None => return Err(timeout()),
            Some(Err(_)) => return Err(self.closed_error()),
            Some(Ok(permit)) => permit,
        };
        let (reply, receiver) = oneshot::channel();
        let mut guard = RequestGuard {
            id,
            cancels: &self.cancels,
            permit: Some(permit),
            receiver: Some(receiver),
            reason: CANCELLED_REASON,
            armed: true,
        };
        let command = Command::Request {
            id,
            method,
            bytes,
            cancellable,
            reply,
        };
        // An unsent command is not in the channel (`send` is cancel-safe), so
        // a cancellation for it names an id the driver never saw.
        match until(deadline, self.commands.send(command)).await {
            None => return Err(timeout()),
            Some(Err(_)) => {
                guard.armed = false;
                return Err(self.closed_error());
            }
            Some(Ok(())) => {}
        }
        match until(deadline, guard.wait()).await {
            None => {
                guard.reason = TIMED_OUT_REASON;
                Err(timeout())
            }
            Some(answer) => {
                guard.armed = false;
                answer.unwrap_or_else(|_| Err(self.closed_error()))
            }
        }
    }

    fn close_reason(&self) -> Option<CloseReason> {
        self.closed.borrow().clone()
    }

    fn closed_error(&self) -> RequestError {
        RequestError::Closed(self.close_reason().unwrap_or(CloseReason::DriverFailed))
    }

    async fn shutdown(&self) -> CloseReason {
        self.shutdown.send_replace(true);
        let mut driver = self.driver.lock().await;
        if let Some(task) = driver.as_mut() {
            // Awaited in place and cleared without an await in between, so a
            // shutdown future dropped while waiting leaves the handle for the
            // next call.
            let _ = task.await;
            *driver = None;
        }
        self.close_reason().unwrap_or(CloseReason::DriverFailed)
    }
}

async fn until<F: Future>(deadline: Option<Instant>, work: F) -> Option<F::Output> {
    match deadline {
        None => Some(work.await),
        Some(deadline) => tokio::time::timeout_at(deadline, work).await.ok(),
    }
}

type Answer = Result<Reply, RequestError>;

/// Cancels its request when dropped while armed: the cancellation carries the
/// request's permit to the driver, which releases it once the request is out
/// of its queue and maps.
struct RequestGuard<'a> {
    id: u64,
    cancels: &'a mpsc::UnboundedSender<Cancel>,
    permit: Option<OwnedSemaphorePermit>,
    receiver: Option<oneshot::Receiver<Answer>>,
    reason: &'static str,
    armed: bool,
}

impl RequestGuard<'_> {
    async fn wait(&mut self) -> Result<Answer, oneshot::error::RecvError> {
        match self.receiver.as_mut() {
            Some(receiver) => receiver.await,
            None => std::future::pending().await,
        }
    }
}

impl Drop for RequestGuard<'_> {
    fn drop(&mut self) {
        // Close the reply channel before cancelling, so a driver that has not
        // yet taken the request sees it abandoned and never sends it.
        drop(self.receiver.take());
        if self.armed {
            let _ = self.cancels.send(Cancel {
                id: self.id,
                reason: self.reason,
                _permit: self.permit.take(),
            });
        }
    }
}

#[derive(Debug)]
enum Command {
    Request {
        id: u64,
        method: &'static str,
        bytes: Vec<u8>,
        cancellable: bool,
        reply: oneshot::Sender<Answer>,
    },
    Notify {
        bytes: Vec<u8>,
    },
}

#[derive(Debug)]
struct Cancel {
    id: u64,
    reason: &'static str,
    _permit: Option<OwnedSemaphorePermit>,
}

#[derive(Debug)]
struct Reply {
    result: Value,
    /// The response message's size, for list byte limits.
    bytes: usize,
}

struct Pending {
    method: &'static str,
    cancellable: bool,
    reply: oneshot::Sender<Answer>,
}

struct Outgoing {
    /// The request this message is, if it is one, so a cancellation can
    /// withdraw it before it is written.
    request: Option<u64>,
    bytes: Vec<u8>,
}

type WriteFuture<S> = Pin<Box<dyn Future<Output = (S, io::Result<()>)> + Send>>;

/// The connection's state, owned by one task.
struct Driver<R, S> {
    receiver: R,
    /// `None` while `writing` holds it.
    sender: Option<S>,
    writing: Option<WriteFuture<S>>,
    queue: VecDeque<Outgoing>,
    pending: HashMap<u64, Pending>,
    /// Recently cancelled ids, whose late responses are ignored silently.
    cancelled: VecDeque<u64>,
    commands: mpsc::Receiver<Command>,
    cancels: mpsc::UnboundedReceiver<Cancel>,
    shutdown: watch::Receiver<bool>,
    events: mpsc::Sender<ClientEvent>,
    dropped_events: Arc<AtomicU64>,
    tools: watch::Sender<u64>,
    close: watch::Sender<Option<CloseReason>>,
    permits: Arc<Semaphore>,
    limits: Limits,
    shutdown_timeout: Duration,
}

impl<R: MessageReceiver, S: MessageSender> Driver<R, S> {
    async fn run(mut self) {
        let reason = self.serve().await;
        self.finish(reason).await;
    }

    async fn serve(&mut self) -> CloseReason {
        loop {
            // Shutdown before anything else: each cancellation handled below
            // releases a permit, which lets another request in, so callers
            // that keep cancelling could otherwise keep this loop busy.
            if !matches!(self.shutdown.has_changed(), Ok(false)) {
                return CloseReason::Shutdown;
            }
            // Cancellations next, so a withdrawn request is never written; at
            // most one per permit each pass, then the `select!` below runs.
            for _ in 0..self.limits.max_pending_requests.get() {
                let Ok(cancel) = self.cancels.try_recv() else {
                    break;
                };
                self.cancel(cancel);
            }
            self.start_write();
            let room = self.queue.len() < self.limits.max_queued_messages.get();
            tokio::select! {
                biased;
                _ = self.shutdown.changed() => return CloseReason::Shutdown,
                (sender, result) = poll_write(&mut self.writing) => {
                    self.writing = None;
                    self.sender = Some(sender);
                    if let Err(error) = result {
                        return CloseReason::Send { kind: error.kind(), message: error.to_string() };
                    }
                }
                command = self.commands.recv(), if room => match command {
                    Some(command) => self.command(command),
                    None => return CloseReason::Shutdown,
                },
                frame = self.receiver.receive(), if room => match frame {
                    Ok(Some(bytes)) => {
                        if let Some(reason) = self.frame(bytes) {
                            return reason;
                        }
                    }
                    Ok(None) => return CloseReason::EndOfStream,
                    Err(error) => return CloseReason::from_receive(error),
                },
                // Last, so cancellations arriving without pause cannot starve
                // writes, requests, or reads; the pass at the top of the loop
                // handles the cancellations queued before each write starts.
                Some(cancel) = self.cancels.recv() => self.cancel(cancel),
            }
        }
    }

    fn start_write(&mut self) {
        if self.writing.is_some() || self.queue.is_empty() {
            return;
        }
        let (Some(mut sender), Some(outgoing)) = (self.sender.take(), self.queue.pop_front())
        else {
            return;
        };
        self.writing = Some(Box::pin(async move {
            let result = sender.send(&outgoing.bytes).await;
            (sender, result)
        }));
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Request {
                id,
                method,
                bytes,
                cancellable,
                reply,
            } => {
                // Abandoned before it reached the driver: never send it.
                if reply.is_closed() {
                    return;
                }
                self.pending.insert(
                    id,
                    Pending {
                        method,
                        cancellable,
                        reply,
                    },
                );
                self.queue.push_back(Outgoing {
                    request: Some(id),
                    bytes,
                });
            }
            Command::Notify { bytes } => self.queue.push_back(Outgoing {
                request: None,
                bytes,
            }),
        }
    }

    fn cancel(&mut self, cancel: Cancel) {
        let Some(entry) = self.pending.remove(&cancel.id) else {
            return;
        };
        if let Some(index) = self
            .queue
            .iter()
            .position(|outgoing| outgoing.request == Some(cancel.id))
        {
            self.queue.remove(index);
            return;
        }
        if self.cancelled.len() == self.limits.max_pending_requests.get() {
            self.cancelled.pop_front();
        }
        self.cancelled.push_back(cancel.id);
        if entry.cancellable {
            let mut params = Map::new();
            params.insert("requestId".into(), cancel.id.into());
            params.insert("reason".into(), cancel.reason.into());
            self.queue.push_back(Outgoing {
                request: None,
                bytes: jsonrpc::encode_notification("notifications/cancelled", Some(params)),
            });
        }
    }

    /// Handle one incoming message; `Some` closes the connection.
    fn frame(&mut self, bytes: Vec<u8>) -> Option<CloseReason> {
        let limit = self.limits.max_message_bytes.get();
        if bytes.len() > limit {
            return Some(CloseReason::MessageTooLarge { limit });
        }
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return None;
        }
        match jsonrpc::parse(&bytes) {
            Err(malformed) => self.event(ClientEvent::Diagnostic(Diagnostic::MalformedMessage {
                detail: malformed.to_string(),
            })),
            Ok(Incoming::Response { id, outcome }) => self.response(id, outcome, bytes.len()),
            Ok(Incoming::ErrorWithoutId(error)) => {
                self.event(ClientEvent::Diagnostic(Diagnostic::ErrorWithoutId(error)));
            }
            Ok(Incoming::Request { id, method }) => self.server_request(&id, &method),
            Ok(Incoming::Notification { method, params }) => self.notification(method, params),
        }
        None
    }

    fn response(&mut self, id: Value, outcome: Outcome, bytes: usize) {
        let number = id.as_u64();
        let Some(entry) = number.and_then(|number| self.pending.remove(&number)) else {
            if let Some(index) =
                number.and_then(|number| self.cancelled.iter().position(|id| *id == number))
            {
                self.cancelled.remove(index);
                return;
            }
            let id = preview(&id.to_string(), 64);
            self.event(ClientEvent::Diagnostic(Diagnostic::UnknownResponse { id }));
            return;
        };
        let method = entry.method;
        let answer = match outcome {
            Outcome::Result(result) => Ok(Reply { result, bytes }),
            Outcome::Error(error) => Err(RequestError::Server { method, error }),
            Outcome::Invalid(detail) => Err(RequestError::InvalidResponse {
                method,
                detail: detail.into(),
            }),
        };
        let _ = entry.reply.send(answer);
    }

    fn server_request(&mut self, id: &RequestId, method: &str) {
        let bytes = if method == "ping" {
            jsonrpc::encode_result(id, Value::Object(Map::new()))
        } else {
            jsonrpc::encode_error(
                id,
                error_codes::METHOD_NOT_FOUND,
                format!("Method not found: {}", preview(method, 200)),
            )
        };
        self.queue.push_back(Outgoing {
            request: None,
            bytes,
        });
    }

    fn notification(&mut self, method: String, params: Option<Value>) {
        match method.as_str() {
            "notifications/tools/list_changed" => {
                self.tools
                    .send_modify(|generation| *generation = generation.wrapping_add(1));
            }
            // The client answers server requests at once, so there is nothing
            // in progress for the server to cancel.
            "notifications/cancelled" => {}
            _ => self.event(ClientEvent::Notification { method, params }),
        }
    }

    fn event(&mut self, event: ClientEvent) {
        if let Err(mpsc::error::TrySendError::Full(_)) = self.events.try_send(event) {
            self.dropped_events.fetch_add(1, Ordering::Relaxed);
        }
    }

    async fn finish(mut self, reason: CloseReason) {
        // Record the reason before anything can observe the closed channels.
        self.close.send_replace(Some(reason.clone()));
        self.permits.close();
        self.commands.close();
        while self.commands.try_recv().is_ok() {}
        for (_, entry) in self.pending.drain() {
            let _ = entry.reply.send(Err(RequestError::Closed(reason.clone())));
        }
        self.cancels.close();
        while self.cancels.try_recv().is_ok() {}
        let flush = !matches!(reason, CloseReason::Send { .. });
        let deadline = Instant::now().checked_add(self.shutdown_timeout);
        let mut queue = std::mem::take(&mut self.queue);
        let writing = self.writing.take();
        let sender = self.sender.take();
        let _ = until(deadline, async move {
            let mut sender = match (writing, sender) {
                (Some(writing), _) => match writing.await {
                    (sender, Ok(())) => sender,
                    (_, Err(_)) => return,
                },
                (None, Some(sender)) => sender,
                (None, None) => return,
            };
            if flush {
                // Requests in the queue were failed above and are not sent.
                for outgoing in queue
                    .drain(..)
                    .filter(|outgoing| outgoing.request.is_none())
                {
                    if sender.send(&outgoing.bytes).await.is_err() {
                        return;
                    }
                }
            }
            let _ = sender.close().await;
        })
        .await;
        // `self` drops here, with the receiver and every channel end.
    }
}

async fn poll_write<S>(writing: &mut Option<WriteFuture<S>>) -> (S, io::Result<()>) {
    match writing {
        Some(future) => future.await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{LineReader, LineWriter};

    type TestDriver = Driver<LineReader<tokio::io::Empty>, LineWriter<tokio::io::Sink>>;

    /// A driver whose channel ends are dropped; only its synchronous state
    /// transitions are exercised.
    fn driver() -> TestDriver {
        let limits = Limits::default();
        let (_, commands) = mpsc::channel(1);
        let (_, cancels) = mpsc::unbounded_channel();
        let (events, _) = mpsc::channel(1);
        let (_, shutdown) = watch::channel(false);
        let (tools, _) = watch::channel(0);
        let (close, _) = watch::channel(None);
        Driver {
            receiver: LineReader::new(tokio::io::empty(), limits.max_message_bytes),
            sender: Some(LineWriter::new(tokio::io::sink())),
            writing: None,
            queue: VecDeque::new(),
            pending: HashMap::new(),
            cancelled: VecDeque::new(),
            commands,
            cancels,
            shutdown,
            events,
            dropped_events: Arc::new(AtomicU64::new(0)),
            tools,
            close,
            permits: Arc::new(Semaphore::new(1)),
            limits,
            shutdown_timeout: Duration::ZERO,
        }
    }

    /// Admit a request and take it off the queue, as if it were written.
    fn written(driver: &mut TestDriver, id: u64, method: &'static str, cancellable: bool) {
        let (reply, receiver) = oneshot::channel();
        std::mem::forget(receiver);
        driver.command(Command::Request {
            id,
            method,
            bytes: Vec::new(),
            cancellable,
            reply,
        });
        assert_eq!(
            driver
                .queue
                .pop_front()
                .and_then(|outgoing| outgoing.request),
            Some(id)
        );
    }

    fn cancel(id: u64) -> Cancel {
        Cancel {
            id,
            reason: TIMED_OUT_REASON,
            _permit: None,
        }
    }

    #[test]
    fn a_written_initialize_is_never_cancelled() {
        // The specification forbids cancelling `initialize`. A failed connect
        // usually shuts down before the driver sees the cancellation, so the
        // rule is checked on the transition itself.
        let mut driver = driver();
        written(&mut driver, 1, "initialize", false);
        driver.cancel(cancel(1));
        assert!(driver.queue.is_empty());
        assert!(driver.pending.is_empty());
        // Control: a cancellable request is followed by a notification.
        written(&mut driver, 2, "tools/call", true);
        driver.cancel(cancel(2));
        let notice: Value = serde_json::from_slice(&driver.queue[0].bytes).unwrap();
        assert_eq!(notice["method"], "notifications/cancelled");
        assert_eq!(notice["params"]["requestId"], 2);
    }

    /// Records each message it is asked to write.
    /// It also requests shutdown as it writes, so the first write is the
    /// moment shutdown arrives.
    struct Recorder {
        written: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
        shutdown: watch::Sender<bool>,
    }

    impl MessageSender for Recorder {
        async fn send(&mut self, message: &[u8]) -> io::Result<()> {
            self.written
                .lock()
                .map_err(|_| io::Error::other("poisoned"))?
                .push(message.to_vec());
            self.shutdown.send_replace(true);
            Ok(())
        }

        async fn close(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_is_not_held_off_by_a_backlog_of_cancellations() {
        // Callers that cancel as fast as permits are released keep refilling
        // the cancellation channel while the driver works through it; a
        // backlog far longer than one pass stands in for them. Shutdown is
        // requested while the first cancellation notice is written, so any
        // notice past the first pass of 16 is work done after it. Before
        // this test, the driver drained every queued cancellation at once and
        // took cancellations ahead of finishing a write, so all 10,000
        // notices were written.
        const BACKLOG: u64 = 10_000;
        let limits = Limits {
            max_pending_requests: nonzero(16),
            ..Limits::default()
        };
        let (_commands, command_receiver) = mpsc::channel(1);
        let (cancels, cancel_receiver) = mpsc::unbounded_channel();
        let (events, _) = mpsc::channel(1);
        let (shutdown, shutdown_receiver) = watch::channel(false);
        let (tools, _) = watch::channel(0);
        let (close, closed) = watch::channel(None);
        let written = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = Recorder {
            written: written.clone(),
            shutdown,
        };
        let mut driver = Driver {
            receiver: LineReader::new(tokio::io::empty(), limits.max_message_bytes),
            sender: Some(recorder),
            writing: None,
            queue: VecDeque::new(),
            pending: HashMap::new(),
            cancelled: VecDeque::new(),
            commands: command_receiver,
            cancels: cancel_receiver,
            shutdown: shutdown_receiver,
            events,
            dropped_events: Arc::new(AtomicU64::new(0)),
            tools,
            close,
            permits: Arc::new(Semaphore::new(16)),
            limits,
            shutdown_timeout: Duration::from_secs(2),
        };
        for id in 1..=BACKLOG {
            let (reply, receiver) = oneshot::channel();
            std::mem::forget(receiver);
            driver.pending.insert(
                id,
                Pending {
                    method: "tools/call",
                    cancellable: true,
                    reply,
                },
            );
            cancels.send(cancel(id)).unwrap();
        }
        tokio::time::timeout(Duration::from_secs(10), driver.run())
            .await
            .expect("the driver stopped");
        assert_eq!(*closed.borrow(), Some(CloseReason::Shutdown));
        let notices = written.lock().unwrap().len();
        assert!(notices > 0, "the first pass wrote no notice");
        assert!(
            notices <= 16,
            "{notices} cancellation notices: work continued after shutdown was requested"
        );
    }

    #[test]
    fn the_cancelled_list_is_bounded() {
        let mut driver = driver();
        let limit = driver.limits.max_pending_requests.get() as u64;
        for id in 1..=limit + 10 {
            written(&mut driver, id, "tools/call", true);
            driver.cancel(cancel(id));
            driver.queue.clear();
        }
        assert_eq!(driver.cancelled.len() as u64, limit);
        assert_eq!(driver.cancelled.front(), Some(&11));
    }
}
