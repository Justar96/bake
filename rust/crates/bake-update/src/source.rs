//! The native release route: where the manifest and the archives are,
//! and how they are fetched.
//!
//! ```text
//! <base>/stable.json       the manifest
//! <base>/stable.json.sig   base64 Ed25519 over its exact bytes
//! <base>/releases/<version>/<file>
//! ```
//!
//! `<base>` is `https://bake.justar.dev/native` ([D45]). Bake 0.3's
//! `latest.json` route stays on its final TypeScript release and never
//! serves a native archive, so nothing here reads it.
//!
//! Only `https` is fetched. A test serves its own releases from a
//! loopback `http` origin, which [`Source::allow_loopback_http`] permits
//! and a release build never sets.
//!
//! [D45]: ../../../docs/roadmap/rust-0.4/scope-00/support.md

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use reqwest::{Client, Url};
use ring::digest::{Context, SHA256};

use crate::error::UpdateError;
use crate::manifest::{
    Artifact, MAX_METADATA_BYTES, Manifest, parse_manifest, release_keys, verify_signature,
};
use crate::update::Effects;

/// The host the owner publishes native releases on.
pub const DEFAULT_BASE_URL: &str = "https://bake.justar.dev/native";

/// How long one manifest or signature request may take.
pub const METADATA_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the archive may go without delivering more bytes.
pub const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_secs(60);

/// How long one connection may take to open.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Where the releases are and which keys sign their manifests.
#[derive(Debug, Clone)]
pub struct Source {
    /// The route's origin and path, without a trailing slash.
    pub base: String,
    /// Trusted public keys, base64 DER SubjectPublicKeyInfo (Ed25519).
    pub keys: Vec<String>,
    /// Allow a loopback `http` origin. Tests set it; the command does not.
    pub allow_loopback_http: bool,
}

impl Default for Source {
    /// The published route and the embedded native release key.
    fn default() -> Self {
        Self {
            base: DEFAULT_BASE_URL.to_owned(),
            keys: release_keys(),
            allow_loopback_http: false,
        }
    }
}

impl Source {
    /// Fetch `stable.json` and its signature, and return the manifest
    /// only when a trusted key signed exactly those bytes.
    ///
    /// # Errors
    ///
    /// [`UpdateError::Network`] when the host cannot be reached or
    /// answers with an error, [`UpdateError::Signature`] when no trusted
    /// key signed the bytes, and [`UpdateError::Manifest`] when the
    /// manifest says something it may not.
    pub async fn fetch_manifest(&self) -> Result<Manifest, UpdateError> {
        let bytes = self.fetch_metadata("stable.json").await?;
        let signature = self.fetch_metadata("stable.json.sig").await?;
        let signature = String::from_utf8(signature).map_err(|_| {
            UpdateError::Signature("the release manifest's signature is not text".to_owned())
        })?;
        if !verify_signature(&bytes, &signature, &self.keys) {
            return Err(UpdateError::Signature(
                "the release manifest is not signed by Bake's release key; refusing it".to_owned(),
            ));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| UpdateError::Manifest("the release manifest is not UTF-8".to_owned()))?;
        parse_manifest(&text)
    }

    /// Stream one artifact to `path`, hashing it as it arrives.
    ///
    /// The body is refused as soon as it passes the size the manifest
    /// states, and again when the whole download is shorter than that or
    /// hashes to anything else, so a truncated, padded, or substituted
    /// archive never reaches the unpacker.
    ///
    /// # Errors
    ///
    /// [`UpdateError::Network`] for the request, [`UpdateError::Artifact`]
    /// for a size or hash that is not the manifest's, and
    /// [`UpdateError::Install`] when the file cannot be written.
    pub async fn download(
        &self,
        version: &str,
        artifact: &Artifact,
        path: &Path,
        effects: &dyn Effects,
    ) -> Result<(), UpdateError> {
        let url = self.url(&format!("releases/{version}/{}", artifact.file))?;
        let client = self.client(DOWNLOAD_READ_TIMEOUT)?;
        let mut response = client.get(url.clone()).send().await.map_err(|error| {
            UpdateError::Network(format!("could not download {url}: {}", reason(&error)))
        })?;
        if !response.status().is_success() {
            return Err(UpdateError::Network(format!(
                "{url} answered {}",
                response.status().as_u16()
            )));
        }
        let mut file = effects.create(path).map_err(|error| {
            UpdateError::Install(format!("could not write {}: {error}", path.display()))
        })?;
        let mut digest = Context::new(&SHA256);
        let mut received: u64 = 0;
        loop {
            let chunk = response.chunk().await.map_err(|error| {
                UpdateError::Network(format!("could not download {url}: {}", reason(&error)))
            })?;
            let Some(chunk) = chunk else { break };
            received = received.saturating_add(chunk.len() as u64);
            if received > artifact.size {
                return Err(UpdateError::Artifact(format!(
                    "{} is larger than the {} bytes the release manifest states",
                    artifact.file, artifact.size
                )));
            }
            digest.update(&chunk);
            file.write_all(&chunk).map_err(|error| {
                UpdateError::Install(format!("could not write {}: {error}", path.display()))
            })?;
        }
        file.flush().map_err(|error| {
            UpdateError::Install(format!("could not write {}: {error}", path.display()))
        })?;
        drop(file);
        if received != artifact.size {
            return Err(UpdateError::Artifact(format!(
                "{} is {received} bytes, not the {} the release manifest states",
                artifact.file, artifact.size
            )));
        }
        let found = hex(digest.finish().as_ref());
        if found != artifact.sha256 {
            return Err(UpdateError::Artifact(format!(
                "{} does not match the SHA-256 the release manifest states",
                artifact.file
            )));
        }
        Ok(())
    }

    async fn fetch_metadata(&self, name: &str) -> Result<Vec<u8>, UpdateError> {
        let url = self.url(name)?;
        let client = self.client(METADATA_TIMEOUT)?;
        let response = client
            .get(url.clone())
            .timeout(METADATA_TIMEOUT)
            .send()
            .await
            .map_err(|error| {
                UpdateError::Network(format!("could not reach {url}: {}", reason(&error)))
            })?;
        if !response.status().is_success() {
            return Err(UpdateError::Network(format!(
                "{url} answered {}",
                response.status().as_u16()
            )));
        }
        let bytes = response.bytes().await.map_err(|error| {
            UpdateError::Network(format!("could not read {url}: {}", reason(&error)))
        })?;
        if bytes.len() > MAX_METADATA_BYTES {
            return Err(UpdateError::Manifest(format!(
                "{url} is larger than a release manifest can be"
            )));
        }
        Ok(bytes.to_vec())
    }

    /// One route path as a URL, refusing a scheme that is not `https`.
    fn url(&self, path: &str) -> Result<Url, UpdateError> {
        let base = self.base.trim_end_matches('/');
        let url = Url::parse(&format!("{base}/{path}"))
            .map_err(|_| UpdateError::Network(format!("{base} is not a release host address")))?;
        if url.scheme() == "https" || (self.allow_loopback_http && is_loopback_http(&url)) {
            Ok(url)
        } else {
            Err(UpdateError::Network(format!(
                "{base} is not an https address; refusing to fetch a release over it"
            )))
        }
    }

    fn client(&self, read_timeout: Duration) -> Result<Client, UpdateError> {
        Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(read_timeout)
            .build()
            .map_err(|error| {
                UpdateError::Network(format!("could not start an HTTPS client: {error}"))
            })
    }
}

/// Whether `url` is clear-text HTTP to this machine, which only a test
/// server is.
fn is_loopback_http(url: &Url) -> bool {
    if url.scheme() != "http" {
        return false;
    }
    match url.host_str() {
        // An IPv6 host is bracketed in a URL.
        Some(host) => {
            let host = host.trim_start_matches('[').trim_end_matches(']');
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        }
        None => false,
    }
}

/// A request failure in one clause, without the chain of causes reqwest
/// prints.
fn reason(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "the request timed out".to_owned()
    } else if error.is_connect() {
        "the host could not be reached".to_owned()
    } else {
        "the request failed".to_owned()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_and_a_permitted_loopback_origin_are_fetched() {
        let source = Source::default();
        assert_eq!(source.base, "https://bake.justar.dev/native");
        assert!(!source.allow_loopback_http);
        assert_eq!(
            source.url("stable.json").map(|url| url.to_string()),
            Ok("https://bake.justar.dev/native/stable.json".to_owned())
        );
        for base in [
            "http://bake.justar.dev/native",
            "http://127.0.0.1:8080",
            "file:///tmp",
        ] {
            let source = Source {
                base: base.to_owned(),
                ..Source::default()
            };
            let error = source.url("stable.json").expect_err(base);
            assert!(error.message().contains("not an https address"), "{error}");
        }
        let loopback = Source {
            base: "http://127.0.0.1:8080".to_owned(),
            allow_loopback_http: true,
            ..Source::default()
        };
        assert!(loopback.url("stable.json").is_ok());
        let elsewhere = Source {
            base: "http://example.com".to_owned(),
            allow_loopback_http: true,
            ..Source::default()
        };
        assert!(elsewhere.url("stable.json").is_err());
    }

    #[test]
    fn a_trailing_slash_does_not_double_the_path() {
        let source = Source {
            base: "https://bake.justar.dev/native/".to_owned(),
            ..Source::default()
        };
        assert_eq!(
            source
                .url("releases/0.4.0/bake-rs-v0.4.0-linux-x64.tar.gz")
                .map(|url| url.to_string()),
            Ok(
                "https://bake.justar.dev/native/releases/0.4.0/bake-rs-v0.4.0-linux-x64.tar.gz"
                    .to_owned()
            )
        );
    }
}
