//! A loopback Streamable HTTP MCP server for [`super::http`]'s tests: one
//! request per connection, answered from a state the test changes between
//! requests, with every request recorded.
//!
//! It is test code with its own HTTP reader, independent of the transport
//! under test, as the web tests' server ([`crate::web::test_server`]) and
//! `bake-mcp`'s HTTP fixture are. Each server binds port 0 on 127.0.0.1 and
//! owns its connections' tasks; [`Server::stop`] aborts and awaits them.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

/// One received request.
#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub(crate) method: String,
    pub(crate) path: String,
    /// Lowercased names.
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl Request {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub(crate) fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The JSON-RPC method, if the body has one.
    pub(crate) fn rpc_method(&self) -> Option<String> {
        self.json()
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
}

/// One step of an answer.
#[derive(Clone, Debug)]
pub(crate) enum Step {
    Write(Vec<u8>),
    Sleep(Duration),
    /// Drop the connection, whatever was written so far.
    Close,
}

/// What the server answers one request with.
pub(crate) type Handler = Arc<dyn Fn(&Request) -> Vec<Step> + Send + Sync>;

/// A loopback server that records every request.
pub(crate) struct Server {
    /// The MCP endpoint, `http://127.0.0.1:<port>/mcp`.
    pub(crate) url: String,
    pub(crate) port: u16,
    requests: Arc<Mutex<Vec<Request>>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Server {
    pub(crate) async fn start(handler: Handler) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("address").port();
        let requests: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { return };
                        connections.spawn(serve(stream, Arc::clone(&handler), Arc::clone(&recorded)));
                    }
                    Some(_) = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Self {
            url: format!("http://127.0.0.1:{port}/mcp"),
            port,
            requests,
            task: Some(task),
        }
    }

    pub(crate) fn requests(&self) -> Vec<Request> {
        lock(&self.requests).clone()
    }

    /// The requests whose JSON-RPC method is `method`.
    pub(crate) fn rpc(&self, method: &str) -> Vec<Request> {
        self.requests()
            .into_iter()
            .filter(|request| request.rpc_method().as_deref() == Some(method))
            .collect()
    }

    /// The requests with this HTTP method.
    pub(crate) fn with_method(&self, method: &str) -> Vec<Request> {
        self.requests()
            .into_iter()
            .filter(|request| request.method == method)
            .collect()
    }

    /// Abort every connection's task and wait until they have ended.
    pub(crate) async fn stop(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

async fn serve(mut stream: TcpStream, handler: Handler, requests: Arc<Mutex<Vec<Request>>>) {
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    let steps = {
        lock(&requests).push(request.clone());
        handler(&request)
    };
    for step in steps {
        match step {
            Step::Write(bytes) => {
                if stream.write_all(&bytes).await.is_err() {
                    return;
                }
                let _ = stream.flush().await;
            }
            Step::Sleep(duration) => tokio::time::sleep(duration).await,
            Step::Close => return,
        }
    }
    let _ = stream.shutdown().await;
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

/// A whole response with a `Content-Length`.
pub(crate) fn reply(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Vec<Step> {
    let mut head = format!("HTTP/1.1 {status} X\r\ncontent-length: {}\r\n", body.len());
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("connection: close\r\n\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body);
    vec![Step::Write(bytes)]
}

/// A JSON response.
pub(crate) fn json_reply(headers: &[(&str, &str)], body: &Value) -> Vec<Step> {
    let mut all = vec![("content-type", "application/json")];
    all.extend_from_slice(headers);
    reply(200, &all, body.to_string().as_bytes())
}

/// A status with no body.
pub(crate) fn status(code: u16) -> Vec<Step> {
    reply(code, &[], b"")
}

/// A server-sent event stream head, for a handler that writes its own
/// events and may then close the connection.
pub(crate) fn event_stream_head() -> Step {
    Step::Write(
        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n".to_vec(),
    )
}

/// One `data:` event.
pub(crate) fn event(data: &Value) -> Step {
    Step::Write(format!("data: {data}\n\n").into_bytes())
}

/// How the fixture answers `initialize`.
#[derive(Clone, Debug)]
pub(crate) enum Initialize {
    /// With a result, assigning [`State::session`].
    Answer,
    /// Not at all: the connection waits this long and is then dropped.
    Stall(Duration),
    /// With this status and `WWW-Authenticate` challenge.
    Status(u16, Option<String>),
}

/// A 307 answer to a POST: to `location`, for a POST to `path`, or to every
/// POST when it is `None`, which loops until the client's hop bound.
#[derive(Clone, Debug)]
pub(crate) struct Redirect {
    pub(crate) location: String,
    pub(crate) path: Option<String>,
}

/// How the fixture answers `tools/call`.
#[derive(Clone, Debug)]
pub(crate) enum Call {
    /// With this text.
    Answer(String),
    /// With an event stream it drops before the response.
    Disconnect,
    /// With one event whose data is this many bytes.
    OversizedEvent(usize),
}

/// What the fixture does, between requests.
#[derive(Debug)]
pub(crate) struct State {
    /// The session ID the next `initialize` assigns.
    pub(crate) session: String,
    /// Sessions the server has expired: a request carrying one gets 404.
    pub(crate) expired: HashSet<String>,
    /// The tool names `tools/list` answers with.
    pub(crate) tools: Vec<String>,
    /// A `tools/list` answer padded to this many bytes.
    pub(crate) oversized_list: Option<usize>,
    /// Answer a POST with a 307 instead of serving it.
    pub(crate) redirect: Option<Redirect>,
    /// How `initialize` is answered.
    pub(crate) initialize: Initialize,
    /// How `tools/call` is answered.
    pub(crate) call: Call,
    /// Answer `tools/list` with a 500 whose body is this text with the
    /// credential headers the request carried echoed into it, as a server
    /// that reports what it rejected would.
    pub(crate) list_error: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            session: "session-1".to_owned(),
            expired: HashSet::new(),
            tools: vec!["echo".to_owned()],
            oversized_list: None,
            redirect: None,
            initialize: Initialize::Answer,
            call: Call::Answer("hello".to_owned()),
            list_error: None,
        }
    }
}

/// A shared [`State`] a test changes while its server runs.
#[derive(Clone, Default)]
pub(crate) struct Shared(Arc<Mutex<State>>);

impl Shared {
    pub(crate) fn set(&self, change: impl FnOnce(&mut State)) {
        change(&mut lock(&self.0));
    }

    fn read(&self) -> State {
        let state = lock(&self.0);
        State {
            session: state.session.clone(),
            expired: state.expired.clone(),
            tools: state.tools.clone(),
            oversized_list: state.oversized_list,
            redirect: state.redirect.clone(),
            initialize: state.initialize.clone(),
            call: state.call.clone(),
            list_error: state.list_error.clone(),
        }
    }
}

/// The MCP endpoint: `initialize` assigns a session, `tools/list` answers
/// with the state's tools, `tools/call` answers as the state says, a GET has
/// no server stream (405), and a DELETE succeeds. A request that carries an
/// expired session gets 404, as the specification asks.
pub(crate) fn protocol(shared: &Shared) -> Handler {
    let shared = shared.clone();
    Arc::new(move |request: &Request| {
        let state = shared.read();
        match request.method.as_str() {
            "GET" => return status(405),
            "DELETE" => return status(200),
            _ => {}
        }
        if let Some(redirect) = &state.redirect
            && redirect
                .path
                .as_ref()
                .is_none_or(|path| request.path == *path)
        {
            return reply(307, &[("location", &redirect.location)], b"");
        }
        if request
            .header("mcp-session-id")
            .is_some_and(|session| state.expired.contains(session))
        {
            return reply(404, &[], b"Session not found");
        }
        let message = request.json();
        let Some(id) = message.get("id").cloned() else {
            return status(202);
        };
        match message.get("method").and_then(Value::as_str) {
            Some("initialize") => match state.initialize {
                Initialize::Answer => json_reply(
                    &[("mcp-session-id", &state.session)],
                    &initialize_result(&id),
                ),
                Initialize::Stall(wait) => vec![Step::Sleep(wait), Step::Close],
                Initialize::Status(code, ref challenge) => match challenge {
                    Some(challenge) => reply(
                        code,
                        &[("www-authenticate", challenge)],
                        b"authorization required",
                    ),
                    None => status(code),
                },
            },
            Some("tools/list") => {
                if let Some(error) = &state.list_error {
                    let echoed = format!(
                        "{error} x-api-key={} authorization={}",
                        request.header("x-api-key").unwrap_or("none"),
                        request.header("authorization").unwrap_or("none"),
                    );
                    return reply(500, &[], echoed.as_bytes());
                }
                let mut result = json!({"jsonrpc": "2.0", "id": id, "result": {
                    "tools": state.tools.iter().map(|name| json!({"name": name, "inputSchema": {"type": "object"}})).collect::<Vec<_>>(),
                }});
                if let Some(size) = state.oversized_list {
                    result["result"]["tools"][0]["description"] = json!("x".repeat(size));
                }
                json_reply(&[], &result)
            }
            Some("tools/call") => match state.call {
                Call::Answer(ref text) => json_reply(&[], &tool_result(&id, text)),
                Call::Disconnect => vec![
                    event_stream_head(),
                    Step::Write(b"id: partial\n".to_vec()),
                    Step::Close,
                ],
                Call::OversizedEvent(size) => vec![
                    event_stream_head(),
                    event(&tool_result(&id, &"x".repeat(size))),
                    Step::Close,
                ],
            },
            _ => json_reply(
                &[],
                &json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "no such method"}}),
            ),
        }
    })
}

pub(crate) fn initialize_result(id: &Value) -> Value {
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

pub(crate) fn tool_result(id: &Value, text: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": text}]}})
}
