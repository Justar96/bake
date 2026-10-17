//! A Model Context Protocol client for Bake's Rust agent.
//!
//! `bake-mcp` ports the client of Pi's `mcp` package (`packages/mcp`,
//! release v1.3.0, revision `71023aec1546d6d677a65669c5665c0927ce0dd0`, MIT;
//! see the crate's `NOTICE`): the connection's initialization and capability
//! negotiation, `tools/list` with pagination and
//! `notifications/tools/list_changed`, `tools/call`, JSON-RPC errors,
//! cancellation, and awaited shutdown, over a transport the caller supplies.
//!
//! | Module | Pi source |
//! |---|---|
//! | [`jsonrpc`] | `src/protocol/jsonrpc.ts` |
//! | [`protocol`] | `src/protocol/types.ts`, `src/protocol/content.ts`, and the result checks of `src/client.ts` |
//! | [`transport`] | `src/transports/transport.ts` and the framing of `src/transports/stdio.ts` |
//! | [`client`] | `src/client.ts` |
//! | [`stdio`] | the process lifecycle of `src/transports/stdio.ts` |
//!
//! The client runs over any [`MessageReceiver`] and [`MessageSender`],
//! usually [`LineReader`] and [`LineWriter`] over a child's pipes. [`stdio`]
//! spawns a server and owns its process, process tree, and standard error
//! next to the client; the caller decides whether the server may run and
//! confines it (D24), and gives it its environment. [`McpClient::start`] returns a [`Connecting`] that owns the
//! client's task through initialization, and [`Connecting::shutdown`] and
//! [`McpClient::shutdown`] return once that task has ended and dropped both
//! halves, so the caller can then end the process, also when initialization
//! was cancelled.
//!
//! # Not implemented
//!
//! This is the protocol boundary only. Not implemented here: the Streamable
//! HTTP transport and OAuth (Pi `src/transports/streamable-http.ts`,
//! `src/oauth/`), resources, prompts, progress notifications
//! and their timeout renewal, roots, sampling, elicitation, and the stateless
//! MCP 2026-07-28 revision (`server/discover`, per-request version metadata).
//! The client negotiates only the handshake-based revisions of
//! [`ProtocolVersion::SUPPORTED`], as Pi v1.3.0 does. Tool name mapping,
//! content conversion, the model tool adapter, configuration, and reconnects
//! belong to the integration in `bake-coding-agent`.
#![warn(missing_docs)]
// `unsafe` is denied everywhere but `stdio`'s server module, whose
// syscalls carry their contracts.
#![deny(unsafe_code)]

pub mod client;
pub mod jsonrpc;
pub mod protocol;
pub mod stdio;
pub mod transport;

pub use client::{
    ClientEvent, ClientEvents, ClientOptions, CloseReason, ConnectError, Connecting, Diagnostic,
    Limits, McpClient, OptionsError, RequestError, ToolListChanges,
};
pub use jsonrpc::{JsonRpcError, RequestId};
pub use protocol::{
    CallToolResult, Implementation, InitializeResult, ProtocolVersion, ServerCapabilities, Tool,
    ToolsCapability, ToolsPage,
};
pub use stdio::{
    ProcessReport, ShutdownIncomplete, SpawnError, StartError, StderrSnapshot, StdioCommand,
    StdioOptions, StdioProcess, Stop, TreeEnd, scrubbed_parent_environment, spawn_stdio,
};
pub use transport::{
    DEFAULT_MAX_MESSAGE_BYTES, LineReader, LineWriter, MessageReceiver, MessageSender, ReceiveError,
};
