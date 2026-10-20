//! `bake-rs --self-check`: proof that this executable loads on its host and
//! carries what a release needs, for the native packager
//! (`scripts/release/pack-rust.ts`) to check before it archives the binary.
//!
//! The check reads no Bake home, credentials, settings, environment, or
//! network, needs no terminal, writes nothing but its one standard-output
//! line, and starts no async runtime. It does not test a provider route, a
//! sandbox backend, or a model turn.

use ring::digest::{SHA256, digest};
use serde_json::{Map, Value};

use crate::docs::Topic;

/// The record's `kind`, which names the producer.
pub const KIND: &str = "bake-rs-self-check";

/// The record's `schema`. A change to the record's members or their meaning
/// takes a new number, and the packager accepts only the numbers it knows.
pub const SCHEMA: u64 = 1;

/// The Bake release platform of an operating system and architecture, as
/// Rust's `target_os` and `target_arch` name them, or `None` for a host Bake
/// does not release for. The names are Node's `process.platform` and
/// `process.arch`, which the release archives and manifests use.
pub fn platform(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("darwin-arm64"),
        ("macos", "x86_64") => Some("darwin-x64"),
        ("linux", "aarch64") => Some("linux-arm64"),
        ("linux", "x86_64") => Some("linux-x64"),
        ("windows", "x86_64") => Some("win32-x64"),
        _ => None,
    }
}

/// The platform this executable was compiled for.
pub fn host_platform() -> Option<&'static str> {
    platform(std::env::consts::OS, std::env::consts::ARCH)
}

/// The self-check record, one line of JSON ending in a newline:
///
/// ```json
/// {"kind":"bake-rs-self-check","schema":1,"version":"0.4.0-dev","runtime":"rust",
///  "target":"linux-x64","docs":{"cli":"<sha-256 hex>",...}}
/// ```
///
/// `version` is the Cargo package version, `runtime` is always `rust` (no
/// Node or Bun runs), `target` the [`platform`], and `docs` the SHA-256 of
/// every embedded documentation topic's Markdown, by topic name, so a
/// packager can match the binary against the documentation sources it
/// was built from.
///
/// # Errors
///
/// The reason the executable is not fit to release: an unsupported
/// platform, no documentation, or a topic that is unnamed, repeated, empty,
/// or lacks its `# ` title.
pub fn record(version: &str, platform: Option<&str>, topics: &[Topic]) -> Result<String, String> {
    let target = platform.ok_or_else(|| {
        format!(
            "{}-{} is not a Bake release platform",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    if topics.is_empty() {
        return Err("no documentation topics are embedded".to_owned());
    }
    let mut docs = Map::new();
    for topic in topics {
        if topic.name.is_empty() {
            return Err("an embedded documentation topic has no name".to_owned());
        }
        if !topic.text.starts_with("# ") || !topic.text.ends_with('\n') {
            return Err(format!(
                "embedded documentation topic '{}' is not a titled Markdown page",
                topic.name
            ));
        }
        let hash = hex(digest(&SHA256, topic.text.as_bytes()).as_ref());
        if docs
            .insert(topic.name.to_owned(), Value::String(hash))
            .is_some()
        {
            return Err(format!(
                "embedded documentation topic '{}' is listed twice",
                topic.name
            ));
        }
    }
    let mut record = Map::new();
    record.insert("kind".into(), KIND.into());
    record.insert("schema".into(), SCHEMA.into());
    record.insert("version".into(), version.into());
    record.insert("runtime".into(), "rust".into());
    record.insert("target".into(), target.into());
    record.insert("docs".into(), Value::Object(docs));
    Ok(format!("{}\n", Value::Object(record)))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn topic(name: &'static str, text: &'static str) -> Topic {
        Topic {
            name,
            summary: "",
            text,
        }
    }

    #[test]
    fn every_release_platform_has_a_name_and_others_have_none() {
        let named = [
            ("macos", "aarch64", "darwin-arm64"),
            ("macos", "x86_64", "darwin-x64"),
            ("linux", "aarch64", "linux-arm64"),
            ("linux", "x86_64", "linux-x64"),
            ("windows", "x86_64", "win32-x64"),
        ];
        for (os, arch, name) in named {
            assert_eq!(platform(os, arch), Some(name));
        }
        for (os, arch) in [
            ("windows", "aarch64"),
            ("linux", "x86"),
            ("linux", "riscv64"),
            ("freebsd", "x86_64"),
            ("macos", "arm"),
        ] {
            assert_eq!(platform(os, arch), None, "{os}-{arch}");
        }
    }

    #[test]
    fn the_record_names_the_version_target_and_document_hashes() {
        let line = record("1.2.3-dev", Some("linux-x64"), &[topic("cli", "# CLI\n")])
            .expect("a valid record");
        assert!(line.ends_with('\n') && !line.trim_end().contains('\n'));
        let value: Value = serde_json::from_str(&line).expect("JSON");
        assert_eq!(
            value,
            serde_json::json!({
                "kind": "bake-rs-self-check",
                "schema": 1,
                "version": "1.2.3-dev",
                "runtime": "rust",
                "target": "linux-x64",
                // `printf '# CLI\n' | sha256sum`
                "docs": { "cli": "cc41e13199bc586f0dabc9fd44d69a5691e7e2c5909ef49ddb2ef6e7f1b510de" },
            })
        );
    }

    #[test]
    fn an_unsupported_platform_or_broken_documentation_fails() {
        let ok = [topic("cli", "# CLI\n")];
        assert!(
            record("1.0.0", None, &ok)
                .expect_err("unsupported")
                .contains("is not a Bake release platform")
        );
        assert_eq!(
            record("1.0.0", Some("linux-x64"), &[]),
            Err("no documentation topics are embedded".to_owned())
        );
        for (topics, reason) in [
            (vec![topic("", "# A\n")], "has no name"),
            (vec![topic("a", "")], "is not a titled Markdown page"),
            (vec![topic("a", "A\n")], "is not a titled Markdown page"),
            (vec![topic("a", "# A")], "is not a titled Markdown page"),
            (
                vec![topic("a", "# A\n"), topic("a", "# B\n")],
                "is listed twice",
            ),
        ] {
            let error = record("1.0.0", Some("linux-x64"), &topics).expect_err(reason);
            assert!(error.contains(reason), "{error}");
        }
    }

    #[test]
    fn the_updater_names_this_host_the_same_way() {
        // `bake update` matches a manifest's artifacts against this
        // platform's name, and the packager matches the archive against
        // the record's; one disagreement would install another platform's
        // archive or refuse this one.
        assert_eq!(host_platform(), bake_update::host_target());
    }

    #[test]
    fn the_shipped_documentation_passes() {
        let line = record(
            "0.0.0",
            host_platform().or(Some("linux-x64")),
            crate::docs::TOPICS,
        )
        .expect("the embedded topics are valid");
        let value: Value = serde_json::from_str(&line).expect("JSON");
        assert_eq!(
            value["docs"].as_object().map(Map::len),
            Some(crate::docs::TOPICS.len())
        );
    }
}
