//! The child environment: nothing is inherited unless the caller asks.
//!
//! Pi's `StdioTransport` merges the whole parent environment by default
//! (`inheritEnv`). Bake's TypeScript MCP transport instead passes
//! `scrubbedParentEnv()` from `packages/subprocess/subprocess/src/index.ts`
//! with `inheritEnv: false`, and Bake keeps that rule: credential-shaped
//! names (`SENSITIVE_ENV_PATTERN`, `/KEY|PASSWORD|SECRET|TOKEN/i`), every
//! `DSH_*` name, and the `BAKE_*` spellings of the facts the harness manages
//! never reach a server implicitly. A credential a server needs goes through
//! its explicit configured environment, set after the scrub.

use std::ffi::{OsStr, OsString};

use bake_process::environment::is_credential_name;

/// The `BAKE_*` spellings of the facts the harness manages (`managedBakeNames`
/// in the TypeScript source); other `BAKE_*` names are user settings and pass.
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
    is_credential_name(name) || upper.starts_with("DSH_") || MANAGED_BAKE.contains(&upper.as_str())
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
    fn names_follow_the_typescript_scrub() {
        // `SENSITIVE_ENV_PATTERN` cases from Bake's
        // packages/subprocess/subprocess/tests/service.spec.ts, as the Rust
        // exec port checks them, plus the `DSH_*` and managed `BAKE_*` rules.
        let dropped = [
            "SCRUB_PROBE_PASSWORD",
            "API_KEY",
            "client_secret",
            "myToken",
            "MONKEY",
            "key",
            "DSH_HOME",
            "dsh_session_id",
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
