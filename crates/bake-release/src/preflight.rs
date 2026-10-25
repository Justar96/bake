//! Checks a release tag before anything is built.
//!
//! The tag must name the workspace version, which must be stable, with
//! written changelog notes, and newer than the native release the download
//! service serves; a rerun (`retry`) accepts the same version, which its
//! publish job then takes from the version's GitHub release instead of
//! signing it again.
//!
//! Ported from Bake's `scripts/release/preflight.ts` (revision 97ce6abba0).

use std::path::Path;

use bake_update::version::compare_versions;

use crate::util::{Result, read_text};
use crate::workspace::{changelog_section, is_stable, version};

/// What a preflight found ready: the version and its release notes.
#[derive(Debug)]
pub struct Ready {
    pub version: String,
    pub notes: String,
}

/// Checks the workspace against `tag` and, when given, the version the
/// live channel serves. Every problem is reported, one per line.
pub fn preflight(root: &Path, tag: Option<&str>, live: Option<&str>, retry: bool) -> Result<Ready> {
    let mut problems = Vec::new();
    let version = version(root)?;
    if !is_stable(&version) {
        problems.push(format!(
            "Automated releases require a stable, canonical version: {version}"
        ));
    }
    if let Some(tag) = tag
        && tag != format!("v{version}")
    {
        problems.push(format!(
            "Tag {tag} does not name the workspace version {version}; expected v{version}"
        ));
    }
    let notes = changelog_section(&read_text(&root.join("CHANGELOG.md"))?, &version);
    match &notes {
        None => problems.push(format!("CHANGELOG.md has no {version} section")),
        Some(notes) if notes.is_empty() => {
            problems.push(format!("CHANGELOG.md's {version} section is empty"))
        }
        Some(_) => {}
    }
    if let Some(published) = live {
        let order = compare_versions(&version, published);
        if order == std::cmp::Ordering::Less || (order == std::cmp::Ordering::Equal && !retry) {
            problems.push(format!(
                "{version} is not newer than the published native {published}; release a new version"
            ));
        }
    }
    if !problems.is_empty() {
        return Err(problems.join("\n"));
    }
    Ok(Ready {
        version,
        notes: notes.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checked_out_version_is_ready_against_an_older_channel() {
        let root = crate::util::repository_root();
        let version = version(&root).expect("version");
        let tag = format!("v{version}");
        let ready = preflight(&root, Some(&tag), Some("0.0.1"), false).expect("ready");
        assert!(!ready.notes.is_empty());
        let refused = preflight(&root, Some("v0.0.0"), Some(&version), false).expect_err("refused");
        assert!(
            refused.contains("does not name the workspace version"),
            "{refused}"
        );
        assert!(refused.contains("is not newer"), "{refused}");
        assert!(
            preflight(&root, Some(&tag), Some(&version), true).is_ok(),
            "a retry may see its own version"
        );
    }
}
