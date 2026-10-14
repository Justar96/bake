//! The route a login saves and its Pi `models.json` provider config.
//!
//! The route is the `llm-pi-ai.providers.cliproxyapi` entry
//! `configureCliProxyApi` writes (`apps/tui/packages/app/src/cliproxyapi.ts`),
//! with the defaults `CLIPROXYAPI_ROUTE_DEFAULTS` and the in-place upgrade
//! `planCliProxyRouteUpgrade`. [`CliProxyRoute::to_pi_provider_config`] maps
//! it onto one provider of Pi's `models.json` (Pi v1.1.0
//! `packages/coding-agent/src/core/model-config.ts`, `ProviderConfigSchema`;
//! `docs/models.md`), resolving each model as Bake's `llm-pi-ai` did
//! (`packages/llm/llm-pi-ai/src/catalog.ts`, `resolveRouteCatalog` and
//! `resolveModelReasoning`).
//!
//! # Mapping onto Pi
//!
//! | Route field | Pi `models.json` |
//! |---|---|
//! | `displayName` | provider `name`, left out when empty, which 0.3 accepted and Pi refuses |
//! | `baseURL` | provider `baseUrl` |
//! | `api` | provider `api` |
//! | `apiKeyEnv` | provider `apiKey` as `$NAME`, Pi's environment reference; the key itself never enters the config |
//! | `headers` | provider `headers`, each value escaped with [`escape_pi_config_literal`] so Pi's config-value resolver yields it unchanged: 0.3 sent header values literally, while Pi runs a value starting with `!` as a shell command and replaces `$NAME` and `${NAME}` from the environment |
//! | `compat` (`sendSessionAffinityHeaders`) | provider `compat`, which Pi merges into each model's under the model's own fields, as Bake's per-field route default did |
//! | `retryPolicy` (`backoff.maxDelayMs` 60 s) | no field; Pi's `retry.provider.maxRetryDelayMs` and `retry.maxAgentDelayMs` settings already default to 60 s |
//! | model `api`, `baseURL` | model `api`, `baseUrl`: the Anthropic Messages models' proxy root |
//! | model `name` | model `name`; the id when the name is absent or empty, which 0.3 accepted and Pi refuses |
//! | model `reasoningEfforts` | model `reasoning` and `thinkingLevelMap`: each offered level maps to its wire value, every level not offered to `null` (unsupported), and a valueless `off` is left out; `false` or absent is `reasoning: false` |
//! | model `compat.forceAdaptiveThinking` | model `compat.forceAdaptiveThinking`, which Pi's Anthropic adapter reads the same way |
//! | model `contextWindow`, `maxTokens`, `input` | the same fields, with Bake's route fallbacks (`defaultContextWindow` 262,144, `defaultMaxTokens` 32,768, `defaultInput` `[text]`) written out, because Pi's own defaults (128,000, 16,384) differ |
//!
//! Bake-only fields with no Pi counterpart are not carried: a model's
//! `description`, `systemPromptUpdate`, and `toolUpdate`, and the route's
//! request-tuning fields (`timeoutMs`, `streamIdleTimeoutMs`, image budgets,
//! `messagesWire`, `strictTools`, and the like).

use bake_ai::ModelThinkingLevel;
use serde_json::{Map, Value, json};

use super::catalog::{CliProxyModel, Modality};
use super::endpoints::{CliProxyEndpoints, cli_proxy_endpoints};
use super::{CLIPROXYAPI_ID, CLIPROXYAPI_KEY};

/// `DEFAULT_CONTEXT_WINDOW` in `packages/llm/llm-pi-ai/src/config.ts`.
pub const DEFAULT_CONTEXT_WINDOW: u64 = 262_144;
/// `DEFAULT_MAX_TOKENS` in `packages/llm/llm-pi-ai/src/config.ts`.
pub const DEFAULT_MAX_TOKENS: u64 = 32_768;

/// `CLIPROXYAPI_ROUTE_DEFAULTS`: route defaults for a proxy that balances
/// several upstream credentials.
///
/// - `retryPolicy.backoff.maxDelayMs`: once every credential for a model is
///   cooling down, CPA answers `429 model_cooldown` with a `reset_seconds`
///   hint, commonly 30 to 60 seconds, so a retry waits up to a minute.
/// - `compat.sendSessionAffinityHeaders`: provider prompt caches are per
///   credential, so a session must stay on one; CPA's sticky routing keys
///   on `x-session-affinity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CliProxyRouteDefaults {
    /// `retryPolicy.mode`.
    pub retry_mode: &'static str,
    /// `retryPolicy.backoff.maxDelayMs`.
    pub retry_max_delay_ms: u64,
    /// `compat.sendSessionAffinityHeaders`.
    pub send_session_affinity_headers: bool,
}

/// The route defaults' values.
pub const CLIPROXYAPI_ROUTE_DEFAULTS: CliProxyRouteDefaults = CliProxyRouteDefaults {
    retry_mode: "normal",
    retry_max_delay_ms: 60_000,
    send_session_affinity_headers: true,
};

impl CliProxyRouteDefaults {
    /// The `retryPolicy` value.
    pub fn retry_policy_json(&self) -> Value {
        json!({ "mode": self.retry_mode, "backoff": { "maxDelayMs": self.retry_max_delay_ms } })
    }

    /// The defaults as the TypeScript constant spells them.
    pub fn to_json(&self) -> Value {
        json!({
            "retryPolicy": self.retry_policy_json(),
            "compat": { "sendSessionAffinityHeaders": self.send_session_affinity_headers },
        })
    }
}

/// Every Pi thinking level, in escalation order (`THINKING_LEVELS`).
const THINKING_LEVELS: [ModelThinkingLevel; 7] = [
    ModelThinkingLevel::Off,
    ModelThinkingLevel::Minimal,
    ModelThinkingLevel::Low,
    ModelThinkingLevel::Medium,
    ModelThinkingLevel::High,
    ModelThinkingLevel::Xhigh,
    ModelThinkingLevel::Max,
];

/// Parse a Pi thinking level's spelling.
pub(crate) fn thinking_level(value: &str) -> Option<ModelThinkingLevel> {
    THINKING_LEVELS
        .into_iter()
        .find(|level| level.as_str() == value)
}

/// A model's `reasoningEfforts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReasoningEfforts {
    /// `false`: a non-reasoning model.
    Disabled,
    /// Offered levels and their wire values; only `off` may have none.
    Levels(Vec<(ModelThinkingLevel, Option<String>)>),
}

/// One `models` entry of the route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteModel {
    /// Model id.
    pub id: String,
    /// Wire protocol, when it leaves the route's.
    pub api: Option<String>,
    /// Endpoint, when it leaves the route's.
    pub base_url: Option<String>,
    /// Display name.
    pub name: Option<String>,
    /// Context window in tokens.
    pub context_window: Option<u64>,
    /// Output limit in tokens.
    pub max_tokens: Option<u64>,
    /// Request modalities; empty states no answer.
    pub input: Option<Vec<Modality>>,
    /// Offered reasoning levels.
    pub reasoning_efforts: Option<ReasoningEfforts>,
    /// pi-ai compatibility switches, kept as written.
    pub compat: Option<Map<String, Value>>,
}

fn modalities(input: &[Modality]) -> Value {
    Value::Array(
        input
            .iter()
            .map(|modality| json!(modality.as_str()))
            .collect(),
    )
}

impl RouteModel {
    /// The model in Bake's settings shape, with the TypeScript's key order.
    pub fn to_settings_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("id".into(), json!(self.id));
        if let Some(api) = &self.api {
            out.insert("api".into(), json!(api));
        }
        if let Some(base_url) = &self.base_url {
            out.insert("baseURL".into(), json!(base_url));
        }
        if let Some(name) = &self.name {
            out.insert("name".into(), json!(name));
        }
        if let Some(value) = self.context_window {
            out.insert("contextWindow".into(), json!(value));
        }
        if let Some(value) = self.max_tokens {
            out.insert("maxTokens".into(), json!(value));
        }
        if let Some(input) = &self.input {
            out.insert("input".into(), modalities(input));
        }
        match &self.reasoning_efforts {
            None => {}
            Some(ReasoningEfforts::Disabled) => {
                out.insert("reasoningEfforts".into(), Value::Bool(false));
            }
            Some(ReasoningEfforts::Levels(levels)) => {
                let map: Map<String, Value> = levels
                    .iter()
                    .map(|(level, wire)| (level.as_str().to_owned(), json!(wire)))
                    .collect();
                out.insert("reasoningEfforts".into(), Value::Object(map));
            }
        }
        if let Some(compat) = &self.compat {
            out.insert("compat".into(), Value::Object(compat.clone()));
        }
        Value::Object(out)
    }
}

impl From<&CliProxyModel> for RouteModel {
    fn from(model: &CliProxyModel) -> Self {
        Self {
            id: model.id.clone(),
            api: model.api.map(|api| api.as_str().to_owned()),
            base_url: model.base_url.clone(),
            name: Some(model.name.clone()),
            context_window: model.context_window,
            max_tokens: model.max_tokens,
            input: model.input.clone(),
            reasoning_efforts: model.reasoning_efforts.as_ref().map(|efforts| {
                ReasoningEfforts::Levels(
                    efforts
                        .iter()
                        .filter_map(|level| {
                            thinking_level(level).map(|parsed| (parsed, Some(level.clone())))
                        })
                        .collect(),
                )
            }),
            compat: model.force_adaptive_thinking.then(|| {
                let mut compat = Map::new();
                compat.insert("forceAdaptiveThinking".into(), Value::Bool(true));
                compat
            }),
        }
    }
}

/// The `cliproxyapi` route of Bake's `llm-pi-ai` settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliProxyRoute {
    /// `displayName`.
    pub display_name: Option<String>,
    /// `apiKeyEnv`: the credential reference the key resolves through.
    pub api_key_env: Option<String>,
    /// `api`: the route's wire protocol.
    pub api: Option<String>,
    /// `baseURL`: the route's endpoint, `<root>/v1`.
    pub base_url: String,
    /// `headers`.
    pub headers: Option<Map<String, Value>>,
    /// `retryPolicy`, kept as written; Pi has no per-provider field for it.
    pub retry_policy: Option<Value>,
    /// `compat`: per-field defaults for every model.
    pub compat: Option<Map<String, Value>>,
    /// `defaultContextWindow`.
    pub default_context_window: Option<u64>,
    /// `defaultMaxTokens`.
    pub default_max_tokens: Option<u64>,
    /// `defaultInput`.
    pub default_input: Option<Vec<Modality>>,
    /// `models`.
    pub models: Vec<RouteModel>,
}

/// Why a saved route cannot be read: the field's path and what is wrong.
/// Never quotes a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteError {
    /// Dotted path below the route, such as `models[2].contextWindow`.
    pub path: String,
    /// What the field must be.
    pub reason: &'static str,
}

impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.path, self.reason)
    }
}

impl std::error::Error for RouteError {}

/// A model the route lists that Pi cannot be given, and why; Bake's
/// `llm-pi-ai` also left such a model out and kept serving the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedModel {
    /// Model id.
    pub id: String,
    /// Why it was left out.
    pub reason: &'static str,
}

/// A route as one provider of Pi's `models.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct PiProviderConfig {
    /// The provider object, keyed under [`CLIPROXYAPI_ID`] in `providers`.
    pub config: Value,
    /// Models left out.
    pub skipped: Vec<SkippedModel>,
}

fn invalid(path: impl Into<String>, reason: &'static str) -> RouteError {
    RouteError {
        path: path.into(),
        reason,
    }
}

fn optional_string(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Option<String>, RouteError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(_) => Err(invalid(join(path, key), "must be a non-empty string")),
    }
}

/// A string field the 0.3 schema accepts empty (`z.string()`): a model's
/// `name` and the route's `displayName`.
fn optional_text(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Option<String>, RouteError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(invalid(join(path, key), "must be a string")),
    }
}

fn optional_positive(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Option<u64>, RouteError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => match value.as_u64() {
            Some(number) if number > 0 => Ok(Some(number)),
            _ => match value.as_f64() {
                Some(float) if float.fract() == 0.0 && float >= 1.0 && float < u64::MAX as f64 => {
                    Ok(Some(float as u64))
                }
                _ => Err(invalid(join(path, key), "must be a positive integer")),
            },
        },
    }
}

fn optional_object(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Option<Map<String, Value>>, RouteError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(map)) => Ok(Some(map.clone())),
        Some(_) => Err(invalid(join(path, key), "must be a mapping")),
    }
}

fn optional_input(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Option<Vec<Modality>>, RouteError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().and_then(Modality::parse))
            .collect::<Option<Vec<_>>>()
            .map(Some)
            .ok_or_else(|| invalid(join(path, key), "must list only text and image")),
        Some(_) => Err(invalid(join(path, key), "must be a list")),
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

fn reasoning_efforts(
    object: &Map<String, Value>,
    path: &str,
) -> Result<Option<ReasoningEfforts>, RouteError> {
    let path = join(path, "reasoningEfforts");
    match object.get("reasoningEfforts") {
        None => Ok(None),
        Some(Value::Bool(false)) => Ok(Some(ReasoningEfforts::Disabled)),
        // A valueless `reasoningEfforts:` declares nothing; resolution
        // refuses it, as it refuses `{}`.
        Some(Value::Null) => Ok(Some(ReasoningEfforts::Levels(Vec::new()))),
        Some(Value::Object(map)) => {
            let mut levels = Vec::new();
            for (key, value) in map {
                let level = thinking_level(key)
                    .ok_or_else(|| invalid(path.clone(), "names a level Pi does not have"))?;
                let wire = match value {
                    Value::String(wire) => Some(wire.clone()),
                    Value::Null => None,
                    _ => return Err(invalid(path.clone(), "values must be strings")),
                };
                levels.push((level, wire));
            }
            Ok(Some(ReasoningEfforts::Levels(levels)))
        }
        Some(_) => Err(invalid(path, "must be false or a mapping")),
    }
}

impl CliProxyRoute {
    /// The route `configureCliProxyApi` saves for a validated proxy: its
    /// `/v1` base URL over Responses, the listed models, the credential
    /// reference `CLIPROXYAPI_API_KEY`, and the route defaults.
    pub fn from_login(endpoints: &CliProxyEndpoints, models: &[CliProxyModel]) -> Self {
        let mut compat = Map::new();
        compat.insert(
            "sendSessionAffinityHeaders".into(),
            Value::Bool(CLIPROXYAPI_ROUTE_DEFAULTS.send_session_affinity_headers),
        );
        Self {
            display_name: Some("CLIProxyAPI".into()),
            api_key_env: Some(CLIPROXYAPI_KEY.into()),
            api: Some("openai-responses".into()),
            base_url: endpoints.inference.clone(),
            headers: None,
            retry_policy: Some(CLIPROXYAPI_ROUTE_DEFAULTS.retry_policy_json()),
            compat: Some(compat),
            default_context_window: None,
            default_max_tokens: None,
            default_input: None,
            models: models.iter().map(RouteModel::from).collect(),
        }
    }

    /// Read a saved route, the `llm-pi-ai.providers.cliproxyapi` value.
    /// A field of the wrong type is refused with its path; fields Pi has no
    /// counterpart for are ignored.
    pub fn from_settings(value: &Value) -> Result<Self, RouteError> {
        let Value::Object(route) = value else {
            return Err(invalid(CLIPROXYAPI_ID, "must be a mapping"));
        };
        let base_url = optional_string(route, "baseURL", "")?
            .ok_or_else(|| invalid("baseURL", "is required"))?;
        let headers = optional_object(route, "headers", "")?;
        if let Some(headers) = &headers
            && headers.values().any(|value| !value.is_string())
        {
            return Err(invalid("headers", "values must be strings"));
        }
        if let Some(name) = optional_string(route, "apiKeyEnv", "")?
            && !is_credential_ref(&name)
        {
            return Err(invalid("apiKeyEnv", "must be an environment variable name"));
        }
        let default_input = optional_input(route, "defaultInput", "")?;
        if default_input.as_ref().is_some_and(Vec::is_empty) {
            return Err(invalid("defaultInput", "must name at least one modality"));
        }
        let entries = match route.get("models") {
            Some(Value::Array(entries)) => entries,
            None | Some(Value::Null) => return Err(invalid("models", "is required")),
            Some(_) => return Err(invalid("models", "must be a list")),
        };
        let mut models = Vec::with_capacity(entries.len());
        for (index, entry) in entries.iter().enumerate() {
            let path = format!("models[{index}]");
            let Value::Object(entry) = entry else {
                return Err(invalid(path, "must be a mapping"));
            };
            let id = match entry.get("id") {
                Some(Value::String(id)) => id.clone(),
                _ => return Err(invalid(join(&path, "id"), "must be a string")),
            };
            models.push(RouteModel {
                id,
                api: optional_string(entry, "api", &path)?,
                base_url: optional_string(entry, "baseURL", &path)?,
                name: optional_text(entry, "name", &path)?,
                context_window: optional_positive(entry, "contextWindow", &path)?,
                max_tokens: optional_positive(entry, "maxTokens", &path)?,
                input: optional_input(entry, "input", &path)?,
                reasoning_efforts: reasoning_efforts(entry, &path)?,
                compat: optional_object(entry, "compat", &path)?,
            });
        }
        Ok(Self {
            display_name: optional_text(route, "displayName", "")?,
            api_key_env: optional_string(route, "apiKeyEnv", "")?,
            api: optional_string(route, "api", "")?,
            base_url,
            headers,
            retry_policy: route
                .get("retryPolicy")
                .filter(|value| !value.is_null())
                .cloned(),
            compat: optional_object(route, "compat", "")?,
            default_context_window: optional_positive(route, "defaultContextWindow", "")?,
            default_max_tokens: optional_positive(route, "defaultMaxTokens", "")?,
            default_input,
            models,
        })
    }

    /// The route in Bake's settings shape, as `configureCliProxyApi` writes
    /// it, with the TypeScript's key order.
    pub fn to_settings_json(&self) -> Value {
        let mut out = Map::new();
        let mut put = |key: &str, value: Option<Value>| {
            if let Some(value) = value {
                out.insert(key.into(), value);
            }
        };
        put(
            "displayName",
            self.display_name.as_ref().map(|name| json!(name)),
        );
        put(
            "apiKeyEnv",
            self.api_key_env.as_ref().map(|name| json!(name)),
        );
        put("api", self.api.as_ref().map(|api| json!(api)));
        put("baseURL", Some(json!(self.base_url)));
        put(
            "models",
            Some(Value::Array(
                self.models
                    .iter()
                    .map(RouteModel::to_settings_json)
                    .collect(),
            )),
        );
        put("retryPolicy", self.retry_policy.clone());
        put("compat", self.compat.clone().map(Value::Object));
        put("headers", self.headers.clone().map(Value::Object));
        put(
            "defaultContextWindow",
            self.default_context_window.map(|value| json!(value)),
        );
        put(
            "defaultMaxTokens",
            self.default_max_tokens.map(|value| json!(value)),
        );
        put(
            "defaultInput",
            self.default_input.as_ref().map(|input| modalities(input)),
        );
        Value::Object(out)
    }

    /// The credential reference the key resolves through:
    /// `apiKeyEnv`, else the route's own `CLIPROXYAPI_API_KEY`.
    pub fn key_ref(&self) -> &str {
        self.api_key_env.as_deref().unwrap_or(CLIPROXYAPI_KEY)
    }

    /// The route as one provider of Pi's `models.json` (see the module
    /// documentation for each mapping).
    pub fn to_pi_provider_config(&self) -> PiProviderConfig {
        let mut skipped = Vec::new();
        let mut counts = std::collections::HashMap::<&str, usize>::new();
        for model in &self.models {
            *counts.entry(model.id.as_str()).or_default() += 1;
        }
        let default_input = self
            .default_input
            .clone()
            .unwrap_or_else(|| vec![Modality::Text]);
        let mut models = Vec::new();
        for model in &self.models {
            // A later duplicate invalidates the id, the earlier entry too.
            if counts.get(model.id.as_str()).copied().unwrap_or(0) > 1 {
                skipped.push(SkippedModel {
                    id: model.id.clone(),
                    reason: "is listed more than once",
                });
                continue;
            }
            match pi_model(model, self, &default_input) {
                Ok(value) => models.push(value),
                Err(reason) => skipped.push(SkippedModel {
                    id: model.id.clone(),
                    reason,
                }),
            }
        }
        let mut config = Map::new();
        // Pi requires a non-empty `name`; 0.3 accepted an empty one.
        if let Some(name) = self.display_name.as_ref().filter(|name| !name.is_empty()) {
            config.insert("name".into(), json!(name));
        }
        config.insert("baseUrl".into(), json!(self.base_url));
        if let Some(api) = &self.api {
            config.insert("api".into(), json!(api));
        }
        config.insert("apiKey".into(), json!(format!("${}", self.key_ref())));
        if let Some(headers) = &self.headers {
            let escaped = headers
                .iter()
                .map(|(name, value)| {
                    let value = match value {
                        Value::String(text) => Value::String(escape_pi_config_literal(text)),
                        other => other.clone(),
                    };
                    (name.clone(), value)
                })
                .collect();
            config.insert("headers".into(), Value::Object(escaped));
        }
        if let Some(compat) = &self.compat {
            config.insert("compat".into(), Value::Object(compat.clone()));
        }
        config.insert("models".into(), Value::Array(models));
        PiProviderConfig {
            config: Value::Object(config),
            skipped,
        }
    }
}

/// A literal string in the form Pi's `resolveConfigValue` (Pi v1.1.0
/// `packages/coding-agent/src/core/resolve-config-value.ts`) resolves back to
/// that same string: every `$` becomes `$$`, and a leading `!`, which would
/// make Pi run the value as a shell command, becomes `$!`.
pub fn escape_pi_config_literal(value: &str) -> String {
    let escaped = value.replace('$', "$$");
    match escaped.strip_prefix('!') {
        Some(rest) => format!("$!{rest}"),
        None => escaped,
    }
}

/// `credentialRef`'s grammar: a POSIX shell identifier.
pub(crate) fn is_credential_ref(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// One model of the provider config, resolved as `resolveRouteCatalog`
/// resolved it for a route the installed catalog does not ship.
fn pi_model(
    model: &RouteModel,
    route: &CliProxyRoute,
    default_input: &[Modality],
) -> Result<Value, &'static str> {
    if model.id.is_empty() {
        return Err("has an empty id");
    }
    let api = model.api.as_ref().or(route.api.as_ref());
    if api.is_none() {
        return Err("names no wire protocol and the route names none");
    }
    let mut out = Map::new();
    out.insert("id".into(), json!(model.id));
    out.insert(
        "name".into(),
        // Pi requires a non-empty `name`; 0.3 accepted an empty one, and
        // the id stands in for it as it does for an absent one.
        json!(
            model
                .name
                .as_deref()
                .filter(|name| !name.is_empty())
                .unwrap_or(&model.id)
        ),
    );
    if let Some(api) = &model.api {
        out.insert("api".into(), json!(api));
    }
    if let Some(base_url) = &model.base_url {
        out.insert("baseUrl".into(), json!(base_url));
    }
    match &model.reasoning_efforts {
        None | Some(ReasoningEfforts::Disabled) => {
            out.insert("reasoning".into(), Value::Bool(false));
        }
        Some(ReasoningEfforts::Levels(levels)) => {
            out.insert("reasoning".into(), Value::Bool(true));
            out.insert("thinkingLevelMap".into(), thinking_level_map(levels)?);
        }
    }
    let input = model
        .input
        .as_ref()
        .filter(|input| !input.is_empty())
        .map_or(default_input, Vec::as_slice);
    let input: Vec<&str> = input.iter().map(|modality| modality.as_str()).collect();
    out.insert("input".into(), json!(input));
    out.insert(
        "contextWindow".into(),
        json!(
            model
                .context_window
                .or(route.default_context_window)
                .unwrap_or(DEFAULT_CONTEXT_WINDOW)
        ),
    );
    out.insert(
        "maxTokens".into(),
        json!(
            model
                .max_tokens
                .or(route.default_max_tokens)
                .unwrap_or(DEFAULT_MAX_TOKENS)
        ),
    );
    if let Some(compat) = &model.compat {
        out.insert("compat".into(), Value::Object(compat.clone()));
    }
    Ok(Value::Object(out))
}

/// `resolveModelReasoning`: an offered level maps to its wire value, a
/// level not offered to `null`, and a valueless `off` is left out.
fn thinking_level_map(
    levels: &[(ModelThinkingLevel, Option<String>)],
) -> Result<Value, &'static str> {
    if levels.is_empty() {
        return Err("has an empty reasoningEfforts");
    }
    for (level, wire) in levels {
        match wire {
            None if *level != ModelThinkingLevel::Off => {
                return Err("needs a wire value for every level but off");
            }
            Some(wire) if wire.is_empty() => return Err("has an empty reasoning wire value"),
            _ => {}
        }
    }
    if !levels
        .iter()
        .any(|(level, _)| *level != ModelThinkingLevel::Off)
    {
        return Err("offers no reasoning level beyond off");
    }
    let mut map = Map::new();
    for level in THINKING_LEVELS {
        match levels.iter().find(|(offered, _)| *offered == level) {
            None => {
                map.insert(level.as_str().into(), Value::Null);
            }
            Some((_, Some(wire))) => {
                map.insert(level.as_str().into(), json!(wire));
            }
            Some((_, None)) => {}
        }
    }
    Ok(Value::Object(map))
}

/// One thing an in-place upgrade changes on a route an earlier login wrote
/// (`CliProxyRouteChange`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliProxyRouteChange {
    /// Claude over Anthropic Messages, chat-only families over Chat
    /// Completions.
    Protocols,
    /// Adaptive thinking for Claude models that list `xhigh` or `max`.
    AdaptiveThinking,
    /// The retry policy default.
    Retry,
    /// The session-affinity default.
    Affinity,
}

/// A route upgrade as `set` operations on paths below the `llm-pi-ai`
/// namespace (`CliProxyRouteUpgradePlan`).
#[derive(Debug, Clone, PartialEq)]
pub struct CliProxyRouteUpgradePlan {
    /// What the plan changes, in first-seen order.
    pub changes: Vec<CliProxyRouteChange>,
    /// `(path, value)` pairs, each path starting `providers.cliproxyapi`.
    pub ops: Vec<(Vec<String>, Value)>,
}

impl CliProxyRouteUpgradePlan {
    /// Apply the plan to the route value in memory; the settings file is
    /// never written.
    pub fn apply_to_route(&self, route: &mut Value) {
        for (path, value) in &self.ops {
            // Every op path starts `providers`, `cliproxyapi`.
            let mut target = &mut *route;
            let Some((last, parents)) = path.get(2..).and_then(<[String]>::split_last) else {
                continue;
            };
            for key in parents {
                let Value::Object(map) = target else {
                    return;
                };
                target = map
                    .entry(key.clone())
                    .or_insert_with(|| Value::Object(Map::new()));
                if !target.is_object() {
                    *target = Value::Object(Map::new());
                }
            }
            if let Value::Object(map) = target {
                map.insert(last.clone(), value.clone());
            }
        }
    }
}

fn push_change(changes: &mut Vec<CliProxyRouteChange>, change: CliProxyRouteChange) {
    if !changes.contains(&change) {
        changes.push(change);
    }
}

/// `planCliProxyRouteUpgrade`: bring a route an earlier login wrote up to
/// what the current login writes, from the saved route alone. Only absent
/// fields are filled; a value the user set, even `false`, is kept. A route
/// whose credential or protocol was changed by hand is left alone.
pub fn plan_cli_proxy_route_upgrade(saved: &Value) -> Option<CliProxyRouteUpgradePlan> {
    let saved = saved.as_object()?;
    if saved.get("apiKeyEnv").and_then(Value::as_str) != Some(CLIPROXYAPI_KEY) {
        return None;
    }
    let base_url = saved.get("baseURL").and_then(Value::as_str)?;
    let entries = saved.get("models").and_then(Value::as_array)?;
    if saved
        .get("api")
        .is_some_and(|api| api.as_str() != Some("openai-responses"))
    {
        return None;
    }
    let root = cli_proxy_endpoints(base_url).ok()?.root;
    let mut changes = Vec::new();
    let models: Vec<Value> = entries
        .iter()
        .map(|entry| {
            let Some(id) = entry.get("id").and_then(Value::as_str) else {
                return entry.clone();
            };
            let Value::Object(mut model) = entry.clone() else {
                return entry.clone();
            };
            if !model.contains_key("api") {
                let api = super::catalog::cli_proxy_api(id, None);
                if api != super::catalog::CliProxyApi::OpenAiResponses {
                    model.insert("api".into(), json!(api.as_str()));
                    push_change(&mut changes, CliProxyRouteChange::Protocols);
                }
            }
            let messages = model.get("api").and_then(Value::as_str) == Some("anthropic-messages");
            if messages && !model.contains_key("baseURL") {
                model.insert("baseURL".into(), json!(root));
                push_change(&mut changes, CliProxyRouteChange::Protocols);
            }
            let adaptive_efforts = model
                .get("reasoningEfforts")
                .and_then(Value::as_object)
                .is_some_and(|efforts| {
                    efforts
                        .keys()
                        .any(|level| level == "xhigh" || level == "max")
                });
            let compat = model.get("compat").and_then(Value::as_object).cloned();
            if messages
                && adaptive_efforts
                && compat
                    .as_ref()
                    .is_none_or(|compat| !compat.contains_key("forceAdaptiveThinking"))
            {
                let mut compat = compat.unwrap_or_default();
                compat.insert("forceAdaptiveThinking".into(), Value::Bool(true));
                model.insert("compat".into(), Value::Object(compat));
                push_change(&mut changes, CliProxyRouteChange::AdaptiveThinking);
            }
            Value::Object(model)
        })
        .collect();
    let route = |key: &str| {
        let mut path = vec!["providers".to_owned(), super::CLIPROXYAPI_ID.to_owned()];
        path.extend(key.split('.').map(str::to_owned));
        path
    };
    let mut ops = Vec::new();
    if !changes.is_empty() {
        ops.push((route("models"), Value::Array(models)));
    }
    if !saved.contains_key("retryPolicy") {
        ops.push((
            route("retryPolicy"),
            CLIPROXYAPI_ROUTE_DEFAULTS.retry_policy_json(),
        ));
        push_change(&mut changes, CliProxyRouteChange::Retry);
    }
    let affinity_set = saved
        .get("compat")
        .and_then(Value::as_object)
        .is_some_and(|compat| compat.contains_key("sendSessionAffinityHeaders"));
    if !affinity_set {
        ops.push((
            route("compat.sendSessionAffinityHeaders"),
            Value::Bool(CLIPROXYAPI_ROUTE_DEFAULTS.send_session_affinity_headers),
        ));
        push_change(&mut changes, CliProxyRouteChange::Affinity);
    }
    (!ops.is_empty()).then_some(CliProxyRouteUpgradePlan { changes, ops })
}

#[cfg(test)]
mod tests;
