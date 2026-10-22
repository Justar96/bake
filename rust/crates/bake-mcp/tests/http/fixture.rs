//! A loopback HTTP/1.1 server for the transport's tests.
//!
//! Each test owns its server: it binds port 0 on 127.0.0.1, and the accept
//! task owns every connection's task. [`Server::stop`] aborts and awaits
//! them, and fails the test if a handler panicked (a failed assertion inside
//! a handler would otherwise end only its own task) or the server did not
//! stop in time; aborting a handler at teardown is not a failure. Dropping a
//! server (a failed test) aborts the accept task, whose `JoinSet` aborts the
//! connections. The server answers one request per
//! connection and closes it, so a body without `Content-Length` ends at the
//! close, as SSE streams do. It is test code with an independent HTTP
//! reader, not the transport's: only `reqwest` speaks HTTP in the product.

#![allow(dead_code)]

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};

/// Every wait in these tests is bounded; a hang fails instead of stalling.
pub const WAIT: Duration = Duration::from_secs(30);

pub async fn bounded<F: Future>(what: &str, work: F) -> F::Output {
    match tokio::time::timeout(WAIT, work).await {
        Ok(output) => output,
        Err(_) => panic!("timed out waiting for {what}"),
    }
}

/// One received request.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    /// Lowercased names.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The JSON-RPC method, if the body has one.
    pub fn rpc_method(&self) -> Option<String> {
        self.json()
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
}

/// The response side of one connection.
pub struct Responder {
    stream: TcpStream,
}

impl Responder {
    /// Status line and headers, with `Connection: close` and no length: the
    /// body ends when the responder is dropped.
    pub async fn head(&mut self, status: u16, headers: &[(&str, &str)]) -> io::Result<()> {
        let mut text = format!("HTTP/1.1 {status} X\r\nconnection: close\r\n");
        for (name, value) in headers {
            text.push_str(&format!("{name}: {value}\r\n"));
        }
        text.push_str("\r\n");
        self.write(text.as_bytes()).await
    }

    pub async fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stream.write_all(bytes).await?;
        self.stream.flush().await
    }

    /// A whole response with a `Content-Length`.
    pub async fn send(mut self, status: u16, headers: &[(&str, &str)], body: &[u8]) {
        let length = body.len().to_string();
        let mut all: Vec<(&str, &str)> = headers.to_vec();
        all.push(("content-length", &length));
        let _ = self.head(status, &all).await;
        let _ = self.write(body).await;
    }

    pub async fn json(self, headers: &[(&str, &str)], body: &Value) {
        let mut all = vec![("content-type", "application/json")];
        all.extend_from_slice(headers);
        self.send(200, &all, body.to_string().as_bytes()).await;
    }

    pub async fn status(self, status: u16) {
        self.send(status, &[], b"").await;
    }

    /// Wait until the client closes the connection.
    pub async fn closed(&mut self) {
        let mut buffer = [0_u8; 256];
        loop {
            match self.stream.read(&mut buffer).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    }
}

type Handler =
    Arc<dyn Fn(Request, Responder) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Wrap an async closure as a handler.
pub fn handler<F, Fut>(f: F) -> Handler
where
    F: Fn(Request, Responder) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    Arc::new(move |request, responder| Box::pin(f(request, responder)))
}

pub struct Server {
    /// The MCP endpoint, `http://127.0.0.1:<port>/mcp`.
    pub url: String,
    pub origin: String,
    requests: Arc<Mutex<Vec<Request>>>,
    connections: Arc<AtomicUsize>,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
    /// Panic messages of handler tasks, kept outside the accept task so an
    /// aborted accept task loses none already seen.
    panics: Arc<Mutex<Vec<String>>>,
}

/// How a server stopped.
#[derive(Debug, Default)]
pub struct StopReport {
    /// The server task did not end within the deadline; it was then aborted
    /// and awaited.
    pub timed_out: bool,
    /// Panics of handler tasks (and of the server task), in the order seen.
    pub panics: Vec<String>,
}

fn panic_message(error: tokio::task::JoinError) -> Option<String> {
    if !error.is_panic() {
        // Cancelled: the teardown's own abort.
        return None;
    }
    let payload = error.into_panic();
    Some(
        payload
            .downcast_ref::<&str>()
            .map(|text| (*text).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a non-string panic".to_owned()),
    )
}

impl Server {
    pub async fn start(handler: Handler) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let (stop, mut stopped) = watch::channel(false);
        let panics = Arc::new(Mutex::new(Vec::new()));
        let task = tokio::spawn({
            let requests = requests.clone();
            let connections = connections.clone();
            let panics = panics.clone();
            async move {
                let record = |joined: Result<(), tokio::task::JoinError>| {
                    if let Some(message) = joined.err().and_then(panic_message) {
                        panics.lock().unwrap().push(message);
                    }
                };
                let mut tasks = JoinSet::new();
                loop {
                    tokio::select! {
                        _ = stopped.changed() => break,
                        accepted = listener.accept() => {
                            let Ok((stream, _)) = accepted else { continue };
                            connections.fetch_add(1, Ordering::SeqCst);
                            tasks.spawn(serve(stream, handler.clone(), requests.clone()));
                        }
                        Some(joined) = tasks.join_next(), if !tasks.is_empty() => record(joined),
                    }
                }
                tasks.abort_all();
                while let Some(joined) = tasks.join_next().await {
                    record(joined);
                }
            }
        });
        Self {
            url: format!("{origin}/mcp"),
            origin,
            requests,
            connections,
            stop,
            task: Some(task),
            panics,
        }
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    /// Abort every connection's task, wait until all have ended, and fail
    /// the test if a handler panicked or the server did not stop in time.
    pub async fn stop(self) {
        let report = self.stop_within(WAIT).await;
        assert!(!report.timed_out, "the fixture server did not stop");
        assert!(
            report.panics.is_empty(),
            "fixture handlers panicked: {:?}",
            report.panics
        );
    }

    /// Stop as [`stop`](Self::stop) does, waiting at most `deadline` for the
    /// server task, and report instead of failing. Past the deadline the
    /// task is aborted, which aborts its connection tasks, and then awaited:
    /// the handle is kept through the timeout, so it is never detached.
    pub async fn stop_within(mut self, deadline: Duration) -> StopReport {
        self.stop.send_replace(true);
        let mut report = StopReport::default();
        if let Some(mut task) = self.task.take() {
            let joined = match tokio::time::timeout(deadline, &mut task).await {
                Ok(joined) => joined,
                Err(_) => {
                    report.timed_out = true;
                    task.abort();
                    bounded("the aborted fixture server", task).await
                }
            };
            if let Some(message) = joined.err().and_then(panic_message) {
                self.panics.lock().unwrap().push(message);
            }
        }
        report.panics = std::mem::take(&mut *self.panics.lock().unwrap());
        report
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn serve(mut stream: TcpStream, handler: Handler, requests: Arc<Mutex<Vec<Request>>>) {
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    requests.lock().unwrap().push(request.clone());
    handler(request, Responder { stream }).await;
}

async fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    let head_end = loop {
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
        if buffer.len() > 64 * 1024 {
            return None;
        }
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let path = first.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}

pub const SESSION: &str = "session-1";

pub fn initialize_result(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": "2025-11-25",
            "capabilities": {"tools": {"listChanged": true}},
            "serverInfo": {"name": "http-fixture", "version": "1.0.0"},
        },
    })
}

pub fn tool_result(id: &Value, text: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": text}]}})
}

/// Pi's `protocolHandler`: GET has no stream (405), DELETE succeeds,
/// notifications are accepted, `initialize` and `tools/list` answer with
/// JSON, and other requests with an SSE stream that names an event ID.
pub async fn protocol(request: Request, responder: Responder) {
    match request.method.as_str() {
        "GET" => return responder.status(405).await,
        "DELETE" => return responder.status(200).await,
        _ => {}
    }
    let message = request.json();
    let Some(id) = message.get("id").cloned() else {
        return responder.status(202).await;
    };
    match message.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            responder
                .json(&[("mcp-session-id", SESSION)], &initialize_result(&id))
                .await;
        }
        Some("tools/list") => {
            responder
                .json(
                    &[],
                    &json!({"jsonrpc": "2.0", "id": id, "result": {"tools": [
                        {"name": "echo", "inputSchema": {"type": "object"}}
                    ]}}),
                )
                .await;
        }
        _ => {
            let mut responder = responder;
            let _ = responder
                .head(200, &[("content-type", "text/event-stream")])
                .await;
            let _ = responder.write(b"id: tool-result\n").await;
            let event = format!("data: {}\n\n", tool_result(&id, "hello"));
            let _ = responder.write(event.as_bytes()).await;
        }
    }
}
