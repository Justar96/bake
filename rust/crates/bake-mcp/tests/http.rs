//! The Streamable HTTP transport against loopback servers.
//!
//! Tests named after a Pi case port it from Pi v1.3.0's
//! `packages/mcp/test/streamable-http.test.ts`; the others are Bake's, for
//! the ownership, bounds, and credential isolation this crate adds. Each test
//! owns its server and port, and every wait is bounded. Races are placed
//! with barriers (`oneshot`, `Notify`) that the fixture's handlers wait on,
//! not with sleeps.

#[path = "http/fixture.rs"]
mod fixture;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bake_mcp::{
    ClientEvent, ClientEvents, ClientOptions, CloseReason, ConnectError, Diagnostic, HttpError,
    HttpOptions, HttpTransport, Implementation, McpClient, MessageReceiver, MessageSender,
    ProtocolVersion, RequestError, SessionClose,
};
use fixture::{
    Request, Responder, SESSION, Server, bounded, handler, initialize_result, protocol, tool_result,
};
use serde_json::json;
use tokio::sync::{Notify, oneshot};

fn client_options() -> ClientOptions {
    let mut options = ClientOptions::new(Implementation::new("http-test", "1.0.0"));
    options.request_timeout = Some(Duration::from_secs(20));
    options
}

fn http(server: &Server) -> HttpOptions {
    let mut options = HttpOptions::new(server.url.clone());
    options.reconnect.initial_delay = Duration::from_millis(1);
    options
}

async fn connect(options: HttpOptions) -> (HttpTransport, McpClient, ClientEvents) {
    let (transport, receiver, sender) = HttpTransport::start(options).unwrap();
    let mut connecting = McpClient::start(receiver, sender, client_options()).unwrap();
    let (client, events) = bounded("initialize", connecting.initialize())
        .await
        .unwrap();
    (transport, client, events)
}

async fn connect_error(options: HttpOptions) -> (HttpTransport, ConnectError) {
    let (transport, receiver, sender) = HttpTransport::start(options).unwrap();
    let mut connecting = McpClient::start(receiver, sender, client_options()).unwrap();
    let error = bounded("initialize", connecting.initialize())
        .await
        .unwrap_err();
    (transport, error)
}

async fn close(transport: HttpTransport, client: McpClient) -> SessionClose {
    bounded("the client's shutdown", client.shutdown()).await;
    bounded("the transport's shutdown", transport.shutdown()).await
}

fn text(result: &bake_mcp::CallToolResult) -> &str {
    result.content[0]["text"].as_str().unwrap()
}

fn server_error(error: &RequestError) -> String {
    match error {
        RequestError::Server { error, .. } => error.message.clone(),
        other => panic!("not a server error: {other:?}"),
    }
}

#[tokio::test]
async fn handles_json_and_sse_responses_with_session_and_protocol_headers() {
    // Pi: "handles JSON and SSE responses with session and protocol headers",
    // with "opens the GET stream after initialization and sends
    // Last-Event-ID only when resuming".
    let server = Server::start(handler(protocol)).await;
    let (transport, client, _events) =
        connect(http(&server).header("Authorization", "Bearer static")).await;
    assert!(transport.has_session());
    assert_eq!(
        transport.protocol_version(),
        Some(ProtocolVersion::V2025_11_25)
    );
    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools[0].name(), "echo");
    let result = client.call_tool("echo", None).await.unwrap();
    assert_eq!(text(&result), "hello");
    assert_eq!(
        close(transport, client).await,
        SessionClose::Deleted { status: 200 }
    );

    let requests = server.requests();
    let initialize = &requests[0];
    assert_eq!(initialize.rpc_method().as_deref(), Some("initialize"));
    assert_eq!(initialize.header("mcp-session-id"), None);
    assert_eq!(initialize.header("mcp-protocol-version"), None);
    assert_eq!(
        initialize.header("accept"),
        Some("application/json, text/event-stream")
    );
    assert_eq!(initialize.header("content-type"), Some("application/json"));
    for request in &requests[1..] {
        assert_eq!(
            request.header("mcp-session-id"),
            Some(SESSION),
            "{request:?}"
        );
        assert_eq!(request.header("mcp-protocol-version"), Some("2025-11-25"));
    }
    for request in &requests {
        assert_eq!(request.header("authorization"), Some("Bearer static"));
        assert_eq!(request.header("last-event-id"), None);
    }
    let order: Vec<String> = requests
        .iter()
        .map(|request| {
            request
                .rpc_method()
                .unwrap_or_else(|| request.method.clone())
        })
        .collect();
    let initialized = order
        .iter()
        .position(|entry| entry == "notifications/initialized")
        .unwrap();
    let get = order.iter().position(|entry| entry == "GET").unwrap();
    assert!(get > initialized, "{order:?}");
    assert_eq!(order.last().map(String::as_str), Some("DELETE"));
    server.stop().await;
}

#[tokio::test]
async fn authorization_required_is_typed_and_ends_the_connection() {
    // Pi: "classifies authentication failures".
    let server = Server::start(handler(|_, responder: Responder| async move {
        responder
            .send(
                401,
                &[(
                    "www-authenticate",
                    r#"Bearer resource_metadata="https://example.com/meta""#,
                )],
                b"login required",
            )
            .await;
    }))
    .await;
    let (transport, error) = connect_error(http(&server)).await;
    let ConnectError::Initialize(RequestError::Closed(CloseReason::Receive { kind, message })) =
        error
    else {
        panic!("{error:?}");
    };
    assert_eq!(kind, std::io::ErrorKind::PermissionDenied);
    assert_eq!(message, "MCP server requires authorization (HTTP 401)");
    assert_eq!(
        transport.failure(),
        Some(HttpError::AuthorizationRequired {
            status: 401,
            challenge: Some(r#"Bearer resource_metadata="https://example.com/meta""#.into()),
            body: "login required".into(),
        })
    );
    assert_eq!(
        bounded("shutdown", transport.shutdown()).await,
        SessionClose::NoSession
    );
    server.stop().await;
}

#[tokio::test]
async fn insufficient_scope_is_authorization_and_a_plain_403_fails_one_request() {
    let server = Server::start(handler(
        |request: Request, responder: Responder| async move {
            let name = request.json()["params"]["name"].as_str().map(str::to_owned);
            match name.as_deref() {
                Some("forbidden") => responder.send(403, &[], b"origin refused").await,
                Some("admin") => {
                    responder
                        .send(
                            403,
                            &[(
                                "www-authenticate",
                                r#"Bearer error="insufficient_scope", scope="admin""#,
                            )],
                            b"",
                        )
                        .await
                }
                _ => protocol(request, responder).await,
            }
        },
    ))
    .await;
    let mut options = http(&server);
    options.open_server_stream = false;
    let (transport, client, _events) = connect(options).await;
    let error = client.call_tool("forbidden", None).await.unwrap_err();
    assert_eq!(
        server_error(&error),
        "MCP HTTP request failed with status 403: origin refused"
    );
    assert_eq!(transport.failure(), None);
    // The connection still works.
    client.call_tool("echo", None).await.unwrap();
    let error = client.call_tool("admin", None).await.unwrap_err();
    assert!(
        matches!(error, RequestError::Closed(CloseReason::Receive { .. })),
        "{error:?}"
    );
    assert!(matches!(
        transport.failure(),
        Some(HttpError::AuthorizationRequired { status: 403, .. })
    ));
    close(transport, client).await;
    server.stop().await;
}

#[tokio::test]
async fn fails_only_the_request_whose_sse_stream_breaks() {
    // Pi: "fails only the request whose SSE stream breaks".
    let slow_gate = Arc::new(Notify::new());
    let server = Server::start(handler({
        let slow_gate = slow_gate.clone();
        move |request: Request, responder: Responder| {
            let slow_gate = slow_gate.clone();
            async move {
                let name = request.json()["params"]["name"].as_str().map(str::to_owned);
                match name.as_deref() {
                    Some("broken") => {
                        let mut responder = responder;
                        let _ = responder
                            .head(200, &[("content-type", "text/event-stream")])
                            .await;
                        let _ = responder.write(b"data: not json\n\n").await;
                    }
                    Some("slow") => {
                        slow_gate.notified().await;
                        protocol(request, responder).await;
                    }
                    _ => protocol(request, responder).await,
                }
            }
        }
    }))
    .await;
    let mut options = http(&server);
    options.open_server_stream = false;
    let (transport, client, mut events) = connect(options).await;
    let client = Arc::new(client);
    let slow = tokio::spawn({
        let client = client.clone();
        async move { client.call_tool("slow", None).await }
    });
    let error = client.call_tool("broken", None).await.unwrap_err();
    assert!(
        server_error(&error).starts_with("MCP response stream failed"),
        "{error:?}"
    );
    slow_gate.notify_one();
    let result = bounded("the slow call", slow).await.unwrap().unwrap();
    assert_eq!(text(&result), "hello");
    let event = events.try_recv().unwrap();
    assert!(
        matches!(
            event,
            ClientEvent::Diagnostic(Diagnostic::MalformedMessage { .. })
        ),
        "{event:?}"
    );
    assert!(events.try_recv().is_none());
    let client = Arc::into_inner(client).unwrap();
    close(transport, client).await;
    server.stop().await;
}

#[tokio::test]
async fn resumes_a_response_stream_the_server_closed_before_answering() {
    // Pi: "resumes a response stream the server closed before answering".
    let resumed = Arc::new(Mutex::new(Vec::new()));
    let server = Server::start(handler({
        let resumed = resumed.clone();
        move |request: Request, responder: Responder| {
            let resumed = resumed.clone();
            async move {
                if let Some(last) = request.header("last-event-id") {
                    resumed.lock().unwrap().push(last.to_owned());
                    let mut responder = responder;
                    let _ = responder
                        .head(200, &[("content-type", "text/event-stream")])
                        .await;
                    let event = format!("id: 2\ndata: {}\n\n", tool_result(&json!(2), "resumed"));
                    let _ = responder.write(event.as_bytes()).await;
                    return;
                }
                if request.rpc_method().as_deref() != Some("tools/call") {
                    return protocol(request, responder).await;
                }
                // Priming event (ID, no data) and a retry hint, then the
                // server drops the stream.
                let mut responder = responder;
                let _ = responder
                    .head(200, &[("content-type", "text/event-stream")])
                    .await;
                let _ = responder.write(b"id: 1\nretry: 5\ndata:\n\n").await;
            }
        }
    }))
    .await;
    let mut options = http(&server);
    options.open_server_stream = false;
    let (transport, client, mut events) = connect(options).await;
    let result = client.call_tool("echo", None).await.unwrap();
    assert_eq!(text(&result), "resumed");
    assert_eq!(*resumed.lock().unwrap(), ["1"]);
    assert!(events.try_recv().is_none());
    // The tool ran once: resumption is a GET, never a second POST.
    let posts = server
        .requests()
        .iter()
        .filter(|request| request.rpc_method().as_deref() == Some("tools/call"))
        .count();
    assert_eq!(posts, 1);
    close(transport, client).await;
    server.stop().await;
}

#[tokio::test]
async fn fails_a_request_whose_response_stream_ends_without_an_answer() {
    // Pi: "fails a request whose response stream ends without an answer".
    let server = Server::start(handler(
        |request: Request, responder: Responder| async move {
            if request.rpc_method().as_deref() != Some("tools/call") {
                return protocol(request, responder).await;
            }
            let mut responder = responder;
            let _ = responder
                .head(200, &[("content-type", "text/event-stream")])
                .await;
            let _ = responder.write(b": nothing here\n\n").await;
        },
    ))
    .await;
    let mut options = http(&server);
    options.open_server_stream = false;
    let (transport, client, _events) = connect(options).await;
    let error = client.call_tool("echo", None).await.unwrap_err();
    assert_eq!(
        server_error(&error),
        "MCP response stream failed: stream ended without a response"
    );
    close(transport, client).await;
    server.stop().await;
}

#[tokio::test]
async fn reconnects_the_get_stream_after_it_drops() {
    // Pi: "reconnects the GET stream after it drops".
    let gets = Arc::new(AtomicUsize::new(0));
    let last_ids = Arc::new(Mutex::new(Vec::new()));
    let server = Server::start(handler({
        let gets = gets.clone();
        let last_ids = last_ids.clone();
        move |request: Request, responder: Responder| {
            let gets = gets.clone();
            let last_ids = last_ids.clone();
            async move {
                if request.method != "GET" {
                    return protocol(request, responder).await;
                }
                let count = gets.fetch_add(1, Ordering::SeqCst) + 1;
                last_ids
                    .lock()
                    .unwrap()
                    .push(request.header("last-event-id").map(str::to_owned));
                let mut responder = responder;
                let _ = responder
                    .head(200, &[("content-type", "text/event-stream")])
                    .await;
                let notification =
                    json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"});
                let _ = responder
                    .write(format!("id: g{count}\ndata: {notification}\n\n").as_bytes())
                    .await;
                if count > 1 {
                    // Stay open until the client goes.
                    responder.closed().await;
                }
            }
        }
    }))
    .await;
    let (transport, client, _events) = connect(http(&server)).await;
    let mut changes = client.tool_list_changes();
    bounded("two list changes", async {
        while changes.generation() < 2 {
            changes.changed().await.unwrap();
        }
    })
    .await;
    assert_eq!(*last_ids.lock().unwrap(), [None, Some("g1".to_owned())]);
    close(transport, client).await;
    server.stop().await;
}

#[tokio::test]
async fn rejects_a_request_the_server_accepts_without_a_response() {
    // Pi: "rejects a request the server accepts without a response".
    let server = Server::start(handler(
        |request: Request, responder: Responder| async move {
            if request.rpc_method().as_deref() == Some("tools/call") {
                return responder.status(202).await;
            }
            protocol(request, responder).await;
        },
    ))
    .await;
    let mut options = http(&server);
    options.open_server_stream = false;
    let (transport, client, _events) = connect(options).await;
    let error = client.call_tool("echo", None).await.unwrap_err();
    assert!(
        server_error(&error).contains("without a response"),
        "{error:?}"
    );
    close(transport, client).await;
    server.stop().await;
}

#[tokio::test]
async fn includes_the_response_body_in_http_errors() {
    // Pi: "includes the response body in HTTP errors".
    let server = Server::start(handler(|_, responder: Responder| async move {
        responder.send(400, &[], b"Invalid Accept header").await;
    }))
    .await;
    let (transport, error) = connect_error(http(&server)).await;
    let ConnectError::Initialize(error) = error else {
        panic!("{error:?}");
    };
    assert_eq!(
        server_error(&error),
        "MCP HTTP request failed with status 400: Invalid Accept header"
    );
    assert_eq!(transport.failure(), None);
    bounded("shutdown", transport.shutdown()).await;
    server.stop().await;
}

#[tokio::test]
async fn classifies_an_expired_established_session() {
    // Pi: "classifies an expired established session". The session is not
    // re-initialized, and no DELETE is sent for it.
    let server = Server::start(handler(
        |request: Request, responder: Responder| async move {
            if request.rpc_method().as_deref() == Some("tools/list") {
                return responder.send(404, &[], b"gone").await;
            }
            protocol(request, responder).await;
        },
    ))
    .await;
    let mut options = http(&server);
    options.open_server_stream = false;
    let (transport, client, _events) = connect(options).await;
    let error = client.list_tools().await.unwrap_err();
    let RequestError::Closed(CloseReason::Receive { kind, message }) = &error else {
        panic!("{error:?}");
    };
    assert_eq!(*kind, std::io::ErrorKind::NotConnected);
    assert_eq!(message, "MCP session expired: gone");
    assert_eq!(
        transport.failure(),
        Some(HttpError::SessionExpired {
            body: "gone".into()
        })
    );
    assert_eq!(close(transport, client).await, SessionClose::Expired);
    let initializes = server
        .requests()
        .iter()
        .filter(|request| request.rpc_method().as_deref() == Some("initialize"))
        .count();
    assert_eq!(initializes, 1);
    assert!(
        server
            .requests()
            .iter()
            .all(|request| request.method != "DELETE")
    );
    server.stop().await;
}

#[tokio::test]
async fn redirects_are_not_followed_and_configured_secrets_stay_out_of_errors() {
    let elsewhere = Server::start(handler(protocol)).await;
    let target = format!("{}/steal?token=query-secret", elsewhere.origin);
    let server = Server::start(handler(move |request: Request, responder: Responder| {
        let target = target.clone();
        async move {
            let name = request.json()["params"]["name"].as_str().map(str::to_owned);
            match name.as_deref() {
                Some("redirect") => responder.send(307, &[("location", &target)], b"").await,
                Some("echo-secret") => {
                    let auth = request.header("authorization").unwrap_or("").to_owned();
                    let body = format!("rejected {auth} for session {SESSION}");
                    responder.send(500, &[], body.as_bytes()).await;
                }
                _ => protocol(request, responder).await,
            }
        }
    }))
    .await;
    let mut options = HttpOptions::new(format!("{}?key=url-secret", server.url))
        .header("Authorization", "Bearer header-secret");
    options.open_server_stream = false;
    let (transport, client, _events) = connect(options).await;
    let error = client.call_tool("redirect", None).await.unwrap_err();
    assert_eq!(
        server_error(&error),
        "MCP server answered with a redirect (HTTP 307), which is not followed"
    );
    assert_eq!(elsewhere.connections(), 0, "the redirect was followed");
    let error = client.call_tool("echo-secret", None).await.unwrap_err();
    let message = server_error(&error);
    assert_eq!(
        message,
        "MCP HTTP request failed with status 500: rejected [redacted] for session [redacted]"
    );
    let debug = format!("{transport:?} {error:?}");
    for secret in ["header-secret", "url-secret", SESSION] {
        assert!(!debug.contains(secret), "{secret} in {debug}");
    }
    close(transport, client).await;
    server.stop().await;
    elsewhere.stop().await;
}

// Bake's own: a caller that admits egress supplies the HTTP client, which
// then owns resolution, pinning, and redirects. A redirect its policy
// follows reaches the new path; one it refuses fails the request, with the
// policy's reason, and the transport still contacts nobody else.
#[tokio::test]
async fn a_supplied_client_owns_the_redirect_policy() {
    #[derive(Debug)]
    struct Refused;

    impl std::fmt::Display for Refused {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a cross-origin redirect is refused")
        }
    }

    impl std::error::Error for Refused {}

    let elsewhere = Server::start(handler(protocol)).await;
    let other_origin = elsewhere.origin.clone();
    let moved = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&moved);
    let server = Server::start(handler(move |request: Request, responder: Responder| {
        let other = format!("{other_origin}/mcp");
        let counted = Arc::clone(&counted);
        async move {
            if request.path == "/moved" {
                counted.fetch_add(1, Ordering::SeqCst);
                return protocol(request, responder).await;
            }
            match request.json()["params"]["name"].as_str() {
                Some("same-origin") => {
                    responder.send(307, &[("location", "/moved")], b"").await;
                }
                Some("cross-origin") => {
                    responder.send(307, &[("location", &other)], b"").await;
                }
                _ => protocol(request, responder).await,
            }
        }
    }))
    .await;
    let authorized = reqwest::Url::parse(&server.url).unwrap();
    let client = bake_mcp::http::client_builder()
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            let same = attempt.url().scheme() == authorized.scheme()
                && attempt.url().host_str() == authorized.host_str()
                && attempt.url().port() == authorized.port();
            if same {
                attempt.follow()
            } else {
                attempt.error(Refused)
            }
        }))
        .build()
        .unwrap();
    let (transport, receiver, sender) =
        HttpTransport::start_with_client(http(&server), client).unwrap();
    let mut connecting = McpClient::start(receiver, sender, client_options()).unwrap();
    let (client, _events) = bounded("initialize", connecting.initialize())
        .await
        .unwrap();
    let result = client.call_tool("same-origin", None).await.unwrap();
    assert_eq!(text(&result), "hello");
    assert_eq!(moved.load(Ordering::SeqCst), 1);
    let error = client.call_tool("cross-origin", None).await.unwrap_err();
    assert!(
        server_error(&error).contains("a cross-origin redirect is refused"),
        "{error:?}"
    );
    assert_eq!(
        elsewhere.connections(),
        0,
        "the refused target was contacted"
    );
    close(transport, client).await;
    server.stop().await;
    elsewhere.stop().await;
}

#[tokio::test]
async fn split_bytes_and_utf8_across_writes_decode_whole() {
    let expected = "héllo 😀 世界";
    let server = Server::start(handler(
        move |request: Request, responder: Responder| async move {
            if request.rpc_method().as_deref() != Some("tools/call") {
                return protocol(request, responder).await;
            }
            let id = request.json()["id"].clone();
            let body = format!(
                ": comment\r\nid: 1\r\nevent: message\r\ndata: {}\r\n\r\n",
                tool_result(&id, expected)
            );
            let mut responder = responder;
            let _ = responder
                .head(200, &[("content-type", "text/event-stream")])
                .await;
            for byte in body.as_bytes() {
                if responder.write(&[*byte]).await.is_err() {
                    return;
                }
            }
        },
    ))
    .await;
    let mut options = http(&server);
    options.open_server_stream = false;
    let (transport, client, _events) = connect(options).await;
    let result = client.call_tool("echo", None).await.unwrap();
    assert_eq!(text(&result), expected);
    close(transport, client).await;
    server.stop().await;
}

#[tokio::test]
async fn an_oversized_event_fails_its_request_without_reading_on() {
    let ended = Arc::new(Notify::new());
    let server = Server::start(handler({
        let ended = ended.clone();
        move |request: Request, responder: Responder| {
            let ended = ended.clone();
            async move {
                if request.rpc_method().as_deref() != Some("tools/call") {
                    return protocol(request, responder).await;
                }
                let mut responder = responder;
                let _ = responder
                    .head(200, &[("content-type", "text/event-stream")])
                    .await;
                // Data lines without end; only the client can stop this.
                let line = format!("data: {}\n", "x".repeat(1024));
                while responder.write(line.as_bytes()).await.is_ok() {}
                ended.notify_one();
            }
        }
    }))
    .await;
    let mut options = http(&server);
    options.open_server_stream = false;
    options.limits.max_message_bytes = std::num::NonZeroUsize::new(64 * 1024).unwrap();
    let (transport, client, _events) = connect(options).await;
    let error = client.call_tool("echo", None).await.unwrap_err();
    assert_eq!(
        server_error(&error),
        "MCP response stream failed: MCP HTTP message exceeds 65536 bytes"
    );
    // The transport dropped the stream: the server's writes fail.
    bounded("the server to see the stream dropped", ended.notified()).await;
    close(transport, client).await;
    server.stop().await;
}

#[tokio::test]
async fn an_oversized_json_body_is_refused() {
    let server = Server::start(handler(|request: Request, responder: Responder| async move {
        if request.rpc_method().as_deref() != Some("tools/list") {
            return protocol(request, responder).await;
        }
        let id = request.json()["id"].clone();
        let big = json!({"jsonrpc": "2.0", "id": id, "result": {"tools": [], "pad": "x".repeat(4096)}});
        responder.json(&[], &big).await;
    }))
    .await;
    let mut options = http(&server);
    options.open_server_stream = false;
    options.limits.max_message_bytes = std::num::NonZeroUsize::new(1024).unwrap();
    let (transport, client, _events) = connect(options).await;
    let error = client.list_tools().await.unwrap_err();
    assert_eq!(server_error(&error), "MCP HTTP message exceeds 1024 bytes");
    close(transport, client).await;
    server.stop().await;
}

#[tokio::test]
async fn a_malformed_session_id_fails_initialization() {
    let server = Server::start(handler(
        |request: Request, responder: Responder| async move {
            let id = request.json()["id"].clone();
            responder
                .json(&[("mcp-session-id", "has space")], &initialize_result(&id))
                .await;
        },
    ))
    .await;
    let (transport, error) = connect_error(http(&server)).await;
    let ConnectError::Initialize(error) = error else {
        panic!("{error:?}");
    };
    assert_eq!(
        server_error(&error),
        "MCP server sent a session ID that is not visible ASCII"
    );
    assert!(!transport.has_session());
    bounded("shutdown", transport.shutdown()).await;
    server.stop().await;
}

#[tokio::test]
async fn a_stalled_initialize_times_out_and_shutdown_releases_its_connection() {
    let (seen, waiting) = oneshot::channel();
    let seen = Arc::new(Mutex::new(Some(seen)));
    let (dropped, gone) = oneshot::channel();
    let dropped = Arc::new(Mutex::new(Some(dropped)));
    let server = Server::start(handler(move |_, mut responder: Responder| {
        let seen = seen.lock().unwrap().take();
        let dropped = dropped.lock().unwrap().take();
        async move {
            if let Some(seen) = seen {
                let _ = seen.send(());
            }
            // Never answer; report when the client lets go.
            responder.closed().await;
            if let Some(dropped) = dropped {
                let _ = dropped.send(());
            }
        }
    }))
    .await;
    let (transport, receiver, sender) = HttpTransport::start(http(&server)).unwrap();
    let mut options = client_options();
    options.request_timeout = Some(Duration::from_millis(300));
    let mut connecting = McpClient::start(receiver, sender, options).unwrap();
    let error = bounded("initialize", connecting.initialize())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ConnectError::Initialize(RequestError::Timeout { .. })
    ));
    bounded("the server to see the POST", waiting)
        .await
        .unwrap();
    bounded("the transport's shutdown", transport.shutdown()).await;
    bounded("the stalled POST to be dropped", gone)
        .await
        .unwrap();
    server.stop().await;
}

#[tokio::test]
async fn requests_and_notifications_interleave_across_streams() {
    let get_ready = Arc::new(Notify::new());
    let server = Server::start(handler({
        let get_ready = get_ready.clone();
        move |request: Request, responder: Responder| {
            let get_ready = get_ready.clone();
            async move {
                let mut responder = responder;
                if request.method == "GET" {
                    let _ = responder
                        .head(200, &[("content-type", "text/event-stream")])
                        .await;
                    get_ready.notified().await;
                    let log = json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {"data": "from get"}});
                    let _ = responder.write(format!("data: {log}\n\n").as_bytes()).await;
                    responder.closed().await;
                    return;
                }
                if request.rpc_method().as_deref() != Some("tools/call") {
                    return protocol(request, responder).await;
                }
                let id = request.json()["id"].clone();
                let _ = responder
                    .head(200, &[("content-type", "text/event-stream")])
                    .await;
                let progress = json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {"data": "from post"}});
                let _ = responder
                    .write(format!("data: {progress}\n\n").as_bytes())
                    .await;
                get_ready.notify_one();
                let _ = responder
                    .write(format!("data: {}\n\n", tool_result(&id, "done")).as_bytes())
                    .await;
            }
        }
    }))
    .await;
    let (transport, client, mut events) = connect(http(&server)).await;
    let result = client.call_tool("echo", None).await.unwrap();
    assert_eq!(text(&result), "done");
    let mut seen = Vec::new();
    while seen.len() < 2 {
        match bounded("notifications", events.recv()).await.unwrap() {
            ClientEvent::Notification { params, .. } => {
                seen.push(params.unwrap()["data"].as_str().unwrap().to_owned());
            }
            other => panic!("{other:?}"),
        }
    }
    seen.sort();
    assert_eq!(seen, ["from get", "from post"]);
    close(transport, client).await;
    server.stop().await;
}

#[tokio::test]
async fn cancelling_a_running_request_frees_its_place_for_the_next() {
    // One request may run; the next waits behind it. Cancelling the first
    // must abort its POST, or the next never runs: the server holds the
    // first open until the client lets go.
    let (first_seen, first_waiting) = oneshot::channel();
    let first_seen = Arc::new(Mutex::new(Some(first_seen)));
    let (first_dropped, first_gone) = oneshot::channel();
    let first_dropped = Arc::new(Mutex::new(Some(first_dropped)));
    let server = Server::start(handler(move |request: Request, responder: Responder| {
        let first_seen = first_seen.clone();
        let first_dropped = first_dropped.clone();
        async move {
            let name = request.json()["params"]["name"].as_str().map(str::to_owned);
            if name.as_deref() == Some("stall") {
                let mut responder = responder;
                if let Some(seen) = first_seen.lock().unwrap().take() {
                    let _ = seen.send(());
                }
                responder.closed().await;
                if let Some(dropped) = first_dropped.lock().unwrap().take() {
                    let _ = dropped.send(());
                }
                return;
            }
            protocol(request, responder).await;
        }
    }))
    .await;
    let mut options = http(&server);
    options.open_server_stream = false;
    options.limits.max_in_flight_requests = std::num::NonZeroUsize::new(1).unwrap();
    let (transport, client, _events) = connect(options).await;
    let client = Arc::new(client);
    let stalled = tokio::spawn({
        let client = client.clone();
        async move { client.call_tool("stall", None).await }
    });
    bounded("the stalled POST", first_waiting).await.unwrap();
    let next: Vec<_> = (0..2)
        .map(|_| {
            let client = client.clone();
            tokio::spawn(async move { client.call_tool("echo", None).await })
        })
        .collect();
    // Dropping the call's future cancels it.
    stalled.abort();
    let _ = stalled.await;
    bounded("the stalled POST to be aborted", first_gone)
        .await
        .unwrap();
    for call in next {
        let result = bounded("a later call", call).await.unwrap().unwrap();
        assert_eq!(text(&result), "hello");
    }
    let cancelled = server
        .requests()
        .iter()
        .any(|request| request.rpc_method().as_deref() == Some("notifications/cancelled"));
    assert!(cancelled);
    close(transport, Arc::into_inner(client).unwrap()).await;
    server.stop().await;
}

#[tokio::test]
async fn shutdown_during_io_ends_every_exchange_and_late_messages_are_not_delivered() {
    let (streams_open, both_open) = tokio::sync::mpsc::channel(2);
    let (closed_tx, mut closed_rx) = tokio::sync::mpsc::channel(2);
    let server = Server::start(handler(move |request: Request, responder: Responder| {
        let streams_open = streams_open.clone();
        let closed_tx = closed_tx.clone();
        async move {
            let stream =
                request.method == "GET" || request.rpc_method().as_deref() == Some("tools/call");
            if !stream {
                return protocol(request, responder).await;
            }
            let mut responder = responder;
            let _ = responder
                .head(200, &[("content-type", "text/event-stream")])
                .await;
            let _ = streams_open.send(()).await;
            responder.closed().await;
            // Late: the transport has gone.
            let late = json!({"jsonrpc": "2.0", "method": "notifications/message"});
            let late_write = responder
                .write(format!("data: {late}\n\n").as_bytes())
                .await;
            let _ = closed_tx.send(late_write.is_ok()).await;
        }
    }))
    .await;
    let (transport, receiver, sender) = HttpTransport::start(http(&server)).unwrap();
    let mut connecting = McpClient::start(receiver, sender, client_options()).unwrap();
    let (client, mut events) = bounded("initialize", connecting.initialize())
        .await
        .unwrap();
    let client = Arc::new(client);
    let call = tokio::spawn({
        let client = client.clone();
        async move { client.call_tool("echo", None).await }
    });
    let mut both_open = both_open;
    bounded("the GET stream", both_open.recv()).await.unwrap();
    bounded("the call's stream", both_open.recv())
        .await
        .unwrap();
    // Owner shutdown first, while both streams are open.
    let closed = bounded("the transport's shutdown", transport.shutdown()).await;
    assert_eq!(closed, SessionClose::Deleted { status: 200 });
    for _ in 0..2 {
        bounded("a stream to close", closed_rx.recv())
            .await
            .unwrap();
    }
    let error = bounded("the call", call).await.unwrap().unwrap_err();
    assert_eq!(error, RequestError::Closed(CloseReason::EndOfStream));
    assert_eq!(
        bounded("the client to close", client.closed()).await,
        CloseReason::EndOfStream
    );
    assert!(events.try_recv().is_none());
    bounded("the client's shutdown", client.shutdown()).await;
    server.stop().await;
}

#[tokio::test]
async fn a_slow_consumer_holds_back_the_server() {
    // The receiver is driven by hand. The server streams far more than the
    // socket buffers hold; while nothing is received, its writes must stop.
    const EVENTS: usize = 512;
    let (finished_tx, mut finished) = tokio::sync::mpsc::channel(1);
    let server = Server::start(handler(move |_, responder: Responder| {
        let finished_tx = finished_tx.clone();
        async move {
            let mut responder = responder;
            let _ = responder
                .head(200, &[("content-type", "text/event-stream")])
                .await;
            let pad = "x".repeat(64 * 1024);
            for index in 0..EVENTS {
                let message =
                    json!({"jsonrpc": "2.0", "method": "n", "params": {"i": index, "pad": pad}});
                if responder
                    .write(format!("data: {message}\n\n").as_bytes())
                    .await
                    .is_err()
                {
                    return;
                }
            }
            let response = json!({"jsonrpc": "2.0", "id": 1, "result": {}});
            let _ = responder
                .write(format!("data: {response}\n\n").as_bytes())
                .await;
            let _ = finished_tx.send(()).await;
        }
    }))
    .await;
    let mut options = http(&server);
    options.limits.max_queued_messages = std::num::NonZeroUsize::new(1).unwrap();
    let (transport, mut receiver, mut sender) = HttpTransport::start(options).unwrap();
    sender
        .send(br#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#)
        .await
        .unwrap();
    // 32 MiB cannot fit in loopback socket buffers; if the transport read on
    // without a consumer, the server would finish here.
    let early = tokio::time::timeout(Duration::from_millis(500), finished.recv()).await;
    assert!(early.is_err(), "the server finished with no consumer");
    // Every message arrives, in order: none was dropped to keep reading.
    for index in 0..=EVENTS {
        let message = bounded("a message", receiver.receive())
            .await
            .unwrap()
            .unwrap();
        let message: serde_json::Value = serde_json::from_slice(&message).unwrap();
        if index < EVENTS {
            assert_eq!(message["params"]["i"], index, "a message was lost");
        } else {
            assert_eq!(message["id"], 1);
        }
    }
    bounded("the server to finish", finished.recv())
        .await
        .unwrap();
    drop(sender);
    bounded("shutdown", transport.shutdown()).await;
    server.stop().await;
}

#[tokio::test]
async fn a_dropped_receive_loses_no_part_of_an_event() {
    let (half_written, half) = oneshot::channel();
    let half_written = Arc::new(Mutex::new(Some(half_written)));
    let rest = Arc::new(Notify::new());
    let server = Server::start(handler({
        let rest = rest.clone();
        move |_, responder: Responder| {
            let half_written = half_written.lock().unwrap().take();
            let rest = rest.clone();
            async move {
                let mut responder = responder;
                let _ = responder
                    .head(200, &[("content-type", "text/event-stream")])
                    .await;
                let _ = responder.write(b"data: {\"jsonrpc\":\"2.0\",").await;
                if let Some(half_written) = half_written {
                    let _ = half_written.send(());
                }
                rest.notified().await;
                let _ = responder
                    .write(b"\"id\":1,\"result\":{\"ok\":\"\xC3\xA9\"}}\n\n")
                    .await;
            }
        }
    }))
    .await;
    let (transport, mut receiver, mut sender) = HttpTransport::start(http(&server)).unwrap();
    sender
        .send(br#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#)
        .await
        .unwrap();
    bounded("half an event", half).await.unwrap();
    // A receive that is dropped while the event is half read.
    let early = tokio::time::timeout(Duration::from_millis(50), receiver.receive()).await;
    assert!(early.is_err());
    rest.notify_one();
    let message = bounded("the event", receiver.receive())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        message,
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":\"é\"}}".as_bytes()
    );
    drop(sender);
    bounded("shutdown", transport.shutdown()).await;
    server.stop().await;
}

#[tokio::test]
async fn delete_outcomes_are_reported_and_bounded() {
    for (status, expected) in [
        (405, SessionClose::NotAllowed),
        (500, SessionClose::Rejected { status: 500 }),
    ] {
        let server = Server::start(handler(
            move |request: Request, responder: Responder| async move {
                if request.method == "DELETE" {
                    return responder.status(status).await;
                }
                protocol(request, responder).await;
            },
        ))
        .await;
        let (transport, client, _events) = connect(http(&server)).await;
        assert_eq!(close(transport, client).await, expected);
        server.stop().await;
    }
    // A DELETE the server never answers ends at its deadline.
    let server = Server::start(handler(
        |request: Request, mut responder: Responder| async move {
            if request.method == "DELETE" {
                return responder.closed().await;
            }
            protocol(request, responder).await;
        },
    ))
    .await;
    let mut options = http(&server);
    options.delete_timeout = Duration::from_millis(200);
    let (transport, client, _events) = connect(options).await;
    assert_eq!(
        close(transport, client).await,
        SessionClose::Failed(HttpError::Timeout)
    );
    server.stop().await;
}

#[tokio::test]
async fn the_halves_refuse_work_after_shutdown() {
    let server = Server::start(handler(protocol)).await;
    let (transport, mut receiver, mut sender) = HttpTransport::start(http(&server)).unwrap();
    assert_eq!(
        bounded("shutdown", transport.shutdown()).await,
        SessionClose::NoSession
    );
    let error = sender.send(b"{}").await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
    assert!(
        bounded("receive", receiver.receive())
            .await
            .unwrap()
            .is_none()
    );
    // Close after shutdown returns at once.
    bounded("close", sender.close()).await.unwrap();
    assert_eq!(server.connections(), 0);
    server.stop().await;
}

#[tokio::test]
async fn a_cancellation_gets_through_when_requests_are_saturated() {
    // Driven by hand, so the saturation is exact: with one request allowed
    // to run and one to wait, request 1 runs (the server holds it), 2 waits,
    // and 3 sits in the admission channel, so a fourth request's send cannot
    // complete. Cancellations must still be taken, through the separate
    // notification channel: 3's while 3 is still in the channel, so 3 is
    // never sent, and 1's, which aborts 1's POST so that 2 runs.
    let (first_dropped, first_gone) = oneshot::channel();
    let first_dropped = Arc::new(Mutex::new(Some(first_dropped)));
    let (first_seen, first_waiting) = oneshot::channel();
    let first_seen = Arc::new(Mutex::new(Some(first_seen)));
    let server = Server::start(handler(move |request: Request, responder: Responder| {
        let first_seen = first_seen.clone();
        let first_dropped = first_dropped.clone();
        async move {
            let id = request.json()["id"].clone();
            if id == json!(1) {
                let mut responder = responder;
                if let Some(seen) = first_seen.lock().unwrap().take() {
                    let _ = seen.send(());
                }
                responder.closed().await;
                if let Some(dropped) = first_dropped.lock().unwrap().take() {
                    let _ = dropped.send(());
                }
                return;
            }
            if id.is_null() {
                return responder.status(202).await;
            }
            responder
                .json(&[], &json!({"jsonrpc": "2.0", "id": id, "result": {}}))
                .await;
        }
    }))
    .await;
    let mut options = http(&server);
    options.limits.max_in_flight_requests = std::num::NonZeroUsize::new(1).unwrap();
    let (transport, mut receiver, mut sender) = HttpTransport::start(options).unwrap();
    let request = |id: u64| format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call"}}"#);
    sender.send(request(1).as_bytes()).await.unwrap();
    bounded("request 1 to reach the server", first_waiting)
        .await
        .unwrap();
    sender.send(request(2).as_bytes()).await.unwrap();
    sender.send(request(3).as_bytes()).await.unwrap();
    let fourth = tokio::time::timeout(
        Duration::from_millis(200),
        sender.send(request(4).as_bytes()),
    )
    .await;
    assert!(fourth.is_err(), "requests were not saturated");
    for id in [3, 1] {
        let cancel = format!(
            r#"{{"jsonrpc":"2.0","method":"notifications/cancelled","params":{{"requestId":{id}}}}}"#
        );
        bounded("a cancellation to be taken", sender.send(cancel.as_bytes()))
            .await
            .unwrap();
    }
    bounded("request 1's POST to be aborted", first_gone)
        .await
        .unwrap();
    let mut answered = Vec::new();
    while answered.is_empty() {
        let message = bounded("an answer", receiver.receive())
            .await
            .unwrap()
            .unwrap();
        let message: serde_json::Value = serde_json::from_slice(&message).unwrap();
        answered.push(message["id"].as_u64().unwrap());
    }
    answered.sort_unstable();
    assert_eq!(answered, [2]);
    drop(sender);
    bounded("shutdown", transport.shutdown()).await;
    let posted: Vec<_> = server
        .requests()
        .iter()
        .map(|request| request.json()["id"].clone())
        .collect();
    assert!(!posted.contains(&json!(3)), "a cancelled request was sent");
    assert!(
        !posted.contains(&json!(4)),
        "the abandoned send was delivered"
    );
    server.stop().await;
}

#[tokio::test]
async fn the_fixture_reports_handler_panics() {
    // A failed assertion inside a handler ends only that connection's task;
    // the fixture must still fail the test, here observed through its report.
    let server = Server::start(handler(|_, _responder: Responder| async move {
        panic!("handler assertion");
    }))
    .await;
    let response = bake_mcp::http::client_builder()
        .build()
        .unwrap()
        .get(&server.url)
        .send()
        .await;
    assert!(response.is_err(), "the panicking handler answered");
    let report = server.stop_within(fixture::WAIT).await;
    assert!(!report.timed_out);
    assert_eq!(report.panics, ["handler assertion"]);
}

/// Blocks the thread that drops it until the test opens `gate`, then
/// reports through `done`: a connection task that will not finish its abort.
struct SlowDrop {
    gate: std::sync::mpsc::Receiver<()>,
    done: Option<oneshot::Sender<()>>,
}

impl Drop for SlowDrop {
    fn drop(&mut self) {
        let _ = self.gate.recv_timeout(fixture::WAIT);
        if let Some(done) = self.done.take() {
            let _ = done.send(());
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_timed_out_fixture_stop_still_aborts_and_awaits_its_server_task() {
    // The connection task's abort blocks its worker in `SlowDrop`, so the
    // server task cannot finish draining within the deadline. The stop must
    // then abort and await the server task rather than detach it, and a
    // teardown abort must not be reported as a panic.
    let (open_gate, gate) = std::sync::mpsc::channel();
    let (done, dropped) = oneshot::channel();
    let guard = Arc::new(Mutex::new(Some(SlowDrop {
        gate,
        done: Some(done),
    })));
    let (entered, handler_entered) = oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered)));
    let server = Server::start(handler(move |_, mut responder: Responder| {
        let guard = guard.lock().unwrap().take();
        let entered = entered.lock().unwrap().take();
        async move {
            let _guard = guard;
            if let Some(entered) = entered {
                let _ = entered.send(());
            }
            responder.closed().await;
        }
    }))
    .await;
    let client = tokio::spawn({
        let url = server.url.clone();
        async move {
            bake_mcp::http::client_builder()
                .build()
                .unwrap()
                .get(url)
                .send()
                .await
        }
    });
    bounded("the handler to start", handler_entered)
        .await
        .unwrap();
    let address = server.origin.trim_start_matches("http://").to_owned();
    let report = server.stop_within(Duration::from_millis(200)).await;
    assert!(report.timed_out);
    assert!(report.panics.is_empty(), "{:?}", report.panics);
    // The server task has ended and dropped its listener: a detached task
    // would still hold the port while it waits for the connection task.
    assert!(
        tokio::net::TcpStream::connect(&address).await.is_err(),
        "the server task outlived its stop"
    );
    // The stop returned with the connection task still in its abort; it
    // ends once released.
    open_gate.send(()).unwrap();
    bounded("the connection task to end", dropped)
        .await
        .unwrap();
    let _ = bounded("the client request", client).await;
}
