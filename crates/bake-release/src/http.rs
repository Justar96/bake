//! The HTTPS client the release checks use, and reading the native release
//! a host publishes as a client reads it.

use std::time::Duration;

use bake_update::manifest::{Manifest, parse_manifest, verify_signature};
use reqwest::Client;

use crate::util::Result;

/// The live download service.
pub const DEFAULT_BASE_URL: &str = "https://bake.justar.dev";

/// The host a release checks: `BAKE_RELEASE_BASE_URL`, or the live one,
/// without a trailing slash.
pub fn base_url() -> String {
    std::env::var("BAKE_RELEASE_BASE_URL")
        .ok()
        .map(|base| base.trim().to_owned())
        .filter(|base| !base.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned())
        .trim_end_matches('/')
        .to_owned()
}

/// A single-threaded runtime for one step's requests.
pub fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime: {error}"))
}

/// A rustls client trusting the Mozilla roots, as `bake-update` builds.
pub fn client(timeout: Duration) -> Result<Client> {
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| format!("TLS: {error}"))?
        .with_root_certificates(rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        })
        .with_no_client_auth();
    Client::builder()
        .tls_backend_preconfigured(tls)
        .connect_timeout(Duration::from_secs(15))
        .timeout(timeout)
        .build()
        .map_err(|error| format!("could not start an HTTPS client: {error}"))
}

/// GETs `url`: its status and body.
pub async fn get(client: &Client, url: &str) -> Result<(u16, Vec<u8>)> {
    let response = client
        .get(url)
        .header("cache-control", "no-store")
        .send()
        .await
        .map_err(|error| format!("{url}: {error}"))?;
    let status = response.status().as_u16();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("{url}: {error}"))?;
    Ok((status, bytes.to_vec()))
}

/// The native release `base` publishes, verified with `keys` over the
/// exact bytes served and then by the manifest's own rules; `None` when it
/// publishes none (404).
pub async fn published_release(
    client: &Client,
    base: &str,
    keys: &[String],
) -> Result<Option<Manifest>> {
    let (status, bytes) = get(client, &format!("{base}/native/stable.json")).await?;
    if status == 404 {
        return Ok(None);
    }
    if status != 200 {
        return Err(format!("native/stable.json answered {status}"));
    }
    let (status, signature) = get(client, &format!("{base}/native/stable.json.sig")).await?;
    if status != 200 {
        return Err(format!("native/stable.json.sig answered {status}"));
    }
    let signature =
        String::from_utf8(signature).map_err(|_| "the signature is not text".to_owned())?;
    if !verify_signature(&bytes, &signature, keys) {
        return Err("the native release manifest signature did not verify".to_owned());
    }
    let text = String::from_utf8(bytes).map_err(|_| "the manifest is not UTF-8".to_owned())?;
    parse_manifest(&text)
        .map(Some)
        .map_err(|error| error.to_string())
}
