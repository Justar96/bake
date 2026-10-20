//! The signed native release manifest: what `native/stable.json` may say,
//! and proof that the release owner said it.
//!
//! The rules are Bake 0.3's (`packages/boot/updater/src/manifest.ts`)
//! tightened for the native route: the schema and channel are named in the
//! document, every member is known, and the manifest refuses anything else.
//! The key is the native one, which no 0.3 updater carries ([D45]).
//!
//! [D45]: ../../../docs/roadmap/rust-0.4/scope-00/support.md

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ring::signature::{ED25519, UnparsedPublicKey};
use serde_json::Value;

use crate::error::UpdateError;
use crate::version::is_release_version;

/// The only `schema` a native manifest may carry. A change to the
/// members or their meaning takes a new number.
pub const SCHEMA: &str = "bake-native-release/1";

/// The only `channel` 0.4.0 publishes.
pub const CHANNEL: &str = "stable";

/// The platforms a native release can carry an archive for, as
/// `bake-rs --self-check` and `scripts/release/pack-rust.ts` name them.
pub const TARGETS: [&str; 5] = [
    "darwin-arm64",
    "darwin-x64",
    "linux-arm64",
    "linux-x64",
    "win32-x64",
];

/// Largest manifest or signature that is read; a real one is a few
/// hundred bytes.
pub const MAX_METADATA_BYTES: usize = 64 * 1024;

/// The largest `size` a manifest may state: JavaScript's safe integer, so
/// the manifest's own writer and reader agree.
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// The native release public key: base64 DER SubjectPublicKeyInfo, the
/// format `distribution/host/release-key.pub` uses, and a different key.
/// The owner holds its private half; nothing in this repository signs.
pub const NATIVE_RELEASE_KEY: &str =
    include_str!("../../../../distribution/host/native-release-key.pub");

/// The keys a release manifest must be signed by. There is one; a second
/// entry would let a new key be trusted before the old one stops signing.
pub fn release_keys() -> Vec<String> {
    vec![NATIVE_RELEASE_KEY.trim().to_owned()]
}

/// The release platform of this executable's host, or `None` for a host
/// Bake publishes no archive for.
pub fn host_target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("darwin-arm64"),
        ("macos", "x86_64") => Some("darwin-x64"),
        ("linux", "aarch64") => Some("linux-arm64"),
        ("linux", "x86_64") => Some("linux-x64"),
        ("windows", "x86_64") => Some("win32-x64"),
        _ => None,
    }
}

/// One platform's archive, as the manifest describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// `bake-rs-v<version>-<target>.tar.gz`, relative to
    /// `native/releases/<version>/`.
    pub file: String,
    /// Lowercase hex SHA-256 of the archive's bytes.
    pub sha256: String,
    /// The archive's size in bytes.
    pub size: u64,
}

/// The channel's current release and the archives it publishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub version: String,
    /// When the release was published, as `YYYY-MM-DDTHH:MM:SSZ`.
    pub published: String,
    /// One artifact per published platform, by target name.
    pub artifacts: BTreeMap<String, Artifact>,
}

impl Manifest {
    /// This host's archive, when the release publishes one.
    pub fn artifact(&self, target: &str) -> Option<&Artifact> {
        self.artifacts.get(target)
    }
}

/// Read manifest text, refusing anything a native manifest may not say.
///
/// # Errors
///
/// [`UpdateError::Manifest`] names the first member that is missing,
/// unknown, or malformed.
pub fn parse_manifest(text: &str) -> Result<Manifest, UpdateError> {
    let value: Value = serde_json::from_str(text)
        .map_err(|_| UpdateError::Manifest("the release manifest is not JSON".to_owned()))?;
    let Value::Object(members) = value else {
        return Err(UpdateError::Manifest(
            "the release manifest is not an object".to_owned(),
        ));
    };
    for name in members.keys() {
        if !matches!(
            name.as_str(),
            "schema" | "channel" | "version" | "published" | "artifacts"
        ) {
            return Err(UpdateError::Manifest(format!(
                "the release manifest has an unknown member '{name}'"
            )));
        }
    }
    let text_member = |name: &str| -> Result<String, UpdateError> {
        match members.get(name) {
            Some(Value::String(value)) => Ok(value.clone()),
            _ => Err(UpdateError::Manifest(format!(
                "the release manifest has no {name}"
            ))),
        }
    };
    let schema = text_member("schema")?;
    if schema != SCHEMA {
        return Err(UpdateError::Manifest(format!(
            "the release manifest is schema '{schema}', not {SCHEMA}"
        )));
    }
    let channel = text_member("channel")?;
    if channel != CHANNEL {
        return Err(UpdateError::Manifest(format!(
            "the release manifest is for the '{channel}' channel, not {CHANNEL}"
        )));
    }
    let version = text_member("version")?;
    if !is_release_version(&version) {
        return Err(UpdateError::Manifest(
            "the release manifest has an invalid version".to_owned(),
        ));
    }
    let published = text_member("published")?;
    if !is_published_instant(&published) {
        return Err(UpdateError::Manifest(
            "the release manifest has an invalid published time".to_owned(),
        ));
    }
    let Some(Value::Object(listed)) = members.get("artifacts") else {
        return Err(UpdateError::Manifest(
            "the release manifest lists no artifacts".to_owned(),
        ));
    };
    if listed.is_empty() {
        return Err(UpdateError::Manifest(
            "the release manifest lists no artifacts".to_owned(),
        ));
    }
    let mut artifacts = BTreeMap::new();
    for (target, artifact) in listed {
        if !TARGETS.contains(&target.as_str()) {
            return Err(UpdateError::Manifest(format!(
                "the release manifest names an unsupported target '{target}'"
            )));
        }
        artifacts.insert(target.clone(), parse_artifact(target, artifact, &version)?);
    }
    Ok(Manifest {
        version,
        published,
        artifacts,
    })
}

fn parse_artifact(target: &str, value: &Value, version: &str) -> Result<Artifact, UpdateError> {
    let invalid = || {
        UpdateError::Manifest(format!(
            "the release manifest's {target} artifact is invalid"
        ))
    };
    let Value::Object(members) = value else {
        return Err(invalid());
    };
    for name in members.keys() {
        if !matches!(name.as_str(), "file" | "sha256" | "size") {
            return Err(UpdateError::Manifest(format!(
                "the release manifest's {target} artifact has an unknown member '{name}'"
            )));
        }
    }
    let Some(Value::String(file)) = members.get("file") else {
        return Err(invalid());
    };
    if *file != archive_name(version, target) {
        return Err(UpdateError::Manifest(format!(
            "the release manifest's {target} artifact is not named {}",
            archive_name(version, target)
        )));
    }
    let Some(Value::String(sha256)) = members.get("sha256") else {
        return Err(invalid());
    };
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid());
    }
    let size = members.get("size").and_then(Value::as_u64).unwrap_or(0);
    if size == 0 || size > MAX_SAFE_INTEGER {
        return Err(invalid());
    }
    Ok(Artifact {
        file: file.clone(),
        sha256: sha256.clone(),
        size,
    })
}

/// The archive `scripts/release/pack-rust.ts` writes for a release.
pub fn archive_name(version: &str, target: &str) -> String {
    format!("bake-rs-v{version}-{target}.tar.gz")
}

/// Whether `value` is an RFC 3339 instant in UTC, as the manifest writes
/// it: `YYYY-MM-DDTHH:MM:SSZ`, with optional fractional seconds.
fn is_published_instant(value: &str) -> bool {
    let Some(body) = value.strip_suffix('Z') else {
        return false;
    };
    let (stamp, fraction) = match body.split_once('.') {
        None => (body, None),
        Some((stamp, fraction)) => (stamp, Some(fraction)),
    };
    if fraction.is_some_and(|fraction| {
        fraction.is_empty() || fraction.len() > 9 || !fraction.bytes().all(|b| b.is_ascii_digit())
    }) {
        return false;
    }
    let Some((date, time)) = stamp.split_once('T') else {
        return false;
    };
    let parts: Vec<&str> = date.split('-').collect();
    let times: Vec<&str> = time.split(':').collect();
    if parts.len() != 3 || times.len() != 3 {
        return false;
    }
    let number = |text: &str, width: usize| -> Option<u32> {
        if text.len() != width || !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        text.parse().ok()
    };
    let (Some(_year), Some(month), Some(day)) = (
        number(parts[0], 4),
        number(parts[1], 2),
        number(parts[2], 2),
    ) else {
        return false;
    };
    let (Some(hour), Some(minute), Some(second)) = (
        number(times[0], 2),
        number(times[1], 2),
        number(times[2], 2),
    ) else {
        return false;
    };
    (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && hour < 24
        && minute < 60
        // A leap second is a real instant a publisher may stamp.
        && second < 61
}

/// Whether one of `keys` signed `bytes`.
///
/// The signature is Ed25519 over the manifest's exact bytes, so a manifest
/// that was reformatted, however harmlessly, no longer verifies.
///
/// `signature` is base64, surrounding whitespace ignored; each key is
/// base64 DER SubjectPublicKeyInfo for Ed25519, which is a fixed 44-byte
/// document: the algorithm header and the 32 raw key bytes `ring` takes.
pub fn verify_signature(bytes: &[u8], signature: &str, keys: &[String]) -> bool {
    let Ok(decoded) = BASE64.decode(signature.trim()) else {
        return false;
    };
    if decoded.len() != 64 {
        return false;
    }
    keys.iter().any(|key| match ed25519_key(key) {
        Some(key) => UnparsedPublicKey::new(&ED25519, key)
            .verify(bytes, &decoded)
            .is_ok(),
        None => false,
    })
}

/// DER SubjectPublicKeyInfo for Ed25519: `SEQUENCE { SEQUENCE { OID
/// 1.3.101.112 }, BIT STRING (0 unused bits) }`, then the raw key.
const SPKI_ED25519_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// The 32 raw public-key bytes of a base64 DER SPKI Ed25519 key.
fn ed25519_key(key: &str) -> Option<Vec<u8>> {
    let decoded = BASE64.decode(key.trim()).ok()?;
    let (prefix, raw) = decoded.split_at_checked(SPKI_ED25519_PREFIX.len())?;
    if prefix != SPKI_ED25519_PREFIX || raw.len() != 32 {
        return None;
    }
    Some(raw.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manifest that parses, so each test can change one member.
    fn manifest_text(body: &str) -> String {
        body.to_owned()
    }

    const VALID: &str = r#"{"schema":"bake-native-release/1","channel":"stable","version":"0.4.0",
      "published":"2026-10-20T00:00:00Z","artifacts":{"linux-x64":{"file":"bake-rs-v0.4.0-linux-x64.tar.gz",
      "sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","size":123}}}"#;

    #[test]
    fn a_valid_manifest_names_its_version_and_artifacts() {
        let manifest = parse_manifest(VALID).expect("a valid manifest");
        assert_eq!(manifest.version, "0.4.0");
        assert_eq!(manifest.published, "2026-10-20T00:00:00Z");
        assert_eq!(
            manifest.artifact("linux-x64"),
            Some(&Artifact {
                file: "bake-rs-v0.4.0-linux-x64.tar.gz".to_owned(),
                sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                    .to_owned(),
                size: 123,
            })
        );
        assert_eq!(manifest.artifact("win32-x64"), None);
    }

    #[test]
    fn every_malformed_manifest_is_refused_by_name() {
        let cases: [(&str, &str); 12] = [
            ("not json", "is not JSON"),
            ("[]", "is not an object"),
            (
                &manifest_text(&VALID.replace("\"channel\"", "\"chanel\"")),
                "unknown member 'chanel'",
            ),
            (
                &manifest_text(&VALID.replace("bake-native-release/1", "bake-native-release/2")),
                "is schema 'bake-native-release/2'",
            ),
            (
                &manifest_text(&VALID.replace("\"stable\"", "\"beta\"")),
                "'beta' channel",
            ),
            (
                &manifest_text(&VALID.replace("\"0.4.0\"", "\"0.4\"")),
                "invalid version",
            ),
            (
                &manifest_text(&VALID.replace("2026-10-20T00:00:00Z", "yesterday")),
                "invalid published time",
            ),
            (
                &manifest_text(&VALID.replace("\"linux-x64\"", "\"linux-mips\"")),
                "unsupported target 'linux-mips'",
            ),
            (
                &manifest_text(&VALID.replace("\"size\":123", "\"size\":0")),
                "artifact is invalid",
            ),
            (
                &manifest_text(&VALID.replace("\"size\":123", "\"size\":1.5")),
                "artifact is invalid",
            ),
            (
                &manifest_text(&VALID.replace("bake-rs-v0.4.0-linux-x64", "bake-v0.4.0-linux-x64")),
                "is not named bake-rs-v0.4.0-linux-x64.tar.gz",
            ),
            (
                &manifest_text(&VALID.replace("\"size\":123", "\"size\":123,\"mode\":493")),
                "artifact has an unknown member 'mode'",
            ),
        ];
        for (text, reason) in cases {
            let error = parse_manifest(text).expect_err(reason);
            assert!(error.message().contains(reason), "{error} for {reason}");
        }
        // An empty artifact list publishes nothing, and an uppercase hash is
        // not the lowercase hex the contract fixes.
        assert!(parse_manifest(&VALID.replace(
            "{\"linux-x64\":{\"file\":\"bake-rs-v0.4.0-linux-x64.tar.gz\",\n      \"sha256\":\"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\",\"size\":123}}",
            "{}"
        ))
        .is_err());
        assert!(parse_manifest(&VALID.replace("abcdef0123", "ABCDEF0123")).is_err());
    }

    #[test]
    fn published_instants_are_utc_and_bounded() {
        for value in [
            "2026-10-20T00:00:00Z",
            "2026-01-02T23:59:60Z",
            "2026-10-20T12:30:05.123Z",
        ] {
            assert!(is_published_instant(value), "{value}");
        }
        for value in [
            "2026-10-20T00:00:00",
            "2026-10-20T00:00:00+02:00",
            "2026-13-20T00:00:00Z",
            "2026-10-32T00:00:00Z",
            "2026-10-20T24:00:00Z",
            "2026-10-20T00:60:00Z",
            "2026-10-20 00:00:00Z",
            "26-10-20T00:00:00Z",
            "2026-10-20T00:00:00.Z",
        ] {
            assert!(!is_published_instant(value), "{value}");
        }
    }

    #[test]
    fn the_embedded_key_is_an_ed25519_public_key() {
        let keys = release_keys();
        assert_eq!(keys.len(), 1);
        assert_eq!(ed25519_key(&keys[0]).map(|key| key.len()), Some(32));
        // The 0.3 release key signs 0.3 manifests only; it is a different key.
        assert_ne!(
            keys[0],
            "MCowBQYDK2VwAyEAG1Z33Mis04XnFsekLrnPDBwSgn26uNeLz3XtGal+xC4="
        );
    }

    #[test]
    fn a_malformed_key_or_signature_verifies_nothing() {
        let keys = release_keys();
        assert!(!verify_signature(b"bytes", "not base64!", &keys));
        assert!(!verify_signature(
            b"bytes",
            &BASE64.encode([0u8; 64]),
            &keys
        ));
        assert!(!verify_signature(
            b"bytes",
            &BASE64.encode([0u8; 63]),
            &keys
        ));
        for key in ["", "zzz", &BASE64.encode([0u8; 44])] {
            assert!(ed25519_key(key).is_none(), "{key}");
            assert!(!verify_signature(
                b"bytes",
                &BASE64.encode([0u8; 64]),
                &[key.to_owned()]
            ));
        }
    }
}
