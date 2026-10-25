//! What a release reads from the Cargo workspace: the version every crate
//! carries, the documentation the binary embeds, and the NOTICE of each
//! Bake crate `bake-cli` links.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::util::{Result, read_text, run};

/// The package whose binary, `bake`, is released.
pub const ROOT_PACKAGE: &str = "bake-cli";

/// The `[workspace.package]` version of the root `Cargo.toml`, which every
/// crate and so every release carries.
///
/// # Errors
///
/// When the manifest has none, or it is not a canonical release version.
pub fn version(root: &Path) -> Result<String> {
    let text = read_text(&root.join("Cargo.toml"))?;
    let table: toml::Table = text
        .parse()
        .map_err(|error| format!("Cargo.toml does not parse: {error}"))?;
    let version = table
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("version"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| "Cargo.toml has no version in [workspace.package]".to_owned())?;
    if !bake_update::version::is_release_version(version) {
        return Err(format!("Cargo.toml names no release version: {version}"));
    }
    Ok(version.to_owned())
}

/// Whether `version` is stable: a release a tag may publish, with no
/// pre-release part.
pub fn is_stable(version: &str) -> bool {
    bake_update::version::is_release_version(version) && !version.contains('-')
}

/// The toolchain channel `rust-toolchain.toml` pins.
pub fn toolchain(root: &Path) -> Result<String> {
    let text = read_text(&root.join("rust-toolchain.toml"))?;
    let table: toml::Table = text
        .parse()
        .map_err(|error| format!("rust-toolchain.toml does not parse: {error}"))?;
    table
        .get("toolchain")
        .and_then(|toolchain| toolchain.get("channel"))
        .and_then(toml::Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| "rust-toolchain.toml names no channel".to_owned())
}

/// What the packer reads from `cargo metadata --no-deps`.
#[derive(Debug, Clone)]
pub struct Inputs {
    /// `bake-cli`'s version, which the binary must report.
    pub version: String,
    /// The `docs/` directory compiled into the binary.
    pub docs: PathBuf,
    /// The NOTICE of each workspace crate `bake-cli` links, by crate name.
    pub notices: BTreeMap<String, PathBuf>,
    pub target_dir: PathBuf,
}

/// Reads [`Inputs`] from `cargo metadata`.
pub fn inputs(root: &Path) -> Result<Inputs> {
    let text = run(
        "cargo",
        &["metadata", "--locked", "--no-deps", "--format-version", "1"],
        root,
    )?;
    let metadata: Value =
        serde_json::from_str(&text).map_err(|error| format!("cargo metadata: {error}"))?;
    inputs_from(&metadata)
}

/// [`inputs`] over parsed metadata.
pub fn inputs_from(metadata: &Value) -> Result<Inputs> {
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or_else(|| "cargo metadata lists no packages".to_owned())?;
    let by_name: HashMap<&str, &Value> = packages
        .iter()
        .filter_map(|package| Some((package.get("name")?.as_str()?, package)))
        .collect();
    let cli = by_name
        .get(ROOT_PACKAGE)
        .ok_or_else(|| format!("cargo metadata lists no {ROOT_PACKAGE} package"))?;
    // Normal path dependencies only: build and dev dependencies are not
    // linked into the binary.
    let mut linked = BTreeSet::new();
    let mut stack = vec![ROOT_PACKAGE];
    while let Some(name) = stack.pop() {
        if !linked.insert(name.to_owned()) {
            continue;
        }
        let Some(package) = by_name.get(name) else {
            continue;
        };
        for dependency in package
            .get("dependencies")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let normal = dependency.get("kind").is_none_or(Value::is_null);
            let path = dependency.get("path").and_then(Value::as_str).is_some();
            if let Some(dep) = dependency.get("name").and_then(Value::as_str)
                && normal
                && path
                && by_name.contains_key(dep)
            {
                stack.push(dep);
            }
        }
    }
    let dir = |package: &Value| -> Option<PathBuf> {
        Path::new(package.get("manifest_path")?.as_str()?)
            .parent()
            .map(Path::to_path_buf)
    };
    let mut notices = BTreeMap::new();
    for name in &linked {
        if let Some(package_dir) = by_name.get(name.as_str()).and_then(|package| dir(package)) {
            let notice = package_dir.join("NOTICE");
            if notice.is_file() {
                notices.insert(name.clone(), notice);
            }
        }
    }
    let version = cli
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{ROOT_PACKAGE} has no version"))?
        .to_owned();
    let docs = dir(cli)
        .ok_or_else(|| format!("{ROOT_PACKAGE} has no manifest path"))?
        .join("docs");
    let target_dir = metadata
        .get("target_directory")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| "cargo metadata names no target directory".to_owned())?;
    Ok(Inputs {
        version,
        docs,
        notices,
        target_dir,
    })
}

/// The section of `changelog` for `version`: the text under its
/// `## [<version>]` heading up to the next `## ` heading, trimmed, or
/// `None` when the changelog has no such heading.
pub fn changelog_section(changelog: &str, version: &str) -> Option<String> {
    let heading = format!("## [{version}]");
    let mut lines = changelog.lines();
    lines.by_ref().find(|line| {
        line.strip_prefix(&heading)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    })?;
    let body: Vec<&str> = lines.take_while(|line| !line.starts_with("## ")).collect();
    Some(body.join("\n").trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_changelog_section_is_the_text_under_its_heading() {
        let text = "# Changelog\n\n## [Unreleased]\n\n## [0.4.2] - 2026-10-25\n\nIntro.\n\n- one\n\n## [0.4.1] - 2026-10-25\n\n- older\n";
        assert_eq!(
            changelog_section(text, "0.4.2").as_deref(),
            Some("Intro.\n\n- one")
        );
        assert_eq!(changelog_section(text, "0.4.1").as_deref(), Some("- older"));
        assert_eq!(changelog_section(text, "0.4.").as_deref(), None);
        assert_eq!(changelog_section(text, "Unreleased").as_deref(), Some(""));
        assert_eq!(changelog_section(text, "9.9.9"), None);
    }

    #[test]
    fn a_stable_version_has_no_pre_release() {
        assert!(is_stable("0.4.2"));
        assert!(!is_stable("0.4.2-rc.1"));
        assert!(!is_stable("v0.4.2"));
    }

    #[test]
    fn the_workspace_version_is_this_crates() {
        let root = crate::util::repository_root();
        assert_eq!(version(&root).expect("version"), env!("CARGO_PKG_VERSION"));
        assert!(!toolchain(&root).expect("channel").is_empty());
    }

    #[test]
    fn linked_notices_follow_normal_path_dependencies() {
        let root = crate::util::repository_root();
        let metadata = serde_json::json!({
            "target_directory": "/t",
            "packages": [
                {"name": "bake-cli", "version": "1.2.3", "manifest_path": root.join("crates/bake-cli/Cargo.toml"),
                 "dependencies": [
                    {"name": "bake-agent", "kind": null, "path": "x"},
                    {"name": "bake-tui", "kind": "dev", "path": "x"},
                    {"name": "serde", "kind": null}]},
                {"name": "bake-agent", "version": "1.2.3", "manifest_path": root.join("crates/bake-agent/Cargo.toml"), "dependencies": []},
                {"name": "bake-tui", "version": "1.2.3", "manifest_path": root.join("crates/bake-tui/Cargo.toml"), "dependencies": []}
            ]
        });
        let inputs = inputs_from(&metadata).expect("inputs");
        assert_eq!(inputs.version, "1.2.3");
        assert_eq!(
            inputs.notices.keys().collect::<Vec<_>>(),
            ["bake-agent"],
            "a dev dependency is not linked"
        );
    }
}
