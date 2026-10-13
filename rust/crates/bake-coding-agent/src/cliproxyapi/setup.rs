//! Interactive setup, model refresh, and route-upgrade notices for the
//! CLIProxyAPI route: `configureCliProxyApi`, `refreshCliProxyModels`,
//! `withSavedCapabilities`, `cliProxyModelsInUse`, `upgradeCliProxyRoute`,
//! and `cliProxyUpgradeNotice` in `apps/tui/packages/app/src/cliproxyapi.ts`,
//! with the English copy from `apps/tui/packages/ui/src/copy.ts`.
//!
//! The TypeScript reads and writes through async settings and credential
//! services; these functions take synchronous callbacks instead and do no
//! file I/O themselves. A caller on an async runtime must not do blocking
//! disk I/O inside a callback: hand it to `tokio::task::spawn_blocking` (or
//! an equivalent) rather than stalling the executor.

use std::fmt;
use std::future::Future;

use bake_ai::AbortSignal;
use indexmap::IndexMap;
use reqwest::Url;
use serde_json::{Map, Value, json};

use super::catalog::CliProxyModel;
use super::endpoints::{CliProxyUrlError, cli_proxy_endpoints, js_trim};
use super::fetch::{CheckField, FetchCliProxyModelsError, fetch_cli_proxy_models};
use super::import::{ApiKey, KeyRejection, normalize_api_key};
use super::route::{CliProxyRoute, CliProxyRouteChange, plan_cli_proxy_route_upgrade};
use super::{CLIPROXYAPI_DEFAULT_URL, CLIPROXYAPI_ID, CLIPROXYAPI_KEY};

const COPY_SIGN_IN_TITLE: &str = "Sign in";
const COPY_LOGIN_EMPTY: &str = "Type or paste a value, or press Esc to cancel";
const COPY_KEY_INVALID: &str =
    "That key contains spaces or characters an HTTP header cannot carry; paste the key alone";
const COPY_KEY_STORED_LOCALLY: &str = "saved in Bake's credential store, never in the session";
const COPY_CLI_PROXY_URL: &str = "Base URL";
const COPY_CLI_PROXY_KEY: &str = "API key";
const COPY_CLI_PROXY_URL_HINT: &str =
    "The proxy root, /v1, or /backend-api address; leave it empty for the default";
const COPY_CLI_PROXY_BAD_URL: &str = "Enter an http(s) address such as http://127.0.0.1:8317, without a query, fragment, or credentials";
const COPY_CLI_PROXY_UPGRADED: &str = "Updated the CLIProxyAPI route for this version: ";
const COPY_CLI_PROXY_RELOGIN: &str = "The CLIProxyAPI route is from an earlier version and could not be updated; run /login cliproxyapi for: ";
const COPY_CLI_PROXY_CHANGE_SEPARATOR: &str = ", ";
const COPY_CLI_PROXY_CHANGE_PROTOCOLS: &str =
    "Claude over Anthropic Messages and chat-only models over Chat Completions";
const COPY_CLI_PROXY_CHANGE_ADAPTIVE: &str = "adaptive thinking for Claude";
const COPY_CLI_PROXY_CHANGE_RETRY: &str = "waiting out account cooldowns";
const COPY_CLI_PROXY_CHANGE_AFFINITY: &str = "one account per session";

/// What startup did with a route an earlier login wrote
/// (`CliProxyRouteUpgrade`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliProxyRouteUpgrade {
    /// Already current or not a login route.
    Current,
    /// Upgraded in place.
    Upgraded {
        /// What changed, in first-seen order.
        changes: Vec<CliProxyRouteChange>,
    },
    /// The route needs the changes, but settings could not take them in place;
    /// a new `/login cliproxyapi` writes them.
    Relogin {
        /// What a new login would change.
        changes: Vec<CliProxyRouteChange>,
        /// Why the settings write did not happen.
        reason: String,
    },
}

fn change_label(change: CliProxyRouteChange) -> &'static str {
    match change {
        CliProxyRouteChange::Protocols => COPY_CLI_PROXY_CHANGE_PROTOCOLS,
        CliProxyRouteChange::AdaptiveThinking => COPY_CLI_PROXY_CHANGE_ADAPTIVE,
        CliProxyRouteChange::Retry => COPY_CLI_PROXY_CHANGE_RETRY,
        CliProxyRouteChange::Affinity => COPY_CLI_PROXY_CHANGE_AFFINITY,
    }
}

/// `cliProxyUpgradeNotice`: the startup notice for a route upgrade, naming
/// what changed, or what a new login would change when the route could not be
/// upgraded in place; `None` when the route was already current.
pub fn cli_proxy_upgrade_notice(result: &CliProxyRouteUpgrade) -> Option<String> {
    let (prefix, changes) = match result {
        CliProxyRouteUpgrade::Current => return None,
        CliProxyRouteUpgrade::Upgraded { changes } => (COPY_CLI_PROXY_UPGRADED, changes),
        CliProxyRouteUpgrade::Relogin { changes, .. } => (COPY_CLI_PROXY_RELOGIN, changes),
    };
    let joined = changes
        .iter()
        .copied()
        .map(change_label)
        .collect::<Vec<_>>()
        .join(COPY_CLI_PROXY_CHANGE_SEPARATOR);
    Some(format!("{prefix}{joined}"))
}

/// `upgradeCliProxyRoute`: upgrade a saved CLIProxyAPI route in place through
/// a settings mutator so an update reaches it without signing in again.
///
/// `mutate` runs synchronously; on an async runtime it must not block on
/// disk I/O (see the module docs).
pub fn upgrade_cli_proxy_route(
    saved: Option<&Value>,
    writable: bool,
    mutate: impl FnOnce(&[(Vec<String>, Value)]) -> Result<(), String>,
) -> CliProxyRouteUpgrade {
    let Some(saved) = saved else {
        return CliProxyRouteUpgrade::Current;
    };
    let Some(plan) = plan_cli_proxy_route_upgrade(saved) else {
        return CliProxyRouteUpgrade::Current;
    };
    if !writable {
        return CliProxyRouteUpgrade::Relogin {
            changes: plan.changes,
            reason: "the settings file is read-only".into(),
        };
    }
    match mutate(&plan.ops) {
        Ok(()) => CliProxyRouteUpgrade::Upgraded {
            changes: plan.changes,
        },
        Err(reason) => CliProxyRouteUpgrade::Relogin {
            changes: plan.changes,
            reason,
        },
    }
}

/// Capabilities a proxy listing can omit for a model that still has them
/// (`LISTED_CAPABILITIES`).
const LISTED_CAPABILITIES: [&str; 5] = [
    "contextWindow",
    "maxTokens",
    "input",
    "reasoningEfforts",
    "compat",
];

/// An empty array or mapping. JSON `null` is a value, as in the TypeScript,
/// whose emptiness test only drops `undefined` and empty containers.
fn is_empty_capability(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

/// `withSavedCapabilities`: fill the capabilities a listed model lacks from
/// its saved entry. The protocol, endpoint, and name always follow the
/// listing; adaptive thinking travels with the saved efforts it was derived
/// from.
pub fn with_saved_capabilities(
    listed: &CliProxyModel,
    saved: Option<&Map<String, Value>>,
) -> Value {
    let mut out = match listed.to_json() {
        Value::Object(map) => map,
        other => return other,
    };
    let Some(saved) = saved else {
        return Value::Object(out);
    };
    for field in LISTED_CAPABILITIES {
        let Some(value) = saved.get(field) else {
            continue;
        };
        if out.contains_key(field) || is_empty_capability(value) {
            continue;
        }
        if field == "compat" {
            // A listing with its own efforts already derived its own adaptive thinking.
            if listed.reasoning_efforts.is_none()
                && value
                    .as_object()
                    .and_then(|compat| compat.get("forceAdaptiveThinking"))
                    == Some(&Value::Bool(true))
            {
                out.insert("compat".into(), json!({ "forceAdaptiveThinking": true }));
            }
            continue;
        }
        out.insert(field.to_owned(), value.clone());
    }
    Value::Object(out)
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = Map::new();
            for key in keys {
                let field = canonical(&map[key]);
                if is_empty_capability(&field) {
                    continue;
                }
                out.insert(key.clone(), field);
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Why [`refresh_cli_proxy_models`] left the saved list as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshCliProxyModelsError {
    /// The proxy could not be read, or the caller's signal aborted.
    Fetch(FetchCliProxyModelsError),
    /// The settings mutator refused the new model list.
    SettingsWrite(String),
}

impl fmt::Display for RefreshCliProxyModelsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fetch(error) => error.fmt(f),
            Self::SettingsWrite(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for RefreshCliProxyModelsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Fetch(error) => Some(error),
            Self::SettingsWrite(_) => None,
        }
    }
}

impl From<FetchCliProxyModelsError> for RefreshCliProxyModelsError {
    fn from(error: FetchCliProxyModelsError) -> Self {
        Self::Fetch(error)
    }
}

/// `refreshCliProxyModels`: bring a saved CLIProxyAPI route's models up to
/// what the proxy lists now, keeping models in `keep` and preserving saved
/// capabilities a listing leaves out.
///
/// Returns whether the saved list changed. A refusal from `mutate` is
/// returned as [`RefreshCliProxyModelsError::SettingsWrite`], as the
/// TypeScript lets the settings rejection propagate. `mutate` runs
/// synchronously; on an async runtime it must not block on disk I/O (see the
/// module docs).
pub async fn refresh_cli_proxy_models(
    saved: Option<&Value>,
    api_key: Option<&str>,
    writable: bool,
    keep: &[String],
    signal: Option<&AbortSignal>,
    mutate: impl FnOnce(Vec<(Vec<String>, Value)>) -> Result<(), String>,
) -> Result<bool, RefreshCliProxyModelsError> {
    if !writable {
        return Ok(false);
    }
    let Some(saved_value) = saved else {
        return Ok(false);
    };
    let Some(saved_obj) = saved_value.as_object() else {
        return Ok(false);
    };
    if saved_obj.get("apiKeyEnv").and_then(Value::as_str) != Some(CLIPROXYAPI_KEY) {
        return Ok(false);
    }
    let Some(base_url) = saved_obj.get("baseURL").and_then(Value::as_str) else {
        return Ok(false);
    };
    let Some(saved_models) = saved_obj.get("models").and_then(Value::as_array) else {
        return Ok(false);
    };
    if saved_obj
        .get("api")
        .is_some_and(|api| api.as_str() != Some("openai-responses"))
        || plan_cli_proxy_route_upgrade(saved_value).is_some()
    {
        return Ok(false);
    }
    let Ok(endpoints) = cli_proxy_endpoints(base_url) else {
        return Ok(false);
    };
    let Some(key) = api_key else {
        return Ok(false);
    };

    let listed =
        fetch_cli_proxy_models(&endpoints.models, key, signal, Some(&endpoints.root)).await?;
    if signal.is_some_and(AbortSignal::aborted) {
        return Err(FetchCliProxyModelsError::Aborted.into());
    }

    let listed_ids: std::collections::HashSet<&str> =
        listed.iter().map(|model| model.id.as_str()).collect();
    let mut saved_by_id: IndexMap<&str, &Map<String, Value>> = IndexMap::new();
    for entry in saved_models {
        if let Some(obj) = entry.as_object()
            && let Some(id) = obj.get("id").and_then(Value::as_str)
        {
            saved_by_id.insert(id, obj);
        }
    }

    let mut models: Vec<Value> = listed
        .iter()
        .map(|model| with_saved_capabilities(model, saved_by_id.get(model.id.as_str()).copied()))
        .collect();
    for (id, entry) in &saved_by_id {
        if keep.iter().any(|k| k == id) && !listed_ids.contains(id) {
            models.push(Value::Object((*entry).clone()));
        }
    }

    let next_value = Value::Array(models);
    if canonical(&next_value) == canonical(&Value::Array(saved_models.clone())) {
        return Ok(false);
    }

    mutate(vec![(
        vec![
            "providers".to_owned(),
            CLIPROXYAPI_ID.to_owned(),
            "models".to_owned(),
        ],
        next_value,
    )])
    .map_err(RefreshCliProxyModelsError::SettingsWrite)?;
    Ok(true)
}

/// `cliProxyModelsInUse`: the CLIProxyAPI model ids that the session's current
/// model, the new-session default selection, or the subagent allow-list use.
pub fn cli_proxy_models_in_use(
    current: Option<(&str, &str)>,
    default_selection: Option<(&str, &str)>,
    allowed_models: Option<&Value>,
) -> Vec<String> {
    let mut ids = Vec::new();
    let mut push = |provider: &str, model: &str| {
        if provider == CLIPROXYAPI_ID && !ids.iter().any(|known| known == model) {
            ids.push(model.to_owned());
        }
    };
    if let Some((provider, model)) = current {
        push(provider, model);
    }
    if let Some((provider, model)) = default_selection {
        push(provider, model);
    }
    if let Some(entries) = allowed_models.and_then(Value::as_array) {
        for entry in entries {
            if let Some(obj) = entry.as_object()
                && let (Some(provider), Some(model)) = (
                    obj.get("provider").and_then(Value::as_str),
                    obj.get("model").and_then(Value::as_str),
                )
            {
                push(provider, model);
            }
        }
    }
    ids
}

/// Kind of field requested by [`LoginPrompt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginPromptKind {
    /// Plain text input (`kind: 'text'`).
    Text,
    /// Secret input (`kind: 'secret'`).
    Secret,
}

/// Step position within a multi-step login flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoginPromptStep {
    /// 1-based step index.
    pub index: usize,
    /// Total number of steps.
    pub count: usize,
}

/// One question asked during [`configure_cli_proxy_api`], matching
/// `LoginPrompt` in `apps/tui/packages/app/src/login.ts`.
///
/// A secret prompt's `initial` can hold the key typed before an address
/// failure, so its `Debug` output redacts `initial`.
#[derive(Clone, PartialEq, Eq)]
pub struct LoginPrompt {
    /// Input kind (`text` or `secret`).
    pub kind: LoginPromptKind,
    /// Panel title (`"Sign in · CLIProxyAPI"`).
    pub title: String,
    /// Field label (`"Base URL"` or `"API key"`).
    pub message: String,
    /// Step counter.
    pub step: LoginPromptStep,
    /// Default value used when the user submits an empty string.
    pub fallback: Option<String>,
    /// Helper text below the field.
    pub hint: Option<String>,
    /// Validation or check failure from the previous attempt.
    pub error: Option<String>,
    /// Previously typed text restored when re-prompting. For a
    /// [`LoginPromptKind::Secret`] prompt this is the key itself.
    pub initial: Option<String>,
}

impl fmt::Debug for LoginPrompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let initial: &dyn fmt::Debug = match (&self.initial, self.kind) {
            (Some(_), LoginPromptKind::Secret) => &Some("<redacted>"),
            (initial, _) => initial,
        };
        f.debug_struct("LoginPrompt")
            .field("kind", &self.kind)
            .field("title", &self.title)
            .field("message", &self.message)
            .field("step", &self.step)
            .field("fallback", &self.fallback)
            .field("hint", &self.hint)
            .field("error", &self.error)
            .field("initial", initial)
            .finish()
    }
}

/// Error returned by [`configure_cli_proxy_api`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigureCliProxyError<P> {
    /// The saved route's `baseURL` is not a proxy URL; the TypeScript's
    /// `cliProxyEndpoints` throws before any prompt.
    InvalidSavedUrl(CliProxyUrlError),
    /// The caller's [`AbortSignal`] aborted.
    Aborted,
    /// The user declined a prompt.
    Declined(P),
    /// Reading the key stored before this login failed; nothing was written.
    CredentialRead(String),
    /// Storing the API key failed.
    CredentialWrite(String),
    /// Saving the route in settings failed, and the previous key was restored.
    SettingsWrite(String),
    /// Saving the route in settings failed, and restoring the previous key also failed.
    RollbackFailed {
        /// The settings error.
        settings_error: String,
        /// The credential rollback error.
        rollback_error: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetupStep {
    Url,
    Key,
}

/// `configureCliProxyApi`: ask for the proxy's address and key, check them
/// against its model list, then save the key and route, restoring the
/// previous key if saving settings fails.
///
/// `saved_base_url` is the saved route's `baseURL` when it is a string; one
/// that is not a proxy URL ends the setup with
/// [`ConfigureCliProxyError::InvalidSavedUrl`], as the TypeScript throws.
/// After the check passes, `read_credential` returns the key stored now (the
/// TypeScript's `credentials.resolve`), read just before `set_credential`
/// overwrites it; settings refusal restores it, or calls `unset_credential`
/// when there was none. `mutate_settings` receives the route for
/// `llm-pi-ai.providers.cliproxyapi`.
///
/// The callbacks run synchronously; on an async runtime they must not block
/// on disk I/O (see the module docs).
#[allow(clippy::too_many_arguments)]
pub async fn configure_cli_proxy_api<P, F, Fut>(
    saved_base_url: Option<&str>,
    mut prompt: F,
    signal: Option<&AbortSignal>,
    mut checking: impl FnMut(&str),
    read_credential: impl FnOnce() -> Result<Option<String>, String>,
    mut set_credential: impl FnMut(&str) -> Result<(), String>,
    unset_credential: impl FnOnce() -> Result<(), String>,
    mutate_settings: impl FnOnce(Value) -> Result<(), String>,
) -> Result<usize, ConfigureCliProxyError<P>>
where
    F: FnMut(LoginPrompt) -> Fut,
    Fut: Future<Output = Result<String, P>>,
{
    let default_url = match saved_base_url {
        Some(url) => {
            cli_proxy_endpoints(url)
                .map_err(ConfigureCliProxyError::InvalidSavedUrl)?
                .root
        }
        None => CLIPROXYAPI_DEFAULT_URL.to_owned(),
    };
    let title = format!("{COPY_SIGN_IN_TITLE} \u{00b7} CLIProxyAPI");
    let mut step = SetupStep::Url;
    let mut typed_url = String::new();
    let mut typed_key = String::new();
    let mut url_error: Option<String> = None;
    let mut key_error: Option<String> = None;
    // A root `cli_proxy_endpoints` produced parses again; mapping the error
    // keeps the setup panic-free regardless.
    let mut endpoints =
        cli_proxy_endpoints(&default_url).map_err(ConfigureCliProxyError::InvalidSavedUrl)?;
    let valid_key: ApiKey;
    let models: Vec<CliProxyModel>;

    loop {
        if step == SetupStep::Url {
            let answer = prompt(LoginPrompt {
                kind: LoginPromptKind::Text,
                title: title.clone(),
                message: COPY_CLI_PROXY_URL.into(),
                step: LoginPromptStep { index: 1, count: 2 },
                fallback: Some(default_url.clone()),
                hint: Some(COPY_CLI_PROXY_URL_HINT.into()),
                error: url_error.clone(),
                initial: (!typed_url.is_empty()).then(|| typed_url.clone()),
            })
            .await
            .map_err(ConfigureCliProxyError::Declined)?;
            if signal.is_some_and(AbortSignal::aborted) {
                return Err(ConfigureCliProxyError::Aborted);
            }
            typed_url = js_trim(&answer).to_owned();
            let candidate = if typed_url.is_empty() {
                &default_url
            } else {
                &typed_url
            };
            match cli_proxy_endpoints(candidate) {
                Ok(parsed) => {
                    endpoints = parsed;
                    url_error = None;
                    step = SetupStep::Key;
                }
                Err(_) => {
                    url_error = Some(COPY_CLI_PROXY_BAD_URL.into());
                    continue;
                }
            }
        }

        let host = Url::parse(&endpoints.root)
            .ok()
            .and_then(|u| {
                let h = u.host_str()?;
                Some(match u.port() {
                    Some(port) => format!("{h}:{port}"),
                    None => h.to_owned(),
                })
            })
            .unwrap_or_default();

        let answer = prompt(LoginPrompt {
            kind: LoginPromptKind::Secret,
            title: title.clone(),
            message: COPY_CLI_PROXY_KEY.into(),
            step: LoginPromptStep { index: 2, count: 2 },
            fallback: None,
            hint: Some(format!("{host} \u{00b7} {COPY_KEY_STORED_LOCALLY}")),
            error: key_error.clone(),
            initial: (!typed_key.is_empty()).then(|| typed_key.clone()),
        })
        .await
        .map_err(ConfigureCliProxyError::Declined)?;
        if signal.is_some_and(AbortSignal::aborted) {
            return Err(ConfigureCliProxyError::Aborted);
        }
        typed_key = js_trim(&answer).to_owned();
        let checked = match normalize_api_key(&typed_key) {
            Ok(key) => key,
            Err(KeyRejection::Empty) => {
                key_error = Some(COPY_LOGIN_EMPTY.into());
                typed_key.clear();
                continue;
            }
            Err(KeyRejection::IllegalCharacters) => {
                key_error = Some(COPY_KEY_INVALID.into());
                typed_key.clear();
                continue;
            }
        };
        key_error = None;
        checking(&host);

        match fetch_cli_proxy_models(
            &endpoints.models,
            checked.expose(),
            signal,
            Some(&endpoints.root),
        )
        .await
        {
            Ok(fetched) => {
                valid_key = checked;
                models = fetched;
                break;
            }
            Err(FetchCliProxyModelsError::Aborted) => {
                return Err(ConfigureCliProxyError::Aborted);
            }
            Err(FetchCliProxyModelsError::InvalidUrl) => {
                url_error = Some(COPY_CLI_PROXY_BAD_URL.into());
                step = SetupStep::Url;
            }
            Err(FetchCliProxyModelsError::Check(error)) => {
                let text = error.failure.text();
                if error.field() == CheckField::Key {
                    key_error = Some(text);
                    typed_key.clear();
                } else {
                    url_error = Some(text);
                    step = SetupStep::Url;
                }
            }
        }
    }

    if signal.is_some_and(AbortSignal::aborted) {
        return Err(ConfigureCliProxyError::Aborted);
    }

    let previous_key = read_credential().map_err(ConfigureCliProxyError::CredentialRead)?;
    set_credential(valid_key.expose()).map_err(ConfigureCliProxyError::CredentialWrite)?;
    let route = CliProxyRoute::from_login(&endpoints, &models).to_settings_json();
    if let Err(settings_error) = mutate_settings(route) {
        let rollback = match &previous_key {
            Some(prev) => set_credential(prev),
            None => unset_credential(),
        };
        if let Err(rollback_error) = rollback {
            return Err(ConfigureCliProxyError::RollbackFailed {
                settings_error,
                rollback_error,
            });
        }
        return Err(ConfigureCliProxyError::SettingsWrite(settings_error));
    }

    Ok(models.len())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use bake_ai::AbortController;

    use super::*;

    async fn serve_sequence(responses: Vec<(u16, Value)>) -> (u16, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = Arc::clone(&seen);
        let queue = Arc::new(Mutex::new(VecDeque::from(responses)));
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = [0u8; 2048];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                seen_clone
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf[..n]).into_owned());
                let Some((status, body)) = queue.lock().unwrap().pop_front() else {
                    return;
                };
                if status == 0 {
                    drop(socket);
                    continue;
                }
                let body_bytes = body.to_string().into_bytes();
                let status_text = match status {
                    200 => "200 OK",
                    401 => "401 Unauthorized",
                    503 => "503 Service Unavailable",
                    _ => "400 Bad Request",
                };
                let head = format!(
                    "HTTP/1.1 {status_text}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body_bytes.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&body_bytes).await;
                let _ = socket.shutdown().await;
            }
        });
        (port, seen)
    }

    // cliproxyapi.test.ts: "upgrading a route an earlier login wrote" >
    // "writes the upgrade through settings and reports what changed",
    // "asks for a new login when settings cannot take the upgrade", and
    // "names the changes in the startup notice".
    #[test]
    fn upgrades_route_and_formats_startup_notice() {
        let legacy = json!({
            "displayName": "CLIProxyAPI",
            "apiKeyEnv": "CLIPROXYAPI_API_KEY",
            "api": "openai-responses",
            "baseURL": "https://proxy.example/v1",
            "models": [
                { "id": "gpt-test", "name": "GPT" },
                { "id": "claude-test", "name": "Claude", "reasoningEfforts": { "high": "high", "max": "max" } },
                { "id": "glm-test", "name": "GLM" },
            ],
        });

        let mut writes = Vec::new();
        let upgraded = upgrade_cli_proxy_route(Some(&legacy), true, |ops| {
            writes.extend_from_slice(ops);
            Ok(())
        });
        assert_eq!(
            upgraded,
            CliProxyRouteUpgrade::Upgraded {
                changes: vec![
                    CliProxyRouteChange::Protocols,
                    CliProxyRouteChange::AdaptiveThinking,
                    CliProxyRouteChange::Retry,
                    CliProxyRouteChange::Affinity,
                ],
            }
        );
        assert_eq!(writes, plan_cli_proxy_route_upgrade(&legacy).unwrap().ops);

        let read_only = upgrade_cli_proxy_route(Some(&legacy), false, |_| unreachable!());
        assert_eq!(
            read_only,
            CliProxyRouteUpgrade::Relogin {
                changes: vec![
                    CliProxyRouteChange::Protocols,
                    CliProxyRouteChange::AdaptiveThinking,
                    CliProxyRouteChange::Retry,
                    CliProxyRouteChange::Affinity,
                ],
                reason: "the settings file is read-only".into(),
            }
        );

        let refused =
            upgrade_cli_proxy_route(Some(&legacy), true, |_| Err("settings rejected".into()));
        assert_eq!(
            refused,
            CliProxyRouteUpgrade::Relogin {
                changes: vec![
                    CliProxyRouteChange::Protocols,
                    CliProxyRouteChange::AdaptiveThinking,
                    CliProxyRouteChange::Retry,
                    CliProxyRouteChange::Affinity,
                ],
                reason: "settings rejected".into(),
            }
        );

        assert_eq!(
            upgrade_cli_proxy_route(None, true, |_| unreachable!()),
            CliProxyRouteUpgrade::Current
        );

        let changes = vec![CliProxyRouteChange::Retry, CliProxyRouteChange::Affinity];
        assert_eq!(
            cli_proxy_upgrade_notice(&CliProxyRouteUpgrade::Current),
            None
        );
        assert_eq!(
            cli_proxy_upgrade_notice(&CliProxyRouteUpgrade::Upgraded {
                changes: changes.clone()
            })
            .as_deref(),
            Some(
                "Updated the CLIProxyAPI route for this version: waiting out account cooldowns, one account per session"
            )
        );
        assert_eq!(
            cli_proxy_upgrade_notice(&CliProxyRouteUpgrade::Relogin {
                changes,
                reason: "read-only".into(),
            })
            .as_deref(),
            Some(
                "The CLIProxyAPI route is from an earlier version and could not be updated; run /login cliproxyapi for: waiting out account cooldowns, one account per session"
            )
        );
    }

    // cliproxyapi.test.ts: "refreshing a saved route's models".
    #[tokio::test]
    async fn refreshes_a_saved_routes_models_and_keeps_saved_capabilities() {
        let (port, seen) = serve_sequence(vec![
            (
                200,
                json!({ "data": [{ "id": "gpt-new" }, { "id": "claude-new", "owned_by": "anthropic" }] }),
            ),
            (
                200,
                json!({ "data": [
                    { "id": "claude-sonnet-9", "owned_by": "anthropic", "display_name": "Claude Sonnet 9", "input_modalities": ["text"] },
                    { "id": "gpt-old", "supported_reasoning_levels": ["medium"] },
                ] }),
            ),
            (
                200,
                json!({ "data": [{ "id": "gpt-old" }, { "id": "gpt-kept" }] }),
            ),
            (503, json!({})),
        ])
        .await;

        let base_url = format!("http://127.0.0.1:{port}/v1");
        let root_url = format!("http://127.0.0.1:{port}");
        let saved = json!({
            "displayName": "CLIProxyAPI",
            "apiKeyEnv": "CLIPROXYAPI_API_KEY",
            "api": "openai-responses",
            "baseURL": base_url,
            "models": [
                { "id": "gpt-old", "name": "gpt-old", "input": [] },
                { "id": "gpt-kept", "name": "gpt-kept" },
            ],
            "retryPolicy": { "mode": "normal", "backoff": { "maxDelayMs": 60_000 } },
            "compat": { "sendSessionAffinityHeaders": true },
        });

        // 1. Saves the models the proxy lists now, keeping a model still in use.
        let mut writes = Vec::new();
        let changed = refresh_cli_proxy_models(
            Some(&saved),
            Some("proxy-key"),
            true,
            &["gpt-kept".into()],
            None,
            |ops| {
                writes = ops;
                Ok(())
            },
        )
        .await
        .expect("refresh 1");
        assert!(changed);
        assert!(
            seen.lock().unwrap()[0]
                .to_ascii_lowercase()
                .contains("authorization: bearer proxy-key")
        );
        assert_eq!(
            writes,
            vec![(
                vec!["providers".into(), "cliproxyapi".into(), "models".into()],
                json!([
                    { "id": "gpt-new", "name": "gpt-new" },
                    { "id": "claude-new", "api": "anthropic-messages", "baseURL": root_url, "name": "claude-new" },
                    { "id": "gpt-kept", "name": "gpt-kept" },
                ]),
            )]
        );

        // 2. Keeps the saved efforts and limits of a model whose listing stops reporting them.
        let claude = json!({
            "id": "claude-sonnet-9",
            "api": "anthropic-messages",
            "baseURL": root_url,
            "name": "Claude Sonnet 9",
            "contextWindow": 1_000_000,
            "maxTokens": 128_000,
            "input": ["text", "image"],
            "reasoningEfforts": { "low": "low", "medium": "medium", "high": "high", "xhigh": "xhigh", "max": "max" },
            "compat": { "forceAdaptiveThinking": true },
        });
        let mut saved_with_caps = saved.clone();
        saved_with_caps["models"] = json!([
            claude,
            { "id": "gpt-old", "name": "gpt-old", "reasoningEfforts": { "low": "low", "high": "high" } },
        ]);
        let mut writes2 = Vec::new();
        let changed2 = refresh_cli_proxy_models(
            Some(&saved_with_caps),
            Some("proxy-key"),
            true,
            &[],
            None,
            |ops| {
                writes2 = ops;
                Ok(())
            },
        )
        .await
        .expect("refresh 2");
        assert!(changed2);
        let mut expected_claude = claude.clone();
        expected_claude["input"] = json!(["text"]);
        assert_eq!(
            writes2[0].1,
            json!([
                expected_claude,
                { "id": "gpt-old", "name": "gpt-old", "reasoningEfforts": { "medium": "medium" } },
            ])
        );

        // 3. Writes nothing when the list is unchanged, ignoring empty fields.
        let changed3 = refresh_cli_proxy_models(
            Some(&saved),
            Some("proxy-key"),
            true,
            &[],
            None,
            |_| unreachable!(),
        )
        .await
        .expect("refresh 3");
        assert!(!changed3);

        // 4. Leaves routes it does not own, and asks nothing without a key or writable file.
        let call_count_before = seen.lock().unwrap().len();
        for invalid_route in [
            None,
            Some({
                let mut r = saved.clone();
                r["apiKeyEnv"] = json!("OTHER_KEY");
                r
            }),
            Some({
                let mut r = saved.clone();
                r["api"] = json!("openai-completions");
                r
            }),
            Some({
                let mut r = saved.clone();
                r.as_object_mut().unwrap().remove("retryPolicy");
                r
            }),
        ] {
            assert!(
                !refresh_cli_proxy_models(
                    invalid_route.as_ref(),
                    Some("proxy-key"),
                    true,
                    &[],
                    None,
                    |_| unreachable!(),
                )
                .await
                .unwrap()
            );
        }
        assert!(
            !refresh_cli_proxy_models(Some(&saved), None, true, &[], None, |_| unreachable!())
                .await
                .unwrap()
        );
        assert!(
            !refresh_cli_proxy_models(
                Some(&saved),
                Some("proxy-key"),
                false,
                &[],
                None,
                |_| unreachable!(),
            )
            .await
            .unwrap()
        );
        assert_eq!(seen.lock().unwrap().len(), call_count_before);

        // 5. Keeps the saved list when the proxy cannot be read (HTTP 503).
        let err = refresh_cli_proxy_models(
            Some(&saved),
            Some("proxy-key"),
            true,
            &[],
            None,
            |_| unreachable!(),
        )
        .await
        .expect_err("503 fails");
        assert!(matches!(
            err,
            RefreshCliProxyModelsError::Fetch(FetchCliProxyModelsError::Check(_))
        ));
    }

    // cliproxyapi.test.ts: "names the proxy models the session, the default,
    // and the subagent allow-list use".
    #[test]
    fn names_the_proxy_models_in_use() {
        let allowed = json!([
            { "provider": "cliproxyapi", "model": "claude-sub" },
            { "provider": "deepseek", "model": "deepseek-v4-flash" },
            { "provider": "cliproxyapi", "model": "gpt-default" },
        ]);
        assert_eq!(
            cli_proxy_models_in_use(
                Some(("cliproxyapi", "gpt-session")),
                Some(("cliproxyapi", "gpt-default")),
                Some(&allowed),
            ),
            vec!["gpt-session", "gpt-default", "claude-sub"]
        );
        assert_eq!(
            cli_proxy_models_in_use(Some(("openai", "gpt-6")), None, None),
            Vec::<String>::new()
        );
    }

    // cliproxyapi.ts `refreshCliProxyModels`: a settings refusal propagates
    // (`await settings.mutate(...)` rejects) instead of reporting a change.
    #[tokio::test]
    async fn refresh_reports_a_settings_refusal() {
        let (port, _seen) =
            serve_sequence(vec![(200, json!({ "data": [{ "id": "gpt-new" }] }))]).await;
        let saved = json!({
            "displayName": "CLIProxyAPI",
            "apiKeyEnv": "CLIPROXYAPI_API_KEY",
            "api": "openai-responses",
            "baseURL": format!("http://127.0.0.1:{port}/v1"),
            "models": [{ "id": "gpt-old", "name": "gpt-old" }],
            "retryPolicy": { "mode": "normal", "backoff": { "maxDelayMs": 60_000 } },
            "compat": { "sendSessionAffinityHeaders": true },
        });
        let err =
            refresh_cli_proxy_models(Some(&saved), Some("proxy-key"), true, &[], None, |_| {
                Err("settings rejected".into())
            })
            .await
            .expect_err("settings refused");
        assert_eq!(
            err,
            RefreshCliProxyModelsError::SettingsWrite("settings rejected".into())
        );
    }

    // Further branches of `withSavedCapabilities` (cliproxyapi.ts): saved
    // adaptive thinking travels only with the saved efforts it came from, and
    // a saved `null` is a value, not an empty field, in the copy and in
    // `canonical`.
    #[test]
    fn saved_adaptive_thinking_follows_the_efforts_and_null_is_a_value() {
        let listed = CliProxyModel {
            id: "claude-x".into(),
            api: None,
            base_url: None,
            name: "Claude X".into(),
            context_window: None,
            max_tokens: None,
            input: None,
            reasoning_efforts: Some(vec!["high".into()]),
            force_adaptive_thinking: false,
        };
        let saved = json!({
            "id": "claude-x",
            "contextWindow": null,
            "maxTokens": 64_000,
            "input": [],
            "reasoningEfforts": { "low": "low", "max": "max" },
            "compat": { "forceAdaptiveThinking": true },
        });
        let saved = saved.as_object().expect("object");
        assert_eq!(
            with_saved_capabilities(&listed, Some(saved)),
            json!({
                "id": "claude-x",
                "name": "Claude X",
                "contextWindow": null,
                "maxTokens": 64_000,
                "reasoningEfforts": { "high": "high" },
            })
        );
        let without_efforts = CliProxyModel {
            reasoning_efforts: None,
            ..listed.clone()
        };
        assert_eq!(
            with_saved_capabilities(&without_efforts, Some(saved)),
            json!({
                "id": "claude-x",
                "name": "Claude X",
                "contextWindow": null,
                "maxTokens": 64_000,
                "reasoningEfforts": { "low": "low", "max": "max" },
                "compat": { "forceAdaptiveThinking": true },
            })
        );
        assert_eq!(
            canonical(&json!({ "b": null, "a": [], "c": {} })),
            json!({ "b": null })
        );
    }

    // cliproxyapi.test.ts: "saves a validated URL and model route without
    // placing the key in settings" and "restores the previous key when route
    // settings reject a reconfiguration", with an unreadable address and a
    // rejected key asked for again on the way.
    #[tokio::test]
    async fn configures_cli_proxy_api_retrying_and_rolling_back_on_error() {
        let (port, _seen) = serve_sequence(vec![
            (401, json!({})),
            (
                200,
                json!({ "models": [{ "slug": "gpt-test" }, { "slug": "claude-test", "owned_by": "anthropic" }] }),
            ),
            (200, json!({ "data": [{ "id": "gpt-test" }] })),
        ])
        .await;
        let good_url = format!("http://127.0.0.1:{port}/v1");
        let answers = Arc::new(Mutex::new(VecDeque::from(vec![
            "bad url with spaces".to_owned(),
            good_url.clone(),
            "wrong-key".to_owned(),
            "right-key".to_owned(),
        ])));
        let questions = Arc::new(Mutex::new(Vec::new()));
        let mut stored_keys = Vec::new();
        let mut written_route = None;
        let mut checked_hosts = Vec::new();

        let count = configure_cli_proxy_api(
            Some("https://proxy.example/prefix/v1"),
            |q| {
                questions.lock().unwrap().push(q);
                let next = answers.lock().unwrap().pop_front().unwrap();
                async move { Ok::<_, &'static str>(next) }
            },
            None,
            |host| checked_hosts.push(host.to_owned()),
            || Ok(None),
            |key| {
                stored_keys.push(key.to_owned());
                Ok(())
            },
            || Ok(()),
            |route| {
                written_route = Some(route);
                Ok(())
            },
        )
        .await
        .expect("configured");

        assert_eq!(count, 2);
        assert_eq!(stored_keys, vec!["right-key"]);
        let qs = questions.lock().unwrap().clone();
        assert_eq!(qs.len(), 4);
        assert_eq!(
            qs[0].fallback.as_deref(),
            Some("https://proxy.example/prefix")
        );
        assert_eq!(qs[1].error.as_deref(), Some(COPY_CLI_PROXY_BAD_URL));
        assert_eq!(qs[1].initial.as_deref(), Some("bad url with spaces"));
        assert_eq!(qs[2].kind, LoginPromptKind::Secret);
        assert_eq!(qs[2].error, None);
        assert_eq!(
            qs[3].error.as_deref(),
            Some("The proxy rejected this API key (HTTP 401)")
        );
        assert_eq!(qs[3].initial, None);

        let route = written_route.expect("route written");
        assert!(!route.to_string().contains("right-key"));
        assert_eq!(route["baseURL"], json!(good_url));

        // Rollback when settings reject the write; the previous key is read
        // just before the new one is stored.
        let answers2 = Arc::new(Mutex::new(VecDeque::from(vec![
            good_url,
            "new-key".to_owned(),
        ])));
        let mut rollback_keys = Vec::new();
        let err = configure_cli_proxy_api(
            None,
            |_| {
                let next = answers2.lock().unwrap().pop_front().unwrap();
                async move { Ok::<_, &'static str>(next) }
            },
            None,
            |_| {},
            || Ok(Some("old-key".into())),
            |key| {
                rollback_keys.push(key.to_owned());
                Ok(())
            },
            || unreachable!("a previous key is restored, not unset"),
            |_| Err("settings rejected".into()),
        )
        .await
        .expect_err("settings rejected");
        assert_eq!(
            err,
            ConfigureCliProxyError::SettingsWrite("settings rejected".into())
        );
        assert_eq!(rollback_keys, vec!["new-key", "old-key"]);
    }

    // cliproxyapi.test.ts: "asks for the key again after a rejection, and the
    // address again, with what was typed, after an unreachable proxy". The
    // proxy here drops the connection rather than refusing it, and the
    // address answered again is the same loopback one.
    #[tokio::test]
    async fn asks_for_the_address_again_after_an_unreachable_proxy_keeping_the_key() {
        let (port, _seen) = serve_sequence(vec![
            (401, json!({})),
            (0, json!({})),
            (200, json!({ "data": [{ "id": "gpt-test" }] })),
        ])
        .await;
        let url = format!("http://127.0.0.1:{port}");
        let host = format!("127.0.0.1:{port}");
        let answers = Mutex::new(VecDeque::from(vec![
            url.clone(),
            "wrong-key".to_owned(),
            "sk-kept-secret".to_owned(),
            "bad url with spaces".to_owned(),
            url.clone(),
            "sk-kept-secret".to_owned(),
        ]));
        let questions = Mutex::new(Vec::new());
        let mut checked = Vec::new();
        let mut stored = Vec::new();

        let count = configure_cli_proxy_api(
            None,
            |q| {
                questions.lock().unwrap().push(q);
                let next = answers.lock().unwrap().pop_front().unwrap();
                async move { Ok::<_, &'static str>(next) }
            },
            None,
            |h| checked.push(h.to_owned()),
            || Ok(None),
            |key| {
                stored.push(key.to_owned());
                Ok(())
            },
            || Ok(()),
            |_| Ok(()),
        )
        .await
        .expect("configured");

        assert_eq!(count, 1);
        assert_eq!(stored, vec!["sk-kept-secret"]);
        assert_eq!(checked, vec![host.clone(), host.clone(), host.clone()]);
        let qs = questions.into_inner().unwrap();
        let summary: Vec<_> = qs
            .iter()
            .map(|q| (q.kind, q.error.clone(), q.initial.clone()))
            .collect();
        assert_eq!(summary.len(), 6);
        assert_eq!(summary[0], (LoginPromptKind::Text, None, None));
        assert_eq!(summary[1], (LoginPromptKind::Secret, None, None));
        // Rejected: the key alone, fresh.
        assert_eq!(
            summary[2],
            (
                LoginPromptKind::Secret,
                Some("The proxy rejected this API key (HTTP 401)".into()),
                None
            )
        );
        // Unreachable: back to the address, which keeps what was typed.
        assert_eq!(summary[3].0, LoginPromptKind::Text);
        let unreachable = summary[3].1.as_deref().expect("unreachable error");
        assert!(
            unreachable.starts_with(&format!("Could not reach {host} (")),
            "{unreachable}"
        );
        assert_eq!(summary[3].2.as_deref(), Some(url.as_str()));
        // Unreadable address: refused before any request.
        assert_eq!(
            summary[4],
            (
                LoginPromptKind::Text,
                Some(COPY_CLI_PROXY_BAD_URL.into()),
                Some("bad url with spaces".into())
            )
        );
        // The key survives an address problem, so Enter checks it again.
        assert_eq!(
            summary[5],
            (LoginPromptKind::Secret, None, Some("sk-kept-secret".into()))
        );
        // The kept key never shows in a prompt's `Debug` output.
        for q in &qs {
            assert!(!format!("{q:?}").contains("sk-kept-secret"), "{q:?}");
        }
        assert!(format!("{:?}", qs[5]).contains("<redacted>"));
        assert!(format!("{:?}", qs[4]).contains("bad url with spaces"));
    }

    /// One login against a proxy that lists one model, answering the address
    /// and `new-key`. `set_fails_from` makes the credential store refuse
    /// from that call on (1-based). Returns the result, every key passed to
    /// the store, how often the key was unset, and whether settings were
    /// written.
    async fn login_outcome(
        previous: Result<Option<String>, String>,
        set_fails_from: Option<usize>,
        settings: Result<(), String>,
    ) -> (
        Result<usize, ConfigureCliProxyError<&'static str>>,
        Vec<String>,
        usize,
        bool,
    ) {
        let (port, _seen) =
            serve_sequence(vec![(200, json!({ "data": [{ "id": "gpt-test" }] }))]).await;
        let answers = Mutex::new(VecDeque::from(vec![
            format!("http://127.0.0.1:{port}"),
            "new-key".to_owned(),
        ]));
        let mut set_calls = Vec::new();
        let mut unset_calls = 0;
        let mut settings_called = false;
        let result = configure_cli_proxy_api(
            None,
            |_| {
                let next = answers.lock().unwrap().pop_front().unwrap();
                async move { Ok::<_, &'static str>(next) }
            },
            None,
            |_| {},
            || previous,
            |key| {
                set_calls.push(key.to_owned());
                if set_fails_from.is_some_and(|from| set_calls.len() >= from) {
                    Err(format!("store refused call {}", set_calls.len()))
                } else {
                    Ok(())
                }
            },
            || {
                unset_calls += 1;
                Ok(())
            },
            |_| {
                settings_called = true;
                settings
            },
        )
        .await;
        (result, set_calls, unset_calls, settings_called)
    }

    // Further branches of `configureCliProxyApi`'s credential handling
    // (cliproxyapi.ts): no previous key is unset on rollback, a failed
    // restore is reported with both errors (`AggregateError`), and a failed
    // store or read writes no settings.
    #[tokio::test]
    async fn login_credential_failures_and_rollback_without_a_previous_key() {
        let (result, set, unset, settings) =
            login_outcome(Ok(None), None, Err("settings rejected".into())).await;
        assert_eq!(
            result,
            Err(ConfigureCliProxyError::SettingsWrite(
                "settings rejected".into()
            ))
        );
        assert_eq!(set, vec!["new-key"]);
        assert_eq!(unset, 1);
        assert!(settings);

        let (result, set, unset, _) = login_outcome(
            Ok(Some("old-key".into())),
            Some(2),
            Err("settings rejected".into()),
        )
        .await;
        assert_eq!(
            result,
            Err(ConfigureCliProxyError::RollbackFailed {
                settings_error: "settings rejected".into(),
                rollback_error: "store refused call 2".into(),
            })
        );
        assert_eq!(set, vec!["new-key", "old-key"]);
        assert_eq!(unset, 0);

        let (result, set, unset, settings) =
            login_outcome(Ok(Some("old-key".into())), Some(1), Ok(())).await;
        assert_eq!(
            result,
            Err(ConfigureCliProxyError::CredentialWrite(
                "store refused call 1".into()
            ))
        );
        assert_eq!(set, vec!["new-key"]);
        assert_eq!(unset, 0);
        assert!(!settings);

        let (result, set, _, settings) =
            login_outcome(Err("store unreadable".into()), None, Ok(())).await;
        assert_eq!(
            result,
            Err(ConfigureCliProxyError::CredentialRead(
                "store unreadable".into()
            ))
        );
        assert!(set.is_empty());
        assert!(!settings);
    }

    // `configureCliProxyApi` (cliproxyapi.ts): a saved `baseURL` that is not
    // a proxy URL throws before any prompt, and an abort ends the setup.
    #[tokio::test]
    async fn refuses_an_invalid_saved_address_and_stops_on_abort() {
        let asked = Mutex::new(0);
        let result = configure_cli_proxy_api(
            Some("https://proxy.example/?account=1"),
            |_| {
                *asked.lock().unwrap() += 1;
                async { Ok::<String, &'static str>(String::new()) }
            },
            None,
            |_| {},
            || unreachable!(),
            |_| unreachable!(),
            || unreachable!(),
            |_| unreachable!(),
        )
        .await;
        assert_eq!(
            result,
            Err(ConfigureCliProxyError::InvalidSavedUrl(
                CliProxyUrlError::Disallowed
            ))
        );
        assert_eq!(*asked.lock().unwrap(), 0);

        let controller = AbortController::new();
        let signal = controller.signal();
        let result = configure_cli_proxy_api(
            None,
            |_| {
                controller.abort();
                async { Ok::<String, &'static str>(String::new()) }
            },
            Some(&signal),
            |_| {},
            || unreachable!(),
            |_| unreachable!(),
            || unreachable!(),
            |_| unreachable!(),
        )
        .await;
        assert_eq!(result, Err(ConfigureCliProxyError::Aborted));
    }
}
