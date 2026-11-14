//! `bake-release prepare`: the release commit's changes on `develop`.
//!
//! It sets the new version in `[workspace.package]`, refreshes both
//! lockfiles, turns the changelog's `[Unreleased]` entries into the
//! version's section under a fresh empty `[Unreleased]`, and runs the
//! preflight offline for the version's tag. It neither commits nor pushes:
//! the person releasing reads the diff and does that.

use std::path::{Path, PathBuf};

use bake_update::version::compare_versions;

use crate::preflight;
use crate::runner::{Command, Runner};
use crate::util::{Result, read_text};
use crate::workspace::{changelog_section, is_stable, version_in};

/// The branch a release is prepared on.
pub const BRANCH: &str = "develop";

/// What [`prepare`] is asked to do.
#[derive(Debug)]
pub struct Options {
    pub root: PathBuf,
    pub version: String,
    /// The release date, `YYYY-MM-DD`.
    pub date: String,
    pub dry_run: bool,
}

/// `manifest` with the `version` of its `[workspace.package]` table set to
/// `version` and every other byte kept.
///
/// # Errors
///
/// When the table has no plain `version = "..."` line, or the edited text
/// would not name `version` there.
pub fn set_workspace_version(manifest: &str, version: &str) -> Result<String> {
    let mut in_table = false;
    let mut offset = 0;
    for line in manifest.split_inclusive('\n') {
        let content = line.split('#').next().unwrap_or_default().trim();
        if content.starts_with('[') {
            in_table = content == "[workspace.package]";
        } else if in_table && let Some((start, end)) = version_value(line) {
            let edited = format!(
                "{}{version}{}",
                &manifest[..offset + start],
                &manifest[offset + end..]
            );
            return match version_in(&edited) {
                Ok(found) if found == version => Ok(edited),
                _ => Err(format!(
                    "Cargo.toml's [workspace.package] version line could not be set to {version}"
                )),
            };
        }
        offset += line.len();
    }
    Err("Cargo.toml has no version line in [workspace.package]".to_owned())
}

/// The byte range of the quoted value of a `version = "..."` line.
fn version_value(line: &str) -> Option<(usize, usize)> {
    let indent = line.len() - line.trim_start().len();
    let rest = line[indent..].strip_prefix("version")?;
    let after_key = rest.trim_start_matches([' ', '\t']);
    let after_equals = after_key.strip_prefix('=')?.trim_start_matches([' ', '\t']);
    let value = after_equals.strip_prefix('"')?;
    let length = value.find(['"', '\\', '\n'])?;
    value[length..].starts_with('"').then_some(())?;
    let start = line.len() - value.len();
    Some((start, start + length))
}

/// Whether `line` is the changelog heading `## [<name>]`, as
/// [`changelog_section`] reads it.
fn is_heading(line: &str, name: &str) -> bool {
    // As `str::lines` ends a line: at `\n`, or `\r\n`.
    let line = line
        .strip_suffix('\n')
        .map_or(line, |line| line.strip_suffix('\r').unwrap_or(line));
    line.strip_prefix("## [")
        .and_then(|rest| rest.strip_prefix(name))
        .and_then(|rest| rest.strip_prefix(']'))
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
}

/// `changelog` with its `## [Unreleased]` entries moved under a new
/// `## [<version>] - <date>` heading, and `## [Unreleased]` left empty
/// above it. Every other byte is kept.
///
/// # Errors
///
/// When the changelog has no `[Unreleased]` heading, its section is empty,
/// or the version already has a section.
pub fn rotate_changelog(changelog: &str, version: &str, date: &str) -> Result<String> {
    let lines: Vec<&str> = changelog.split_inclusive('\n').collect();
    if lines.iter().any(|line| is_heading(line, version)) {
        return Err(format!("CHANGELOG.md already has a {version} section"));
    }
    let unreleased = lines
        .iter()
        .position(|line| is_heading(line, "Unreleased"))
        .ok_or_else(|| "CHANGELOG.md has no ## [Unreleased] heading".to_owned())?;
    let next = lines[unreleased + 1..]
        .iter()
        .position(|line| line.starts_with("## "))
        .map_or(lines.len(), |at| unreleased + 1 + at);
    let body = lines[unreleased + 1..next].concat();
    let entries = body.trim();
    if entries.is_empty() {
        return Err(format!(
            "CHANGELOG.md's [Unreleased] section is empty; write {version}'s entries there first"
        ));
    }
    let mut out = lines[..=unreleased].concat();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("\n## [{version}] - {date}\n\n{entries}\n"));
    if next < lines.len() {
        out.push('\n');
        out.push_str(&lines[next..].concat());
    }
    Ok(out)
}

/// Whether `date` reads as `YYYY-MM-DD` with a month and day in range.
pub fn is_date(date: &str) -> bool {
    let bytes = date.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let number = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = date.get(range)?;
        part.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    matches!(
        (number(0..4), number(5..7), number(8..10)),
        (Some(_), Some(1..=12), Some(1..=31))
    )
}

/// Refuses a version a release cannot move to from `current`.
pub fn check_version(version: &str, current: &str) -> Result<()> {
    if !is_stable(version) {
        return Err(format!(
            "{version} is not a canonical release version (MAJOR.MINOR.PATCH)"
        ));
    }
    if compare_versions(version, current) != std::cmp::Ordering::Greater {
        return Err(format!(
            "{version} is not newer than the workspace version {current}"
        ));
    }
    Ok(())
}

/// Prepares the release, or with `dry_run` shows what it would change.
pub fn prepare(options: &Options, runner: &mut dyn Runner) -> Result<()> {
    let root = &options.root;
    let version = options.version.as_str();
    if !is_date(&options.date) {
        return Err(format!(
            "{} is not a date; give --date as YYYY-MM-DD",
            options.date
        ));
    }
    let branch = runner
        .query(&Command::new(
            "git",
            &["rev-parse", "--abbrev-ref", "HEAD"],
            root,
        ))?
        .unwrap_or_default();
    if branch.trim() != BRANCH {
        return Err(format!(
            "A release is prepared on {BRANCH}; this checkout is on {}",
            branch.trim()
        ));
    }
    let manifest_path = root.join("Cargo.toml");
    let changelog_path = root.join("CHANGELOG.md");
    let manifest = read_text(&manifest_path)?;
    let current = version_in(&manifest)?;
    check_version(version, &current)?;
    let changelog = read_text(&changelog_path)?;
    let next_manifest = set_workspace_version(&manifest, version)?;
    let next_changelog = rotate_changelog(&changelog, version, &options.date)?;
    let tag = format!("v{version}");
    preflight::check(version, &next_changelog, Some(&tag), None)?;
    let entries = changelog_section(&next_changelog, version)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.starts_with("- "))
        .count();
    let updates = [
        Command::new("cargo", &["update", "--workspace", "--offline"], root),
        Command::new(
            "cargo",
            &["update", "--workspace", "--offline"],
            root.join("fuzz"),
        ),
    ];
    let changes = [
        format!("Cargo.toml: [workspace.package] version {current} -> {version}"),
        format!(
            "CHANGELOG.md: the [Unreleased] entries ({entries} bullets) become ## [{version}] - {}, under an empty [Unreleased]",
            options.date
        ),
    ];
    if options.dry_run {
        runner.say(&format!(
            "A dry run; nothing is written. Preparing Bake {version} would change:"
        ));
        for change in &changes {
            runner.say(&format!("  {change}"));
        }
        for update in &updates {
            runner.say(&format!("would run: {}", in_dir(update, root)));
        }
        runner.say(&format!(
            "The preflight for {tag} passes on the changed files."
        ));
    } else {
        write(&manifest_path, &next_manifest)?;
        write(&changelog_path, &next_changelog)?;
        for change in &changes {
            runner.say(&format!("Changed {change}"));
        }
        for update in &updates {
            runner.say(&format!("$ {}", in_dir(update, root)));
            runner.effect(update)?;
        }
        let ready = preflight::preflight(root, Some(&tag), None)?;
        runner.say(&format!("Bake {ready} is ready to release"));
    }
    runner.say(&next_steps(version));
    Ok(())
}

/// What the person releasing does after `prepare`.
pub fn next_steps(version: &str) -> String {
    format!(
        "Next:
  1. Review the diff, then commit and push it:
       git commit -am \"release: {version}\" && git push origin {BRANCH}
  2. Open the release pull request and merge it with a merge commit:
       gh pr create --base main --head {BRANCH} --title \"release: {version}\" --body \"Bake {version}\"
  3. Build, pack, and check the archives on Linux x64 and Windows x64, then run
       bake-release publish {version}
     (distribution/README.md, \"Release steps\")."
    )
}

fn in_dir(command: &Command, root: &Path) -> String {
    match command.dir.strip_prefix(root) {
        Ok(relative) if !relative.as_os_str().is_empty() => {
            format!("(cd {} && {command})", relative.display())
        }
        _ => command.to_string(),
    }
}

fn write(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::fake::Fake;
    use crate::util::Scratch;
    use proptest::prelude::*;

    const MANIFEST: &str = "[workspace]\nmembers = [\"a\"]\n\n[workspace.package]\nedition = \"2024\"\nversion = \"0.4.8\" # the release\nlicense = \"MIT\"\n\n[workspace.dependencies]\nversion = \"9\"\n";

    const CHANGELOG: &str = "# Changelog\n\nIntro.\n\n## [Unreleased]\n\nSummary.\n\n- **One.** First.\n- **Two.** Second.\n\n## [0.4.8] - 2026-11-14\n\n- Older.\n";

    #[test]
    fn the_version_line_of_the_package_table_is_set() {
        let edited = set_workspace_version(MANIFEST, "0.4.9").expect("set");
        assert_eq!(
            edited,
            MANIFEST.replace("version = \"0.4.8\"", "version = \"0.4.9\"")
        );
        assert_eq!(version_in(&edited).expect("version"), "0.4.9");
        assert!(set_workspace_version("[workspace]\nversion = \"1.0.0\"\n", "1.0.1").is_err());
        assert!(
            set_workspace_version("[workspace.package]\nversion.workspace = true\n", "1.0.1")
                .is_err()
        );
    }

    #[test]
    fn the_unreleased_entries_become_the_versions_section() {
        let rotated = rotate_changelog(CHANGELOG, "0.4.9", "2026-11-20").expect("rotate");
        assert_eq!(
            rotated,
            "# Changelog\n\nIntro.\n\n## [Unreleased]\n\n## [0.4.9] - 2026-11-20\n\nSummary.\n\n- **One.** First.\n- **Two.** Second.\n\n## [0.4.8] - 2026-11-14\n\n- Older.\n"
        );
        let last = rotate_changelog("## [Unreleased]\n- a", "1.0.0", "2026-01-01").expect("last");
        assert_eq!(last, "## [Unreleased]\n\n## [1.0.0] - 2026-01-01\n\n- a\n");
    }

    #[test]
    fn a_changelog_with_nothing_to_release_is_refused() {
        let empty = "## [Unreleased]\n\n## [0.4.8] - 2026-11-14\n\n- Older.\n";
        assert!(
            rotate_changelog(empty, "0.4.9", "2026-11-20")
                .expect_err("empty")
                .contains("empty")
        );
        assert!(
            rotate_changelog(CHANGELOG, "0.4.8", "2026-11-20")
                .expect_err("again")
                .contains("already has")
        );
        assert!(rotate_changelog("# Changelog\n", "0.4.9", "2026-11-20").is_err());
    }

    #[test]
    fn versions_and_dates_are_checked() {
        assert!(check_version("0.4.9", "0.4.8").is_ok());
        assert!(check_version("0.5.0", "0.4.8").is_ok());
        assert!(check_version("0.4.8", "0.4.8").is_err());
        assert!(check_version("0.4.7", "0.4.8").is_err());
        assert!(check_version("0.4.9-rc.1", "0.4.8").is_err());
        assert!(check_version("0.4.09", "0.4.8").is_err());
        assert!(is_date("2026-11-14"));
        for bad in [
            "2026-13-01",
            "2026-00-10",
            "2026-1-14",
            "2026-11-32",
            "20261114",
            "2026-11-1x",
            "２０２６-11-14",
        ] {
            assert!(!is_date(bad), "{bad}");
        }
    }

    struct Checkout {
        branch: &'static str,
    }

    fn checkout(root: &Path) -> Fake<Checkout> {
        std::fs::write(root.join("Cargo.toml"), MANIFEST).expect("manifest");
        std::fs::write(root.join("CHANGELOG.md"), CHANGELOG).expect("changelog");
        Fake {
            world: Checkout { branch: BRANCH },
            log: Vec::new(),
            answer: |world, command| {
                assert_eq!(command.args, ["rev-parse", "--abbrev-ref", "HEAD"]);
                Ok(Some(format!("{}\n", world.branch)))
            },
            apply: |_, _| Ok(()),
        }
    }

    fn options(root: &Path, version: &str, dry_run: bool) -> Options {
        Options {
            root: root.to_path_buf(),
            version: version.to_owned(),
            date: "2026-11-20".to_owned(),
            dry_run,
        }
    }

    #[test]
    fn prepare_writes_the_release_and_refreshes_both_lockfiles() {
        let scratch = Scratch::new("bake-release-prepare").expect("scratch");
        let root = scratch.path();
        let mut runner = checkout(root);
        prepare(&options(root, "0.4.9", false), &mut runner).expect("prepare");
        let manifest = read_text(&root.join("Cargo.toml")).expect("manifest");
        assert_eq!(version_in(&manifest).expect("version"), "0.4.9");
        let changelog = read_text(&root.join("CHANGELOG.md")).expect("changelog");
        assert!(changelog.contains("## [Unreleased]\n\n## [0.4.9] - 2026-11-20\n\nSummary."));
        assert_eq!(
            runner.effects(),
            [
                "cargo update --workspace --offline",
                "cargo update --workspace --offline"
            ]
        );
        assert!(
            runner
                .log
                .iter()
                .any(|line| line == "$ (cd fuzz && cargo update --workspace --offline)")
        );
        assert!(
            runner
                .log
                .iter()
                .any(|line| line.contains("git commit -am \"release: 0.4.9\""))
        );
    }

    #[test]
    fn a_dry_run_writes_nothing_and_runs_nothing() {
        let scratch = Scratch::new("bake-release-prepare").expect("scratch");
        let root = scratch.path();
        let mut runner = checkout(root);
        prepare(&options(root, "0.4.9", true), &mut runner).expect("dry run");
        assert_eq!(
            read_text(&root.join("Cargo.toml")).expect("manifest"),
            MANIFEST
        );
        assert_eq!(
            read_text(&root.join("CHANGELOG.md")).expect("changelog"),
            CHANGELOG
        );
        assert!(runner.effects().is_empty());
        assert!(
            runner
                .log
                .iter()
                .any(|line| line.contains("0.4.8 -> 0.4.9"))
        );
    }

    #[test]
    fn prepare_refuses_an_old_version_or_another_branch() {
        let scratch = Scratch::new("bake-release-prepare").expect("scratch");
        let root = scratch.path();
        let mut runner = checkout(root);
        assert!(
            prepare(&options(root, "0.4.8", false), &mut runner)
                .expect_err("same")
                .contains("not newer")
        );
        runner.world.branch = "main";
        assert!(
            prepare(&options(root, "0.4.9", false), &mut runner)
                .expect_err("branch")
                .contains("develop")
        );
        assert_eq!(
            read_text(&root.join("Cargo.toml")).expect("manifest"),
            MANIFEST
        );
        assert!(runner.effects().is_empty());
    }

    fn line() -> impl Strategy<Value = String> {
        prop_oneof![
            "[ -~]{0,20}",
            Just("## [Unreleased]".to_owned()),
            Just("## [1.2.3] - 2026-01-01".to_owned()),
            Just("- entry".to_owned()),
            Just(String::new()),
            Just("\r".to_owned()),
        ]
    }

    fn text() -> impl Strategy<Value = String> {
        (prop::collection::vec(line(), 0..12), any::<bool>())
            .prop_map(|(lines, newline)| lines.join(if newline { "\n" } else { "\r\n" }))
    }

    /// The non-blank lines, trimmed: the entries are moved trimmed.
    fn without_blanks(text: &str) -> Vec<&str> {
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Any text either is refused or gains exactly the heading: the
        /// version's section is what `[Unreleased]` held, `[Unreleased]` is
        /// empty, and every other non-blank line keeps its place and text.
        #[test]
        fn rotation_moves_only_the_unreleased_entries(changelog in text()) {
            if let Ok(rotated) = rotate_changelog(&changelog, "9.9.9", "2026-01-01") {
                let before = changelog_section(&changelog, "Unreleased");
                prop_assert!(before.as_deref().is_some_and(|entries| !entries.is_empty()));
                prop_assert_eq!(changelog_section(&rotated, "9.9.9"), before);
                prop_assert_eq!(changelog_section(&rotated, "Unreleased"), Some(String::new()));
                let mut lines = without_blanks(&rotated);
                let heading = lines.iter().position(|line| *line == "## [9.9.9] - 2026-01-01");
                prop_assert!(heading.is_some());
                lines.remove(heading.unwrap_or_default());
                prop_assert_eq!(lines, without_blanks(&changelog));
            }
        }

        /// Any text either is refused or changes only the package version.
        #[test]
        fn the_version_edit_changes_only_the_version(
            before in "[ -~\n]{0,40}",
            after in "[ -~\n]{0,40}",
            indent in "[ \t]{0,2}",
            old in "(0|[1-9][0-9]{0,2})\\.(0|[1-9][0-9]{0,2})\\.(0|[1-9][0-9]{0,2})",
        ) {
            let manifest = format!("{before}\n[workspace.package]\n{indent}version = \"{old}\"\n{after}");
            if let Ok(edited) = set_workspace_version(&manifest, "9.9.9") {
                prop_assert_eq!(version_in(&edited), Ok("9.9.9".to_owned()));
                prop_assert_eq!(edited.len() + old.len(), manifest.len() + 5);
                if let Ok(back) = set_workspace_version(&edited, &old) {
                    prop_assert_eq!(back, manifest);
                }
            }
        }

        /// Arbitrary text never panics the edit.
        #[test]
        fn any_text_is_read_without_a_panic(manifest in "\\PC{0,80}") {
            let _ = set_workspace_version(&manifest, "1.0.0");
            let _ = rotate_changelog(&manifest, "1.0.0", "2026-01-01");
        }
    }
}
