//! Models, providers, and request authentication.
//!
//! Ported from Pi `packages/coding-agent/src/core/model-runtime.ts`,
//! `provider-composer.ts`, and `model-registry.ts`, with the auth
//! resolution of `packages/ai/src/auth/resolve.ts` and `models.ts`
//! (v1.1.0), as far as print mode needs them.
//!
//! | Module | Pi source |
//! |---|---|
//! | [`config`] | `core/model-config.ts`, `utils/json.ts` |
//! | [`resolver`] | `core/model-resolver.ts` |
//! | this module | `core/model-runtime.ts`, `core/provider-composer.ts`, `ai/src/auth/resolve.ts` |
//!
//! `bake-ai` ports no built-in catalog, so a registry starts empty. Its
//! providers come from two layers, composed as Pi composes a built-in
//! provider with `models.json`:
//!
//! 1. **Added providers** ([`ModelRegistry::add_provider_config`]): a
//!    provider config in `models.json` shape with an optional API key, the
//!    seam through which Bake's CLIProxyAPI route is configured. Each acts
//!    as the base provider of its id.
//! 2. **`models.json`** under the Bake home, which overlays an added
//!    provider of the same id (its `baseUrl`, `compat`, models, overrides,
//!    API key, and headers win) or defines a provider of its own.
//!
//! A request's API key is, in order: the runtime key (`--api-key`), the
//! provider's credential in `auth.json`, the `models.json` `apiKey` value,
//! then the added provider's key. Configuration values may be commands or
//! environment templates ([`crate::config_value`]). Headers from the
//! provider and model configs are resolved the same way; `authHeader: true`
//! adds `Authorization: Bearer <key>`.
//!
//! # Not ported
//!
//! Built-in providers and their environment-variable keys, remote catalogs,
//! the models store, OAuth providers and login, virtual models, extension
//! providers, images and classifiers, and availability refresh. `oauth:
//! "radius"` is accepted but builds an ordinary API-key provider.

pub mod config;
pub mod resolver;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bake_agent::StreamFn;
use bake_ai::{
    ApiRegistry, AssistantMessage, AssistantMessageEventStream, Model, ProviderHeaders,
    SimpleStreamOptions, StopReason, TranscriptContext, assistant_message_channel,
};
use serde_json::Value;

use crate::auth_storage::{AuthStorage, Credential};
use crate::config_value::with_command_cancel;
use crate::config_value::{
    ScopedEnv, config_value_env_var_names, is_command_config_value, resolve_config_value_or_err,
    resolve_headers_or_err,
};
use crate::owned_work::{WorkGate, WorkPermit};
pub use config::{JsonObject, ModelConfig};

/// Resolved request authentication, Pi's `AuthResult`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestAuth {
    /// The API key.
    pub api_key: Option<String>,
    /// Request headers.
    pub headers: Option<Vec<(String, String)>>,
    /// Values scoped to the provider's requests.
    pub env: Option<ScopedEnv>,
    /// Where the key came from, such as `configured API key`.
    pub source: String,
}

/// One layer of Pi's `composeApiKeyAuth`.
#[derive(Debug, Clone)]
struct KeyAuth {
    provider: String,
    /// The layer's `apiKey` configuration value.
    raw_key: Option<String>,
    /// An added provider's literal key.
    literal_key: Option<String>,
    raw_headers: Option<Vec<(String, String)>>,
    auth_header: bool,
    inherited: Option<Box<KeyAuth>>,
}

/// A stored or runtime API-key credential.
#[derive(Debug, Clone)]
struct KeyCredential {
    key: Option<String>,
    env: Option<ScopedEnv>,
}

fn env_is_set(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.is_empty())
}

impl KeyAuth {
    fn check(&self, credential: Option<&KeyCredential>) -> Option<String> {
        if let Some(credential) = credential {
            if let Some(inherited) = &self.inherited {
                return inherited.check(Some(credential));
            }
            return credential
                .key
                .as_ref()
                .map(|_| "stored credential".to_owned());
        }
        if let Some(raw) = &self.raw_key {
            if is_command_config_value(raw) {
                return Some("configured API key".to_owned());
            }
            if config_value_env_var_names(raw)
                .iter()
                .all(|name| env_is_set(name))
            {
                return Some("configured API key".to_owned());
            }
            return None;
        }
        if self.literal_key.is_some() {
            return Some("configured API key".to_owned());
        }
        self.inherited.as_ref()?.check(None)
    }

    fn resolve(&self, credential: Option<&KeyCredential>) -> Result<Option<RequestAuth>, String> {
        let result = if let Some(credential) = credential {
            match &self.inherited {
                Some(inherited) => inherited.resolve(Some(credential))?,
                None => credential.key.as_ref().map(|key| RequestAuth {
                    api_key: Some(key.clone()),
                    headers: None,
                    env: credential.env.clone(),
                    source: "stored credential".to_owned(),
                }),
            }
        } else if let Some(raw) = &self.raw_key {
            let key = resolve_config_value_or_err(
                raw,
                &format!("API key for provider \"{}\"", self.provider),
                None,
            )?;
            match &self.inherited {
                Some(inherited) => inherited.resolve(Some(&KeyCredential {
                    key: Some(key),
                    env: None,
                }))?,
                None => Some(RequestAuth {
                    api_key: Some(key),
                    headers: None,
                    env: None,
                    source: "configured API key".to_owned(),
                }),
            }
        } else if let Some(key) = &self.literal_key {
            Some(RequestAuth {
                api_key: Some(key.clone()),
                headers: None,
                env: None,
                source: "configured API key".to_owned(),
            })
        } else {
            match &self.inherited {
                Some(inherited) => inherited.resolve(None)?,
                None => None,
            }
        };
        let Some(mut result) = result else {
            return Ok(None);
        };
        let mut explicit_env = credential
            .and_then(|credential| credential.env.clone())
            .unwrap_or_default();
        explicit_env.extend(result.env.clone().unwrap_or_default());
        let headers = resolve_headers_or_err(
            self.raw_headers.as_deref(),
            &format!("provider \"{}\"", self.provider),
            Some(&explicit_env),
        )?;
        result.headers = with_configured_auth(&result, headers, self.auth_header)?;
        Ok(Some(result))
    }
}

/// Pi's `withConfiguredAuth`.
fn with_configured_auth(
    auth: &RequestAuth,
    headers: Option<Vec<(String, String)>>,
    auth_header: bool,
) -> Result<Option<Vec<(String, String)>>, String> {
    let mut merged: Option<Vec<(String, String)>> = match (&auth.headers, headers) {
        (None, None) => None,
        (base, extra) => {
            let mut merged = base.clone().unwrap_or_default();
            for (name, value) in extra.unwrap_or_default() {
                set_header_exact(&mut merged, name, value);
            }
            Some(merged)
        }
    };
    if auth_header {
        let Some(key) = &auth.api_key else {
            return Err("authHeader requires a resolved API key".to_owned());
        };
        set_header_exact(
            merged.get_or_insert_with(Vec::new),
            "Authorization".to_owned(),
            format!("Bearer {key}"),
        );
    }
    Ok(merged)
}

/// A JavaScript object spread: same-named keys are replaced in place.
fn set_header_exact(headers: &mut Vec<(String, String)>, name: String, value: String) {
    match headers.iter_mut().find(|(known, _)| *known == name) {
        Some(entry) => entry.1 = value,
        None => headers.push((name, value)),
    }
}

/// Pi's `mergeHeaders`: names compare case-insensitively and the override's
/// spelling wins.
fn merge_headers_ci(
    base: Option<Vec<(String, String)>>,
    extra: Option<Vec<(String, String)>>,
) -> Option<Vec<(String, String)>> {
    if base.is_none() && extra.is_none() {
        return None;
    }
    let mut merged = base.unwrap_or_default();
    for (name, value) in extra.unwrap_or_default() {
        merged.retain(|(known, _)| !known.eq_ignore_ascii_case(&name));
        merged.push((name, value));
    }
    Some(merged)
}

/// A composed provider.
#[derive(Debug, Clone)]
struct Provider {
    id: String,
    name: String,
    models: Vec<Model>,
    /// Raw header values per model id, from model definitions and
    /// overrides.
    model_headers: BTreeMap<String, Vec<(String, String)>>,
    auth: KeyAuth,
}

fn string_member(object: &JsonObject, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn string_record(value: Option<&Value>) -> Option<Vec<(String, String)>> {
    let object = value?.as_object()?;
    Some(
        object
            .iter()
            .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
            .collect(),
    )
}

fn is_object(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Object(_)))
}

/// Pi's `mergeCompat` on JSON objects.
fn merge_compat(base: Option<&Value>, override_value: Option<&Value>) -> Option<Value> {
    let Some(Value::Object(overrides)) = override_value else {
        return base.cloned();
    };
    let base_object = base.and_then(Value::as_object);
    let mut merged = base_object.cloned().unwrap_or_default();
    for (key, value) in overrides {
        merged.insert(key.clone(), value.clone());
    }
    for key in [
        "openRouterRouting",
        "vercelGatewayRouting",
        "chatTemplateKwargs",
        "chatTemplateArgs",
    ] {
        let base_value = base_object.and_then(|object| object.get(key));
        let override_nested = overrides.get(key);
        if is_object(base_value) || is_object(override_nested) {
            let mut nested = base_value
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if let Some(Value::Object(extra)) = override_nested {
                for (name, value) in extra {
                    nested.insert(name.clone(), value.clone());
                }
            }
            merged.insert(key.to_owned(), Value::Object(nested));
        }
    }
    Some(Value::Object(merged))
}

fn shallow_merge(base: Option<&Value>, extra: &Value) -> Value {
    let mut merged = base.and_then(Value::as_object).cloned().unwrap_or_default();
    if let Value::Object(extra) = extra {
        for (key, value) in extra {
            merged.insert(key.clone(), value.clone());
        }
    }
    Value::Object(merged)
}

/// A JSON count as the model's unsigned field: finite positive numbers are
/// truncated.
fn count(value: &Value) -> Value {
    match value.as_f64() {
        Some(number) if number.is_finite() && number > 0.0 => Value::from(number as u64),
        _ => Value::from(0u64),
    }
}

fn model_from_json(model: Value, provider: &str, id: &str) -> Result<Model, String> {
    serde_json::from_value(model)
        .map_err(|error| format!("Provider {provider}, model {id}: {error}"))
}

fn model_to_json(model: &Model) -> JsonObject {
    match serde_json::to_value(model) {
        Ok(Value::Object(object)) => object,
        _ => JsonObject::new(),
    }
}

fn positive_or_err(
    definition: &JsonObject,
    key: &str,
    provider: &str,
    id: &str,
    label: &str,
) -> Result<(), String> {
    if let Some(value) = definition.get(key).and_then(Value::as_f64)
        && value <= 0.0
    {
        return Err(format!("Provider {provider}, model {id}: invalid {label}"));
    }
    Ok(())
}

/// Pi's `modelFromJson`.
fn model_from_definition(
    provider: &str,
    definition: &JsonObject,
    config: &JsonObject,
    defaults: Option<&Model>,
) -> Result<Model, String> {
    let id = string_member(definition, "id").unwrap_or_default();
    let api = string_member(definition, "api")
        .or_else(|| string_member(config, "api"))
        .or_else(|| defaults.map(|model| model.api.clone()))
        .ok_or_else(|| {
            format!(
                "Provider {provider}, model {id}: no \"api\" specified. Set at provider or model level."
            )
        })?;
    let base_url = string_member(definition, "baseUrl")
        .or_else(|| string_member(config, "baseUrl"))
        .or_else(|| defaults.map(|model| model.base_url.clone()))
        .ok_or_else(|| {
            format!("Provider {provider}: \"baseUrl\" is required when defining custom models.")
        })?;
    positive_or_err(definition, "contextWindow", provider, &id, "contextWindow")?;
    positive_or_err(definition, "maxTokens", provider, &id, "maxTokens")?;
    let mut model = JsonObject::new();
    model.insert("id".into(), Value::String(id.clone()));
    model.insert(
        "name".into(),
        Value::String(string_member(definition, "name").unwrap_or_else(|| id.clone())),
    );
    model.insert("api".into(), Value::String(api));
    model.insert("provider".into(), Value::String(provider.to_owned()));
    model.insert("baseUrl".into(), Value::String(base_url));
    model.insert(
        "reasoning".into(),
        Value::Bool(
            definition
                .get("reasoning")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
    );
    for key in [
        "thinkingLevelMap",
        "inputLimits",
        "promptCache",
        "samplingParams",
        "samplingParamsByThinkingLevel",
    ] {
        if let Some(value) = definition.get(key) {
            model.insert(key.into(), value.clone());
        }
    }
    model.insert(
        "input".into(),
        definition
            .get("input")
            .cloned()
            .unwrap_or_else(|| Value::from(vec!["text"])),
    );
    model.insert(
        "cost".into(),
        definition.get("cost").cloned().unwrap_or_else(
            || serde_json::json!({ "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }),
        ),
    );
    model.insert(
        "contextWindow".into(),
        definition
            .get("contextWindow")
            .map_or(Value::from(128_000u64), count),
    );
    model.insert(
        "maxTokens".into(),
        definition
            .get("maxTokens")
            .map_or(Value::from(16_384u64), count),
    );
    if let Some(compat) = merge_compat(config.get("compat"), definition.get("compat")) {
        model.insert("compat".into(), compat);
    }
    model_from_json(Value::Object(model), provider, &id)
}

/// Pi's `findModelDefaults`.
fn find_model_defaults<'a>(models: &'a [Model], id: &str, api: Option<&str>) -> Option<&'a Model> {
    models
        .iter()
        .find(|model| model.id == id)
        .or_else(|| api.and_then(|api| models.iter().find(|model| model.api == api)))
        .or_else(|| {
            models
                .iter()
                .find(|model| model.api == "openai-completions")
        })
        .or_else(|| models.first())
}

/// Pi's `applyModelsJson`.
fn apply_models_json(
    provider: &str,
    base_models: &[Model],
    config: &JsonObject,
) -> Result<Vec<Model>, String> {
    let has = |key: &str| config.contains_key(key);
    if has("oauth") && !has("baseUrl") {
        return Err(format!(
            "Provider {provider}: \"baseUrl\" is required when \"oauth\" is set."
        ));
    }
    let has_models = config
        .get("models")
        .and_then(Value::as_array)
        .is_some_and(|models| !models.is_empty());
    let has_overrides = config
        .get("modelOverrides")
        .and_then(Value::as_object)
        .is_some_and(|overrides| !overrides.is_empty());
    if !has_models
        && !has("baseUrl")
        && !has("headers")
        && !has("compat")
        && !has_overrides
        && !has("apiKey")
        && !has("oauth")
        && !has("authHeader")
    {
        return Err(format!(
            "Provider {provider}: must specify \"baseUrl\", \"headers\", \"compat\", \"modelOverrides\", or \"models\"."
        ));
    }
    let base_url = string_member(config, "baseUrl");
    let radius = config.get("oauth").and_then(Value::as_str) == Some("radius");
    let mut models = Vec::with_capacity(base_models.len());
    for model in base_models {
        let mut object = model_to_json(model);
        if !radius && let Some(url) = &base_url {
            object.insert("baseUrl".into(), Value::String(url.clone()));
        }
        match merge_compat(object.get("compat"), config.get("compat")) {
            Some(compat) => {
                object.insert("compat".into(), compat);
            }
            None => {
                object.remove("compat");
            }
        }
        models.push(model_from_json(Value::Object(object), provider, &model.id)?);
    }
    for definition in config
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
    {
        let id = string_member(definition, "id").unwrap_or_default();
        let api = string_member(definition, "api").or_else(|| string_member(config, "api"));
        let defaults = find_model_defaults(&models, &id, api.as_deref()).cloned();
        let model = model_from_definition(provider, definition, config, defaults.as_ref())?;
        match models.iter().position(|known| known.id == id) {
            Some(index) => models[index] = model,
            None => models.push(model),
        }
    }
    Ok(models)
}

/// Pi's `mergeInputLimits` on JSON.
fn merge_input_limits(base: Option<&Value>, extra: &Value) -> Value {
    let mut merged = shallow_merge(base, extra);
    let base_images = base.and_then(|base| base.get("images"));
    if let Some(images) = extra.get("images") {
        let mut images_merged = shallow_merge(base_images, images);
        let base_resize = base_images.and_then(|images| images.get("resize"));
        match images.get("resize") {
            Some(resize) => {
                if let Value::Object(object) = &mut images_merged {
                    object.insert("resize".into(), shallow_merge(base_resize, resize));
                }
            }
            None => {
                if let (Value::Object(object), Some(resize)) = (&mut images_merged, base_resize) {
                    object.insert("resize".into(), resize.clone());
                }
            }
        }
        if let Value::Object(object) = &mut merged {
            object.insert("images".into(), images_merged);
        }
    } else if let (Value::Object(object), Some(images)) = (&mut merged, base_images) {
        object.insert("images".into(), images.clone());
    }
    merged
}

/// Pi's `applyModelOverride` on a model's JSON.
fn apply_model_override(model: &Model, override_value: &JsonObject) -> Result<Model, String> {
    let mut object = model_to_json(model);
    for key in ["name", "reasoning", "input"] {
        if let Some(value) = override_value.get(key) {
            object.insert(key.into(), value.clone());
        }
    }
    for key in ["contextWindow", "maxTokens"] {
        if let Some(value) = override_value.get(key) {
            object.insert(key.into(), count(value));
        }
    }
    for key in ["thinkingLevelMap", "promptCache", "samplingParams"] {
        if let Some(value) = override_value.get(key) {
            let merged = shallow_merge(object.get(key), value);
            object.insert(key.into(), merged);
        }
    }
    if let Some(limits) = override_value.get("inputLimits") {
        let merged = merge_input_limits(object.get("inputLimits"), limits);
        object.insert("inputLimits".into(), merged);
    }
    if let Some(Value::Object(cost)) = override_value.get("cost") {
        let mut merged = object
            .get("cost")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for key in ["input", "output", "cacheRead", "cacheWrite", "tiers"] {
            if let Some(value) = cost.get(key) {
                merged.insert(key.into(), value.clone());
            }
        }
        object.insert("cost".into(), Value::Object(merged));
    }
    if let Some(Value::Object(levels)) = override_value.get("samplingParamsByThinkingLevel") {
        let mut merged = object
            .get("samplingParamsByThinkingLevel")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for level in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] {
            if let Some(params) = levels.get(level).filter(|params| params.is_object()) {
                let combined = shallow_merge(merged.get(level), params);
                merged.insert(level.into(), combined);
            }
        }
        object.insert(
            "samplingParamsByThinkingLevel".into(),
            Value::Object(merged),
        );
    }
    if let Some(compat) = merge_compat(object.get("compat"), override_value.get("compat")) {
        object.insert("compat".into(), compat);
    }
    model_from_json(Value::Object(object), &model.provider, &model.id)
}

/// Pi's `composeModelProvider` for one config layer over an optional base.
fn compose(
    id: &str,
    base: Option<&Provider>,
    config: &JsonObject,
    literal_key: Option<String>,
) -> Result<Provider, String> {
    let base_models = base.map(|base| base.models.as_slice()).unwrap_or_default();
    let mut models = apply_models_json(id, base_models, config)?;
    let overrides = config.get("modelOverrides").and_then(Value::as_object);
    if let Some(overrides) = overrides {
        for model in &mut models {
            if let Some(Value::Object(override_value)) = overrides.get(&model.id) {
                *model = apply_model_override(model, override_value)?;
            }
        }
    }
    let mut model_headers = base
        .map(|base| base.model_headers.clone())
        .unwrap_or_default();
    for model in &models {
        let mut headers: Vec<(String, String)> = Vec::new();
        let sources = [
            overrides
                .and_then(|overrides| overrides.get(&model.id))
                .and_then(|value| value.get("headers")),
            config
                .get("models")
                .and_then(Value::as_array)
                .and_then(|definitions| {
                    definitions.iter().find(|definition| {
                        definition.get("id").and_then(Value::as_str) == Some(&model.id)
                    })
                })
                .and_then(|definition| definition.get("headers")),
        ];
        for source in sources {
            for (name, value) in string_record(source).unwrap_or_default() {
                set_header_exact(&mut headers, name, value);
            }
        }
        if !headers.is_empty() {
            model_headers.insert(model.id.clone(), headers);
        }
    }
    Ok(Provider {
        id: id.to_owned(),
        name: string_member(config, "name")
            .or_else(|| base.map(|base| base.name.clone()))
            .unwrap_or_else(|| id.to_owned()),
        models,
        model_headers,
        auth: KeyAuth {
            provider: id.to_owned(),
            raw_key: string_member(config, "apiKey"),
            literal_key,
            raw_headers: string_record(config.get("headers")),
            auth_header: config
                .get("authHeader")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            inherited: base.map(|base| Box::new(base.auth.clone())),
        },
    })
}

/// A provider added programmatically, below `models.json`.
#[derive(Debug, Clone)]
struct AddedProvider {
    id: String,
    config: JsonObject,
    api_key: Option<String>,
}

/// Pi's `ModelRuntime`, reduced to what a session needs.
#[derive(Debug, Clone, Default)]
pub struct ModelRegistry {
    config: ModelConfig,
    added: Vec<AddedProvider>,
    auth: AuthStorage,
    providers: Vec<Provider>,
    composition_errors: Vec<(String, String)>,
    configured: BTreeSet<String>,
}

/// The `models.json` path under a Bake home.
pub fn models_path(home: &Path) -> std::path::PathBuf {
    home.join("models.json")
}

/// The `auth.json` path under a Bake home.
pub fn auth_path(home: &Path) -> std::path::PathBuf {
    home.join("auth.json")
}

impl ModelRegistry {
    /// A registry over a loaded `models.json` and credentials.
    pub fn new(config: ModelConfig, auth: AuthStorage) -> Self {
        let mut registry = Self {
            config,
            auth,
            ..Self::default()
        };
        registry.rebuild();
        registry
    }

    /// The registry of a Bake home: `models.json` and `auth.json` under it.
    /// Reading may run `!command` keys of `auth.json`; it blocks.
    pub fn load(home: &Path) -> Self {
        Self::new(
            ModelConfig::load(&models_path(home)),
            AuthStorage::load(&auth_path(home)),
        )
    }

    /// The seam for Bake's CLIProxyAPI route: adds a provider config in
    /// Pi's `models.json` provider shape (a JSON object) with an optional
    /// literal API key, below `$BAKE_HOME/models.json`: a `models.json`
    /// provider of the same id overlays it as Pi overlays a built-in
    /// provider, and stored or runtime credentials still come first. Adding
    /// an id again replaces it. Fails, changing nothing, when the config is
    /// not a valid `models.json` provider.
    pub fn add_provider_config(
        &mut self,
        id: &str,
        config: Value,
        api_key: Option<String>,
    ) -> Result<(), String> {
        config::validate_provider_config(id, &config)?;
        let Value::Object(config) = config else {
            return Err("Invalid provider config: expected an object".to_owned());
        };
        // Built eagerly, as Pi validates a registration.
        compose(id, None, &config, api_key.clone())?;
        self.added.retain(|added| added.id != id);
        self.added.push(AddedProvider {
            id: id.to_owned(),
            config,
            api_key,
        });
        self.rebuild();
        Ok(())
    }

    /// Pi's `setRuntimeApiKey` (`--api-key`): a key for this process that
    /// takes priority over stored credentials.
    pub fn set_runtime_api_key(&mut self, provider: &str, key: &str) {
        self.auth.set_runtime_api_key(provider, key);
        self.refresh_configured();
    }

    fn provider_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.added.iter().map(|added| added.id.clone()).collect();
        for id in self.config.provider_ids() {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    }

    fn rebuild(&mut self) {
        self.providers.clear();
        self.composition_errors.clear();
        for id in self.provider_ids() {
            let added = self.added.iter().find(|added| added.id == id);
            let base = match added {
                Some(added) => match compose(&id, None, &added.config, added.api_key.clone()) {
                    Ok(provider) => Some(provider),
                    Err(error) => {
                        self.composition_errors.push((id.clone(), error));
                        None
                    }
                },
                None => None,
            };
            let provider = match self.config.provider(&id) {
                None => base,
                Some(config) => match compose(&id, base.as_ref(), config, None) {
                    Ok(provider) => Some(provider),
                    Err(error) => {
                        self.composition_errors.push((id.clone(), error));
                        base
                    }
                },
            };
            if let Some(provider) = provider {
                self.providers.push(provider);
            }
        }
        self.refresh_configured();
    }

    fn credential(&self, provider: &str) -> Option<Option<KeyCredential>> {
        match self.auth.read(provider) {
            None => Some(None),
            Some(Credential::ApiKey { key, env }) => Some(Some(KeyCredential { key, env })),
            // An OAuth credential owns its provider; with no OAuth method
            // the provider has no auth, as in Pi.
            Some(Credential::OAuth) => None,
        }
    }

    /// Pi's `checkAuth`: where the provider's key would come from, without
    /// running a configured command.
    pub fn check_auth(&self, provider: &str) -> Option<String> {
        let entry = self.providers.iter().find(|entry| entry.id == provider)?;
        let credential = self.credential(provider)?;
        entry.auth.check(credential.as_ref())
    }

    fn refresh_configured(&mut self) {
        self.configured = self
            .providers
            .iter()
            .filter(|provider| self.check_auth(&provider.id).is_some())
            .map(|provider| provider.id.clone())
            .collect();
    }

    /// Pi's `hasConfiguredAuth`, from the snapshot taken at the last change.
    pub fn has_configured_auth(&self, provider: &str) -> bool {
        self.configured.contains(provider)
    }

    /// Every model, providers in order.
    pub fn models(&self) -> Vec<Model> {
        self.providers
            .iter()
            .flat_map(|provider| provider.models.iter().cloned())
            .collect()
    }

    /// Pi's `getAvailableSnapshot`: models of providers with configured
    /// auth.
    pub fn available(&self) -> Vec<Model> {
        self.providers
            .iter()
            .filter(|provider| self.configured.contains(&provider.id))
            .flat_map(|provider| provider.models.iter().cloned())
            .collect()
    }

    /// The model with `provider` and `id`.
    pub fn model(&self, provider: &str, id: &str) -> Option<Model> {
        self.providers
            .iter()
            .find(|entry| entry.id == provider)?
            .models
            .iter()
            .find(|model| model.id == id)
            .cloned()
    }

    /// Whether a provider of this id exists.
    pub fn has_provider(&self, provider: &str) -> bool {
        self.providers.iter().any(|entry| entry.id == provider)
    }

    /// Pi's `getError`: the `models.json` load error and provider
    /// composition errors.
    pub fn error(&self) -> Option<String> {
        let mut errors: Vec<String> = Vec::new();
        if let Some(error) = self.config.error() {
            errors.push(error.to_owned());
        }
        for (provider, error) in &self.composition_errors {
            errors.push(format!("Provider \"{provider}\": {error}"));
        }
        (!errors.is_empty()).then(|| errors.join("\n\n"))
    }

    /// Pi's `getAuth(model)`: the key and headers of a request, with the
    /// model's headers and its configured headers merged in. `Ok(None)`
    /// when the provider is unknown or not configured; `Err` with Pi's
    /// message when a configured value fails to resolve. May run commands,
    /// so it blocks.
    pub fn get_auth(&self, model: &Model) -> Result<Option<RequestAuth>, String> {
        self.get_auth_with_key(model, None)
    }

    /// [`Self::get_auth`] with an explicit key, which Pi resolves through
    /// the provider's auth as a credential so its headers still apply.
    pub fn get_auth_with_key(
        &self,
        model: &Model,
        api_key: Option<&str>,
    ) -> Result<Option<RequestAuth>, String> {
        let Some(provider) = self
            .providers
            .iter()
            .find(|entry| entry.id == model.provider)
        else {
            return Ok(None);
        };
        let credential = match api_key {
            Some(key) => Some(KeyCredential {
                key: Some(key.to_owned()),
                env: None,
            }),
            None => match self.credential(&model.provider) {
                Some(credential) => credential,
                None => return Ok(None),
            },
        };
        let Some(mut result) = provider
            .auth
            .resolve(credential.as_ref())
            .map_err(|_| format!("API key auth failed for provider {}", provider.id))?
        else {
            return Ok(None);
        };
        let model_headers: Option<Vec<(String, String)>> = model.headers.as_ref().map(|headers| {
            headers
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect()
        });
        let configured = resolve_headers_or_err(
            provider.model_headers.get(&model.id).map(Vec::as_slice),
            &format!("model \"{}/{}\"", model.provider, model.id),
            result.env.as_ref(),
        )?;
        result.headers =
            merge_headers_ci(merge_headers_ci(result.headers, model_headers), configured);
        Ok(Some(result))
    }

    /// A stream function over `apis` that authenticates each request
    /// through this registry, Pi's `ModelRuntime.streamSimple`: the stream
    /// is returned at once and auth resolves on a blocking thread behind
    /// it, Pi's `lazyStream`. Explicit `api_key` and headers in the options
    /// win per field.
    pub fn stream_fn(self: &Arc<Self>, apis: Arc<ApiRegistry>) -> StreamFn {
        self.stream_fn_owned(apis, None)
    }

    /// [`Self::stream_fn`] whose request tasks each hold a permit of
    /// `gate`, so closing the gate waits for them; once it is closed,
    /// requests end at once as aborted.
    pub fn stream_fn_owned(
        self: &Arc<Self>,
        apis: Arc<ApiRegistry>,
        gate: Option<WorkGate>,
    ) -> StreamFn {
        let registry = Arc::clone(self);
        Arc::new(move |model, context, options| {
            let permit = match &gate {
                None => None,
                Some(gate) => match gate.enter() {
                    Some(permit) => Some(permit),
                    None => {
                        let (sender, stream) = assistant_message_channel();
                        sender.finish(aborted(model));
                        return Ok(stream);
                    }
                },
            };
            Ok(lazy_stream(
                LazyRequest {
                    registry: Arc::clone(&registry),
                    apis: Arc::clone(&apis),
                    model: model.clone(),
                    context: context.clone(),
                    options,
                },
                permit,
            ))
        })
    }
}

/// The message of a request aborted before its provider started, as the
/// providers word it.
fn aborted(model: &Model) -> AssistantMessage {
    let mut message = AssistantMessage::pending(model);
    message.stop_reason = StopReason::Aborted;
    message.error_message = Some("Request was aborted".to_owned());
    message
}

fn setup_error(model: &Model, error: &str) -> AssistantMessage {
    let mut message = AssistantMessage::pending(model);
    message.stop_reason = StopReason::Error;
    message.error_message = Some(error.to_owned());
    message
}

async fn resolve_request(
    registry: Arc<ModelRegistry>,
    model: &Model,
    mut options: SimpleStreamOptions,
    cancel: Arc<AtomicBool>,
) -> Result<SimpleStreamOptions, String> {
    if !registry.has_provider(&model.provider) {
        return Err(format!("Unknown provider: {}", model.provider));
    }
    let lookup = model.clone();
    let explicit_key = options.base.api_key.clone();
    let auth = tokio::task::spawn_blocking(move || {
        with_command_cancel(cancel, || {
            registry.get_auth_with_key(&lookup, explicit_key.as_deref())
        })
    })
    .await
    .map_err(|error| error.to_string())??
    .ok_or_else(|| format!("Provider is not configured: {}", model.provider))?;
    if options.base.api_key.is_none() {
        options.base.api_key = auth.api_key.clone();
    }
    let explicit: Option<Vec<(String, String)>> = options.base.headers.as_ref().map(|headers| {
        headers
            .iter()
            .filter_map(|(name, value)| Some((name.clone(), value.clone()?)))
            .collect()
    });
    let removed: Vec<String> = options
        .base
        .headers
        .iter()
        .flatten()
        .filter(|(_, value)| value.is_none())
        .map(|(name, _)| name.clone())
        .collect();
    let merged = merge_headers_ci(auth.headers, explicit);
    options.base.headers = merged.map(|headers| {
        let mut headers: ProviderHeaders = headers
            .into_iter()
            .map(|(name, value)| (name, Some(value)))
            .collect();
        headers.extend(removed.into_iter().map(|name| (name, None)));
        headers
    });
    if let Some(env) = auth.env {
        let mut scoped = env;
        scoped.extend(options.base.env.take().unwrap_or_default());
        options.base.env = Some(scoped);
    }
    Ok(options)
}

/// One request [`lazy_stream`] makes.
struct LazyRequest {
    registry: Arc<ModelRegistry>,
    apis: Arc<ApiRegistry>,
    model: Model,
    context: TranscriptContext,
    options: SimpleStreamOptions,
}

/// Pi's `lazyStream`: returns at once, then authenticates and forwards the
/// provider's stream. A setup failure ends the stream with an error
/// message. The request's abort signal kills a `!command` authentication
/// runs, waits for its blocking thread, and ends the stream as aborted;
/// while forwarding, it ends the task once the consumer is gone. `permit`
/// is held until the task ends.
fn lazy_stream(request: LazyRequest, permit: Option<WorkPermit>) -> AssistantMessageEventStream {
    let LazyRequest {
        registry,
        apis,
        model,
        context,
        options,
    } = request;
    let (sender, stream) = assistant_message_channel();
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        sender.finish(setup_error(
            &model,
            "bake-ai streams must run inside a Tokio runtime",
        ));
        return stream;
    };
    let signal = options.base.signal.clone();
    handle.spawn(async move {
        let _permit = permit;
        let cancelled = async {
            match &signal {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending().await,
            }
        };
        // Set on abort, and if this task is dropped, so that a command
        // the auth thread runs is killed either way.
        struct CancelOnDrop(Arc<AtomicBool>);
        impl Drop for CancelOnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = CancelOnDrop(Arc::clone(&cancel));
        let resolving = resolve_request(registry, &model, options, Arc::clone(&cancel));
        tokio::pin!(resolving);
        let resolved = tokio::select! {
            resolved = &mut resolving => resolved,
            () = cancelled => {
                cancel.store(true, Ordering::SeqCst);
                // The killed command returns promptly; the auth thread is
                // owned work, so wait for it.
                let _ = resolving.await;
                sender.finish(aborted(&model));
                return;
            }
        };
        let options = match resolved {
            Ok(options) => options,
            Err(error) => {
                sender.finish(setup_error(&model, &error));
                return;
            }
        };
        let Some(provider) = apis.get(&model.api) else {
            sender.finish(setup_error(
                &model,
                &format!("No API provider registered for api: {}", model.api),
            ));
            return;
        };
        let inner = provider.stream_simple(&model, &context, options);
        let mut watching = signal.is_some();
        loop {
            let event = tokio::select! {
                event = inner.next() => event,
                () = async {
                    match &signal {
                        Some(signal) => signal.cancelled().await,
                        None => std::future::pending().await,
                    }
                }, if watching => {
                    // The provider ends an aborted stream itself; stop
                    // early only when no one reads it any more.
                    watching = false;
                    if sender.is_closed() {
                        return;
                    }
                    continue;
                }
            };
            let Some(event) = event else { break };
            if sender.is_closed() {
                return;
            }
            sender.push(event);
        }
        sender.end(inner.result().await);
    });
    stream
}

#[cfg(test)]
mod tests;
