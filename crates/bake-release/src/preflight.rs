//! Checks a release tag before anything is built.
//!
//! The tag must name the workspace version, which must be stable, with
//! written changelog notes, and newer than the native release the download
//! service serves.

use std::path::Path;

use bake_update::version::compare_versions;

use crate::util::{Result, read_text};
use crate::workspace::{changelog_section, is_stable, version};

/// Checks the workspace against `tag` and, when given, the version the
/// live channel serves, and returns the workspace version. Every problem
/// is reported, one per line.
pub fn preflight(root: &Path, tag: Option<&str>, live: Option<&str>) -> Result<String> {
    let version = version(root)?;
    check(&version, &read_text(&root.join("CHANGELOG.md"))?, tag, live)?;
    Ok(version)
}

/// [`preflight`] over a version and changelog text.
pub fn check(version: &str, changelog: &str, tag: Option<&str>, live: Option<&str>) -> Result<()> {
    let mut problems = Vec::new();
    if !is_stable(version) {
        problems.push(format!(
            "A release requires a stable, canonical version: {version}"
        ));
    }
    if let Some(tag) = tag
        && tag != format!("v{version}")
    {
        problems.push(format!(
            "Tag {tag} does not name the workspace version {version}; expected v{version}"
        ));
    }
    match changelog_section(changelog, version) {
        None => problems.push(format!("CHANGELOG.md has no {version} section")),
        Some(notes) if notes.is_empty() => {
            problems.push(format!("CHANGELOG.md's {version} section is empty"));
        }
        Some(_) => {}
    }
    if let Some(published) = live
        && compare_versions(version, published) != std::cmp::Ordering::Greater
    {
        problems.push(format!(
            "{version} is not newer than the published native {published}; release a new version"
        ));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checked_out_version_is_ready_against_an_older_channel() {
        let root = crate::util::repository_root();
        let version = version(&root).expect("version");
        let tag = format!("v{version}");
        assert_eq!(
            preflight(&root, Some(&tag), Some("0.0.1")).expect("ready"),
            version
        );
        let refused = preflight(&root, Some("v0.0.0"), Some(&version)).expect_err("refused");
        assert!(
            refused.contains("does not name the workspace version"),
            "{refused}"
        );
        assert!(refused.contains("is not newer"), "{refused}");
    }

    #[test]
    fn a_version_needs_written_notes() {
        let changelog = "## [Unreleased]\n\n## [1.0.0] - 2026-01-01\n\n- a\n";
        assert!(check("1.0.0", changelog, Some("v1.0.0"), None).is_ok());
        assert!(
            check("1.0.1", changelog, None, None)
                .expect_err("none")
                .contains("no 1.0.1 section")
        );
        assert!(
            check("1.0.0-rc.1", "## [1.0.0-rc.1]\n- a\n", None, None)
                .expect_err("pre")
                .contains("stable")
        );
    }
}
