//! Scope 16's proof list for the native updater: every refused update,
//! held lock, interrupted switch, and failed candidate leaves the working
//! install byte for byte as it was, and an install can be updated and
//! rolled back with its command runnable at every step.
//!
//! Releases are served from a loopback HTTP fixture the test owns, signed
//! with an ephemeral Ed25519 key pair made by `ring`.

mod support;

use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bake_update::check::CandidateCheck;
use bake_update::error::UpdateError;
use bake_update::layout::{Name, Pointer, Root};
use bake_update::lock::{LockKind, acquire};
use bake_update::manifest::{archive_name, host_target};
use bake_update::source::Source;
use bake_update::update::{Effects, Outcome, Updater};

use support::{Keys, Server, TempDir, manifest_json, release_archive, snapshot, temp_dir};

/// One install on disk and the host that publishes its updates.
struct Fixture {
    dir: TempDir,
    server: Server,
    keys: Keys,
    target: &'static str,
    pointer: Pointer,
}

impl Fixture {
    fn new(name: &str) -> Self {
        Self::with_pointer(name, Pointer::host())
    }

    fn with_pointer(name: &str, pointer: Pointer) -> Self {
        let fixture = Self {
            dir: temp_dir(name),
            server: Server::start(),
            keys: Keys::new(),
            target: host_target().expect("a Bake release platform"),
            pointer,
        };
        std::fs::create_dir_all(fixture.root().releases()).expect("releases");
        fixture
    }

    fn root(&self) -> Root {
        Root::with_pointer(self.dir.path(), self.pointer)
    }

    /// Unpack a release into `releases/<version>`, as an install does.
    fn install(&self, version: &str) {
        let archive = self.dir.path().join(format!("{version}.tar.gz"));
        std::fs::write(&archive, release_archive(version, self.target, version, 0))
            .expect("an archive");
        bake_update::archive::extract(&archive, &self.root().release(version)).expect("unpacked");
        std::fs::remove_file(&archive).expect("the archive is staging only");
    }

    /// Install `version` and make it current, as a fresh install leaves
    /// the root.
    fn installed(&self, version: &str) -> &Self {
        self.install(version);
        self.root()
            .point_at(Name::Current, version)
            .expect("current");
        self
    }

    /// Publish `archive` as the stable channel's `version`.
    fn publish(&self, version: &str, archive: &[u8]) {
        self.publish_manifest(&manifest_json(version, &[(self.target, archive)]));
        self.server.put(
            &format!("releases/{version}/{}", archive_name(version, self.target)),
            archive.to_vec(),
        );
    }

    /// Serve `text` as the manifest, signed by the fixture's key.
    fn publish_manifest(&self, text: &str) {
        let signature = self.keys.sign(text.as_bytes());
        self.server.put("stable.json", text.as_bytes().to_vec());
        self.server.put("stable.json.sig", signature.into_bytes());
    }

    fn source(&self) -> Source {
        Source {
            base: self.server.base(),
            keys: vec![self.keys.public()],
            allow_loopback_http: true,
        }
    }

    /// An updater for this install, running `running`.
    fn updater(&self, running: &str) -> Updater {
        Updater::new(self.root(), self.source(), running).expect("a release platform")
    }

    /// Every file under the root, for a byte-for-byte comparison.
    fn snapshot(&self) -> Vec<(String, Vec<u8>)> {
        snapshot(self.dir.path())
    }

    /// What `<root>/current/bin/bake --version` prints, proving the
    /// pointer leads to a release that still runs.
    #[cfg(unix)]
    fn run_current(&self) -> String {
        let command = bake_update::layout::executable(&self.dir.path().join("current"))
            .expect("the current command");
        let output = std::process::Command::new(command)
            .arg("--version")
            .output()
            .expect("the command runs");
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }
}

/// A candidate check that accepts every release, for the tests that are
/// about something else and must run on every host.
struct AcceptCandidate;

impl CandidateCheck for AcceptCandidate {
    fn check(&self, _release: &Path, _version: &str, _target: &str) -> Result<(), String> {
        Ok(())
    }
}

/// A candidate check that refuses the release it is given.
struct RefuseCandidate;

impl CandidateCheck for RefuseCandidate {
    fn check(&self, _release: &Path, _version: &str, _target: &str) -> Result<(), String> {
        Err("it exited 1".to_owned())
    }
}

/// A disk with no room: the download's file accepts nothing.
struct FullDisk;

impl Effects for FullDisk {
    fn create(&self, _path: &Path) -> io::Result<Box<dyn Write + Send>> {
        Ok(Box::new(NoRoom))
    }
}

struct NoRoom;

impl Write for NoRoom {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::StorageFull,
            "no space left on device",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A release whose files another process holds open, as a running
/// Windows executable's are.
struct HeldRelease(String);

impl Effects for HeldRelease {
    fn remove_release(&self, path: &Path) -> io::Result<()> {
        if path.file_name().is_some_and(|name| name == self.0.as_str()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the file is in use by another process",
            ));
        }
        std::fs::remove_dir_all(path)
    }
}

/// Every refusal of a published update, each leaving the install as it
/// was. The cases are scope 16's proof list, from a tampered signature to
/// a candidate that does not run.
#[tokio::test]
async fn a_refused_update_leaves_the_working_install_untouched() {
    let archive_of = |version: &str, target: &str| release_archive(version, target, version, 0);
    let target = host_target().expect("a Bake release platform");
    let other = if target == "linux-x64" {
        "darwin-arm64"
    } else {
        "linux-x64"
    };

    // A tampered signature: the manifest's bytes are the owner's, the
    // signature is one byte different.
    let fixture = Fixture::new("tampered");
    fixture.installed("1.0.0");
    let archive = archive_of("2.0.0", target);
    fixture.publish("2.0.0", &archive);
    let text = manifest_json("2.0.0", &[(target, &archive)]);
    let mut signature = fixture.keys.sign(text.as_bytes()).into_bytes();
    signature[0] = if signature[0] == b'A' { b'B' } else { b'A' };
    fixture.server.put("stable.json.sig", signature);
    let before = fixture.snapshot();
    let error = fixture
        .updater("1.0.0")
        .update(None)
        .await
        .expect_err("a tampered signature");
    assert!(matches!(error, UpdateError::Signature(_)), "{error}");
    assert_eq!(fixture.snapshot(), before);
    assert_eq!(fixture.root().current(), Some("1.0.0".to_owned()));

    // A manifest signed by another key.
    let fixture = Fixture::new("wrong-key");
    fixture.installed("1.0.0");
    let archive = archive_of("2.0.0", target);
    fixture.publish("2.0.0", &archive);
    let text = manifest_json("2.0.0", &[(target, &archive)]);
    fixture.server.put(
        "stable.json.sig",
        Keys::new().sign(text.as_bytes()).into_bytes(),
    );
    let before = fixture.snapshot();
    let error = fixture
        .updater("1.0.0")
        .update(None)
        .await
        .expect_err("another key");
    assert!(matches!(error, UpdateError::Signature(_)), "{error}");
    assert_eq!(fixture.snapshot(), before);

    // An archive that is not the one the manifest hashes.
    let fixture = Fixture::new("hash");
    fixture.installed("1.0.0");
    let archive = archive_of("2.0.0", target);
    fixture.publish("2.0.0", &archive);
    let mut substituted = archive_of("2.0.0", target);
    substituted.extend_from_slice(&[0, 0, 0, 0]);
    fixture.server.put(
        &format!("releases/2.0.0/{}", archive_name("2.0.0", target)),
        substituted,
    );
    let before = fixture.snapshot();
    let error = fixture
        .updater("1.0.0")
        .update(None)
        .await
        .expect_err("another archive");
    assert!(matches!(error, UpdateError::Artifact(_)), "{error}");
    assert!(error.message().contains("larger than"), "{error}");
    assert_eq!(fixture.snapshot(), before);

    // The same bytes, hashed differently: the hash check, not the size.
    let fixture = Fixture::new("hash-only");
    fixture.installed("1.0.0");
    let archive = archive_of("2.0.0", target);
    let mut other_archive = archive.clone();
    let last = other_archive.len() - 1;
    other_archive[last] ^= 0xff;
    assert_eq!(archive.len(), other_archive.len());
    fixture.publish("2.0.0", &archive);
    fixture.server.put(
        &format!("releases/2.0.0/{}", archive_name("2.0.0", target)),
        other_archive,
    );
    let before = fixture.snapshot();
    let error = fixture
        .updater("1.0.0")
        .update(None)
        .await
        .expect_err("another archive");
    assert!(error.message().contains("SHA-256"), "{error}");
    assert_eq!(fixture.snapshot(), before);

    // A truncated archive the manifest describes exactly: the download
    // passes and the unpacking refuses it.
    let fixture = Fixture::new("truncated");
    fixture.installed("1.0.0");
    let whole = archive_of("2.0.0", target);
    let truncated = whole[..whole.len() / 2].to_vec();
    fixture.publish("2.0.0", &truncated);
    let before = fixture.snapshot();
    let error = fixture
        .updater("1.0.0")
        .update(None)
        .await
        .expect_err("a truncated archive");
    assert!(matches!(error, UpdateError::Archive(_)), "{error}");
    assert_eq!(fixture.snapshot(), before);

    // A manifest that publishes no archive for this platform.
    let fixture = Fixture::new("platform");
    fixture.installed("1.0.0");
    let archive = archive_of("2.0.0", other);
    fixture.publish_manifest(&manifest_json("2.0.0", &[(other, &archive)]));
    let before = fixture.snapshot();
    let error = fixture
        .updater("1.0.0")
        .update(None)
        .await
        .expect_err("another platform");
    assert!(matches!(error, UpdateError::Artifact(_)), "{error}");
    assert!(error.message().contains(target), "{error}");
    assert_eq!(fixture.snapshot(), before);

    // A manifest older than the installed release: a rollback attack.
    let fixture = Fixture::new("older");
    fixture.installed("2.0.0");
    let archive = archive_of("1.0.0", target);
    fixture.publish("1.0.0", &archive);
    let before = fixture.snapshot();
    let error = fixture
        .updater("2.0.0")
        .update(None)
        .await
        .expect_err("an older manifest");
    assert!(matches!(error, UpdateError::Version(_)), "{error}");
    assert!(error.message().contains("older than"), "{error}");
    assert_eq!(fixture.snapshot(), before);
    // A check reports it the same way, and installs nothing either.
    let error = fixture
        .updater("2.0.0")
        .check_only()
        .await
        .expect_err("an older manifest");
    assert!(matches!(error, UpdateError::Version(_)), "{error}");
    assert_eq!(fixture.snapshot(), before);

    // A manifest with a member the schema does not have.
    let fixture = Fixture::new("unknown-member");
    fixture.installed("1.0.0");
    let archive = archive_of("2.0.0", target);
    let text = manifest_json("2.0.0", &[(target, &archive)])
        .replace("\"artifacts\"", "\"notes\":\"hello\",\"artifacts\"");
    fixture.publish_manifest(&text);
    let before = fixture.snapshot();
    let error = fixture
        .updater("1.0.0")
        .update(None)
        .await
        .expect_err("an unknown member");
    assert!(matches!(error, UpdateError::Manifest(_)), "{error}");
    assert!(error.message().contains("'notes'"), "{error}");
    assert_eq!(fixture.snapshot(), before);
}

/// The size the manifest states bounds the download, so a host that
/// keeps sending bytes is cut off.
#[tokio::test]
async fn an_oversized_download_is_refused_while_it_streams() {
    let target = host_target().expect("a Bake release platform");
    let fixture = Fixture::new("oversized");
    fixture.installed("1.0.0");
    let archive = release_archive("2.0.0", target, "2.0.0", 0);
    // The manifest describes a shorter archive than the host serves.
    let stated = &archive[..archive.len() - 64];
    fixture.publish_manifest(&manifest_json("2.0.0", &[(target, stated)]));
    fixture.server.put(
        &format!("releases/2.0.0/{}", archive_name("2.0.0", target)),
        archive.clone(),
    );
    let before = fixture.snapshot();
    let error = fixture
        .updater("1.0.0")
        .update(None)
        .await
        .expect_err("an oversized archive");
    assert!(matches!(error, UpdateError::Artifact(_)), "{error}");
    assert!(error.message().contains("larger than"), "{error}");
    assert_eq!(fixture.snapshot(), before);
    assert_eq!(fixture.root().current(), Some("1.0.0".to_owned()));
}

/// A download that cannot be written leaves nothing behind.
#[tokio::test]
async fn a_full_disk_leaves_the_working_install_untouched() {
    let target = host_target().expect("a Bake release platform");
    let fixture = Fixture::new("full-disk");
    fixture.installed("1.0.0");
    let archive = release_archive("2.0.0", target, "2.0.0", 0);
    fixture.publish("2.0.0", &archive);
    let before = fixture.snapshot();
    let mut updater = fixture.updater("1.0.0");
    updater.effects = Arc::new(FullDisk);
    let error = updater.update(None).await.expect_err("a full disk");
    assert!(matches!(error, UpdateError::Install(_)), "{error}");
    assert!(error.message().contains("no space left"), "{error}");
    assert_eq!(fixture.snapshot(), before);
    assert_eq!(fixture.root().current(), Some("1.0.0".to_owned()));
}

/// A candidate that does not pass its own self-check never becomes
/// current, whether it fails outright or reports another version.
#[tokio::test]
async fn a_failed_candidate_self_check_keeps_the_pointer() {
    let target = host_target().expect("a Bake release platform");
    for (name, archive, injected) in [
        (
            "self-check-exit",
            release_archive("2.0.0", target, "2.0.0", 1),
            false,
        ),
        (
            "self-check-version",
            release_archive("2.0.0", target, "1.9.9", 0),
            false,
        ),
        (
            "self-check-injected",
            release_archive("2.0.0", target, "2.0.0", 0),
            true,
        ),
    ] {
        let fixture = Fixture::new(name);
        fixture.installed("1.0.0");
        fixture.publish("2.0.0", &archive);
        let before = fixture.snapshot();
        let mut updater = fixture.updater("1.0.0");
        if injected {
            updater.check = Arc::new(RefuseCandidate);
        } else if cfg!(not(unix)) {
            // The other two candidates are shell scripts.
            continue;
        }
        let error = updater.update(None).await.expect_err(name);
        assert!(
            matches!(error, UpdateError::SelfCheck(_)),
            "{name}: {error}"
        );
        assert!(error.message().contains("did not run"), "{name}: {error}");
        assert_eq!(fixture.snapshot(), before, "{name}");
        assert_eq!(fixture.root().current(), Some("1.0.0".to_owned()), "{name}");
    }
}

/// A contended lock refuses promptly and changes nothing.
#[tokio::test]
async fn a_held_install_lock_refuses_the_update_and_the_rollback() {
    let target = host_target().expect("a Bake release platform");
    let fixture = Fixture::new("locked");
    fixture.installed("1.0.0");
    fixture.publish("2.0.0", &release_archive("2.0.0", target, "2.0.0", 0));
    let held = acquire(fixture.dir.path(), LockKind::Install).expect("the lock");
    let before = fixture.snapshot();
    let started = Instant::now();
    let mut updater = fixture.updater("1.0.0");
    updater.check = Arc::new(AcceptCandidate);
    let error = updater.update(None).await.expect_err("a held lock");
    assert!(matches!(error, UpdateError::Locked(_)), "{error}");
    assert!(
        error.message().contains("another Bake install is running"),
        "{error}"
    );
    assert!(started.elapsed() < Duration::from_secs(5), "it waited");
    let error = updater.rollback().expect_err("a held lock");
    assert!(matches!(error, UpdateError::Locked(_)), "{error}");
    assert_eq!(fixture.snapshot(), before);
    drop(held);
    // With the lock free, the same update runs.
    assert!(matches!(
        updater.update(None).await.expect("the update"),
        Outcome::Updated { .. }
    ));
}

/// A run killed between writing the temporary pointer and renaming it,
/// or between the two pointer moves, leaves an install the next run
/// repairs: nothing points at a half-installed release, and the update
/// proceeds.
#[tokio::test]
async fn an_interrupted_pointer_switch_is_repaired_by_the_next_run() {
    let target = host_target().expect("a Bake release platform");
    let fixture = Fixture::new("interrupted");
    fixture.installed("1.0.0");
    fixture.install("2.0.0");
    // A temporary pointer and a staging directory an interrupted run left.
    let leftover = fixture.dir.path().join(".current.stale-run");
    std::fs::write(&leftover, "2.0.0\n").expect("a leftover pointer");
    let staging = fixture.root().releases().join(".staging-stale-run");
    std::fs::create_dir_all(staging.join("unpacked")).expect("a leftover staging directory");
    // `previous` was written, and the run stopped before `current` moved.
    fixture
        .root()
        .point_at(Name::Previous, "1.0.0")
        .expect("previous");
    assert_eq!(fixture.root().current(), Some("1.0.0".to_owned()));
    // Previous equals current, so there is nothing to roll back to.
    let mut updater = fixture.updater("1.0.0");
    updater.check = Arc::new(AcceptCandidate);
    assert_eq!(
        updater.rollback().expect("a rollback that finds nothing"),
        Outcome::NoPrevious {
            current: "1.0.0".to_owned()
        }
    );
    fixture.publish("3.0.0", &release_archive("3.0.0", target, "3.0.0", 0));
    let outcome = updater.update(None).await.expect("the update proceeds");
    assert_eq!(
        outcome,
        Outcome::Updated {
            from: Some("1.0.0".to_owned()),
            to: "3.0.0".to_owned(),
            pruned: vec!["2.0.0".to_owned()],
        }
    );
    assert!(!leftover.exists(), "the stale pointer stayed");
    assert!(!staging.exists(), "the stale staging directory stayed");
    assert_eq!(fixture.root().current(), Some("3.0.0".to_owned()));
    assert_eq!(fixture.root().previous(), Some("1.0.0".to_owned()));
}

/// Retention keeps current and previous. A release whose files are held
/// open stays for a later prune instead of failing the update.
#[tokio::test]
async fn a_held_release_is_left_for_a_later_prune() {
    let target = host_target().expect("a Bake release platform");
    let fixture = Fixture::new("prune");
    fixture.installed("1.0.0");
    fixture.install("0.9.0");
    fixture.install("0.8.0");
    fixture.publish("2.0.0", &release_archive("2.0.0", target, "2.0.0", 0));
    let mut updater = fixture.updater("1.0.0");
    updater.check = Arc::new(AcceptCandidate);
    updater.effects = Arc::new(HeldRelease("0.9.0".to_owned()));
    let outcome = updater.update(None).await.expect("the update");
    assert_eq!(
        outcome,
        Outcome::Updated {
            from: Some("1.0.0".to_owned()),
            to: "2.0.0".to_owned(),
            pruned: vec!["0.8.0".to_owned()],
        }
    );
    let mut installed = fixture.root().installed();
    installed.sort();
    assert_eq!(installed, ["0.9.0", "1.0.0", "2.0.0"]);
    assert_eq!(fixture.root().current(), Some("2.0.0".to_owned()));
    assert_eq!(fixture.root().previous(), Some("1.0.0".to_owned()));
}

/// A check reports the published version and installs nothing.
#[tokio::test]
async fn a_check_only_run_installs_nothing() {
    let target = host_target().expect("a Bake release platform");
    let fixture = Fixture::new("check-only");
    fixture.installed("1.0.0");
    fixture.publish("2.0.0", &release_archive("2.0.0", target, "2.0.0", 0));
    let before = fixture.snapshot();
    assert_eq!(
        fixture
            .updater("1.0.0")
            .check_only()
            .await
            .expect("a check"),
        Outcome::Available {
            installed: "1.0.0".to_owned(),
            version: "2.0.0".to_owned()
        }
    );
    assert_eq!(fixture.snapshot(), before);
    fixture.publish("1.0.0", &release_archive("1.0.0", target, "1.0.0", 0));
    assert_eq!(
        fixture
            .updater("1.0.0")
            .check_only()
            .await
            .expect("a check"),
        Outcome::UpToDate {
            version: "1.0.0".to_owned()
        }
    );
    assert_eq!(fixture.snapshot(), before);
}

/// The Windows pointer form: `current.txt` and `previous.txt` hold the
/// version and a newline, and the same switch and rollback work.
#[tokio::test]
async fn the_version_file_pointer_switches_and_rolls_back() {
    let target = host_target().expect("a Bake release platform");
    let fixture = Fixture::with_pointer("pointer-file", Pointer::File);
    fixture.installed("1.0.0");
    fixture.publish("2.0.0", &release_archive("2.0.0", target, "2.0.0", 0));
    let mut updater = fixture.updater("1.0.0");
    updater.check = Arc::new(AcceptCandidate);
    assert!(matches!(
        updater.update(None).await.expect("the update"),
        Outcome::Updated { .. }
    ));
    assert_eq!(
        std::fs::read_to_string(fixture.dir.path().join("current.txt")).expect("current.txt"),
        "2.0.0\n"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.dir.path().join("previous.txt")).expect("previous.txt"),
        "1.0.0\n"
    );
    assert_eq!(
        updater.rollback().expect("the rollback"),
        Outcome::RolledBack {
            from: "2.0.0".to_owned(),
            to: "1.0.0".to_owned()
        }
    );
    assert_eq!(
        std::fs::read_to_string(fixture.dir.path().join("current.txt")).expect("current.txt"),
        "1.0.0\n"
    );
    assert_eq!(fixture.root().previous(), Some("2.0.0".to_owned()));
}

/// The whole route on Unix: an install of 1.0.0, an update to 2.0.0, and
/// a rollback, with `current/bin/bake --version` run after each step.
#[cfg(unix)]
#[tokio::test]
async fn an_install_updates_and_rolls_back_with_a_runnable_command() {
    let target = host_target().expect("a Bake release platform");
    let fixture = Fixture::new("end-to-end");
    fixture.installed("1.0.0");
    assert_eq!(fixture.run_current(), "bake 1.0.0");
    fixture.publish("2.0.0", &release_archive("2.0.0", target, "2.0.0", 0));

    let updater = fixture.updater("1.0.0");
    let outcome = updater.update(None).await.expect("the update");
    assert_eq!(
        outcome,
        Outcome::Updated {
            from: Some("1.0.0".to_owned()),
            to: "2.0.0".to_owned(),
            pruned: Vec::new(),
        }
    );
    assert_eq!(fixture.run_current(), "bake 2.0.0");
    assert_eq!(
        std::fs::read_link(fixture.dir.path().join("current")).expect("current"),
        Path::new("releases/2.0.0")
    );

    // Up to date: the same manifest installs nothing more.
    assert_eq!(
        updater.update(None).await.expect("up to date"),
        Outcome::UpToDate {
            version: "2.0.0".to_owned()
        }
    );
    assert_eq!(fixture.run_current(), "bake 2.0.0");

    let outcome = updater.rollback().expect("the rollback");
    assert_eq!(
        outcome,
        Outcome::RolledBack {
            from: "2.0.0".to_owned(),
            to: "1.0.0".to_owned()
        }
    );
    assert_eq!(fixture.run_current(), "bake 1.0.0");
    assert_eq!(fixture.root().previous(), Some("2.0.0".to_owned()));

    // Rolling forward again returns to 2.0.0, and both releases stay.
    assert_eq!(
        updater.rollback().expect("the second rollback"),
        Outcome::RolledBack {
            from: "1.0.0".to_owned(),
            to: "2.0.0".to_owned()
        }
    );
    assert_eq!(fixture.run_current(), "bake 2.0.0");
    let mut installed = fixture.root().installed();
    installed.sort();
    assert_eq!(installed, ["1.0.0", "2.0.0"]);
}

/// `--force` installs the published version even when it is not newer,
/// and refuses to name a version the channel does not publish.
#[cfg(unix)]
#[tokio::test]
async fn forcing_a_version_reinstalls_only_what_is_published() {
    let target = host_target().expect("a Bake release platform");
    let fixture = Fixture::new("force");
    fixture.installed("2.0.0");
    fixture.publish("1.0.0", &release_archive("1.0.0", target, "1.0.0", 0));
    let updater = fixture.updater("2.0.0");
    let error = updater
        .update(Some("3.0.0"))
        .await
        .expect_err("not published");
    assert!(matches!(error, UpdateError::Version(_)), "{error}");
    assert!(error.message().contains("publishes Bake 1.0.0"), "{error}");
    let outcome = updater.update(Some("1.0.0")).await.expect("the downgrade");
    assert_eq!(
        outcome,
        Outcome::Updated {
            from: Some("2.0.0".to_owned()),
            to: "1.0.0".to_owned(),
            pruned: Vec::new(),
        }
    );
    assert_eq!(fixture.run_current(), "bake 1.0.0");
}
