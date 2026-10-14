//! The catalog read against a loopback server. Each test owns its server,
//! whose task is aborted when the test drops it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bake_ai::AbortController;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::*;

const KEY: &str = "sk-test-secret-key";

/// How the server answers each connection.
#[derive(Clone)]
enum Answer {
    /// Write these bytes and close.
    Bytes(Vec<u8>),
    /// Read the request, then never answer.
    Hang,
    /// Send headers for a chunked body, then `total` bytes in chunks.
    Stream { total: usize },
}

struct Server {
    port: u16,
    heads: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    fn models_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1/models?client_version=pi", self.port)
    }

    fn heads(&self) -> Vec<String> {
        self.heads
            .lock()
            .map(|heads| heads.clone())
            .unwrap_or_default()
    }
}

fn response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Answer {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nconnection: close\r\ncontent-length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    Answer::Bytes(bytes)
}

fn json_response(body: &serde_json::Value) -> Answer {
    response(
        "200 OK",
        &[("content-type", "application/json")],
        body.to_string().as_bytes(),
    )
}

async fn serve(answer: Answer) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let heads = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&heads);
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let answer = answer.clone();
            let recorded = Arc::clone(&recorded);
            // Each connection is served inline: one request per test.
            let mut head = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !head.windows(4).any(|window| window == b"\r\n\r\n") && head.len() < 64 * 1024 {
                match socket.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => head.extend_from_slice(&buffer[..read]),
                }
            }
            if let Ok(mut heads) = recorded.lock() {
                heads.push(String::from_utf8_lossy(&head).into_owned());
            }
            match answer {
                Answer::Bytes(bytes) => {
                    let _ = socket.write_all(&bytes).await;
                    let _ = socket.shutdown().await;
                }
                Answer::Hang => std::future::pending::<()>().await,
                Answer::Stream { total } => {
                    let _ = socket
                        .write_all(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n")
                        .await;
                    let chunk = vec![b' '; 64 * 1024];
                    let mut sent = 0;
                    while sent < total {
                        let size = chunk.len().min(total - sent);
                        let frame = format!("{size:x}\r\n");
                        if socket.write_all(frame.as_bytes()).await.is_err()
                            || socket.write_all(&chunk[..size]).await.is_err()
                            || socket.write_all(b"\r\n").await.is_err()
                        {
                            break;
                        }
                        sent += size;
                    }
                    let _ = socket.write_all(b"0\r\n\r\n").await;
                }
            }
        }
    });
    Server { port, heads, task }
}

async fn check_error(server: &Server) -> CliProxyCheckError {
    match fetch_cli_proxy_models(&server.models_url(), KEY, None, None).await {
        Err(FetchCliProxyModelsError::Check(error)) => error,
        other => panic!("expected a check failure, got {other:?}"),
    }
}

fn assert_no_key(error: &CliProxyCheckError) {
    let shown = format!("{error} {error:?} {}", error.failure.text());
    assert!(!shown.contains(KEY), "the key leaked into {shown}");
}

// cliproxyapi.test.ts: "saves a validated URL and model route …", the
// request half: the models URL, the bearer key, and JSON accepted; and
// Anthropic models sent to the root the caller names.
#[tokio::test]
async fn reads_the_catalog_with_the_bearer_key() {
    let server = serve(json_response(&serde_json::json!({
        "models": [{ "slug": "gpt-test" }, { "slug": "claude-test", "owned_by": "anthropic" }],
    })))
    .await;
    let root = format!("http://127.0.0.1:{}", server.port);
    let models = fetch_cli_proxy_models(&server.models_url(), KEY, None, Some(&root))
        .await
        .expect("the catalog");
    let summary: Vec<(String, Option<String>)> = models
        .iter()
        .map(|model| (model.id.clone(), model.base_url.clone()))
        .collect();
    assert_eq!(
        summary,
        [
            ("gpt-test".to_owned(), None),
            ("claude-test".to_owned(), Some(root))
        ]
    );
    let heads = server.heads();
    assert_eq!(heads.len(), 1);
    let head = heads[0].to_ascii_lowercase();
    assert!(
        head.starts_with("get /v1/models?client_version=pi http/1.1\r\n"),
        "{head}"
    );
    assert!(head.contains(&format!(
        "authorization: bearer {}\r\n",
        KEY.to_ascii_lowercase()
    )));
    assert!(head.contains("accept: application/json\r\n"));
}

// `fetchCliProxyModels` decodes the body with `new TextDecoder().decode`,
// which drops a leading UTF-8 byte order mark before `JSON.parse`.
#[tokio::test]
async fn reads_a_catalog_that_starts_with_a_byte_order_mark() {
    let mut body = b"\xef\xbb\xbf".to_vec();
    body.extend_from_slice(br#"{"data":[{"id":"gpt-test"}]}"#);
    let server = serve(response(
        "200 OK",
        &[("content-type", "application/json")],
        &body,
    ))
    .await;
    let models = fetch_cli_proxy_models(&server.models_url(), KEY, None, None)
        .await
        .expect("the catalog after its byte order mark");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "gpt-test");
}

// cliproxyapi.test.ts: "CLIProxyAPI models" > "validates the model response
// before setup proceeds".
#[tokio::test]
async fn validates_the_model_response_before_setup_proceeds() {
    let server = serve(json_response(&serde_json::json!({ "data": [] }))).await;
    let error = check_error(&server).await;
    assert_eq!(error.failure, CliProxyCheckFailure::Empty);
    assert!(error.to_string().contains("no selectable models"));
}

// cliproxyapi.test.ts: "a proxy that does not validate" > "says which field
// to ask again, and words each failure", against real responses: a refused
// connection, a redirect, a timeout, 401, 404, and a body that is not JSON;
// plus 403, another status, and a list that is not one.
#[tokio::test]
async fn says_which_field_to_ask_again_and_words_each_failure() {
    let words = |error: &CliProxyCheckError| (error.field(), error.failure.text());

    // Refused: bind a port, then free it.
    let free = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let port = free.local_addr().expect("its address").port();
    drop(free);
    let refused = fetch_cli_proxy_models(
        &format!("http://127.0.0.1:{port}/v1/models"),
        KEY,
        None,
        None,
    )
    .await;
    let Err(FetchCliProxyModelsError::Check(refused)) = refused else {
        panic!("expected a refused connection, got {refused:?}");
    };
    assert_eq!(
        words(&refused),
        (
            CheckField::Url,
            format!("Could not reach 127.0.0.1:{port} (ECONNREFUSED)")
        )
    );
    assert_eq!(
        refused.message,
        format!("CLIProxyAPI at 127.0.0.1:{port} is unreachable: ECONNREFUSED")
    );
    assert_no_key(&refused);

    let redirect = serve(response(
        "302 Found",
        &[("location", "http://example.invalid/")],
        b"",
    ))
    .await;
    let error = check_error(&redirect).await;
    assert_eq!(
        words(&error),
        (
            CheckField::Url,
            "The address redirects elsewhere; enter the address it redirects to".into()
        )
    );
    assert_eq!(
        error.message,
        format!(
            "CLIProxyAPI at 127.0.0.1:{} redirected the model request",
            redirect.port
        )
    );
    assert_eq!(redirect.heads().len(), 1, "the redirect was not followed");

    let hang = serve(Answer::Hang).await;
    let timed_out = fetch_with_timeout(
        &hang.models_url(),
        KEY,
        None,
        None,
        Duration::from_millis(200),
    )
    .await;
    let Err(FetchCliProxyModelsError::Check(timed_out)) = timed_out else {
        panic!("expected a timeout, got {timed_out:?}");
    };
    assert_eq!(
        words(&timed_out),
        (
            CheckField::Url,
            format!("No answer within 15 seconds from 127.0.0.1:{}", hang.port)
        )
    );
    assert_eq!(
        timed_out.message,
        format!(
            "CLIProxyAPI at 127.0.0.1:{} did not answer within 15 seconds",
            hang.port
        )
    );

    for (status, field, text) in [
        (
            "401 Unauthorized",
            CheckField::Key,
            "The proxy rejected this API key (HTTP 401)",
        ),
        (
            "403 Forbidden",
            CheckField::Key,
            "The proxy rejected this API key (HTTP 403)",
        ),
        (
            "404 Not Found",
            CheckField::Url,
            "No CLIProxyAPI model list at this address (HTTP 404)",
        ),
        (
            "503 Service Unavailable",
            CheckField::Url,
            "The proxy answered HTTP 503",
        ),
    ] {
        let server = serve(response(status, &[], b"no")).await;
        let error = check_error(&server).await;
        assert_eq!(words(&error), (field, text.to_owned()));
        let code = &status[..3];
        assert_eq!(
            error.message,
            format!("CLIProxyAPI model request failed (HTTP {code})")
        );
        assert_no_key(&error);
    }

    let html = serve(response("200 OK", &[], b"<html>")).await;
    let error = check_error(&html).await;
    assert_eq!(
        words(&error),
        (
            CheckField::Url,
            "This address did not answer with a CLIProxyAPI model list".into()
        )
    );
    assert_eq!(error.message, "CLIProxyAPI returned invalid model JSON");

    let not_a_list = serve(json_response(&serde_json::json!({ "data": {} }))).await;
    let error = check_error(&not_a_list).await;
    assert_eq!(error.failure, CliProxyCheckFailure::NotProxy);
    assert_eq!(error.message, "CLIProxyAPI returned an invalid model list");
}

// fetchCliProxyModels' 4 MiB cap, from the declared length before the body
// is read and from the bytes of a body that declares none.
#[tokio::test]
async fn refuses_an_oversize_catalog() {
    let declared = serve(Answer::Bytes(
        format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            MAX_CATALOG_BYTES + 1
        )
        .into_bytes(),
    ))
    .await;
    let error = check_error(&declared).await;
    assert_eq!(error.failure, CliProxyCheckFailure::TooLarge);
    assert_eq!(error.message, "CLIProxyAPI model list is too large");
    assert_eq!(
        error.failure.text(),
        "The proxy's model list is too large to read"
    );

    let streamed = serve(Answer::Stream {
        total: MAX_CATALOG_BYTES + 1,
    })
    .await;
    assert_eq!(
        check_error(&streamed).await.failure,
        CliProxyCheckFailure::TooLarge
    );

    // Exactly the cap is read: whitespace around an empty list.
    let mut body = vec![b' '; MAX_CATALOG_BYTES - 2];
    body.extend_from_slice(b"[]");
    let at_cap = serve(response("200 OK", &[], &body)).await;
    assert_eq!(
        check_error(&at_cap).await.failure,
        CliProxyCheckFailure::Empty
    );
}

// cliproxyapi.test.ts: "a proxy that does not validate" > "lets the caller
// abort instead of reporting a failure": an aborted signal ends the request
// as an abort, before connecting or while waiting.
#[tokio::test]
async fn lets_the_caller_abort_instead_of_reporting_a_failure() {
    let hang = serve(Answer::Hang).await;
    let controller = AbortController::new();
    controller.abort();
    let signal = controller.signal();
    assert_eq!(
        fetch_cli_proxy_models(&hang.models_url(), KEY, Some(&signal), None).await,
        Err(FetchCliProxyModelsError::Aborted)
    );
    assert!(hang.heads().is_empty(), "an aborted signal sent nothing");

    let controller = AbortController::new();
    let signal = controller.signal();
    let started = std::time::Instant::now();
    let aborter = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        controller.abort();
    });
    let result = fetch_cli_proxy_models(&hang.models_url(), KEY, Some(&signal), None).await;
    assert_eq!(result, Err(FetchCliProxyModelsError::Aborted));
    assert!(started.elapsed() < Duration::from_secs(5));
    aborter.await.expect("the aborter ends");
}

#[tokio::test]
async fn refuses_an_invalid_url_and_an_unsendable_key() {
    assert_eq!(
        fetch_cli_proxy_models("not a url", KEY, None, None).await,
        Err(FetchCliProxyModelsError::InvalidUrl)
    );
    let server = serve(json_response(&serde_json::json!([{ "id": "a" }]))).await;
    let result = fetch_cli_proxy_models(&server.models_url(), "bad\nkey", None, None).await;
    let Err(FetchCliProxyModelsError::Check(error)) = result else {
        panic!("expected a check failure, got {result:?}");
    };
    assert!(matches!(
        error.failure,
        CliProxyCheckFailure::Unreachable { .. }
    ));
    assert!(!format!("{error:?}").contains("bad\nkey"));
    assert!(server.heads().is_empty());
}
