//! The per-call policy, from `SandboxExecutionPolicy`/`SandboxPolicy` in
//! `packages/sandbox/sandbox/src/index.ts` and `writableRoots` in `roots.ts`.

use std::path::{Path, PathBuf};

use crate::mode::SandboxMode;
use crate::paths::canonical_path;

/// Everything one confined execution may touch. It travels with each call,
/// not with the provider: bash may run `read-only` while another consumer
/// runs `workspace-write` at the same instant, and an approved escalation is
/// a new call with a wider policy. Callers resolve defaults before building
/// it; providers treat it as complete.
///
/// The root is carried under every mode so a caller can resolve the policy
/// once before choosing how to enforce it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SandboxPolicy {
    /// The file-effect mode.
    pub mode: SandboxMode,
    /// The absolute directory `workspace-write` may write under: the calling
    /// session's working directory.
    pub workspace_root: PathBuf,
    /// The calling session, when there is one; backends may key per-session
    /// state off it (private temporary directories, for example).
    pub session_id: Option<String>,
    /// Further directories `workspace-write` may write under, added by policy
    /// layers above the TypeScript contract. Empty by default, which is the
    /// TypeScript behavior.
    pub extra_writable_roots: Vec<PathBuf>,
}

impl SandboxPolicy {
    /// A policy with no session and no extra roots.
    pub fn new(mode: SandboxMode, workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            mode,
            workspace_root: workspace_root.into(),
            session_id: None,
            extra_writable_roots: Vec::new(),
        }
    }

    /// The same policy for the session `id`.
    pub fn with_session_id(mut self, id: impl Into<String>) -> Self {
        self.session_id = Some(id.into());
        self
    }

    /// The same policy with `roots` writable as well under `workspace-write`.
    pub fn with_extra_writable_roots(mut self, roots: impl IntoIterator<Item = PathBuf>) -> Self {
        self.extra_writable_roots.extend(roots);
        self
    }

    /// The same policy under `mode`: an approved escalation's retry.
    pub fn with_mode(mut self, mode: SandboxMode) -> Self {
        self.mode = mode;
        self
    }

    /// The canonical directories this policy may write under
    /// (`writableRoots`): none outside `workspace-write`; there, the
    /// workspace, `/tmp`, the OS temporary directory, and any extra roots,
    /// canonicalized and without duplicates, in that order.
    pub fn writable_roots(&self) -> Vec<PathBuf> {
        if self.mode != SandboxMode::WorkspaceWrite {
            return Vec::new();
        }
        let candidates = [
            self.workspace_root.clone(),
            PathBuf::from("/tmp"),
            std::env::temp_dir(),
        ]
        .into_iter()
        .chain(self.extra_writable_roots.iter().cloned());
        let mut roots: Vec<PathBuf> = Vec::new();
        for root in candidates {
            let root = canonical_path(&root);
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        roots
    }

    /// Whether this policy lets a process write `path`, with the path and
    /// every root canonicalized first. `danger-full-access` allows
    /// everything and `read-only` nothing.
    pub fn allows_write(&self, path: &Path) -> bool {
        match self.mode {
            SandboxMode::DangerFullAccess => true,
            SandboxMode::ReadOnly => false,
            SandboxMode::WorkspaceWrite => {
                let path = canonical_path(path);
                self.writable_roots()
                    .iter()
                    .any(|root| crate::paths::is_path_under(&path, root))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_workspace_write_has_roots() {
        let root = std::env::temp_dir();
        assert!(
            SandboxPolicy::new(SandboxMode::ReadOnly, &root)
                .writable_roots()
                .is_empty()
        );
        assert!(
            SandboxPolicy::new(SandboxMode::DangerFullAccess, &root)
                .writable_roots()
                .is_empty()
        );
        let roots = SandboxPolicy::new(SandboxMode::WorkspaceWrite, &root).writable_roots();
        assert_eq!(roots[0], canonical_path(&root));
        let unique: std::collections::HashSet<_> = roots.iter().collect();
        assert_eq!(unique.len(), roots.len(), "{roots:?}");
    }

    #[test]
    fn extra_roots_follow_the_typescript_ones() {
        let workspace = std::env::temp_dir().join("bake-sandbox-policy-extra-ws");
        let extra = std::env::temp_dir().join("bake-sandbox-policy-extra-root");
        let roots = SandboxPolicy::new(SandboxMode::WorkspaceWrite, &workspace)
            .with_extra_writable_roots([extra.clone()])
            .writable_roots();
        assert_eq!(roots.first(), Some(&canonical_path(&workspace)));
        assert_eq!(roots.last(), Some(&canonical_path(&extra)));
    }

    #[test]
    fn allows_write_follows_the_mode() {
        let workspace = std::env::temp_dir().join("bake-sandbox-policy-allows");
        let inside = workspace.join("a.txt");
        let policy = SandboxPolicy::new(SandboxMode::WorkspaceWrite, &workspace);
        assert!(policy.allows_write(&inside));
        assert!(!SandboxPolicy::new(SandboxMode::ReadOnly, &workspace).allows_write(&inside));
        assert!(
            SandboxPolicy::new(SandboxMode::DangerFullAccess, &workspace)
                .allows_write(Path::new("/etc/passwd"))
        );
    }
}
