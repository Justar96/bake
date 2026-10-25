//! The credential filter shared by Bake's bash, direct execution, and MCP
//! children. Ported from `SENSITIVE_ENV_PATTERN` in
//! `packages/subprocess/subprocess/src/index.ts`.
//!
//! Apply it to inherited environment values before adding credentials a
//! caller explicitly supplies. It is a name heuristic: a secret under an
//! innocuous name still passes.

use std::ffi::{OsStr, OsString};

/// Whether `name` contains `KEY`, `PASSWORD`, `SECRET`, or `TOKEN`, ignoring
/// ASCII case, as Bake's TypeScript subprocess filter does.
pub fn is_credential_name(name: &OsStr) -> bool {
    let upper = name.to_string_lossy().to_ascii_uppercase();
    ["KEY", "PASSWORD", "SECRET", "TOKEN"]
        .iter()
        .any(|fragment| upper.contains(fragment))
}

/// Keeps only environment entries whose names are not credential-shaped.
/// Values, spelling, and order of the remaining entries are preserved.
pub fn scrub_environment(
    env: impl IntoIterator<Item = (OsString, OsString)>,
) -> impl Iterator<Item = (OsString, OsString)> {
    env.into_iter()
        .filter(|(name, _)| !is_credential_name(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_names_follow_the_typescript_pattern() {
        // `packages/subprocess/subprocess/tests/service.spec.ts`, plus
        // mixed-case and substring cases. Managed harness names are the
        // caller's policy, separate from this credential filter.
        let names = [
            "PATH",
            "HOME",
            "LANG",
            "NORMAL",
            "BAKE_HOME",
            "DSH_HOME",
            "SCRUB_PROBE_PASSWORD",
            "API_KEY",
            "client_secret",
            "myToken",
            "MONKEY",
            "key",
            "password",
            "secret",
            "token",
        ];
        let env = names.map(|name| (OsString::from(name), OsString::from(name)));
        let kept: Vec<_> = scrub_environment(env).collect();
        let expected = ["PATH", "HOME", "LANG", "NORMAL", "BAKE_HOME", "DSH_HOME"]
            .map(|name| (OsString::from(name), OsString::from(name)));
        assert_eq!(kept, expected);
    }
}
