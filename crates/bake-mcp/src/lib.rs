//! A Model Context Protocol client for Bake.
//!
//! `bake-mcp` ports the client of Pi's `mcp` package (`packages/mcp`,
//! release v1.3.0, revision `71023aec1546d6d677a65669c5665c0927ce0dd0`, MIT;
//! see the crate's `NOTICE`): the connection's initialization and capability
//! negotiation, `tools/list` with pagination and
//! `notifications/tools/list_changed`, `tools/call`, the resource listings
//! and `resources/read`, JSON-RPC errors, cancellation, and awaited
//! shutdown, over owned stdio and Streamable HTTP transports or one the
//! caller supplies.
//!
//! | Module | Pi source |
//! |---|---|
//! | [`jsonrpc`] | `src/protocol/jsonrpc.ts` |
//! | [`protocol`] | `src/protocol/types.ts`, `src/protocol/content.ts`, and the result checks of `src/client.ts` |
//! | [`transport`] | `src/transports/transport.ts` and the framing of `src/transports/stdio.ts` |
//! | [`client`] | `src/client.ts` |
//! | [`stdio`] | the process lifecycle of `src/transports/stdio.ts` |
//! | [`http`] | `src/transports/streamable-http.ts`, with its SSE decoder |
//!
//! The client runs over any [`MessageReceiver`] and [`MessageSender`],
//! usually [`LineReader`] and [`LineWriter`] over a child's pipes. [`stdio`]
//! spawns a server and owns its process, process tree, and standard error
//! next to the client; the caller decides whether the server may run and
//! confines it (D24), and gives it its environment. For a remote server,
//! [`HttpTransport::start`] returns the transport owner and its message halves;
//! the caller awaits [`HttpTransport::shutdown`] after the client.
//! [`HttpTransport::start_with_client`] takes the HTTP client from the
//! caller, which then owns address resolution and pinning, redirects,
//! proxies, and TLS, as Bake's MCP egress admission does.
//! [`McpClient::start`] returns a [`Connecting`] that owns the
//! client's task through initialization, and [`Connecting::shutdown`] and
//! [`McpClient::shutdown`] return once that task has ended and dropped both
//! halves, so the caller can then end the process, also when initialization
//! was cancelled.
//!
//! # Not implemented
//!
//! Not implemented here: OAuth and the auth provider (Pi `src/oauth/`,
//! `src/auth-provider.ts`), re-initialization after an HTTP session expires,
//! which belongs to the transport's owner (Bake's `mcp::http` starts a new
//! transport and client), resource subscriptions (`resources/subscribe`),
//! prompts, progress notifications
//! and their timeout renewal, roots, sampling, elicitation, and the stateless
//! MCP 2026-07-28 revision (`server/discover`, per-request version metadata).
//! `notifications/resources/list_changed` is reported as a
//! [`ClientEvent::Notification`], as Pi handles it: resource listings are
//! made on demand, so the next one already shows the new set.
//! The client negotiates only the handshake-based revisions of
//! [`ProtocolVersion::SUPPORTED`], as Pi v1.3.0 does. Tool name mapping,
//! content conversion, the model tool adapter, configuration, and reconnects
//! belong to the integration in `bake-coding-agent`.
#![warn(missing_docs)]
// The process syscalls live in `bake-process`.
#![forbid(unsafe_code)]

pub mod client;
pub mod http;
pub mod jsonrpc;
pub mod protocol;
mod sse;
pub mod stdio;
pub mod transport;

pub use client::{
    ClientEvent, ClientEvents, ClientOptions, CloseReason, ConnectError, Connecting, Diagnostic,
    Limits, McpClient, OptionsError, RequestError, ToolListChanges,
};
pub use http::{
    HttpError, HttpLimits, HttpOptions, HttpOptionsError, HttpReceiver, HttpSender, HttpTransport,
    Reconnect, SessionClose,
};
pub use jsonrpc::{JsonRpcError, RequestId};
pub use protocol::{
    CallToolResult, Implementation, InitializeResult, ProtocolVersion, ReadResourceResult,
    Resource, ResourceContents, ResourcesCapability, ResourcesPage, ServerCapabilities, Tool,
    ToolsCapability, ToolsPage,
};
pub use stdio::{
    ProcessReport, ShutdownIncomplete, SpawnError, StartError, StderrSnapshot, StdioCommand,
    StdioOptions, StdioProcess, Stop, TreeEnd, scrubbed_parent_environment, spawn_stdio,
};
pub use transport::{
    DEFAULT_MAX_MESSAGE_BYTES, LineReader, LineWriter, MessageReceiver, MessageSender, ReceiveError,
};
