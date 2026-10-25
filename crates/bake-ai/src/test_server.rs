//! A loopback HTTP/1.1 server for protocol tests. Each test owns its server
//! and port; the server task ends when the test's runtime shuts down.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One received request.
#[derive(Debug, Clone)]
pub struct Recorded {
    /// `METHOD path` of the request line.
    pub line: String,
    /// Lowercased header names with values.
    pub headers: Vec<(String, String)>,
    /// The JSON body.
    pub body: serde_json::Value,
}

impl Recorded {
    /// The value of header `name`.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// A canned response: status, extra headers, and body chunks written with
/// `pause` between them. A `hang` response never ends its body.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub chunks: Vec<String>,
    pub pause: Duration,
    pub hang: bool,
}

impl Reply {
    /// A 200 `text/event-stream` body.
    pub fn sse(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            chunks: vec![body.into()],
            pause: Duration::ZERO,
            hang: false,
        }
    }

    /// A JSON error response.
    pub fn error(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            chunks: vec![body.into()],
            pause: Duration::ZERO,
            hang: false,
        }
    }
}

/// A running server; `base` is its root URL.
pub struct Server {
    pub base: String,
    pub requests: Arc<Mutex<Vec<Recorded>>>,
}

impl Server {
    /// The requests received so far.
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests
            .lock()
            .map(|requests| requests.clone())
            .unwrap_or_default()
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Serves `replies` in order, one per connection; later connections reuse
/// the last reply.
pub async fn serve(replies: Vec<Reply>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("local address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    tokio::spawn(async move {
        let mut served = 0usize;
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let reply = replies
                .get(served.min(replies.len().saturating_sub(1)))
                .cloned();
            served += 1;
            let recorded = Arc::clone(&recorded);
            tokio::spawn(async move {
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];
                let head_end = loop {
                    let Ok(read) = socket.read(&mut chunk).await else {
                        return;
                    };
                    if read == 0 {
                        return;
                    }
                    buffer.extend_from_slice(&chunk[..read]);
                    if let Some(end) = find(&buffer, b"\r\n\r\n") {
                        break end;
                    }
                };
                let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
                let mut lines = head.split("\r\n");
                let line = lines
                    .next()
                    .unwrap_or("")
                    .rsplit_once(' ')
                    .map(|(line, _)| line.to_owned())
                    .unwrap_or_default();
                let headers: Vec<(String, String)> = lines
                    .filter_map(|line| line.split_once(':'))
                    .map(|(name, value)| (name.trim().to_lowercase(), value.trim().to_owned()))
                    .collect();
                let length = headers
                    .iter()
                    .find(|(name, _)| name == "content-length")
                    .and_then(|(_, value)| value.parse::<usize>().ok())
                    .unwrap_or(0);
                while buffer.len() < head_end + 4 + length {
                    let Ok(read) = socket.read(&mut chunk).await else {
                        return;
                    };
                    if read == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..read]);
                }
                let body = serde_json::from_slice(buffer.get(head_end + 4..).unwrap_or(&[]))
                    .unwrap_or_default();
                if let Ok(mut requests) = recorded.lock() {
                    requests.push(Recorded {
                        line,
                        headers,
                        body,
                    });
                }
                let Some(reply) = reply else { return };
                let mut head = format!(
                    "HTTP/1.1 {} X\r\nconnection: close\r\ntransfer-encoding: chunked\r\n",
                    reply.status
                );
                for (name, value) in &reply.headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                head.push_str("\r\n");
                if socket.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                for (index, part) in reply.chunks.iter().enumerate() {
                    if index > 0 && !reply.pause.is_zero() {
                        tokio::time::sleep(reply.pause).await;
                    }
                    let framed = format!("{:x}\r\n{part}\r\n", part.len());
                    if socket.write_all(framed.as_bytes()).await.is_err() {
                        return;
                    }
                    let _ = socket.flush().await;
                }
                if reply.hang {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                }
                let _ = socket.write_all(b"0\r\n\r\n").await;
                let _ = socket.shutdown().await;
            });
        }
    });
    Server {
        base: format!("http://{address}"),
        requests,
    }
}

/// SSE text of `(event name, data)` pairs.
pub fn sse_events(events: &[(&str, serde_json::Value)]) -> String {
    events
        .iter()
        .map(|(name, data)| format!("event: {name}\ndata: {data}\n\n"))
        .collect()
}

/// SSE text of data-only events, as OpenAI sends them, ending in `[DONE]`.
pub fn sse_data(events: &[serde_json::Value]) -> String {
    let mut text: String = events
        .iter()
        .map(|data| format!("data: {data}\n\n"))
        .collect();
    text.push_str("data: [DONE]\n\n");
    text
}
