//! The CLIProxyAPI provider route, retained by D22, D25, and D32.
//!
//! Bake's own contract, not Pi's: ported from the 0.3 TypeScript
//! `apps/tui/packages/app/src/cliproxyapi.ts` and proved by fixture tests
//! derived from `apps/tui/packages/app/tests/cliproxyapi.test.ts`, each
//! naming the case it follows. Its output is Pi's: [`route`] turns a route
//! into one provider of Pi's `models.json` (Pi v1.1.0,
//! `packages/coding-agent/src/core/model-config.ts`), which a Pi model
//! registry accepts.
//!
//! | Module | Source |
//! |---|---|
//! | [`endpoints`] | `cliProxyEndpoints` |
//! | [`catalog`] | `cliProxyApi`, `cliProxyModels`, `CliProxyModel` |
//! | [`fetch`](mod@fetch) | `fetchCliProxyModels`, `CliProxyCheckFailure`, `CliProxyCheckError`, `cliProxyFailureText` |
//! | [`route`] | `CLIPROXYAPI_ROUTE_DEFAULTS`, the route `configureCliProxyApi` saves, `planCliProxyRouteUpgrade`; `llm-pi-ai`'s `resolveModelReasoning` and route defaults (`packages/llm/llm-pi-ai/src/{catalog,config}.ts`) |
//! | [`import`](mod@import) | the D25 read of `settings.yaml` and `.credentials.yaml`: `packages/settings/settings-file`, `packages/credentials/credentials-local`, `packages/core/agent-default-model`, and `normalizeApiKey` in `packages/llm/llm` |
//! | `yaml` | a bounded YAML reader for both files |
//!
//! Not ported here: the interactive login (`configureCliProxyApi`'s
//! prompts), writing the route or key, `refreshCliProxyModels`, logout, the
//! startup notice, and the per-vendor effort and limit backfill that
//! `llm-pi-ai` applied from its installed catalog.

pub mod catalog;
pub mod endpoints;
pub mod fetch;
pub mod import;
pub mod route;
mod yaml;

pub use catalog::{
    CliProxyApi, CliProxyModel, InvalidModelList, Modality, cli_proxy_api, cli_proxy_models,
};
pub use endpoints::{CliProxyEndpoints, CliProxyUrlError, cli_proxy_endpoints};
pub use fetch::{
    CATALOG_TIMEOUT, CheckField, CliProxyCheckError, CliProxyCheckFailure,
    FetchCliProxyModelsError, MAX_CATALOG_BYTES, fetch_cli_proxy_models,
};
pub use import::{
    ApiKey, CliProxyImport, DefaultModel, ImportError, ImportedKey, ImportedRoute, KeyRejection,
    KeySource, import_cliproxyapi, import_from_bake_home, normalize_api_key,
};
pub use route::{
    CLIPROXYAPI_ROUTE_DEFAULTS, CliProxyRoute, CliProxyRouteChange, CliProxyRouteDefaults,
    CliProxyRouteUpgradePlan, PiProviderConfig, ReasoningEfforts, RouteError, RouteModel,
    SkippedModel, plan_cli_proxy_route_upgrade,
};
pub use yaml::{YamlError, YamlErrorKind};

/// The route's provider id (`CLIPROXYAPI_ID`).
pub const CLIPROXYAPI_ID: &str = "cliproxyapi";
/// The credential reference the route's key is stored under
/// (`CLIPROXYAPI_KEY`).
pub const CLIPROXYAPI_KEY: &str = "CLIPROXYAPI_API_KEY";
/// The address a login offers first (`CLIPROXYAPI_DEFAULT_URL`).
pub const CLIPROXYAPI_DEFAULT_URL: &str = "http://127.0.0.1:8317";
