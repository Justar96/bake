//! The Bake home, retained by D25 and D32.
//!
//! Bake's own rule, not Pi's: `BAKE_HOME`, then `DSH_HOME`, then `~/.bake`,
//! a blank (empty or whitespace-only) variable counting as unset, as
//! `scripts/release/bake` and `resolveDshHome` in
//! `packages/util/home-paths/src/index.ts` apply it. A leading `~` in the
//! value is expanded against the home directory, and the result is made
//! absolute, as `resolveDshHome` does. Pi's equivalent is its agent
//! directory, `~/.pi/agent` (`PI_CODING_AGENT_DIR`), which Bake does not read.
//!
//! A value that is not valid Unicode is used as the path it names, without
//! `~` expansion, rather than treated as unset.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::session::paths::{lexical_normalize, normalize_path};

/// `BAKE_HOME`.
pub const BAKE_HOME_ENV: &str = "BAKE_HOME";
/// `DSH_HOME`, read when `BAKE_HOME` is unset or blank.
pub const DSH_HOME_ENV: &str = "DSH_HOME";
/// The default home's directory name under the user's home.
pub const DEFAULT_HOME_DIR_NAME: &str = ".bake";

/// Resolve the Bake home from an environment lookup and the user's home
/// directory. `None` when no variable is set and the user's home is
/// unknown.
pub fn resolve_bake_home(
    env: impl Fn(&str) -> Option<OsString>,
    user_home: Option<&Path>,
    current_dir: Option<&Path>,
) -> Option<PathBuf> {
    let configured = [BAKE_HOME_ENV, DSH_HOME_ENV]
        .into_iter()
        .filter_map(&env)
        .find(|value| !value.to_string_lossy().trim().is_empty());
    let selected = match configured {
        Some(value) => match value.into_string() {
            Ok(value) => normalize_path(&value),
            Err(value) => PathBuf::from(value),
        },
        None => user_home?.join(DEFAULT_HOME_DIR_NAME),
    };
    let absolute = if selected.is_absolute() {
        selected
    } else {
        current_dir?.join(selected)
    };
    Some(lexical_normalize(&absolute))
}

/// The Bake home from the process environment.
pub fn bake_home() -> Option<PathBuf> {
    let current_dir = std::env::current_dir().ok();
    resolve_bake_home(
        |name| std::env::var_os(name),
        std::env::home_dir().as_deref(),
        current_dir.as_deref(),
    )
}

/// `<home>/sessions`, where Bake keeps Pi-format sessions in place of Pi's
/// `~/.pi/agent/sessions`.
pub fn sessions_dir(home: &Path) -> PathBuf {
    home.join("sessions")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(*value))
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_non_unicode_home_is_used_as_given() {
        use std::os::unix::ffi::OsStringExt;
        let raw = OsString::from_vec(b"/h\xff".to_vec());
        let value = raw.clone();
        let home = resolve_bake_home(
            move |name| (name == BAKE_HOME_ENV).then(|| value.clone()),
            Some(Path::new("/home/u")),
            Some(Path::new("/work")),
        );
        assert_eq!(home, Some(PathBuf::from(raw)));
    }

    #[cfg(unix)]
    #[test]
    fn precedence_and_blank_values() {
        let user = Path::new("/home/u");
        let cwd = Path::new("/work");
        let resolve = |pairs: &[(&str, &str)]| resolve_bake_home(env(pairs), Some(user), Some(cwd));
        assert_eq!(resolve(&[]), Some(PathBuf::from("/home/u/.bake")));
        assert_eq!(
            resolve(&[("BAKE_HOME", "/b"), ("DSH_HOME", "/d")]),
            Some(PathBuf::from("/b"))
        );
        assert_eq!(
            resolve(&[("BAKE_HOME", "  "), ("DSH_HOME", "/d")]),
            Some(PathBuf::from("/d"))
        );
        assert_eq!(
            resolve(&[("BAKE_HOME", ""), ("DSH_HOME", " ")]),
            Some(PathBuf::from("/home/u/.bake"))
        );
        assert_eq!(
            resolve(&[("BAKE_HOME", "rel/../h")]),
            Some(PathBuf::from("/work/h"))
        );
        assert_eq!(resolve_bake_home(env(&[]), None, Some(cwd)), None);
    }

    #[test]
    fn sessions_live_under_the_home() {
        assert_eq!(
            sessions_dir(Path::new("h")),
            Path::new("h").join("sessions")
        );
    }
}
