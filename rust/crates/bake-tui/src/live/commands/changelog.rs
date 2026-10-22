//! `/changelog`, adapted from Pi v1.3.0
//! `packages/coding-agent/src/modes/interactive/interactive-mode.ts`
//! (`handleChangelogCommand`) and `src/utils/changelog.ts`
//! (`parseChangelog`, `normalizeChangelogLinks`), with
//! `test/changelog.test.ts`.
//!
//! The changelog is the repository's `CHANGELOG.md`, compiled into the
//! binary as `bake docs` compiles its topics, so it always matches the
//! installed build and needs no files. As in Pi, only `## [x.y.z]` sections
//! are entries (`Unreleased` is not one), shown oldest first under `What's
//! New`, with each entry's relative links pinned to its tag on Bake's
//! repository. Pi's rewrite of its old `pi-mono` repository URLs has no
//! Bake counterpart and is not ported. Bake's transcript shows the Markdown
//! as text, as it shows answers.

use std::sync::LazyLock;

use bake_coding_agent::bug_report::BAKE_REPOSITORY;
use bake_tui_view::copy;
use bake_tui_view::transcript::Row;
use regex::Regex;

/// The repository's changelog as this build ships it.
pub const CHANGELOG: &str = include_str!("../../../../../../CHANGELOG.md");

/// One released section, Pi's `ChangelogEntry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// The section, its heading included, trimmed.
    pub content: String,
}

impl Entry {
    fn tag(&self) -> String {
        format!("v{}.{}.{}", self.major, self.minor, self.patch)
    }
}

static VERSION_HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"##\s+\[?(\d+)\.(\d+)\.(\d+)\]?").expect("a fixed pattern"));
static INLINE_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(!?\[[^\]\n]+\]\()([^\s)]+)((?:\s+[^)]*)?\))").expect("a fixed pattern")
});
static URL_SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[a-z][a-z0-9+.-]*:").expect("a fixed pattern"));

/// Pi's `parseChangelog`: each `## ` line that names a version starts an
/// entry, running to the next `## ` line; other `## ` sections are skipped.
pub fn parse(text: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut current: Option<(u64, u64, u64)> = None;
    let mut lines: Vec<&str> = Vec::new();
    let mut flush = |current: Option<(u64, u64, u64)>, lines: &[&str]| {
        if let Some((major, minor, patch)) = current
            && !lines.is_empty()
        {
            entries.push(Entry {
                major,
                minor,
                patch,
                content: lines.join("\n").trim().to_owned(),
            });
        }
    };
    for line in text.split('\n') {
        if line.starts_with("## ") {
            flush(current, &lines);
            lines.clear();
            current = VERSION_HEADING.captures(line).and_then(|found| {
                Some((
                    found[1].parse().ok()?,
                    found[2].parse().ok()?,
                    found[3].parse().ok()?,
                ))
            });
            if current.is_some() {
                lines.push(line);
            }
        } else if current.is_some() {
            lines.push(line);
        }
    }
    flush(current, &lines);
    entries
}

/// `path.posix.normalize` for a relative path: `.` segments dropped and
/// `..` applied; `..` past the start is kept, as Node keeps it.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let mut joined = parts.join("/");
    if joined.is_empty() {
        joined.push('.');
    } else if path.ends_with('/') {
        joined.push('/');
    }
    joined
}

/// JavaScript's `encodeURI`.
fn encode_uri(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b";,/?:@&=+$-_.!~*'()#".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Pi's `normalizeChangelogLinkTarget` for Bake's repository, whose
/// changelog is at its root.
fn link_target(target: &str, tag: &str) -> String {
    let repo = format!("https://github.com/{BAKE_REPOSITORY}");
    let mut canonical = target.to_owned();
    for route in ["blob", "tree"] {
        for branch in ["main", "master"] {
            let floating = format!("{repo}/{route}/{branch}/");
            if let Some(rest) = canonical.strip_prefix(&floating) {
                canonical = format!("{repo}/{route}/{tag}/{rest}");
            }
        }
    }
    if canonical.starts_with('#') || canonical.starts_with("//") || URL_SCHEME.is_match(&canonical)
    {
        return canonical;
    }
    let (before_hash, fragment) = match canonical.find('#') {
        Some(at) => canonical.split_at(at),
        None => (canonical.as_str(), ""),
    };
    let (path, query) = match before_hash.find('?') {
        Some(at) => before_hash.split_at(at),
        None => (before_hash, ""),
    };
    if path.is_empty() {
        return canonical.clone();
    }
    let path_part = path.replace('\\', "/");
    let repository_path = normalize(path_part.trim_start_matches('/'));
    if repository_path == "." || repository_path == ".." || repository_path.starts_with("../") {
        return canonical.clone();
    }
    let base = repository_path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("");
    let route = if path_part.ends_with('/') || !base.contains('.') {
        "tree"
    } else {
        "blob"
    };
    format!(
        "{repo}/{route}/{tag}/{}{query}{fragment}",
        encode_uri(&repository_path)
    )
}

/// Pi's `normalizeChangelogLinks`: inline Markdown links in `markdown`
/// pinned to `entry`'s tag.
pub fn normalize_links(markdown: &str, entry: &Entry) -> String {
    let tag = entry.tag();
    INLINE_LINK
        .replace_all(markdown, |found: &regex::Captures<'_>| {
            format!("{}{}{}", &found[1], link_target(&found[2], &tag), &found[3])
        })
        .into_owned()
}

/// Pi's `handleChangelogCommand`: the heading, then every entry, oldest
/// first, or Pi's note when there is none.
pub fn rows(text: &str) -> Vec<Row> {
    let entries = parse(text);
    let markdown = if entries.is_empty() {
        copy::CHANGELOG_EMPTY.to_owned()
    } else {
        entries
            .iter()
            .rev()
            .map(|entry| normalize_links(&entry.content, entry))
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    vec![
        Row::Info(copy::CHANGELOG_TITLE.to_owned()),
        Row::Answer(markdown),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(major: u64, minor: u64, patch: u64) -> Entry {
        Entry {
            major,
            minor,
            patch,
            content: String::new(),
        }
    }

    #[test]
    fn only_versioned_sections_are_entries() {
        let text = "# Changelog\n\nIntro.\n\n## [Unreleased]\n\n- soon\n\n## [0.3.8] - 2026-10-05\n\n- eight\n\n## Notes\n\n- skipped\n\n## 0.3.7\n- seven\n";
        let entries = parse(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            (entries[0].major, entries[0].minor, entries[0].patch),
            (0, 3, 8)
        );
        assert_eq!(entries[0].content, "## [0.3.8] - 2026-10-05\n\n- eight");
        assert_eq!(entries[1].content, "## 0.3.7\n- seven");
        assert!(parse("# Changelog\n\n## [Unreleased]\n- x\n").is_empty());
    }

    // changelog.test.ts "rewrites package-relative changelog links to
    // tag-pinned GitHub source links", with Bake's root-level changelog.
    #[test]
    fn relative_links_are_pinned_to_the_entry_s_tag() {
        let markdown = [
            "[Settings](rust/crates/bake-cli/docs/settings.md#members)",
            "[Docs](docs/roadmap/)",
            "[Scripts](scripts)",
            "[Root](/README.md)",
            "[Outside](../elsewhere.md)",
            "![Image](docs/a.png \"title\")",
        ]
        .join("\n");
        let base = format!("https://github.com/{BAKE_REPOSITORY}");
        assert_eq!(
            normalize_links(&markdown, &entry(0, 3, 8)),
            [
                format!(
                    "[Settings]({base}/blob/v0.3.8/rust/crates/bake-cli/docs/settings.md#members)"
                ),
                format!("[Docs]({base}/tree/v0.3.8/docs/roadmap/)"),
                format!("[Scripts]({base}/tree/v0.3.8/scripts)"),
                format!("[Root]({base}/blob/v0.3.8/README.md)"),
                "[Outside](../elsewhere.md)".to_owned(),
                format!("![Image]({base}/blob/v0.3.8/docs/a.png \"title\")"),
            ]
            .join("\n")
        );
    }

    // changelog.test.ts "canonicalizes old repository URLs without changing
    // external links": floating branches are pinned; other links stay.
    #[test]
    fn floating_repository_links_are_pinned_and_others_kept() {
        let base = format!("https://github.com/{BAKE_REPOSITORY}");
        let markdown = [
            format!("[Readme]({base}/blob/main/README.md)"),
            format!("[Issue]({base}/issues/12)"),
            "[External](https://example.com/docs)".to_owned(),
            "[Local anchor](#settings)".to_owned(),
            "[Mail](mailto:a@example.com)".to_owned(),
        ]
        .join("\n");
        assert_eq!(
            normalize_links(&markdown, &entry(0, 3, 7)),
            [
                format!("[Readme]({base}/blob/v0.3.7/README.md)"),
                format!("[Issue]({base}/issues/12)"),
                "[External](https://example.com/docs)".to_owned(),
                "[Local anchor](#settings)".to_owned(),
                "[Mail](mailto:a@example.com)".to_owned(),
            ]
            .join("\n")
        );
    }

    #[test]
    fn the_shipped_changelog_shows_its_entries_oldest_first() {
        let rows = rows(CHANGELOG);
        assert_eq!(rows[0], Row::Info("What's New".into()));
        let Row::Answer(text) = &rows[1] else {
            panic!("an answer row");
        };
        // Whatever versions ship, each released heading appears once, in
        // order from the oldest to the newest.
        let entries = parse(CHANGELOG);
        assert!(!entries.is_empty(), "the changelog has released entries");
        let mut at = 0;
        for entry in entries.iter().rev() {
            let heading = entry.content.lines().next().expect("a heading");
            let found = text[at..].find(heading).expect(heading);
            at += found + heading.len();
        }
        let versions: Vec<(u64, u64, u64)> = entries
            .iter()
            .map(|entry| (entry.major, entry.minor, entry.patch))
            .collect();
        assert!(
            versions.windows(2).all(|pair| pair[0] > pair[1]),
            "the file lists the newest first: {versions:?}"
        );
        assert!(!text.contains("## [Unreleased]"));
        assert_eq!(
            super::rows("# Changelog\n")[1],
            Row::Answer("No changelog entries found.".into())
        );
    }
}
