//! Streamable HTTP MCP servers a session owns, against loopback servers the
//! test owns: the egress policy (cross-origin redirects, pinned addresses,
//! payload bounds), re-initialization after an expired session, a call that
//! fails without a retry, a stalled startup, the session `DELETE`, an
//! awaited shutdown, the OAuth refusal, and redaction.
//!
//! The cases named after a Pi case follow `packages/mcp/test/streamable-http.test.ts`
//! (v1.3.0) through the session instead of the transport; the rest are
//! Bake's, for the admission and reconnection this module adds. No case
//! touches the network: every server binds 127.0.0.1 and every name is
//! answered by the case's own resolver.

use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bake_agent::{AgentTool, AgentToolUpdateCallback, ToolContext};
use bake_ai::UserContentBlock;
use serde_json::{Value, json};

use super::super::egress::HostResolver;
use super::super::http_test_server::{Call, Initialize, Server, Shared, protocol};
use super::super::{
    McpHttpOptions, McpOptions, McpServerState, McpServers, McpTimings, McpToolFilter,
    parse_mcp_config,
};
use crate::owned_work::WorkGate;
use crate::permissions::{ApprovalPolicy, StandingPermissions};

/// Bounds each wait for something the case already caused.
const BOUND: Duration = Duration::from_secs(20);

/// A session's servers and the gate that owns them.
struct Started {
    servers: McpServers,
    work: WorkGate,
}

impl Started {
    /// Shuts the session down and waits until every server's task has
    /// ended, as [`crate::agent_session::AgentSession::shutdown`] does.
    async fn shutdown(&self) {
        self.work.close().await;
    }

    fn state(&self) -> McpServerState {
        self.servers.statuses().swap_remove(0).state
    }

    fn tool(&self, name: &str) -> Arc<AgentTool> {
        self.servers
            .tools()
            .into_iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("{name} is not published: {:?}", self.servers.tool_names()))
    }

    /// Calls a published tool as the agent would.
    async fn call(&self, name: &str) -> Result<String, String> {
        let result = (self.tool(name).execute)(
            "call".into(),
            json!({}),
            None,
            AgentToolUpdateCallback::ignore(),
            ToolContext::detached(),
        )
        .await?;
        let text: String = result
            .content
            .iter()
            .filter_map(|block| match block {
                UserContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect();
        if result.is_error { Err(text) } else { Ok(text) }
    }

    /// Waits until `check` holds for the server's state.
    async fn wait_for(&self, what: &str, check: impl Fn(&McpServerState) -> bool) {
        let mut changes = self.servers.subscribe();
        let deadline = tokio::time::Instant::now() + BOUND;
        loop {
            changes.borrow_and_update();
            let state = self.state();
            if check(&state) {
                return;
            }
            if tokio::time::timeout_at(deadline, changes.changed())
                .await
                .is_err()
            {
                panic!("waiting for {what}: the server stayed {state:?}");
            }
        }
    }
}

/// Starts one `url` server from the configuration a test writes, through
/// the real parser, with `settings` and `resolver`.
fn start(entry: Value, settings: McpHttpOptions, resolver: Option<HostResolver>) -> Started {
    let config = parse_mcp_config(
        &json!({ "mcpServers": { "remote": entry } }).to_string(),
        std::path::Path::new("mcp.json"),
    );
    assert!(config.errors.is_empty(), "{:?}", config.errors);
    let options = McpOptions {
        servers: config.servers,
        standing: Arc::new(StandingPermissions::new(
            // A `url` server needs no sandbox backend: the connection is
            // this process's own, as the web tools' are (D40).
            bake_sandbox::SandboxMode::ReadOnly,
            ApprovalPolicy::Never,
            "custom",
            std::env::temp_dir(),
            None,
        )),
        sandbox: Arc::new(bake_sandbox::provider::UnavailableProvider::new(Some(
            "no backend in this test".into(),
        ))),
        timings: McpTimings {
            startup_wait: BOUND,
            ..McpTimings::default()
        },
        tool_filter: McpToolFilter::default(),
        http: settings,
        resolver,
    };
    super::super::validate(&options).expect("valid options");
    let work = WorkGate::default();
    let (servers, refused) = super::super::start(
        options,
        std::env::temp_dir(),
        vec!["read".to_owned()],
        &work,
    );
    assert_eq!(refused, None);
    Started { servers, work }
}

/// The quickest reconnect budget a case can observe.
fn quick() -> McpHttpOptions {
    McpHttpOptions {
        reconnect_initial_delay: Duration::from_millis(1),
        reconnect_max_delay: Duration::from_millis(20),
        ..McpHttpOptions::default()
    }
}

/// A `url` entry for `url`, with a short request timeout so a stall is a
/// case, not a hang.
fn entry(url: &str) -> Value {
    json!({ "url": url, "timeout": 2.0 })
}

fn connected(state: &McpServerState) -> bool {
    matches!(state, McpServerState::Connected { .. })
}

fn generation(state: &McpServerState) -> u64 {
    match state {
        McpServerState::Connected { generation, .. } => *generation,
        other => panic!("not connected: {other:?}"),
    }
}

fn error_text(state: &McpServerState) -> String {
    match state {
        McpServerState::Failed { error } | McpServerState::Disconnected { error } => error.clone(),
        McpServerState::Refused { reason } => reason.clone(),
        other => panic!("not an error state: {other:?}"),
    }
}

// Pi: "handles JSON and SSE responses with session and protocol headers",
// through the session: the tools of a `url` server are published as one
// generation and callable, every request after `initialize` carries the
// session ID and the negotiated protocol version, and no sandbox backend is
// needed.
#[tokio::test]
async fn a_url_server_publishes_and_calls_its_tools() {
    let state = Shared::default();
    state.set(|state| state.tools = vec!["echo".into(), "ping".into()]);
    let server = Server::start(protocol(&state)).await;
    let started = start(entry(&server.url), quick(), None);
    started.wait_for("the first generation", connected).await;
    assert_eq!(
        started.servers.tool_names(),
        ["mcp__remote__echo", "mcp__remote__ping"]
    );
    assert_eq!(generation(&started.state()), 1);
    assert_eq!(started.call("mcp__remote__echo").await, Ok("hello".into()));
    let listed = server.rpc("tools/list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].header("mcp-session-id"), Some("session-1"));
    assert_eq!(listed[0].header("mcp-protocol-version"), Some("2025-11-25"));
    started.shutdown().await;
    assert_eq!(started.state(), McpServerState::Closed);
    server.stop().await;
}

// Bake's own: `mcp.json` authorizes one origin, so a redirect that leaves
// it is refused before it is sent, while a same-origin one is followed.
#[tokio::test]
async fn a_redirect_to_another_origin_is_refused() {
    let state = Shared::default();
    let server = Server::start(protocol(&state)).await;
    let other = Server::start(protocol(&Shared::default())).await;
    state.set(|state| state.redirect = Some(other.url.clone()));
    let started = start(entry(&server.url), quick(), None);
    started
        .wait_for("the refusal", |state| {
            matches!(
                state,
                McpServerState::Failed { .. } | McpServerState::Disconnected { .. }
            )
        })
        .await;
    let error = error_text(&started.state());
    assert!(
        error.contains("mcp.json authorizes only the configured origin"),
        "{error}"
    );
    assert!(started.servers.tool_names().is_empty());
    // The other origin was never contacted.
    assert!(other.requests().is_empty(), "{:?}", other.requests());
    started.shutdown().await;
    server.stop().await;
    other.stop().await;

    // A same-origin redirect is followed, within the bound.
    let state = Shared::default();
    let server = Server::start(protocol(&state)).await;
    state.set(|state| state.redirect = Some(format!("http://127.0.0.1:{}/moved", server.port)));
    let started = start(entry(&server.url), quick(), None);
    started
        .wait_for("the server to answer somehow", |state| {
            !matches!(state, McpServerState::Connecting)
        })
        .await;
    assert!(
        server
            .requests()
            .iter()
            .any(|request| request.path == "/moved"),
        "{:?}",
        server.requests()
    );
    started.shutdown().await;
    server.stop().await;
}

// Bake's own: the host is resolved once per connection and the addresses
// are pinned, so an answer that changes afterwards cannot move the
// connection (DNS rebinding).
#[tokio::test]
async fn a_changed_dns_answer_cannot_move_a_connection() {
    let state = Shared::default();
    let server = Server::start(protocol(&state)).await;
    let moved = Server::start(protocol(&Shared::default())).await;
    // The same port on another loopback address, which the resolver starts
    // answering with once the connection is up.
    let answer: Arc<Mutex<Vec<IpAddr>>> =
        Arc::new(Mutex::new(vec!["127.0.0.1".parse().expect("address")]));
    let calls = Arc::new(AtomicUsize::new(0));
    let (answers, counted) = (Arc::clone(&answer), Arc::clone(&calls));
    let resolver: HostResolver = Arc::new(move |_host| {
        counted.fetch_add(1, Ordering::SeqCst);
        Ok(answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone())
    });
    let url = format!("http://mcp.test:{}/mcp", server.port);
    let started = start(entry(&url), quick(), Some(resolver));
    started.wait_for("the first generation", connected).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "resolved once per connection"
    );
    // The name now answers with the other server's address.
    *answer.lock().unwrap_or_else(PoisonError::into_inner) =
        vec!["127.0.0.2".parse().expect("address")];
    assert_eq!(started.call("mcp__remote__echo").await, Ok("hello".into()));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no later lookup");
    assert!(
        server
            .requests()
            .iter()
            .any(|request| request.rpc_method().as_deref() == Some("tools/call")),
        "the call reached the pinned server"
    );
    assert!(moved.requests().is_empty(), "{:?}", moved.requests());
    started.shutdown().await;
    server.stop().await;
    moved.stop().await;
}

// Pi: "rejects an oversized SSE event" and "rejects an oversized JSON
// response", through the session: a listing over the bound publishes
// nothing, and an event over it fails its call.
#[tokio::test]
async fn an_oversized_response_or_event_is_refused() {
    let state = Shared::default();
    let server = Server::start(protocol(&state)).await;
    let settings = McpHttpOptions {
        max_message_bytes: std::num::NonZeroUsize::new(4_096).expect("limit"),
        max_reconnect_attempts: 1,
        ..quick()
    };
    state.set(|state| state.oversized_list = Some(8_192));
    let started = start(entry(&server.url), settings, None);
    started
        .wait_for("the refused listing", |state| {
            matches!(state, McpServerState::Failed { .. })
        })
        .await;
    let error = error_text(&started.state());
    assert!(error.contains("exceeds 4096 bytes"), "{error}");
    assert!(started.servers.tool_names().is_empty());
    started.shutdown().await;
    server.stop().await;

    let state = Shared::default();
    let server = Server::start(protocol(&state)).await;
    let settings = McpHttpOptions {
        max_message_bytes: std::num::NonZeroUsize::new(4_096).expect("limit"),
        ..quick()
    };
    state.set(|state| state.call = Call::OversizedEvent(8_192));
    let started = start(entry(&server.url), settings, None);
    started.wait_for("the first generation", connected).await;
    let error = started
        .call("mcp__remote__echo")
        .await
        .expect_err("the oversized event fails the call");
    assert!(error.contains("exceeds 4096 bytes"), "{error}");
    started.shutdown().await;
    server.stop().await;
}

// The specification's session expiry, which Pi's transport reports and Bake
// 0.3 could only answer with a reload: the server re-initializes at once
// and publishes the new listing as one new generation, and the call that
// found the session gone fails without being sent again.
#[tokio::test]
async fn an_expired_session_re_initializes_and_republishes_its_tools() {
    let state = Shared::default();
    let server = Server::start(protocol(&state)).await;
    let started = start(entry(&server.url), quick(), None);
    started.wait_for("the first generation", connected).await;
    assert_eq!(started.servers.tool_names(), ["mcp__remote__echo"]);
    state.set(|state| {
        state.expired.insert("session-1".to_owned());
        state.session = "session-2".to_owned();
        state.tools = vec!["echo".into(), "added".into()];
    });
    let error = started
        .call("mcp__remote__echo")
        .await
        .expect_err("the expired session fails the call");
    assert!(error.contains("expired"), "{error}");
    started
        .wait_for("the second generation", |state| {
            connected(state) && generation(state) == 2
        })
        .await;
    assert_eq!(
        started.servers.tool_names(),
        ["mcp__remote__echo", "mcp__remote__added"]
    );
    assert_eq!(server.rpc("initialize").len(), 2);
    // The call was posted once: its outcome was unknown, so it is never
    // sent again.
    assert_eq!(server.rpc("tools/call").len(), 1);
    assert_eq!(
        server.rpc("tools/list")[1].header("mcp-session-id"),
        Some("session-2")
    );
    assert_eq!(started.call("mcp__remote__added").await, Ok("hello".into()));
    started.shutdown().await;
    server.stop().await;
}

// Pi: "fails the request whose stream breaks", through the session: the
// call fails with the transport's error, is never sent again, and the
// connection keeps serving.
#[tokio::test]
async fn a_disconnect_during_a_tool_call_fails_it_without_a_retry() {
    let state = Shared::default();
    let server = Server::start(protocol(&state)).await;
    let started = start(entry(&server.url), quick(), None);
    started.wait_for("the first generation", connected).await;
    state.set(|state| state.call = Call::Disconnect);
    let error = started
        .call("mcp__remote__echo")
        .await
        .expect_err("the dropped stream fails the call");
    assert!(error.contains("stream"), "{error}");
    assert_eq!(server.rpc("tools/call").len(), 1, "posted once");
    // The connection is unharmed: the same generation answers the next
    // call.
    state.set(|state| state.call = Call::Answer("again".into()));
    assert_eq!(started.call("mcp__remote__echo").await, Ok("again".into()));
    assert_eq!(generation(&started.state()), 1);
    started.shutdown().await;
    server.stop().await;
}

// Pi: "times out a stalled initialize", through the session: the entry's
// `timeout` bounds it, each attempt is counted, and the budget ends the
// server.
#[tokio::test]
async fn a_stalled_startup_times_out_within_the_reconnect_budget() {
    let state = Shared::default();
    state.set(|state| state.initialize = Initialize::Stall(Duration::from_secs(30)));
    let server = Server::start(protocol(&state)).await;
    let settings = McpHttpOptions {
        max_reconnect_attempts: 1,
        ..quick()
    };
    let started = start(json!({ "url": server.url, "timeout": 0.3 }), settings, None);
    started
        .wait_for("the exhausted budget", |state| {
            matches!(state, McpServerState::Failed { .. })
        })
        .await;
    let error = error_text(&started.state());
    assert!(error.contains("failed to connect"), "{error}");
    assert!(
        error.contains("giving up after 1 failed attempts"),
        "{error}"
    );
    assert_eq!(server.rpc("initialize").len(), 2, "one retry, then no more");
    started.shutdown().await;
    server.stop().await;
}

// Pi's transport sends `DELETE` for the session at close, with the headers
// it was given and no credential of its own. Bake's shutdown waits for it.
#[tokio::test]
async fn closing_sends_the_session_delete() {
    let state = Shared::default();
    let server = Server::start(protocol(&state)).await;
    let started = start(
        json!({ "url": server.url, "headers": { "Authorization": "Bearer configured" } }),
        quick(),
        None,
    );
    started.wait_for("the first generation", connected).await;
    started.shutdown().await;
    let deletes = server.with_method("DELETE");
    assert_eq!(deletes.len(), 1, "{:?}", server.requests());
    assert_eq!(deletes[0].header("mcp-session-id"), Some("session-1"));
    assert_eq!(
        deletes[0].header("authorization"),
        Some("Bearer configured"),
        "the configured header, never a refreshed one"
    );
    assert_eq!(started.state(), McpServerState::Closed);
    assert!(started.servers.tool_names().is_empty());
    server.stop().await;
}

// Bake's own: shutdown during a stalled startup returns only once the
// server's task has ended, and nothing is published after it.
#[tokio::test]
async fn shutdown_awaits_a_connecting_server_and_nothing_is_published_late() {
    let state = Shared::default();
    state.set(|state| state.initialize = Initialize::Stall(Duration::from_secs(30)));
    let server = Server::start(protocol(&state)).await;
    let started = start(entry(&server.url), quick(), None);
    // Wait until the server has the connection, so the shutdown lands in
    // the middle of `initialize`.
    let deadline = tokio::time::Instant::now() + BOUND;
    while server.rpc("initialize").is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "no initialize");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    started.shutdown().await;
    assert_eq!(started.state(), McpServerState::Closed);
    assert!(started.servers.tool_names().is_empty());
    // Nothing starts or publishes after the gate closed.
    let requests = server.requests().len();
    state.set(|state| state.initialize = Initialize::Answer);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(server.requests().len(), requests);
    assert_eq!(started.state(), McpServerState::Closed);
    assert!(started.servers.tool_names().is_empty());
    server.stop().await;
}

// OAuth is not implemented: a server that asks for authorization says so
// and is not retried, since no later attempt would carry a credential.
#[tokio::test]
async fn an_authorization_challenge_says_oauth_is_not_supported() {
    let state = Shared::default();
    state.set(|state| {
        state.initialize = Initialize::Status(
            401,
            Some("Bearer resource_metadata=\"https://auth.example.com/.well-known\"".to_owned()),
        );
    });
    let server = Server::start(protocol(&state)).await;
    let started = start(entry(&server.url), quick(), None);
    started
        .wait_for("the refusal", |state| {
            matches!(state, McpServerState::Failed { .. })
        })
        .await;
    let error = error_text(&started.state());
    assert!(
        error.contains("requires authorization (HTTP 401)"),
        "{error}"
    );
    assert!(
        error.contains("does not support OAuth sign-in yet"),
        "{error}"
    );
    assert_eq!(server.rpc("initialize").len(), 1, "not retried");
    started.shutdown().await;
    server.stop().await;
}

// Bake's own: a configured header value is a secret. It reaches the server
// and nothing else: not an error the server's own body echoes it into, and
// not `Debug` output. `bake-mcp` removes each exact configured value, so a
// server that split or re-encoded one would still pass it on; a bare-token
// header and a whole `Bearer` value, which are what `mcp.json` configures,
// are both removed.
#[tokio::test]
async fn header_values_never_appear_in_an_error_or_a_status() {
    const KEY: &str = "sk-mcp-http-test-api-key";
    const TOKEN: &str = "mcp-http-test-bearer-token";
    let state = Shared::default();
    let server = Server::start(protocol(&state)).await;
    state.set(|state| state.list_error = Some("rejected:".to_owned()));
    let settings = McpHttpOptions {
        max_reconnect_attempts: 1,
        ..quick()
    };
    let started = start(
        json!({
            "url": server.url,
            "headers": { "X-Api-Key": KEY, "Authorization": format!("Bearer {TOKEN}") },
        }),
        settings,
        None,
    );
    started
        .wait_for("the failed listing", |state| {
            matches!(state, McpServerState::Failed { .. })
        })
        .await;
    let error = error_text(&started.state());
    assert!(error.contains("failed to list its tools"), "{error}");
    assert!(error.contains("[redacted]"), "{error}");
    assert!(!error.contains(KEY) && !error.contains(TOKEN), "{error}");
    let shown = format!("{:?}", started.servers);
    assert!(!shown.contains(KEY) && !shown.contains(TOKEN), "{shown}");
    let listed = &server.rpc("tools/list")[0];
    assert_eq!(
        (listed.header("x-api-key"), listed.header("authorization")),
        (Some(KEY), Some(format!("Bearer {TOKEN}").as_str())),
        "the server is the only one that sees them"
    );
    started.shutdown().await;
    server.stop().await;
}
