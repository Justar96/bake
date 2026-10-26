//! Ports cases of Pi `test/model-resolver.test.ts` and
//! `test/model-registry.test.ts` (v1.1.0) that need no built-in catalog.
//! Pi's registry cases override built-in providers; here an added provider
//! ([`ModelRegistry::add_provider_config`]) plays the built-in's part.

use bake_ai::ModelThinkingLevel;
use serde_json::json;

use super::resolver::{
    CliModel, ModelCatalog, ModelScopeDiagnostic, ModelScopeDiagnosticCode,
    find_exact_model_reference_match, find_initial_model, parse_model_pattern, resolve_cli_model,
    resolve_model_scope_from_models,
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
    assert_eq!(unknown.error.as_deref(), Some("Unknown provider \"nope\"."));
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

// An abort that lands before authentication finishes ends the request as
// aborted, as Pi's providers end a request whose signal is aborted, without
// sending it to the provider.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_aborted_before_it_starts_never_reaches_the_provider() {
    use bake_ai::providers::faux::{FauxProvider, FauxProviderOptions};
    let faux = Arc::new(FauxProvider::new(FauxProviderOptions::default()));
    let apis = Arc::new(ApiRegistry::new());
    apis.register(faux.clone());
    let mut registry = registry(json!({}), "{}");
    registry
        .add_provider_config(
            "faux",
            json!({
                "baseUrl": "http://127.0.0.1:9/unused",
                "api": faux.model().api,
                "models": [{ "id": "faux-1" }],
            }),
            Some("test-key".into()),
        )
        .expect("faux provider");
    let registry = Arc::new(registry);
    let model = registry.model("faux", "faux-1").expect("faux-1");
    let stream_fn = registry.stream_fn_owned(apis, None);
    for _ in 0..50 {
        let controller = bake_ai::AbortController::new();
        controller.abort();
        let mut options = SimpleStreamOptions::default();
        options.base.signal = Some(controller.signal());
        let stream = stream_fn(&model, &TranscriptContext::default(), options).expect("stream");
        let result = stream.result().await.expect("result");
        assert_eq!(result.stop_reason, StopReason::Aborted);
        assert_eq!(result.error_message.as_deref(), Some("Request was aborted"));
    }
    assert_eq!(faux.call_count(), 0);
}

// Pi `describe("resolveModelScopeWithDiagnostics")`.
#[test]
fn resolve_model_scope_follows_pi() {
    let models = all_models();
    let result = resolve_model_scope_from_models(
        &[
            "sonnet:high".into(),
            "gpt-4o:invalid".into(),
            "missing".into(),
        ],
        &models,
    );
    assert_eq!(
        result
            .scoped_models
            .iter()
            .map(|scoped| (scoped.model.id.as_str(), scoped.thinking_level))
            .collect::<Vec<_>>(),
        vec![
            ("claude-sonnet-4-5", Some(ModelThinkingLevel::High)),
            ("gpt-4o", None),
        ]
    );
    assert_eq!(
        result.diagnostics,
        vec![
            ModelScopeDiagnostic {
                code: ModelScopeDiagnosticCode::InvalidThinkingLevel,
                message: "Invalid thinking level \"invalid\" in pattern \"gpt-4o:invalid\". Using default instead.".into(),
                pattern: "gpt-4o:invalid".into(),
            },
            ModelScopeDiagnostic {
                code: ModelScopeDiagnosticCode::NoMatch,
                message: "No models match pattern \"missing\"".into(),
                pattern: "missing".into(),
            },
        ]
    );

    // Bracketed model ids resolve as exact references before glob matching.
    let mut with_brackets = models.clone();
    with_brackets.push(model("custom", "bracketed-model[1m]", "Bracketed Model"));
    let exact = resolve_model_scope_from_models(
        &["custom/bracketed-model[1m]:high".into()],
        &with_brackets,
    );
    assert_eq!(exact.scoped_models.len(), 1);
    assert_eq!(exact.scoped_models[0].model.id, "bracketed-model[1m]");
    assert_eq!(
        exact.scoped_models[0].thinking_level,
        Some(ModelThinkingLevel::High)
    );
    assert!(exact.diagnostics.is_empty());

    // Glob patterns match both `provider/id` and `id` case-insensitively and deduplicate.
    let globbed =
        resolve_model_scope_from_models(&["*SONNET*:low".into(), "anthropic/*".into()], &models);
    assert_eq!(globbed.scoped_models.len(), 1);
    assert_eq!(globbed.scoped_models[0].model.id, "claude-sonnet-4-5");
    assert_eq!(
        globbed.scoped_models[0].thinking_level,
        Some(ModelThinkingLevel::Low)
    );
}

// Bake: no key, header value, or scoped environment value of any layer
// reaches `Debug`: `models.json`, an added provider, `auth.json` with its
// `env`, a runtime key, and a resolved request's auth.
#[test]
fn debug_output_shows_no_secret() {
    let mut registry = registry(
        json!({ "providers": {
            "config": {
                "baseUrl": "http://localhost:1/v1",
                "api": "openai-responses",
                "apiKey": "sk-config-secret",
                "headers": { "x-config": "hdr-config-secret" },
                "models": [{ "id": "m1", "headers": { "x-model": "hdr-model-secret" } }],
            },
            "stored": custom_provider(None),
        } }),
        r#"{ "stored": { "type": "api_key", "key": "sk-stored-secret", "env": { "TOKEN": "env-secret" } } }"#,
    );
    registry
        .add_provider_config(
            "added",
            json!({
                "baseUrl": "http://localhost:1/v1",
                "api": "openai-responses",
                "headers": { "x-added": "hdr-added-secret" },
                "models": [{ "id": "a1" }],
            }),
            Some("sk-added-secret".into()),
        )
        .expect("added");
    registry.set_runtime_api_key("config", "sk-runtime-secret");
    let auth = auth_of(&registry, "config", "m1")
        .expect("auth")
        .expect("some");
    assert_eq!(auth.api_key.as_deref(), Some("sk-runtime-secret"));
    let stored = auth_of(&registry, "stored", "m1")
        .expect("auth")
        .expect("some");
    let runtime = ModelRuntime::new(registry.clone());
    let shown = format!(
        "{registry:?}\n{registry:#?}\n{auth:?}\n{stored:#?}\n{runtime:?}\n{:?}",
        ModelConfig::from_value(
            json!({ "providers": { "c": { "apiKey": "sk-config-secret", "baseUrl": "http://x", "api": "a", "models": [{ "id": "m" }] } } }),
            "models.json"
        )
    );
    for secret in [
        "sk-config-secret",
        "hdr-config-secret",
        "hdr-model-secret",
        "sk-stored-secret",
        "env-secret",
        "sk-added-secret",
        "hdr-added-secret",
        "sk-runtime-secret",
    ] {
        assert!(!shown.contains(secret), "{secret} leaked:\n{shown}");
    }
    // Names stay visible for diagnosis.
    assert!(shown.contains("x-config") && shown.contains("TOKEN") && shown.contains("added"));
}

fn faux_runtime(
    key: &str,
) -> (
    Arc<bake_ai::providers::faux::FauxProvider>,
    Arc<ApiRegistry>,
    ModelRuntime,
) {
    use bake_ai::providers::faux::{FauxProvider, FauxProviderOptions};
    let faux = Arc::new(FauxProvider::new(FauxProviderOptions::default()));
    let apis = Arc::new(ApiRegistry::new());
    apis.register(faux.clone());
    let mut registry = registry(json!({}), "{}");
    registry
        .add_provider_config(
            "faux",
            json!({
                "baseUrl": "http://127.0.0.1:9/unused",
                "api": faux.model().api,
                "models": [{ "id": "faux-1" }],
            }),
            Some(key.into()),
        )
        .expect("faux provider");
    (faux, apis, ModelRuntime::new(registry))
}

/// A faux response that records the request's API key.
fn recording_key(
    seen: &Arc<std::sync::Mutex<Vec<Option<String>>>>,
) -> bake_ai::providers::faux::FauxResponseStep {
    let seen = Arc::clone(seen);
    bake_ai::providers::faux::FauxResponseStep::from_fn(move |_, options, _, _| {
        seen.lock()
            .expect("seen")
            .push(options.base.api_key.clone());
        Ok(bake_ai::providers::faux::faux_assistant_message("ok"))
    })
}

// Bake: a request owns the registry snapshot current when it starts. An
// update committed after the request started, before its auth resolved,
// reaches only the next request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_keeps_its_snapshot_and_the_next_sees_the_update() {
    let (faux, apis, runtime) = faux_runtime("old-key");
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    faux.set_responses(vec![recording_key(&seen), recording_key(&seen)]);
    let stream_fn = runtime.stream_fn_owned(apis, None);
    let model = runtime.snapshot().model("faux", "faux-1").expect("model");
    let first = stream_fn(
        &model,
        &TranscriptContext::default(),
        SimpleStreamOptions::default(),
    )
    .expect("stream");
    runtime.update(|registry| registry.set_runtime_api_key("faux", "new-key"));
    first.result().await.expect("first");
    let second = stream_fn(
        &model,
        &TranscriptContext::default(),
        SimpleStreamOptions::default(),
    )
    .expect("stream");
    second.result().await.expect("second");
    assert_eq!(
        *seen.lock().expect("seen"),
        [Some("old-key".to_owned()), Some("new-key".to_owned())]
    );
}

// Bake: concurrent updates are serialized, so none is lost.
#[test]
fn concurrent_updates_lose_nothing() {
    let runtime = ModelRuntime::new(registry(json!({}), "{}"));
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let writers: Vec<_> = (0..8)
        .map(|n| {
            let runtime = runtime.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                runtime.update(|registry| {
                    registry
                        .add_provider_config(&format!("p{n}"), custom_provider(Some("k")), None)
                        .expect("add");
                });
            })
        })
        .collect();
    for writer in writers {
        writer.join().expect("writer");
    }
    let snapshot = runtime.snapshot();
    for n in 0..8 {
        assert!(snapshot.has_provider(&format!("p{n}")), "p{n} lost");
    }
}

// Bake: rereading `auth.json` keeps the runtime key (`--api-key`) and the
// providers added below `models.json`, and sees the file's new
// credentials.
#[test]
fn reloading_auth_keeps_runtime_keys_and_added_providers() {
    let dir = crate::test_support::TempDir::new("registry-reload");
    let auth = dir.join("auth.json");
    std::fs::write(
        &auth,
        r#"{ "b": { "type": "api_key", "key": "stored-b" } }"#,
    )
    .expect("auth");
    let mut registry = ModelRegistry::new(
        ModelConfig::from_value(
            json!({ "providers": { "b": custom_provider(None) } }),
            "models.json",
        ),
        AuthStorage::load(&auth),
    );
    registry
        .add_provider_config("a", custom_provider(None), None)
        .expect("added");
    registry.set_runtime_api_key("a", "runtime-a");
    let runtime = ModelRuntime::new(registry);
    std::fs::write(&auth, r#"{ "a": { "type": "api_key", "key": "stored-a" }, "b": { "type": "api_key", "key": "stored-b2" } }"#)
        .expect("auth");
    runtime
        .try_update(ModelRegistry::reload_auth)
        .expect("reload");
    let snapshot = runtime.snapshot();
    assert!(snapshot.has_added_provider("a"));
    assert_eq!(
        snapshot.provider_api_key("a").expect("a").as_deref(),
        Some("runtime-a")
    );
    assert_eq!(
        snapshot.provider_api_key("b").expect("b").as_deref(),
        Some("stored-b2")
    );
    // Removing the added provider leaves models.json's.
    runtime.update(|registry| assert!(registry.remove_provider_config("a")));
    assert!(!runtime.snapshot().has_provider("a"));
    assert!(runtime.snapshot().has_provider("b"));
}

// Bake: a change that fails publishes nothing, though it changed the copy
// before failing.
#[test]
fn a_failed_update_publishes_nothing() {
    let runtime = ModelRuntime::new(registry(json!({}), "{}"));
    let failed: Result<(), String> = runtime.try_update(|registry| {
        registry
            .add_provider_config("half", custom_provider(Some("k")), None)
            .expect("add");
        Err("later step failed".into())
    });
    assert_eq!(failed, Err("later step failed".into()));
    assert!(!runtime.snapshot().has_provider("half"));
    runtime
        .try_update(|registry| {
            registry.add_provider_config("whole", custom_provider(Some("k")), None)
        })
        .expect("publish");
    assert!(runtime.snapshot().has_provider("whole"));
}

// Bake: a request takes its model's definition and auth from one snapshot.
// A route replaced while a response streams (a new proxy and key for the
// same model id) reaches the next request whole: the held request keeps
// the old endpoint with the old key, the next has the new with the new.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replaced_route_pairs_each_request_with_its_own_key() {
    use bake_ai::providers::faux::{FauxResponseStep, faux_assistant_message};
    let (faux, apis, runtime) = faux_runtime("old-key");
    type Seen = Vec<(String, Option<String>)>;
    let seen: Arc<std::sync::Mutex<Seen>> = Arc::default();
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let record = {
        let seen = Arc::clone(&seen);
        move |model: &Model, options: &SimpleStreamOptions| {
            seen.lock()
                .expect("seen")
                .push((model.base_url.clone(), options.base.api_key.clone()));
        }
    };
    let held = {
        let (record, started, release) = (record.clone(), started.clone(), release.clone());
        FauxResponseStep::Factory(Arc::new(move |_, options, _, model| {
            record(model, options);
            let (started, release) = (started.clone(), release.clone());
            Box::pin(async move {
                started.notify_one();
                release.notified().await;
                Ok(faux_assistant_message("old"))
            })
        }))
    };
    faux.set_responses(vec![
        held,
        FauxResponseStep::from_fn(move |_, options, _, model| {
            record(model, options);
            Ok(faux_assistant_message("new"))
        }),
    ]);
    let stream_fn = runtime.stream_fn_owned(apis, None);
    let stale = runtime.snapshot().model("faux", "faux-1").expect("model");
    let first = stream_fn(
        &stale,
        &TranscriptContext::default(),
        SimpleStreamOptions::default(),
    )
    .expect("stream");
    started.notified().await;
    let api = faux.model().api.clone();
    runtime
        .try_update(|registry| {
            registry.add_provider_config(
                "faux",
                json!({
                    "baseUrl": "http://127.0.0.2:9/new",
                    "api": api,
                    "models": [{ "id": "faux-1" }],
                }),
                Some("new-key".into()),
            )
        })
        .expect("replace");
    release.notify_one();
    first.result().await.expect("first");
    // The caller still holds the old definition; the request does not.
    let second = stream_fn(
        &stale,
        &TranscriptContext::default(),
        SimpleStreamOptions::default(),
    )
    .expect("stream");
    second.result().await.expect("second");
    assert_eq!(
        *seen.lock().expect("seen"),
        [
            (
                "http://127.0.0.1:9/unused".to_owned(),
                Some("old-key".to_owned())
            ),
            (
                "http://127.0.0.2:9/new".to_owned(),
                Some("new-key".to_owned())
            ),
        ]
    );
}

// Bake: an owned request is a child of its gate's lifetime. Closing the
// gate aborts a request whose own signal never aborts, the provider sees
// the abort and ends the stream as aborted, and the drain waits for the
// request's task; afterwards a request is refused as aborted at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_the_gate_aborts_an_owned_request_and_waits_for_it() {
    use bake_ai::providers::faux::{FauxResponseStep, faux_assistant_message};
    let (faux, apis, runtime) = faux_runtime("key");
    let (started, request_started) = tokio::sync::oneshot::channel::<()>();
    let started = std::sync::Mutex::new(Some(started));
    faux.set_responses(vec![FauxResponseStep::Factory(Arc::new(
        move |_, options, _, _| {
            let signal = options.base.signal.clone();
            if let Some(started) = started.lock().expect("lock").take() {
                let _ = started.send(());
            }
            Box::pin(async move {
                match signal {
                    Some(signal) => signal.cancelled().await,
                    None => return Err("the provider got no signal".to_owned()),
                }
                Ok(faux_assistant_message("never shown"))
            })
        },
    ))]);
    let gate = WorkGate::default();
    let stream_fn = runtime.stream_fn_owned(apis, Some(gate.clone()));
    let model = runtime.snapshot().model("faux", "faux-1").expect("model");
    let request = bake_ai::AbortController::new();
    let mut options = SimpleStreamOptions::default();
    options.base.signal = Some(request.signal());
    let stream = stream_fn(&model, &TranscriptContext::default(), options).expect("stream");
    tokio::time::timeout(std::time::Duration::from_secs(10), request_started)
        .await
        .expect("the request starts")
        .expect("started");
    tokio::time::timeout(std::time::Duration::from_secs(10), gate.close())
        .await
        .expect("the drain finishes");
    assert!(!request.signal().aborted(), "only the lifetime aborted");
    let result = stream.result().await.expect("result");
    assert_eq!(result.stop_reason, StopReason::Aborted);
    let refused = stream_fn(
        &model,
        &TranscriptContext::default(),
        SimpleStreamOptions::default(),
    )
    .expect("stream");
    assert_eq!(
        refused.result().await.map(|message| message.stop_reason),
        Some(StopReason::Aborted)
    );
    assert_eq!(faux.call_count(), 1);
}

/// Sets its flag when dropped: the end of a provider task's cleanup.
struct ProviderCleanup(Arc<AtomicBool>);

impl Drop for ProviderCleanup {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A faux response whose provider task reports its start and the abort it
/// sees, then holds its cleanup until released. The cleanup flag is set
/// when the provider's future, and so its task, is dropped.
struct HeldProvider {
    started: tokio::sync::oneshot::Receiver<bake_ai::AbortSignal>,
    release: tokio::sync::oneshot::Sender<()>,
    cleaned: Arc<AtomicBool>,
}

fn held_provider(faux: &bake_ai::providers::faux::FauxProvider) -> HeldProvider {
    use bake_ai::providers::faux::{FauxResponseStep, faux_assistant_message};
    let (started_tx, started) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    let parts = std::sync::Mutex::new(Some((started_tx, released)));
    let cleaned = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cleaned);
    faux.set_responses(vec![FauxResponseStep::Factory(Arc::new(
        move |_, options, _, _| {
            let signal = options.base.signal.clone();
            let parts = parts.lock().expect("lock").take();
            let cleanup = ProviderCleanup(Arc::clone(&flag));
            Box::pin(async move {
                let _cleanup = cleanup;
                let (Some(signal), Some((started, released))) = (signal, parts) else {
                    return Err("the provider got no signal".to_owned());
                };
                let _ = started.send(signal.clone());
                signal.cancelled().await;
                // Cleanup that outlasts the abort.
                let _ = released.await;
                Ok(faux_assistant_message("never shown"))
            })
        },
    ))]);
    HeldProvider {
        started,
        release,
        cleaned,
    }
}

const OWNED_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

// Bake: closing the gate waits for the provider's own task, not only the
// request task that forwards its stream. The consumer is gone, so the
// request task has nothing left to forward; the provider holds its cleanup
// after the abort, and the drain must not finish until it ends. The window
// below only bounds how long a broken drain gets to finish early; with the
// rule in place the drain cannot finish before the release at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_the_gate_waits_for_the_provider_task() {
    let (faux, apis, runtime) = faux_runtime("key");
    let held = held_provider(&faux);
    let gate = WorkGate::default();
    let stream_fn = runtime.stream_fn_owned(apis, Some(gate.clone()));
    let model = runtime.snapshot().model("faux", "faux-1").expect("model");
    let stream = stream_fn(
        &model,
        &TranscriptContext::default(),
        SimpleStreamOptions::default(),
    )
    .expect("stream");
    let provider_signal = tokio::time::timeout(OWNED_WAIT, held.started)
        .await
        .expect("the provider starts")
        .expect("started");
    drop(stream);
    let closing = tokio::spawn(gate.close());
    tokio::time::timeout(OWNED_WAIT, provider_signal.cancelled())
        .await
        .expect("the provider sees the abort");
    let mut closing = closing;
    let finished_early = tokio::time::timeout(std::time::Duration::from_millis(500), &mut closing)
        .await
        .is_ok();
    let cleaned_early = held.cleaned.load(Ordering::SeqCst);
    let _ = held.release.send(());
    if !finished_early {
        tokio::time::timeout(OWNED_WAIT, closing)
            .await
            .expect("the drain finishes")
            .expect("joined");
    }
    let cleaned = held.cleaned.load(Ordering::SeqCst);
    assert!(!cleaned_early);
    assert!(
        !finished_early,
        "the drain finished before the provider's task"
    );
    assert!(cleaned, "the drain returned before the provider's cleanup");
}

// Bake: a consumer that drops its stream stops the provider, without any
// abort signal or gate close: the provider's signal aborts, and its task
// ends and is awaited by the request task, which the gate then drains at
// once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_consumer_stops_and_awaits_the_provider() {
    let (faux, apis, runtime) = faux_runtime("key");
    let held = held_provider(&faux);
    let gate = WorkGate::default();
    let stream_fn = runtime.stream_fn_owned(apis, Some(gate.clone()));
    let model = runtime.snapshot().model("faux", "faux-1").expect("model");
    let request = bake_ai::AbortController::new();
    let mut options = SimpleStreamOptions::default();
    options.base.signal = Some(request.signal());
    let stream = stream_fn(&model, &TranscriptContext::default(), options).expect("stream");
    let provider_signal = tokio::time::timeout(OWNED_WAIT, held.started)
        .await
        .expect("the provider starts")
        .expect("started");
    drop(stream);
    let stopped = tokio::time::timeout(OWNED_WAIT, provider_signal.cancelled())
        .await
        .is_ok();
    let _ = held.release.send(());
    // Cleanup independent of the rule under test.
    tokio::time::timeout(OWNED_WAIT, gate.close())
        .await
        .expect("the drain finishes");
    assert!(stopped, "a dropped consumer left the provider running");
    assert!(
        !request.signal().aborted(),
        "only the provider's signal aborted"
    );
    assert!(held.cleaned.load(Ordering::SeqCst));
}

/// A provider that runs the payload hook and reports whether the signal it
/// checks before sending is aborted then, as the HTTP adapters check it.
struct PayloadProbe {
    api: String,
    aborted: std::sync::Mutex<Option<bool>>,
}

impl bake_ai::ApiProvider for PayloadProbe {
    fn api(&self) -> &str {
        &self.api
    }

    fn stream(
        &self,
        model: &bake_ai::Model,
        context: &TranscriptContext,
        options: bake_ai::StreamOptions,
    ) -> bake_ai::AssistantMessageEventStream {
        self.stream_simple(model, context, options.into())
    }

    fn stream_simple(
        &self,
        model: &bake_ai::Model,
        _: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> bake_ai::AssistantMessageEventStream {
        if let Some(hook) = &options.base.on_payload {
            hook(&json!({}), model);
        }
        *self.aborted.lock().expect("lock") = Some(
            options
                .base
                .signal
                .as_ref()
                .is_some_and(bake_ai::AbortSignal::aborted),
        );
        let (sender, stream) = bake_ai::assistant_message_channel();
        let mut message = bake_ai::AssistantMessage::pending(model);
        message.stop_reason = StopReason::Aborted;
        sender.finish(message);
        stream
    }
}

// Bake: an abort made while the payload hook runs reaches the provider's
// signal before the hook returns, so the provider never sends that payload.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_abort_from_the_payload_hook_stops_the_send() {
    let (faux, apis, runtime) = faux_runtime("key");
    let probe = Arc::new(PayloadProbe {
        api: faux.model().api.clone(),
        aborted: std::sync::Mutex::new(None),
    });
    apis.register(probe.clone());
    let stream_fn = runtime.stream_fn_owned(apis, None);
    let model = runtime.snapshot().model("faux", "faux-1").expect("model");
    for refuse in [false, true] {
        let request = bake_ai::AbortController::new();
        let mut options = SimpleStreamOptions::default();
        options.base.signal = Some(request.signal());
        let hook_request = request.clone();
        options.base.on_payload = Some(Arc::new(move |_, _| {
            if refuse {
                hook_request.abort();
            }
            None
        }));
        let stream = stream_fn(&model, &TranscriptContext::default(), options).expect("stream");
        tokio::time::timeout(OWNED_WAIT, stream.result())
            .await
            .expect("the request ends");
        assert_eq!(*probe.aborted.lock().expect("lock"), Some(refuse));
    }
}

/// A loopback server for one streaming request: it answers with SSE
/// headers and holds the body open, and reports when the client closes the
/// connection. The test owns the listener, its port, and the task.
struct HeldServer {
    base: String,
    request_seen: tokio::sync::oneshot::Receiver<()>,
    disconnected: tokio::sync::oneshot::Receiver<()>,
    task: tokio::task::JoinHandle<()>,
}

async fn held_server() -> HeldServer {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let base = format!("http://{}/v1", listener.local_addr().expect("address"));
    let (seen_tx, request_seen) = tokio::sync::oneshot::channel();
    let (gone_tx, disconnected) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut head = Vec::new();
        let mut chunk = [0u8; 4096];
        // Read the request head and its body, which the client sends at
        // once; the length is unknown here, so read until the head ends.
        loop {
            match socket.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(read) => head.extend_from_slice(&chunk[..read]),
            }
            if head.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let response = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
        if socket.write_all(response.as_bytes()).await.is_err() {
            return;
        }
        let _ = seen_tx.send(());
        // Hold the body open; what the client sends next (the rest of its
        // body) is discarded until it closes the connection.
        loop {
            match socket.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let _ = gone_tx.send(());
    });
    HeldServer {
        base,
        request_seen,
        disconnected,
        task,
    }
}

fn http_runtime(base: &str) -> ModelRuntime {
    let mut registry = registry(json!({}), "{}");
    registry
        .add_provider_config(
            "held",
            json!({
                "baseUrl": base,
                "api": "openai-responses",
                "models": [{ "id": "held-1" }],
            }),
            Some("test-key".into()),
        )
        .expect("held provider");
    ModelRuntime::new(registry)
}

// Bake: over a real HTTP stream, a consumer that drops its stream makes
// the provider close its connection, which the server observes, while the
// gate stays open; the gate then drains at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_consumer_closes_the_provider_connection() {
    let server = held_server().await;
    let runtime = http_runtime(&server.base);
    let gate = WorkGate::default();
    let stream_fn =
        runtime.stream_fn_owned(Arc::new(ApiRegistry::with_builtins()), Some(gate.clone()));
    let model = runtime.snapshot().model("held", "held-1").expect("model");
    let stream = stream_fn(
        &model,
        &TranscriptContext::default(),
        SimpleStreamOptions::default(),
    )
    .expect("stream");
    tokio::time::timeout(OWNED_WAIT, server.request_seen)
        .await
        .expect("the request reaches the server")
        .expect("seen");
    drop(stream);
    let disconnected = tokio::time::timeout(OWNED_WAIT, server.disconnected)
        .await
        .is_ok();
    tokio::time::timeout(OWNED_WAIT, gate.close())
        .await
        .expect("the drain finishes");
    server.task.abort();
    let _ = server.task.await;
    assert!(disconnected, "the provider kept its connection open");
}

// Bake: closing the gate under a real HTTP stream aborts the provider and
// waits for its task; the server then sees the connection close.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_the_gate_closes_the_provider_connection() {
    let server = held_server().await;
    let runtime = http_runtime(&server.base);
    let gate = WorkGate::default();
    let stream_fn =
        runtime.stream_fn_owned(Arc::new(ApiRegistry::with_builtins()), Some(gate.clone()));
    let model = runtime.snapshot().model("held", "held-1").expect("model");
    let stream = stream_fn(
        &model,
        &TranscriptContext::default(),
        SimpleStreamOptions::default(),
    )
    .expect("stream");
    tokio::time::timeout(OWNED_WAIT, server.request_seen)
        .await
        .expect("the request reaches the server")
        .expect("seen");
    tokio::time::timeout(OWNED_WAIT, gate.close())
        .await
        .expect("the drain finishes");
    let result = tokio::time::timeout(OWNED_WAIT, stream.result())
        .await
        .expect("the stream ends");
    let disconnected = tokio::time::timeout(OWNED_WAIT, server.disconnected)
        .await
        .is_ok();
    server.task.abort();
    let _ = server.task.await;
    assert_eq!(
        result.map(|message| message.stop_reason),
        Some(StopReason::Aborted)
    );
    assert!(disconnected, "the provider kept its connection open");
}
