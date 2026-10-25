//! Bake's HTTP ownership regressions. Each fixture owns one loopback
//! connection and observes its peer close before its task is joined.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::StreamOptions;
use crate::http::{HttpRequest, SdkErrorShape, SseResponse};
use crate::providers::faux::faux_model;
use crate::types::{AssistantMessageEvent, StopReason, TranscriptContext};
use crate::utils::abort::AbortController;
use crate::utils::error_body::{ProviderError, ProviderErrorKind};
use crate::utils::event_stream::AssistantMessageEventStream;

use super::{anthropic_messages, openai_completions, openai_responses};

const BOUND: Duration = Duration::from_secs(10);

struct Chunk {
    body: Vec<u8>,
    sent: oneshot::Sender<()>,
}

struct Server {
    base: String,
    received: Option<oneshot::Receiver<()>>,
    chunks: mpsc::UnboundedSender<Chunk>,
    task: Option<JoinHandle<io::Result<()>>>,
}

impl Server {
    async fn start(status: Option<u16>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("loopback");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let (received_tx, received) = oneshot::channel();
        let (chunks, mut commands) = mpsc::unbounded_channel::<Chunk>();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            read_request(&mut socket).await?;
            if let Some(status) = status {
                socket.write_all(format!(
                    "HTTP/1.1 {status} X\r\nconnection: close\r\ntransfer-encoding: chunked\r\ncontent-type: text/event-stream\r\n\r\n"
                ).as_bytes()).await?;
                socket.flush().await?;
            }
            let _ = received_tx.send(());
            let mut probe = [0; 1];
            loop {
                tokio::select! {
                    read = socket.read(&mut probe) => {
                        match read {
                            Ok(0) => return Ok(()),
                            Err(error) if matches!(error.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted) => return Ok(()),
                            Err(error) => return Err(error),
                            Ok(_) => return Err(io::Error::other("unexpected second request")),
                        }
                    }
                    command = commands.recv() => {
                        let Some(chunk) = command else { return Ok(()) };
                        socket.write_all(format!("{:x}\r\n", chunk.body.len()).as_bytes()).await?;
                        socket.write_all(&chunk.body).await?;
                        socket.write_all(b"\r\n").await?;
                        socket.flush().await?;
                        let _ = chunk.sent.send(());
                    }
                }
            }
        });
        Self {
            base,
            received: Some(received),
            chunks,
            task: Some(task),
        }
    }

    async fn received(&mut self) {
        tokio::time::timeout(BOUND, self.received.take().expect("one request"))
            .await
            .expect("request arrived")
            .expect("server received it");
    }

    async fn chunk(&self, body: impl Into<Vec<u8>>) {
        let (sent, done) = oneshot::channel();
        self.chunks
            .send(Chunk {
                body: body.into(),
                sent,
            })
            .expect("server open");
        tokio::time::timeout(BOUND, done)
            .await
            .expect("chunk sent")
            .expect("write succeeded");
    }

    async fn closed(&mut self) {
        let task = self.task.as_mut().expect("server task");
        let result = tokio::time::timeout(BOUND, &mut *task).await;
        if result.is_err() {
            task.abort();
            let _ = task.await;
        }
        self.task = None;
        result
            .expect("client closed its connection")
            .expect("server task succeeded")
            .expect("server I/O succeeded");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

async fn read_request(socket: &mut TcpStream) -> io::Result<()> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let read = socket.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "request ended",
            ));
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > 1024 * 1024 {
            return Err(io::Error::other("test request too large"));
        }
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&bytes[..end]);
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                return Ok(());
            }
        }
    }
}

fn stream(api: &str, base_url: &str, mut options: StreamOptions) -> AssistantMessageEventStream {
    let mut model = faux_model("fixture");
    model.api = api.into();
    model.provider = if api == "anthropic-messages" {
        "anthropic"
    } else {
        "openai"
    }
    .into();
    model.base_url = base_url.into();
    options.api_key = Some("fixture-key".into());
    let context = TranscriptContext::default();
    match api {
        "openai-completions" => openai_completions::stream(
            &model,
            &context,
            openai_completions::OpenAICompletionsOptions {
                base: options,
                ..Default::default()
            },
        ),
        "openai-responses" => openai_responses::stream(
            &model,
            &context,
            openai_responses::OpenAIResponsesOptions {
                base: options,
                ..Default::default()
            },
        ),
        "anthropic-messages" => anthropic_messages::stream(
            &model,
            &context,
            anthropic_messages::AnthropicOptions {
                base: options,
                ..Default::default()
            },
        ),
        _ => unreachable!("test protocol"),
    }
}

#[tokio::test]
async fn dropping_every_consumer_stops_a_provider_waiting_for_http() {
    for api in [
        "openai-completions",
        "openai-responses",
        "anthropic-messages",
    ] {
        for status in [None, Some(200), Some(401)] {
            let mut server = Server::start(status).await;
            let stream = stream(api, &server.base, StreamOptions::default());
            let watch = stream.producer_watch();
            server.received().await;
            if status == Some(200) {
                assert!(matches!(
                    stream.next().await,
                    Some(AssistantMessageEvent::Start { .. })
                ));
            }
            drop(stream);
            let ended = tokio::time::timeout(BOUND, watch.ended()).await;
            server.closed().await;
            assert!(
                ended.is_ok(),
                "{api}, status {status:?}: provider outlived its consumers"
            );
        }
    }
}

async fn response(base: &str, options: &StreamOptions) -> Result<SseResponse, ProviderError> {
    crate::http::send(
        &HttpRequest {
            url: base.to_owned(),
            headers: Vec::new(),
            body: b"{}".to_vec(),
            error_shape: SdkErrorShape::OpenAI,
        },
        options,
    )
    .await
}

fn poll_once<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

#[tokio::test]
async fn a_silent_body_has_its_own_ten_minute_default() {
    let mut server = Server::start(Some(200)).await;
    let mut response = response(
        &server.base,
        &StreamOptions {
            timeout_ms: Some(30_000),
            ..Default::default()
        },
    )
    .await
    .expect("headers");
    tokio::time::pause();
    let start = tokio::time::Instant::now();
    let error = tokio::time::timeout(Duration::from_secs(601), response.next_events())
        .await
        .expect("body timeout before the outer bound")
        .expect_err("idle timeout");
    let elapsed = start.elapsed();
    tokio::time::resume();
    drop(response);
    server.closed().await;
    assert_eq!(error.kind, ProviderErrorKind::Timeout);
    assert_eq!(elapsed.as_secs(), 600);
}

#[tokio::test]
async fn heartbeat_chunks_renew_the_idle_budget_without_limiting_total_duration() {
    let mut server = Server::start(Some(200)).await;
    let mut response = response(
        &server.base,
        &StreamOptions {
            idle_timeout_ms: Some(60_000),
            ..Default::default()
        },
    )
    .await
    .expect("headers");
    for _ in 0..3 {
        tokio::time::pause();
        let next = response.next_events();
        tokio::pin!(next);
        assert!(poll_once(next.as_mut()).is_pending());
        tokio::time::advance(Duration::from_secs(45)).await;
        assert!(poll_once(next.as_mut()).is_pending());
        tokio::time::resume();
        server.chunk(b": heartbeat\n\n".to_vec()).await;
        let events = tokio::time::timeout(BOUND, next)
            .await
            .expect("heartbeat arrived")
            .expect("read succeeded")
            .expect("body open");
        assert!(
            events.is_empty(),
            "heartbeat bytes are activity without an SSE event"
        );
    }
    tokio::time::pause();
    let start = tokio::time::Instant::now();
    let error = tokio::time::timeout(Duration::from_secs(61), response.next_events())
        .await
        .expect("body timeout before the outer bound")
        .expect_err("silence times out");
    let elapsed = start.elapsed();
    tokio::time::resume();
    drop(response);
    server.closed().await;
    assert_eq!(error.kind, ProviderErrorKind::Timeout);
    assert_eq!(elapsed.as_secs(), 60);
}

#[tokio::test]
async fn disabling_idle_timeout_still_observes_cancellation() {
    let mut server = Server::start(Some(200)).await;
    let abort = AbortController::new();
    let mut response = response(
        &server.base,
        &StreamOptions {
            idle_timeout_ms: Some(0),
            signal: Some(abort.signal()),
            ..Default::default()
        },
    )
    .await
    .expect("headers");
    tokio::time::pause();
    {
        let next = response.next_events();
        tokio::pin!(next);
        assert!(poll_once(next.as_mut()).is_pending());
        tokio::time::advance(Duration::from_secs(601)).await;
        assert!(poll_once(next.as_mut()).is_pending());
        abort.abort();
        assert_eq!(
            next.await.expect_err("aborted").kind,
            ProviderErrorKind::Aborted
        );
    }
    tokio::time::resume();
    drop(response);
    server.closed().await;
}

#[tokio::test]
async fn stalled_error_bodies_time_out_without_retrying_an_authentication_failure() {
    let mut server = Server::start(Some(401)).await;
    let options = StreamOptions {
        idle_timeout_ms: Some(20),
        max_retries: Some(1),
        ..Default::default()
    };
    let result = tokio::time::timeout(BOUND, response(&server.base, &options)).await;
    server.closed().await;
    let error = result
        .expect("the error body is bounded")
        .err()
        .expect("401 failed");
    assert_eq!(error.kind, ProviderErrorKind::Http);
    assert_eq!(error.status, Some(401));
    assert_eq!(
        error.message,
        "401 HTTP error body unavailable: idle deadline reached."
    );
}

#[tokio::test]
async fn protocols_surface_idle_failures_and_preserve_authentication_failures() {
    for api in [
        "openai-completions",
        "openai-responses",
        "anthropic-messages",
    ] {
        for status in [200, 401] {
            let mut server = Server::start(Some(status)).await;
            let stream = stream(
                api,
                &server.base,
                StreamOptions {
                    idle_timeout_ms: Some(20),
                    ..Default::default()
                },
            );
            let result = tokio::time::timeout(BOUND, stream.result())
                .await
                .expect("idle failure ends the response")
                .expect("terminal result");
            tokio::time::timeout(BOUND, stream.producer_ended())
                .await
                .expect("producer cleaned up");
            server.closed().await;
            assert_eq!(result.stop_reason, StopReason::Error, "{api}");
            assert_eq!(
                crate::utils::retry::is_retryable_assistant_error(&result),
                status == 200,
                "{api}, status {status}"
            );
            if status == 401 {
                assert!(
                    result
                        .error_message
                        .as_deref()
                        .unwrap()
                        .contains("401 HTTP error body unavailable:"),
                    "{api}"
                );
            }
        }
    }
}

#[tokio::test]
async fn error_bodies_are_bounded_and_truncation_keeps_the_status() {
    let mut server = Server::start(Some(401)).await;
    let url = server.base.clone();
    let options = StreamOptions {
        max_retries: Some(1),
        ..Default::default()
    };
    let (result, ()) = tokio::time::timeout(BOUND, async {
        tokio::join!(response(&url, &options), async {
            server.received().await;
            server.chunk(vec![b'x'; 64 * 1024 + 1]).await;
        })
    })
    .await
    .expect("size limit ends the error body");
    server.closed().await;
    let error = result.err().expect("401 failed");
    assert_eq!(error.kind, ProviderErrorKind::Http);
    assert_eq!(error.status, Some(401));
    assert!(
        error
            .message
            .ends_with("[HTTP error body truncated at 65536 bytes]")
    );
    assert!(error.message.len() < 65 * 1024);
}

#[tokio::test]
async fn a_remaining_consumer_keeps_the_provider_alive() {
    let mut server = Server::start(Some(200)).await;
    let stream = stream("openai-completions", &server.base, StreamOptions::default());
    let kept = stream.clone();
    let watch = stream.producer_watch();
    assert!(matches!(
        stream.next().await,
        Some(AssistantMessageEvent::Start { .. })
    ));
    drop(stream);
    assert!(poll_once(Box::pin(watch.ended()).as_mut()).is_pending());
    server.chunk(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_vec()).await;
    let result = tokio::time::timeout(BOUND, kept.result())
        .await
        .expect("finished")
        .expect("result");
    tokio::time::timeout(BOUND, watch.ended())
        .await
        .expect("producer cleaned up");
    server.closed().await;
    assert_eq!(result.stop_reason, StopReason::Stop);
}
