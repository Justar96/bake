use serde_json::json;

use super::*;
use crate::cliproxyapi::catalog::cli_proxy_models;

fn login_route(base_url: &str, catalog: Value) -> CliProxyRoute {
    let endpoints = cli_proxy_endpoints(base_url).expect("a valid proxy URL");
    let models = cli_proxy_models(&catalog, Some(&endpoints.root)).expect("a model list");
    CliProxyRoute::from_login(&endpoints, &models)
}

// cliproxyapi.test.ts: "saves a validated URL and model route without placing
// the key in settings", the settings half: the `/v1` base URL over Responses,
// Claude on the proxy root, the multi-credential defaults, and no key.
#[test]
fn saves_a_validated_url_and_model_route_without_placing_the_key_in_settings() {
    let route = login_route(
        "https://proxy.example/v1",
        json!({ "models": [{ "slug": "gpt-test" }, { "slug": "claude-test", "owned_by": "anthropic" }] }),
    );
    let saved = route.to_settings_json();
    assert_eq!(
        saved,
        json!({
            "displayName": "CLIProxyAPI", "apiKeyEnv": "CLIPROXYAPI_API_KEY", "api": "openai-responses",
            "baseURL": "https://proxy.example/v1",
            "models": [
                { "id": "gpt-test", "name": "gpt-test" },
                { "id": "claude-test", "api": "anthropic-messages", "baseURL": "https://proxy.example",
                  "name": "claude-test" },
            ],
            "retryPolicy": { "mode": "normal", "backoff": { "maxDelayMs": 60_000 } },
            "compat": { "sendSessionAffinityHeaders": true },
        })
    );
    assert_eq!(
        CLIPROXYAPI_ROUTE_DEFAULTS.to_json(),
        json!({
            "retryPolicy": { "mode": "normal", "backoff": { "maxDelayMs": 60_000 } },
            "compat": { "sendSessionAffinityHeaders": true },
        })
    );
    // Reading back what a login saved gives the same route.
    assert_eq!(CliProxyRoute::from_settings(&saved), Ok(route));
}

/// The route `/login cliproxyapi` wrote before 0.1.7: every model on the
/// route's Responses protocol, and no multi-account defaults.
fn legacy() -> Value {
    json!({
        "displayName": "CLIProxyAPI", "apiKeyEnv": "CLIPROXYAPI_API_KEY", "api": "openai-responses",
        "baseURL": "https://proxy.example/v1",
        "models": [
            { "id": "gpt-test", "name": "GPT" },
            { "id": "claude-test", "name": "Claude", "reasoningEfforts": { "high": "high", "max": "max" } },
            { "id": "glm-test", "name": "GLM" },
        ],
    })
}

fn path(keys: &[&str]) -> Vec<String> {
    keys.iter().map(|key| (*key).to_owned()).collect()
}

// cliproxyapi.test.ts: "upgrading a route an earlier login wrote" > "fills
// exactly what the current login writes, from the saved route alone".
#[test]
fn fills_exactly_what_the_current_login_writes_from_the_saved_route_alone() {
    let plan = plan_cli_proxy_route_upgrade(&legacy()).expect("an upgrade");
    assert_eq!(
        plan.changes,
        [
            CliProxyRouteChange::Protocols,
            CliProxyRouteChange::AdaptiveThinking,
            CliProxyRouteChange::Retry,
            CliProxyRouteChange::Affinity,
        ]
    );
    assert_eq!(
        plan.ops,
        vec![
            (
                path(&["providers", "cliproxyapi", "models"]),
                json!([
                    { "id": "gpt-test", "name": "GPT" },
                    { "id": "claude-test", "name": "Claude", "reasoningEfforts": { "high": "high", "max": "max" },
                      "api": "anthropic-messages", "baseURL": "https://proxy.example",
                      "compat": { "forceAdaptiveThinking": true } },
                    { "id": "glm-test", "name": "GLM", "api": "openai-completions" },
                ])
            ),
            (
                path(&["providers", "cliproxyapi", "retryPolicy"]),
                json!({ "mode": "normal", "backoff": { "maxDelayMs": 60_000 } })
            ),
            (
                path(&[
                    "providers",
                    "cliproxyapi",
                    "compat",
                    "sendSessionAffinityHeaders"
                ]),
                json!(true)
            ),
        ]
    );
    // Applied in memory, the upgraded route needs nothing more.
    let mut upgraded = legacy();
    plan.apply_to_route(&mut upgraded);
    assert_eq!(
        upgraded["compat"],
        json!({ "sendSessionAffinityHeaders": true })
    );
    assert_eq!(plan_cli_proxy_route_upgrade(&upgraded), None);
}

// cliproxyapi.test.ts: "upgrading a route an earlier login wrote" > "finds
// nothing to change on the route the current login writes".
#[test]
fn finds_nothing_to_change_on_the_route_the_current_login_writes() {
    let route = login_route(
        "https://proxy.example/v1",
        json!({ "models": [{ "slug": "gpt-test" },
            { "slug": "claude-test", "supported_reasoning_levels": ["high", "max"] }, { "slug": "kimi-test" }] }),
    );
    assert_eq!(
        plan_cli_proxy_route_upgrade(&route.to_settings_json()),
        None
    );
}

// cliproxyapi.test.ts: "upgrading a route an earlier login wrote" > "keeps
// every value the user set, including an explicit opt-out".
#[test]
fn keeps_every_value_the_user_set_including_an_explicit_opt_out() {
    let mut route = legacy();
    route["models"] = json!([{ "id": "claude-test", "api": "openai-responses" }]);
    route["retryPolicy"] = json!({ "mode": "normal" });
    route["compat"] = json!({ "sendSessionAffinityHeaders": false });
    assert_eq!(plan_cli_proxy_route_upgrade(&route), None);
}

// cliproxyapi.test.ts: "upgrading a route an earlier login wrote" > "leaves a
// route that is not a login's alone".
#[test]
fn leaves_a_route_that_is_not_a_logins_alone() {
    let with = |key: &str, value: Value| {
        let mut route = legacy();
        route[key] = value;
        route
    };
    assert_eq!(plan_cli_proxy_route_upgrade(&Value::Null), None);
    assert_eq!(
        plan_cli_proxy_route_upgrade(&with("apiKeyEnv", json!("OTHER_KEY"))),
        None
    );
    assert_eq!(
        plan_cli_proxy_route_upgrade(&with("api", json!("openai-completions"))),
        None
    );
    assert_eq!(
        plan_cli_proxy_route_upgrade(&with("baseURL", json!(42))),
        None
    );
    assert_eq!(
        plan_cli_proxy_route_upgrade(&with("models", json!("gpt-test"))),
        None
    );
}

/// A route exercising every mapping onto Pi.
fn mapped_route() -> Value {
    json!({
        "displayName": "CLIProxyAPI", "apiKeyEnv": "CLIPROXYAPI_API_KEY", "api": "openai-responses",
        "baseURL": "https://proxy.example/v1",
        "headers": { "x-team": "core", "x-cmd": "!echo pwned", "x-env": "a $HOME ${HOME} $$ $! b!",
                     "x-edge": "${ $1 ${1x} !! $" },
        "defaultContextWindow": 200_000,
        "models": [
            { "id": "gpt-6-sol", "name": "GPT 6 Sol", "contextWindow": 272_000, "maxTokens": 128_000,
              "input": ["text", "image"],
              "reasoningEfforts": { "low": "low", "medium": "medium", "high": "high", "xhigh": "xhigh" } },
            { "id": "claude-opus-5-5", "api": "anthropic-messages", "baseURL": "https://proxy.example",
              "name": "Claude Opus 5.5", "input": [],
              "reasoningEfforts": { "off": null, "low": "low", "high": "high", "max": "max" },
              "compat": { "forceAdaptiveThinking": true } },
            { "id": "glm-test", "api": "openai-completions", "reasoningEfforts": false,
              "description": "not carried", "systemPromptUpdate": "in-history" },
            { "id": "bad-empty", "reasoningEfforts": {} },
            { "id": "bad-off-only", "reasoningEfforts": { "off": null } },
            { "id": "bad-null-wire", "reasoningEfforts": { "high": null } },
            { "id": "bad-empty-wire", "reasoningEfforts": { "high": "" } },
            { "id": "twice" },
            { "id": "twice", "name": "again" },
        ],
        "retryPolicy": { "mode": "normal", "backoff": { "maxDelayMs": 60_000 } },
        "compat": { "sendSessionAffinityHeaders": true },
        "timeoutMs": 1000,
    })
}

/// What `mapped_route` becomes; `check-with-pi.ts` beside this file loads
/// this value through Pi v1.1.0's `ModelConfig` and model registry.
fn mapped_pi_config() -> Value {
    json!({
        "name": "CLIProxyAPI",
        "baseUrl": "https://proxy.example/v1",
        "api": "openai-responses",
        "apiKey": "$CLIPROXYAPI_API_KEY",
        "headers": { "x-team": "core", "x-cmd": "$!echo pwned",
                     "x-env": "a $$HOME $${HOME} $$$$ $$! b!",
                     "x-edge": "$${ $$1 $${1x} !! $$" },
        "compat": { "sendSessionAffinityHeaders": true },
        "models": [
            { "id": "gpt-6-sol", "name": "GPT 6 Sol", "reasoning": true,
              "thinkingLevelMap": { "off": null, "minimal": null, "low": "low", "medium": "medium",
                                    "high": "high", "xhigh": "xhigh", "max": null },
              "input": ["text", "image"], "contextWindow": 272_000, "maxTokens": 128_000 },
            { "id": "claude-opus-5-5", "name": "Claude Opus 5.5", "api": "anthropic-messages",
              "baseUrl": "https://proxy.example", "reasoning": true,
              "thinkingLevelMap": { "minimal": null, "low": "low", "medium": null, "high": "high",
                                    "xhigh": null, "max": "max" },
              "input": ["text"], "contextWindow": 200_000, "maxTokens": 32_768,
              "compat": { "forceAdaptiveThinking": true } },
            { "id": "glm-test", "name": "glm-test", "api": "openai-completions", "reasoning": false,
              "input": ["text"], "contextWindow": 200_000, "maxTokens": 32_768 },
        ],
    })
}

// llm-pi-ai's `resolveRouteCatalog` and `resolveModelReasoning`
// (packages/llm/llm-pi-ai/src/catalog.ts) as one Pi provider: each mapping
// in the module documentation, the refused reasoning shapes, and a
// duplicated id leaving both entries out.
#[test]
fn maps_a_saved_route_onto_a_pi_provider() {
    let route = CliProxyRoute::from_settings(&mapped_route()).expect("a readable route");
    let provider = route.to_pi_provider_config();
    assert_eq!(provider.config, mapped_pi_config());
    let skipped: Vec<(&str, &str)> = provider
        .skipped
        .iter()
        .map(|model| (model.id.as_str(), model.reason))
        .collect();
    assert_eq!(
        skipped,
        [
            ("bad-empty", "has an empty reasoningEfforts"),
            ("bad-off-only", "offers no reasoning level beyond off"),
            (
                "bad-null-wire",
                "needs a wire value for every level but off"
            ),
            ("bad-empty-wire", "has an empty reasoning wire value"),
            ("twice", "is listed more than once"),
            ("twice", "is listed more than once"),
        ]
    );
    // The recorded Pi check reads the same bytes this test compares.
    let recorded: Value = serde_json::from_str(include_str!("pi-provider.json"))
        .expect("the recorded provider config");
    assert_eq!(recorded["providers"][CLIPROXYAPI_ID], mapped_pi_config());
}

// The login's own route onto Pi: Bake's fallbacks for unsized models, the
// Responses models on the route's protocol, Claude on the proxy root.
#[test]
fn maps_a_login_route_onto_a_pi_provider() {
    let route = login_route(
        "http://127.0.0.1:8317",
        json!({ "data": [
            { "id": "gpt-test", "context_window": 128_000, "supported_reasoning_levels": ["low", "high"] },
            { "id": "claude-test", "owned_by": "anthropic", "input_modalities": ["image"],
              "supported_reasoning_levels": ["high", "max"] },
        ] }),
    );
    assert_eq!(
        route.to_pi_provider_config().config,
        json!({
            "name": "CLIProxyAPI", "baseUrl": "http://127.0.0.1:8317/v1", "api": "openai-responses",
            "apiKey": "$CLIPROXYAPI_API_KEY", "compat": { "sendSessionAffinityHeaders": true },
            "models": [
                { "id": "gpt-test", "name": "gpt-test", "reasoning": true,
                  "thinkingLevelMap": { "off": null, "minimal": null, "low": "low", "medium": null,
                                        "high": "high", "xhigh": null, "max": null },
                  "input": ["text"], "contextWindow": 128_000, "maxTokens": 32_768 },
                { "id": "claude-test", "name": "claude-test", "api": "anthropic-messages",
                  "baseUrl": "http://127.0.0.1:8317", "reasoning": true,
                  "thinkingLevelMap": { "off": null, "minimal": null, "low": null, "medium": null,
                                        "high": "high", "xhigh": null, "max": "max" },
                  "input": ["text", "image"], "contextWindow": 262_144, "maxTokens": 32_768,
                  "compat": { "forceAdaptiveThinking": true } },
            ],
        })
    );
}

// Settings fields of the wrong type are refused with their path, never
// their value.
#[test]
fn refuses_a_malformed_route_by_path() {
    let refused = |patch: Value| {
        let mut route = mapped_route();
        if let (Value::Object(route), Value::Object(patch)) = (&mut route, patch) {
            route.extend(patch);
        }
        CliProxyRoute::from_settings(&route)
            .map(|_| ())
            .map_err(|error| error.to_string())
    };
    assert_eq!(
        refused(json!({ "baseURL": 1 })),
        Err("baseURL must be a non-empty string".into())
    );
    assert_eq!(
        refused(json!({ "baseURL": null })),
        Err("baseURL is required".into())
    );
    assert_eq!(
        refused(json!({ "models": {} })),
        Err("models must be a list".into())
    );
    assert_eq!(
        refused(json!({ "apiKeyEnv": "1-not-a-name" })),
        Err("apiKeyEnv must be an environment variable name".into())
    );
    assert_eq!(
        refused(json!({ "headers": { "x": 1 } })),
        Err("headers values must be strings".into())
    );
    assert_eq!(
        refused(json!({ "defaultInput": [] })),
        Err("defaultInput must name at least one modality".into())
    );
    assert_eq!(
        refused(json!({ "models": [{ "id": "a", "contextWindow": 0 }] })),
        Err("models[0].contextWindow must be a positive integer".into())
    );
    assert_eq!(
        refused(json!({ "models": [{ "id": "a", "maxTokens": 1.5 }] })),
        Err("models[0].maxTokens must be a positive integer".into())
    );
    assert_eq!(
        refused(json!({ "models": [{ "id": "a", "input": ["audio"] }] })),
        Err("models[0].input must list only text and image".into())
    );
    assert_eq!(
        refused(json!({ "models": [{ "id": "a", "reasoningEfforts": { "turbo": "x" } }] })),
        Err("models[0].reasoningEfforts names a level Pi does not have".into())
    );
    assert_eq!(
        refused(json!({ "models": [{ "id": "a", "reasoningEfforts": true }] })),
        Err("models[0].reasoningEfforts must be false or a mapping".into())
    );
    assert_eq!(
        refused(json!({ "models": [7] })),
        Err("models[0] must be a mapping".into())
    );
    assert_eq!(
        refused(json!({ "models": [{ "name": "x" }] })),
        Err("models[0].id must be a string".into())
    );
    assert_eq!(
        CliProxyRoute::from_settings(&json!([])).map_err(|error| error.to_string()),
        Err("cliproxyapi must be a mapping".into())
    );
}

// Pi v1.1.0 `resolve-config-value.ts` (`resolveConfigValue`, the header
// path of `provider-composer.ts`): 0.3 sent route header values literally,
// so each is escaped until Pi's resolver yields it unchanged. The mapped
// route's headers are resolved by Pi itself in `check-with-pi.mts`.
#[test]
fn escapes_header_values_pi_would_run_or_interpolate() {
    for (literal, escaped) in [
        ("", ""),
        ("plain", "plain"),
        ("!echo pwned", "$!echo pwned"),
        ("!", "$!"),
        ("a!b", "a!b"),
        ("$HOME", "$$HOME"),
        ("${HOME}", "$${HOME}"),
        ("$!", "$$!"),
        ("!$", "$!$$"),
        ("$$", "$$$$"),
    ] {
        assert_eq!(escape_pi_config_literal(literal), escaped, "{literal:?}");
    }
}

// llm-pi-ai's schema (`config.ts`, `modelFields.name` and the route's
// `displayName`, both `z.string()`) accepts an empty string, and
// `resolveRouteCatalog` kept it; Pi's `models.json` requires a non-empty
// `name`, so the model takes its id and the provider name is left out.
#[test]
fn reads_an_empty_name_and_gives_pi_a_non_empty_one() {
    let route = CliProxyRoute::from_settings(&json!({
        "displayName": "", "api": "openai-responses", "baseURL": "https://proxy.example/v1",
        "models": [{ "id": "gpt-test", "name": "" }],
    }))
    .expect("an empty name is a readable route");
    assert_eq!(route.models[0].name.as_deref(), Some(""));
    let config = route.to_pi_provider_config().config;
    assert_eq!(config.get("name"), None);
    assert_eq!(config["models"][0]["name"], json!("gpt-test"));
    assert_eq!(
        CliProxyRoute::from_settings(&json!({
            "baseURL": "https://proxy.example/v1", "models": [{ "id": "x", "name": 1 }],
        }))
        .map(|_| ())
        .map_err(|error| error.to_string()),
        Err("models[0].name must be a string".into())
    );
}
