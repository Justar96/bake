//! A loopback HTTP/1.1 server for the web tests: each connection gets
//! the handler's canned steps, then is closed.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One step of a canned response.
#[derive(Clone)]
pub(crate) enum Step {
    Write(Vec<u8>),
    Sleep(Duration),
}

/// What the server answers for a request path and body.
pub(crate) type Handler = Arc<dyn Fn(&str, &[u8]) -> Vec<Step> + Send + Sync>;

/// Each request's head and body, in arrival order.
type Recorded = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// A loopback server; it records each request's head and body.
pub(crate) struct Server {
    pub(crate) port: u16,
    heads: Recorded,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    pub(crate) async fn start(handler: Handler) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("address").port();
        let heads = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&heads);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let handler = Arc::clone(&handler);
                let recorded = Arc::clone(&recorded);
                tokio::spawn(async move {
                    let mut buffer = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                        }
                    }
                    let end = buffer
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .map_or(buffer.len(), |index| index + 4);
                    let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
                    let length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    while buffer.len() < end + length {
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                        }
                    }
                    let body = buffer[end..].to_vec();
                    let path = head.split_whitespace().nth(1).unwrap_or("/").to_owned();
                    recorded.lock().expect("heads").push((head, body.clone()));
                    for step in handler(&path, &body) {
                        match step {
                            Step::Write(bytes) => {
                                if socket.write_all(&bytes).await.is_err() {
                                    return;
                                }
                                let _ = socket.flush().await;
                            }
                            Step::Sleep(duration) => tokio::time::sleep(duration).await,
                        }
                    }
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self { port, heads, task }
    }

    pub(crate) fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub(crate) fn requests(&self) -> usize {
        self.heads.lock().expect("heads").len()
    }

    pub(crate) fn head(&self, index: usize) -> String {
        self.heads.lock().expect("heads")[index].0.clone()
    }

    /// The body of request `index`, as JSON.
    pub(crate) fn body(&self, index: usize) -> serde_json::Value {
        serde_json::from_slice(&self.heads.lock().expect("heads")[index].1)
            .unwrap_or(serde_json::Value::Null)
    }
}

/// A complete response with a `content-length`.
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

/// A response without a length, its body sent in `chunks` until close.
pub(crate) fn streamed(content_type: &str, chunks: &[&[u8]]) -> Vec<Step> {
    let mut steps = vec![Step::Write(
        format!("HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\nconnection: close\r\n\r\n")
            .into_bytes(),
    )];
    steps.extend(chunks.iter().map(|chunk| Step::Write(chunk.to_vec())));
    steps
}
