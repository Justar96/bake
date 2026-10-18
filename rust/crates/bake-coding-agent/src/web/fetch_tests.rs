//! Ports of `packages/web/web-fetch-http/tests/fetch-http.spec.ts`
//! against a loopback server. Most tests inject a resolver that answers
//! with the loopback address, as the TypeScript stubbed
//! `publicHttpNetwork.resolve`; the policy tests use the real one.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bake_ai::AbortController;

use super::*;
use crate::web::test_server::{Server, Step, reply, streamed};

fn text(body: &str) -> Vec<Step> {
    reply(200, &[("content-type", "text/plain")], body.as_bytes())
}

fn limits() -> HttpFetchLimits {
    HttpFetchLimits {
        timeout: Duration::from_secs(5),
        user_agent: "test-agent/1.0".into(),
        ..HttpFetchLimits::default()
    }
}

fn loopback() -> AddressResolver {
    Arc::new(|_host: &str| Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]))
}

fn fetcher(limits: HttpFetchLimits) -> HttpFetcher {
    HttpFetcher::with_resolver(limits, loopback(), None)
}

async fn fetch(limits: HttpFetchLimits, url: &str) -> Result<WebFetchResult, WebError> {
    fetcher(limits).fetch(url, None).await
}

async fn server(handler: impl Fn(&str) -> Vec<Step> + Send + Sync + 'static) -> Server {
    Server::start(Arc::new(move |path: &str, _body: &[u8]| handler(path))).await
}

// "HttpFetchProvider success" > "fetches a text body" and "uses an
// explicitly injected validated-address resolver".
#[tokio::test]
async fn fetches_a_text_body() {
    let server = server(|_| text("hello")).await;
    let url = format!("{}/a", server.base());
    let result = fetch(limits(), &url).await.expect("fetch");
    assert_eq!(
        result,
        WebFetchResult {
            url: url.clone(),
            status_code: 200,
            body: WebFetchBody::Text("hello".into()),
            truncated: false,
        }
    );
}

// "fetches an html body and classifies it as html".
#[tokio::test]
async fn classifies_an_html_body() {
    let server = server(|_| {
        reply(
            200,
            &[("content-type", "text/html; charset=utf-8")],
            b"<p>x</p>",
        )
    })
    .await;
    let result = fetch(limits(), &server.base()).await.expect("fetch");
    assert_eq!(result.body, WebFetchBody::Html("<p>x</p>".into()));
}

// network: "pins the connection to the validated address without
// resolving the URL hostname again".
#[tokio::test]
async fn pins_the_connection_to_the_validated_address() {
    let server = server(|_| text("pinned")).await;
    let url = format!("http://does-not-resolve.invalid:{}/", server.port);
    let result = fetch(limits(), &url).await.expect("fetch");
    assert_eq!(result.body.content(), "pinned");
    assert!(
        server
            .head(0)
            .contains(&format!("host: does-not-resolve.invalid:{}", server.port))
    );
}

// "sends the configured user agent".
#[tokio::test]
async fn sends_the_configured_user_agent_and_no_credentials() {
    let server = server(|_| text("x")).await;
    fetch(limits(), &server.base()).await.expect("fetch");
    let head = server.head(0).to_ascii_lowercase();
    assert!(head.contains("user-agent: test-agent/1.0"), "{head}");
    assert!(
        head.contains(
            "accept: text/html,application/xhtml+xml,text/*;q=0.9,application/json;q=0.8"
        )
    );
    assert!(!head.contains("cookie") && !head.contains("authorization"));
}

// "returns a non-2xx response as a result, not an error".
#[tokio::test]
async fn returns_a_non_2xx_response_as_a_result() {
    let server = server(|_| reply(404, &[("content-type", "text/plain")], b"missing")).await;
    let result = fetch(limits(), &server.base()).await.expect("fetch");
    assert_eq!(result.status_code, 404);
    assert_eq!(result.body.content(), "missing");
}

// "HttpFetchProvider caps".
#[tokio::test]
async fn rejects_an_over_cap_content_length() {
    let server = server(|_| text("0123456789")).await;
    let error = fetch(
        HttpFetchLimits {
            max_response_bytes: 5,
            ..limits()
        },
        &server.base(),
    )
    .await
    .expect_err("too large");
    assert_eq!(error.code, WEB_FETCH_TOO_LARGE);
    assert_eq!(error.message, "response exceeds the maximum of 5 bytes");
}

#[tokio::test]
async fn truncates_a_stream_that_grows_past_the_byte_cap() {
    let server = server(|_| streamed("text/plain", &[b"01234", b"56789"])).await;
    let capped = HttpFetchLimits {
        max_response_bytes: 7,
        ..limits()
    };
    let result = fetch(capped, &server.base()).await.expect("fetch");
    assert_eq!(result.body.content(), "0123456");
    assert!(result.truncated);
}

#[tokio::test]
async fn does_not_flag_a_body_that_exactly_fills_the_byte_cap() {
    let server = server(|_| streamed("text/plain", &[b"01234", b"56789"])).await;
    let exact = HttpFetchLimits {
        max_response_bytes: 10,
        ..limits()
    };
    let result = fetch(exact, &server.base()).await.expect("fetch");
    assert_eq!(result.body.content(), "0123456789");
    assert!(!result.truncated);
}

#[tokio::test]
async fn truncates_a_decoded_body_past_the_character_cap() {
    let server = server(|_| text("abcdef")).await;
    let capped = HttpFetchLimits {
        max_body_chars: 4,
        ..limits()
    };
    let result = fetch(capped, &server.base()).await.expect("fetch");
    assert_eq!(result.body.content(), "abcd");
    assert!(result.truncated);
}

#[tokio::test]
async fn rejects_unsupported_or_missing_content_types() {
    let server = server(|path| match path {
        "/png" => reply(200, &[("content-type", "image/png")], b"\x89PNG"),
        _ => reply(200, &[], b"bytes"),
    })
    .await;
    let error = fetch(limits(), &format!("{}/png", server.base()))
        .await
        .expect_err("png");
    assert_eq!(error.code, WEB_UNSUPPORTED_CONTENT_TYPE);
    assert_eq!(error.message, "unsupported content type \"image/png\"");
    let error = fetch(limits(), &format!("{}/none", server.base()))
        .await
        .expect_err("none");
    assert_eq!(error.message, "unsupported content type \"unknown\"");
}

// "accepts a declared content-length within the cap", "decodes a
// non-UTF-8 declared charset", "rejects an unsupported declared charset".
#[tokio::test]
async fn decodes_declared_charsets() {
    let server = server(|path| match path {
        "/latin1" => reply(
            200,
            &[("content-type", "text/plain; charset=iso-8859-1")],
            b"caf\xe9",
        ),
        _ => reply(200, &[("content-type", "text/plain; charset=x-nope")], b"x"),
    })
    .await;
    let result = fetch(limits(), &format!("{}/latin1", server.base()))
        .await
        .expect("latin-1");
    assert_eq!(result.body.content(), "café");
    let error = fetch(limits(), &format!("{}/nope", server.base()))
        .await
        .expect_err("charset");
    assert_eq!(error.code, WEB_UNSUPPORTED_CONTENT_TYPE);
    assert_eq!(error.message, "unsupported charset \"x-nope\"");
}

fn redirect(location: &str) -> Vec<Step> {
    reply(302, &[("location", location)], b"")
}

// "HttpFetchProvider redirects" > "follows a same-origin redirect and
// reports the final URL" and "follows a relative same-origin redirect".
#[tokio::test]
async fn follows_same_origin_redirects() {
    let base = Arc::new(Mutex::new(String::new()));
    let absolute = Arc::clone(&base);
    let server = server(move |path| match path {
        "/start" => redirect(&format!("{}/relative", absolute.lock().expect("base"))),
        "/relative" => redirect("/final"),
        _ => text("done"),
    })
    .await;
    *base.lock().expect("base") = server.base();
    let result = fetch(limits(), &format!("{}/start", server.base()))
        .await
        .expect("fetch");
    assert_eq!(result.url, format!("{}/final", server.base()));
    assert_eq!(result.body.content(), "done");
    assert_eq!(server.requests(), 3);
}

// "blocks a cross-origin redirect with WEB_REDIRECT_BLOCKED".
#[tokio::test]
async fn blocks_a_cross_origin_redirect() {
    let server = server(|_| redirect("https://other.test/x")).await;
    let error = fetch(limits(), &server.base()).await.expect_err("blocked");
    assert_eq!(error.code, WEB_REDIRECT_BLOCKED);
    assert_eq!(
        error.message,
        "cross-origin redirect to https://other.test is not followed automatically; retry against that URL directly"
    );
    assert_eq!(server.requests(), 1);
}

// "re-validates a redirect target, rejecting same-origin credentials in
// the Location".
#[tokio::test]
async fn revalidates_a_redirect_target() {
    let base = Arc::new(Mutex::new(String::new()));
    let target = Arc::clone(&base);
    let server = server(move |_| {
        let base = target.lock().expect("base").clone();
        redirect(&base.replace("http://", "http://user:pass@"))
    })
    .await;
    *base.lock().expect("base") = server.base();
    let error = fetch(limits(), &server.base()).await.expect_err("blocked");
    assert_eq!(error.code, WEB_BLOCKED_URL);
}

// "rejects exceeding the redirect hop cap", "follows exactly maxRedirects
// hops", "makes exactly maxRedirects+1 requests before blocking".
#[tokio::test]
async fn enforces_the_redirect_hop_cap() {
    let server = server(|path| {
        let hop: usize = path.trim_start_matches("/hop").parse().unwrap_or(0);
        if hop < 3 {
            redirect(&format!("/hop{}", hop + 1))
        } else {
            text("landed")
        }
    })
    .await;
    let start = format!("{}/hop0", server.base());
    let exact = HttpFetchLimits {
        max_redirects: 3,
        ..limits()
    };
    assert_eq!(
        fetch(exact, &start).await.expect("3 hops").body.content(),
        "landed"
    );
    let before = server.requests();
    let short = HttpFetchLimits {
        max_redirects: 2,
        ..limits()
    };
    let error = fetch(short, &start).await.expect_err("too many");
    assert_eq!(error.code, WEB_REDIRECT_BLOCKED);
    assert_eq!(error.message, "exceeded the maximum of 2 redirects");
    assert_eq!(server.requests() - before, 3);
}

// "reports an over-limit redirect as "exceeded", not cross-origin" and
// "maxRedirects: 0 follows no redirect but still fetches a direct 200".
#[tokio::test]
async fn checks_the_budget_before_the_next_hop() {
    let server = server(|path| match path {
        "/away" => redirect("https://elsewhere.test/"),
        _ => text("direct"),
    })
    .await;
    let none = HttpFetchLimits {
        max_redirects: 0,
        ..limits()
    };
    let error = fetch(none.clone(), &format!("{}/away", server.base()))
        .await
        .expect_err("budget");
    assert_eq!(error.message, "exceeded the maximum of 0 redirects");
    assert_eq!(
        fetch(none, &server.base())
            .await
            .expect("direct")
            .body
            .content(),
        "direct"
    );
}

// "treats a redirect without a Location header as a provider error".
#[tokio::test]
async fn a_redirect_without_location_is_a_provider_error() {
    let server = server(|_| reply(301, &[], b"")).await;
    let error = fetch(limits(), &server.base())
        .await
        .expect_err("no location");
    assert_eq!(error.code, WEB_PROVIDER_ERROR);
    assert_eq!(
        error.message,
        "redirect response (HTTP 301) without a Location header"
    );
}

// Every hop is resolved and validated again: a same-origin redirect whose
// name now answers with a private address is refused (DNS rebinding).
#[tokio::test]
async fn revalidates_the_address_of_every_hop() {
    let server = server(|path| match path {
        "/first" => redirect("/second"),
        _ => text("private"),
    })
    .await;
    let lookups = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&lookups);
    let resolver: AddressResolver = Arc::new(move |host: &str| {
        if counted.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])
        } else {
            resolve_public_addresses(host, &|_: &str| Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]))
        }
    });
    let fetcher = HttpFetcher::with_resolver(limits(), resolver, None);
    let url = format!("http://rebinding.test:{}/first", server.port);
    let error = fetcher.fetch(&url, None).await.expect_err("rebound");
    assert_eq!(error.code, WEB_BLOCKED_URL);
    assert_eq!(
        error.message,
        "URL hostname \"rebinding.test\" resolves to a non-public IP address"
    );
    assert_eq!(lookups.load(Ordering::SeqCst), 2);
    assert_eq!(server.requests(), 1);
}

// "HttpFetchProvider invalid URLs and abort" > "blocks a loopback
// destination before opening a connection", with the real resolver.
#[tokio::test]
async fn blocks_non_public_destinations_before_connecting() {
    let server = server(|_| text("secret")).await;
    let real = HttpFetcher::new(limits(), None);
    for url in [
        server.base(),
        format!("http://localhost:{}/", server.port),
        format!("http://[::1]:{}/", server.port),
        format!("http://[::ffff:127.0.0.1]:{}/", server.port),
        "http://169.254.169.254/latest/meta-data/".to_owned(),
        "http://10.0.0.1/".to_owned(),
    ] {
        let error = real.fetch(&url, None).await.expect_err(&url);
        assert_eq!(error.code, WEB_BLOCKED_URL, "{url}: {}", error.message);
    }
    assert_eq!(server.requests(), 0);
}

// "rejects a non-http scheme before any network access" and "rejects
// credentials in the URL".
#[tokio::test]
async fn rejects_invalid_urls_before_any_network_access() {
    let error = fetch(limits(), "file:///etc/passwd")
        .await
        .expect_err("scheme");
    assert_eq!(error.code, super::super::WEB_INVALID_URL);
    let error = fetch(limits(), "https://user:pass@example.com/")
        .await
        .expect_err("credentials");
    assert_eq!(error.message, "credentials in URLs are not allowed");
}

// "honors a pre-aborted signal" and "aborts an in-flight fetch via the
// signal".
#[tokio::test]
async fn honors_cancellation() {
    let server = server(|_| {
        vec![
            Step::Sleep(Duration::from_secs(10)),
            Step::Write(b"HTTP/1.1 200 OK\r\n\r\n".to_vec()),
        ]
    })
    .await;
    let controller = AbortController::new();
    controller.abort();
    let error = fetcher(limits())
        .fetch(&server.base(), Some(&controller.signal()))
        .await
        .expect_err("pre-aborted");
    assert_eq!(error.code, WEB_ABORTED);
    assert_eq!(server.requests(), 0);

    let controller = AbortController::new();
    let signal = controller.signal();
    let started = std::time::Instant::now();
    let aborter = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        controller.abort();
    });
    let error = fetcher(limits())
        .fetch(&server.base(), Some(&signal))
        .await
        .expect_err("aborted");
    aborter.await.expect("aborter");
    assert_eq!(error.code, WEB_ABORTED);
    assert_eq!(error.message, "web fetch aborted");
    assert!(started.elapsed() < Duration::from_secs(5));
}

// "times out a slow response with WEB_FETCH_TIMEOUT" and "classifies a
// timeout DURING the body read as WEB_FETCH_TIMEOUT".
#[tokio::test]
async fn times_out_slow_responses() {
    let server = server(|path| match path {
        "/head" => vec![Step::Sleep(Duration::from_secs(10))],
        _ => {
            let mut steps = streamed("text/plain", &[b"partial"]);
            steps.push(Step::Sleep(Duration::from_secs(10)));
            steps
        }
    })
    .await;
    let quick = HttpFetchLimits {
        timeout: Duration::from_millis(300),
        ..limits()
    };
    for path in ["/head", "/body"] {
        let error = fetch(quick.clone(), &format!("{}{path}", server.base()))
            .await
            .expect_err(path);
        assert_eq!(error.code, WEB_FETCH_TIMEOUT, "{path}");
        assert_eq!(error.message, "web fetch timed out");
    }
}

// "maps a connection failure to WEB_PROVIDER_ERROR".
#[tokio::test]
async fn maps_a_connection_failure_to_a_provider_error() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("address").port();
    drop(listener);
    let error = fetch(limits(), &format!("http://127.0.0.1:{port}/"))
        .await
        .expect_err("refused");
    assert_eq!(error.code, WEB_PROVIDER_ERROR);
    assert!(
        error.message.starts_with("web fetch failed: "),
        "{}",
        error.message
    );
}

// A lookup a cancelled fetch left running is the session's: closing its
// gate waits for it.
#[tokio::test]
async fn the_session_gate_owns_a_running_lookup() {
    let gate = crate::owned_work::WorkGate::default();
    let resolver: AddressResolver = Arc::new(|_host: &str| {
        std::thread::sleep(Duration::from_millis(300));
        Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])
    });
    let fetcher = HttpFetcher::with_resolver(limits(), resolver, Some(gate.clone()));
    let controller = AbortController::new();
    let signal = controller.signal();
    let fetching = fetcher.fetch("http://slow.test/", Some(&signal));
    let abort = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        controller.abort();
    };
    let (result, ()) = tokio::join!(fetching, abort);
    assert_eq!(result.expect_err("aborted").code, WEB_ABORTED);
    let started = std::time::Instant::now();
    gate.close().await;
    assert!(started.elapsed() >= Duration::from_millis(100));
    let error = fetcher
        .fetch("http://slow.test/", None)
        .await
        .expect_err("closed");
    assert_eq!(error.code, WEB_ABORTED);
}

fn ip(text: &str) -> IpAddr {
    text.parse().expect("ip")
}

/// A lookup answering `answers` for every name but the DNS64 discovery
/// name, which gets `discovery`.
fn lookup(
    answers: Vec<IpAddr>,
    discovery: Vec<IpAddr>,
) -> impl Fn(&str) -> Result<Vec<IpAddr>, String> {
    move |host: &str| {
        Ok(if host == "ipv4only.arpa" {
            discovery.clone()
        } else {
            answers.clone()
        })
    }
}

// "public-network policy": "retains one fully public DNS answer set",
// "rejects the whole DNS answer set when one address is not public",
// "rejects empty ... resolver results", "propagates resolver failures".
#[test]
fn resolves_and_validates_whole_answer_sets() {
    let public = vec![ip("8.8.4.4"), ip("2001:4860:4860::8888")];
    assert_eq!(
        resolve_public_addresses("example.test", &lookup(public.clone(), vec![])),
        Ok(public)
    );
    let error = resolve_public_addresses(
        "rebinding.test",
        &lookup(vec![ip("8.8.8.8"), ip("127.0.0.1")], vec![]),
    )
    .expect_err("private");
    assert_eq!(error.code, WEB_BLOCKED_URL);
    let error = resolve_public_addresses("empty.test", &lookup(vec![], vec![])).expect_err("empty");
    assert_eq!(error.code, WEB_PROVIDER_ERROR);
    assert_eq!(
        error.message,
        "hostname \"empty.test\" resolved to no addresses"
    );
    let error = resolve_public_addresses("broken.test", &|_: &str| Err("dns failed".into()))
        .expect_err("failed");
    assert_eq!(error.message, "web fetch failed: dns failed");
}

// "validates bracketed IPv6 literals after checking for an active DNS64
// prefix", and the two NAT64 translation cases.
#[test]
fn checks_ipv6_answers_against_active_nat64_prefixes() {
    let asked = Mutex::new(Vec::<String>::new());
    let recording = |host: &str| {
        asked.lock().expect("asked").push(host.to_owned());
        Ok(vec![ip("192.0.0.170")])
    };
    assert_eq!(
        resolve_public_addresses("[2001:4860:4860::8888]", &recording),
        Ok(vec![ip("2001:4860:4860::8888")])
    );
    assert_eq!(*asked.lock().expect("asked"), ["ipv4only.arpa"]);
    let prefix = vec![ip("2001:4860:64:64::c000:aa")];
    let error = resolve_public_addresses(
        "nat64.test",
        &lookup(vec![ip("2001:4860:64:64::7f00:1")], prefix.clone()),
    )
    .expect_err("translated private");
    assert_eq!(
        error.message,
        "URL hostname \"nat64.test\" resolves through NAT64 to a non-public IPv4 address"
    );
    assert_eq!(
        resolve_public_addresses(
            "nat64.test",
            &lookup(vec![ip("2001:4860:64:64::808:808")], prefix)
        ),
        Ok(vec![ip("2001:4860:64:64::808:808")])
    );
}
