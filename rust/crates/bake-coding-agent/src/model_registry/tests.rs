//! Ports cases of Pi `test/model-resolver.test.ts` and
//! `test/model-registry.test.ts` (v1.1.0) that need no built-in catalog.
//! Pi's registry cases override built-in providers; here an added provider
//! ([`ModelRegistry::add_provider_config`]) plays the built-in's part.

use bake_ai::ModelThinkingLevel;
use serde_json::json;

use super::resolver::{
    CliModel, ModelCatalog, find_exact_model_reference_match, find_initial_model,
    parse_model_pattern, resolve_cli_model,
};
use super::*;

fn model(provider: &str, id: &str, name: &str) -> Model {
    let mut model = bake_ai::providers::faux::faux_model(id);
    model.provider = provider.to_owned();
    model.name = name.to_owned();
    model.api = "anthropic-messages".into();
    model
}

/// Pi's `mockModels` and `mockOpenRouterModels`.
fn all_models() -> Vec<Model> {
    let mut sonnet = model("anthropic", "claude-sonnet-4-5", "Claude Sonnet 4.5");
    sonnet.reasoning = true;
    let mut qwen = model(
        "openrouter",
        "qwen/qwen3-coder:exacto",
        "Qwen3 Coder Exacto",
    );
    qwen.reasoning = true;
    vec![
        sonnet,
        model("openai", "gpt-4o", "GPT-4o"),
        qwen,
        model("openrouter", "openai/gpt-4o:extended", "GPT-4o Extended"),
    ]
}

struct Catalog {
    models: Vec<Model>,
    authed: Vec<&'static str>,
}

impl ModelCatalog for Catalog {
    fn all_models(&self) -> Vec<Model> {
        self.models.clone()
    }

    fn provider_has_auth(&self, provider: &str) -> bool {
        self.authed.contains(&provider)
    }
}

fn catalog(models: Vec<Model>) -> Catalog {
    Catalog {
        models,
        authed: Vec::new(),
    }
}

fn parse(pattern: &str) -> (Option<String>, Option<ModelThinkingLevel>, Option<String>) {
    let parsed = parse_model_pattern(pattern, &all_models(), true);
    (
        parsed.model.map(|model| model.id),
        parsed.thinking_level,
        parsed.warning,
    )
}

// Pi `describe("parseModelPattern")`.
#[test]
fn parse_model_pattern_follows_pi() {
    assert_eq!(
        parse("claude-sonnet-4-5"),
        (Some("claude-sonnet-4-5".into()), None, None)
    );
    assert_eq!(
        parse("sonnet"),
        (Some("claude-sonnet-4-5".into()), None, None)
    );
    assert_eq!(parse("nonexistent"), (None, None, None));
    assert_eq!(
        parse("sonnet:high"),
        (
            Some("claude-sonnet-4-5".into()),
            Some(ModelThinkingLevel::High),
            None
        )
    );
    assert_eq!(
        parse("gpt-4o:medium"),
        (
            Some("gpt-4o".into()),
            Some(ModelThinkingLevel::Medium),
            None
        )
    );
    for level in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] {
        let (id, thinking, warning) = parse(&format!("sonnet:{level}"));
        assert_eq!(id.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(thinking.map(ModelThinkingLevel::as_str), Some(level));
        assert_eq!(warning, None);
    }
    let (id, thinking, warning) = parse("sonnet:random");
    assert_eq!((id.as_deref(), thinking), (Some("claude-sonnet-4-5"), None));
    assert_eq!(
        warning.as_deref(),
        Some(
            "Invalid thinking level \"random\" in pattern \"sonnet:random\". Using default instead."
        )
    );
    assert!(parse("gpt-4o:invalid").2.is_some());
    assert_eq!(
        parse("qwen/qwen3-coder:exacto"),
        (Some("qwen/qwen3-coder:exacto".into()), None, None)
    );
    let parsed = parse_model_pattern(
        "openrouter/qwen/qwen3-coder:exacto:high",
        &all_models(),
        true,
    );
    assert_eq!(
        parsed.model.map(|m| m.provider).as_deref(),
        Some("openrouter")
    );
    assert_eq!(parsed.thinking_level, Some(ModelThinkingLevel::High));
    assert_eq!(
        parse("openai/gpt-4o:extended"),
        (Some("openai/gpt-4o:extended".into()), None, None)
    );
    let (id, thinking, warning) = parse("qwen/qwen3-coder:exacto:high:random");
    assert_eq!(
        (id.as_deref(), thinking),
        (Some("qwen/qwen3-coder:exacto"), None)
    );
    assert!(warning.is_some_and(|warning| warning.contains("random")));
    assert!(parse("").0.is_some());
    let (id, _, warning) = parse("sonnet:");
    assert_eq!(id.as_deref(), Some("claude-sonnet-4-5"));
    assert!(warning.is_some_and(|warning| warning.contains("Invalid thinking level")));
}

fn cli(provider: Option<&str>, pattern: &str, catalog: &Catalog) -> CliModel {
    resolve_cli_model(provider, Some(pattern), None, catalog)
}

fn picked(result: &CliModel) -> Option<(String, String)> {
    assert_eq!(result.error, None);
    result
        .model
        .as_ref()
        .map(|model| (model.provider.clone(), model.id.clone()))
}

fn pair(provider: &str, id: &str) -> Option<(String, String)> {
    Some((provider.to_owned(), id.to_owned()))
}

// Pi `describe("resolveCliModel")`.
#[test]
fn resolve_cli_model_follows_pi() {
    let all = catalog(all_models());
    assert_eq!(
        picked(&cli(None, "openai/gpt-4o", &all)),
        pair("openai", "gpt-4o")
    );
    assert_eq!(
        picked(&cli(Some("openai"), "4o", &all)),
        pair("openai", "gpt-4o")
    );
    let thinking = cli(None, "sonnet:high", &all);
    assert_eq!(picked(&thinking), pair("anthropic", "claude-sonnet-4-5"));
    assert_eq!(thinking.thinking_level, Some(ModelThinkingLevel::High));
    assert_eq!(
        picked(&cli(None, "openai/gpt-4o:extended", &all)),
        pair("openrouter", "openai/gpt-4o:extended")
    );
    assert_eq!(
        picked(&cli(Some("openai"), "gpt-4o:extended", &all)),
        pair("openai", "gpt-4o:extended")
    );
    assert_eq!(
        picked(&cli(
            Some("openrouter"),
            "openrouter/openai/ghost-model",
            &all
        )),
        pair("openrouter", "openai/ghost-model")
    );
    let empty = cli(Some("openai"), "gpt-4o", &catalog(Vec::new()));
    assert!(empty.model.is_none());
    assert!(
        empty
            .error
            .is_some_and(|error| error.contains("No models available"))
    );
    assert_eq!(
        picked(&cli(None, "openrouter/qwen", &all)),
        pair("openrouter", "qwen/qwen3-coder:exacto")
    );
    let unknown = cli(Some("nope"), "x", &all);
    assert_eq!(
        unknown.error.as_deref(),
        Some("Unknown provider \"nope\". Use --list-models to see available providers/models.")
    );
}

#[test]
fn ambiguous_bare_ids_prefer_the_sole_authenticated_provider() {
    let models = vec![
        model("azure", "gpt-5.6-sol", "GPT 5.6 Sol"),
        model("openai-codex", "gpt-5.6-sol", "GPT 5.6 Sol"),
    ];
    let one = Catalog {
        models: models.clone(),
        authed: vec!["openai-codex"],
    };
    assert_eq!(
        picked(&cli(None, "gpt-5.6-sol", &one)),
        pair("openai-codex", "gpt-5.6-sol")
    );
    let none = cli(None, "gpt-5.6-sol", &catalog(models));
    assert_eq!(none.model, None);
    assert_eq!(
        none.error.as_deref(),
        Some(
            "Model \"gpt-5.6-sol\" is ambiguous across providers: azure/gpt-5.6-sol, openai-codex/gpt-5.6-sol. No matching provider is authenticated. Use --provider or provider/model."
        )
    );
}

#[test]
fn provider_split_and_authenticated_raw_ids() {
    let mut models = all_models();
    models.push(model("zai", "glm-5", "GLM-5"));
    models.push(model("vercel-ai-gateway", "zai/glm-5", "GLM-5"));
    let everyone = Catalog {
        models,
        authed: vec![
            "zai",
            "vercel-ai-gateway",
            "anthropic",
            "openai",
            "openrouter",
        ],
    };
    assert_eq!(
        picked(&cli(None, "zai/glm-5", &everyone)),
        pair("zai", "glm-5")
    );
    let mut models = all_models();
    models.push(model(
        "commandcode",
        "xiaomi/mimo-v2.5-pro",
        "Xiaomi MiMo via Commandcode",
    ));
    models.push(model("xiaomi", "mimo-v2.5-pro", "Xiaomi MiMo"));
    let commandcode = Catalog {
        models,
        authed: vec!["commandcode"],
    };
    assert_eq!(
        picked(&cli(None, "xiaomi/mimo-v2.5-pro", &commandcode)),
        pair("commandcode", "xiaomi/mimo-v2.5-pro")
    );
}

// Pi `describe("custom model fallback with :thinking suffix (#5552)")`.
#[test]
fn custom_model_fallback_strips_thinking_suffixes() {
    let mut models = all_models();
    models.push(model("neuralwatt", "some-base-model", "Some Base Model"));
    let neuralwatt = catalog(models);
    let high = cli(None, "neuralwatt/zai-org/GLM-5.1-FP8:high", &neuralwatt);
    assert_eq!(picked(&high), pair("neuralwatt", "zai-org/GLM-5.1-FP8"));
    assert!(high.model.as_ref().is_some_and(|model| model.reasoning));
    assert_eq!(high.thinking_level, Some(ModelThinkingLevel::High));
    assert_eq!(
        high.warning.as_deref(),
        Some(
            "Model \"zai-org/GLM-5.1-FP8\" not found for provider \"neuralwatt\". Using custom model id."
        )
    );
    let plain = cli(None, "neuralwatt/zai-org/GLM-5.1-FP8", &neuralwatt);
    assert_eq!(picked(&plain), pair("neuralwatt", "zai-org/GLM-5.1-FP8"));
    assert_eq!(plain.thinking_level, None);
    for level in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] {
        let result = cli(
            None,
            &format!("neuralwatt/zai-org/GLM-5.1-FP8:{level}"),
            &neuralwatt,
        );
        assert_eq!(picked(&result), pair("neuralwatt", "zai-org/GLM-5.1-FP8"));
        assert_eq!(
            result.thinking_level.map(ModelThinkingLevel::as_str),
            Some(level)
        );
    }
    let banana = cli(None, "neuralwatt/zai-org/GLM-5.1-FP8:banana", &neuralwatt);
    assert_eq!(
        picked(&banana),
        pair("neuralwatt", "zai-org/GLM-5.1-FP8:banana")
    );
    let explicit = cli(Some("neuralwatt"), "zai-org/GLM-5.1-FP8:high", &neuralwatt);
    assert_eq!(picked(&explicit), pair("neuralwatt", "zai-org/GLM-5.1-FP8"));
    assert_eq!(explicit.thinking_level, Some(ModelThinkingLevel::High));
    let thinking = resolve_cli_model(
        None,
        Some("neuralwatt/zai-org/GLM-5.1-FP8:high"),
        Some(ModelThinkingLevel::Medium),
        &neuralwatt,
    );
    assert_eq!(
        picked(&thinking),
        pair("neuralwatt", "zai-org/GLM-5.1-FP8:high")
    );
    assert_eq!(thinking.thinking_level, None);
}

#[test]
fn exact_references_reject_ambiguity() {
    let mut models = all_models();
    models.push(model("other", "gpt-4o", "GPT-4o"));
    assert!(find_exact_model_reference_match("gpt-4o", &models).is_none());
    assert_eq!(
        find_exact_model_reference_match(" OpenAI/GPT-4o ", &models).map(|m| m.provider.as_str()),
        Some("openai")
    );
    assert!(find_exact_model_reference_match("  ", &models).is_none());
}

fn registry(models_json: serde_json::Value, auth: &str) -> ModelRegistry {
    ModelRegistry::new(
        ModelConfig::from_value(models_json, "models.json"),
        AuthStorage::from_json(auth),
    )
}

fn custom_provider(api_key: Option<&str>) -> serde_json::Value {
    let mut provider = json!({
        "baseUrl": "http://localhost:1/v1",
        "api": "openai-responses",
        "models": [{ "id": "m1" }, { "id": "m2", "reasoning": true, "contextWindow": 1000.7 }],
    });
    if let Some(key) = api_key {
        provider["apiKey"] = json!(key);
    }
    provider
}

fn auth_of(
    registry: &ModelRegistry,
    provider: &str,
    id: &str,
) -> Result<Option<RequestAuth>, String> {
    let model = registry.model(provider, id).ok_or("no model")?;
    registry.get_auth(&model)
}

#[test]
fn custom_models_take_pi_defaults() {
    let registry = registry(
        json!({ "providers": { "p": custom_provider(Some("lit")) } }),
        "{}",
    );
    assert_eq!(registry.error(), None);
    let m1 = registry.model("p", "m1").expect("m1");
    assert_eq!(m1.name, "m1");
    assert_eq!(m1.api, "openai-responses");
    assert_eq!(m1.base_url, "http://localhost:1/v1");
    assert_eq!((m1.context_window, m1.max_tokens), (128_000, 16_384));
    assert_eq!(m1.input, vec![bake_ai::InputModality::Text]);
    assert!(!m1.reasoning);
    let m2 = registry.model("p", "m2").expect("m2");
    assert!(m2.reasoning);
    assert_eq!(m2.context_window, 1000);
    assert!(registry.has_configured_auth("p"));
    assert_eq!(registry.available().len(), 2);
}

// Pi: "non-built-in provider custom models still require baseUrl" and
// "reports every provider composition error".
#[test]
fn composition_errors_are_reported() {
    let registry = registry(
        json!({ "providers": {
            "a": { "api": "openai-responses", "models": [{ "id": "x" }] },
            "b": { "baseUrl": "u", "models": [{ "id": "y" }] },
            "c": { "name": "only a name" },
            "d": { "baseUrl": "u", "api": "x", "models": [{ "id": "z", "maxTokens": 0 }] },
        } }),
        "{}",
    );
    assert_eq!(
        registry.error().as_deref(),
        Some(
            "Provider \"a\": Provider a: \"baseUrl\" is required when defining custom models.\n\n\
             Provider \"b\": Provider b, model y: no \"api\" specified. Set at provider or model level.\n\n\
             Provider \"c\": Provider c: must specify \"baseUrl\", \"headers\", \"compat\", \"modelOverrides\", or \"models\".\n\n\
             Provider \"d\": Provider d, model z: invalid maxTokens"
        )
    );
    assert!(registry.models().is_empty());
}

// Pi "API key resolution": literals, `$` templates, `$$` and `$!` escapes,
// and commands.
#[test]
fn api_keys_resolve_as_pi_resolves_them() {
    let key = |value: &str| {
        let registry = registry(
            json!({ "providers": { "p": custom_provider(Some(value)) } }),
            "{}",
        );
        auth_of(&registry, "p", "m1").map(|auth| auth.and_then(|auth| auth.api_key))
    };
    assert_eq!(key("literal-key"), Ok(Some("literal-key".into())));
    assert_eq!(key("$$PATH"), Ok(Some("$PATH".into())));
    assert_eq!(key("PATH"), Ok(Some("PATH".into())));
    assert_eq!(
        key("$BAKE_TEST_SURELY_UNSET_KEY"),
        Err("API key auth failed for provider p".into())
    );
    #[cfg(unix)]
    {
        assert_eq!(
            key("!echo '  from-command  '"),
            Ok(Some("from-command".into()))
        );
        assert_eq!(key("!echo 'a b' | tr ' ' '-'"), Ok(Some("a-b".into())));
        assert_eq!(
            key("!exit 1"),
            Err("API key auth failed for provider p".into())
        );
    }
}

// Pi: "missing explicit env apiKey keeps provider unavailable" and
// "provider auth status reports command apiKey values ... without
// executing them".
#[test]
fn availability_checks_without_running_commands() {
    let unset = registry(
        json!({ "providers": { "p": custom_provider(Some("$BAKE_TEST_SURELY_UNSET_KEY")) } }),
        "{}",
    );
    assert!(!unset.has_configured_auth("p"));
    assert!(unset.available().is_empty());
    let none = registry(json!({ "providers": { "p": custom_provider(None) } }), "{}");
    assert!(!none.has_configured_auth("p"));
    let command = registry(
        json!({ "providers": { "p": custom_provider(Some("!exit 1")) } }),
        "{}",
    );
    assert_eq!(
        command.check_auth("p").as_deref(),
        Some("configured API key")
    );
}

// Pi: "stored credentials bypass lower-priority configured auth commands"
// and "stored API key env propagates to request auth and resolves headers".
#[test]
fn stored_and_runtime_keys_come_first() {
    let mut provider = custom_provider(Some("!exit 1"));
    provider["headers"] = json!({ "x-team": "$TEAM" });
    let mut registry = registry(
        json!({ "providers": { "p": provider } }),
        r#"{"p":{"type":"api_key","key":"stored","env":{"TEAM":"red"}}}"#,
    );
    let auth = auth_of(&registry, "p", "m1").expect("auth").expect("some");
    assert_eq!(auth.api_key.as_deref(), Some("stored"));
    assert_eq!(auth.source, "stored credential");
    assert_eq!(auth.headers, Some(vec![("x-team".into(), "red".into())]));
    // A runtime key carries no env, as Pi's `RuntimeCredentials.read`.
    registry.set_runtime_api_key("p", "runtime");
    assert_eq!(
        auth_of(&registry, "p", "m1"),
        Err("API key auth failed for provider p".into())
    );
    let mut plain = self::registry(
        json!({ "providers": { "p": custom_provider(Some("!exit 1")) } }),
        r#"{"p":{"type":"api_key","key":"stored"}}"#,
    );
    plain.set_runtime_api_key("p", "runtime");
    let auth = auth_of(&plain, "p", "m1").expect("auth").expect("some");
    assert_eq!(auth.api_key.as_deref(), Some("runtime"));
    // An OAuth credential owns the provider, which has no OAuth method.
    let oauth = self::registry(
        json!({ "providers": { "p": custom_provider(Some("lit")) } }),
        r#"{"p":{"type":"oauth","access":"a","refresh":"r","expires":1}}"#,
    );
    assert!(!oauth.has_configured_auth("p"));
    assert_eq!(auth_of(&oauth, "p", "m1"), Ok(None));
}

// Pi: "getApiKeyAndHeaders resolves authHeader on every request", "model
// override can add headers at request time", and "getApiKeyAndHeaders
// preserves the legacy missing-key authHeader error".
#[test]
fn headers_and_auth_header() {
    let mut provider = custom_provider(Some("k"));
    provider["authHeader"] = json!(true);
    provider["headers"] = json!({ "x-a": "1" });
    provider["modelOverrides"] = json!({ "m1": { "headers": { "X-A": "2", "x-b": "3" } } });
    let registry = registry(json!({ "providers": { "p": provider } }), "{}");
    let auth = auth_of(&registry, "p", "m1").expect("auth").expect("some");
    assert_eq!(
        auth.headers,
        Some(vec![
            ("Authorization".into(), "Bearer k".into()),
            ("X-A".into(), "2".into()),
            ("x-b".into(), "3".into()),
        ])
    );
    let mut keyless = custom_provider(None);
    keyless["authHeader"] = json!(true);
    let keyless = self::registry(
        json!({ "providers": { "p": keyless } }),
        r#"{"p":{"type":"api_key"}}"#,
    );
    assert_eq!(auth_of(&keyless, "p", "m1"), Ok(None));
}

// Pi: "model override applies to a single built-in model", "model override
// can change cost fields partially", "model override deep merges compat
// settings", and "model override for non-existent model ID is ignored".
#[test]
fn model_overrides_merge_as_pi_merges() {
    let mut provider = custom_provider(Some("k"));
    provider["compat"] = json!({ "supportsStore": false, "openRouterRouting": { "only": ["a"] } });
    provider["models"][0]["cost"] =
        json!({ "input": 1, "output": 2, "cacheRead": 3, "cacheWrite": 4 });
    provider["modelOverrides"] = json!({
        "m1": {
            "name": "Renamed",
            "cost": { "output": 9 },
            "compat": { "openRouterRouting": { "order": ["b"] } },
            "maxTokens": 77,
        },
        "missing": { "name": "ignored" },
    });
    let registry = registry(json!({ "providers": { "p": provider } }), "{}");
    assert_eq!(registry.error(), None);
    let m1 = registry.model("p", "m1").expect("m1");
    assert_eq!(m1.name, "Renamed");
    assert_eq!(m1.max_tokens, 77);
    assert_eq!(
        (
            m1.cost.rates.input,
            m1.cost.rates.output,
            m1.cost.rates.cache_write
        ),
        (1.0, 9.0, 4.0)
    );
    let compat = m1.compat.expect("compat");
    assert_eq!(compat.supports_store, Some(false));
    assert_eq!(
        compat.open_router_routing,
        Some(json!({ "only": ["a"], "order": ["b"] }))
    );
    assert_eq!(
        registry.model("p", "m2").map(|m| m.name).as_deref(),
        Some("m2")
    );
    assert_eq!(registry.models().len(), 2);
}

/// The seam for lane CLIPROXY: an added provider sits below `models.json`.
#[test]
fn added_providers_sit_below_models_json() {
    let mut registry = registry(
        json!({ "providers": { "proxy": {
            "baseUrl": "http://override/v1",
            "apiKey": "json-key",
            "models": [{ "id": "extra", "api": "openai-completions" }],
        } } }),
        "{}",
    );
    registry
        .add_provider_config(
            "proxy",
            json!({
                "baseUrl": "http://proxy/v1",
                "api": "openai-responses",
                "headers": { "x-route": "proxy" },
                "models": [{ "id": "gpt" }],
            }),
            Some("proxy-key".into()),
        )
        .expect("added");
    registry
        .add_provider_config(
            "solo",
            json!({ "baseUrl": "http://solo/v1", "api": "anthropic-messages", "models": [{ "id": "s" }] }),
            Some("$not-a-template".into()),
        )
        .expect("added");
    assert_eq!(registry.error(), None);
    // models.json wins: its baseUrl reaches the added provider's models.
    let gpt = registry.model("proxy", "gpt").expect("gpt");
    assert_eq!(gpt.base_url, "http://override/v1");
    assert_eq!(gpt.api, "openai-responses");
    assert!(registry.model("proxy", "extra").is_some());
    let auth = auth_of(&registry, "proxy", "gpt")
        .expect("auth")
        .expect("some");
    assert_eq!(auth.api_key.as_deref(), Some("json-key"));
    assert_eq!(auth.headers, Some(vec![("x-route".into(), "proxy".into())]));
    // Without a models.json key, the added key is literal.
    let solo = auth_of(&registry, "solo", "s")
        .expect("auth")
        .expect("some");
    assert_eq!(solo.api_key.as_deref(), Some("$not-a-template"));
    assert!(registry.has_configured_auth("solo"));
    // Added providers come first, in the order added.
    assert_eq!(
        registry
            .models()
            .iter()
            .map(|m| format!("{}/{}", m.provider, m.id))
            .collect::<Vec<_>>(),
        vec!["proxy/gpt", "proxy/extra", "solo/s"]
    );
    assert_eq!(
        registry.add_provider_config("bad", json!({ "baseUrl": 3 }), None),
        Err("Invalid provider config:\n  - providers.bad.baseUrl: must be string".into())
    );
    assert!(!registry.has_provider("bad"));
}

#[test]
fn initial_model_prefers_the_saved_default_with_auth() {
    let registry = registry(
        json!({ "providers": {
            "a": custom_provider(Some("k")),
            "b": custom_provider(None),
        } }),
        "{}",
    );
    let saved = find_initial_model(
        Some("a"),
        Some("m2"),
        Some(ModelThinkingLevel::Low),
        &[],
        &registry,
    );
    assert_eq!(saved.model.map(|m| m.id).as_deref(), Some("m2"));
    assert_eq!(saved.thinking_level, ModelThinkingLevel::Low);
    // Pi: "findInitialModel ignores an unauthenticated saved default".
    let fallback = find_initial_model(Some("b"), Some("m2"), None, &[], &registry);
    assert_eq!(
        fallback
            .model
            .map(|m| format!("{}/{}", m.provider, m.id))
            .as_deref(),
        Some("a/m1")
    );
    assert_eq!(fallback.thinking_level, ModelThinkingLevel::Medium);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_stream_function_fails_setup_with_pi_messages() {
    let registry = Arc::new(registry(
        json!({ "providers": { "p": custom_provider(Some("$BAKE_TEST_SURELY_UNSET_KEY")) } }),
        "{}",
    ));
    let stream_fn = registry.stream_fn(Arc::new(ApiRegistry::with_builtins()));
    let context = TranscriptContext::default();
    let mut model = registry.model("p", "m1").expect("m1");
    let stream = stream_fn(&model, &context, SimpleStreamOptions::default()).expect("stream");
    let result = stream.result().await.expect("result");
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("API key auth failed for provider p")
    );
    model.provider = "ghost".into();
    let stream = stream_fn(&model, &context, SimpleStreamOptions::default()).expect("stream");
    assert_eq!(
        stream
            .result()
            .await
            .and_then(|m| m.error_message)
            .as_deref(),
        Some("Unknown provider: ghost")
    );
}
