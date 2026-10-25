use std::io::{BufRead as _, BufReader};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;

use proptest::prelude::*;

use super::*;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// One real HTTP response. An incomplete body stays open until drop, so
/// rejecting it cannot depend on EOF or a successful request timeout.
struct MetadataServer {
    address: SocketAddr,
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl MetadataServer {
    fn start(response: Vec<u8>, close_body: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        let address = listener.local_addr().expect("listener address");
        let (stop, stopped) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request connection");
            stream
                .set_read_timeout(Some(TEST_TIMEOUT))
                .expect("read deadline");
            stream
                .set_write_timeout(Some(TEST_TIMEOUT))
                .expect("write deadline");
            for line in BufReader::new(&stream).lines() {
                match line {
                    Ok(line) if !line.is_empty() => {}
                    _ => break,
                }
            }
            if stream.write_all(&response).is_err() {
                return;
            }
            if close_body {
                let _ = stream.shutdown(Shutdown::Write);
            } else {
                let _ = stopped.recv_timeout(TEST_TIMEOUT * 2);
            }
        });
        Self {
            address,
            stop,
            thread: Some(thread),
        }
    }

    async fn fetch(&self, name: &str) -> Result<Vec<u8>, UpdateError> {
        let source = Source {
            base: format!("http://{}", self.address),
            allow_loopback_http: true,
            ..Source::default()
        };
        tokio::time::timeout(TEST_TIMEOUT, source.fetch_metadata(name))
            .await
            .expect("metadata fetch settles before the server finishes its body")
    }

    fn oversized_error(&self, name: &str) -> UpdateError {
        UpdateError::Manifest(format!(
            "http://{}/{name} is larger than a release manifest can be",
            self.address
        ))
    }
}

impl Drop for MetadataServer {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        // Wake accept even if the test stopped before making a request.
        if let Ok(stream) = TcpStream::connect(self.address) {
            let _ = stream.shutdown(Shutdown::Both);
        }
        self.thread
            .take()
            .expect("server thread")
            .join()
            .expect("server settles");
    }
}

fn response(headers: &str, body: &[u8]) -> Vec<u8> {
    let mut response = format!("HTTP/1.1 200 OK\r\n{headers}\r\n").into_bytes();
    response.extend_from_slice(body);
    response
}

#[tokio::test]
async fn metadata_at_the_bound_is_accepted_for_both_routes() {
    for name in ["stable.json", "stable.json.sig"] {
        let body = vec![b'x'; MAX_METADATA_BYTES];
        let headers = format!("Content-Length: {}\r\n", body.len());
        let server = MetadataServer::start(response(&headers, &body), false);
        assert_eq!(server.fetch(name).await, Ok(body));
    }
}

#[tokio::test]
async fn oversized_metadata_is_refused_before_the_response_ends() {
    let body = vec![b'x'; MAX_METADATA_BYTES + 1];
    let mut chunk = format!("{:x}\r\n", body.len()).into_bytes();
    chunk.extend_from_slice(&body);
    chunk.extend_from_slice(b"\r\n");
    for name in ["stable.json", "stable.json.sig"] {
        // None of these responses finishes: the length is not reached,
        // the final chunk is absent, or the connection remains open.
        for bytes in [
            response(&format!("Content-Length: {}\r\n", body.len() + 1), &body),
            response("Transfer-Encoding: chunked\r\n", &chunk),
            response("Connection: close\r\n", &body),
        ] {
            let server = MetadataServer::start(bytes, false);
            assert_eq!(server.fetch(name).await, Err(server.oversized_error(name)));
        }
    }
}

#[tokio::test]
async fn a_truncated_metadata_response_keeps_its_network_error() {
    let server = MetadataServer::start(response("Content-Length: 4\r\n", b"abc"), true);
    assert_eq!(
        server.fetch("stable.json").await,
        Err(UpdateError::Network(format!(
            "could not read http://{}/stable.json: the request failed",
            server.address
        )))
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn metadata_bytes_and_the_limit_are_independent_of_http_chunk_boundaries(
        size in prop_oneof![
            0..256usize,
            MAX_METADATA_BYTES - 1..=MAX_METADATA_BYTES + 1,
            MAX_METADATA_BYTES + 2..MAX_METADATA_BYTES + 256,
        ],
        chunk_size in 1..4096usize,
        byte in any::<u8>(),
    ) {
        let body = vec![byte; size];
        let mut chunks = Vec::new();
        for chunk in body.chunks(chunk_size) {
            write!(chunks, "{:x}\r\n", chunk.len()).expect("chunk header");
            chunks.extend_from_slice(chunk);
            chunks.extend_from_slice(b"\r\n");
        }
        chunks.extend_from_slice(b"0\r\n\r\n");
        let server = MetadataServer::start(response("Transfer-Encoding: chunked\r\n", &chunks), false);
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("test runtime");
        let result = runtime.block_on(server.fetch("stable.json"));
        if size <= MAX_METADATA_BYTES {
            prop_assert_eq!(result, Ok(body));
        } else {
            prop_assert_eq!(result, Err(server.oversized_error("stable.json")));
        }
    }
}
