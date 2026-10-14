//! The per-call policy, from `SandboxExecutionPolicy`/`SandboxPolicy` in
//! `packages/sandbox/sandbox/src/index.ts` and `writableRoots` in `roots.ts`.

use std::path::{Path, PathBuf};

use crate::mode::SandboxMode;
use crate::paths::{canonical_path, normalize_lexically};

/// Everything one confined execution may touch. It travels with each call,
/// not with the provider: bash may run `read-only` while another consumer
/// runs `workspace-write` at the same instant, and an approved escalation is
/// a new call with a wider policy. Callers resolve defaults before building
/// it; providers treat it as complete.
///
/// The root is carried under every mode so a caller can resolve the policy
/// once before choosing how to enforce it.
///
/// # Canonical roots
///
/// [`workspace_root`](Self::workspace_root) and
/// [`extra_writable_roots`](Self::extra_writable_roots) must be canonical
/// (see [`canonical_path`]), resolved once per session, when the session
/// starts. The policy never resolves them again: a confined command that
/// moves the workspace and leaves a symbolic link in its place must not
/// redirect the next call's grant to the link's target. The local backends
/// refuse a call whose root is no longer the directory it named (see
/// [`crate::backends`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SandboxPolicy {
    /// The file-effect mode.
    pub mode: SandboxMode,
    /// The directory `workspace-write` may write under: the calling
    /// session's working directory, canonical and resolved once when the
    /// session started (see [Canonical roots](Self#canonical-roots)).
    pub workspace_root: PathBuf,
    /// The calling session, when there is one; backends may key per-session
    /// state off it (private temporary directories, for example).
    pub session_id: Option<String>,
    /// Further directories `workspace-write` may write under, added by policy
    /// layers above the TypeScript contract; canonical, like the workspace. Empty by default, which is the
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

    /// The directories this policy may write under (`writableRoots`): none
    /// outside `workspace-write`; there, the workspace, `/tmp` (Unix only), the OS
    /// temporary directory, and any extra roots, without duplicates, in that
    /// order.
    ///
    /// The workspace and extra roots are used as given, only lexically
    /// normalized (`.` removed, `..` applied): the caller resolved them once
    /// per session (see [Canonical roots](Self#canonical-roots)), and
    /// resolving them again on every call would follow whatever a confined
    /// command left at that path. Only `/tmp` and the OS temporary directory
    /// are canonicalized, because only the system administrator controls
    /// those paths themselves (`/tmp` is `/private/tmp` on macOS).
    ///
    /// TypeScript's `writableRoots` called `realpathSync` on every root on
    /// every call; this is the deliberate difference.
    pub fn writable_roots(&self) -> Vec<PathBuf> {
        if self.mode != SandboxMode::WorkspaceWrite {
            return Vec::new();
        }
        // `/tmp` is a POSIX path; on Windows it would name `C:\tmp`, which no
        // command expects to be writable.
        let system_temp = cfg!(unix).then(|| canonical_path(Path::new("/tmp")));
        let candidates = [normalize_lexically(&self.workspace_root)]
            .into_iter()
            .chain(system_temp)
            .chain([canonical_path(&std::env::temp_dir())])
            .chain(
                self.extra_writable_roots
                    .iter()
                    .map(|root| normalize_lexically(root)),
            );
        let mut roots: Vec<PathBuf> = Vec::new();
        for root in candidates {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        roots
    }

    /// Whether this policy lets a process write `path`: the path is
    /// canonicalized and compared with [`writable_roots`](Self::writable_roots)
    /// by component. `danger-full-access` allows everything and `read-only`
    /// nothing.
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
        let root = canonical_path(&root);
        let roots = SandboxPolicy::new(SandboxMode::WorkspaceWrite, &root).writable_roots();
        assert_eq!(roots[0], root);
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
        assert_eq!(roots.first(), Some(&workspace));
        assert_eq!(roots.last(), Some(&extra));
    }

    #[test]
    fn caller_roots_are_normalized_but_never_resolved() {
        let policy = SandboxPolicy::new(SandboxMode::WorkspaceWrite, "/ws/./a/../b")
            .with_extra_writable_roots([PathBuf::from("/x/y/..")]);
        let roots = policy.writable_roots();
        assert_eq!(roots.first(), Some(&PathBuf::from("/ws/b")));
        assert_eq!(roots.last(), Some(&PathBuf::from("/x")));
        assert_eq!(
            roots.contains(&canonical_path(Path::new("/tmp"))),
            cfg!(unix),
            "{roots:?}"
        );
    }

    /// The reviewer's workspace swap: after the workspace is moved and a
    /// link left in its place, the roots still name the original path, not
    /// the link's target.
    #[cfg(unix)]
    #[test]
    fn a_workspace_replaced_by_a_link_is_not_followed() {
        let base = canonical_path(&std::env::temp_dir())
            .join(format!("bake-policy-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let workspace = base.join("ws");
        let victim = base.join("victim");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(&victim).expect("victim");
        let policy = SandboxPolicy::new(SandboxMode::WorkspaceWrite, &workspace);
        std::fs::rename(&workspace, base.join("ws.gone")).expect("move");
        std::os::unix::fs::symlink(&victim, &workspace).expect("link");
        let roots = policy.writable_roots();
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(roots.first(), Some(&workspace));
        assert!(!roots.contains(&victim), "{roots:?}");
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
