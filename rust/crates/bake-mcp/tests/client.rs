//! The client against a scripted server over in-memory pipes.
//!
//! Tests named after a Pi case port it from Pi v1.3.0's
//! `packages/mcp/test/client.test.ts` or `test/stdio.test.ts`; the others are
//! Bake's, for the bounds, cancellation, and ownership this crate adds.
//! Tests that prove something did not happen run on a paused clock and wait
//! with [`settle`], which returns only once every other task is idle.

mod common;

use std::io;
use std::pin::pin;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use bake_mcp::{
    ClientEvent, CloseReason, ConnectError, Diagnostic, JsonRpcError, McpClient, ProtocolVersion,
    RequestError, ToolsCapability,
};
use common::{Server, connect, connect_wire, init_result, options, wire, wire_sized};
use serde_json::{Map, Value, json};
use tokio::io::AsyncWriteExt;
use tokio::time::Instant;

/// `tokio::join!` bounded by [`common::WAIT`]: a client that never answers
/// fails the test with a named wait instead of hanging it.
macro_rules! exchange {
    ($($future:expr),+ $(,)?) => {
        common::bounded("a client and server exchange", async { tokio::join!($($future),+) }).await
    };
}

/// Under a paused clock, a sleep ends only when no task can run, so this
/// returns once the client's task has done everything it can.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(1)).await;
}

/// Poll `future` once, so it reaches its first wait.
async fn poll_once<F: Future + Unpin>(future: &mut F) {
    tokio::select! {
        biased;
        _ = future => panic!("finished on its first poll"),
        () = std::future::ready(()) => {}
    }
}

fn args(value: Value) -> Option<Map<String, Value>> {
    value.as_object().cloned()
}

fn text_result(text: &str) -> Value {
    json!({"content": [{"type": "text", "text": text}]})
}

/// Answer one ping, so every message the server sent before has been read.
async fn sync(client: &McpClient, server: &mut Server) {
    let ((), ()) = exchange!(async { client.ping().await.expect("ping") }, async {
        let (id, _) = server.request("ping").await;
        server.reply(&id, json!({})).await;
    });
}

#[tokio::test]
async fn initializes_the_connection_before_exposing_server_information() {
    // Pi: "initializes the connection before exposing server information".
    let w = wire(64 * 1024, 1 << 20);
    let common::Wire {
        receiver,
        sender,
        mut server,
        mut receiver_alive,
        mut sender_alive,
    } = w;
    let (connected, initialize) = exchange!(
        McpClient::connect(receiver, sender, options()),
        server.handshake(init_result())
    );
    let (client, _events) = connected.unwrap();
    assert_eq!(
        initialize,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "test-client", "version": "2.0.0"},
            },
        })
    );
    let info = client.server();
    assert_eq!(info.protocol_version, ProtocolVersion::LATEST);
    assert_eq!(info.server_info.name, "test-server");
    assert_eq!(info.server_info.version, "1.0.0");
    assert_eq!(
        info.capabilities.tools(),
        Some(ToolsCapability { list_changed: true })
    );
    assert_eq!(
        info.capabilities.raw(),
        json!({"tools": {"listChanged": true}}).as_object().unwrap()
    );
    assert_eq!(info.instructions.as_deref(), Some("Use test tools."));
    assert!(receiver_alive.held() && sender_alive.held());
    assert_eq!(client.close_reason(), None);

    assert_eq!(
        common::bounded("shutdown", client.shutdown()).await,
        CloseReason::Shutdown
    );
    assert!(!receiver_alive.held() && !sender_alive.held());
    server.expect_eof().await;
}

#[tokio::test]
async fn paginates_tools_and_preserves_protocol_tool_definitions() {
    // Pi: "paginates tools and preserves protocol tool definitions".
    let (client, _events, mut server) = connect().await;
    let (tools, ()) = exchange!(client.list_tools(), async {
        let (id, params) = server.request("tools/list").await;
        assert_eq!(params, Value::Null);
        server
            .reply(
                &id,
                json!({
                    "tools": [{"name": "search", "description": "Search", "inputSchema": {"type": "object"}}],
                    "nextCursor": "page-2",
                }),
            )
            .await;
        let (id, params) = server.request("tools/list").await;
        assert_eq!(params, json!({"cursor": "page-2"}));
        // Some servers end pagination with an empty cursor instead of omitting it.
        server
            .reply(
                &id,
                json!({
                    "tools": [{
                        "name": "read",
                        "inputSchema": {"type": "object"},
                        "outputSchema": {"type": "object"},
                        "annotations": {"readOnlyHint": true},
                    }],
                    "nextCursor": "",
                }),
            )
            .await;
    });
    let tools = tools.unwrap();
    assert_eq!(
        tools.iter().map(|tool| tool.name()).collect::<Vec<_>>(),
        ["search", "read"]
    );
    assert_eq!(tools[0].description(), Some("Search"));
    assert_eq!(
        tools[0].input_schema(),
        json!({"type": "object"}).as_object().unwrap()
    );
    assert_eq!(
        Value::Object(tools[1].definition().clone()),
        json!({"name": "read", "outputSchema": {"type": "object"}, "annotations": {"readOnlyHint": true}})
    );
    common::bounded("shutdown", client.shutdown()).await;
}

#[tokio::test]
async fn returns_structured_tool_content_and_surfaces_json_rpc_errors() {
    // Pi: "returns structured tool content and surfaces JSON-RPC errors".
    let (client, _events, mut server) = connect().await;
    let (result, ()) = exchange!(
        client.call_tool("count", args(json!({"count": 3}))),
        async {
            let (id, params) = server.request("tools/call").await;
            assert_eq!(params, json!({"name": "count", "arguments": {"count": 3}}));
            server
            .reply(&id, json!({"content": [{"type": "text", "text": "ok"}], "structuredContent": {"count": 3}}))
            .await;
        }
    );
    let result = result.unwrap();
    assert_eq!(result.content, [json!({"type": "text", "text": "ok"})]);
    assert_eq!(result.structured_content, Some(json!({"count": 3})));
    assert!(!result.is_error);

    let (failed, ()) = exchange!(client.call_tool("fail", None), async {
        let (id, params) = server.request("tools/call").await;
        assert_eq!(params, json!({"name": "fail"}));
        server
            .reply_error(
                &id,
                json!({"code": 1234, "message": "tool failed", "data": {"retryable": false}}),
            )
            .await;
    });
    let error = failed.unwrap_err();
    assert_eq!(
        error,
        RequestError::Server {
            method: "tools/call",
            error: JsonRpcError {
                code: 1234,
                message: "tool failed".into(),
                data: Some(json!({"retryable": false})),
            },
        }
    );
    assert_eq!(
        error.to_string(),
        "the MCP server returned an error for tools/call: tool failed (code 1234); data: {\"retryable\":false}"
    );
    common::bounded("shutdown", client.shutdown()).await;
}

#[tokio::test]
async fn a_cancelled_request_notifies_the_server_and_ignores_its_late_response() {
    // Pi: "cancels aborted and timed-out requests" (the abort half). Dropping
    // the future is the Rust form of aborting its signal.
    let (client, mut events, mut server) = connect().await;
    let mut call = Box::pin(client.call_tool("wait", args(json!({}))));
    let (id, _) = tokio::select! {
        _ = &mut call => panic!("answered without a response"),
        request = server.request("tools/call") => request,
    };
    drop(call);
    assert_eq!(
        server.recv().await,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": 2, "reason": "Request cancelled"},
        })
    );
    // The specification asks the client to ignore a response that crosses
    // the cancellation, so it is not reported.
    server.reply(&id, text_result("late")).await;
    sync(&client, &mut server).await;
    assert_eq!(events.try_recv(), None);
    common::bounded("shutdown", client.shutdown()).await;
}

#[tokio::test(start_paused = true)]
async fn a_timed_out_request_is_cancelled() {
    // Pi: "cancels aborted and timed-out requests" (the timeout half).
    let mut options = options();
    options.request_timeout = Some(Duration::from_millis(5));
    let (client, _events, mut server, _, _) =
        connect_wire(wire(64 * 1024, 1 << 20), options, init_result()).await;
    let (result, ()) = exchange!(client.call_tool("wait", args(json!({}))), async {
        server.request("tools/call").await;
    });
    assert_eq!(
        result.unwrap_err(),
        RequestError::Timeout {
            method: "tools/call",
            after: Duration::from_millis(5),
        }
    );
    assert_eq!(
        server.recv().await,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": 2, "reason": "Request timed out"},
        })
    );
    common::bounded("shutdown", client.shutdown()).await;
}

#[tokio::test(start_paused = true)]
async fn does_not_send_notifications_cancelled_for_a_timed_out_initialize() {
    // Pi: "does not send notifications/cancelled for a timed-out initialize".
    let common::Wire {
        receiver,
        sender,
        mut server,
        mut receiver_alive,
        mut sender_alive,
    } = wire(64 * 1024, 1 << 20);
    let mut options = options();
    options.request_timeout = Some(Duration::from_millis(5));
    let (connected, _) = exchange!(
        McpClient::connect(receiver, sender, options),
        server.request("initialize")
    );
    assert_eq!(
        connected.unwrap_err(),
        ConnectError::Initialize(RequestError::Timeout {
            method: "initialize",
            after: Duration::from_millis(5),
        })
    );
    // The failed connection has already released both halves.
    assert!(!receiver_alive.held() && !sender_alive.held());
    server.expect_eof().await;
}

#[tokio::test]
async fn reports_stray_output_without_failing_pending_requests() {
    // Pi: "reports transport errors without failing pending requests"; a
    // stdio server's stray output takes the place of an injected error.
    let (client, mut events, mut server) = connect().await;
    let (result, ()) = exchange!(client.call_tool("wait", None), async {
        let (id, _) = server.request("tools/call").await;
        server.send_raw(b"stray log line\n").await;
        server.send_raw(b"\xff\xfe\n").await;
        server.send_raw(b"   \n\r\n").await;
        server
            .send_raw(br#"{"jsonrpc":"1.0","id":2,"result":{}}"#)
            .await;
        server
            .send_raw(b"\n[{\"jsonrpc\":\"2.0\",\"method\":\"x\"}]\n")
            .await;
        server
            .send_raw(
                br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}"#,
            )
            .await;
        server.send_raw(b"\n").await;
        server.reply(&id, json!({"content": []})).await;
    });
    assert_eq!(result.unwrap().content, Vec::<Value>::new());
    let mut details = Vec::new();
    while let Some(event) = events.try_recv() {
        match event {
            ClientEvent::Diagnostic(Diagnostic::MalformedMessage { detail }) => {
                details.push(detail)
            }
            ClientEvent::Diagnostic(Diagnostic::ErrorWithoutId(error)) => {
                assert_eq!(error.code, -32700);
                details.push("error without id".into());
            }
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert_eq!(details.len(), 5, "{details:?}");
    assert!(details[0].starts_with("not JSON"), "{details:?}");
    assert!(details[1].starts_with("not JSON"), "{details:?}");
    assert_eq!(details[2], "\"jsonrpc\" is not \"2.0\"");
    assert_eq!(details[3], "a JSON-RPC batch, which MCP does not use");
    assert_eq!(details[4], "error without id");
    // No diagnostic repeats the server's text.
    assert!(details.iter().all(|detail| !detail.contains("stray")));
    common::bounded("shutdown", client.shutdown()).await;
}

#[tokio::test]
async fn accepts_servers_that_answer_with_an_older_protocol_version() {
    // Pi: "accepts servers that answer with an older protocol version".
    let (client, _events, _server, _, _) = connect_wire(
        wire(64 * 1024, 1 << 20),
        options(),
        json!({"protocolVersion": "2024-11-05", "capabilities": {}, "serverInfo": {"name": "old-server", "version": "0.1.0"}}),
    )
    .await;
    assert_eq!(
        client.server().protocol_version,
        ProtocolVersion::V2024_11_05
    );
    assert_eq!(client.server().capabilities.tools(), None);
    common::bounded("shutdown", client.shutdown()).await;

    let common::Wire {
        receiver,
        sender,
        mut server,
        mut receiver_alive,
        mut sender_alive,
    } = wire(64 * 1024, 1 << 20);
    let (connected, ()) = exchange!(McpClient::connect(receiver, sender, options()), async {
        let (id, _) = server.request("initialize").await;
        server
            .reply(
                &id,
                json!({"protocolVersion": "1999-01-01", "capabilities": {}, "serverInfo": {"name": "ancient-server", "version": "0.1.0"}}),
            )
            .await;
    });
    let error = connected.unwrap_err();
    assert_eq!(
        error,
        ConnectError::UnsupportedProtocolVersion {
            selected: "1999-01-01".into()
        }
    );
    assert!(
        error
            .to_string()
            .contains("unsupported protocol version \"1999-01-01\"")
    );
    assert!(!receiver_alive.held() && !sender_alive.held());
    // No `notifications/initialized` follows a refused result.
    server.expect_eof().await;
}

#[tokio::test]
async fn reports_a_server_that_refuses_initialize() {
    // A server that speaks only MCP 2026-07-28 rejects `initialize` and
    // should name its versions (versioning, "Backward Compatibility").
    let common::Wire {
        receiver,
        sender,
        mut server,
        ..
    } = wire(64 * 1024, 1 << 20);
    let (connected, ()) = exchange!(McpClient::connect(receiver, sender, options()), async {
        let (id, _) = server.request("initialize").await;
        server
            .reply_error(
                &id,
                json!({"code": -32601, "message": "Unsupported protocol version", "data": {"supported": ["2026-07-28"]}}),
            )
            .await;
    });
    let error = connected.unwrap_err();
    assert!(
        matches!(&error, ConnectError::Initialize(RequestError::Server { method: "initialize", error }) if error.code == -32601)
    );
    assert!(
        error.to_string().contains("\"supported\":[\"2026-07-28\"]"),
        "{error}"
    );
    server.expect_eof().await;

    let common::Wire {
        receiver,
        sender,
        mut server,
        ..
    } = wire(64 * 1024, 1 << 20);
    let (connected, ()) = exchange!(McpClient::connect(receiver, sender, options()), async {
        let (id, _) = server.request("initialize").await;
        server
            .reply(
                &id,
                json!({"protocolVersion": "2025-11-25", "capabilities": {}}),
            )
            .await;
    });
    assert_eq!(
        connected.unwrap_err(),
        ConnectError::Initialize(RequestError::InvalidResult {
            method: "initialize",
            detail: "\"serverInfo\" is not an object".into(),
        })
    );
    server.expect_eof().await;
}

#[tokio::test]
async fn defaults_missing_tool_result_content_to_an_empty_list() {
    // Pi: "defaults missing tool result content to an empty list".
    let (client, _events, mut server) = connect().await;
    let (result, ()) = exchange!(client.call_tool("structured", None), async {
        let (id, _) = server.request("tools/call").await;
        server
            .reply(&id, json!({"structuredContent": {"ok": true}}))
            .await;
    });
    let result = result.unwrap();
    assert!(result.content.is_empty());
    assert_eq!(result.structured_content, Some(json!({"ok": true})));
    let (result, ()) = exchange!(client.call_tool("broken", None), async {
        let (id, _) = server.request("tools/call").await;
        server.reply(&id, json!({"content": "not a list"})).await;
    });
    let error = result.unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid MCP tools/call result: \"content\" is not an array"
    );
    common::bounded("shutdown", client.shutdown()).await;
}

#[tokio::test]
async fn accepts_structured_content_that_is_not_an_object() {
    // Pi: "accepts structured content that is not an object" (#10416).
    let (client, _events, mut server) = connect().await;
    for structured in [
        json!([{"id": 1}, {"id": 2}]),
        Value::Null,
        json!("text"),
        json!(3),
        json!(false),
    ] {
        let (result, ()) = exchange!(client.call_tool("structured", None), async {
            let (id, _) = server.request("tools/call").await;
            server
                .reply(&id, json!({"content": [], "structuredContent": structured}))
                .await;
        });
        assert_eq!(result.unwrap().structured_content, Some(structured));
    }
    common::bounded("shutdown", client.shutdown()).await;
}

#[tokio::test]
async fn fails_pending_requests_once_when_the_transport_drops() {
    // Pi: "notifies close listeners once when the transport drops".
    let (client, _events, mut server) = connect().await;
    let (result, ()) = exchange!(client.call_tool("wait", None), async {
        server.request("tools/call").await;
        server.close_output();
    });
    assert_eq!(
        result.unwrap_err(),
        RequestError::Closed(CloseReason::EndOfStream)
    );
    assert_eq!(client.closed().await, CloseReason::EndOfStream);
    assert_eq!(client.close_reason(), Some(CloseReason::EndOfStream));
    assert_eq!(
        client.ping().await.unwrap_err(),
        RequestError::Closed(CloseReason::EndOfStream)
    );
    // Shutdown reports the first reason and still waits for the task.
    assert_eq!(
        common::bounded("shutdown", client.shutdown()).await,
        CloseReason::EndOfStream
    );
}

#[tokio::test]
async fn answers_ping_refuses_other_server_requests_and_dispatches_notifications() {
    // Pi: "answers roots/list and dispatches notifications". This client
    // declares no roots capability, so `roots/list` is refused as unknown.
    let (client, mut events, mut server) = connect().await;
    let mut changes = client.tool_list_changes();
    assert_eq!(changes.generation(), 0);
    server
        .send(json!({"jsonrpc": "2.0", "id": "p", "method": "ping"}))
        .await;
    assert_eq!(
        server.recv().await,
        json!({"jsonrpc": "2.0", "id": "p", "result": {}})
    );
    server
        .send(json!({"jsonrpc": "2.0", "id": 7, "method": "roots/list"}))
        .await;
    assert_eq!(
        server.recv().await,
        json!({"jsonrpc": "2.0", "id": 7, "error": {"code": -32601, "message": "Method not found: roots/list"}})
    );
    server
        .notify("notifications/tools/list_changed", None)
        .await;
    server
        .notify("notifications/tools/list_changed", None)
        .await;
    server
        .notify("notifications/cancelled", Some(json!({"requestId": "p"})))
        .await;
    server
        .notify(
            "notifications/message",
            Some(json!({"level": "info", "data": "hello"})),
        )
        .await;
    sync(&client, &mut server).await;
    // Two notifications, one wake-up.
    assert_eq!(changes.changed().await, Some(2));
    assert_eq!(changes.generation(), 2);
    assert_eq!(
        events.try_recv(),
        Some(ClientEvent::Notification {
            method: "notifications/message".into(),
            params: Some(json!({"level": "info", "data": "hello"})),
        })
    );
    assert_eq!(events.try_recv(), None);
    common::bounded("shutdown", client.shutdown()).await;
    assert_eq!(changes.changed().await, None);
    assert_eq!(events.recv().await, None);
}

#[tokio::test]
async fn reads_fragmented_combined_and_crlf_messages() {
    // Pi `test/stdio.test.ts`: "connects to a newline-delimited MCP server",
    // with the server's output split at every byte.
    let (client, _events, mut server) = connect().await;
    let (first, second, ()) = exchange!(
        client.call_tool("é", None),
        client.call_tool("😀", None),
        async {
            let (a, _) = server.request("tools/call").await;
            let (b, _) = server.request("tools/call").await;
            let response = format!(
                "{}\r\n",
                json!({"jsonrpc": "2.0", "id": a, "result": text_result("é😀")})
            );
            for byte in response.as_bytes() {
                server.send_raw(&[*byte]).await;
                tokio::task::yield_now().await;
            }
            // Two messages in one write.
            let both = format!(
                "{}\n{}\n",
                json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {}}),
                json!({"jsonrpc": "2.0", "id": b, "result": text_result("two")}),
            );
            server.send_raw(both.as_bytes()).await;
        }
    );
    assert_eq!(
        first.unwrap().content,
        text_result("é😀")["content"].as_array().unwrap().clone()
    );
    assert_eq!(
        second.unwrap().content,
        text_result("two")["content"].as_array().unwrap().clone()
    );
    common::bounded("shutdown", client.shutdown()).await;
}

#[tokio::test]
async fn a_message_over_the_limit_closes_the_connection() {
    const LIMIT: usize = 512;
    let mut options = options();
    options.limits.max_message_bytes = std::num::NonZeroUsize::new(LIMIT).unwrap();
    let (client, _events, mut server, mut receiver_alive, _) =
        connect_wire(wire(64 * 1024, LIMIT), options, init_result()).await;
    // A response of exactly the limit is read (the control).
    let (result, ()) = exchange!(client.call_tool("fits", None), async {
        let (id, _) = server.request("tools/call").await;
        let empty = json!({"jsonrpc": "2.0", "id": id, "result": text_result("")}).to_string();
        let padded = json!({"jsonrpc": "2.0", "id": id, "result": text_result(&"x".repeat(LIMIT - empty.len()))});
        assert_eq!(padded.to_string().len(), LIMIT);
        server.send(padded).await;
    });
    assert!(result.is_ok());
    // One byte more closes the connection before its newline arrives.
    let (result, ()) = exchange!(client.call_tool("too-large", None), async {
        let (id, _) = server.request("tools/call").await;
        let empty = json!({"jsonrpc": "2.0", "id": id, "result": text_result("")}).to_string();
        let text = "x".repeat(LIMIT + 1 - empty.len());
        let line = json!({"jsonrpc": "2.0", "id": id, "result": text_result(&text)}).to_string();
        assert_eq!(line.len(), LIMIT + 1);
        server.send_raw(line.as_bytes()).await;
    });
    assert_eq!(
        result.unwrap_err(),
        RequestError::Closed(CloseReason::MessageTooLarge { limit: LIMIT })
    );
    receiver_alive_released(&mut receiver_alive, &client).await;
}

async fn receiver_alive_released(alive: &mut common::Alive, client: &McpClient) {
    common::bounded("shutdown", client.shutdown()).await;
    assert!(!alive.held());
}

#[tokio::test]
async fn a_stream_that_ends_inside_a_message_is_reported() {
    let (client, _events, mut server) = connect().await;
    let (result, ()) = exchange!(client.call_tool("wait", None), async {
        server.request("tools/call").await;
        server.send_raw(br#"{"jsonrpc":"2.0","id":2,"res"#).await;
        server.close_output();
    });
    assert_eq!(
        result.unwrap_err(),
        RequestError::Closed(CloseReason::IncompleteMessage { bytes: 28 })
    );
    assert!(
        client
            .closed()
            .await
            .to_string()
            .contains("ended inside a message (28 bytes")
    );
    common::bounded("shutdown", client.shutdown()).await;
}

#[tokio::test]
async fn a_failed_write_closes_the_connection() {
    let (client, _events, mut server) = connect().await;
    server.close_input();
    let error = client.ping().await.unwrap_err();
    let RequestError::Closed(CloseReason::Send { kind, .. }) = error else {
        panic!("expected a send failure, got {error:?}");
    };
    assert_eq!(kind, io::ErrorKind::BrokenPipe);
    assert!(matches!(
        common::bounded("shutdown", client.shutdown()).await,
        CloseReason::Send { .. }
    ));
}

#[tokio::test]
async fn wrong_and_duplicate_ids_are_reported_and_not_matched() {
    let (client, mut events, mut server) = connect().await;
    let (result, ()) = exchange!(client.call_tool("wait", None), async {
        let (id, _) = server.request("tools/call").await;
        assert_eq!(id, json!(2));
        server.reply(&json!(999), text_result("wrong")).await;
        // The same digits as a string are a different id.
        server.reply(&json!("2"), text_result("string")).await;
        server.reply(&json!(2.5), text_result("fraction")).await;
        server.reply(&id, text_result("right")).await;
        server.reply(&id, text_result("duplicate")).await;
    });
    assert_eq!(
        result.unwrap().content,
        text_result("right")["content"].as_array().unwrap().clone()
    );
    sync(&client, &mut server).await;
    let mut ids = Vec::new();
    while let Some(event) = events.try_recv() {
        let ClientEvent::Diagnostic(Diagnostic::UnknownResponse { id }) = event else {
            panic!("unexpected event {event:?}");
        };
        ids.push(id);
    }
    assert_eq!(ids, ["999", "\"2\"", "2.5", "2"]);
    common::bounded("shutdown", client.shutdown()).await;
}

#[tokio::test]
async fn an_invalid_response_fails_its_request() {
    let (client, _events, mut server) = connect().await;
    let (result, ()) = exchange!(client.call_tool("wait", None), async {
        let (id, _) = server.request("tools/call").await;
        server
            .send(json!({"jsonrpc": "2.0", "id": id, "result": {}, "error": {"code": 1, "message": "m"}}))
            .await;
    });
    assert_eq!(
        result.unwrap_err(),
        RequestError::InvalidResponse {
            method: "tools/call",
            detail: "the response has both \"result\" and \"error\"".into(),
        }
    );
    common::bounded("shutdown", client.shutdown()).await;
}

/// Serve `tools/list` pages from `page(index, cursor)` until `pages` were sent.
async fn serve_pages(server: &mut Server, pages: usize, page: impl Fn(usize, &Value) -> Value) {
    for index in 0..pages {
        let (id, params) = server.request("tools/list").await;
        server.reply(&id, page(index, &params)).await;
    }
}

fn tool(name: &str) -> Value {
    json!({"name": name, "inputSchema": {"type": "object"}})
}

#[tokio::test]
async fn pagination_refuses_repeated_cursors() {
    let (client, _events, mut server) = connect().await;
    let (result, ()) = exchange!(
        client.list_tools(),
        serve_pages(&mut server, 3, |index, _| {
            json!({"tools": [tool(&format!("t{index}"))], "nextCursor": (["a", "b", "a"][index])})
        })
    );
    assert_eq!(
        result.unwrap_err(),
        RequestError::RepeatedCursor {
            method: "tools/list",
            cursor: "a".into(),
        }
    );
    common::bounded("shutdown", client.shutdown()).await;
}

#[tokio::test]
async fn pagination_stops_at_the_page_limit() {
    let mut options = options();
    options.limits.max_list_pages = std::num::NonZeroUsize::new(3).unwrap();
    let (client, _events, mut server, _, _) =
        connect_wire(wire(64 * 1024, 1 << 20), options, init_result()).await;
    // Control: a list of exactly three pages is read.
    let (result, ()) = exchange!(
        client.list_tools(),
        serve_pages(&mut server, 3, |index, _| {
            let next = if index < 2 {
                json!(format!("c{index}"))
            } else {
                Value::Null
            };
            json!({"tools": [tool(&format!("t{index}"))], "nextCursor": next})
        })
    );
    assert_eq!(result.unwrap().len(), 3);
    // A fourth cursor is refused without requesting its page.
    let (result, ()) = exchange!(
        client.list_tools(),
        serve_pages(
            &mut server,
            3,
            |index, _| json!({"tools": [], "nextCursor": format!("c{index}")})
        )
    );
    assert_eq!(
        result.unwrap_err(),
        RequestError::TooManyPages {
            method: "tools/list",
            limit: 3,
        }
    );
    sync(&client, &mut server).await;
    common::bounded("shutdown", client.shutdown()).await;
}

#[tokio::test]
async fn pagination_stops_at_the_tool_and_byte_limits() {
    let mut options = options();
    options.limits.max_listed_tools = std::num::NonZeroUsize::new(2).unwrap();
    options.limits.max_list_bytes = std::num::NonZeroUsize::new(400).unwrap();
    let (client, _events, mut server, _, _) =
        connect_wire(wire(64 * 1024, 1 << 20), options, init_result()).await;
    // Control: two tools fit.
    let (result, ()) = exchange!(
        client.list_tools(),
        serve_pages(
            &mut server,
            1,
            |_, _| json!({"tools": [tool("a"), tool("b")]})
        )
    );
    assert_eq!(result.unwrap().len(), 2);
    let (result, ()) = exchange!(
        client.list_tools(),
        serve_pages(&mut server, 2, |index, _| {
            let next = if index == 0 {
                json!("next")
            } else {
                Value::Null
            };
            json!({"tools": [tool(&format!("t{index}")), tool(&format!("u{index}"))], "nextCursor": next})
        })
    );
    assert_eq!(
        result.unwrap_err(),
        RequestError::TooManyItems {
            method: "tools/list",
            limit: 2,
        }
    );
    let (result, ()) = exchange!(
        client.list_tools(),
        serve_pages(
            &mut server,
            1,
            |_, _| json!({"tools": [{"name": "big", "inputSchema": {"description": "x".repeat(400)}}]})
        )
    );
    assert_eq!(
        result.unwrap_err(),
        RequestError::ListTooLarge {
            method: "tools/list",
            limit: 400,
        }
    );
    common::bounded("shutdown", client.shutdown()).await;
}

#[tokio::test]
async fn a_request_over_the_limit_is_refused_before_it_is_sent() {
    let mut options = options();
    options.limits.max_message_bytes = std::num::NonZeroUsize::new(256).unwrap();
    let (client, _events, mut server, _, _) =
        connect_wire(wire(64 * 1024, 256), options, init_result()).await;
    let error = client
        .call_tool("big", args(json!({"text": "x".repeat(256)})))
        .await
        .unwrap_err();
    let RequestError::MessageTooLarge {
        method,
        size,
        limit,
    } = error
    else {
        panic!("expected a refused request, got {error:?}");
    };
    assert_eq!((method, limit), ("tools/call", 256));
    assert!(size > 256);
    // Nothing was written: the next message the server reads is the ping.
    sync(&client, &mut server).await;
    common::bounded("shutdown", client.shutdown()).await;
}

#[tokio::test(start_paused = true)]
async fn requests_withdrawn_before_or_while_being_written() {
    // The client-to-server pipe holds 64 bytes and the server is not reading,
    // so the first request's write stays in flight.
    let (client, _events, mut server, _, _) =
        connect_wire(wire(64, 1 << 20), options(), init_result()).await;
    let mut first = Box::pin(client.call_tool("first", args(json!({"text": "x".repeat(4096)}))));
    let mut second = Box::pin(client.call_tool("second", None));
    poll_once(&mut first).await;
    poll_once(&mut second).await;
    settle().await;
    // The first is being written; the second waits in the queue.
    drop(second);
    drop(first);
    settle().await;
    let (id, params) = server.request("tools/call").await;
    assert_eq!((id, &params["name"]), (json!(2), &json!("first")));
    // The written request is cancelled; the queued one is withdrawn unsent.
    assert_eq!(
        server.recv().await["params"],
        json!({"requestId": 2, "reason": "Request cancelled"})
    );
    let ((), message) = exchange!(async { client.ping().await.unwrap() }, async {
        let message = server.recv().await;
        server.reply(&message["id"], json!({})).await;
        message
    });
    assert_eq!(
        message,
        json!({"jsonrpc": "2.0", "id": 4, "method": "ping"})
    );
    common::bounded("shutdown", client.shutdown()).await;
}

#[tokio::test(start_paused = true)]
async fn a_request_dropped_before_the_task_takes_it_is_never_sent() {
    // On this single-threaded runtime the client's task cannot run between
    // the first poll, which puts the request in the task's channel, and the
    // drop, so the request and its cancellation reach the task together.
    let (client, _events, mut server) = connect().await;
    let mut call = Box::pin(client.call_tool("abandoned", None));
    poll_once(&mut call).await;
    drop(call);
    settle().await;
    let ((), ()) = exchange!(async { client.ping().await.unwrap() }, async {
        // Checked as it arrives, so a client that sent the abandoned request
        // fails here rather than waiting for a ping answer that never comes.
        let message = server.recv().await;
        assert_eq!(
            message,
            json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}),
            "the abandoned request was written"
        );
        server.reply(&message["id"], json!({})).await;
    });
    common::bounded("shutdown", client.shutdown()).await;
}

#[tokio::test(start_paused = true)]
async fn a_request_abandoned_while_the_queue_is_full_is_never_sent() {
    // With the queue at its threshold the task takes no request, so an
    // abandoned one waits in the task's channel while its cancellation, for
    // an id the task has not seen, is handled first. When room returns, the
    // task must find the request abandoned instead of writing it.
    let mut options = options();
    options.limits.max_queued_messages = std::num::NonZeroUsize::new(1).unwrap();
    let (client, _events, mut server, _, _) =
        connect_wire(wire(64, 1 << 20), options, init_result()).await;
    // `first` is being written into the full 64-byte pipe; `second` waits in
    // the queue, which is now at its threshold.
    let mut first = Box::pin(client.call_tool("first", args(json!({"text": "x".repeat(4096)}))));
    poll_once(&mut first).await;
    settle().await;
    let mut second = Box::pin(client.call_tool("second", None));
    poll_once(&mut second).await;
    settle().await;
    let mut abandoned = Box::pin(client.call_tool("abandoned", None));
    poll_once(&mut abandoned).await;
    drop(abandoned);
    settle().await;
    let ((), ()) = exchange!(async { client.ping().await.unwrap() }, async {
        assert_eq!(server.request("tools/call").await.1["name"], "first");
        assert_eq!(server.request("tools/call").await.1["name"], "second");
        let message = server.recv().await;
        assert_eq!(
            message,
            json!({"jsonrpc": "2.0", "id": 5, "method": "ping"}),
            "the abandoned request was written"
        );
        server.reply(&message["id"], json!({})).await;
    });
    common::bounded("shutdown", client.shutdown()).await;
    assert!(first.await.is_err() && second.await.is_err());
}

#[tokio::test(start_paused = true)]
async fn requests_beyond_the_pending_limit_wait_for_a_permit() {
    let mut options = options();
    options.limits.max_pending_requests = std::num::NonZeroUsize::new(2).unwrap();
    let (client, _events, mut server, _, _) =
        connect_wire(wire(64 * 1024, 1 << 20), options, init_result()).await;
    let name = |result: Result<bake_mcp::CallToolResult, RequestError>| {
        result.unwrap().content[0]["text"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let (a, b, c, ()) = exchange!(
        client.call_tool("a", None),
        client.call_tool("b", None),
        client.call_tool("c", None),
        async {
            let first = server.request("tools/call").await;
            let second = server.request("tools/call").await;
            // The third waits for a permit and is not written.
            settle().await;
            assert!(
                tokio::time::timeout(Duration::from_millis(1), server.next())
                    .await
                    .is_err()
            );
            // Answers in reverse order still reach their own requests.
            let echo = |params: &Value| text_result(params["name"].as_str().unwrap());
            server.reply(&second.0, echo(&second.1)).await;
            let third = server.request("tools/call").await;
            server.reply(&third.0, echo(&third.1)).await;
            server.reply(&first.0, echo(&first.1)).await;
        }
    );
    assert_eq!([name(a), name(b), name(c)], ["a", "b", "c"]);
    common::bounded("shutdown", client.shutdown()).await;
}

#[tokio::test(start_paused = true)]
async fn a_server_that_does_not_read_its_answers_is_slowed_not_buffered() {
    // Both pipes hold 256 bytes and at most four answers may wait, so a
    // server flooding pings without reading the answers fills its own pipe.
    let mut options = options();
    options.limits.max_queued_messages = std::num::NonZeroUsize::new(4).unwrap();
    let (client, _events, mut server, _, _) =
        connect_wire(wire_sized(256, 256, 1 << 20), options, init_result()).await;
    let flood: String = (0..200)
        .map(|index| {
            format!(
                "{}\n",
                json!({"jsonrpc": "2.0", "id": index, "method": "ping"})
            )
        })
        .collect();
    let mut output = server.take_output();
    let mut writing = Box::pin(async move {
        output.write_all(flood.as_bytes()).await.unwrap();
        output
    });
    settle().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(1), &mut writing)
            .await
            .is_err(),
        "the client kept reading while its answers could not be written"
    );
    // Control: once the server reads, the flood completes and every ping is
    // answered in order.
    let (_output, ()) = exchange!(writing, async {
        for index in 0..200 {
            assert_eq!(
                server.recv().await,
                json!({"jsonrpc": "2.0", "id": index, "result": {}})
            );
        }
    });
    common::bounded("shutdown", client.shutdown()).await;
}

#[tokio::test]
async fn shutdown_fails_pending_requests_flushes_answers_and_releases_the_transport() {
    // The client-to-server pipe holds 64 bytes, so writes wait for the server.
    let (client, mut events, mut server, mut receiver_alive, mut sender_alive) =
        connect_wire(wire(64, 1 << 20), options(), init_result()).await;
    let mut written =
        Box::pin(client.call_tool("written", args(json!({"text": "x".repeat(1024)}))));
    poll_once(&mut written).await;
    // A server request whose answer queues behind the request being written;
    // the notification after it proves the client has read both.
    server
        .send(json!({"jsonrpc": "2.0", "id": "p", "method": "ping"}))
        .await;
    server.notify("notifications/message", None).await;
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Notification { .. })
    ));
    let mut queued = Box::pin(client.call_tool("queued", None));
    poll_once(&mut queued).await;
    // Control: the task holds both halves until shutdown.
    assert!(receiver_alive.held() && sender_alive.held());

    let (first, second, ()) = exchange!(client.shutdown(), client.shutdown(), async {
        let (_, params) = server.request("tools/call").await;
        assert_eq!(params["name"], "written");
        // The answer is flushed; the queued request nobody awaits is not.
        assert_eq!(
            server.recv().await,
            json!({"jsonrpc": "2.0", "id": "p", "result": {}})
        );
        server.expect_eof().await;
    });
    assert_eq!(
        (first, second),
        (CloseReason::Shutdown, CloseReason::Shutdown)
    );
    assert!(!receiver_alive.held() && !sender_alive.held());
    assert_eq!(
        written.await.unwrap_err(),
        RequestError::Closed(CloseReason::Shutdown)
    );
    assert_eq!(
        queued.await.unwrap_err(),
        RequestError::Closed(CloseReason::Shutdown)
    );
    assert_eq!(
        client.ping().await.unwrap_err(),
        RequestError::Closed(CloseReason::Shutdown)
    );
    assert_eq!(events.recv().await, None);
}

#[tokio::test(start_paused = true)]
async fn shutdown_gives_up_on_a_server_that_never_reads() {
    let mut options = options();
    options.shutdown_timeout = Duration::from_secs(2);
    let (client, _events, _server, mut receiver_alive, mut sender_alive) =
        connect_wire(wire(64, 1 << 20), options, init_result()).await;
    let mut stuck = Box::pin(client.call_tool("stuck", args(json!({"text": "x".repeat(1024)}))));
    poll_once(&mut stuck).await;
    settle().await;
    let started = Instant::now();
    assert_eq!(
        common::bounded("shutdown", client.shutdown()).await,
        CloseReason::Shutdown
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(2) && elapsed < Duration::from_millis(2_100),
        "{elapsed:?}"
    );
    assert!(!receiver_alive.held() && !sender_alive.held());
    assert_eq!(
        stuck.await.unwrap_err(),
        RequestError::Closed(CloseReason::Shutdown)
    );
}

#[tokio::test]
async fn dropping_the_client_stops_its_task() {
    let (client, _events, mut server, receiver_alive, sender_alive) =
        connect_wire(wire(64 * 1024, 1 << 20), options(), init_result()).await;
    drop(client);
    server.expect_eof().await;
    receiver_alive.released().await;
    sender_alive.released().await;
}

#[tokio::test]
async fn dropping_connect_during_initialize_stops_its_task() {
    let common::Wire {
        receiver,
        sender,
        mut server,
        receiver_alive,
        sender_alive,
    } = wire(64 * 1024, 1 << 20);
    let mut connecting = Box::pin(McpClient::connect(receiver, sender, options()));
    tokio::select! {
        _ = &mut connecting => panic!("connected without an answer"),
        _ = server.request("initialize") => {}
    }
    drop(connecting);
    // `initialize` is never cancelled; the client just closes its output.
    server.expect_eof().await;
    receiver_alive.released().await;
    sender_alive.released().await;
}

#[tokio::test]
async fn events_beyond_the_limit_are_dropped_and_counted() {
    let mut options = options();
    options.limits.max_events = std::num::NonZeroUsize::new(2).unwrap();
    let (client, mut events, mut server, _, _) =
        connect_wire(wire(64 * 1024, 1 << 20), options, init_result()).await;
    for index in 0..5 {
        server
            .notify("notifications/message", Some(json!(index)))
            .await;
    }
    sync(&client, &mut server).await;
    assert_eq!(client.dropped_events(), 3);
    let params = |event: Option<ClientEvent>| match event {
        Some(ClientEvent::Notification { params, .. }) => params,
        other => panic!("unexpected event {other:?}"),
    };
    assert_eq!(params(events.try_recv()), Some(json!(0)));
    assert_eq!(params(events.try_recv()), Some(json!(1)));
    assert_eq!(events.try_recv(), None);
    // Once read, the queue takes events again.
    server.notify("notifications/message", Some(json!(5))).await;
    assert_eq!(params(events.recv().await), Some(json!(5)));
    assert_eq!(client.dropped_events(), 3);
    common::bounded("shutdown", client.shutdown()).await;
}

#[test]
fn connect_outside_a_runtime_fails_without_panicking() {
    let common::Wire {
        receiver, sender, ..
    } = wire(64, 1024);
    let mut connecting = pin!(McpClient::connect(receiver, sender, options()));
    let mut context = Context::from_waker(Waker::noop());
    match connecting.as_mut().poll(&mut context) {
        Poll::Ready(Err(ConnectError::NoRuntime)) => {}
        other => panic!(
            "expected NoRuntime, got {:?}",
            other.map(|result| result.map(drop))
        ),
    }
}
