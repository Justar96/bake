//! A scripted MCP server on the far side of two in-memory pipes, and
//! transport halves that report when the client's task drops them.
//!
//! The server reads and writes raw newline-delimited JSON with Tokio's own
//! line reader, not with `bake-mcp`'s framing, so the client's framing is
//! checked against an independent peer.

#![allow(dead_code)]

use std::io;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bake_mcp::{ClientEvents, ClientOptions, Implementation, LineReader, LineWriter, McpClient};
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, DuplexStream, ReadBuf,
};
use tokio::sync::oneshot;

/// Every wait in these tests is bounded; a hang fails instead of stalling CI.
/// Under a paused clock it also bounds the auto-advance, so it must stay far
/// above every timeout under test.
pub const WAIT: Duration = Duration::from_secs(60);

pub async fn bounded<F: Future>(what: &str, work: F) -> F::Output {
    match tokio::time::timeout(WAIT, work).await {
        Ok(output) => output,
        Err(_) => panic!("timed out waiting for {what}"),
    }
}

/// A byte stream that drops a `oneshot::Sender` with itself, so a test can
/// tell exactly when the client's task released it.
pub struct Tracked<T> {
    inner: T,
    _alive: oneshot::Sender<()>,
}

impl<T: AsyncRead + Unpin> AsyncRead for Tracked<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Tracked<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Whether the client still holds one transport half.
pub struct Alive(oneshot::Receiver<()>);

impl Alive {
    pub fn held(&mut self) -> bool {
        matches!(self.0.try_recv(), Err(oneshot::error::TryRecvError::Empty))
    }

    pub async fn released(self) {
        let _ = bounded("the client to drop a transport half", self.0).await;
    }
}

pub type ClientReceiver = LineReader<Tracked<DuplexStream>>;
pub type ClientSender = LineWriter<Tracked<DuplexStream>>;

/// The client's halves, the server, and the liveness of each half.
pub struct Wire {
    pub receiver: ClientReceiver,
    pub sender: ClientSender,
    pub server: Server,
    pub receiver_alive: Alive,
    pub sender_alive: Alive,
}

/// Pipes whose client-to-server direction buffers `upload` bytes, with the
/// client reading lines of at most `limit` bytes.
pub fn wire(upload: usize, limit: usize) -> Wire {
    wire_sized(upload, 64 * 1024, limit)
}

/// Like [`wire`], with the server-to-client pipe buffering `download` bytes.
pub fn wire_sized(upload: usize, download: usize, limit: usize) -> Wire {
    let (client_out, server_in) = tokio::io::duplex(upload);
    let (server_out, client_in) = tokio::io::duplex(download);
    let (receiver_token, receiver_alive) = oneshot::channel();
    let (sender_token, sender_alive) = oneshot::channel();
    Wire {
        receiver: LineReader::new(
            Tracked {
                inner: client_in,
                _alive: receiver_token,
            },
            NonZeroUsize::new(limit).unwrap(),
        ),
        sender: LineWriter::new(Tracked {
            inner: client_out,
            _alive: sender_token,
        }),
        server: Server {
            input: Some(BufReader::new(server_in)),
            output: Some(server_out),
        },
        receiver_alive: Alive(receiver_alive),
        sender_alive: Alive(sender_alive),
    }
}

pub fn options() -> ClientOptions {
    let mut options = ClientOptions::new(Implementation::new("test-client", "2.0.0"));
    options.request_timeout = None;
    options
}

/// Pi's test server's initialize result (`test/client.test.ts`).
pub fn init_result() -> Value {
    json!({
        "protocolVersion": "2025-11-25",
        "capabilities": {"tools": {"listChanged": true}},
        "serverInfo": {"name": "test-server", "version": "1.0.0"},
        "instructions": "Use test tools.",
    })
}

pub struct Server {
    input: Option<BufReader<DuplexStream>>,
    output: Option<DuplexStream>,
}

impl Server {
    /// The next message, or `None` at end of stream.
    pub async fn next(&mut self) -> Option<Value> {
        let input = self.input.as_mut().expect("the server's input is open");
        let mut line = Vec::new();
        let read = bounded("a client message", input.read_until(b'\n', &mut line))
            .await
            .expect("the server's read failed");
        if read == 0 {
            return None;
        }
        assert_eq!(
            line.pop(),
            Some(b'\n'),
            "a client message ended without a newline"
        );
        Some(serde_json::from_slice(&line).expect("the client sent invalid JSON"))
    }

    pub async fn recv(&mut self) -> Value {
        match self.next().await {
            Some(message) => message,
            None => panic!("the client closed its output"),
        }
    }

    /// The next request, checked to be `method`; returns its id and params.
    pub async fn request(&mut self, method: &str) -> (Value, Value) {
        let message = self.recv().await;
        assert_eq!(message["method"], method, "{message}");
        assert!(message.get("id").is_some(), "{message}");
        (
            message["id"].clone(),
            message.get("params").cloned().unwrap_or(Value::Null),
        )
    }

    pub async fn expect_eof(&mut self) {
        if let Some(message) = self.next().await {
            panic!("expected the client to close its output, got {message}");
        }
    }

    pub async fn send(&mut self, message: Value) {
        let mut bytes = message.to_string().into_bytes();
        bytes.push(b'\n');
        self.send_raw(&bytes).await;
    }

    pub async fn send_raw(&mut self, bytes: &[u8]) {
        let output = self.output.as_mut().expect("the server's output is open");
        bounded("the server's write", output.write_all(bytes))
            .await
            .expect("the server's write failed");
    }

    pub async fn reply(&mut self, id: &Value, result: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "result": result}))
            .await;
    }

    pub async fn reply_error(&mut self, id: &Value, error: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "error": error}))
            .await;
    }

    pub async fn notify(&mut self, method: &str, params: Option<Value>) {
        let mut message = json!({"jsonrpc": "2.0", "method": method});
        if let Some(params) = params {
            message["params"] = params;
        }
        self.send(message).await;
    }

    /// Answer `initialize` with `result` and take `notifications/initialized`.
    pub async fn handshake(&mut self, result: Value) -> Value {
        let initialize = self.recv().await;
        assert_eq!(initialize["method"], "initialize", "{initialize}");
        self.reply(&initialize["id"].clone(), result).await;
        let initialized = self.recv().await;
        assert_eq!(
            initialized,
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
        );
        initialize
    }

    /// Take the server's output to write it from another future.
    pub fn take_output(&mut self) -> DuplexStream {
        self.output.take().expect("the server's output is open")
    }

    /// End the server's output (the client sees end of stream).
    pub fn close_output(&mut self) {
        self.output = None;
    }

    /// Stop reading (the client's writes fail once the pipe notices).
    pub fn close_input(&mut self) {
        self.input = None;
    }
}

/// A client connected through `wire` to a server answering `result`.
pub async fn connect_wire(
    wire: Wire,
    options: ClientOptions,
    result: Value,
) -> (McpClient, ClientEvents, Server, Alive, Alive) {
    let Wire {
        receiver,
        sender,
        mut server,
        receiver_alive,
        sender_alive,
    } = wire;
    let (connected, _) = bounded("the handshake", async {
        tokio::join!(
            McpClient::connect(receiver, sender, options),
            server.handshake(result)
        )
    })
    .await;
    let (client, events) = connected.expect("the client connects");
    (client, events, server, receiver_alive, sender_alive)
}

pub async fn connect() -> (McpClient, ClientEvents, Server) {
    let (client, events, server, _, _) =
        connect_wire(wire(64 * 1024, 1024 * 1024), options(), init_result()).await;
    (client, events, server)
}
