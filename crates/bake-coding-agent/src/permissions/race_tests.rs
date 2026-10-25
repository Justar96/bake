//! The swap race between the containment check and the write, made
//! deterministic by the test seam in [`super::fs::seam`]: a hook swaps a
//! link in after the guard has checked the target and before the call
//! touches it. Not a TypeScript case: `fs-sandbox/src/index.ts` accepted
//! this race; Bake's write closes it ([`super::beneath`]).
//!
//! Each case runs twice on Linux, through `openat2` and through the
//! per-component walk that stands in where the kernel lacks it. The
//! directories sit beside the test binary, outside `/tmp` and the OS
//! temporary directory that `workspace-write` also allows; a case skips
//! with a note where the binary itself sits under one.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bake_agent::{AgentTool, AgentToolUpdateCallback};
use bake_sandbox::{SandboxMode, UnavailableProvider};
use serde_json::{Value, json};

use super::fs::seam;
use super::{NoApprover, StandingPermissions, ToolPermissions, sandbox_file_tools};
use crate::tools::index::{ToolsOptions, create_all_tool_definitions};

const DENIED: &str = "[sandbox: file access denied under workspace-write mode]\n[sandbox: escalation available — retry this exact operation once with sandbox_permissions (the narrowest wider mode that suffices) + justification; the approval prompt asks the user]";

/// A workspace `ws` and a directory `out` outside it, removed on drop.
struct Dirs {
    base: PathBuf,
}

impl Dirs {
    fn new(test: &str) -> Option<Self> {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let exe = std::env::current_exe().expect("test binary");
        let parent = exe.parent().expect("binary directory");
        let base = parent.join(format!(
            "bake-race-{test}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("ws")).expect("ws");
        std::fs::create_dir_all(base.join("out")).expect("out");
        let base = std::fs::canonicalize(&base).expect("canonical base");
        let dirs = Self { base };
        let temp = [std::env::temp_dir(), PathBuf::from("/tmp")]
            .map(|root| std::fs::canonicalize(&root).unwrap_or(root));
        if temp.iter().any(|root| dirs.base.starts_with(root)) {
            println!("skipping {test}: the test binary is under a temporary directory");
            return None;
        }
        Some(dirs)
    }

    fn ws(&self) -> PathBuf {
        self.base.join("ws")
    }

    fn out(&self) -> PathBuf {
        self.base.join("out")
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// `write` and `edit` for a session in `ws` under `workspace-write`.
fn file_tools(ws: &Path) -> (AgentTool, AgentTool) {
    let standing = StandingPermissions::new(
        SandboxMode::WorkspaceWrite,
        super::ApprovalPolicy::Ask,
        "workspace-write",
        ws,
        Some("race".into()),
    );
    let permissions = ToolPermissions::new(
        Arc::new(standing),
        Arc::new(NoApprover),
        Arc::new(UnavailableProvider::new(None)),
    );
    let tools = sandbox_file_tools(
        create_all_tool_definitions(ws, &ToolsOptions::default()),
        ws,
        &permissions,
    );
    let find = |name: &str| {
        tools
            .iter()
            .find(|definition| definition.name() == name)
            .expect("tool")
            .tool
            .clone()
    };
    (find("write"), find("edit"))
}

async fn call(tool: &AgentTool, args: Value) -> Result<String, String> {
    let args = match &tool.prepare_arguments {
        Some(prepare) => prepare(args).expect("prepared"),
        None => args,
    };
    (tool.execute)(
        "call-1".into(),
        args,
        None,
        AgentToolUpdateCallback::ignore(),
        bake_agent::ToolContext::detached(),
    )
    .await
    .map(|result| format!("{:?}", result.content))
}

/// Replaces `link` (a directory or a file) with a symbolic link to `to`.
fn swap(link: &Path, to: &Path) {
    let aside = link.with_extension("aside");
    std::fs::rename(link, aside).expect("move aside");
    std::os::unix::fs::symlink(to, link).expect("link");
}

/// Runs `case` through `openat2` and, on Linux, through the walk too.
async fn both_ways<F, Fut>(case: F)
where
    F: Fn(Way) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    case(Way::Openat2).await;
    if cfg!(target_os = "linux") {
        case(Way::Walk).await;
    }
}

/// How a case opens its targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Way {
    Openat2,
    Walk,
}

impl std::fmt::Display for Way {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl Way {
    /// Holds the walk for targets under `dirs` while the case runs.
    fn hold(self, dirs: &Dirs) -> Option<seam::NoOpenat2> {
        (self == Way::Walk).then(|| seam::NoOpenat2::hold(&dirs.base))
    }
}

#[tokio::test]
async fn a_directory_swapped_after_the_check_is_refused() {
    both_ways(|way| async move {
        let Some(dirs) = Dirs::new("dir") else {
            return;
        };
        let _way = way.hold(&dirs);
        let (write, edit) = file_tools(&dirs.ws());
        std::fs::create_dir(dirs.ws().join("sub")).expect("sub");
        let (sub, out) = (dirs.ws().join("sub"), dirs.out());
        seam::on_after_check(&dirs.ws().join("sub/f.txt"), move || swap(&sub, &out));
        let written = call(&write, json!({ "path": "sub/f.txt", "content": "x" })).await;
        assert_eq!(written.err().as_deref(), Some(DENIED), "{way}");
        assert!(!dirs.out().join("f.txt").exists(), "{way}");
        // New parent directories are made beneath the root too.
        std::fs::create_dir(dirs.ws().join("deep")).expect("deep");
        let (deep, out) = (dirs.ws().join("deep"), dirs.out());
        seam::on_after_check(&dirs.ws().join("deep/a/b/f.txt"), move || {
            swap(&deep, &out);
        });
        let written = call(&write, json!({ "path": "deep/a/b/f.txt", "content": "x" })).await;
        assert_eq!(written.err().as_deref(), Some(DENIED), "{way}");
        assert!(!dirs.out().join("a").exists(), "{way}");
        // edit makes no directories: its open alone must refuse the link.
        std::fs::create_dir_all(dirs.ws().join("old")).expect("old");
        std::fs::create_dir_all(dirs.out().join("victim")).expect("victim");
        std::fs::write(dirs.ws().join("old/f.txt"), "a").expect("file");
        std::fs::write(dirs.out().join("victim/f.txt"), "a").expect("victim file");
        let (old, victim) = (dirs.ws().join("old"), dirs.out().join("victim"));
        seam::on_after_check(&dirs.ws().join("old/f.txt"), move || swap(&old, &victim));
        let edits = json!({ "path": "old/f.txt", "edits": [{ "oldText": "a", "newText": "b" }] });
        let edited = call(&edit, edits).await;
        assert_eq!(edited.err().as_deref(), Some(DENIED), "{way}");
        let kept = std::fs::read_to_string(dirs.out().join("victim/f.txt")).expect("victim");
        assert_eq!(kept, "a", "{way}");
        // The writable root itself is opened without following a link.
        let (ws, out) = (dirs.ws(), dirs.out());
        seam::on_after_check(&dirs.ws().join("root.txt"), move || swap(&ws, &out));
        let written = call(&write, json!({ "path": "root.txt", "content": "x" })).await;
        assert_eq!(written.err().as_deref(), Some(DENIED), "{way}");
        assert!(!dirs.out().join("root.txt").exists(), "{way}");
    })
    .await;
}

#[tokio::test]
async fn a_file_swapped_after_the_check_is_refused() {
    both_ways(|way| async move {
        let Some(dirs) = Dirs::new("file") else {
            return;
        };
        let _way = way.hold(&dirs);
        let (write, edit) = file_tools(&dirs.ws());
        let (file, victim) = (dirs.ws().join("f.txt"), dirs.out().join("victim.txt"));
        std::fs::write(&file, "a").expect("file");
        std::fs::write(&victim, "a").expect("victim");
        let (link, to) = (file.clone(), victim.clone());
        seam::on_after_check(&file, move || swap(&link, &to));
        let written = call(&write, json!({ "path": "f.txt", "content": "x" })).await;
        assert_eq!(written.err().as_deref(), Some(DENIED), "{way}");
        assert_eq!(
            std::fs::read_to_string(&victim).expect("victim"),
            "a",
            "{way}"
        );
        // A dangling link swapped in is refused, not followed.
        std::fs::remove_file(&file).expect("unlink");
        std::fs::write(&file, "a").expect("file");
        let (link, to) = (file.clone(), dirs.out().join("new.txt"));
        seam::on_after_check(&file, move || {
            let _ = std::fs::remove_file(link.with_extension("aside"));
            swap(&link, &to);
        });
        let written = call(&write, json!({ "path": "f.txt", "content": "x" })).await;
        assert_eq!(written.err().as_deref(), Some(DENIED), "{way}");
        assert!(!dirs.out().join("new.txt").exists(), "{way}");
        // edit reads and writes through the checked target too.
        std::fs::remove_file(&file).expect("unlink");
        let _ = std::fs::remove_file(file.with_extension("aside"));
        std::fs::write(&file, "a").expect("file");
        let (link, to) = (file.clone(), victim.clone());
        seam::on_after_check(&file, move || swap(&link, &to));
        let edits = json!({ "path": "f.txt", "edits": [{ "oldText": "a", "newText": "b" }] });
        let edited = call(&edit, edits).await;
        assert_eq!(edited.err().as_deref(), Some(DENIED), "{way}");
        assert_eq!(
            std::fs::read_to_string(&victim).expect("victim"),
            "a",
            "{way}"
        );
    })
    .await;
}

// Pi's observable behavior stays: a link inside the workspace that points
// inside it writes its target, because the check resolved it first.
#[tokio::test]
async fn links_inside_the_workspace_still_write_their_targets() {
    both_ways(|way| async move {
        let Some(dirs) = Dirs::new("inside") else {
            return;
        };
        let _way = way.hold(&dirs);
        let (write, edit) = file_tools(&dirs.ws());
        std::fs::create_dir(dirs.ws().join("real")).expect("real");
        std::os::unix::fs::symlink(dirs.ws().join("real"), dirs.ws().join("dir")).expect("link");
        std::os::unix::fs::symlink("real/target.txt", dirs.ws().join("file")).expect("link");
        let written = call(&write, json!({ "path": "dir/new/f.txt", "content": "x" })).await;
        assert!(written.is_ok(), "{way}: {written:?}");
        assert_eq!(
            std::fs::read_to_string(dirs.ws().join("real/new/f.txt")).expect("written"),
            "x"
        );
        let written = call(&write, json!({ "path": "file", "content": "a" })).await;
        assert!(written.is_ok(), "{way}: {written:?}");
        let edits = json!({ "path": "file", "edits": [{ "oldText": "a", "newText": "b" }] });
        let edited = call(&edit, edits).await;
        assert!(edited.is_ok(), "{way}: {edited:?}");
        assert_eq!(
            std::fs::read_to_string(dirs.ws().join("real/target.txt")).expect("target"),
            "b"
        );
        assert!(
            std::fs::symlink_metadata(dirs.ws().join("file"))
                .expect("link")
                .file_type()
                .is_symlink(),
            "{way}"
        );
    })
    .await;
}

// The workspace root is pinned, canonical, when the session starts: a
// session opened through a link writes its workspace, and moving the
// workspace and leaving a link to elsewhere in its place does not redirect
// later writes.
#[tokio::test]
async fn a_moved_workspace_does_not_redirect_writes() {
    both_ways(|way| async move {
        let Some(dirs) = Dirs::new("moved") else {
            return;
        };
        let _way = way.hold(&dirs);
        std::os::unix::fs::symlink(&dirs.base, dirs.base.join("alias")).expect("alias");
        let (write, _) = file_tools(&dirs.base.join("alias/ws"));
        let written = call(&write, json!({ "path": "before.txt", "content": "x" })).await;
        assert!(written.is_ok(), "{way}: {written:?}");
        assert!(dirs.ws().join("before.txt").exists(), "{way}");
        std::fs::rename(dirs.ws(), dirs.base.join("ws.moved")).expect("move");
        std::os::unix::fs::symlink(dirs.out(), dirs.ws()).expect("link");
        let written = call(&write, json!({ "path": "f.txt", "content": "x" })).await;
        assert_eq!(written.err().as_deref(), Some(DENIED), "{way}");
        assert!(!dirs.out().join("f.txt").exists(), "{way}");
        assert!(!dirs.base.join("ws.moved/f.txt").exists(), "{way}");
    })
    .await;
}
