//! Request-building and response-shaping tests ported from Pi's
//! network-free protocol tests (v1.1.0).
//!
//! Each test names the Pi file and case it follows. Pi builds models from
//! its provider catalog, which Bake does not port; these tests write the
//! catalog entry's relevant fields out instead. Where Pi mocks the SDK, the
//! request body is built directly or read from a loopback server the test
//! owns.

use serde_json::{Value, json};

use crate::api::simple_options::build_base_options;
use crate::api::{
    anthropic_messages, openai_completions, openai_responses, openai_responses_shared,
};
use crate::options::{SimpleStreamOptions, StreamOptions};
use crate::test_server::{Reply, serve, sse_data, sse_events};
use crate::transcript::normalize_context;
use crate::types::{
    AssistantContentBlock, CacheRetention, Context, Message, Model, StopReason, ThinkingLevel,
    Tool, TranscriptContext,
};
use crate::utils::hash::short_hash;

fn model_from(value: Value) -> Model {
    serde_json::from_value(value).expect("a valid model")
}

/// Pi's `getModel("openai", "gpt-4o-mini")` without its compat, as the
/// completions tests use it.
fn gpt_4o_mini(api: &str) -> Model {
    model_from(json!({
        "id": "gpt-4o-mini",
        "name": "GPT-4o mini",
        "api": api,
        "provider": "openai",
        "baseUrl": "https://api.openai.com/v1",
        "reasoning": false,
        "input": ["text", "image"],
        "cost": { "input": 0.15, "output": 0.6, "cacheRead": 0.08, "cacheWrite": 0 },
        "contextWindow": 128000,
        "maxTokens": 16384
    }))
}

fn anthropic_model(id: &str, compat: Value, thinking_level_map: Value) -> Model {
    let mut value = json!({
        "id": id,
        "name": id,
        "api": "anthropic-messages",
        "provider": "anthropic",
        "baseUrl": "https://api.anthropic.com",
        "reasoning": true,
        "input": ["text", "image"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 200000,
        "maxTokens": 32000
    });
    if !compat.is_null() {
        value["compat"] = compat;
    }
    if !thinking_level_map.is_null() {
        value["thinkingLevelMap"] = thinking_level_map;
    }
    model_from(value)
}

fn messages(values: Vec<Value>) -> Vec<Message> {
    values
        .into_iter()
        .map(|value| serde_json::from_value(value).expect("a valid message"))
        .collect()
}

fn context(
    system: Option<&str>,
    values: Vec<Value>,
    tools: Option<Vec<Tool>>,
) -> TranscriptContext {
    normalize_context(Context {
        system_prompt: system.map(Into::into),
        messages: messages(values),
        tools,
    })
}

fn user(text: &str) -> Value {
    json!({ "role": "user", "content": text, "timestamp": 1 })
}

fn empty_usage() -> Value {
    json!({
        "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
    })
}

fn assistant(content: Value, api: &str, provider: &str, model: &str, stop: &str) -> Value {
    json!({
        "role": "assistant",
        "content": content,
        "api": api,
        "provider": provider,
        "model": model,
        "usage": empty_usage(),
        "stopReason": stop,
        "timestamp": 2
    })
}

fn tool_result(id: &str, name: &str, content: Value) -> Value {
    json!({
        "role": "toolResult",
        "toolCallId": id,
        "toolName": name,
        "content": content,
        "isError": false,
        "timestamp": 3
    })
}

fn tool(name: &str, parameters: Value, constrained: Option<Value>) -> Tool {
    let mut value = json!({ "name": name, "description": "A tool", "parameters": parameters });
    if let Some(constrained) = constrained {
        value["constrainedSampling"] = constrained;
    }
    serde_json::from_value(value).expect("a valid tool")
}

// ---- openai-completions: openai-completions-tool-result-images.test.ts ----

// "omits empty text parts from user messages with images"
#[test]
fn completions_omit_empty_text_parts_beside_images() {
    let model = gpt_4o_mini("openai-completions");
    let context = context(
        None,
        vec![json!({
            "role": "user",
            "content": [
                { "type": "text", "text": "" },
                { "type": "image", "data": "ZmFrZQ==", "mimeType": "image/png" }
            ],
            "timestamp": 1
        })],
        None,
    );
    let compat = openai_completions::get_compat(&model);
    assert_eq!(
        openai_completions::convert_messages(&model, &context, &compat).unwrap(),
        vec![json!({
            "role": "user",
            "content": [{ "type": "image_url", "image_url": { "url": "data:image/png;base64,ZmFrZQ==" } }]
        })]
    );
}

// "batches tool-result images after consecutive tool results"
#[test]
fn completions_batch_tool_result_images_after_the_tool_results() {
    let model = gpt_4o_mini("openai-completions");
    let image_result = |id: &str| {
        tool_result(
            id,
            "read",
            json!([
                { "type": "text", "text": "Read image file [image/png]" },
                { "type": "image", "data": "ZmFrZQ==", "mimeType": "image/png" }
            ]),
        )
    };
    let context = context(
        None,
        vec![
            user("Read the images"),
            assistant(
                json!([
                    { "type": "toolCall", "id": "tool-1", "name": "read", "arguments": { "path": "img-1.png" } },
                    { "type": "toolCall", "id": "tool-2", "name": "read", "arguments": { "path": "img-2.png" } }
                ]),
                "openai-completions",
                "openai",
                "gpt-4o-mini",
                "toolUse",
            ),
            image_result("tool-1"),
            image_result("tool-2"),
        ],
        None,
    );
    let compat = openai_completions::get_compat(&model);
    let converted = openai_completions::convert_messages(&model, &context, &compat).unwrap();
    let roles: Vec<&str> = converted
        .iter()
        .map(|message| message["role"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(roles, ["user", "assistant", "tool", "tool", "user"]);
    let images = converted
        .last()
        .and_then(|message| message["content"].as_array())
        .map(|parts| {
            parts
                .iter()
                .filter(|part| part["type"] == "image_url")
                .count()
        });
    assert_eq!(images, Some(2));
}

// "uses '(no tool output)' placeholder for empty tool results without images"
#[test]
fn completions_send_a_placeholder_for_empty_tool_results() {
    let model = gpt_4o_mini("openai-completions");
    let context = context(
        None,
        vec![
            user("Run the command"),
            assistant(
                json!([{ "type": "toolCall", "id": "tool-1", "name": "bash", "arguments": { "command": "true" } }]),
                "openai-completions",
                "openai",
                "gpt-4o-mini",
                "toolUse",
            ),
            tool_result("tool-1", "bash", json!([{ "type": "text", "text": "" }])),
        ],
        None,
    );
    let compat = openai_completions::get_compat(&model);
    let converted = openai_completions::convert_messages(&model, &context, &compat).unwrap();
    let tool = converted.iter().find(|message| message["role"] == "tool");
    assert_eq!(
        tool.map(|message| &message["content"]),
        Some(&json!("(no tool output)"))
    );
}

// ---- openai-completions: openai-completions-empty-tools.test.ts ----

fn completions_params(
    model: &Model,
    context: &TranscriptContext,
    max_tokens: Option<u64>,
) -> Value {
    let simple = SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some("test".into()),
            max_tokens,
            ..StreamOptions::default()
        },
        ..SimpleStreamOptions::default()
    };
    let options = openai_completions::OpenAICompletionsOptions {
        base: build_base_options(model, context, &simple),
        ..Default::default()
    };
    let compat = openai_completions::get_compat(model);
    openai_completions::build_params(model, context, &options, &compat, CacheRetention::Short)
        .unwrap()
}

// "omits tools field when context.tools is an empty array" and "... is
// undefined"
#[test]
fn completions_omit_an_empty_tools_field() {
    let model = gpt_4o_mini("openai-completions");
    for tools in [Some(Vec::new()), None] {
        let params = completions_params(&model, &context(None, vec![user("hi")], tools), None);
        assert!(params.get("tools").is_none(), "{params}");
    }
}

// "sends default maxTokens", "sends explicit maxTokens", and the two
// "clamps ... maxTokens to remaining context" cases
#[test]
fn completions_send_and_clamp_max_tokens() {
    let model = gpt_4o_mini("openai-completions");
    let short = context(None, vec![user("hi")], None);
    let params = completions_params(&model, &short, None);
    assert!(params.get("max_tokens").is_none());
    assert_eq!(params["max_completion_tokens"], json!(model.max_tokens));
    let params = completions_params(&model, &short, Some(1234));
    assert_eq!(params["max_completion_tokens"], json!(1234));

    let mut small = gpt_4o_mini("openai-completions");
    small.context_window = 10_000;
    small.max_tokens = 8000;
    let long = context(None, vec![user(&"x".repeat(8000))], None);
    for explicit in [None, Some(7000)] {
        let params = completions_params(&small, &long, explicit);
        assert!(params.get("max_tokens").is_none());
        assert_eq!(params["max_completion_tokens"], json!(3618));
    }
}

// "still emits tools: [] for Anthropic/LiteLLM proxy when conversation has
// tool history"
#[test]
fn completions_keep_an_empty_tools_list_with_tool_history() {
    let model = gpt_4o_mini("openai-completions");
    let context = context(
        None,
        vec![
            user("use the tool"),
            assistant(
                json!([{ "type": "toolCall", "id": "t1", "name": "noop", "arguments": {} }]),
                "openai-completions",
                "openai",
                "gpt-4o-mini",
                "toolUse",
            ),
            tool_result("t1", "noop", json!([{ "type": "text", "text": "done" }])),
        ],
        Some(Vec::new()),
    );
    assert_eq!(
        completions_params(&model, &context, None)["tools"],
        json!([])
    );
}

// ---- openai-completions: response shaping over loopback ----

async fn completions_result(model: &Model, chunks: &[Value]) -> crate::types::AssistantMessage {
    let server = serve(vec![Reply::sse(sse_data(chunks))]).await;
    let mut model = model.clone();
    model.base_url = format!("{}/v1", server.base);
    let stream = openai_completions::stream(
        &model,
        &context(None, vec![user("hello")], None),
        openai_completions::OpenAICompletionsOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                max_retries: Some(0),
                ..StreamOptions::default()
            },
            ..Default::default()
        },
    );
    stream.result().await.expect("a final message")
}

// openai-completions-raw-stop-reason.test.ts: "preserves raw finish reasons
// for successful stops" and "... for provider error stops"
#[tokio::test]
async fn completions_keep_the_raw_finish_reason() {
    let model = gpt_4o_mini("openai-completions");
    let message = completions_result(
        &model,
        &[json!({ "id": "chatcmpl-1", "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }] })],
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("stop"));
    assert_eq!(message.error_message, None);

    let message = completions_result(
        &model,
        &[json!({ "id": "chatcmpl-2", "choices": [{ "index": 0, "delta": {}, "finish_reason": "content_filter" }] })],
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("content_filter"));
    assert_eq!(
        message.error_message.as_deref(),
        Some("Provider finish_reason: content_filter")
    );
}

// openai-completions-response-model.test.ts: all three cases
#[tokio::test]
async fn completions_surface_a_routed_response_model() {
    let mut model = gpt_4o_mini("openai-completions");
    model.id = "openrouter/auto".into();
    model.provider = "openrouter".into();
    let usage = json!({ "prompt_tokens": 10, "completion_tokens": 5, "prompt_tokens_details": { "cached_tokens": 0 } });
    let routed = completions_result(
        &model,
        &[
            json!({ "id": "c1", "model": "anthropic/claude-opus-4.8", "choices": [{ "index": 0, "delta": { "content": "hi" } }] }),
            json!({ "id": "c1", "model": "anthropic/claude-opus-4.8", "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }], "usage": usage }),
        ],
    )
    .await;
    assert_eq!(routed.model, "openrouter/auto");
    assert_eq!(
        routed.response_model.as_deref(),
        Some("anthropic/claude-opus-4.8")
    );
    assert_eq!(routed.provider, "openrouter");
    assert_eq!(routed.stop_reason, StopReason::Stop);

    let echoed = completions_result(
        &model,
        &[
            json!({ "id": "c2", "model": "openrouter/auto", "choices": [{ "index": 0, "delta": { "content": "hi" } }] }),
            json!({ "id": "c2", "model": "openrouter/auto", "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }], "usage": usage }),
        ],
    )
    .await;
    assert_eq!(echoed.response_model, None);

    let missing = completions_result(
        &model,
        &[
            json!({ "id": "c3", "choices": [{ "index": 0, "delta": { "content": "hi" } }] }),
            json!({ "id": "c3", "model": "", "choices": [{ "index": 0, "delta": { "content": "!" } }] }),
            json!({ "id": "c3", "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }], "usage": usage }),
        ],
    )
    .await;
    assert_eq!(missing.model, "openrouter/auto");
    assert_eq!(missing.response_model, None);
}

// Bake's no-panic rule: provider token counts near `u64::MAX` saturate.
#[tokio::test]
async fn completions_saturate_huge_token_counts() {
    let model = gpt_4o_mini("openai-completions");
    let message = completions_result(
        &model,
        &[
            json!({ "id": "c", "choices": [{ "index": 0, "delta": { "content": "hi" } }] }),
            json!({ "id": "c", "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
                    "usage": { "prompt_tokens": 5, "completion_tokens": u64::MAX } }),
        ],
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.usage.total_tokens, u64::MAX);
}

// ---- openai-responses ----

const OPENAI_TOOL_CALL_PROVIDERS: [&str; 3] = ["openai", "openai-codex", "opencode"];

fn codex_model() -> Model {
    model_from(json!({
        "id": "gpt-5.5",
        "name": "GPT-5.5",
        "api": "openai-codex-responses",
        "provider": "openai-codex",
        "baseUrl": "https://chatgpt.com/backend-api",
        "reasoning": true,
        "input": ["text", "image"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 272000,
        "maxTokens": 128000
    }))
}

fn responses_input(model: &Model, context: &TranscriptContext) -> Vec<Value> {
    openai_responses_shared::convert_responses_messages(
        model,
        context,
        &OPENAI_TOOL_CALL_PROVIDERS.into_iter().collect(),
        &openai_responses_shared::ConvertResponsesMessagesOptions::default(),
    )
    .unwrap()
}

// openai-responses-foreign-toolcall-id.test.ts: "hashes foreign Copilot tool
// item IDs into a bounded Codex-safe fc_<hash> shape"
#[test]
fn responses_hash_foreign_tool_item_ids() {
    let item = "I9b95oN1wD/cHXKTw3PpRkL6KkCtzTJhUxMouMWYwHeTo2j3htzfSk7YPx2vifiIM4g3A8XXyOj8q4Bt6SLUG7gqY1E3ELkrkVQNHglRfUmWj84lqxJY+Puieb3VKyX0FB+83TUzn91cDMF/4gzt990IzqVrc+nIb9RRscRD070Du16q1glydVjWR0SBJsE6TbY/esOjFpqplogQqrajm1eI++f3eLi73R6q7hVusY0QbeFySVxABCjhN0lXB04caBe1rzHjYzul6MAXj7uq+0r17VLq+yrtyYhN12wkmFqHeqTyEei6EFPbMy24Nc+IbJlkP0OCg02W+gOnyBFcbi2ctvJFSOhSjt1CqBdqCnnhwUqXjbWiT0wh3DmLScRgTHmGkaI+oAcQQjfic65nxj+TnEkReA==";
    let raw = format!("call_4VnzVawQXPB9MgYib7CiQFEY|{item}");
    let model = codex_model();
    let context = context(
        Some("You are concise."),
        vec![
            user("Use the tool."),
            assistant(
                json!([{ "type": "toolCall", "id": raw, "name": "edit", "arguments": { "path": "src/styles/app.css" } }]),
                "openai-responses",
                "github-copilot",
                "gpt-5.5",
                "toolUse",
            ),
            tool_result(&raw, "edit", json!([{ "type": "text", "text": "ok" }])),
        ],
        None,
    );
    let input = responses_input(&model, &context);
    let call = input
        .iter()
        .find(|item| item["type"] == "function_call")
        .expect("a function_call item");
    let id = call["id"].as_str().unwrap_or_default();
    assert_eq!(id, format!("fc_{}", short_hash(item)));
    assert!(id.len() <= 64);
    assert!(
        id.strip_prefix("fc_")
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric())),
        "{id}"
    );
}

// openai-responses-message-id.test.ts: "generates unique fallback message
// IDs for multiple text blocks in one assistant turn"
#[test]
fn responses_number_fallback_message_ids() {
    let model = codex_model();
    let context = context(
        Some("You are concise."),
        vec![
            user("hello"),
            assistant(
                json!([
                    { "type": "thinking", "thinking": "private reasoning" },
                    { "type": "text", "text": "visible answer" }
                ]),
                "anthropic-messages",
                "anthropic",
                "claude-opus-4-8",
                "stop",
            ),
        ],
        None,
    );
    let ids: Vec<String> = responses_input(&model, &context)
        .iter()
        .filter(|item| item["type"] == "message")
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    assert_eq!(ids, ["msg_pi_1", "msg_pi_1_1"]);
}

// openai-responses-empty-tool-result.test.ts: "uses '(no tool output)'
// placeholder for empty tool results without images"
#[test]
fn responses_send_a_placeholder_for_empty_tool_results() {
    let model = gpt_4o_mini("openai-responses");
    let context = context(
        None,
        vec![
            user("Run the command"),
            assistant(
                json!([{ "type": "toolCall", "id": "tool-1", "name": "bash", "arguments": { "command": "true" } }]),
                "openai-responses",
                "openai",
                "gpt-4o-mini",
                "toolUse",
            ),
            tool_result("tool-1", "bash", json!([{ "type": "text", "text": "" }])),
        ],
        None,
    );
    let input = responses_input(&model, &context);
    let output = input
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .map(|item| &item["output"]);
    assert_eq!(output, Some(&json!("(no tool output)")));
}

async fn responses_error(reply: Reply) -> String {
    let server = serve(vec![reply]).await;
    let model = model_from(json!({
        "id": "gpt-5-mini",
        "name": "GPT-5 Mini",
        "api": "openai-responses",
        "provider": "openai",
        "baseUrl": format!("{}/v1", server.base),
        "reasoning": true,
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 400000,
        "maxTokens": 128000
    }));
    let message = openai_responses::stream(
        &model,
        &context(Some(""), vec![user("hi")], Some(Vec::new())),
        openai_responses::OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                max_retries: Some(0),
                ..StreamOptions::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("a final message");
    assert_eq!(message.stop_reason, StopReason::Error);
    message.error_message.unwrap_or_default()
}

// openai-responses-usage-limit.test.ts: both cases
#[tokio::test]
async fn responses_link_chatgpt_usage_on_usage_limits() {
    let limit = json!({ "code": "subscription_sharing_usage_limit_exceeded", "message": "Usage limit reached." });
    let rejected = responses_error(Reply::error(
        429,
        json!({ "error": { "code": limit["code"], "message": limit["message"], "type": "rate_limit_error" } })
            .to_string(),
    ))
    .await;
    assert!(
        rejected.contains("subscription_sharing_usage_limit_exceeded"),
        "{rejected}"
    );
    assert!(
        rejected.contains("Check your ChatGPT usage: https://chatgpt.com/settings/usage"),
        "{rejected}"
    );

    let failed = responses_error(Reply::sse(sse_events(&[(
        "response.failed",
        json!({ "type": "response.failed", "sequence_number": 0,
                "response": { "id": "resp_failed", "status": "failed", "error": limit } }),
    )])))
    .await;
    assert!(
        failed.contains("subscription_sharing_usage_limit_exceeded: Usage limit reached."),
        "{failed}"
    );
    assert!(
        failed.contains("Check your ChatGPT usage: https://chatgpt.com/settings/usage"),
        "{failed}"
    );
}

// ---- anthropic-messages ----

fn anthropic_params(
    model: &Model,
    context: &TranscriptContext,
    options: anthropic_messages::AnthropicOptions,
) -> Value {
    anthropic_messages::build_params(model, context, false, &options).unwrap()
}

fn with_temperature(temperature: f64) -> anthropic_messages::AnthropicOptions {
    anthropic_messages::AnthropicOptions {
        base: StreamOptions {
            temperature: Some(temperature),
            ..StreamOptions::default()
        },
        ..Default::default()
    }
}

// anthropic-temperature-compat.test.ts. Pi's catalog sets
// `supportsTemperature: false` on Opus 4.7 and later; the custom-model case
// sets it by hand.
#[test]
fn anthropic_omits_temperature_when_unsupported() {
    let context = context(None, vec![user("Hello")], None);
    let unsupported = anthropic_model(
        "vendor--claude-opus-4-7",
        json!({ "supportsTemperature": false }),
        Value::Null,
    );
    for temperature in [0.0, 1.0] {
        let params = anthropic_params(&unsupported, &context, with_temperature(temperature));
        assert!(params.get("temperature").is_none(), "{params}");
    }
    let supported = anthropic_model("claude-sonnet-4-6", Value::Null, Value::Null);
    let params = anthropic_params(&supported, &context, with_temperature(0.0));
    assert_eq!(params["temperature"], json!(0.0));
}

fn strict_model() -> Model {
    anthropic_model(
        "claude-opus-4-8",
        json!({ "forceAdaptiveThinking": true, "supportsStrictTools": true }),
        Value::Null,
    )
}

fn first_tool(tool: Tool) -> Value {
    let context = context(None, vec![user("Use the tool")], Some(vec![tool]));
    let options = anthropic_messages::AnthropicOptions {
        base: StreamOptions {
            cache_retention: Some(CacheRetention::None),
            ..StreamOptions::default()
        },
        ..Default::default()
    };
    anthropic_params(&strict_model(), &context, options)["tools"][0].clone()
}

fn prefer() -> Option<Value> {
    Some(json!({ "type": "json_schema", "strict": "prefer" }))
}

// anthropic-strict-tool-schema.test.ts: "only sends the full input schema
// for strict JSON-schema tools"
#[test]
fn anthropic_sends_the_full_schema_only_for_strict_tools() {
    let legacy = first_tool(tool(
        "lookup",
        json!({ "type": "object", "title": "LookupInput", "additionalProperties": false,
                "properties": { "value": { "type": "string" } }, "required": ["value"] }),
        None,
    ));
    assert!(legacy.get("strict").is_none());
    assert_eq!(
        legacy["input_schema"],
        json!({ "type": "object", "properties": { "value": { "type": "string" } }, "required": ["value"] })
    );

    let strict = first_tool(tool(
        "lookup",
        json!({ "type": "object", "title": "StrictLookupInput",
                "properties": { "value": { "type": "string" }, "optional": { "type": "number" } },
                "required": ["value"] }),
        prefer(),
    ));
    assert_eq!(strict["strict"], json!(true));
    let schema = &strict["input_schema"];
    assert_eq!(schema["additionalProperties"], json!(false));
    assert_eq!(schema["required"], json!(["value", "optional"]));
    assert_eq!(
        schema["properties"]["optional"],
        json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] })
    );
    assert_eq!(schema["title"], json!("StrictLookupInput"));
}

// anthropic-strict-tool-schema.test.ts: "sends prefer tools non-strict when
// they use keywords Anthropic strict mode rejects"
#[test]
fn anthropic_sends_unsupported_strict_schemas_non_strict() {
    for parameters in [
        json!({ "type": "object", "properties": { "timeoutMs": { "type": "integer", "minimum": 1, "maximum": 300000 } } }),
        json!({ "type": "object", "properties": { "options": { "type": "object",
                "properties": { "tags": { "type": "array", "items": { "type": "string" }, "minItems": 2 } },
                "required": ["tags"] } }, "required": ["options"] }),
        json!({ "type": "object", "properties": { "expression": { "type": "string", "format": "regex" } }, "required": ["expression"] }),
    ] {
        let converted = first_tool(tool("lookup", parameters, prefer()));
        assert!(converted.get("strict").is_none(), "{converted}");
    }
    let supported = first_tool(tool(
        "lookup",
        json!({ "type": "object", "properties": {
            "code": { "type": "string", "minLength": 1, "maxLength": 1000, "pattern": "^[a-z]+$" },
            "url": { "type": "string", "format": "uri" },
            "tags": { "type": "array", "items": { "type": "string" }, "minItems": 1 }
        }, "required": ["code", "url", "tags"] }),
        prefer(),
    ));
    assert_eq!(supported["strict"], json!(true));
}

/// The body `stream_simple` sends, read from a loopback server.
async fn anthropic_simple_body(
    model: &Model,
    context: &TranscriptContext,
    reasoning: Option<ThinkingLevel>,
) -> Value {
    let server = serve(vec![Reply::error(400, "{}")]).await;
    let mut model = model.clone();
    model.base_url = server.base.clone();
    let options = SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some("fake-key".into()),
            max_retries: Some(0),
            ..StreamOptions::default()
        },
        reasoning,
        ..SimpleStreamOptions::default()
    };
    let _ = anthropic_messages::stream_simple(&model, context, options)
        .result()
        .await;
    server
        .requests()
        .first()
        .map(|request| request.body.clone())
        .expect("one request")
}

// anthropic-thinking-disable.test.ts: the payload cases. The models carry
// the catalog's `forceAdaptiveThinking` and `thinkingLevelMap` entries.
#[tokio::test]
async fn anthropic_disables_or_adapts_thinking() {
    let context = context(None, vec![user("Hello")], None);
    let budget = anthropic_model("claude-sonnet-4-5", Value::Null, Value::Null);
    let adaptive = anthropic_model(
        "claude-opus-4-8",
        json!({ "forceAdaptiveThinking": true }),
        json!({ "xhigh": "xhigh" }),
    );
    let fable = anthropic_model(
        "claude-fable-5",
        json!({ "forceAdaptiveThinking": true }),
        json!({ "off": null, "xhigh": "xhigh", "max": "max" }),
    );
    for model in [&budget, &adaptive] {
        let body = anthropic_simple_body(model, &context, None).await;
        assert_eq!(
            body["thinking"],
            json!({ "type": "disabled" }),
            "{}",
            model.id
        );
        assert!(body.get("output_config").is_none());
    }
    let body = anthropic_simple_body(&fable, &context, None).await;
    assert!(body.get("thinking").is_none(), "{body}");
    assert!(body.get("output_config").is_none());
    for (level, effort) in [
        (ThinkingLevel::High, "high"),
        (ThinkingLevel::Xhigh, "xhigh"),
    ] {
        let body = anthropic_simple_body(&adaptive, &context, Some(level)).await;
        assert_eq!(
            body["thinking"],
            json!({ "type": "adaptive", "display": "summarized" })
        );
        assert_eq!(body["output_config"], json!({ "effort": effort }));
    }
}

fn signature_context(signature: &str, thinking: &str, extra_text: bool) -> TranscriptContext {
    let mut content =
        vec![json!({ "type": "thinking", "thinking": thinking, "thinkingSignature": signature })];
    if extra_text {
        content.push(json!({ "type": "text", "text": "answer" }));
    }
    context(
        None,
        vec![
            user("first"),
            assistant(
                Value::Array(content),
                "anthropic-messages",
                "xiaomi-token-plan-ams",
                "mimo-v2.5-pro",
                "stop",
            ),
            user("second"),
        ],
        None,
    )
}

fn assistant_content(model: &Model, context: &TranscriptContext) -> Value {
    let params = anthropic_params(
        model,
        context,
        anthropic_messages::AnthropicOptions::default(),
    );
    params["messages"]
        .as_array()
        .and_then(|messages| {
            messages
                .iter()
                .find(|message| message["role"] == "assistant")
        })
        .map(|message| message["content"].clone())
        .unwrap_or_default()
}

// anthropic-empty-thinking-signature-compat.test.ts: the three cases that
// do not read Pi's catalog
#[test]
fn anthropic_replays_empty_thinking_signatures_by_compat() {
    let mut model = anthropic_model("mimo-v2.5-pro", Value::Null, Value::Null);
    model.provider = "xiaomi-token-plan-ams".into();
    assert_eq!(
        assistant_content(&model, &signature_context("", "internal reasoning", false)),
        json!([{ "type": "text", "text": "internal reasoning" }])
    );
    assert_eq!(
        assistant_content(&model, &signature_context("signed-thinking", "", false)),
        json!([{ "type": "thinking", "thinking": "", "signature": "signed-thinking" }])
    );
    model.compat = Some(serde_json::from_value(json!({ "allowEmptySignature": true })).unwrap());
    assert_eq!(
        assistant_content(&model, &signature_context(" ", "internal reasoning", false)),
        json!([{ "type": "thinking", "thinking": "internal reasoning", "signature": "" }])
    );
    // "preserves unsigned thinking for Fireworks %s", with the compat flag the
    // catalog gives those models.
    assert_eq!(
        assistant_content(&model, &signature_context("", "internal reasoning", true)),
        json!([
            { "type": "thinking", "thinking": "internal reasoning", "signature": "" },
            { "type": "text", "text": "answer" }
        ])
    );
}

// anthropic-tool-name-normalization.test.ts. Pi runs these cases live with an
// OAuth token; here a loopback server checks the outbound name and replies
// with a tool call in Claude Code casing.
#[tokio::test]
async fn anthropic_oauth_tool_names_round_trip() {
    for (name, wire) in [
        ("todowrite", "TodoWrite"),
        ("read", "Read"),
        ("find", "find"),
        ("my_custom_tool", "my_custom_tool"),
    ] {
        let body = sse_events(&[
            (
                "message_start",
                json!({ "type": "message_start", "message": { "id": "msg_1", "usage": { "input_tokens": 1, "output_tokens": 0 } } }),
            ),
            (
                "content_block_start",
                json!({ "type": "content_block_start", "index": 0,
                "content_block": { "type": "tool_use", "id": "toolu_1", "name": wire, "input": {} } }),
            ),
            (
                "content_block_delta",
                json!({ "type": "content_block_delta", "index": 0,
                "delta": { "type": "input_json_delta", "partial_json": "{}" } }),
            ),
            (
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": 0 }),
            ),
            (
                "message_delta",
                json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" }, "usage": { "output_tokens": 1 } }),
            ),
            ("message_stop", json!({ "type": "message_stop" })),
        ]);
        let server = serve(vec![Reply::sse(body)]).await;
        let mut model = anthropic_model("claude-sonnet-4-6", Value::Null, Value::Null);
        model.base_url = server.base.clone();
        let context = context(
            Some("You are a helpful assistant."),
            vec![user("Use the tool.")],
            Some(vec![tool(
                name,
                json!({ "type": "object", "properties": {} }),
                None,
            )]),
        );
        let message = anthropic_messages::stream(
            &model,
            &context,
            anthropic_messages::AnthropicOptions {
                base: StreamOptions {
                    api_key: Some("sk-ant-oat-test".into()),
                    max_retries: Some(0),
                    ..StreamOptions::default()
                },
                ..Default::default()
            },
        )
        .result()
        .await
        .expect("a final message");
        let sent = server
            .requests()
            .first()
            .map(|request| request.body["tools"][0]["name"].clone());
        assert_eq!(sent, Some(json!(wire)), "{name}");
        assert_eq!(
            message.stop_reason,
            StopReason::ToolUse,
            "{:?}",
            message.error_message
        );
        let returned = message.content.iter().find_map(|block| match block {
            AssistantContentBlock::ToolCall(call) => Some(call.name.clone()),
            _ => None,
        });
        assert_eq!(returned.as_deref(), Some(name));
    }
}

// Bake's no-panic rule: provider token counts near `u64::MAX` saturate.
#[tokio::test]
async fn anthropic_saturates_huge_token_counts() {
    let body = sse_events(&[
        (
            "message_start",
            json!({ "type": "message_start", "message": { "id": "msg_1",
            "usage": { "input_tokens": u64::MAX, "output_tokens": 0, "cache_read_input_tokens": 5 } } }),
        ),
        (
            "content_block_start",
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        ),
        (
            "content_block_delta",
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "hi" } }),
        ),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }),
        ),
        (
            "message_delta",
            json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": u64::MAX } }),
        ),
        ("message_stop", json!({ "type": "message_stop" })),
    ]);
    let server = serve(vec![Reply::sse(body)]).await;
    let mut model = anthropic_model("claude-sonnet-4-6", Value::Null, Value::Null);
    model.base_url = server.base.clone();
    let message = anthropic_messages::stream(
        &model,
        &context(None, vec![user("hi")], None),
        anthropic_messages::AnthropicOptions {
            base: StreamOptions {
                api_key: Some("sk-test".into()),
                max_retries: Some(0),
                ..StreamOptions::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("a final message");
    assert_eq!(
        message.stop_reason,
        StopReason::Stop,
        "{:?}",
        message.error_message
    );
    assert_eq!(message.usage.total_tokens, u64::MAX);
}
