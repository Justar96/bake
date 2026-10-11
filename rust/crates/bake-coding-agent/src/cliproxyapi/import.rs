//! The D25 import: the CLIProxyAPI route, its key, and the default model,
//! read once from a 0.3 Bake home and never written.
//!
//! Sources, all Bake TypeScript:
//!
//! - `settings.yaml` as `packages/settings/settings-file/src/index.ts`
//!   (`resolveSpec`, `parse`) reads it: a YAML mapping of namespace
//!   sections. The route is `llm-pi-ai.providers.cliproxyapi`
//!   (`packages/llm/llm-pi-ai/src/config.ts`), upgraded in memory as
//!   startup's `upgradeCliProxyRoute` upgraded it in place
//!   (`planCliProxyRouteUpgrade`). The default model is `agent-default-model`
//!   (`packages/core/agent-default-model/src/index.ts`, `selection`).
//! - `.credentials.yaml` as `packages/credentials/credentials-local/src/index.ts`
//!   reads it (`assertOwnerOnly`, `parseCredentialsDocument`, `parseRefs`,
//!   `renderFlatLayoutMigration`), and the key resolves as
//!   `LocalCredentialProvider.resolve` orders it: a non-empty inherited
//!   environment variable first, then the file's `refs`. The key is then
//!   judged as `normalizeApiKey` judges it (`packages/llm/llm/src/api-key.ts`):
//!   trimmed, non-empty, printable ASCII.
//!
//! Both files are untrusted: each is capped at [`MAX_FILE_BYTES`] and read
//! by the module's bounded YAML reader (its errors are
//! [`super::YamlError`]). No error, `Debug` output, or log holds the key.

use std::ffi::OsString;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

use bake_ai::ModelThinkingLevel;
use serde_json::{Map, Value};

use super::CLIPROXYAPI_ID;
use super::route::{
    CliProxyRoute, CliProxyRouteChange, PiProviderConfig, RouteError, is_credential_ref,
    plan_cli_proxy_route_upgrade, thinking_level,
};
use super::yaml::{YamlError, parse_document};

/// The largest settings or credentials file read.
pub const MAX_FILE_BYTES: usize = 1024 * 1024;
/// `settings.yaml`, the settings document's name in the home.
pub const SETTINGS_FILENAME: &str = "settings.yaml";
/// `.credentials.yaml` (`CREDENTIALS_FILENAME`).
pub const CREDENTIALS_FILENAME: &str = ".credentials.yaml";
/// The credentials layout this import reads (`DOCUMENT_VERSION`).
pub const CREDENTIALS_VERSION: u64 = 1;

/// An API key. Its `Debug` and `Display` never show it.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// The key, for the request that sends it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

impl fmt::Display for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Where the key came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// The process environment.
    Environment,
    /// `.credentials.yaml`.
    CredentialsFile,
}

/// The resolved key and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedKey {
    /// The trimmed key.
    pub key: ApiKey,
    /// Its source.
    pub source: KeySource,
}

/// The imported route.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedRoute {
    /// The route as saved, after the in-memory upgrade.
    pub route: CliProxyRoute,
    /// What the in-memory upgrade filled in.
    pub upgraded: Vec<CliProxyRouteChange>,
    /// The route as a Pi `models.json` provider config.
    pub provider: PiProviderConfig,
    /// The key, when the environment or the credentials file has one.
    pub key: Option<ImportedKey>,
}

/// The default model for new sessions (`agent-default-model`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultModel {
    /// Provider route, such as `cliproxyapi`.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// The saved `reasoningEffort`, as written.
    pub reasoning_effort: Option<String>,
    /// That effort as a Pi thinking level, when it names one.
    pub thinking_level: Option<ModelThinkingLevel>,
}

/// What a 0.3 home holds for the CLIProxyAPI route.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CliProxyImport {
    /// The route, when `settings.yaml` has one.
    pub route: Option<ImportedRoute>,
    /// The default model, when `settings.yaml` saves one.
    pub default_model: Option<DefaultModel>,
}

/// Why a key cannot be used (`ApiKeyRejection`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRejection {
    /// Blank after trimming.
    Empty,
    /// Holds a character outside printable ASCII.
    IllegalCharacters,
}

/// Why the import failed. Paths and key names may appear; values never do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// No home: no `BAKE_HOME`, `DSH_HOME`, or user home directory.
    NoHome,
    /// A file could not be read.
    Read {
        /// The file.
        file: PathBuf,
        /// The I/O error's kind.
        kind: std::io::ErrorKind,
    },
    /// A file exceeds [`MAX_FILE_BYTES`].
    TooLarge {
        /// The file.
        file: PathBuf,
    },
    /// A file is not acceptable YAML.
    Yaml {
        /// The file.
        file: PathBuf,
        /// What is wrong, with its position.
        error: YamlError,
    },
    /// A document has the wrong shape.
    Invalid {
        /// The file.
        file: PathBuf,
        /// The field's dotted path.
        path: String,
        /// What it must be.
        reason: String,
    },
    /// The credentials file is readable beyond its owner (POSIX).
    CredentialsMode {
        /// The file.
        file: PathBuf,
        /// Its permission bits.
        mode: u32,
    },
    /// The resolved key cannot be sent.
    UnusableKey {
        /// The credential reference it resolved through.
        name: String,
        /// Why.
        reason: KeyRejection,
    },
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHome => f.write_str("the Bake home is unknown: set BAKE_HOME"),
            Self::Read { file, kind } => write!(f, "could not read {}: {kind}", file.display()),
            Self::TooLarge { file } => write!(
                f,
                "{} is larger than {MAX_FILE_BYTES} bytes",
                file.display()
            ),
            Self::Yaml { file, error } => write!(f, "{} {error}", file.display()),
            Self::Invalid { file, path, reason } => {
                write!(f, "{}: {path} {reason}", file.display())
            }
            Self::CredentialsMode { file, mode } => write!(
                f,
                "{} is readable beyond its owner (mode {mode:o}); run \"chmod 600 {}\"",
                file.display(),
                file.display()
            ),
            Self::UnusableKey { name, reason } => match reason {
                KeyRejection::Empty => write!(f, "the API key resolved from {name} is blank"),
                KeyRejection::IllegalCharacters => write!(
                    f,
                    "the API key resolved from {name} contains characters no HTTP header can carry"
                ),
            },
        }
    }
}

impl std::error::Error for ImportError {}

/// `normalizeApiKey`: trim, then require printable ASCII without spaces.
pub fn normalize_api_key(raw: &str) -> Result<ApiKey, KeyRejection> {
    let value = super::endpoints::js_trim(raw);
    if value.is_empty() {
        return Err(KeyRejection::Empty);
    }
    if !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Err(KeyRejection::IllegalCharacters);
    }
    Ok(ApiKey(value.to_owned()))
}

/// Read at most [`MAX_FILE_BYTES`]; `None` when the file does not exist.
fn read_capped(file: &Path) -> Result<Option<Vec<u8>>, ImportError> {
    let handle = match std::fs::File::open(file) {
        Ok(handle) => handle,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ImportError::Read {
                file: file.to_owned(),
                kind: error.kind(),
            });
        }
    };
    let mut bytes = Vec::new();
    handle
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ImportError::Read {
            file: file.to_owned(),
            kind: error.kind(),
        })?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(ImportError::TooLarge {
            file: file.to_owned(),
        });
    }
    Ok(Some(bytes))
}

fn read_yaml(file: &Path) -> Result<Option<Value>, ImportError> {
    let Some(bytes) = read_capped(file)? else {
        return Ok(None);
    };
    parse_document(&bytes, MAX_FILE_BYTES)
        .map(Some)
        .map_err(|error| ImportError::Yaml {
            file: file.to_owned(),
            error,
        })
}

fn invalid(file: &Path, path: impl Into<String>, reason: impl Into<String>) -> ImportError {
    ImportError::Invalid {
        file: file.to_owned(),
        path: path.into(),
        reason: reason.into(),
    }
}

/// A document's root as a mapping; `null` (an empty document) is empty.
fn root_mapping(file: &Path, value: Value) -> Result<Map<String, Value>, ImportError> {
    match value {
        Value::Null => Ok(Map::new()),
        Value::Object(map) => Ok(map),
        _ => Err(invalid(file, "(root)", "must be a mapping")),
    }
}

/// `selection`: provider and model both non-empty, or nothing.
fn default_model(
    file: &Path,
    settings: &Map<String, Value>,
) -> Result<Option<DefaultModel>, ImportError> {
    let section = match settings.get("agent-default-model") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(section)) => section,
        Some(_) => return Err(invalid(file, "agent-default-model", "must be a mapping")),
    };
    let field = |key: &str| match section.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(invalid(
            file,
            format!("agent-default-model.{key}"),
            "must be a string",
        )),
    };
    let (Some(provider), Some(model)) = (field("provider")?, field("model")?) else {
        return Ok(None);
    };
    if provider.is_empty() || model.is_empty() {
        return Ok(None);
    }
    let reasoning_effort = field("reasoningEffort")?;
    Ok(Some(DefaultModel {
        thinking_level: reasoning_effort.as_deref().and_then(thinking_level),
        provider,
        model,
        reasoning_effort,
    }))
}

/// The saved route value, `llm-pi-ai.providers.cliproxyapi`.
fn saved_route<'a>(
    file: &Path,
    settings: &'a Map<String, Value>,
) -> Result<Option<&'a Value>, ImportError> {
    let section = match settings.get("llm-pi-ai") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(section)) => section,
        Some(_) => return Err(invalid(file, "llm-pi-ai", "must be a mapping")),
    };
    let providers = match section.get("providers") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(providers)) => providers,
        Some(_) => return Err(invalid(file, "llm-pi-ai.providers", "must be a mapping")),
    };
    Ok(providers
        .get(CLIPROXYAPI_ID)
        .filter(|route| !route.is_null()))
}

/// `parseCredentialsDocument` and `parseRefs`, plus the pre-release flat
/// layout `renderFlatLayoutMigration` recognizes, read as its migration
/// would leave it but without rewriting the file. Only `refs` is needed;
/// `records` must be a mapping and is otherwise not read.
fn credential_refs(file: &Path, value: Value) -> Result<Map<String, Value>, ImportError> {
    let root = root_mapping(file, value)?;
    if root.is_empty() {
        return Ok(Map::new());
    }
    if !root.contains_key("version") {
        let flat = root.iter().all(|(key, value)| {
            is_credential_ref(key) && value.as_str().is_some_and(|value| !value.is_empty())
        });
        if flat {
            return Ok(root);
        }
        return Err(invalid(
            file,
            "version",
            format!(
                "is missing; this build reads version {CREDENTIALS_VERSION} with entries under refs"
            ),
        ));
    }
    // `toJS` reads `1.0` as the number 1 too.
    if root.get("version").and_then(Value::as_f64) != Some(CREDENTIALS_VERSION as f64) {
        return Err(invalid(
            file,
            "version",
            format!("must be {CREDENTIALS_VERSION}"),
        ));
    }
    for key in root.keys() {
        if !matches!(key.as_str(), "version" | "refs" | "records") {
            return Err(invalid(file, key.as_str(), "is not a known top-level key"));
        }
    }
    if !matches!(
        root.get("records"),
        None | Some(Value::Null | Value::Object(_))
    ) {
        return Err(invalid(file, "records", "must be a mapping"));
    }
    let refs = match root.get("refs") {
        None | Some(Value::Null) => return Ok(Map::new()),
        Some(Value::Object(refs)) => refs,
        Some(_) => return Err(invalid(file, "refs", "must be a mapping")),
    };
    for (key, value) in refs {
        if !is_credential_ref(key) {
            return Err(invalid(
                file,
                "refs",
                "has a key that is not an environment variable name",
            ));
        }
        match value {
            Value::String(value) if !value.is_empty() => {}
            Value::String(_) => {
                return Err(invalid(
                    file,
                    format!("refs.{key}"),
                    "is empty; remove the key instead",
                ));
            }
            _ => return Err(invalid(file, format!("refs.{key}"), "must be a string")),
        }
    }
    Ok(refs.clone())
}

/// `assertOwnerOnly`: refuse a credentials file other users can read.
/// POSIX only; Windows has no mode to inspect.
fn assert_owner_only(file: &Path) -> Result<(), ImportError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(file) {
            Ok(metadata) => {
                let mode = metadata.permissions().mode() & 0o777;
                if mode & 0o077 != 0 {
                    return Err(ImportError::CredentialsMode {
                        file: file.to_owned(),
                        mode,
                    });
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(ImportError::Read {
                    file: file.to_owned(),
                    kind: error.kind(),
                });
            }
        }
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

/// `LocalCredentialProvider.resolve` for one reference: a non-empty
/// inherited environment variable, then the file. The `.env` fallbacks
/// below the file are not read.
fn resolve_key(
    home: &Path,
    name: &str,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Option<ImportedKey>, ImportError> {
    let judged = |raw: &str, source| {
        normalize_api_key(raw)
            .map(|key| Some(ImportedKey { key, source }))
            .map_err(|reason| ImportError::UnusableKey {
                name: name.to_owned(),
                reason,
            })
    };
    if let Some(value) = env(name).filter(|value| !value.is_empty()) {
        // A value that is not Unicode cannot be a printable-ASCII key.
        return match value.into_string() {
            Ok(value) => judged(&value, KeySource::Environment),
            Err(_) => Err(ImportError::UnusableKey {
                name: name.to_owned(),
                reason: KeyRejection::IllegalCharacters,
            }),
        };
    }
    let file = home.join(CREDENTIALS_FILENAME);
    assert_owner_only(&file)?;
    let Some(document) = read_yaml(&file)? else {
        return Ok(None);
    };
    let refs = credential_refs(&file, document)?;
    match refs.get(name).and_then(Value::as_str) {
        Some(value) => judged(value, KeySource::CredentialsFile),
        None => Ok(None),
    }
}

/// Import from the home at `home`, reading environment variables through
/// `env`. Neither file is written. A missing file imports nothing from it.
pub fn import_cliproxyapi(
    home: &Path,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<CliProxyImport, ImportError> {
    let file = home.join(SETTINGS_FILENAME);
    let Some(document) = read_yaml(&file)? else {
        return Ok(CliProxyImport::default());
    };
    let settings = root_mapping(&file, document)?;
    let default_model = default_model(&file, &settings)?;
    let Some(saved) = saved_route(&file, &settings)? else {
        return Ok(CliProxyImport {
            route: None,
            default_model,
        });
    };
    let mut saved = saved.clone();
    let upgraded = match plan_cli_proxy_route_upgrade(&saved) {
        Some(plan) => {
            plan.apply_to_route(&mut saved);
            plan.changes
        }
        None => Vec::new(),
    };
    let route = CliProxyRoute::from_settings(&saved).map_err(|RouteError { path, reason }| {
        invalid(
            &file,
            format!("llm-pi-ai.providers.{CLIPROXYAPI_ID}.{path}"),
            reason,
        )
    })?;
    let key = resolve_key(home, route.key_ref(), env)?;
    let provider = route.to_pi_provider_config();
    Ok(CliProxyImport {
        route: Some(ImportedRoute {
            route,
            upgraded,
            provider,
            key,
        }),
        default_model,
    })
}

/// [`import_cliproxyapi`] from the Bake home ([`crate::home::bake_home`])
/// and the process environment.
pub fn import_from_bake_home() -> Result<CliProxyImport, ImportError> {
    let home = crate::home::bake_home().ok_or(ImportError::NoHome)?;
    import_cliproxyapi(&home, &|name| std::env::var_os(name))
}

#[cfg(test)]
mod tests;
