//! Copies checked native archives into the download service's payload and
//! signs the manifest that lists them.
//!
//! The route is `native/stable.json`, `native/stable.json.sig`, and
//! `native/releases/<version>/` under `distribution/host/public/`. The key
//! is Ed25519 PKCS#8 PEM, read from a file; its public half must be the
//! committed one. An ephemeral key signs a local check instead and leaves
//! its public half beside the archives. Only the release platforms are
//! accepted, and a complete release needs both. The version must be newer
//! than every native release already published, local and live: a channel
//! never moves backward.
//!
//! Ported from Bake's `scripts/release/assemble-rust.ts` (revision
//! 97ce6abba0).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use bake_update::manifest::{
    Artifact, RELEASE_TARGETS, TARGETS, archive_name, parse_manifest, verify_signature,
};
use bake_update::version::compare_versions;

use crate::archive::{assert_native, read_layout};
use crate::manifest::{SigningKey, committed_key, manifest_bytes, published_at};
use crate::util::{Result, read_text, sha256_file};

/// Where an ephemeral key's public half is left, beside the archives.
pub const EPHEMERAL_PUBLIC_KEY: &str = "ephemeral-native-release-key.pub";

/// What [`assemble`] reads and writes.
#[derive(Debug)]
pub struct Options {
    pub root: PathBuf,
    pub version: String,
    /// The archives to publish.
    pub source: PathBuf,
    /// The service payload, `distribution/host/public`.
    pub public: PathBuf,
    /// Require an archive for every release platform.
    pub complete: bool,
    /// The live channel's version, when it was checked.
    pub live: Option<String>,
    pub now: SystemTime,
}

/// What [`assemble`] wrote.
#[derive(Debug)]
pub struct Assembled {
    pub bytes: Vec<u8>,
    pub signature: String,
    pub files: Vec<String>,
}

/// The default archive directory for `version`.
pub fn source_dir(root: &Path, version: &str) -> PathBuf {
    root.join(crate::pack::OUTPUT_DIR).join(version)
}

/// The key for this run: the release key from `key_file`, whose public
/// half must be the committed key, or a throwaway one whose public half is
/// written into `source`.
pub fn signing_key(
    root: &Path,
    ephemeral: bool,
    key_file: Option<&Path>,
    source: &Path,
) -> Result<SigningKey> {
    if ephemeral {
        let key = SigningKey::ephemeral()?;
        std::fs::create_dir_all(source)
            .map_err(|error| format!("{}: {error}", source.display()))?;
        std::fs::write(
            source.join(EPHEMERAL_PUBLIC_KEY),
            format!("{}\n", key.public_key()),
        )
        .map_err(|error| format!("{}: {error}", source.display()))?;
        return Ok(key);
    }
    let path = key_file.ok_or_else(|| {
        "Name the native release signing key with --key-file or BAKE_NATIVE_RELEASE_SIGNING_KEY_FILE, or pass --ephemeral-key for a local check".to_owned()
    })?;
    let key = SigningKey::from_pem(&read_text(path)?)?;
    if key.public_key() != committed_key(root)? {
        return Err(format!(
            "The signing key does not match {}",
            crate::manifest::NATIVE_KEY_FILE
        ));
    }
    Ok(key)
}

/// Stages and signs a native release; nothing is written before every
/// archive and the channel order are checked.
pub fn assemble(options: &Options, key: &SigningKey) -> Result<Assembled> {
    let version = options.version.as_str();
    if !bake_update::version::is_release_version(version) {
        return Err(format!("Not a native release version: {version}"));
    }
    if !options.source.is_dir() {
        return Err(format!(
            "No archives at {}; run `bake-release pack` on each platform first",
            options.source.display()
        ));
    }
    let local = options.public.join("native/stable.json");
    let mut published = Vec::new();
    if local.exists() {
        let manifest = parse_manifest(&read_text(&local)?).map_err(|error| error.to_string())?;
        published.push((local.display().to_string(), manifest.version));
    }
    if let Some(live) = &options.live {
        published.push(("the live channel".to_owned(), live.clone()));
    }
    for (where_, other) in &published {
        if compare_versions(version, other) != std::cmp::Ordering::Greater {
            return Err(format!(
                "{version} is not newer than the native release {other} published at {where_}; a channel never moves backward"
            ));
        }
    }
    let mut names: Vec<String> = std::fs::read_dir(&options.source)
        .map_err(|error| format!("{}: {error}", options.source.display()))?
        .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
        .collect();
    names.sort();
    let mut artifacts = BTreeMap::new();
    let mut staged = Vec::new();
    for name in names {
        if name == EPHEMERAL_PUBLIC_KEY {
            continue;
        }
        let Some(target) = TARGETS
            .iter()
            .find(|target| name == archive_name(version, target))
        else {
            return Err(format!(
                "{name} is not a native archive of {version} for a release platform; expected {} and its siblings",
                archive_name(version, "linux-x64")
            ));
        };
        if !RELEASE_TARGETS.contains(target) {
            return Err(format!(
                "{name} is refused: Bake releases {} only",
                RELEASE_TARGETS.join(" and ")
            ));
        }
        let path = options.source.join(&name);
        assert_native(&path, &read_layout(&path)?)?;
        let (sha256, size) = sha256_file(&path)?;
        if size == 0 {
            return Err(format!("Empty native archive: {name}"));
        }
        artifacts.insert(
            (*target).to_owned(),
            Artifact {
                file: name.clone(),
                sha256,
                size,
            },
        );
        staged.push((path, name));
    }
    if staged.is_empty() {
        return Err(format!(
            "No native archives found in {}",
            options.source.display()
        ));
    }
    if options.complete {
        let missing: Vec<String> = RELEASE_TARGETS
            .iter()
            .filter(|target| !artifacts.contains_key(**target))
            .map(|target| archive_name(version, target))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "A complete release needs {} in {}",
                missing.join(" and "),
                options.source.display()
            ));
        }
    }
    let bytes = manifest_bytes(version, &published_at(options.now), &artifacts);
    // Parsed back as a client reads it, so nothing unreadable is signed.
    let text =
        String::from_utf8(bytes.clone()).map_err(|_| "the manifest is not UTF-8".to_owned())?;
    let parsed = parse_manifest(&text).map_err(|error| error.to_string())?;
    if parsed.artifacts != artifacts {
        return Err("the written manifest does not read back as staged".to_owned());
    }
    let signature = key.sign(&bytes);
    if !verify_signature(&bytes, &signature, &[key.public_key()]) {
        return Err("the manifest signature does not verify".to_owned());
    }
    let destination = options.public.join("native/releases").join(version);
    std::fs::create_dir_all(&destination)
        .map_err(|error| format!("{}: {error}", destination.display()))?;
    for (path, name) in &staged {
        std::fs::copy(path, destination.join(name)).map_err(|error| format!("{name}: {error}"))?;
    }
    std::fs::write(options.public.join("native/stable.json"), &bytes)
        .map_err(|error| format!("native/stable.json: {error}"))?;
    std::fs::write(
        options.public.join("native/stable.json.sig"),
        format!("{signature}\n"),
    )
    .map_err(|error| format!("native/stable.json.sig: {error}"))?;
    Ok(Assembled {
        bytes,
        signature,
        files: staged.into_iter().map(|(_, name)| name).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::{Member, Source, write_tar_gz};
    use crate::util::Scratch;

    fn archive(dir: &Path, version: &str, target: &str) {
        let members = vec![
            Member {
                path: "bin/bake".to_owned(),
                mode: 0o755,
                source: Source::Bytes(b"x".to_vec()),
            },
            Member {
                path: "licenses/inventory.json".to_owned(),
                mode: 0o644,
                source: Source::Bytes(b"{}".to_vec()),
            },
            Member {
                path: "notices/bake-cli/NOTICE".to_owned(),
                mode: 0o644,
                source: Source::Bytes(b"n".to_vec()),
            },
        ];
        write_tar_gz(&members, &dir.join(archive_name(version, target)), 0).expect("archive");
    }

    fn options(scratch: &Scratch, version: &str) -> Options {
        Options {
            root: crate::util::repository_root(),
            version: version.to_owned(),
            source: scratch.path().join("source"),
            public: scratch.path().join("public"),
            complete: true,
            live: Some("0.4.0".to_owned()),
            now: SystemTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn a_complete_release_is_staged_and_signed() {
        let scratch = Scratch::new("bake-release-assemble").expect("scratch");
        let options = options(&scratch, "0.4.2");
        let key = signing_key(&options.root, true, None, &options.source).expect("key");
        for target in RELEASE_TARGETS {
            archive(&options.source, "0.4.2", target);
        }
        let assembled = assemble(&options, &key).expect("assembled");
        assert_eq!(assembled.files.len(), 2);
        let served = std::fs::read(options.public.join("native/stable.json")).expect("manifest");
        let signature = read_text(&options.public.join("native/stable.json.sig")).expect("sig");
        let public = read_text(&options.source.join(EPHEMERAL_PUBLIC_KEY)).expect("key");
        assert!(verify_signature(
            &served,
            signature.trim(),
            &[public.trim().to_owned()]
        ));
        let manifest = parse_manifest(std::str::from_utf8(&served).expect("utf8")).expect("parse");
        for (target, artifact) in &manifest.artifacts {
            let copy = options
                .public
                .join("native/releases/0.4.2")
                .join(&artifact.file);
            assert_eq!(
                sha256_file(&copy).expect("hash"),
                (artifact.sha256.clone(), artifact.size),
                "{target}"
            );
        }
        // The same version again moves nothing backward.
        assert!(
            assemble(&options, &key)
                .expect_err("again")
                .contains("never moves backward")
        );
    }

    #[test]
    fn an_incomplete_older_or_foreign_release_is_refused() {
        let scratch = Scratch::new("bake-release-assemble").expect("scratch");
        let options = options(&scratch, "0.4.2");
        let key = signing_key(&options.root, true, None, &options.source).expect("key");
        archive(&options.source, "0.4.2", "linux-x64");
        assert!(
            assemble(&options, &key)
                .expect_err("incomplete")
                .contains("complete release needs")
        );
        let older = Options {
            live: Some("0.4.2".to_owned()),
            ..self::options(&scratch, "0.4.2")
        };
        assert!(assemble(&older, &key).is_err());
        archive(&options.source, "0.4.2", "darwin-arm64");
        assert!(
            assemble(
                &Options {
                    complete: false,
                    ..self::options(&scratch, "0.4.2")
                },
                &key
            )
            .expect_err("darwin")
            .contains("refused")
        );
        assert!(signing_key(&options.root, false, None, &options.source).is_err());
        assert!(
            !options.public.join("native/stable.json").exists(),
            "nothing was written"
        );
    }
}
