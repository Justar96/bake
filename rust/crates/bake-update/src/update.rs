//! One update run: check the signed manifest, install a newer release
//! beside the running one, and move the pointer.
//!
//! Every step before the pointer move leaves the working install exactly
//! as it was:
//!
//! 1. Take `install.lock`, and clear what an interrupted run left.
//! 2. Stream the archive into `releases/.staging-<uuid>/`, checking its
//!    size and hash as it arrives.
//! 3. Unpack it there, refusing an unsafe or oversized archive.
//! 4. Run the unpacked release's own self-check ([`crate::check`]).
//! 5. Rename it to `releases/<version>`, on the same filesystem.
//! 6. Point `previous` at the release that is current, then `current` at
//!    the new one, each an atomic rename.
//! 7. Prune: keep current and previous, and nothing else.
//!
//! An interruption before step 6 leaves `current` naming the old release
//! and the next run clears the staging directory; an interruption between
//! the two pointer moves leaves both naming the old release, which the
//! next update replaces and a rollback reports as nothing to return to.
//!
//! Bake 0.3's `packages/boot/updater/src/{install,rollback}.ts` is the
//! reference for the order of these steps.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use ring::rand::{SecureRandom as _, SystemRandom};

use crate::archive;
use crate::check::{CandidateCheck, SelfCheck};
use crate::error::UpdateError;
use crate::layout::{Name, Root};
use crate::lock::{LockKind, acquire};
use crate::manifest::{Manifest, host_target, release_target};
use crate::source::Source;
use crate::version::compare_versions;

/// The effects an update makes that a test replaces to reach a failure
/// the filesystem will not produce on demand.
pub trait Effects: Send + Sync {
    /// Create the file the archive streams into.
    ///
    /// # Errors
    ///
    /// Whatever the filesystem reports; a full disk is `StorageFull`.
    fn create(&self, path: &Path) -> std::io::Result<Box<dyn Write + Send>> {
        Ok(Box::new(std::fs::File::create(path)?))
    }

    /// Remove a release directory while pruning.
    ///
    /// # Errors
    ///
    /// Whatever the filesystem reports; Windows refuses to remove a file
    /// a running process holds open.
    fn remove_release(&self, path: &Path) -> std::io::Result<()> {
        std::fs::remove_dir_all(path)
    }
}

/// The effects of a real update.
#[derive(Debug, Clone, Copy, Default)]
pub struct HostEffects;

impl Effects for HostEffects {}

/// What one run found, or did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The installed release is the published one.
    UpToDate { version: String },
    /// A check-only run found a newer release.
    Available { installed: String, version: String },
    /// The release is installed and current.
    Updated {
        from: Option<String>,
        to: String,
        /// Releases retention removed.
        pruned: Vec<String>,
    },
    /// `current` names the earlier release again.
    RolledBack { from: String, to: String },
    /// No earlier release is installed to return to.
    NoPrevious { current: String },
}

/// One native install and the route it updates from.
pub struct Updater {
    /// The install root.
    pub root: Root,
    /// Where the signed releases are.
    pub source: Source,
    /// The release platform this host runs.
    pub target: String,
    /// The version of the executable running this update, used when the
    /// install has no pointer yet.
    pub running: String,
    pub effects: Arc<dyn Effects>,
    pub check: Arc<dyn CandidateCheck>,
}

impl Updater {
    /// An updater for `root` with the shipped effects and self-check.
    ///
    /// # Errors
    ///
    /// [`UpdateError::Platform`] on a platform Bake publishes no native
    /// update for, which in 0.4.0 is every platform but Linux.
    pub fn new(root: Root, source: Source, running: &str) -> Result<Self, UpdateError> {
        let target = release_target(host_target())?;
        Ok(Self {
            root,
            source,
            target: target.to_owned(),
            running: running.to_owned(),
            effects: Arc::new(HostEffects),
            check: Arc::new(SelfCheck::default()),
        })
    }

    /// The release the next command runs: the pointer's, or this
    /// executable's when the install has no pointer.
    pub fn installed(&self) -> String {
        self.root.current().unwrap_or_else(|| self.running.clone())
    }

    /// Report whether a newer release is published, installing nothing.
    ///
    /// This writes nothing under the root and takes no install lock, as
    /// the lock's owner kinds say: it is held by an install, an update, a
    /// rollback, and a prune. So `--check` still answers while one of
    /// those runs.
    ///
    /// # Errors
    ///
    /// The manifest's own failures; an older published version is
    /// refused here too, so a check cannot report a downgrade as news.
    pub async fn check_only(&self) -> Result<Outcome, UpdateError> {
        let manifest = self.source.fetch_manifest().await?;
        let installed = self.installed();
        match compare_versions(&manifest.version, &installed) {
            std::cmp::Ordering::Greater => Ok(Outcome::Available {
                installed,
                version: manifest.version,
            }),
            std::cmp::Ordering::Equal => Ok(Outcome::UpToDate { version: installed }),
            std::cmp::Ordering::Less => Err(older_manifest(&manifest.version, &installed)),
        }
    }

    /// Install the published release and make it current.
    ///
    /// Without `force`, only a strictly newer version is installed, and a
    /// manifest naming an older one is refused as a rollback attack.
    /// `force` must name the published version, and then installs it
    /// whether or not it is newer.
    ///
    /// # Errors
    ///
    /// Every failure leaves `current` where it was; the error says which
    /// step refused.
    pub async fn update(&self, force: Option<&str>) -> Result<Outcome, UpdateError> {
        let manifest = self.source.fetch_manifest().await?;
        let artifact = manifest.artifact(&self.target).cloned().ok_or_else(|| {
            UpdateError::Artifact(format!(
                "Bake {} publishes no archive for {}",
                manifest.version, self.target
            ))
        })?;
        if let Some(forced) = force
            && forced != manifest.version
        {
            return Err(UpdateError::Version(format!(
                "the stable channel publishes Bake {}, not {forced}",
                manifest.version
            )));
        }
        let _lock = acquire(self.root.path(), LockKind::Update)?;
        // Read the pointer under the lock: another run may have moved it
        // since the manifest was fetched.
        let installed = self.installed();
        if force.is_none() {
            match compare_versions(&manifest.version, &installed) {
                std::cmp::Ordering::Greater => {}
                std::cmp::Ordering::Equal => {
                    return Ok(Outcome::UpToDate { version: installed });
                }
                std::cmp::Ordering::Less => {
                    return Err(older_manifest(&manifest.version, &installed));
                }
            }
        }
        self.root.sweep();
        let staging = self.root.releases().join(format!(".staging-{}", unique()));
        let result = self.stage(&manifest, &artifact, &staging).await;
        let _ = std::fs::remove_dir_all(&staging);
        result?;
        let from = self.root.current();
        if let Some(from) = &from {
            self.root.point_at(Name::Previous, from)?;
        }
        self.root.point_at(Name::Current, &manifest.version)?;
        let pruned = self.prune();
        Ok(Outcome::Updated {
            from,
            to: manifest.version,
            pruned,
        })
    }

    /// Download, unpack, and self-check the release into
    /// `releases/<version>`; nothing outside `staging` is written until
    /// the check passes.
    async fn stage(
        &self,
        manifest: &Manifest,
        artifact: &crate::manifest::Artifact,
        staging: &Path,
    ) -> Result<(), UpdateError> {
        let destination = self.root.release(&manifest.version);
        if destination.exists() {
            // A release that is already unpacked is installed again only
            // when it cannot run; an interrupted earlier run can leave a
            // partial directory.
            if self
                .check
                .check(&destination, &manifest.version, &self.target)
                .is_ok()
            {
                return Ok(());
            }
            std::fs::create_dir_all(staging).map_err(create_failure)?;
            let damaged = staging.join("damaged");
            std::fs::rename(&destination, &damaged).map_err(|error| {
                UpdateError::Install(format!(
                    "could not set aside {}: {error}",
                    destination.display()
                ))
            })?;
        }
        std::fs::create_dir_all(staging).map_err(create_failure)?;
        let download = staging.join(&artifact.file);
        self.source
            .download(
                &manifest.version,
                artifact,
                &download,
                self.effects.as_ref(),
            )
            .await?;
        let unpacked = staging.join("unpacked");
        archive::extract(&download, &unpacked)?;
        if let Err(problem) = self.check.check(&unpacked, &manifest.version, &self.target) {
            return Err(UpdateError::SelfCheck(format!(
                "the downloaded Bake {} did not run, so the install is unchanged: {problem}",
                manifest.version
            )));
        }
        std::fs::create_dir_all(self.root.releases()).map_err(create_failure)?;
        std::fs::rename(&unpacked, &destination).map_err(|error| {
            UpdateError::Install(format!(
                "could not move the new release into {}: {error}",
                destination.display()
            ))
        })
    }

    /// Make `previous` current again, and the release it replaces the
    /// one a further rollback returns to.
    ///
    /// # Errors
    ///
    /// [`UpdateError::SelfCheck`] when the earlier release no longer
    /// runs, and the lock's and pointer's own failures. `current` is
    /// unchanged in every case.
    pub fn rollback(&self) -> Result<Outcome, UpdateError> {
        let _lock = acquire(self.root.path(), LockKind::Rollback)?;
        let Some(current) = self.root.current() else {
            return Err(UpdateError::NotInstalled(format!(
                "{} names no current release; install Bake again",
                self.root.path().display()
            )));
        };
        self.root.sweep();
        let Some(previous) = self.root.previous() else {
            return Ok(Outcome::NoPrevious { current });
        };
        let release = self.root.release(&previous);
        if let Err(problem) = self.check.check(&release, &previous, &self.target) {
            return Err(UpdateError::SelfCheck(format!(
                "the earlier Bake {previous} did not run, so the install is unchanged: {problem}"
            )));
        }
        self.root.point_at(Name::Previous, &current)?;
        self.root.point_at(Name::Current, &previous)?;
        Ok(Outcome::RolledBack {
            from: current,
            to: previous,
        })
    }

    /// Remove every release that is neither current nor previous.
    ///
    /// A release whose files are held, as a running Windows executable's
    /// are, stays for a later run instead of failing the update.
    fn prune(&self) -> Vec<String> {
        let keep: Vec<String> = [self.root.current(), self.root.version(Name::Previous)]
            .into_iter()
            .flatten()
            .collect();
        let mut pruned = Vec::new();
        for version in self.root.installed() {
            if keep.contains(&version) {
                continue;
            }
            if self
                .effects
                .remove_release(&self.root.release(&version))
                .is_ok()
            {
                pruned.push(version);
            }
        }
        pruned.sort();
        pruned
    }
}

fn older_manifest(published: &str, installed: &str) -> UpdateError {
    UpdateError::Version(format!(
        "the release manifest publishes Bake {published}, older than the installed {installed}; refusing it"
    ))
}

fn create_failure(error: std::io::Error) -> UpdateError {
    UpdateError::Install(format!("could not create a staging directory: {error}"))
}

/// A name no other run uses: 16 random bytes in the shape of a UUID.
pub(crate) fn unique() -> String {
    let mut bytes = [0u8; 16];
    if SystemRandom::new().fill(&mut bytes).is_err() {
        // The random source is the OS's; fall back on the process id and
        // the clock, which are enough to keep one run's names apart.
        let pid = std::process::id().to_le_bytes();
        bytes[..4].copy_from_slice(&pid);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or(0)
            .to_le_bytes();
        bytes[4..16].copy_from_slice(&nanos[..12]);
    }
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[cfg(test)]
pub mod tests {
    use std::path::PathBuf;

    use super::*;

    /// A directory under the system temp directory, removed on drop.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A private directory for one test.
    pub fn temp_dir(name: &str) -> TempDir {
        let path = std::env::temp_dir().join(format!("bake-update-{name}-{}", unique()));
        std::fs::create_dir_all(&path).expect("a temporary directory");
        TempDir(path)
    }

    /// Write `text` as an executable script.
    #[cfg(unix)]
    pub fn write_script(path: &Path, text: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::write(path, text).expect("a script");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("its mode");
    }

    /// A release directory whose `bin/bake` answers `--self-check` with
    /// the record of `version` on `target`, and exits `code`.
    #[cfg(unix)]
    pub fn fake_release(release: &Path, version: &str, target: &str, code: i32) {
        std::fs::create_dir_all(release.join("bin")).expect("bin");
        write_script(
            &release.join("bin").join("bake"),
            &fake_command(version, target, code),
        );
    }

    /// The script a fake release's command runs: the self-check record,
    /// `--version`, and nothing else.
    #[cfg(unix)]
    pub fn fake_command(version: &str, target: &str, code: i32) -> String {
        format!(
            "#!/bin/sh\n\
             case \"$1\" in\n\
             --self-check) printf '{{\"kind\":\"bake-self-check\",\"schema\":1,\"version\":\"{version}\",\"runtime\":\"rust\",\"target\":\"{target}\",\"docs\":{{}}}}\\n'; exit {code};;\n\
             --version) printf 'bake {version}\\n'; exit 0;;\n\
             esac\n\
             exit 2\n"
        )
    }

    #[test]
    fn a_unique_name_has_the_shape_of_a_uuid_and_repeats_nothing() {
        let first = unique();
        assert_eq!(first.len(), 36);
        assert_eq!(
            first.split('-').map(str::len).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        assert!(first.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'));
        assert_ne!(first, unique());
    }
}
