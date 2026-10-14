//! Stored credentials from `auth.json`, read only.
//!
//! Ported from the read path of Pi
//! `packages/coding-agent/src/core/auth-storage.ts` (v1.1.0) and the
//! runtime-key overlay of `runtime-credentials.ts`. Pi keeps credentials in
//! `~/.pi/agent/auth.json`; Bake reads `$BAKE_HOME/auth.json`
//! ([`crate::home`]). The file maps a provider id to a credential:
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
//! # Deviations from Pi
//!
//! - **Read only.** No login, logout, or refresh writes the file, and no
//!   lock is taken.
//! - **Loaded once.** Pi rereads the file when its revision changes; Bake
//!   reads it when the store is created, for a process that runs one
//!   session.
//! - **Malformed files.** As Pi's `AuthStorage`, a file that is missing or
//!   not JSON holds no credentials. A credential that is not an object, or
//!   whose `type` is neither `api_key` nor `oauth`, is ignored; a
//!   non-string `key` or `env` value is treated as absent, where Pi would
//!   pass it on. A file larger than [`MAX_AUTH_FILE_BYTES`] is not read.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde_json::Value;

use crate::config_value::{ScopedEnv, resolve_config_value};

/// The largest `auth.json` read, in bytes.
pub const MAX_AUTH_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// A stored credential, Pi's `Credential`.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Credentials by provider id, Pi's `CredentialStore` read path with its
/// runtime overlay (`--api-key`).
#[derive(Debug, Clone, Default)]
pub struct AuthStorage {
    stored: BTreeMap<String, Credential>,
    runtime: BTreeMap<String, String>,
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

fn parse_credential(value: &Value) -> Option<Credential> {
    let object = value.as_object()?;
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

impl AuthStorage {
    /// No credentials.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Credentials from `auth.json` text.
    pub fn from_json(text: &str) -> Self {
        let stored = serde_json::from_str::<Value>(strip_bom(text))
            .ok()
            .and_then(|value| match value {
                Value::Object(object) => Some(object),
                _ => None,
            })
            .map(|object| {
                object
                    .iter()
                    .filter_map(|(provider, value)| {
                        Some((provider.clone(), parse_credential(value)?))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            stored,
            runtime: BTreeMap::new(),
        }
    }

    /// Credentials from the file at `path`; none when it is missing,
    /// unreadable, or not JSON.
    pub fn load(path: &Path) -> Self {
        match read_text_file(path, MAX_AUTH_FILE_BYTES) {
            Ok(Some(text)) => Self::from_json(&text),
            _ => Self::empty(),
        }
    }

    /// Pi's `setRuntimeApiKey`: a key for this process that takes priority
    /// over the stored credential and is never written.
    pub fn set_runtime_api_key(&mut self, provider: &str, key: &str) {
        self.runtime.insert(provider.to_owned(), key.to_owned());
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
}
