//! The CLIProxyAPI provider route, Bake's own contract, not Pi's. Its
//! output is Pi's: [`route`] turns a route into one provider of Pi's
//! `models.json` (Pi v1.1.0, `packages/coding-agent/src/core/model-config.ts`),
//! which a Pi model registry accepts.
//!
//! | Module | Contents |
//! |---|---|
//! | [`endpoints`] | the proxy's base, models, and responses URLs |
//! | [`catalog`] | the proxy's model list and its models |
//! | [`fetch`](mod@fetch) | fetching and checking the model list |
//! | [`route`] | the saved route, its defaults, and its upgrade |
//! | [`account`] | the Bake home's account in `settings.json` and `auth.json`: sign-in, refresh, and sign-out |
//! | [`setup`] | configuring the route, refreshing its models, and the upgrade notice |
//! | [`key`] | the API key a sign-in checks |
//!
//! Not ported here: the per-vendor effort and limit backfill from an
//! installed catalog.

pub mod account;
pub mod catalog;
pub mod endpoints;
pub mod fetch;
pub mod key;
pub mod route;
pub mod setup;

pub use catalog::{
    CliProxyApi, CliProxyModel, InvalidModelList, Modality, cli_proxy_api, cli_proxy_models,
};
pub use endpoints::{CliProxyEndpoints, CliProxyUrlError, cli_proxy_endpoints};
pub use fetch::{
    CATALOG_TIMEOUT, CheckField, CliProxyCheckError, CliProxyCheckFailure,
    FetchCliProxyModelsError, MAX_CATALOG_BYTES, fetch_cli_proxy_models,
};
pub use key::{ApiKey, KeyRejection, normalize_api_key};
pub use route::{
    CLIPROXYAPI_ROUTE_DEFAULTS, CliProxyRoute, CliProxyRouteChange, CliProxyRouteDefaults,
    CliProxyRouteUpgradePlan, PiProviderConfig, ReasoningEfforts, RouteError, RouteModel,
    SkippedModel, plan_cli_proxy_route_upgrade,
};
pub use setup::{
    CheckedLogin, CliProxyRouteUpgrade, ConfigureCliProxyError, LoginPrompt, LoginPromptKind,
    LoginPromptStep, RefreshCliProxyModelsError, check_cli_proxy_login, cli_proxy_models_in_use,
    cli_proxy_upgrade_notice, configure_cli_proxy_api, refresh_cli_proxy_models,
    upgrade_cli_proxy_route, with_saved_capabilities,
};

/// The route's provider id.
pub const CLIPROXYAPI_ID: &str = "cliproxyapi";
/// The credential reference the route's key is stored under.
pub const CLIPROXYAPI_KEY: &str = "CLIPROXYAPI_API_KEY";
/// The address a login offers first.
pub const CLIPROXYAPI_DEFAULT_URL: &str = "http://127.0.0.1:8317";
