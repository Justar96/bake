//! One Streamable HTTP server's lifetime: admitted egress, a connection,
//! re-initialization after an expired HTTP session, and bounded
//! reconnection, as one task the session's gate owns (see the [module
//! documentation](super)).
//!
//! Each connection is a whole generation of its own: [`super::egress`]
//! admits it, resolving and pinning its host again, `bake-mcp`'s
//! [`HttpTransport`] and [`McpClient`] run it, and
//! [`serve`](super::server::serve) initializes, lists, and publishes its
//! tools as [`super::server`] does for a stdio server. When it ends, the
//! client is retired before anything replaces it: its tools are withdrawn,
//! so no later prompt declares them, the client is shut down and drained,
//! and the transport's shutdown sends the session `DELETE` with the
//! configured headers only, obtaining and refreshing no credential, and
//! returns once every HTTP exchange has ended. Only then may a new
//! connection start, so two clients never serve at once and nothing is
//! published after the task's end.
//!
//! # Reconnection
//!
//! Pi v1.3.0 keeps the transport's own stream resumption (`Last-Event-ID`
//! with bounded backoff) and reconnects its GET stream, but drops a
//! connection whose session the server expired: its
//! `packages/coding-agent/src/extensions/mcp/runtime.ts` closes the
//! connection and unregisters the tools. Bake 0.3's supervisor
//! (`packages/mcp/mcp-client/src/connection.ts`) does reconnect, with one
//! attempt budget per outage, and the roadmap's connection-recovery row
//! (`docs/roadmap/rust-0.4/pi-reference.md`) keeps Bake's budget where one
//! exists: this module runs 0.3's policy over Pi's transport.
//!
//! - A failed attempt or a lost connection waits
//!   [`McpHttpOptions::reconnect_initial_delay`], doubling per consecutive
//!   failure up to [`McpHttpOptions::reconnect_max_delay`], and gives up
//!   after [`McpHttpOptions::max_reconnect_attempts`] consecutive failures,
//!   as 0.3's `initialDelayMs`, `maxDelayMs`, and `maxAttempts` do.
//! - A connection that served tools for longer than that ceiling ends the
//!   outage, so the next disconnect starts a fresh budget while a server
//!   that keeps dropping still exhausts it, as 0.3's stability window does.
//! - An HTTP 404 for a request that carried the session ID means the server
//!   expired the session (`HttpError::SessionExpired`). An established
//!   connection re-initializes at once, as the specification asks and Pi's
//!   transport documents, and the new listing is published as one new
//!   generation; the attempt still counts, so a server that answers 404
//!   forever stops within the same budget.
//! - A server that answers 401, or 403 with an `insufficient_scope`
//!   challenge, needs OAuth, which `bake-rs` does not support yet: the
//!   server fails with that message and is not retried.
//! - A tool call in flight when a connection ends fails with the
//!   transport's error. It is never retried and never sent again, on this
//!   connection or the next: the server may have run it already
//!   (`bake-mcp`'s HTTP transport posts a request once).
//! - A refusal of the configuration itself — an unusable URL, a header that
//!   runs a command or names an unset variable, plain `http` to a public
//!   address — is final, since no later attempt would decide otherwise.

use std::time::Duration;

use bake_ai::AbortSignal;
use bake_mcp::{HttpError, HttpTransport, McpClient};
use tokio::time::Instant;

use super::config::{McpHttpConfig, McpServerEntry};
use super::egress::{self, AdmissionError};
use super::server::{
    Connection, End, ServerContext, client_options, serve, stop_client, unless_aborted,
};
use super::{McpHttpOptions, McpServerState};

/// How one connection attempt ended.
enum Attempt {
    /// The session is shutting down.
    Cancelled,
    /// The configuration is not admissible; nothing is retried.
    Refused(String),
    /// The attempt failed; a later one may succeed.
    Failed(String),
    /// It served and then the connection ended.
    Disconnected(String),
    /// The server expired the HTTP session (HTTP 404).
    Expired(String),
    /// The server requires authorization, which needs OAuth.
    Unauthorized(String),
}

/// Runs one Streamable HTTP server until the session's shutdown, a final
/// refusal, or an exhausted reconnect budget.
pub(super) async fn run(
    context: &ServerContext,
    entry: &McpServerEntry,
    config: &McpHttpConfig,
    signal: &AbortSignal,
) {
    let servers = &context.servers;
    let index = context.index;
    let settings = context.http;
    let mut attempts = 0_u32;
    let mut generation = 0_u64;
    let mut ever_served = false;
    loop {
        let before = generation;
        let started = Instant::now();
        let attempt = connect(context, entry, config, signal, &mut generation).await;
        let served = generation > before;
        ever_served |= served;
        let (error, immediate) = match attempt {
            Attempt::Cancelled => return servers.set_state(index, McpServerState::Closed),
            Attempt::Refused(reason) => {
                return servers.set_state(index, McpServerState::Refused { reason });
            }
            Attempt::Unauthorized(error) => {
                return servers.set_state(index, ended(ever_served, error));
            }
            Attempt::Failed(error) | Attempt::Disconnected(error) => (error, false),
            // An established session the server expired re-initializes at
            // once, as Pi's transport documents.
            Attempt::Expired(error) => (error, served),
        };
        // 0.3's stability window: an attempt that served tools and lasted
        // longer than the backoff ceiling closes the outage and starts a
        // fresh budget.
        if served && started.elapsed() > settings.reconnect_max_delay {
            attempts = 0;
        }
        if attempts >= settings.max_reconnect_attempts {
            let error = format!(
                "{error}; bake-rs gave up after {attempts} reconnect attempts for MCP server \"{}\"",
                entry.name
            );
            return servers.set_state(index, ended(ever_served, error));
        }
        let delay = if immediate {
            Duration::ZERO
        } else {
            backoff(settings, attempts)
        };
        attempts += 1;
        servers.set_state(index, McpServerState::Connecting);
        if unless_aborted(signal, tokio::time::sleep(delay))
            .await
            .is_none()
        {
            return servers.set_state(index, McpServerState::Closed);
        }
    }
}

/// The final state of a server that is not retried: `Disconnected` once it
/// served tools, else `Failed`.
fn ended(ever_served: bool, error: String) -> McpServerState {
    if ever_served {
        McpServerState::Disconnected { error }
    } else {
        McpServerState::Failed { error }
    }
}

/// 0.3's bounded exponential backoff: the initial delay doubled per
/// consecutive failed attempt, capped at the ceiling.
fn backoff(settings: McpHttpOptions, attempts: u32) -> Duration {
    let factor = 1_u32.checked_shl(attempts).unwrap_or(u32::MAX);
    settings
        .reconnect_initial_delay
        .saturating_mul(factor)
        .min(settings.reconnect_max_delay)
}

/// One connection, from admission to an awaited stop. Whatever happens, the
/// client and the transport are shut down before this returns, and the
/// server's tools are withdrawn.
async fn connect(
    context: &ServerContext,
    entry: &McpServerEntry,
    config: &McpHttpConfig,
    signal: &AbortSignal,
    generation: &mut u64,
) -> Attempt {
    let name = &entry.name;
    let index = context.index;
    let admission = egress::admit(
        name,
        config,
        &context.http,
        &context.resolver,
        &context.work,
    );
    let admitted = match unless_aborted(signal, admission).await {
        None => return Attempt::Cancelled,
        Some(Err(AdmissionError::Refused(reason))) => return Attempt::Refused(reason),
        Some(Err(AdmissionError::Unresolved(error))) => return Attempt::Failed(error),
        Some(Ok(admitted)) => admitted,
    };
    let (transport, receiver, sender) =
        match HttpTransport::start_with_client(admitted.options, admitted.client) {
            Ok(started) => started,
            Err(error) => {
                return Attempt::Refused(format!(
                    "MCP server \"{name}\" could not start its HTTP transport: {error}"
                ));
            }
        };
    // Owned from here: every path below shuts the transport down, so no
    // exchange outlives this attempt.
    let connecting = match McpClient::start(receiver, sender, client_options(entry)) {
        Ok(connecting) => connecting,
        Err(error) => {
            transport.shutdown().await;
            return Attempt::Failed(format!(
                "MCP server \"{name}\" started, but its client did not: {error}"
            ));
        }
    };
    let mut connection = Connection::Connecting(Box::new(connecting));
    let end = serve(context, entry, signal, &mut connection, generation).await;
    // No later prompt declares the tools of a client being retired.
    context
        .servers
        .withdraw(index, McpServerState::Stopping { retrying: false });
    stop_client(&connection).await;
    transport.shutdown().await;
    classify(name, end, transport.failure())
}

/// Why the attempt ended: a fatal HTTP failure the transport recorded names
/// it; otherwise the connection's own end does.
fn classify(name: &str, end: End, failure: Option<HttpError>) -> Attempt {
    if matches!(end, End::Cancelled) {
        return Attempt::Cancelled;
    }
    match failure {
        Some(HttpError::AuthorizationRequired { status, .. }) => Attempt::Unauthorized(format!(
            "MCP server \"{name}\" requires authorization (HTTP {status}); bake-rs does not support OAuth sign-in yet, so give the server a credential in its \"headers\" in mcp.json"
        )),
        Some(HttpError::SessionExpired { .. }) => Attempt::Expired(format!(
            "MCP server \"{name}\" ended: the server expired its HTTP session"
        )),
        _ => match end {
            // Reported above; kept exhaustive.
            End::Cancelled => Attempt::Cancelled,
            End::Failed(error) => Attempt::Failed(error),
            End::Disconnected(error) => Attempt::Disconnected(error),
        },
    }
}

/// The resolver a session's servers use: its own, else the system's.
pub(super) fn resolver(options: &super::McpOptions) -> egress::HostResolver {
    options
        .resolver
        .clone()
        .unwrap_or_else(egress::system_resolver)
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
