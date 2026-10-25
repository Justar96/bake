//! Checks that the deployed download service serves a native release.
//!
//! Waits for `native/stable.json` to name the version, then verifies it as
//! a client does, with `bake-update`'s own code: a signature from the
//! committed native key, the manifest's rules, and every archive's size and
//! hash, downloaded through the service, which redirects archives to the
//! version's GitHub release. The frozen 0.3 route must still verify with its
//! own key, the installers served must be the committed ones, and the health
//! route must answer. A deploy that reports success is not a release until
//! this passes.
//!
//! Ported from Bake's `scripts/release/verify-live.ts` (revision
//! 97ce6abba0).

use std::path::Path;
use std::time::{Duration, Instant};

use bake_update::HostEffects;
use bake_update::manifest::{Manifest, RELEASE_TARGETS, verify_signature};
use bake_update::source::Source;

use crate::http::{client, get, published_release, runtime};
use crate::manifest::committed_key;
use crate::util::{Result, Scratch, read_text};

/// How long the new deployment has to serve the version.
pub const WAIT: Duration = Duration::from_secs(10 * 60);

/// Verifies everything `base` serves for `version`.
pub fn verify_live(root: &Path, base: &str, version: &str, complete: bool) -> Result<Manifest> {
    let keys = vec![committed_key(root)?];
    let legacy_key = read_text(&root.join("distribution/host/release-key.pub"))?
        .trim()
        .to_owned();
    let runtime = runtime()?;
    runtime.block_on(async {
        let client = client(Duration::from_secs(60))?;
        let started = Instant::now();
        let manifest = loop {
            let last = match published_release(&client, base, &keys).await {
                Ok(Some(manifest)) if manifest.version == version => break manifest,
                Ok(Some(manifest)) => format!("it serves {}", manifest.version),
                Ok(None) => "it publishes no native release".to_owned(),
                Err(error) => error,
            };
            if started.elapsed() >= WAIT {
                return Err(format!("{base} never served Bake {version}: {last}"));
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        };
        let refused: Vec<&String> = manifest
            .artifacts
            .keys()
            .filter(|target| !RELEASE_TARGETS.contains(&target.as_str()))
            .collect();
        if !refused.is_empty() {
            return Err(format!(
                "The live release names {}, which this release does not publish",
                refused
                    .iter()
                    .map(|target| target.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let missing: Vec<&str> = RELEASE_TARGETS
            .iter()
            .copied()
            .filter(|target| !manifest.artifacts.contains_key(*target))
            .collect();
        if complete && !missing.is_empty() {
            return Err(format!("The live release lacks {}", missing.join(", ")));
        }
        // Each archive as `bake update` downloads it: size and hash checked
        // while it streams, through the service's redirect.
        let source = Source {
            base: format!("{base}/native"),
            keys: keys.clone(),
            allow_loopback_http: base.starts_with("http://127.0.0.1"),
        };
        let scratch = Scratch::new("bake-verify-live")?;
        for (target, artifact) in &manifest.artifacts {
            let path = scratch.path().join(&artifact.file);
            source
                .download(&manifest.version, artifact, &path, &HostEffects)
                .await
                .map_err(|error| format!("{target}: {error}"))?;
        }
        // 0.3 installs keep updating from this route.
        let (status, latest) = get(&client, &format!("{base}/latest.json")).await?;
        let (sig_status, signature) = get(&client, &format!("{base}/latest.json.sig")).await?;
        let signature = String::from_utf8_lossy(&signature).into_owned();
        if status != 200
            || sig_status != 200
            || !verify_signature(&latest, signature.trim(), std::slice::from_ref(&legacy_key))
        {
            return Err("The frozen 0.3 route no longer verifies with its key".to_owned());
        }
        for installer in ["install.sh", "install.ps1"] {
            let (status, served) = get(&client, &format!("{base}/{installer}")).await?;
            let committed = std::fs::read(root.join("distribution/host").join(installer))
                .map_err(|error| format!("{installer}: {error}"))?;
            if status != 200 || served != committed {
                return Err(format!("{base}/{installer} is not the committed installer"));
            }
        }
        let (status, _) = get(&client, &format!("{base}/health")).await?;
        if status != 200 {
            return Err(format!("{base}/health answered {status}"));
        }
        Ok(manifest)
    })
}
