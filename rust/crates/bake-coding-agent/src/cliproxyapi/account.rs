//! The CLIProxyAPI account `bake` keeps in the Bake home: sign-in,
//! sign-out, and catalog refresh, written to `settings.json` and
//! `auth.json` only. D25 reads Bake 0.3's `settings.yaml` and
//! `.credentials.yaml` through [`super::import`] and never writes them.
//!
//! The behavior follows Bake's retained contract:
//! `apps/tui/packages/app/src/cliproxyapi.ts` (`configureCliProxyApi`,
//! `refreshCliProxyModels`) and `apps/tui/packages/app/src/login.ts`
//! (`logout`, which removes the route, then the key). The files and the
//! binding below belong to Bake 0.4.
//!
//! # Format
//!
//! - **Route:** the `cliproxyapi` member of `settings.json`
//!   ([`crate::settings::CliProxySetting`]). It holds
//!   [`CliProxyRoute::to_settings_json`] plus an `account` member, an
//!   opaque generation. `null` means signed out. Any present value
//!   suppresses the 0.3 route import.
//! - **Key:** `auth.json`'s `cliproxyapi` record, a Pi API-key credential
//!   with the same `account` member.
//!
//! # Binding
//!
//! The two files cannot change in one atomic step, and a reader cannot
//! read both at one instant. So the account is a route and a key that
//! carry the same generation. Every sign-in draws a new random generation.
//! [`read_account`] uses the route only with the key whose generation
//! matches; otherwise the account is [`AccountView::Disabled`]. Startup,
//! [`CliProxyAccount::apply_to`] (the running session's registry), and
//! [`refresh_account`] all read through it.
//!
//! A sign-in writes the key, then the route. A sign-out writes `null`, then
//! removes the key. The binding holds whatever happens in between:
//!
//! - a crash or a failed write between the two steps;
//! - a power loss that keeps either write and not the other (neither file
//!   is assumed durable; Windows cannot flush a directory at all);
//! - a reader that reads the route before a sign-in and the key after it;
//! - another process's direct `auth.json` write, such as Pi's
//!   `set_api_key`, which drops the `account` member.
//!
//! In each of these cases readers find either a matching pair or a
//! disabled account. A bound `auth.json` record is no credential to
//! [`AuthStorage`]'s ordinary reads, so even when `settings.json` has lost
//! its member and the 0.3 route is imported again, the bound key is never
//! sent there. A `settings.json` that cannot be read disables the account
//! and suppresses the import, rather than falling back. No reader finds a
//! route with another sign-in's key.
//! Disabled shows as a startup warning and, in the session, as no
//! `cliproxyapi` provider; signing in again recovers. The registry takes
//! the matched key as a bound key
//! ([`ModelRegistry::bind_provider_key`]), so a later `auth.json` read
//! cannot pair the route with another key. The runtime key (`--api-key`)
//! still comes first, as for every provider.
//!
//! A sign-in whose route write fails puts the key it replaced back, but
//! only while `auth.json` still holds this sign-in's own record (its
//! generation proves ownership). Otherwise it leaves the account disabled.
//! A refresh changes only the route's `models`, keeps its generation, and
//! writes only while the route is still the one it fetched for.
//!
//! An exclusive lock on `.cliproxyapi.account.lock` in the home makes the
//! sign-ins, sign-outs, and refreshes of every `bake` process take
//! turns. Safety does not depend on it, because each file write holds that
//! file's own lock and changes only its own member. A write whose directory
//! flush fails ([`StoreError::Unsynced`]) counts as written, and is
//! reported as possibly not durable.
//!
//! A `cliproxyapi` provider in `models.json` belongs to the person. It
//! overlays the account's route, so a sign-in is refused while it is there.
//! A sign-out does not touch it.

use std::path::{Path, PathBuf};

use bake_ai::AbortSignal;
use serde_json::{Map, Value, json};

use super::CLIPROXYAPI_ID;
use super::route::CliProxyRoute;
use super::setup::{CheckedLogin, RefreshCliProxyModelsError, refresh_cli_proxy_models};
use crate::auth_storage::{AuthStorage, MAX_AUTH_FILE_BYTES, read_text_file};
use crate::json_store::{JsonFileStore, StoreError, StoreLock, parse_object};
use crate::model_registry::config::{ModelConfig, validate_provider_config};
use crate::model_registry::{ModelRegistry, ModelRuntime, auth_path, models_path};
use crate::settings::{CLIPROXYAPI_MEMBER, SettingsStore};

/// The data name of the account lock, `.cliproxyapi.account.lock`.
const ACCOUNT_LOCK: &str = "cliproxyapi.account";

/// The member binding a route to its key.
const ACCOUNT: &str = crate::auth_storage::ACCOUNT_BINDING;

/// The largest `settings.json` read here, as [`SettingsStore`] reads it.
const MAX_SETTINGS_BYTES: u64 = 16 * 1024 * 1024;

/// The account as one read of the home finds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountView {
    /// No `cliproxyapi` member: the 0.3 import may supply a route.
    Absent,
    /// `null`: signed out.
    SignedOut,
    /// No route is used, for this reason, such as a sign-in that did not
    /// finish. Signing in again recovers.
    Disabled(String),
    /// A route and the key bound to it.
    Bound(BoundAccount),
}

/// A route and the key with the same generation. `Debug` hides the key.
#[derive(Clone, PartialEq, Eq)]
pub struct BoundAccount {
    /// The `cliproxyapi` member as stored, `account` included.
    pub route: Value,
    key: String,
}

impl std::fmt::Debug for BoundAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A route may hold header values, so only its shape is shown.
        f.debug_struct("BoundAccount")
            .field("route", &crate::settings::RouteShape(&self.route))
            .field("key", &"<redacted>")
            .finish()
    }
}

impl BoundAccount {
    /// The key requests to this route send.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The registry provider config of the route, in Pi's `models.json`
    /// shape, with no key of its own.
    pub fn provider_config(&self) -> Result<Value, String> {
        route_provider_config(&self.route)
    }
}

/// Reads the account from the home (see the module documentation).
/// Blocks.
pub fn read_account(home: &Path) -> AccountView {
    read_account_between(home, || {})
}

/// [`read_account`], running `between` after it reads the route and before
/// it reads the key, so tests can write in that gap.
fn read_account_between(home: &Path, between: impl FnOnce()) -> AccountView {
    let settings = match read_object(&home.join("settings.json"), MAX_SETTINGS_BYTES) {
        Ok(object) => object,
        Err(error) => {
            return AccountView::Disabled(format!("settings.json could not be read ({error})"));
        }
    };
    let route = match settings.get(CLIPROXYAPI_MEMBER) {
        None => return AccountView::Absent,
        Some(Value::Null) => return AccountView::SignedOut,
        Some(route @ Value::Object(_)) => route.clone(),
        Some(_) => {
            return AccountView::Disabled(
                "the cliproxyapi setting in settings.json is not a route or null".into(),
            );
        }
    };
    between();
    let Some(generation) = route.get(ACCOUNT).and_then(Value::as_str) else {
        return AccountView::Disabled("the saved route names no sign-in".into());
    };
    let auth = match read_object(&auth_path(home), MAX_AUTH_FILE_BYTES) {
        Ok(object) => object,
        Err(error) => {
            return AccountView::Disabled(format!("auth.json could not be read ({error})"));
        }
    };
    let record = auth.get(CLIPROXYAPI_ID);
    let bound_key = record
        .filter(|record| record.get("type").and_then(Value::as_str) == Some("api_key"))
        .filter(|record| record.get(ACCOUNT).and_then(Value::as_str) == Some(generation))
        .and_then(|record| record.get("key").and_then(Value::as_str));
    match bound_key {
        Some(key) => AccountView::Bound(BoundAccount {
            route,
            key: key.to_owned(),
        }),
        None => AccountView::Disabled(
            "the saved route and the key in auth.json are from different sign-ins".into(),
        ),
    }
}

/// A JSON object file; an empty object when it does not exist.
fn read_object(path: &Path, limit: u64) -> Result<Map<String, Value>, String> {
    match read_text_file(path, limit).map_err(|error| error.to_string())? {
        None => Ok(Map::new()),
        Some(text) => parse_object(&text),
    }
}

/// Why an account operation did not complete. No message holds a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountError {
    /// `models.json` configures `cliproxyapi` itself.
    ManualOverride,
    /// Nothing was written, for this reason.
    Unchanged(String),
    /// A write failed part-way: what failed, and the state it left.
    Partial {
        /// The failed write.
        error: String,
        /// What the account is now.
        outcome: String,
    },
}

impl std::fmt::Display for AccountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ManualOverride => f.write_str(
                "models.json configures the cliproxyapi provider; edit it there, or remove it to sign in here",
            ),
            Self::Unchanged(reason) => write!(f, "Nothing was saved: {reason}"),
            Self::Partial { error, outcome } => write!(f, "{error}; {outcome}"),
        }
    }
}

impl std::error::Error for AccountError {}

/// A finished sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginCommitted {
    /// How many models the route lists.
    pub models: usize,
    /// Writes that reached their file but may not survive a power loss.
    pub warnings: Vec<String>,
}

/// A finished sign-out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogoutCommitted {
    /// Whether the member changed to `null`.
    pub removed_route: bool,
    /// Whether `auth.json` held a key that was removed.
    pub removed_key: bool,
    /// Why the key could not be removed. The route is gone either way, so
    /// the key is not used.
    pub key_error: Option<String>,
    /// Writes that may not survive a power loss.
    pub warnings: Vec<String>,
}

/// What a refresh did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// There is no signed-in account to refresh: the member is absent (a
    /// route only the 0.3 import holds is never refreshed) or `null`.
    NotSignedIn,
    /// The account is off, for this reason ([`AccountView::Disabled`]).
    Disabled(String),
    /// The proxy lists what the route holds, or the route changed meanwhile.
    Unchanged,
    /// The route's models were replaced; how many it lists now.
    Updated(usize),
}

/// A refresh's outcome, with what it has to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refreshed {
    pub outcome: RefreshOutcome,
    /// Committed writes whose directory flush failed: the refreshed route
    /// may not survive a power loss.
    pub unsynced: Vec<String>,
    /// Why the running session's registry could not take the files' state,
    /// or why the account is off.
    pub warnings: Vec<String>,
}

/// Steps of a sign-in and a sign-out, at which tests inject failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Key,
    Route,
    Restore,
    SignOutRoute,
    SignOutKey,
}

/// What a test injects.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Inject {
    /// Stops the operation once the step is written, as a crash would.
    StopAfter(Step),
    /// The step's write is refused.
    Fail(Step),
    /// The route write and the key's restoration are refused.
    FailRouteAndRestore,
    /// The route write is refused; the key's restoration is written but
    /// not flushed.
    FailRouteThenUnsyncedRestore,
    /// Another writer stores a key with Pi's `set_api_key` just before
    /// the route write, which is then refused.
    DirectKeyThenFailRoute,
    /// Each write reaches its file but its directory flush fails.
    Unsynced,
}

/// The account of one Bake home (see the module documentation).
#[derive(Debug, Clone)]
pub struct CliProxyAccount {
    home: PathBuf,
    #[cfg(test)]
    inject: Option<Inject>,
}

/// A step a test stopped, which ends an operation like a crash.
#[cfg(test)]
const STOPPED: &str = "stopped by a test";

impl CliProxyAccount {
    /// The account of the Bake home at `home`.
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            #[cfg(test)]
            inject: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_inject(mut self, inject: Inject) -> Self {
        self.inject = Some(inject);
        self
    }

    /// `Err` when a test refuses this step's write.
    #[cfg(test)]
    fn fail(&self, step: Step) -> Result<(), StoreError> {
        let refused = match self.inject {
            Some(Inject::Fail(failed)) => failed == step,
            Some(Inject::FailRouteAndRestore) => matches!(step, Step::Route | Step::Restore),
            Some(Inject::DirectKeyThenFailRoute | Inject::FailRouteThenUnsyncedRestore) => {
                step == Step::Route
            }
            _ => false,
        };
        if refused {
            return Err(StoreError::Refused {
                path: self.home.clone(),
                reason: "injected failure".into(),
            });
        }
        Ok(())
    }

    #[cfg(not(test))]
    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn fail(&self, _: Step) -> Result<(), StoreError> {
        Ok(())
    }

    #[cfg(test)]
    fn stops_after(&self, step: Step) -> bool {
        self.inject == Some(Inject::StopAfter(step))
    }

    fn settings(&self) -> SettingsStore {
        let store = SettingsStore::new(&self.home);
        #[cfg(test)]
        if self.inject == Some(Inject::Unsynced) {
            return store.with_fault(crate::json_store::Fault::DirectorySync);
        }
        store
    }

    fn auth(&self) -> AuthStorage {
        let auth = AuthStorage::load(&auth_path(&self.home));
        #[cfg(test)]
        if self.inject == Some(Inject::Unsynced) {
            return auth.with_fault(crate::json_store::Fault::DirectorySync);
        }
        auth
    }

    fn lock(&self) -> Result<StoreLock, AccountError> {
        JsonFileStore::new(self.home.join(ACCOUNT_LOCK), 0)
            .lock()
            .map_err(|error| AccountError::Unchanged(error.to_string()))
    }

    /// The account as stored now. Blocks.
    pub fn read(&self) -> AccountView {
        read_account(&self.home)
    }

    /// Whether `models.json` configures `cliproxyapi` itself. Blocks.
    pub fn manual_override(&self) -> bool {
        ModelConfig::load(&models_path(&self.home))
            .provider(CLIPROXYAPI_ID)
            .is_some()
    }

    /// Saves a checked sign-in under a new generation: the key in
    /// `auth.json`, then the route in `settings.json`. Blocks. Once
    /// started, it finishes or reports the state it left, and is never
    /// cancelled part-way.
    pub fn commit_login(&self, checked: &CheckedLogin) -> Result<LoginCommitted, AccountError> {
        if self.manual_override() {
            return Err(AccountError::ManualOverride);
        }
        let route = CliProxyRoute::from_login(&checked.endpoints, &checked.models);
        // Refused before any write: a route that would not read back or
        // compose as a provider.
        let mut route_json = route.to_settings_json();
        CliProxyRoute::from_settings(&route_json)
            .map_err(|error| AccountError::Unchanged(error.to_string()))?;
        validate_provider_config(CLIPROXYAPI_ID, &route.to_pi_provider_config().config)
            .map_err(AccountError::Unchanged)?;
        let generation = new_generation().map_err(AccountError::Unchanged)?;
        if let Some(object) = route_json.as_object_mut() {
            object.insert(ACCOUNT.into(), generation.clone().into());
        }
        let _lock = self.lock()?;
        let mut warnings = Vec::new();

        // 1. The key, bound to the new generation.
        let record = json!({ "type": "api_key", "key": checked.key.expose(), ACCOUNT: generation });
        let mut auth = self.auth();
        let (previous, written) = match self.fail(Step::Key) {
            Ok(()) => auth.swap_record(CLIPROXYAPI_ID, Some(record.clone())),
            Err(error) => (None, Err(error)),
        };
        committed(written, &mut warnings).map_err(AccountError::Unchanged)?;
        #[cfg(test)]
        if self.stops_after(Step::Key) {
            return Err(AccountError::Unchanged(STOPPED.into()));
        }
        #[cfg(test)]
        if self.inject == Some(Inject::DirectKeyThenFailRoute) {
            AuthStorage::load(&auth_path(&self.home))
                .set_api_key(CLIPROXYAPI_ID, "sk-fake-direct-writer")
                .expect("the direct writer");
        }

        #[cfg(test)]
        if self.inject == Some(Inject::FailRouteThenUnsyncedRestore) {
            auth = auth.with_fault(crate::json_store::Fault::DirectorySync);
        }

        // 2. The route, with the same generation.
        let published = self.fail(Step::Route).and_then(|()| {
            self.settings()
                .update_cliproxyapi(|_| Some(Some(route_json.clone())))
        });
        match committed(published, &mut warnings) {
            Ok(_) => Ok(LoginCommitted {
                models: route.models.len(),
                warnings,
            }),
            Err(error) => {
                let (replaced, restored) = match self.fail(Step::Restore) {
                    Ok(()) => auth.replace_if(CLIPROXYAPI_ID, &record, previous),
                    Err(error) => (false, Err(error)),
                };
                let mut outcome = match committed(restored, &mut warnings) {
                    Ok(_) if replaced => "the previous sign-in is back".to_owned(),
                    Ok(_) => "auth.json changed meanwhile, so CLIProxyAPI stays off until you sign in again".to_owned(),
                    Err(_) => "CLIProxyAPI stays off until you sign in again".to_owned(),
                };
                for warning in &warnings {
                    outcome.push_str(&format!(" (may not be durable: {warning})"));
                }
                Err(AccountError::Partial {
                    error: format!("The route could not be saved: {error}"),
                    outcome,
                })
            }
        }
    }

    /// Signs out: the member becomes `null`, so neither the account's route
    /// nor the 0.3 import's is used again, then the key leaves `auth.json`.
    /// Blocks.
    pub fn logout(&self) -> Result<LogoutCommitted, AccountError> {
        let _lock = self.lock()?;
        let mut warnings = Vec::new();
        let mut changed = false;
        let route = self.fail(Step::SignOutRoute).and_then(|()| {
            self.settings().update_cliproxyapi(|current| {
                changed = current != Some(&Value::Null);
                changed.then_some(Some(Value::Null))
            })
        });
        committed(route, &mut warnings).map_err(AccountError::Unchanged)?;
        #[cfg(test)]
        if self.stops_after(Step::SignOutRoute) {
            return Err(AccountError::Unchanged(STOPPED.into()));
        }
        let mut auth = self.auth();
        let (removed_key, key_error) = match self
            .fail(Step::SignOutKey)
            .and_then(|()| auth.remove(CLIPROXYAPI_ID))
        {
            Ok(removed) => (removed, None),
            Err(error) if error.is_committed() => {
                warnings.push(error.to_string());
                (true, None)
            }
            Err(error) => (false, Some(error.to_string())),
        };
        Ok(LogoutCommitted {
            removed_route: changed,
            removed_key,
            key_error,
            warnings,
        })
    }

    /// Replaces the route's `models`, keeping its generation, only while
    /// the route is still `fetched_for`. Returns whether it wrote. Blocks.
    pub fn commit_refresh(
        &self,
        fetched_for: &Value,
        models: Value,
        warnings: &mut Vec<String>,
    ) -> Result<bool, AccountError> {
        let mut next = fetched_for.clone();
        if let Some(route) = next.as_object_mut() {
            route.insert("models".into(), models);
        }
        route_provider_config(&next).map_err(AccountError::Unchanged)?;
        let _lock = self.lock()?;
        let mut ours = false;
        let written = self.settings().update_cliproxyapi(|current| {
            ours = current == Some(fetched_for);
            ours.then(|| Some(next.clone()))
        });
        committed(written, warnings).map_err(AccountError::Unchanged)?;
        Ok(ours)
    }

    /// Brings `registry` to the account as stored now. A bound account
    /// becomes the `cliproxyapi` provider, below `models.json`, with its key
    /// bound. A signed-out or disabled one leaves no route or bound key. An
    /// absent member leaves the registry as it is, so a route from the 0.3
    /// import stays. For [`ModelRuntime::try_update`]. Returns a warning
    /// when the account is disabled, and `Err`, with the registry
    /// unchanged, when the route does not compose. Blocks.
    pub fn apply_to(&self, registry: &mut ModelRegistry) -> Result<Option<String>, String> {
        apply_view(registry, self.read())
    }
}

/// [`CliProxyAccount::apply_to`] for a view already read.
fn apply_view(registry: &mut ModelRegistry, view: AccountView) -> Result<Option<String>, String> {
    match view {
        AccountView::Bound(account) => {
            let config = account.provider_config()?;
            registry.add_provider_config(CLIPROXYAPI_ID, config, None)?;
            registry.bind_provider_key(CLIPROXYAPI_ID, Some(account.key));
            Ok(None)
        }
        AccountView::SignedOut => {
            registry.remove_provider_config(CLIPROXYAPI_ID);
            registry.bind_provider_key(CLIPROXYAPI_ID, None);
            Ok(None)
        }
        AccountView::Disabled(reason) => {
            registry.remove_provider_config(CLIPROXYAPI_ID);
            registry.bind_provider_key(CLIPROXYAPI_ID, None);
            Ok(Some(disabled_warning(&reason)))
        }
        AccountView::Absent => Ok(None),
    }
}

/// The warning for a disabled account.
pub fn disabled_warning(reason: &str) -> String {
    format!("CLIProxyAPI is off: {reason}. Run /login to sign in again.")
}

/// A write's result: `Err` when nothing was written; a committed write
/// whose directory flush failed counts as written, with a warning.
fn committed<T>(
    result: Result<T, StoreError>,
    warnings: &mut Vec<String>,
) -> Result<Option<T>, String> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.is_committed() => {
            warnings.push(error.to_string());
            Ok(None)
        }
        Err(error) => Err(error.to_string()),
    }
}

/// A new random generation. Without randomness there is no sign-in, since
/// a guessable generation could match another sign-in's.
fn new_generation() -> Result<String, String> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random)
        .map_err(|error| format!("no randomness for the sign-in ({error})"))?;
    Ok(random.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// The provider config a saved route gives the registry, in Pi's
/// `models.json` shape, with no key of its own.
pub fn route_provider_config(route: &Value) -> Result<Value, String> {
    let mut route = route.clone();
    if let Some(object) = route.as_object_mut() {
        object.remove(ACCOUNT);
    }
    let route = CliProxyRoute::from_settings(&route)
        .map_err(|error| format!("The saved CLIProxyAPI route is invalid: {error}"))?;
    let mut config = route.to_pi_provider_config().config;
    if let Some(object) = config.as_object_mut() {
        object.remove("apiKey");
    }
    Ok(config)
}

/// Refreshes the signed-in account's route against the proxy, Bake's
/// `refreshCliProxyModels`. Models the proxy lists replace the saved ones.
/// Saved capabilities the listing leaves out are kept, and so are models
/// in `keep` that it no longer lists. The route and the key come from one
/// [`read_account`], never from an older runtime snapshot. When `signal`
/// aborts the fetch, nothing is written. Once the fetch returns, the write
/// and the runtime update run as one owned blocking task. Every outcome but
/// an error brings the runtime to the account as the files hold it then,
/// so a sign-in, sign-out, or disabled account another process left is
/// in effect from the next request, whether or not the catalog changed.
/// Requests already made keep their snapshot.
pub async fn refresh_account(
    account: CliProxyAccount,
    runtime: ModelRuntime,
    keep: Vec<String>,
    signal: Option<AbortSignal>,
) -> Result<Refreshed, String> {
    let reader = account.clone();
    let view = tokio::task::spawn_blocking(move || reader.read())
        .await
        .map_err(|error| error.to_string())?;
    let bound = match view {
        AccountView::Bound(bound) => bound,
        AccountView::Absent | AccountView::SignedOut => {
            return sync(account, runtime, None, RefreshOutcome::NotSignedIn).await;
        }
        AccountView::Disabled(reason) => {
            return sync(account, runtime, None, RefreshOutcome::Disabled(reason)).await;
        }
    };
    let mut fetched_route = bound.route.clone();
    if let Some(object) = fetched_route.as_object_mut() {
        object.remove(ACCOUNT);
    }
    // The runtime key is the person's explicit choice for this process and
    // goes to whichever route is current, as requests send it; no other key
    // is taken from the runtime, which may be older than the files.
    let key = runtime
        .snapshot()
        .runtime_api_key(CLIPROXYAPI_ID)
        .unwrap_or_else(|| bound.key().to_owned());
    let mut fetched = None;
    let changed = refresh_cli_proxy_models(
        Some(&fetched_route),
        Some(&key),
        true,
        &keep,
        signal.as_ref(),
        |ops| {
            fetched = ops.into_iter().next().map(|(_, models)| models);
            Ok(())
        },
    )
    .await
    .map_err(|error| match error {
        RefreshCliProxyModelsError::Fetch(error) => error.to_string(),
        RefreshCliProxyModelsError::SettingsWrite(error) => error,
    })?;
    let write = fetched
        .filter(|_| changed)
        .map(|models| (bound.route, models));
    sync(account, runtime, write, RefreshOutcome::Unchanged).await
}

/// The owned end of a refresh: writes `write` (the route fetched for and
/// its new models) when given, then brings the runtime to the files.
/// Returns `Updated` when the write landed, else `otherwise`.
async fn sync(
    account: CliProxyAccount,
    runtime: ModelRuntime,
    write: Option<(Value, Value)>,
    otherwise: RefreshOutcome,
) -> Result<Refreshed, String> {
    tokio::task::spawn_blocking(move || {
        let mut unsynced = Vec::new();
        let mut warnings = Vec::new();
        let mut outcome = otherwise;
        if let Some((fetched_for, models)) = write {
            let count = models.as_array().map_or(0, Vec::len);
            if account
                .commit_refresh(&fetched_for, models, &mut unsynced)
                .map_err(|error| error.to_string())?
            {
                outcome = RefreshOutcome::Updated(count);
            }
        }
        if let Some(warning) = runtime.try_update(|registry| account.apply_to(registry))? {
            warnings.push(warning);
        }
        Ok(Refreshed {
            outcome,
            unsynced,
            warnings,
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

#[cfg(test)]
pub(crate) fn read_account_with_gap(home: &Path, between: impl FnOnce()) -> AccountView {
    read_account_between(home, between)
}

#[cfg(test)]
mod tests;
