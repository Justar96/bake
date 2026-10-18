//! Ports of `packages/web/web-search-deepseek/tests/deepseek.spec.ts`
//! (the response mapping and the request), the model selection over a
//! fake route catalog, and a response the CLIProxyAPI gateway returned
//! to a live `web_search_20250305` probe on 2026-10-19
//! (`fixtures/cliproxyapi-web-search.json`, its `encrypted_content`
//! removed).

use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::auth_storage::AuthStorage;
use crate::model_registry::{ModelConfig, ModelRegistry};
use crate::web::test_server::{Server, Step, reply};

const RECORDED: &str = include_str!("fixtures/cliproxyapi-web-search.json");

/// A registry whose CLIProxyAPI route lists `models` (id, api) at `base`.
fn registry(base: &str, models: &[(&str, &str)]) -> ModelRuntime {
    let mut registry = ModelRegistry::new(ModelConfig::empty(), AuthStorage::empty());
    let models: Vec<Value> = models
        .iter()
        .map(|(id, api)| json!({ "id": id, "api": api, "baseUrl": base }))
        .collect();
    registry
        .add_provider_config(
            CLIPROXYAPI_ID,
            json!({ "baseUrl": format!("{base}/v1"), "api": "openai-responses", "models": models }),
            Some("route-key".into()),
        )
        .expect("route");
    registry
        .add_provider_config(
            "other",
            json!({ "baseUrl": base, "api": ANTHROPIC_MESSAGES, "models": [{ "id": "claude-elsewhere" }] }),
            Some("other-key".into()),
        )
        .expect("other");
    ModelRuntime::new(registry)
}

fn route() -> Vec<(&'static str, &'static str)> {
    vec![
        ("gpt-5.5", "openai-responses"),
        ("claude-3-5-haiku-20241022", ANTHROPIC_MESSAGES),
        ("claude-sonnet-4-5", ANTHROPIC_MESSAGES),
        ("claude-haiku-4-5", ANTHROPIC_MESSAGES),
        ("claude-opus-5", ANTHROPIC_MESSAGES),
    ]
}

fn model(runtime: &ModelRuntime, provider: &str, id: &str) -> Model {
    runtime.snapshot().model(provider, id).expect("model")
}

fn selected(
    runtime: &ModelRuntime,
    current: Option<&Model>,
    configured: Option<&str>,
) -> Result<String, WebError> {
    select_search_model(&runtime.snapshot().available(), current, configured)
        .map(|model| format!("{}/{}", model.provider, model.id))
}

// (a) The setting wins, with or without the provider prefix; one not on
// the route is an error naming it and the setting.
#[test]
fn the_setting_names_the_model() {
    let runtime = registry("http://h", &route());
    let current = model(&runtime, CLIPROXYAPI_ID, "claude-opus-5");
    assert_eq!(
        selected(&runtime, Some(&current), Some("claude-sonnet-4-5")).as_deref(),
        Ok("cliproxyapi/claude-sonnet-4-5")
    );
    assert_eq!(
        selected(&runtime, None, Some("cliproxyapi/claude-opus-5")).as_deref(),
        Ok("cliproxyapi/claude-opus-5")
    );
    for configured in ["gpt-5.5", "missing", "other/claude-elsewhere"] {
        let error = selected(&runtime, Some(&current), Some(configured)).expect_err(configured);
        assert_eq!(error.code, WEB_PROVIDER_UNAVAILABLE);
        assert_eq!(
            error.message,
            format!(
                "The webSearch.model setting names \"{configured}\", which is not an Anthropic Messages model on the CLIProxyAPI route. The user can set webSearch.model in settings.json to one that is."
            )
        );
    }
}

// (b) Without the setting, the session's model when it is an Anthropic
// Messages model on the route.
#[test]
fn the_session_model_when_it_is_on_the_route() {
    let runtime = registry("http://h", &route());
    let current = model(&runtime, CLIPROXYAPI_ID, "claude-sonnet-4-5");
    assert_eq!(
        selected(&runtime, Some(&current), None).as_deref(),
        Ok("cliproxyapi/claude-sonnet-4-5")
    );
    // Not Anthropic Messages, or not on the route: (c) applies.
    for current in [
        model(&runtime, CLIPROXYAPI_ID, "gpt-5.5"),
        model(&runtime, "other", "claude-elsewhere"),
    ] {
        assert_eq!(
            selected(&runtime, Some(&current), None).as_deref(),
            Ok("cliproxyapi/claude-haiku-4-5")
        );
    }
}

// (c) Else the route's last `haiku` model, else its first Anthropic
// Messages model.
#[test]
fn the_route_last_haiku_else_its_first_anthropic_model() {
    let runtime = registry("http://h", &route());
    assert_eq!(
        selected(&runtime, None, None).as_deref(),
        Ok("cliproxyapi/claude-haiku-4-5")
    );
    let runtime = registry(
        "http://h",
        &[
            ("gpt-5.5", "openai-responses"),
            ("claude-sonnet-4-5", ANTHROPIC_MESSAGES),
            ("claude-opus-5", ANTHROPIC_MESSAGES),
        ],
    );
    assert_eq!(
        selected(&runtime, None, None).as_deref(),
        Ok("cliproxyapi/claude-sonnet-4-5")
    );
}

// (d) No Anthropic Messages model on the route.
#[test]
fn no_model_on_the_route_is_a_model_facing_error() {
    let runtime = registry("http://h", &[("gpt-5.5", "openai-responses")]);
    let error = selected(&runtime, None, None).expect_err("none");
    assert_eq!(error.code, WEB_PROVIDER_UNAVAILABLE);
    assert_eq!(
        error.message,
        "Web search runs through the CLIProxyAPI route and needs one of its Anthropic Messages models, but none is available. The user can sign in with /login, or set webSearch.model in settings.json to such a model."
    );
}

// "citationSnippets": "maps url → cited_text from text blocks, first
// occurrence wins" and "ignores citations missing url or cited_text".
#[test]
fn citation_snippets_keep_each_urls_first_excerpt() {
    let blocks = [
        json!({ "type": "text", "citations": [
            { "url": "https://a.test", "cited_text": "first" },
            { "url": "https://a.test", "cited_text": "second" },
        ] }),
        json!({ "type": "text", "citations": [{ "url": "https://b.test", "cited_text": "b text" }] }),
        json!({ "type": "text", "citations": [
            { "url": "https://c.test" }, { "cited_text": "orphan" }, { "url": "", "cited_text": "empty url" },
        ] }),
    ];
    assert_eq!(
        citation_snippets(&blocks),
        [
            ("https://a.test".to_owned(), "first".to_owned()),
            ("https://b.test".to_owned(), "b text".to_owned()),
        ]
    );
}

fn source(url: &str, title: Option<&str>) -> WebSearchSource {
    WebSearchSource {
        title: title.map(str::to_owned),
        ..WebSearchSource::new(url)
    }
}

// "mapAnthropicResponse" cases.
#[test]
fn maps_result_blocks_to_sources() {
    let joined = map_anthropic_response(&json!({ "content": [
        { "type": "text", "text": "Here is what I found.", "citations": [
            { "type": "web_search_result_location", "url": "https://a.test", "cited_text": "excerpt for A" },
        ] },
        { "type": "web_search_tool_result", "content": [
            { "type": "web_search_result", "url": "https://a.test", "title": "A", "page_age": "2026-02-02" },
            { "type": "web_search_result", "url": "https://b.test", "title": "B" },
        ] },
    ] }))
    .expect("mapped");
    assert_eq!(
        joined,
        WebSearchResult {
            content: None,
            sources: vec![
                WebSearchSource {
                    url: "https://a.test".into(),
                    title: Some("A".into()),
                    snippet: Some("excerpt for A".into()),
                    published_at: Some("2026-02-02".into()),
                },
                source("https://b.test", Some("B")),
            ],
            truncated: false,
        }
    );
    // Deduplicated across blocks, the first kept.
    let deduped = map_anthropic_response(&json!({ "content": [
        { "type": "web_search_tool_result", "content": [{ "type": "web_search_result", "url": "https://a.test", "title": "first" }] },
        { "type": "web_search_tool_result", "content": [{ "type": "web_search_result", "url": "https://a.test", "title": "second" }] },
    ] }))
    .expect("deduped");
    assert_eq!(deduped.sources, [source("https://a.test", Some("first"))]);
    // Non-result items, empty URLs, and empty optional fields skipped.
    let skipped = map_anthropic_response(&json!({ "content": [
        { "type": "text", "text": "no citations here" },
        { "type": "web_search_tool_result" },
        { "type": "web_search_tool_result", "content": [
            { "type": "web_search_result_error", "url": "https://err.test" },
            { "type": "web_search_result", "url": "" },
            { "type": "web_search_result", "url": "https://ok.test", "title": "", "page_age": "" },
        ] },
    ] }))
    .expect("skipped");
    assert_eq!(skipped.sources, [source("https://ok.test", None)]);
}

// "throws WEB_PROVIDER_ERROR (strict mode) when no result block is
// present" and "... when content is absent entirely"; and a result block
// that is an Anthropic tool error.
#[test]
fn a_response_without_results_is_an_error() {
    for response in [
        json!({ "content": [{ "type": "text", "text": "just prose, no search" }] }),
        json!({}),
        json!([1, 2]),
    ] {
        assert_eq!(
            map_anthropic_response(&response),
            Err(MapError::NoResultBlocks)
        );
    }
    let failed = json!({ "content": [{ "type": "web_search_tool_result", "content": {
        "type": "web_search_tool_result_error", "error_code": "max_uses_exceeded"
    } }] });
    assert_eq!(
        map_anthropic_response(&failed),
        Err(MapError::ToolFailed(vec!["max_uses_exceeded".into()]))
    );
}

// The gateway's own response: sources from the result block only, the
// excerpts from the text blocks' citations.
#[test]
fn maps_the_recorded_gateway_response() {
    let response: Value = serde_json::from_str(RECORDED).expect("fixture");
    let result = map_anthropic_response(&response).expect("mapped");
    assert_eq!(result.sources.len(), 10);
    assert_eq!(
        result.sources[0],
        WebSearchSource {
            url: "https://lwn.net/Articles/1038649/".into(),
            title: Some("Rust 1.90.0 released [LWN.net]".into()),
            snippet: Some("Changes include switching to the LLD linker by default, the addition of support for workspace publishing to cargo, and the usual set of stabilized API...".into()),
            published_at: Some("395 days ago".into()),
        }
    );
    // A `null` page age is no date.
    assert_eq!(
        result.sources[1],
        source(
            "https://github.com/rust-lang/rust/releases/tag/1.90.0",
            Some("Release Rust 1.90.0 · rust-lang/rust · GitHub")
        )
    );
    assert_eq!(
        result.sources[4].snippet.as_deref(),
        Some(
            "1.90.0 Released on: 18 September, 2025 Branched from master on: 1 August, 2025 Language Split up the unknown_or_malformed_diagnostic_attributes lint. "
        )
    );
    assert_eq!(result.content, None);
}

async fn gateway(steps: Vec<Step>) -> Server {
    Server::start(Arc::new(move |_path: &str, _body: &[u8]| steps.clone())).await
}

fn json_reply(status: u16, body: &str) -> Vec<Step> {
    reply(
        status,
        &[("content-type", "application/json")],
        body.as_bytes(),
    )
}

// "DeepSeekSearchProvider request mapping" > "records and posts the same
// Anthropic Messages request with the web_search server tool", through
// the route: its key in both headers, the model's base URL.
#[tokio::test]
async fn posts_the_server_tool_request_through_the_route() {
    let server = gateway(json_reply(200, RECORDED)).await;
    let runtime = registry(&server.base(), &route());
    let search = CliProxySearch::new(runtime, None, None);
    let (result, request) = search.search("rust 1.90", None).await.expect("search");
    assert_eq!(result.sources.len(), 10);
    let head = server.head(0).to_ascii_lowercase();
    assert!(head.starts_with("post /v1/messages http/1.1"), "{head}");
    assert!(head.contains("x-api-key: route-key"));
    assert!(head.contains("authorization: bearer route-key"));
    assert!(head.contains("anthropic-version: 2023-06-01"));
    let body = json!({
        "model": "claude-haiku-4-5",
        "max_tokens": 4096,
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": "Perform a web search for the query: rust 1.90" }] }],
        "tools": [{ "type": "web_search_20250305", "name": "web_search", "max_uses": 5 }],
    });
    assert_eq!(server.body(0), body);
    assert_eq!(
        request,
        SearchRequest {
            endpoint: format!("{}/v1/messages", server.base()),
            api_version: "2023-06-01".into(),
            body,
        }
    );
    assert!(!request.to_json().to_string().contains("route-key"));
}

// The chosen model's failure names the model and the setting; there is no
// second request to another model.
#[tokio::test]
async fn a_model_that_cannot_search_is_named_with_the_setting() {
    let server = gateway(json_reply(
        404,
        r#"{"type":"error","error":{"type":"not_found_error","message":"model: claude-haiku-4-5"}}"#,
    ))
    .await;
    let search = CliProxySearch::new(registry(&server.base(), &route()), None, None);
    let error = search.search("q", None).await.expect_err("not found");
    assert_eq!(error.code, WEB_PROVIDER_ERROR);
    assert_eq!(
        error.message,
        "Web search model \"cliproxyapi/claude-haiku-4-5\" could not search: CLIProxyAPI web search error (HTTP 404): model: claude-haiku-4-5. The user can set webSearch.model in settings.json to an Anthropic Messages model on the CLIProxyAPI route that supports web search."
    );
    assert_eq!(server.requests(), 1);

    // "strict mode flows through search(): a prose-only response throws".
    let server = gateway(json_reply(
        200,
        r#"{"content":[{"type":"text","text":"I cannot search."}]}"#,
    ))
    .await;
    let search = CliProxySearch::new(registry(&server.base(), &route()), None, None);
    let error = search.search("q", None).await.expect_err("prose");
    assert!(
        error.message.starts_with("Web search model \"cliproxyapi/claude-haiku-4-5\" could not search: the model returned no web_search_tool_result blocks"),
        "{}",
        error.message
    );
}

// "maps an HTTP error to WEB_PROVIDER_ERROR with the provider message",
// "keeps a status-line message when the error body is not JSON", "maps an
// unparseable success body", and "maps a network failure".
#[tokio::test]
async fn maps_gateway_failures() {
    let server = gateway(json_reply(429, r#"{"error":"model_cooldown"}"#)).await;
    let search = CliProxySearch::new(registry(&server.base(), &route()), None, None);
    assert_eq!(
        search.search("q", None).await.expect_err("429").message,
        "CLIProxyAPI web search error (HTTP 429): model_cooldown (model \"cliproxyapi/claude-haiku-4-5\")"
    );
    let server = gateway(reply(502, &[], b"<html>bad gateway</html>")).await;
    let search = CliProxySearch::new(registry(&server.base(), &route()), None, None);
    assert_eq!(
        search.search("q", None).await.expect_err("502").message,
        "CLIProxyAPI web search error (HTTP 502) (model \"cliproxyapi/claude-haiku-4-5\")"
    );
    let server = gateway(json_reply(200, "not json")).await;
    let search = CliProxySearch::new(registry(&server.base(), &route()), None, None);
    assert!(
        search
            .search("q", None)
            .await
            .expect_err("body")
            .message
            .starts_with("CLIProxyAPI returned an unprocessable response body: ")
    );
    let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!(
        "http://127.0.0.1:{}",
        closed.local_addr().expect("address").port()
    );
    drop(closed);
    let search = CliProxySearch::new(registry(&base, &route()), None, None);
    let error = search.search("q", None).await.expect_err("refused");
    assert_eq!(error.code, WEB_PROVIDER_ERROR);
    assert!(
        error
            .message
            .starts_with("CLIProxyAPI web search request failed: ")
    );
}

// "maps an abort to WEB_ABORTED" and "does not start ... dispatch for a
// pre-aborted call".
#[tokio::test]
async fn honors_cancellation() {
    let server = gateway(vec![Step::Sleep(std::time::Duration::from_secs(10))]).await;
    let search = CliProxySearch::new(registry(&server.base(), &route()), None, None);
    let controller = bake_ai::AbortController::new();
    controller.abort();
    let error = search
        .search("q", Some(&controller.signal()))
        .await
        .expect_err("pre-aborted");
    assert_eq!(
        (error.code, error.message.as_str()),
        (WEB_ABORTED, "web search aborted")
    );
    assert_eq!(server.requests(), 0);
    let controller = bake_ai::AbortController::new();
    let signal = controller.signal();
    let abort = async {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        controller.abort();
    };
    let (result, ()) = tokio::join!(search.search("q", Some(&signal)), abort);
    assert_eq!(result.expect_err("aborted").code, WEB_ABORTED);
}
