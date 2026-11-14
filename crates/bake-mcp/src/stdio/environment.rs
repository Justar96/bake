//! The child environment: nothing is inherited unless the caller asks.
//!
//! Pi's `StdioTransport` merges the whole parent environment by default
//! (`inheritEnv`). Bake instead starts a server from a scrubbed copy of its
//! own environment: credential-shaped names (those containing `KEY`,
//! `PASSWORD`, `SECRET`, or `TOKEN`, ignoring case) and the `BAKE_*` names
//! of the facts the harness manages never reach a server implicitly. A credential a server needs goes through
//! its explicit configured environment, set after the scrub.

use std::ffi::{OsStr, OsString};

use bake_process::environment::is_credential_name;

/// The `BAKE_*` names of the facts the harness manages; other `BAKE_*` names
/// are user settings and pass.
const MANAGED_BAKE: [&str; 4] = [
    "BAKE_HOME",
    "BAKE_SHELL",
    "BAKE_SESSION_ID",
    "BAKE_PTY_SESSION_ID",
];

/// Whether the scrub drops `name`. Names compare case-insensitively, as
/// Windows environment names do.
pub fn is_scrubbed_name(name: &OsStr) -> bool {
    let upper = name.to_string_lossy().to_ascii_uppercase();
    is_credential_name(name) || MANAGED_BAKE.contains(&upper.as_str())
}

/// This process's environment without the names [`is_scrubbed_name`] drops:
/// the base a stdio server's configured environment is laid over. `PATH`,
/// `HOME`, locale, and proxy variables survive, so servers start normally.
///
/// A heuristic: a secret under an innocuous name still passes, and a caller
/// that must not pass the user's environment at all starts from nothing.
pub fn scrubbed_parent_environment() -> Vec<(OsString, OsString)> {
    std::env::vars_os()
        .filter(|(name, _)| !is_scrubbed_name(name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_and_managed_names_are_scrubbed() {
        // The credential filter's cases, plus the managed `BAKE_*` names.
        let dropped = [
            "SCRUB_PROBE_PASSWORD",
            "API_KEY",
            "client_secret",
            "myToken",
            "MONKEY",
            "key",
            "BAKE_HOME",
            "bake_session_id",
            "BAKE_PTY_SESSION_ID",
        ];
        let kept = [
            "PATH",
            "HOME",
            "LANG",
            "NORMAL",
            "BAKE_NO_UPDATE_CHECK",
            "HTTPS_PROXY",
        ];
        for name in dropped {
            assert!(is_scrubbed_name(OsStr::new(name)), "{name} was kept");
        }
        for name in kept {
            assert!(!is_scrubbed_name(OsStr::new(name)), "{name} was dropped");
        }
    }
}
