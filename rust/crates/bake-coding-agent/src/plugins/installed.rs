//! The Wasm plugins a Bake home installs and enables, and what a session
//! loads from them: the owner-controlled configuration of the native
//! plugin design (`docs/roadmap/rust-0.4/native-plugins.md`, "Inspection
//! and management"). Bake's own; Pi loads extensions from directories it
//! discovers, which Bake does not.
//!
//! Under `$BAKE_HOME/plugins/` ([`PluginHome`]):
//!
//! - `store/<sha256>/` holds one installed package, `plugin.json` and
//!   `plugin.wasm`, named by its [`package_hash`]. [`PluginHome::install`]
//!   takes a package already read into memory (from a source directory read
//!   once, without following links, by [`WasmPackage::read_dir`]), writes
//!   it to a private temporary directory beside the store, makes its files
//!   read-only, and renames that directory into place. Bake never writes a
//!   store copy again, and nothing runs at install: no script, and no code
//!   of the package. Read-only bits keep accidental writes out but do not
//!   stop the owner; what makes a changed copy harmless is that every read
//!   rehashes it, and a mismatch makes the plugin unloadable.
//! - `plugins.json` enables plugins: one record per plugin id with the
//!   pinned hash, its API version, version, and the tools it vetoes, copied
//!   from the manifest at [`PluginHome::enable`], and a `failure` once the
//!   plugin has failed. It is written only through the locked, atomic
//!   updates of [`crate::json_store`], and read strictly: an unknown member,
//!   a malformed record, or a file that is not a regular JSON object is an
//!   error, and a session refuses to start rather than guess which tools a
//!   plugin protects.
//!
//! A session ([`PluginHome::load`]) registers each enabled plugin in id
//! order. A plugin with a recorded failure, or whose store copy is missing,
//! changed, or no longer matches its record, blocks the tools its record
//! lists from the first call and is never asked; the record's tool list is
//! what protects them, so it does not depend on the package being readable.
//! A plugin that fails in a session ([`FailureKind::persists`]) is recorded
//! before its denial is written, so every later session, a resumed one
//! included, blocks its tools too, until [`PluginHome::disable`] removes the
//! record. Disabling and enabling again is the only reset. A session reads
//! the file once, when it starts: a failure recorded by another running
//! session, or an `enable` or `disable`, reaches it at its next start.
//!
//! Only the owner's files are trusted ([`trust`]). On Unix `plugins.json`
//! and each store copy are read through descriptors opened one directory
//! at a time from `/` without following links, and each opened object
//! must be owned by the user or root and not writable by others: by mode
//! bits, and on macOS by no ACL entry that allows writing. While any
//! plugin is enabled the plugins directory must lie outside the session's
//! workspace and outside any git working tree. An untrusted `plugins.json`
//! or placement stops the session's setup; an untrusted store copy makes
//! that plugin unloadable, so its tools stay blocked. Owners and ACLs are
//! checked on Linux and macOS only: elsewhere, Windows included, `enable`
//! is refused and a `plugins.json` that enables a plugin stops setup. A
//! home with no `plugins.json`, or one that enables nothing, works on
//! every platform; deleting the file, which needs write access to the
//! plugins directory, disables every plugin.
//!
//! [`FailureKind::persists`]: super::veto::FailureKind::persists

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::veto::{Failure, FailureKind};
#[cfg(not(unix))]
use super::wasm::manifest::{ReadError, read_regular};
use super::wasm::session::{WasmSetup, WasmVeto};
use super::wasm::{SUPPORTED_API_VERSIONS, WasmPackage};
use super::{Contribution, MAX_REASON_BYTES, PluginId, PluginManifest, PluginRegistry, Registered};
use crate::json_store::JsonFileStore;

/// The directory under the Bake home.
pub const PLUGINS_DIR: &str = "plugins";

/// The configuration file in [`PLUGINS_DIR`].
pub const CONFIG_FILE: &str = "plugins.json";

/// The largest `plugins.json`, in bytes.
pub const MAX_CONFIG_BYTES: u64 = 1 << 20;

/// The most plugins one home enables.
pub const MAX_ENABLED: usize = 16;

/// The store's directory within [`PLUGINS_DIR`].
const STORE_DIR: &str = "store";

/// Why no plugin can be enabled where [`trust::SUPPORTED`] is false.
const UNSUPPORTED: &str = "bake-rs cannot yet check who can change plugin files on this platform (it checks owners and permissions on Linux and macOS only), so it enables no plugin here";

/// Why a plugin-home operation did nothing. The message names the file or
/// the plugin; it may quote a plugin's text, so escape it before showing it
/// on a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginHomeError(String);

impl fmt::Display for PluginHomeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PluginHomeError {}

fn error(message: impl Into<String>) -> PluginHomeError {
    PluginHomeError(message.into())
}

/// A package's content hash: SHA-256 over the length of `plugin.json` as
/// eight little-endian bytes, `plugin.json`, then `plugin.wasm`, as 64
/// lowercase hex digits. The length prefix makes the split between the two
/// files part of what is hashed.
pub fn package_hash(package: &WasmPackage) -> String {
    let manifest = package.manifest_bytes();
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    context.update(&(manifest.len() as u64).to_le_bytes());
    context.update(manifest);
    context.update(package.component());
    context
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `text` as a package hash, with or without a `sha256:` prefix.
fn parse_hash(text: &str) -> Result<String, PluginHomeError> {
    let hash = text.strip_prefix("sha256:").unwrap_or(text);
    if hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(hash.to_owned())
    } else {
        Err(error(format!(
            "{:?} is not a package hash: give the 64 lowercase hex digits `bake-rs plugin install` printed",
            super::truncate(text, 80)
        )))
    }
}

/// An installed package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// The plugin id.
    pub id: String,
    /// The plugin version.
    pub version: String,
    /// The package hash, which names its store copy.
    pub sha256: String,
    /// The store already held this exact package.
    pub already: bool,
}

/// What [`PluginHome::enable`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enabled {
    /// The plugin id.
    pub id: String,
    /// Its record in `plugins.json`.
    pub record: EnabledPlugin,
    /// Whether this call enabled it; `false` when it already was.
    pub changed: bool,
}

/// A failure recorded for an enabled plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RecordedFailure {
    /// The failure kind, as the decision entries name it (`trap`, `error`,
    /// `timeout`, `start`, and so on).
    pub kind: String,
    /// The failure's message, at most 16 KiB.
    pub message: String,
    /// When it was recorded, in milliseconds since the Unix epoch.
    pub at_ms: i64,
}

/// One enabled plugin, as `plugins.json` records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EnabledPlugin {
    /// The pinned package hash.
    pub sha256: String,
    /// The `bake:plugin` API version.
    pub api: String,
    /// The plugin version.
    pub version: String,
    /// The tools it vetoes, sorted.
    pub veto_tools: Vec<String>,
    /// Why it is failed, once it has failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<RecordedFailure>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    #[serde(default)]
    plugins: BTreeMap<String, EnabledPlugin>,
}

/// A record's manifest and failure, validated.
fn check_record(
    id: &str,
    record: &EnabledPlugin,
) -> Result<(PluginManifest, Option<Failure>), String> {
    let manifest = PluginManifest::new(id, &record.version, record.veto_tools.iter().cloned())
        .map_err(|error| error.to_string())?;
    if !manifest
        .veto_tools()
        .eq(record.veto_tools.iter().map(String::as_str))
    {
        return Err("vetoTools is not sorted and distinct".to_owned());
    }
    parse_hash(&record.sha256).map_err(|error| error.to_string())?;
    if record.sha256.starts_with("sha256:") {
        return Err("sha256 has a prefix".to_owned());
    }
    if !SUPPORTED_API_VERSIONS.contains(&record.api.as_str()) {
        return Err(format!(
            "API version {:?} is not supported",
            super::truncate(&record.api, 64)
        ));
    }
    let failure = match &record.failure {
        None => None,
        Some(recorded) => {
            let kind = FailureKind::parse(&recorded.kind)
                .filter(|kind| kind.persists())
                .ok_or_else(|| {
                    format!(
                        "failure kind {:?} is not one that is recorded",
                        super::truncate(&recorded.kind, 64)
                    )
                })?;
            if recorded.message.len() > MAX_REASON_BYTES {
                return Err("the failure message is too long".to_owned());
            }
            Some(Failure::new(kind, &recorded.message))
        }
    };
    Ok((manifest, failure))
}

/// Parses and validates the configuration object.
fn parse_config(object: Map<String, Value>) -> Result<ConfigFile, String> {
    let config: ConfigFile =
        serde_json::from_value(Value::Object(object)).map_err(|error| error.to_string())?;
    if config.plugins.len() > MAX_ENABLED {
        return Err(format!("it enables more than {MAX_ENABLED} plugins"));
    }
    for (id, record) in &config.plugins {
        check_record(id, record)
            .map_err(|message| format!("plugin {:?}: {message}", super::truncate(id, 64)))?;
    }
    Ok(config)
}

fn to_object(config: &ConfigFile) -> Map<String, Value> {
    match serde_json::to_value(config) {
        Ok(Value::Object(object)) => object,
        _ => Map::new(),
    }
}

/// The plugin store and configuration of one Bake home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginHome {
    root: PathBuf,
}

impl PluginHome {
    /// The plugins of the Bake home `bake_home`.
    pub fn new(bake_home: &Path) -> Self {
        Self {
            root: bake_home.join(PLUGINS_DIR),
        }
    }

    /// `plugins.json`.
    pub fn config_path(&self) -> PathBuf {
        self.root.join(CONFIG_FILE)
    }

    /// The store directory.
    pub fn store_dir(&self) -> PathBuf {
        self.root.join(STORE_DIR)
    }

    fn store(&self) -> JsonFileStore {
        JsonFileStore::new(self.config_path(), MAX_CONFIG_BYTES)
    }

    fn invalid(&self, reason: impl fmt::Display) -> PluginHomeError {
        error(format!(
            "{} is invalid: {reason}. Repair it, or remove it to disable every plugin",
            self.config_path().display()
        ))
    }

    fn untrusted(&self, reason: impl fmt::Display) -> PluginHomeError {
        error(format!(
            "{} is not trusted: {reason}",
            self.config_path().display()
        ))
    }

    /// The enabled plugins by id; none when `plugins.json` does not exist.
    /// On Unix an existing file is read through the [trust
    /// checks](trust), so an untrusted one is an error. Reads without the
    /// lock, as a session does. Blocks.
    pub fn enabled(&self) -> Result<BTreeMap<String, EnabledPlugin>, PluginHomeError> {
        Ok(self.read_config()?.0)
    }

    /// [`Self::enabled`], and on Unix the canonical plugins directory the
    /// file was read from, when it exists.
    fn read_config(
        &self,
    ) -> Result<(BTreeMap<String, EnabledPlugin>, Option<PathBuf>), PluginHomeError> {
        let empty = || Ok((BTreeMap::new(), None));
        // A home without the file enables nothing and is not checked, so
        // an empty home works anywhere.
        match std::fs::symlink_metadata(self.config_path()) {
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return empty(),
            _ => {}
        }
        #[cfg(unix)]
        let (bytes, root) = {
            let Some(root) =
                trust::open_dir(&self.root).map_err(|reason| self.untrusted(reason))?
            else {
                return empty();
            };
            let Some(bytes) = root
                .read(CONFIG_FILE, MAX_CONFIG_BYTES as usize)
                .map_err(|reason| self.untrusted(reason))?
            else {
                return empty();
            };
            (bytes, Some(root.path().to_owned()))
        };
        #[cfg(not(unix))]
        let (bytes, root) = match read_regular(&self.config_path(), MAX_CONFIG_BYTES as usize) {
            Ok(bytes) => (bytes, None),
            Err(ReadError::NotFound) => return empty(),
            Err(failure) => return Err(self.invalid(failure)),
        };
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err(self.invalid(format!("it is larger than {MAX_CONFIG_BYTES} bytes")));
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| self.invalid("it is not UTF-8"))?;
        let object =
            crate::json_store::parse_object(text).map_err(|reason| self.invalid(reason))?;
        let plugins = parse_config(object)
            .map_err(|reason| self.invalid(reason))?
            .plugins;
        Ok((plugins, root))
    }

    /// Applies `change` to the parsed configuration under the store's lock,
    /// writing it when `change` returns `Ok(true)`. Blocks.
    fn update<T>(
        &self,
        change: impl FnOnce(&mut ConfigFile) -> Result<(bool, T), PluginHomeError>,
    ) -> Result<T, PluginHomeError> {
        let mut outcome = None;
        let updated = self.store().update(|object| {
            let parsed =
                parse_config(std::mem::take(object)).map_err(|reason| self.invalid(reason));
            let result = parsed.and_then(|mut config| {
                let changed = change(&mut config);
                *object = to_object(&config);
                changed
            });
            let write = matches!(result, Ok((true, _)));
            outcome = Some(result.map(|(_, value)| value));
            write
        });
        match (updated, outcome) {
            // An unsynced directory flush still committed the change, and
            // later reads see it.
            (Ok(_), Some(outcome)) => outcome,
            (Err(failure), Some(outcome)) if failure.is_committed() => outcome,
            // Refused before `change` ran (the file is not a regular JSON
            // object), or the write failed.
            (Err(failure), _) => Err(error(failure.to_string())),
            (Ok(_), None) => Err(error("the update did not run")),
        }
    }

    /// The package with hash `sha256` from the store, read once and
    /// rehashed. Blocks.
    pub fn read_installed(&self, sha256: &str) -> Result<WasmPackage, PluginHomeError> {
        let sha256 = parse_hash(sha256)?;
        let dir = self.store_dir().join(&sha256);
        let package = self.read_copy(&sha256, &dir)?;
        let actual = package_hash(&package);
        if actual != sha256 {
            return Err(error(format!(
                "the store copy {} has changed: its contents hash to sha256:{actual}",
                dir.display()
            )));
        }
        Ok(package)
    }

    /// Reads the store copy `dir` of `sha256`: on Unix through the [trust
    /// checks](trust), opening `store` and the copy without following a
    /// link; elsewhere by path, as [`WasmPackage::read_dir`] does.
    fn read_copy(&self, sha256: &str, dir: &Path) -> Result<WasmPackage, PluginHomeError> {
        let not_installed = || {
            error(format!(
                "the package sha256:{sha256} is not installed in {}",
                self.store_dir().display()
            ))
        };
        #[cfg(unix)]
        {
            let untrusted = |reason: String| {
                error(format!(
                    "the store copy {} is not trusted: {reason}",
                    dir.display()
                ))
            };
            let mut copy = trust::open_dir(&self.root).map_err(untrusted)?;
            for name in [STORE_DIR, sha256] {
                copy = match copy {
                    Some(parent) => parent.dir(Path::new(name)).map_err(untrusted)?,
                    None => None,
                };
            }
            let copy = copy.ok_or_else(not_installed)?;
            let read = |name: &str, limit: usize| {
                copy.read(name, limit)
                    .map_err(untrusted)?
                    .ok_or_else(|| error(format!("the store copy {} has no {name}", dir.display())))
            };
            let manifest = read("plugin.json", super::wasm::MAX_MANIFEST_BYTES)?;
            let component = read("plugin.wasm", super::wasm::MAX_COMPONENT_BYTES)?;
            WasmPackage::new(&manifest, component)
                .map_err(|failure| error(format!("the store copy {}: {failure}", dir.display())))
        }
        #[cfg(not(unix))]
        {
            match std::fs::symlink_metadata(dir) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => {
                    return Err(error(format!(
                        "{} is not a directory of the store",
                        dir.display()
                    )));
                }
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
                    return Err(not_installed());
                }
                Err(failure) => {
                    return Err(error(format!("cannot read {}: {failure}", dir.display())));
                }
            }
            WasmPackage::read_dir(dir)
                .map_err(|failure| error(format!("the store copy {}: {failure}", dir.display())))
        }
    }

    /// Copies `package` into the store under its hash (see the module
    /// documentation). Installing a package the store already holds checks
    /// that copy and changes nothing. Does not enable it. Blocks.
    pub fn install(&self, package: &WasmPackage) -> Result<Installed, PluginHomeError> {
        let sha256 = package_hash(package);
        let plugin = package.manifest().plugin();
        let installed = |already| Installed {
            id: plugin.id().as_str().to_owned(),
            version: plugin.version().to_owned(),
            sha256: sha256.clone(),
            already,
        };
        let store = self.store_dir();
        let dest = store.join(&sha256);
        if std::fs::symlink_metadata(&dest).is_ok() {
            return self
                .read_installed(&sha256)
                .map(|_| installed(true))
                .map_err(|failure| {
                    error(format!(
                        "{failure}; remove {} and install again",
                        dest.display()
                    ))
                });
        }
        private_dir(&store)
            .map_err(|failure| error(format!("cannot create {}: {failure}", store.display())))?;
        let temp = TempDir::create(&store)?;
        temp.write("plugin.json", package.manifest_bytes())?;
        temp.write("plugin.wasm", package.component())?;
        temp.seal()?;
        match std::fs::rename(&temp.path, &dest) {
            Ok(()) => {
                temp.disarm();
                sync_dir(&store);
                Ok(installed(false))
            }
            Err(failure) => {
                drop(temp);
                // Another install of the same package may have won the
                // rename; its copy is as good as this one.
                if self.read_installed(&sha256).is_ok() {
                    return Ok(installed(true));
                }
                Err(error(format!(
                    "cannot move the package into {}: {failure}",
                    dest.display()
                )))
            }
        }
    }

    /// Enables the installed package `sha256`: records its id, hash, API
    /// version, version, and vetoed tools in `plugins.json`. Refused when
    /// the store copy does not check out, when another package of the same
    /// id is enabled (disable it first), or past [`MAX_ENABLED`]. Enabling
    /// the package already enabled changes nothing, and keeps a recorded
    /// failure: only [`Self::disable`] clears one. Blocks.
    pub fn enable(&self, sha256: &str) -> Result<Enabled, PluginHomeError> {
        if !trust::SUPPORTED {
            return Err(error(UNSUPPORTED));
        }
        let package = self.read_installed(sha256)?;
        let manifest = package.manifest();
        let id = manifest.plugin().id().as_str().to_owned();
        let record = EnabledPlugin {
            sha256: package_hash(&package),
            api: manifest.api().to_owned(),
            version: manifest.plugin().version().to_owned(),
            veto_tools: manifest.plugin().veto_tools().map(str::to_owned).collect(),
            failure: None,
        };
        self.update(|config| match config.plugins.get(&id) {
            Some(existing) if existing.sha256 == record.sha256 => Ok((
                false,
                Enabled {
                    id: id.clone(),
                    record: existing.clone(),
                    changed: false,
                },
            )),
            Some(existing) => Err(error(format!(
                "plugin {id} is already enabled as sha256:{}; disable it first",
                existing.sha256
            ))),
            None if config.plugins.len() >= MAX_ENABLED => Err(error(format!(
                "at most {MAX_ENABLED} plugins can be enabled"
            ))),
            None => {
                config.plugins.insert(id.clone(), record.clone());
                Ok((
                    true,
                    Enabled {
                        id: id.clone(),
                        record: record.clone(),
                        changed: true,
                    },
                ))
            }
        })
    }

    /// Disables plugin `id`, removing its record and any recorded failure;
    /// the removed record, or `None` when it was not enabled. The store copy
    /// stays. Blocks.
    pub fn disable(&self, id: &str) -> Result<Option<EnabledPlugin>, PluginHomeError> {
        let id = PluginId::new(id).map_err(|failure| error(failure.to_string()))?;
        self.update(|config| {
            let removed = config.plugins.remove(id.as_str());
            Ok((removed.is_some(), removed))
        })
    }

    /// The registry a session in `workspace` runs: each enabled plugin in
    /// id order, as the module documentation describes, and a warning for
    /// each that starts blocked. An invalid or untrusted `plugins.json` is
    /// an error, and so is a plugins directory that overlaps `workspace`
    /// or lies in a git working tree ([`trust::check_outside`]) while it
    /// enables any plugin. Blocks.
    pub fn load(
        &self,
        setup: &WasmSetup,
        workspace: Option<&Path>,
    ) -> Result<LoadedPlugins, PluginHomeError> {
        let (enabled, root) = self.read_config()?;
        if !enabled.is_empty() {
            if !trust::SUPPORTED {
                return Err(self.untrusted(format!(
                    "{UNSUPPORTED}. Run `bake-rs plugin disable` for each enabled plugin, or remove the file"
                )));
            }
            let root = root.ok_or_else(|| self.untrusted("its directory cannot be resolved"))?;
            trust::check_outside(&root, workspace).map_err(|reason| self.untrusted(reason))?;
        }
        let mut registry = PluginRegistry::new();
        let mut warnings = Vec::new();
        for (id, record) in enabled {
            let (manifest, failure) =
                check_record(&id, &record).map_err(|reason| self.invalid(reason))?;
            let tools = record.veto_tools.join(", ");
            let plugin = match failure {
                Some(failure) => {
                    warnings.push(format!(
                        "Plugin {id} failed earlier ({}), so it blocks {tools} until `bake-rs plugin disable {id}`",
                        failure.kind.as_str()
                    ));
                    Contribution::Blocked(failure)
                }
                None => match self.loadable(&id, &record, setup) {
                    Ok((package, command)) => Contribution::Wasm(Arc::new(WasmVeto::new(
                        package,
                        command,
                        setup.queue.clone(),
                        setup.limits.clone(),
                    ))),
                    Err(reason) => {
                        warnings.push(format!(
                            "Plugin {id} cannot be loaded, so it blocks {tools}: {reason}"
                        ));
                        Contribution::Blocked(Failure::new(
                            FailureKind::Unloadable,
                            &format!("its package cannot be loaded: {reason}"),
                        ))
                    }
                },
            };
            registry
                .add(Registered {
                    order: 0,
                    manifest,
                    plugin,
                    sha256: Some(record.sha256.clone()),
                    record: Some(FailureRecord {
                        home: self.clone(),
                        id: id.clone(),
                        sha256: record.sha256.clone(),
                    }),
                })
                .map_err(|failure| self.invalid(failure))?;
        }
        Ok(LoadedPlugins { registry, warnings })
    }

    /// The store copy for `record`, checked against it, and the helper.
    fn loadable(
        &self,
        id: &str,
        record: &EnabledPlugin,
        setup: &WasmSetup,
    ) -> Result<(WasmPackage, super::wasm::HelperCommand), String> {
        let package = self
            .read_installed(&record.sha256)
            .map_err(|failure| failure.to_string())?;
        let manifest = package.manifest();
        // The id too: a record must not run one plugin's package under
        // another plugin's name, order, and failure record.
        let matches = manifest.plugin().id().as_str() == id
            && manifest.api() == record.api
            && manifest.plugin().version() == record.version
            && manifest
                .plugin()
                .veto_tools()
                .eq(record.veto_tools.iter().map(String::as_str));
        if !matches {
            return Err("its store copy does not match its record".to_owned());
        }
        let command = setup
            .command
            .clone()
            .ok_or_else(|| "this bake-rs has no plugin helper".to_owned())?;
        Ok((package, command))
    }
}

/// What [`PluginHome::load`] gives a session.
#[derive(Debug)]
pub struct LoadedPlugins {
    /// The registry for [`crate::agent_session::AgentSessionOptions::plugins`].
    pub registry: PluginRegistry,
    /// One line for each plugin that starts blocked.
    pub warnings: Vec<String>,
}

/// Where a configured plugin's failure is recorded: its record in
/// `plugins.json`, while it still pins the same package.
#[derive(Debug, Clone)]
pub(crate) struct FailureRecord {
    home: PluginHome,
    id: String,
    sha256: String,
}

impl FailureRecord {
    /// Records `failure` unless the record already holds one, or the
    /// plugin was disabled or re-enabled with another package meanwhile.
    /// An `Err` means the failure may not be recorded. Blocks.
    pub(crate) fn record(&self, failure: &Failure) -> Result<(), String> {
        let recorded = RecordedFailure {
            kind: failure.kind.as_str().to_owned(),
            message: failure.message.clone(),
            at_ms: bake_ai::now_ms(),
        };
        self.home
            .update(|config| match config.plugins.get_mut(&self.id) {
                Some(record) if record.sha256 == self.sha256 && record.failure.is_none() => {
                    record.failure = Some(recorded);
                    Ok((true, ()))
                }
                _ => Ok((false, ())),
            })
            .map_err(|failure| {
                format!(
                    "the plugin's failure could not be recorded in {}: {failure}",
                    self.home.config_path().display()
                )
            })
    }
}

fn private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// Flushes a directory after a rename into it, on Unix; best effort, as a
/// later read rehashes the copy anyway.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = std::fs::File::open(dir) {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// A private directory beside the store that becomes a store copy by one
/// rename; removed on drop unless disarmed.
struct TempDir {
    path: PathBuf,
    armed: bool,
}

impl TempDir {
    fn create(store: &Path) -> Result<Self, PluginHomeError> {
        let mut random = [0u8; 8];
        getrandom::getrandom(&mut random)
            .map_err(|failure| error(format!("cannot name a temporary directory: {failure}")))?;
        let name: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = store.join(format!(".install-{name}"));
        let mut builder = std::fs::DirBuilder::new();
        // A new directory only: a name taken already is an error.
        builder.recursive(false);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder
            .create(&path)
            .map_err(|failure| error(format!("cannot create {}: {failure}", path.display())))?;
        Ok(Self { path, armed: true })
    }

    /// Writes `name` once, exclusively, owner-only, and flushes it.
    fn write(&self, name: &str, bytes: &[u8]) -> Result<(), PluginHomeError> {
        use std::io::Write;
        let path = self.path.join(name);
        let failed =
            |failure: std::io::Error| error(format!("cannot write {}: {failure}", path.display()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&path).map_err(failed)?;
        file.write_all(bytes).map_err(failed)?;
        file.sync_all().map_err(failed)
    }

    /// Makes both files read-only and flushes the directory.
    fn seal(&self) -> Result<(), PluginHomeError> {
        for name in ["plugin.json", "plugin.wasm"] {
            let path = self.path.join(name);
            let mut permissions = std::fs::metadata(&path)
                .map_err(|failure| error(format!("cannot seal {}: {failure}", path.display())))?
                .permissions();
            permissions.set_readonly(true);
            std::fs::set_permissions(&path, permissions)
                .map_err(|failure| error(format!("cannot seal {}: {failure}", path.display())))?;
        }
        sync_dir(&self.path);
        Ok(())
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // A known real directory this process created, holding only the
        // two files it wrote. Windows removes a read-only file only once
        // the attribute is cleared; Unix needs no file permission to
        // unlink, and clearing it there would make the file world-writable.
        for name in ["plugin.json", "plugin.wasm"] {
            let path = self.path.join(name);
            #[cfg(windows)]
            if let Ok(metadata) = std::fs::symlink_metadata(&path) {
                let mut permissions = metadata.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                let _ = std::fs::set_permissions(&path, permissions);
            }
            let _ = std::fs::remove_file(&path);
        }
        let _ = std::fs::remove_dir(&self.path);
    }
}

mod trust;

#[cfg(test)]
mod tests;
