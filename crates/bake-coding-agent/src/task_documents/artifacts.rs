//! Content versions for the explicitly declared inputs to a check.
//!
//! The caller runs a check through the normal permission and process owner,
//! captures these inputs before and after it, and records evidence only when
//! both sets agree. Freshness covers these files, not undeclared dependencies,
//! environment variables, remote services, or a whole repository by implication.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::{Component, Path};

use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};

use crate::permissions::beneath::open_regular_read;

/// Maximum declared inputs for one verification.
pub const MAX_ARTIFACTS: usize = 64;
/// Aggregate bytes read to fingerprint a set.
pub const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;

/// Deterministic paths and SHA-256 content versions for a declared input set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArtifactSet(BTreeMap<String, String>);

impl ArtifactSet {
    /// Read bounded workspace-relative regular files without parent traversal.
    /// Resolved paths use the permission layer's link-refusing open, including
    /// its documented Windows junction-race limit. Blocks: the session owner
    /// runs this on its blocking pool and awaits it through cancellation.
    pub fn capture(root: &Path, paths: &[String]) -> io::Result<Self> {
        Self::capture_with(root, paths, |_| {})
    }

    // The hook places filesystem substitutions immediately before the real
    // open in tests; production supplies no operation.
    fn capture_with(
        root: &Path,
        paths: &[String],
        mut before_open: impl FnMut(&Path),
    ) -> io::Result<Self> {
        if paths.is_empty() || paths.len() > MAX_ARTIFACTS {
            return Err(invalid("verification needs 1–64 declared artifact paths"));
        }
        let root = root.canonicalize()?;
        let mut versions = BTreeMap::new();
        let mut remaining = MAX_ARTIFACT_BYTES;
        for name in paths {
            let path = Path::new(name);
            if name.is_empty()
                || name.len() > 4096
                || path
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_)))
            {
                return Err(invalid("artifact paths must be relative without traversal"));
            }
            if versions.contains_key(name) {
                return Err(invalid("artifact paths must be unique"));
            }
            let resolved = root.join(path).canonicalize()?;
            if !resolved.starts_with(&root) {
                return Err(invalid("artifact resolves outside the workspace"));
            }
            if !resolved.metadata()?.is_file() {
                return Err(invalid("artifact must be a regular file"));
            }
            before_open(&resolved);
            let file = open_regular_read(&root, &resolved)?;
            let mut reader = file.take(remaining + 1);
            let mut hash = Context::new(&SHA256);
            let mut buffer = [0; 8192];
            loop {
                let count = reader.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                remaining = remaining
                    .checked_sub(count as u64)
                    .ok_or_else(|| invalid("declared artifacts exceed 64 MiB"))?;
                hash.update(&buffer[..count]);
            }
            let digest = hash
                .finish()
                .as_ref()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            versions.insert(name.clone(), digest);
        }
        Ok(Self(versions))
    }

    /// Validate a decoded set before trusting it as verification metadata.
    pub fn validate(&self) -> io::Result<()> {
        if self.0.is_empty() || self.0.len() > MAX_ARTIFACTS {
            return Err(invalid("invalid artifact count"));
        }
        for (name, digest) in &self.0 {
            if name.is_empty()
                || name.len() > 4096
                || Path::new(name)
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_)))
                || digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(invalid("invalid artifact path or content digest"));
            }
        }
        Ok(())
    }

    /// Capture the declared paths again, retaining their exact identities.
    /// Missing or unreadable inputs return an error. Like [`Self::capture`],
    /// this blocks and belongs on the owner's awaited blocking pool.
    pub fn recapture(&self, root: &Path) -> io::Result<Self> {
        self.validate()?;
        Self::capture(root, &self.0.keys().cloned().collect::<Vec<_>>())
    }

    /// Whether the declared inputs still have these versions. Missing or
    /// unreadable inputs return an error and cannot count as fresh evidence.
    pub fn is_current(&self, root: &Path) -> io::Result<bool> {
        Ok(self.recapture(root)? == *self)
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;
    use std::fs::File;

    #[test]
    fn evidence_tracks_content_including_changes_that_keep_length_and_name() {
        let dir = TempDir::new("artifact-content");
        std::fs::write(dir.join("source"), "old").unwrap();
        std::fs::write(dir.join("check"), "test").unwrap();
        let versions =
            ArtifactSet::capture(dir.path(), &["source".into(), "check".into()]).unwrap();
        assert!(versions.is_current(dir.path()).unwrap());
        std::fs::write(dir.join("source"), "new").unwrap();
        assert!(!versions.is_current(dir.path()).unwrap());
        std::fs::write(dir.join("source"), "old").unwrap();
        std::fs::write(dir.join("check"), "fail").unwrap();
        assert!(!versions.is_current(dir.path()).unwrap());
        std::fs::remove_file(dir.join("check")).unwrap();
        assert!(versions.is_current(dir.path()).is_err());
    }

    #[test]
    fn traversal_duplicates_missing_files_and_non_files_do_not_verify() {
        let dir = TempDir::new("artifact-bounds");
        std::fs::write(dir.join("file"), "content").unwrap();
        for paths in [
            vec![],
            vec!["../outside".into()],
            vec!["/absolute".into()],
            vec!["missing".into()],
            vec!["file".into(), "file".into()],
        ] {
            assert!(
                ArtifactSet::capture(dir.path(), &paths).is_err(),
                "{paths:?}"
            );
        }
        std::fs::create_dir(dir.join("folder")).unwrap();
        assert!(ArtifactSet::capture(dir.path(), &["folder".into()]).is_err());
        File::create(dir.join("large"))
            .unwrap()
            .set_len(MAX_ARTIFACT_BYTES + 1)
            .unwrap();
        assert!(ArtifactSet::capture(dir.path(), &["large".into()]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_outside_the_workspace_cannot_supply_evidence() {
        let dir = TempDir::new("artifact-symlink");
        let outside = TempDir::new("artifact-outside");
        std::fs::write(outside.join("file"), "content").unwrap();
        std::os::unix::fs::symlink(outside.join("file"), dir.join("file")).unwrap();
        assert!(ArtifactSet::capture(dir.path(), &["file".into()]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_parent_swapped_after_resolution_cannot_supply_outside_evidence() {
        for walk in [false, true] {
            let dir = TempDir::new("artifact-parent-swap");
            let outside = TempDir::new("artifact-parent-outside");
            let root = dir.path().canonicalize().unwrap();
            let _walk = walk.then(|| crate::permissions::fs::seam::NoOpenat2::hold(&root));
            std::fs::create_dir(root.join("sub")).unwrap();
            std::fs::write(root.join("sub/file"), "inside").unwrap();
            std::fs::write(outside.join("file"), "outside").unwrap();
            let result = ArtifactSet::capture_with(&root, &["sub/file".into()], |_| {
                std::fs::rename(root.join("sub"), root.join("aside")).unwrap();
                std::os::unix::fs::symlink(outside.path(), root.join("sub")).unwrap();
            });
            assert!(result.is_err(), "walk={walk}: {result:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_link_within_the_workspace_keeps_its_declared_name() {
        let dir = TempDir::new("artifact-inside-link");
        std::fs::write(dir.join("file"), "content").unwrap();
        std::os::unix::fs::symlink("file", dir.join("alias")).unwrap();
        let versions = ArtifactSet::capture(dir.path(), &["alias".into()]).unwrap();
        assert!(versions.0.contains_key("alias"));
        assert!(versions.is_current(dir.path()).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_substituted_after_resolution_needs_no_writer() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::process::{Child, Command, Stdio};
        use std::time::{Duration, Instant};

        const CHILD_ROOT: &str = "BAKE_TEST_ARTIFACT_FIFO_ROOT";
        const CASE: &str =
            "task_documents::artifacts::tests::a_fifo_substituted_after_resolution_needs_no_writer";
        const PATIENCE: Duration = Duration::from_secs(30);
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = Path::new(&root).canonicalize().unwrap();
            for walk in [false, true] {
                let _walk = walk.then(|| crate::permissions::fs::seam::NoOpenat2::hold(&root));
                let name = format!("fifo-{walk}");
                let path = root.join(&name);
                std::fs::write(&path, "regular before the open").unwrap();
                let result =
                    ArtifactSet::capture_with(&root, std::slice::from_ref(&name), |resolved| {
                        std::fs::remove_file(resolved).unwrap();
                        let name = CString::new(resolved.as_os_str().as_bytes()).unwrap();
                        // SAFETY: the NUL-terminated path lives through the call,
                        // and this process owns the directory and its absent leaf.
                        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
                    });
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
                assert!(ArtifactSet::capture(&root, &[name]).is_err());
            }
            return;
        }

        // A regression to a blocking open must fail within a bound and leave
        // no waiting thread behind. The parent owns the path and reaps the child.
        struct OwnedChild(Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let dir = TempDir::new("artifact-fifo");
        let mut child = OwnedChild(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", CASE, "--nocapture"])
                .env(CHILD_ROOT, dir.path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + PATIENCE;
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "artifact capture waited on a FIFO without a writer"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(status.success(), "FIFO check failed: {status}");
    }
}
