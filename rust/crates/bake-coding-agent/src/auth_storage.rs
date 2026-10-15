//! Stored credentials from `auth.json`.
//!
//! Ported from Pi `packages/coding-agent/src/core/auth-storage.ts` and the
//! runtime-key overlay of `runtime-credentials.ts`: the read path from
//! v1.1.0, and the writes (`modify` for an API key, `delete`, `reload`) and
//! their tests from v1.2.0, revision
//! `06a466935536f8c9bcacb2d5efc0fc021be1bdff`. Pi keeps credentials in
//! `~/.pi/agent/auth.json`; Bake reads and writes `$BAKE_HOME/auth.json`
//! ([`crate::home`]), never Bake 0.3's `.credentials.yaml` (D25). The file
//! maps a provider id to a credential:
//!
//! ```json
//! { "openai": { "type": "api_key", "key": "sk-...", "env": { "NAME": "value" } } }
//! ```
//!
//! An API key is a configuration value ([`crate::config_value`]), resolved
//! when read with the credential's own `env` first. OAuth credentials are
//! recognized so that they own their provider, as in Pi, but this lane has
//! no OAuth provider to use them.
//!
//! Writes go through [`crate::json_store`]: under a lock, the change is
//! applied to the file as it is now, so other providers' records, including
//! full OAuth records and entries Bake does not recognize, stay as stored,
//! and an instance loaded earlier does not discard another process's write.
//! Runtime keys ([`AuthStorage::set_runtime_api_key`]) are never written.
//! `Debug` output shows provider names and credential types but no key or
//! `env` value.
//!
//! # Deviations from Pi
//!
//! - **Explicit reloads.** Pi rereads the file whenever its revision
//!   changes; Bake reads it when the store is created, after each of its own
//!   writes, and on [`AuthStorage::reload`], for a process that runs one
//!   session. Reads take no lock: writes replace the file atomically.
//! - **API-key writes only.** [`AuthStorage::set_api_key`] stands for Pi's
//!   `modify` with an API-key credential; with no OAuth provider ported,
//!   nothing writes an OAuth record. A storage without a file
//!   ([`AuthStorage::empty`], [`AuthStorage::from_json`]) applies writes in
//!   memory, as Pi's in-memory backend does.
//! - **Malformed files.** As Pi's `AuthStorage`, a file that is missing or
//!   not JSON holds no credentials when loaded, and as Pi's `modify`, a write
//!   leaves a file that is not a JSON object untouched and fails. A
//!   credential that is not an object, or whose `type` is neither `api_key`
//!   nor `oauth`, is ignored; a non-string `key` or `env` value is treated as
//!   absent, where Pi would pass it on. A file larger than
//!   [`MAX_AUTH_FILE_BYTES`] is not read.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde_json::{Map, Value};

use crate::config_value::{ScopedEnv, resolve_config_value};
use crate::json_store::{JsonFileStore, StoreError, parse_object, remove_member, set_member};

/// The largest `auth.json` read, in bytes.
pub const MAX_AUTH_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// A stored credential, Pi's `Credential`. `Debug` hides the key and the
/// `env` values.
#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    /// `type: "api_key"`.
    ApiKey {
        /// The key, a configuration value until read.
        key: Option<String>,
        /// Values the key and headers resolve against first.
        env: Option<ScopedEnv>,
    },
    /// `type: "oauth"`; this lane reads no member of it.
    OAuth,
}

/// Shown in place of a secret.
struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiKey { key, env } => f
                .debug_struct("ApiKey")
                .field("key", &key.as_ref().map(|_| Redacted))
                .field(
                    "env",
                    &env.as_ref()
                        .map(|env| env.keys().map(|name| (name, Redacted)).collect::<Vec<_>>()),
                )
                .finish(),
            Self::OAuth => f.write_str("OAuth"),
        }
    }
}

/// Credentials by provider id, Pi's `CredentialStore` with its runtime
/// overlay (`--api-key`).
#[derive(Clone, Default)]
pub struct AuthStorage {
    stored: BTreeMap<String, Credential>,
    runtime: BTreeMap<String, String>,
    file: Option<JsonFileStore>,
}

impl fmt::Debug for AuthStorage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthStorage")
            .field("stored", &self.stored)
            .field(
                "runtime",
                &self
                    .runtime
                    .keys()
                    .map(|provider| (provider, Redacted))
                    .collect::<Vec<_>>(),
            )
            .field("path", &self.file.as_ref().map(JsonFileStore::path))
            .finish()
    }
}

/// Strips a leading byte-order mark, Pi's `stripBom`.
pub(crate) fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// Reads a small text file: `Ok(None)` when it does not exist.
pub(crate) fn read_text_file(path: &Path, limit: u64) -> std::io::Result<Option<String>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(std::io::Error::other(format!(
            "file is larger than {limit} bytes"
        )));
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// The member that binds a record to a Bake account route
/// ([`crate::cliproxyapi::account`]).
pub(crate) const ACCOUNT_BINDING: &str = "account";

/// The provider whose records [`ACCOUNT_BINDING`] binds.
const BOUND_PROVIDER: &str = "cliproxyapi";

/// `provider`'s credential in `value`. A `cliproxyapi` record bound to the
/// Bake account's route is no credential here: only a verified account
/// read uses its key, so it never reaches another route of that provider,
/// such as the 0.3 import. Other providers' records are read as Pi reads
/// them, whatever members they carry.
fn parse_credential(provider: &str, value: &Value) -> Option<Credential> {
    let object = value.as_object()?;
    if provider == BOUND_PROVIDER && object.contains_key(ACCOUNT_BINDING) {
        return None;
    }
    match object.get("type").and_then(Value::as_str)? {
        "api_key" => Some(Credential::ApiKey {
            key: object.get("key").and_then(Value::as_str).map(str::to_owned),
            env: object.get("env").and_then(Value::as_object).map(|env| {
                env.iter()
                    .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_owned())))
                    .collect()
            }),
        }),
        "oauth" => Some(Credential::OAuth),
        _ => None,
    }
}

/// The credentials of a parsed `auth.json` object.
fn credentials(object: &Map<String, Value>) -> BTreeMap<String, Credential> {
    object
        .iter()
        .filter_map(|(provider, value)| {
            Some((provider.clone(), parse_credential(provider, value)?))
        })
        .collect()
}

/// A credential as an in-memory storage writes it back. An OAuth record's
/// members are not kept in memory, so only its type survives.
fn credential_record(credential: &Credential) -> Value {
    match credential {
        Credential::ApiKey { key, env } => {
            let mut record = Map::new();
            record.insert("type".into(), "api_key".into());
            if let Some(key) = key {
                record.insert("key".into(), key.as_str().into());
            }
            if let Some(env) = env {
                record.insert(
                    "env".into(),
                    Value::Object(
                        env.iter()
                            .map(|(name, value)| (name.clone(), value.as_str().into()))
                            .collect(),
                    ),
                );
            }
            Value::Object(record)
        }
        Credential::OAuth => serde_json::json!({ "type": "oauth" }),
    }
}

impl AuthStorage {
    /// No credentials, held in memory.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Credentials from `auth.json` text, held in memory.
    pub fn from_json(text: &str) -> Self {
        Self {
            stored: parse_object(text)
                .map(|object| credentials(&object))
                .unwrap_or_default(),
            ..Self::default()
        }
    }

    /// Credentials from the file at `path`, which writes then update; none
    /// when it is missing, unreadable, or not JSON.
    pub fn load(path: &Path) -> Self {
        let stored = match read_text_file(path, MAX_AUTH_FILE_BYTES) {
            Ok(Some(text)) => Self::from_json(&text).stored,
            _ => BTreeMap::new(),
        };
        Self {
            stored,
            runtime: BTreeMap::new(),
            file: Some(JsonFileStore::new(path.to_path_buf(), MAX_AUTH_FILE_BYTES)),
        }
    }

    /// The storage with a failure injected into each write.
    #[cfg(test)]
    pub(crate) fn with_fault(mut self, fault: crate::json_store::Fault) -> Self {
        self.file = self.file.map(|file| file.with_fault(fault));
        self
    }

    /// Pi's `reload`: rereads the file. On an error the credentials read
    /// before stay, as in Pi; a missing file holds none.
    pub fn reload(&mut self) -> Result<(), StoreError> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        let path = file.path();
        let text = read_text_file(path, MAX_AUTH_FILE_BYTES).map_err(|source| StoreError::Io {
            operation: "read",
            path: path.to_path_buf(),
            source,
        })?;
        self.stored = match text {
            None => BTreeMap::new(),
            Some(text) => {
                credentials(&parse_object(&text).map_err(|reason| StoreError::Refused {
                    path: path.to_path_buf(),
                    reason,
                })?)
            }
        };
        Ok(())
    }

    /// Applies `change` to the stored records: under the file's lock when
    /// there is a file, then takes the credentials as now stored. After
    /// [`StoreError::Unsynced`] too they are the file's new contents.
    fn update(
        &mut self,
        change: impl FnOnce(&mut Map<String, Value>) -> bool,
    ) -> Result<(), StoreError> {
        match &self.file {
            Some(file) => {
                let updated = file.update(change)?;
                self.stored = credentials(&updated.object);
                if let Some(error) = updated.unsynced {
                    return Err(error);
                }
            }
            None => {
                let mut object = Map::new();
                for (provider, credential) in &self.stored {
                    object.insert(provider.clone(), credential_record(credential));
                }
                change(&mut object);
                self.stored = credentials(&object);
            }
        }
        Ok(())
    }

    /// Pi's `modify` with an API-key credential: stores `key` (a
    /// configuration value, written as given) for `provider`, replacing its
    /// record and keeping every other provider's. After an error for which
    /// [`StoreError::is_committed`] holds, the key is stored and read back.
    pub fn set_api_key(&mut self, provider: &str, key: &str) -> Result<(), StoreError> {
        let record = serde_json::json!({ "type": "api_key", "key": key });
        self.update(|object| set_member(object, provider, record))
    }

    /// Pi's `delete` through its runtime overlay: removes `provider`'s stored
    /// record, keeping every other provider's, then its runtime key. Returns
    /// whether a record was stored. On an error the runtime key stays, unless
    /// [`StoreError::is_committed`] holds: the record is then removed, and
    /// so is the runtime key.
    pub fn remove(&mut self, provider: &str) -> Result<bool, StoreError> {
        let mut removed = false;
        let result = self.update(|object| {
            removed = remove_member(object, provider);
            removed
        });
        if let Err(error) = &result
            && !error.is_committed()
        {
            return Err(result.unwrap_err());
        }
        self.runtime.remove(provider);
        result.map(|()| removed)
    }

    /// Replaces `provider`'s stored record with `next` (`None` removes it)
    /// under the file's lock, and returns the record it replaced, read under
    /// that same lock, with the write's outcome. After an error for which
    /// [`StoreError::is_committed`] holds, `next` is stored. Bake's
    /// account transactions use it to restore exactly what they replaced.
    pub(crate) fn swap_record(
        &mut self,
        provider: &str,
        next: Option<Value>,
    ) -> (Option<Value>, Result<(), StoreError>) {
        let mut previous = None;
        let result = self.update(|object| {
            previous = object.get(provider).cloned();
            match next {
                Some(record) => set_member(object, provider, record),
                None => remove_member(object, provider),
            }
        });
        (previous, result)
    }

    /// Replaces `provider`'s record with `next` only while it is still
    /// `expected`, under the file's lock: a rollback that must not undo a
    /// newer writer's credential. Returns whether it replaced the record,
    /// with the write's outcome: after an error for which
    /// [`StoreError::is_committed`] holds, the replacement is in the file.
    pub(crate) fn replace_if(
        &mut self,
        provider: &str,
        expected: &Value,
        next: Option<Value>,
    ) -> (bool, Result<(), StoreError>) {
        let mut replaced = false;
        let result = self.update(|object| {
            if object.get(provider) != Some(expected) {
                return false;
            }
            replaced = true;
            match next {
                Some(record) => set_member(object, provider, record),
                None => remove_member(object, provider),
            }
        });
        (replaced, result)
    }

    /// Pi's `setRuntimeApiKey`: a key for this process that takes priority
    /// over the stored credential and is never written.
    pub fn set_runtime_api_key(&mut self, provider: &str, key: &str) {
        self.runtime.insert(provider.to_owned(), key.to_owned());
    }

    /// Pi's `removeRuntimeApiKey`.
    pub fn remove_runtime_api_key(&mut self, provider: &str) {
        self.runtime.remove(provider);
    }

    /// `provider`'s runtime key (`--api-key`), if set.
    pub fn runtime_api_key(&self, provider: &str) -> Option<&str> {
        self.runtime.get(provider).map(String::as_str)
    }

    /// Whether a runtime key is set for `provider`.
    pub fn has_runtime_api_key(&self, provider: &str) -> bool {
        self.runtime.contains_key(provider)
    }

    /// Whether `provider` has a stored or runtime credential.
    pub fn has_credential(&self, provider: &str) -> bool {
        self.runtime.contains_key(provider) || self.stored.contains_key(provider)
    }

    /// Pi's `read`: the runtime key, else the stored credential with its
    /// API key resolved (which may run a command).
    pub fn read(&self, provider: &str) -> Option<Credential> {
        if let Some(key) = self.runtime.get(provider) {
            return Some(Credential::ApiKey {
                key: Some(key.clone()),
                env: None,
            });
        }
        match self.stored.get(provider)? {
            Credential::ApiKey {
                key: Some(key),
                env,
            } => Some(Credential::ApiKey {
                key: resolve_config_value(key, env.as_ref()),
                env: env.clone(),
            }),
            other => Some(other.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Bake: only the CLIProxyAPI account's own record is bound; other
    // providers' records with an `account` member read as Pi reads them.
    #[test]
    fn only_the_cliproxyapi_record_is_bound() {
        let storage = AuthStorage::from_json(
            "{\"other\":{\"type\":\"api_key\",\"key\":\"sk-fake-other\",\"account\":\"x\"},\
             \"oauthy\":{\"type\":\"oauth\",\"account\":\"x\"},\
             \"cliproxyapi\":{\"type\":\"api_key\",\"key\":\"sk-fake-bound\",\"account\":\"x\"}}",
        );
        assert_eq!(
            storage.read("other"),
            Some(Credential::ApiKey {
                key: Some("sk-fake-other".into()),
                env: None,
            })
        );
        assert_eq!(storage.read("oauthy"), Some(Credential::OAuth));
        assert!(storage.has_credential("other") && storage.has_credential("oauthy"));
        assert_eq!(storage.read("cliproxyapi"), None);
        assert!(!storage.has_credential("cliproxyapi"));
    }

    #[test]
    fn reads_api_keys_with_their_env() {
        let storage = AuthStorage::from_json(
            "\u{feff}{\"a\":{\"type\":\"api_key\",\"key\":\"$K\",\"env\":{\"K\":\"scoped\"}},\
             \"b\":{\"type\":\"oauth\",\"access\":\"x\",\"refresh\":\"y\",\"expires\":1},\
             \"c\":{\"type\":\"other\"},\"d\":7,\"e\":{\"type\":\"api_key\",\"key\":3}}",
        );
        assert_eq!(
            storage.read("a"),
            Some(Credential::ApiKey {
                key: Some("scoped".into()),
                env: Some([("K".to_owned(), "scoped".to_owned())].into()),
            })
        );
        assert_eq!(storage.read("b"), Some(Credential::OAuth));
        assert_eq!(storage.read("c"), None);
        assert_eq!(storage.read("d"), None);
        assert_eq!(
            storage.read("e"),
            Some(Credential::ApiKey {
                key: None,
                env: None
            })
        );
    }

    #[test]
    fn runtime_keys_take_priority_and_bad_files_hold_nothing() {
        let mut storage = AuthStorage::from_json("not json");
        assert_eq!(storage.read("a"), None);
        storage.set_runtime_api_key("a", "runtime");
        assert!(storage.has_runtime_api_key("a"));
        assert_eq!(
            storage.read("a"),
            Some(Credential::ApiKey {
                key: Some("runtime".into()),
                env: None
            })
        );
        assert!(AuthStorage::from_json("[]").read("0").is_none());
    }

    // Ports of Pi v1.2.0 `test/auth-storage.test.ts` and
    // `test/runtime-credentials.test.ts` for the writes Bake has. Not ported:
    // reload coalescing, cancellation, and lock-library mocks (writes are
    // synchronous here and the lock is the OS's), OAuth refresh, and `list`.
    mod writes {
        use super::*;
        use crate::json_store::StoreError;
        use crate::test_support::TempDir;
        use serde_json::json;

        fn write_auth(path: &Path, value: &Value) {
            std::fs::write(path, serde_json::to_string(value).unwrap()).unwrap();
        }

        fn read_auth(path: &Path) -> Value {
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
        }

        fn api_key(key: &str) -> Option<Credential> {
            Some(Credential::ApiKey {
                key: Some(key.to_owned()),
                env: None,
            })
        }

        // Pi: "modify persists a credential while preserving unrelated
        // external edits", with an OAuth record and an unknown entry.
        #[test]
        fn a_write_keeps_external_edits_and_other_records() {
            let dir = TempDir::new("auth-write");
            let path = dir.join("auth.json");
            write_auth(
                &path,
                &json!({ "anthropic": { "type": "api_key", "key": "old" } }),
            );
            let mut storage = AuthStorage::load(&path);
            let external = json!({
                "anthropic": { "type": "api_key", "key": "old" },
                "openai": { "type": "api_key", "key": "external", "env": { "X": "1" } },
                "oauth": { "type": "oauth", "access": "a", "refresh": "r", "expires": 5, "extra": [1] },
                "odd": { "type": "unknown", "n": 1.5 },
            });
            write_auth(&path, &external);

            storage.set_api_key("anthropic", "new").unwrap();
            let mut expected = external.clone();
            expected["anthropic"] = json!({ "type": "api_key", "key": "new" });
            assert_eq!(read_auth(&path), expected);
            // The instance now sees the external records too.
            assert_eq!(storage.read("anthropic"), api_key("new"));
            assert_eq!(storage.read("oauth"), Some(Credential::OAuth));
            assert!(storage.has_credential("openai"));
            // Restart: a new instance reads what was written.
            assert_eq!(AuthStorage::load(&path).read("anthropic"), api_key("new"));
        }

        // Pi: "serializes concurrent modifications", with stale instances.
        #[test]
        fn stale_instances_merge_the_current_file() {
            let dir = TempDir::new("auth-stale");
            let path = dir.join("auth.json");
            let mut first = AuthStorage::load(&path);
            let mut second = AuthStorage::load(&path);
            first.set_api_key("anthropic", "anthropic-key").unwrap();
            second.set_api_key("openai", "openai-key").unwrap();
            assert_eq!(
                read_auth(&path),
                json!({
                    "anthropic": { "type": "api_key", "key": "anthropic-key" },
                    "openai": { "type": "api_key", "key": "openai-key" },
                })
            );
            assert_eq!(second.read("anthropic"), api_key("anthropic-key"));
            assert_eq!(first.read("openai"), None);
            first.reload().unwrap();
            assert_eq!(first.read("openai"), api_key("openai-key"));
        }

        // Pi: "delete removes one credential while preserving others".
        #[test]
        fn remove_keeps_the_other_records() {
            let dir = TempDir::new("auth-remove");
            let path = dir.join("auth.json");
            write_auth(
                &path,
                &json!({
                    "anthropic": { "type": "api_key", "key": "anthropic-key" },
                    "openai": { "type": "api_key", "key": "openai-key" },
                }),
            );
            let mut storage = AuthStorage::load(&path);
            write_auth(
                &path,
                &json!({
                    "anthropic": { "type": "api_key", "key": "anthropic-key" },
                    "openai": { "type": "api_key", "key": "openai-key" },
                    "google": { "type": "api_key", "key": "external-key" },
                }),
            );
            assert!(storage.remove("anthropic").unwrap());
            assert!(!storage.remove("anthropic").unwrap());
            assert_eq!(storage.read("anthropic"), None);
            assert_eq!(storage.read("openai"), api_key("openai-key"));
            assert_eq!(storage.read("google"), api_key("external-key"));
            let keys: Vec<String> = read_auth(&path)
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            assert_eq!(keys, ["openai", "google"]);
        }

        // Pi: "in-memory storage implements the same credential-store
        // behavior".
        #[test]
        fn in_memory_storage_applies_writes_in_memory() {
            let mut storage =
                AuthStorage::from_json(r#"{"anthropic":{"type":"api_key","key":"initial"}}"#);
            storage.set_api_key("anthropic", "updated").unwrap();
            assert_eq!(storage.read("anthropic"), api_key("updated"));
            assert!(storage.remove("anthropic").unwrap());
            assert!(!storage.has_credential("anthropic"));
            assert!(storage.reload().is_ok());
        }

        // Pi: "does not overwrite malformed auth files"; the snapshot read
        // before stays, as after Pi's failed reload.
        #[test]
        fn a_malformed_file_is_neither_overwritten_nor_leaked() {
            let dir = TempDir::new("auth-malformed");
            let path = dir.join("auth.json");
            write_auth(
                &path,
                &json!({ "anthropic": { "type": "api_key", "key": "stored" } }),
            );
            let mut storage = AuthStorage::load(&path);
            let broken = "{\"anthropic\":{\"type\":\"api_key\",\"key\":\"sk-secret-value\"";
            std::fs::write(&path, broken).unwrap();
            for error in [
                storage.set_api_key("openai", "new").unwrap_err(),
                storage.remove("anthropic").unwrap_err(),
                storage.reload().unwrap_err(),
            ] {
                assert!(matches!(error, StoreError::Refused { .. }), "{error:?}");
                assert!(!error.to_string().contains("sk-secret"));
                assert!(!format!("{error:?}").contains("sk-secret"));
            }
            assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
            assert_eq!(storage.read("anthropic"), api_key("stored"));
            // As Pi's AuthStorage, loading a malformed file holds nothing.
            assert_eq!(AuthStorage::load(&path).read("anthropic"), None);
        }

        // Pi `runtime-credentials.test.ts`: "runtime overrides mask stored
        // credentials without persisting", "delete clears both the override
        // and persisted credential", and "keeps a runtime override when
        // persistent deletion is cancelled" (here: fails).
        #[test]
        fn runtime_keys_are_never_written() {
            let dir = TempDir::new("auth-runtime");
            let path = dir.join("auth.json");
            write_auth(
                &path,
                &json!({ "anthropic": { "type": "api_key", "key": "stored-key" } }),
            );
            let mut storage = AuthStorage::load(&path);
            storage.set_runtime_api_key("anthropic", "runtime-key");
            storage.set_runtime_api_key("openai", "runtime-only");
            assert_eq!(storage.read("anthropic"), api_key("runtime-key"));
            storage.set_api_key("google", "google-key").unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(!text.contains("runtime"), "{text}");
            assert!(!text.contains("openai"), "{text}");
            storage.remove_runtime_api_key("anthropic");
            assert_eq!(storage.read("anthropic"), api_key("stored-key"));

            storage.set_runtime_api_key("anthropic", "runtime-key");
            std::fs::write(&path, "[]").unwrap();
            assert!(storage.remove("anthropic").is_err());
            assert_eq!(storage.read("anthropic"), api_key("runtime-key"));
            write_auth(
                &path,
                &json!({ "anthropic": { "type": "api_key", "key": "stored-key" } }),
            );
            assert!(storage.remove("anthropic").unwrap());
            assert_eq!(storage.read("anthropic"), None);
            assert_eq!(read_auth(&path), json!({}));
        }

        // The replacement succeeded and only the directory flush failed: the
        // instance takes the new contents, `remove` drops the runtime key as
        // after a success, and the error names neither key.
        #[test]
        fn an_unsynced_write_keeps_the_instance_consistent() {
            use crate::json_store::Fault;
            let dir = TempDir::new("auth-unsynced");
            let path = dir.join("auth.json");
            write_auth(
                &path,
                &json!({ "oauth": { "type": "oauth", "access": "a", "refresh": "r", "expires": 1 } }),
            );
            let mut storage = AuthStorage::load(&path);
            storage.file = Some(
                JsonFileStore::new(path.clone(), MAX_AUTH_FILE_BYTES)
                    .with_fault(Fault::DirectorySync),
            );
            let error = storage
                .set_api_key("openai", "sk-unsynced-secret")
                .unwrap_err();
            assert!(error.is_committed(), "{error:?}");
            assert_eq!(storage.read("openai"), api_key("sk-unsynced-secret"));
            assert_eq!(
                AuthStorage::load(&path).read("openai"),
                api_key("sk-unsynced-secret")
            );

            storage.set_runtime_api_key("openai", "sk-runtime-secret");
            let error = storage.remove("openai").unwrap_err();
            assert!(error.is_committed(), "{error:?}");
            assert_eq!(storage.read("openai"), None);
            assert!(!storage.has_runtime_api_key("openai"));
            assert_eq!(
                read_auth(&path),
                json!({ "oauth": { "type": "oauth", "access": "a", "refresh": "r", "expires": 1 } })
            );
            for text in [error.to_string(), format!("{error:?}")] {
                assert!(!text.contains("secret"), "{text}");
            }
        }

        #[test]
        fn debug_output_hides_keys_and_env_values() {
            let mut storage = AuthStorage::from_json(
                r#"{"a":{"type":"api_key","key":"sk-stored-secret","env":{"NAME":"env-secret"}},
                    "b":{"type":"oauth","access":"oauth-secret","refresh":"r","expires":1}}"#,
            );
            storage.set_runtime_api_key("c", "runtime-secret");
            let debug = format!("{storage:?} {:?}", storage.read("a"));
            for secret in [
                "sk-stored-secret",
                "env-secret",
                "oauth-secret",
                "runtime-secret",
            ] {
                assert!(!debug.contains(secret), "{debug}");
            }
            for shown in ["\"a\"", "NAME", "OAuth", "\"c\"", "<redacted>"] {
                assert!(debug.contains(shown), "{debug}");
            }
        }
    }
}
