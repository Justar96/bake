//! The signed native release manifest a release writes: its exact bytes,
//! its `published` time, and the Ed25519 key that signs it.
//!
//! The reading side is `bake_update::manifest`, which every client runs;
//! the bytes written here are parsed back by it before they are signed, and
//! the signature is checked by its `verify_signature`.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use bake_update::manifest::{Artifact, CHANNEL, SCHEMA, TARGETS};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ring::signature::{Ed25519KeyPair, KeyPair as _};

use crate::util::{Result, read_text};

/// The committed native public key, which every client embeds.
pub const NATIVE_KEY_FILE: &str = "crates/bake-update/native-release-key.pub";

/// The download service's copy of the native key.
pub const SERVICE_KEY_FILE: &str = "distribution/host/native-release-key.pub";

/// The DER prefix of an Ed25519 SubjectPublicKeyInfo, before its 32 bytes.
const SPKI_ED25519_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// The committed native key, base64 DER SubjectPublicKeyInfo.
pub fn committed_key(root: &Path) -> Result<String> {
    Ok(read_text(&root.join(NATIVE_KEY_FILE))?.trim().to_owned())
}

/// The manifest's canonical bytes: its members in schema order, its
/// artifacts in platform order, and a trailing newline. The signature
/// covers exactly these bytes.
pub fn manifest_bytes(
    version: &str,
    published: &str,
    artifacts: &BTreeMap<String, Artifact>,
) -> Vec<u8> {
    let string = |value: &str| serde_json::Value::String(value.to_owned()).to_string();
    let mut listed = Vec::new();
    for target in TARGETS {
        if let Some(artifact) = artifacts.get(target) {
            listed.push(format!(
                "{}:{{\"file\":{},\"sha256\":{},\"size\":{}}}",
                string(target),
                string(&artifact.file),
                string(&artifact.sha256),
                artifact.size
            ));
        }
    }
    format!(
        "{{\"schema\":{},\"channel\":{},\"version\":{},\"published\":{},\"artifacts\":{{{}}}}}\n",
        string(SCHEMA),
        string(CHANNEL),
        string(version),
        string(published),
        listed.join(",")
    )
    .into_bytes()
}

/// `at` as a manifest's `published` time: UTC, `YYYY-MM-DDTHH:MM:SSZ`.
pub fn published_at(at: SystemTime) -> String {
    let seconds = at
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let rest = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The proleptic Gregorian date `days` after 1970-01-01 (Howard Hinnant's
/// `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The key that signs a manifest.
pub struct SigningKey {
    pair: Ed25519KeyPair,
}

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigningKey")
            .field("public", &self.public_key())
            .finish_non_exhaustive()
    }
}

impl SigningKey {
    /// Reads an Ed25519 PKCS#8 PEM private key, as `openssl genpkey
    /// -algorithm ed25519` writes it.
    ///
    /// # Errors
    ///
    /// When the text is not one PEM `PRIVATE KEY` block holding an Ed25519
    /// key.
    pub fn from_pem(text: &str) -> Result<Self> {
        let mut body = String::new();
        let mut inside = false;
        let mut blocks = 0;
        for line in text.lines().map(str::trim) {
            match line {
                "-----BEGIN PRIVATE KEY-----" => {
                    inside = true;
                    blocks += 1;
                }
                "-----END PRIVATE KEY-----" => inside = false,
                line if inside => body.push_str(line),
                _ => {}
            }
        }
        if blocks != 1 || inside {
            return Err(
                "the native release signing key is not one PEM PRIVATE KEY block".to_owned(),
            );
        }
        let der = BASE64
            .decode(body)
            .map_err(|_| "the native release signing key's PEM is not base64".to_owned())?;
        let pair = Ed25519KeyPair::from_pkcs8_maybe_unchecked(&der).map_err(|_| {
            "the native release signing key is not an Ed25519 PKCS#8 key".to_owned()
        })?;
        Ok(Self { pair })
    }

    /// A throwaway key, for a check that must not touch the release key.
    pub fn ephemeral() -> Result<Self> {
        let rng = ring::rand::SystemRandom::new();
        let document = Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|_| "could not generate an Ed25519 key".to_owned())?;
        let pair = Ed25519KeyPair::from_pkcs8(document.as_ref())
            .map_err(|_| "could not read the generated Ed25519 key".to_owned())?;
        Ok(Self { pair })
    }

    /// The public half, as the installers and clients embed it: base64 DER
    /// SubjectPublicKeyInfo.
    pub fn public_key(&self) -> String {
        let mut spki = SPKI_ED25519_PREFIX.to_vec();
        spki.extend_from_slice(self.pair.public_key().as_ref());
        BASE64.encode(spki)
    }

    /// The base64 Ed25519 signature over `bytes`.
    pub fn sign(&self, bytes: &[u8]) -> String {
        BASE64.encode(self.pair.sign(bytes).as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn artifact(version: &str, target: &str, size: u64) -> Artifact {
        Artifact {
            file: bake_update::manifest::archive_name(version, target),
            sha256: "a".repeat(64),
            size,
        }
    }

    #[test]
    fn the_written_manifest_is_one_a_client_reads() {
        let mut artifacts = BTreeMap::new();
        artifacts.insert("linux-x64".to_owned(), artifact("0.4.2", "linux-x64", 9));
        artifacts.insert(
            "linux-arm64".to_owned(),
            artifact("0.4.2", "linux-arm64", 8),
        );
        let bytes = manifest_bytes("0.4.2", "2026-10-25T12:00:00Z", &artifacts);
        let text = String::from_utf8(bytes.clone()).expect("utf8");
        // Platform order, as every earlier release was written.
        assert_eq!(
            text,
            format!(
                "{{\"schema\":\"bake-native-release/1\",\"channel\":\"stable\",\"version\":\"0.4.2\",\"published\":\"2026-10-25T12:00:00Z\",\"artifacts\":{{\"linux-arm64\":{{\"file\":\"bake-v0.4.2-linux-arm64.tar.gz\",\"sha256\":\"{a}\",\"size\":8}},\"linux-x64\":{{\"file\":\"bake-v0.4.2-linux-x64.tar.gz\",\"sha256\":\"{a}\",\"size\":9}}}}}}\n",
                a = "a".repeat(64)
            )
        );
        let parsed = bake_update::manifest::parse_manifest(&text).expect("parsed");
        assert_eq!(parsed.version, "0.4.2");
        assert_eq!(parsed.artifacts, artifacts);
        let key = SigningKey::ephemeral().expect("key");
        let signature = key.sign(&bytes);
        assert!(bake_update::manifest::verify_signature(
            &bytes,
            &signature,
            &[key.public_key()]
        ));
        let other = SigningKey::ephemeral().expect("key");
        assert!(!bake_update::manifest::verify_signature(
            &bytes,
            &signature,
            &[other.public_key()]
        ));
    }

    #[test]
    fn published_times_are_utc_to_the_second() {
        assert_eq!(published_at(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        let at = UNIX_EPOCH + Duration::from_secs(1_792_929_636);
        assert_eq!(published_at(at), "2026-10-25T12:00:36Z");
        // A leap day and the turn of a century.
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400);
        assert_eq!(published_at(leap), "2000-02-29T00:00:00Z");
        for seconds in (0..4_102_444_800u64).step_by(7_919_999) {
            let text = published_at(UNIX_EPOCH + Duration::from_secs(seconds));
            assert!(
                bake_update::manifest::parse_manifest(&format!(
                    "{{\"schema\":\"bake-native-release/1\",\"channel\":\"stable\",\"version\":\"1.0.0\",\"published\":\"{text}\",\"artifacts\":{{\"linux-x64\":{{\"file\":\"bake-v1.0.0-linux-x64.tar.gz\",\"sha256\":\"{}\",\"size\":1}}}}}}",
                    "a".repeat(64)
                ))
                .is_ok(),
                "{text}"
            );
        }
    }

    #[test]
    fn a_pem_key_signs_as_its_public_half_says() {
        // Generated with `openssl genpkey -algorithm ed25519` for this test.
        let pem = "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIHj97U39+RtmKt8xIm1T+affUdsgkXefO0eP1lRPtb34\n-----END PRIVATE KEY-----\n";
        let key = SigningKey::from_pem(pem).expect("key");
        let signature = key.sign(b"bytes");
        assert!(bake_update::manifest::verify_signature(
            b"bytes",
            &signature,
            &[key.public_key()]
        ));
        assert!(SigningKey::from_pem("not a key").is_err());
        assert!(SigningKey::from_pem(&format!("{pem}{pem}")).is_err());
    }

    #[test]
    fn every_copy_of_the_native_key_is_the_same() {
        let root = crate::util::repository_root();
        let key = committed_key(&root).expect("key");
        assert_eq!(key, bake_update::manifest::NATIVE_RELEASE_KEY.trim());
        let service = read_text(&root.join(SERVICE_KEY_FILE)).expect("service key");
        assert_eq!(service.trim(), key);
        let sh = read_text(&root.join("distribution/host/install.sh")).expect("install.sh");
        assert!(
            sh.lines()
                .any(|line| line == format!("native_release_key='{key}'")),
            "install.sh embeds another key"
        );
        let ps1 = read_text(&root.join("distribution/host/install.ps1")).expect("install.ps1");
        assert!(
            ps1.lines()
                .any(|line| line == format!("$nativeReleaseKey = '{key}'")),
            "install.ps1 embeds another key"
        );
    }
}
