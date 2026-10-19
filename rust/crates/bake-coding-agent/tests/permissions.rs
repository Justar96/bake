//! Pi's `bash`, `write`, and `edit` tools under Bake's permission layer
//! (D24). Fixture tests derived from Bake's TypeScript specs, each naming
//! the case it follows:
//!
//! - `packages/fs/fs-sandbox/tests/{fs-sandbox,containment}.spec.ts`
//! - `packages/fs/tool-fs/tests/tools.spec.ts` (`sandbox escalation API`)
//! - `packages/shell/bash-sandbox/tests/{sandbox,foreground-timeout,partial-landlock}.spec.ts`
//! - `packages/shell/tool-bash/tests/tools.spec.ts` (`sandbox escalation`,
//!   `renderResult`)
//!
//! Bash runs through `bake_sandbox::testing::RecordingProvider` and local
//! doubles; real confinement is the backends' to test. The file tests keep
//! their workspace and the "outside" directory under Cargo's per-target
//! temporary directory, outside `/tmp` and the OS temporary directory that
//! `workspace-write` also allows, and skip with a note if the target
//! directory is itself under one. Tests that run bash skip where none
//! resolves (Windows without Git Bash).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bake_agent::{AgentTool, AgentToolResult, AgentToolUpdateCallback};
use bake_ai::{AbortController, AbortSignal, UserContentBlock};
use bake_coding_agent::permissions::approval::ApprovalFuture;
use bake_coding_agent::permissions::{
    ApprovalPolicy, ApprovalRequest, Approver, NoApprover, StandingPermissions, ToolPermissions,
    sandbox_file_tools, sandboxed_bash_tool,
};
use bake_coding_agent::tools::bash::BashToolOptions;
use bake_coding_agent::tools::bash::shell::get_shell_config;
use bake_coding_agent::tools::index::{ToolsOptions, create_all_tool_definitions};
use bake_sandbox::testing::RecordingProvider;
use bake_sandbox::{
    ConfineFuture, ConfinedCommand, Enforcement, EscalationOutcome, RunnerFailureRule, SandboxMode,
    SandboxPolicy, SandboxProvider, UnavailableProvider,
};
use serde_json::{Value, json};
use tokio::sync::Notify;

const WW: SandboxMode = SandboxMode::WorkspaceWrite;
const RO: SandboxMode = SandboxMode::ReadOnly;
const DFA: SandboxMode = SandboxMode::DangerFullAccess;

const HINT_COMMAND: &str = "[sandbox: escalation available — retry this exact command once with sandbox_permissions (the narrowest wider mode that suffices) + justification; the approval prompt asks the user]";
const HINT_OPERATION: &str = "[sandbox: escalation available — retry this exact operation once with sandbox_permissions (the narrowest wider mode that suffices) + justification; the approval prompt asks the user]";

/// A workspace and a sibling outside it, both outside every temporary
/// directory, removed on drop.
struct Dirs {
    base: PathBuf,
}

impl Dirs {
    /// `None`, with a note, when Cargo's target directory is itself under a
    /// directory `workspace-write` allows.
    fn new(test: &str) -> Option<Self> {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "bake-permissions-{test}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("ws")).expect("ws");
        std::fs::create_dir_all(base.join("out")).expect("out");
        let base = std::fs::canonicalize(&base).expect("canonical base");
        let base = strip_verbatim(base);
        let temp_roots = [std::env::temp_dir(), PathBuf::from("/tmp")]
            .map(|root| std::fs::canonicalize(&root).map_or(root, strip_verbatim));
        let dirs = Self { base };
        if temp_roots.iter().any(|root| dirs.base.starts_with(root)) {
            println!("skipping {test}: the Cargo target directory is under a temporary directory");
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

fn strip_verbatim(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|text| text.strip_prefix(r"\\?\")) {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => path,
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// Answers every approval with one outcome and counts the requests.
struct Fixed(EscalationOutcome, Arc<AtomicUsize>);

impl Approver for Fixed {
    fn request(&self, _request: ApprovalRequest, _cancel: Option<AbortSignal>) -> ApprovalFuture {
        self.1.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(self.0))
    }
}

fn permissions(
    root: &Path,
    mode: SandboxMode,
    answerer: Arc<dyn Approver>,
    provider: Arc<dyn SandboxProvider>,
) -> ToolPermissions {
    permissions_under(root, mode, ApprovalPolicy::Ask, answerer, provider)
}

fn permissions_under(
    root: &Path,
    mode: SandboxMode,
    approval: ApprovalPolicy,
    answerer: Arc<dyn Approver>,
    provider: Arc<dyn SandboxProvider>,
) -> ToolPermissions {
    let standing = StandingPermissions::new(mode, approval, "x", root, Some("sess-perm".into()));
    ToolPermissions::new(Arc::new(standing), answerer, provider)
}

/// Pi's tools for `root` with `write`, `edit`, and `bash` sandboxed.
fn sandboxed(root: &Path, permissions: &ToolPermissions) -> Vec<AgentTool> {
    let options = ToolsOptions {
        bash: Some(
            sandboxed_bash_tool(
                root,
                BashToolOptions {
                    spill_directory: Some(root.to_path_buf()),
                    ..BashToolOptions::default()
                },
                permissions,
            )
            .expect("bash"),
        ),
        ..ToolsOptions::default()
    };
    sandbox_file_tools(
        create_all_tool_definitions(root, &options),
        root,
        permissions,
    )
    .into_iter()
    .map(|definition| definition.tool)
    .collect()
}

fn tool<'a>(tools: &'a [AgentTool], name: &str) -> &'a AgentTool {
    tools
        .iter()
        .find(|tool| tool.name == name)
        .expect("tool exists")
}

async fn call(
    tool: &AgentTool,
    args: Value,
    signal: Option<AbortSignal>,
) -> Result<AgentToolResult, String> {
    let args = match &tool.prepare_arguments {
        Some(prepare) => prepare(args).expect("prepared"),
        None => args,
    };
    (tool.execute)(
        "call-1".to_owned(),
        args,
        signal,
        AgentToolUpdateCallback::ignore(),
        bake_agent::ToolContext::detached(),
    )
    .await
}

fn text(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn denial(mode: &str) -> String {
    format!("[sandbox: file access denied under {mode} mode]\n{HINT_OPERATION}")
}

fn headless(root: &Path, mode: SandboxMode) -> ToolPermissions {
    permissions(
        root,
        mode,
        Arc::new(NoApprover),
        Arc::new(UnavailableProvider::new(None)),
    )
}

// ---------------------------------------------------------------------------
// write and edit

/// The permissions of each composition whose tools offer no escalation:
/// full access, the `never` policy, and no answerer.
fn offering_nothing(root: &Path) -> [(&'static str, ToolPermissions); 3] {
    let provider = || Arc::new(UnavailableProvider::new(None)) as Arc<dyn SandboxProvider>;
    [
        ("danger-full-access", headless(root, DFA)),
        (
            "never",
            permissions_under(
                root,
                WW,
                ApprovalPolicy::Never,
                Arc::new(NoApprover),
                provider(),
            ),
        ),
        ("no answerer", headless(root, WW).without_escalation()),
    ]
}

// tool-fs.spec: "advertises the closed target vocabulary on write and edit
// under a confining backend", narrowed to the modes an approval could
// grant; Pi's descriptions and parameters otherwise.
#[test]
fn file_tools_add_the_escalation_parameters() {
    let root = std::env::temp_dir();
    let pi_tool = |name: &str| {
        create_all_tool_definitions(&root, &ToolsOptions::default())
            .into_iter()
            .find(|definition| definition.name() == name)
            .expect("pi tool")
            .tool
    };
    let offered: [(SandboxMode, &[&str]); 2] = [
        (RO, &["workspace-write", "danger-full-access"]),
        (WW, &["danger-full-access"]),
    ];
    for (mode, targets) in offered {
        let tools = sandboxed(&root, &headless(&root, mode));
        for name in ["write", "edit"] {
            let wrapped = tool(&tools, name);
            let pi = pi_tool(name);
            assert_eq!(wrapped.description, pi.description);
            let mut expected = pi.parameters.clone();
            let properties = expected["properties"].as_object_mut().expect("properties");
            properties.insert(
                "sandbox_permissions".into(),
                json!({
                    "type": "string",
                    "enum": targets,
                    "description": "One retry after a sandbox denial; asks the user.",
                }),
            );
            properties.insert(
                "justification".into(),
                json!({
                    "type": "string",
                    "description": "One sentence for the user; required with sandbox_permissions.",
                }),
            );
            assert_eq!(
                serde_json::to_string(&wrapped.parameters).expect("json"),
                serde_json::to_string(&expected).expect("json"),
                "{mode} {name}"
            );
            assert_eq!(wrapped.constrained_sampling, pi.constrained_sampling);
        }
    }
    // Where no escalation is reachable, write and edit are Pi's schemas.
    for (case, permissions) in offering_nothing(&root) {
        let tools = sandboxed(&root, &permissions);
        for name in ["write", "edit"] {
            let pi = pi_tool(name);
            assert_eq!(
                serde_json::to_string(&tool(&tools, name).parameters).expect("json"),
                serde_json::to_string(&pi.parameters).expect("json"),
                "{case} {name}"
            );
            assert_eq!(tool(&tools, name).description, pi.description);
        }
    }
    let tools = sandboxed(&root, &headless(&root, WW));
    // read, grep, find, and ls are Pi's.
    for name in ["read", "grep", "find", "ls"] {
        let pi = create_all_tool_definitions(&root, &ToolsOptions::default())
            .into_iter()
            .find(|definition| definition.name() == name)
            .expect("pi tool")
            .tool;
        assert_eq!(tool(&tools, name).parameters, pi.parameters);
    }
}

// fs-sandbox.spec (workspace-write containment): "a write under the
// workspace lands", "an absolute path outside the workspace is denied, no
// file created", "a `..` traversal out of the workspace is denied", "an edit
// outside the workspace is denied; the original is untouched", "an edit
// inside the workspace lands"; tool-fs.spec: "a denied write maps to the
// shared marker plus the escalation hint (isError)".
#[tokio::test]
async fn workspace_write_contains_writes_and_edits() {
    let Some(dirs) = Dirs::new("contain") else {
        return;
    };
    let tools = sandboxed(&dirs.ws(), &headless(&dirs.ws(), WW));
    let (write, edit) = (tool(&tools, "write"), tool(&tools, "edit"));

    let inside = call(
        write,
        json!({ "path": "nested/ok.txt", "content": "inside" }),
        None,
    )
    .await
    .expect("inside");
    assert_eq!(text(&inside), "Successfully wrote to nested/ok.txt");
    assert_eq!(
        std::fs::read_to_string(dirs.ws().join("nested/ok.txt")).expect("read"),
        "inside"
    );

    let escape = dirs.out().join("newdir").join("escape.txt");
    let refused = call(
        write,
        json!({ "path": escape.to_string_lossy(), "content": "x" }),
        None,
    )
    .await;
    assert_eq!(refused.err(), Some(denial("workspace-write")));
    assert!(!dirs.out().join("newdir").exists(), "no directory was made");

    let traversal = call(
        write,
        json!({ "path": "../sibling-escape.txt", "content": "x" }),
        None,
    )
    .await;
    assert_eq!(traversal.err(), Some(denial("workspace-write")));
    assert!(!dirs.base.join("sibling-escape.txt").exists());

    let original = dirs.out().join("file.txt");
    std::fs::write(&original, "original").expect("original");
    let refused = call(
        edit,
        json!({ "path": original.to_string_lossy(), "edits": [{ "oldText": "original", "newText": "x" }] }),
        None,
    )
    .await;
    assert_eq!(refused.err(), Some(denial("workspace-write")));
    assert_eq!(
        std::fs::read_to_string(&original).expect("read"),
        "original"
    );
    // Refused before the file is checked: a missing target reads the same.
    let missing = call(
        edit,
        json!({ "path": dirs.out().join("missing.txt").to_string_lossy(), "edits": [{ "oldText": "a", "newText": "b" }] }),
        None,
    )
    .await;
    assert_eq!(missing.err(), Some(denial("workspace-write")));

    std::fs::write(dirs.ws().join("edit.txt"), "original").expect("edit.txt");
    let edited = call(
        edit,
        json!({ "path": "edit.txt", "edits": [{ "oldText": "original", "newText": "changed" }] }),
        None,
    )
    .await
    .expect("edited");
    assert_eq!(
        text(&edited),
        "Successfully replaced 1 block(s) in edit.txt."
    );
    assert_eq!(
        std::fs::read_to_string(dirs.ws().join("edit.txt")).expect("read"),
        "changed"
    );
}

// fs-sandbox.spec: "a write to the platform temp area lands (parity with the
// bash runner grant)".
#[tokio::test]
async fn workspace_write_allows_the_temporary_directory() {
    let Some(dirs) = Dirs::new("temp") else {
        return;
    };
    let tools = sandboxed(&dirs.ws(), &headless(&dirs.ws(), WW));
    let target =
        std::env::temp_dir().join(format!("bake-permissions-temp-{}.txt", std::process::id()));
    call(
        tool(&tools, "write"),
        json!({ "path": target.to_string_lossy(), "content": "temp" }),
        None,
    )
    .await
    .expect("temp write");
    assert_eq!(std::fs::read_to_string(&target).expect("read"), "temp");
    let _ = std::fs::remove_file(&target);
}

// Bake's own: where no wider mode is reachable, a denied mutation carries
// the denial marker alone, with no hint naming a parameter the tool lacks.
#[tokio::test]
async fn a_denial_hints_only_at_an_offered_escalation() {
    let Some(dirs) = Dirs::new("no-hint") else {
        return;
    };
    let escape = dirs.out().join("escape.txt");
    for (case, permissions) in offering_nothing(&dirs.ws()) {
        if case == "danger-full-access" {
            continue;
        }
        let tools = sandboxed(&dirs.ws(), &permissions);
        for (name, args) in [
            (
                "write",
                json!({ "path": escape.to_string_lossy(), "content": "x" }),
            ),
            (
                "edit",
                json!({ "path": escape.to_string_lossy(), "edits": [{ "oldText": "a", "newText": "b" }] }),
            ),
        ] {
            let refused = call(tool(&tools, name), args, None).await;
            assert_eq!(
                refused.err().as_deref(),
                Some("[sandbox: file access denied under workspace-write mode]"),
                "{case} {name}"
            );
        }
    }
    assert!(!escape.exists());
}

// fs-sandbox.spec (read-only): "denies write, leaving no file on disk" and
// "denies edit of an existing file (the content is unchanged)".
#[tokio::test]
async fn read_only_refuses_every_mutation() {
    let Some(dirs) = Dirs::new("read-only") else {
        return;
    };
    let tools = sandboxed(&dirs.ws(), &headless(&dirs.ws(), RO));
    let refused = call(
        tool(&tools, "write"),
        json!({ "path": "denied.txt", "content": "x" }),
        None,
    )
    .await;
    assert_eq!(refused.err(), Some(denial("read-only")));
    assert!(!dirs.ws().join("denied.txt").exists());
    std::fs::write(dirs.ws().join("file.txt"), "original").expect("file");
    let refused = call(
        tool(&tools, "edit"),
        json!({ "path": "file.txt", "edits": [{ "oldText": "original", "newText": "changed" }] }),
        None,
    )
    .await;
    assert_eq!(refused.err(), Some(denial("read-only")));
    assert_eq!(
        std::fs::read_to_string(dirs.ws().join("file.txt")).expect("read"),
        "original"
    );
    // Reads pass through ("allows reads (every mode permits reading)").
    let read = call(tool(&tools, "read"), json!({ "path": "file.txt" }), None)
        .await
        .expect("read");
    assert_eq!(text(&read), "original");
}

// fs-sandbox.spec (danger-full-access): "writes anywhere, unfenced".
#[tokio::test]
async fn danger_full_access_writes_anywhere() {
    let Some(dirs) = Dirs::new("danger") else {
        return;
    };
    let tools = sandboxed(&dirs.ws(), &headless(&dirs.ws(), DFA));
    let target = dirs.out().join("free.txt");
    call(
        tool(&tools, "write"),
        json!({ "path": target.to_string_lossy(), "content": "free" }),
        None,
    )
    .await
    .expect("free");
    assert_eq!(std::fs::read_to_string(&target).expect("read"), "free");
}

// fs-sandbox.spec (the per-call policy override): "a workspace-write stamp
// on a read-only default lets a contained write land for that call only";
// tool-fs.spec: "an approved escalation stamps the granted mode onto that
// write" and "a rejected escalation fails closed with its own text and never
// mutates".
#[tokio::test]
async fn an_approved_escalation_widens_one_mutation() {
    let Some(dirs) = Dirs::new("escalate") else {
        return;
    };
    let asked = Arc::new(AtomicUsize::new(0));
    let granting = permissions(
        &dirs.ws(),
        RO,
        Arc::new(Fixed(EscalationOutcome::AllowedOnce, Arc::clone(&asked))),
        Arc::new(UnavailableProvider::new(None)),
    );
    let tools = sandboxed(&dirs.ws(), &granting);
    let write = tool(&tools, "write");
    call(
        write,
        json!({ "path": "escalated.txt", "content": "granted", "sandbox_permissions": "workspace-write", "justification": "the test needs it" }),
        None,
    )
    .await
    .expect("granted");
    assert_eq!(
        std::fs::read_to_string(dirs.ws().join("escalated.txt")).expect("read"),
        "granted"
    );
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    // A neighbouring plain call still runs under read-only.
    let plain = call(write, json!({ "path": "plain.txt", "content": "x" }), None).await;
    assert_eq!(plain.err(), Some(denial("read-only")));
    // The grant is workspace-write: outside stays refused.
    let outside = call(
        write,
        json!({ "path": dirs.out().join("x.txt").to_string_lossy(), "content": "x", "sandbox_permissions": "workspace-write", "justification": "why" }),
        None,
    )
    .await;
    assert_eq!(outside.err(), Some(denial("workspace-write")));

    std::fs::write(dirs.ws().join("a.txt"), "x").expect("a.txt");
    let rejecting = permissions(
        &dirs.ws(),
        WW,
        Arc::new(Fixed(
            EscalationOutcome::Rejected,
            Arc::new(AtomicUsize::new(0)),
        )),
        Arc::new(UnavailableProvider::new(None)),
    );
    let tools = sandboxed(&dirs.ws(), &rejecting);
    let refused = call(
        tool(&tools, "edit"),
        json!({ "path": "a.txt", "edits": [{ "oldText": "x", "newText": "y" }], "sandbox_permissions": "danger-full-access", "justification": "the test needs it" }),
        None,
    )
    .await;
    assert_eq!(
        refused.err().as_deref(),
        Some("the user rejected escalating this operation to \"danger-full-access\"")
    );
    assert_eq!(
        std::fs::read_to_string(dirs.ws().join("a.txt")).expect("read"),
        "x"
    );
    // Pi's input check runs before the escalation, so nothing is asked.
    let invalid = call(
        tool(&tools, "edit"),
        json!({ "path": "a.txt", "edits": [], "sandbox_permissions": "danger-full-access", "justification": "why" }),
        None,
    )
    .await;
    assert_eq!(
        invalid.err().as_deref(),
        Some("Edit tool input is invalid. edits must contain at least one replacement.")
    );
}

// fs-sandbox.spec: "a symlinked directory inside the workspace pointing OUT
// is denied" and "a NEW file created under a symlinked-out directory is
// denied (deepest-ancestor realpath)".
#[cfg(unix)]
#[tokio::test]
async fn links_out_of_the_workspace_are_refused() {
    let Some(dirs) = Dirs::new("links") else {
        return;
    };
    std::os::unix::fs::symlink(dirs.out(), dirs.ws().join("link")).expect("link");
    let tools = sandboxed(&dirs.ws(), &headless(&dirs.ws(), WW));
    let write = tool(&tools, "write");
    let refused = call(write, json!({ "path": "link/f.txt", "content": "x" }), None).await;
    assert_eq!(refused.err(), Some(denial("workspace-write")));
    assert!(!dirs.out().join("f.txt").exists());
    let refused = call(
        write,
        json!({ "path": "link/newdir/deep.txt", "content": "x" }),
        None,
    )
    .await;
    assert_eq!(refused.err(), Some(denial("workspace-write")));
    assert!(!dirs.out().join("newdir").exists());
}

// Not a TypeScript case: the TypeScript fence renames a temporary file over
// the checked name, so a dangling link is replaced, never followed. Pi's
// in-place write follows it, so the guard must judge the file the link
// names, however it is spelled and however many links lead there.
#[cfg(unix)]
#[tokio::test]
async fn dangling_links_are_judged_by_where_they_land() {
    let Some(dirs) = Dirs::new("dangling") else {
        return;
    };
    let ws = dirs.ws();
    let symlink = |target: &Path, name: &str| {
        std::os::unix::fs::symlink(target, ws.join(name)).expect("link");
    };
    symlink(&dirs.out().join("abs.txt"), "abs");
    let up = Path::new("..").join(dirs.out().file_name().expect("out name"));
    symlink(&up.join("rel.txt"), "rel");
    symlink(&dirs.out().join("chained.txt"), "second");
    symlink(Path::new("second"), "first");
    symlink(Path::new("loop-b"), "loop-a");
    symlink(Path::new("loop-a"), "loop-b");
    symlink(&ws.join("inside.txt"), "in");
    let tools = sandboxed(&ws, &headless(&ws, WW));
    let write = tool(&tools, "write");
    for (path, landed) in [
        ("abs", "abs.txt"),
        ("rel", "rel.txt"),
        ("first", "chained.txt"),
    ] {
        let refused = call(write, json!({ "path": path, "content": "x" }), None).await;
        assert_eq!(refused.err(), Some(denial("workspace-write")), "{path}");
        assert!(!dirs.out().join(landed).exists(), "{path}");
    }
    // Pi's own realpath step reports a loop before the guard runs; the
    // guard's walk refuses one too.
    let refused = call(write, json!({ "path": "loop-a", "content": "x" }), None).await;
    assert!(refused.is_err());
    use bake_coding_agent::permissions::fs::write_target;
    assert_eq!(write_target(&ws.join("loop-a")), None);
    assert_eq!(
        write_target(&ws.join("first")),
        Some(dirs.out().join("chained.txt"))
    );
    let written = call(write, json!({ "path": "in", "content": "x" }), None).await;
    assert!(written.is_ok(), "{:?}", written.err());
    assert_eq!(
        std::fs::read_to_string(ws.join("inside.txt")).expect("inside"),
        "x"
    );
    let edit = tool(&tools, "edit");
    let refused = call(
        edit,
        json!({ "path": "abs", "edits": [{ "oldText": "a", "newText": "b" }] }),
        None,
    )
    .await;
    assert_eq!(refused.err(), Some(denial("workspace-write")));
    assert!(!dirs.out().join("abs.txt").exists());
}

// fs-sandbox.spec: "mutates the freshly checked identity, not a stale
// outside targetKey (TOCTOU direction)". The link points inside when the
// call starts and is swapped out after the call holds its mutation queue,
// just before the check; the check reads the link as it then is.
#[cfg(unix)]
#[tokio::test]
async fn a_link_swapped_before_the_check_is_judged_fresh() {
    let Some(dirs) = Dirs::new("swap") else {
        return;
    };
    std::fs::create_dir_all(dirs.ws().join("real")).expect("real");
    let link = dirs.ws().join("link");
    std::os::unix::fs::symlink(dirs.ws().join("real"), &link).expect("link");
    let policy = SandboxPolicy::new(WW, dirs.ws());
    let check = bake_coding_agent::permissions::fs::containment_guard(policy, true);
    let swap_to = dirs.out();
    let guard: bake_coding_agent::tools::definition::MutationGuard = Arc::new(move |path| {
        std::fs::remove_file(&link).map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&swap_to, &link).map_err(|error| error.to_string())?;
        check(path)
    });
    let refused = bake_coding_agent::tools::write::execute_guarded(
        dirs.ws(),
        json!({ "path": "link/f.txt", "content": "x" }),
        None,
        Some(guard),
    )
    .await;
    assert_eq!(refused.err(), Some(denial("workspace-write")));
    assert!(!dirs.out().join("f.txt").exists());
    assert!(!dirs.ws().join("real/f.txt").exists());
}

// ---------------------------------------------------------------------------
// bash

fn have_bash(test: &str) -> bool {
    match get_shell_config(None) {
        Ok(_) => true,
        Err(error) => {
            println!("skipping {test}: no bash found ({error})");
            false
        }
    }
}

/// Runs the caller's argv unchanged, reporting `rules` and `signatures`.
struct Dialect {
    rules: Vec<RunnerFailureRule>,
    signatures: Vec<String>,
}

impl SandboxProvider for Dialect {
    fn name(&self) -> &str {
        "dialect"
    }

    fn confine<'a>(
        &'a self,
        argv: &'a [OsString],
        _policy: &'a SandboxPolicy,
    ) -> ConfineFuture<'a> {
        let mut parts = argv.iter().cloned();
        let program = parts.next().unwrap_or_default();
        let command = ConfinedCommand::new(program, parts.collect(), Enforcement::Full)
            .with_denial_signatures(self.signatures.clone())
            .with_runner_failure_rules(self.rules.clone());
        Box::pin(async move { Ok(command) })
    }
}

/// Wraps the argv with a runner that does not exist.
struct MissingRunner;

impl SandboxProvider for MissingRunner {
    fn name(&self) -> &str {
        "missing"
    }

    fn confine<'a>(
        &'a self,
        argv: &'a [OsString],
        _policy: &'a SandboxPolicy,
    ) -> ConfineFuture<'a> {
        let command = ConfinedCommand::new(missing_runner(), argv.to_vec(), Enforcement::Full);
        Box::pin(async move { Ok(command) })
    }
}

fn missing_runner() -> PathBuf {
    std::env::temp_dir()
        .join("bake-permissions-no-such-dir")
        .join("bake-runner")
}

/// Never finishes confining; says when it starts and when it is dropped.
struct PendingProvider {
    entered: Arc<Notify>,
    dropped: Arc<AtomicUsize>,
}

struct DropFlag(Arc<AtomicUsize>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl SandboxProvider for PendingProvider {
    fn name(&self) -> &str {
        "pending"
    }

    fn confine<'a>(
        &'a self,
        _argv: &'a [OsString],
        _policy: &'a SandboxPolicy,
    ) -> ConfineFuture<'a> {
        let flag = DropFlag(Arc::clone(&self.dropped));
        let entered = Arc::clone(&self.entered);
        Box::pin(async move {
            let _flag = flag;
            entered.notify_one();
            std::future::pending().await
        })
    }
}

/// Fails a call that has not settled within a minute, instead of hanging.
async fn within_a_minute(
    call: impl std::future::Future<Output = Result<AgentToolResult, String>>,
) -> Result<AgentToolResult, String> {
    tokio::time::timeout(std::time::Duration::from_secs(60), call)
        .await
        .unwrap_or_else(|_| Err("the call did not settle within a minute".into()))
}

fn bash_permissions(
    root: &Path,
    mode: SandboxMode,
    provider: Arc<dyn SandboxProvider>,
) -> ToolPermissions {
    permissions(root, mode, Arc::new(NoApprover), provider)
}

fn temp_root(name: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "bake-permissions-bash-{name}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("root");
    root
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// tool-bash.spec: "advertises the sandbox fields and validates their
// pairing" (the description and parameters of a confining composition),
// with only the modes an approval could grant.
#[test]
fn bash_adds_the_sandbox_paragraph_and_parameters() {
    let root = std::env::temp_dir();
    let pi = bake_coding_agent::tools::bash::create_bash_tool(&root, BashToolOptions::default());
    let denial_sentences = " A file sandbox may block commands; trying is safe. `[sandbox: file access denied under <mode> mode]` is a policy denial: do not work around it.";
    // Without a reachable mode: the denial sentences and Pi's parameters.
    for (case, permissions) in offering_nothing(&root) {
        let tools = sandboxed(&root, &permissions);
        let bash = tool(&tools, "bash");
        assert_eq!(
            bash.description,
            format!("{}{denial_sentences}", pi.description),
            "{case}"
        );
        assert_eq!(
            serde_json::to_string(&bash.parameters).expect("json"),
            serde_json::to_string(&pi.parameters).expect("json"),
            "{case}"
        );
    }
    let tools = sandboxed(&root, &headless(&root, RO));
    assert_eq!(
        tool(&tools, "bash").parameters["properties"]["sandbox_permissions"]["enum"],
        json!(["workspace-write", "danger-full-access"])
    );
    let tools = sandboxed(&root, &headless(&root, WW));
    let bash = tool(&tools, "bash");
    assert_eq!(
        bash.description,
        format!(
            "{}{denial_sentences} If a wider mode would help, retry the same command once this turn with the narrowest `sandbox_permissions` and a `justification`; that asks the user, so do not ask in chat. Request wider access up front only after this session was denied the same access. A rejection is final for that command.",
            pi.description
        )
    );
    assert_eq!(
        serde_json::to_string(&bash.parameters).expect("json"),
        serde_json::to_string(&json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute" },
                "timeout": { "type": "number", "description": "Timeout in seconds (optional, no default timeout)" },
                "sandbox_permissions": {
                    "type": "string",
                    "enum": ["danger-full-access"],
                    "description": "For retrying a denied command.",
                },
                "justification": { "type": "string", "description": "One sentence for the user." },
            },
        }))
        .expect("json")
    );
    assert_eq!(bash.output_schema, pi.output_schema);
}

// bash-sandbox.spec: "hands the provider the exact bash argv and the
// per-call policy, and runs the returned argv"; "reports a real permission
// failure as a sandbox denial with the mode it ran under"; tool-bash.spec
// renderResult: "reports sandbox denials before exit status and hints only
// when escalation is advertised".
#[tokio::test]
async fn a_denied_bash_write_shows_the_markers() {
    if !have_bash("a_denied_bash_write_shows_the_markers") {
        return;
    }
    let root = temp_root("denied");
    let _cleanup = Cleanup(root.clone());
    let provider = Arc::new(RecordingProvider::with_denial_signatures([
        "Read-only file system".to_owned(),
    ]));
    let tools = sandboxed(
        &root,
        &bash_permissions(&root, WW, Arc::clone(&provider) as Arc<dyn SandboxProvider>),
    );
    let bash = tool(&tools, "bash");
    let command = "printf 'touch: x: Read-only file system' >&2; exit 1";
    let result = call(bash, json!({ "command": command }), None)
        .await
        .expect("a failed command is a result");
    assert!(result.is_error);
    assert_eq!(
        text(&result),
        format!(
            "touch: x: Read-only file system\n[sandbox: file access denied under workspace-write mode]\n{HINT_COMMAND}\n\nCommand exited with code 1"
        )
    );
    let calls = provider.calls();
    assert_eq!(calls.len(), 1);
    let shell = get_shell_config(None).expect("shell");
    let mut argv: Vec<OsString> = vec![shell.shell.into_os_string()];
    argv.extend(shell.args.iter().map(OsString::from));
    argv.push(command.into());
    assert_eq!(calls[0].0, argv);
    assert_eq!(
        calls[0].1,
        SandboxPolicy::new(WW, bake_sandbox::canonical_path(&root)).with_session_id("sess-perm")
    );

    // A terminated line gains no extra newline.
    let result = call(
        bash,
        json!({ "command": "printf 'cannot touch: Read-only file system\\n' >&2; exit 2" }),
        None,
    )
    .await
    .expect("result");
    assert_eq!(
        text(&result),
        format!(
            "cannot touch: Read-only file system\n[sandbox: file access denied under workspace-write mode]\n{HINT_COMMAND}\n\nCommand exited with code 2"
        )
    );
    // classifyDenial: "never classifies a clean exit ... as a denial", and a
    // failure in another dialect is not a denial.
    let clean = call(
        bash,
        json!({ "command": "printf 'Read-only file system' >&2" }),
        None,
    )
    .await
    .expect("clean");
    assert_eq!(text(&clean), "Read-only file system");
    let other = call(
        bash,
        json!({ "command": "printf 'Permission denied' >&2; exit 1" }),
        None,
    )
    .await
    .expect("other");
    assert_eq!(
        text(&other),
        "Permission denied\n\nCommand exited with code 1"
    );

    // renderResult: "hints only when escalation is advertised". Under
    // `never`, the denial stands alone.
    let tools = sandboxed(
        &root,
        &permissions_under(
            &root,
            WW,
            ApprovalPolicy::Never,
            Arc::new(NoApprover),
            Arc::clone(&provider) as Arc<dyn SandboxProvider>,
        ),
    );
    let result = call(tool(&tools, "bash"), json!({ "command": command }), None)
        .await
        .expect("a failed command is a result");
    assert_eq!(
        text(&result),
        "touch: x: Read-only file system\n[sandbox: file access denied under workspace-write mode]\n\nCommand exited with code 1"
    );
}

// bash-sandbox.spec: "a foreground runner failure throws the fail-closed
// error, never a task result" and "a foreground runner failure outranks
// denial: runner error text may contain denial words"; partial-landlock.spec:
// "keeps a Landlock fatal line at exit %i as insufficient runner-failure
// evidence" (an exit code the rule does not allow).
#[tokio::test]
async fn a_runner_failure_outranks_a_denial() {
    if !have_bash("a_runner_failure_outranks_a_denial") {
        return;
    }
    let root = temp_root("runner");
    let _cleanup = Cleanup(root.clone());
    let provider = Arc::new(Dialect {
        rules: vec![RunnerFailureRule {
            allowed_exit_codes: vec![1],
            fatal_signatures: vec!["bwrap: ".into()],
            informational_lines: vec!["bwrap: note".into()],
        }],
        signatures: vec!["Permission denied".into()],
    });
    let tools = sandboxed(&root, &bash_permissions(&root, WW, provider));
    let bash = tool(&tools, "bash");
    let failed = call(
        bash,
        json!({ "command": "printf 'bwrap: note\\nbwrap: setting up uid map: Permission denied\\n' >&2; exit 1" }),
        None,
    )
    .await;
    assert_eq!(
        failed.err().as_deref(),
        Some(
            "sandbox mode \"workspace-write\" is requested but no sandbox backend is usable on this host; refusing to run the command unconfined. Install bubblewrap or run a Landlock-enforcing kernel (Linux), ensure sandbox-exec is usable (macOS), or ensure the ACL restricted-token runner can start (Windows) — otherwise switch the consumer to danger-full-access. Runner failure: bwrap: setting up uid map: Permission denied"
        )
    );
    let child = call(
        bash,
        json!({ "command": "printf 'bwrap: x: Permission denied' >&2; exit 2" }),
        None,
    )
    .await
    .expect("not a runner failure");
    assert_eq!(
        text(&child),
        format!(
            "bwrap: x: Permission denied\n[sandbox: file access denied under workspace-write mode]\n{HINT_COMMAND}\n\nCommand exited with code 2"
        )
    );
}

// partial-landlock.spec: "classifies a %s configured runner through the
// direct spawn error channel".
#[tokio::test]
async fn a_runner_that_cannot_start_fails_closed() {
    if !have_bash("a_runner_that_cannot_start_fails_closed") {
        return;
    }
    let root = temp_root("missing-runner");
    let _cleanup = Cleanup(root.clone());
    let tools = sandboxed(&root, &bash_permissions(&root, WW, Arc::new(MissingRunner)));
    let failed = call(tool(&tools, "bash"), json!({ "command": "true" }), None).await;
    let message = failed.expect_err("refused");
    assert!(
        message.starts_with("sandbox mode \"workspace-write\" is requested but no sandbox backend is usable on this host; refusing to run the command unconfined."),
        "{message}"
    );
    assert!(
        message.ends_with(&format!(
            " Runner failure: Error: spawn {} ENOENT",
            missing_runner().display()
        )),
        "{message}"
    );
}

// bash-sandbox.spec (fail closed): "propagates the provider's structured
// SANDBOX_UNAVAILABLE"; the command never runs.
#[tokio::test]
async fn an_unavailable_sandbox_runs_nothing() {
    if !have_bash("an_unavailable_sandbox_runs_nothing") {
        return;
    }
    let root = temp_root("unavailable");
    let _cleanup = Cleanup(root.clone());
    let provider = Arc::new(UnavailableProvider::new(Some("no backend here".into())));
    let tools = sandboxed(&root, &bash_permissions(&root, RO, provider));
    let failed = call(
        tool(&tools, "bash"),
        json!({ "command": "touch ran.txt" }),
        None,
    )
    .await;
    assert_eq!(
        failed.err().as_deref(),
        Some(
            "sandbox mode \"read-only\" is requested but no sandbox backend is usable on this host; refusing to run the command unconfined. Install bubblewrap or run a Landlock-enforcing kernel (Linux), ensure sandbox-exec is usable (macOS), or ensure the ACL restricted-token runner can start (Windows) — otherwise switch the consumer to danger-full-access. Runner failure: no backend here"
        )
    );
    assert!(!root.join("ran.txt").exists());
}

// bash-sandbox.spec (danger-full-access): "runs unwrapped: the provider is
// never consulted"; "escalating to danger-full-access bypasses the provider
// entirely"; tool-bash.spec: "runs a granted foreground ... call under the
// approved mode".
#[tokio::test]
async fn full_access_and_grants_choose_the_run() {
    if !have_bash("full_access_and_grants_choose_the_run") {
        return;
    }
    let root = temp_root("grants");
    let _cleanup = Cleanup(root.clone());
    let provider = Arc::new(RecordingProvider::default());
    let open = sandboxed(
        &root,
        &bash_permissions(
            &root,
            DFA,
            Arc::clone(&provider) as Arc<dyn SandboxProvider>,
        ),
    );
    let ran = call(tool(&open, "bash"), json!({ "command": "printf ok" }), None)
        .await
        .expect("ran");
    assert_eq!(text(&ran), "ok");
    assert!(provider.calls().is_empty());

    let asked = Arc::new(AtomicUsize::new(0));
    let granting = permissions(
        &root,
        RO,
        Arc::new(Fixed(EscalationOutcome::AllowedOnce, Arc::clone(&asked))),
        Arc::clone(&provider) as Arc<dyn SandboxProvider>,
    );
    let tools = sandboxed(&root, &granting);
    let bash = tool(&tools, "bash");
    call(
        bash,
        json!({ "command": "true", "sandbox_permissions": "workspace-write", "justification": "the command needs workspace writes" }),
        None,
    )
    .await
    .expect("granted");
    call(bash, json!({ "command": "true" }), None)
        .await
        .expect("plain");
    call(
        bash,
        json!({ "command": "true", "sandbox_permissions": "danger-full-access", "justification": "why" }),
        None,
    )
    .await
    .expect("full");
    assert_eq!(provider.modes(), [WW, RO]);
    assert_eq!(asked.load(Ordering::SeqCst), 2);

    // tool-bash.spec: "maps an approval rejected to its distinct failure".
    let rejecting = permissions(
        &root,
        RO,
        Arc::new(Fixed(
            EscalationOutcome::Rejected,
            Arc::new(AtomicUsize::new(0)),
        )),
        Arc::clone(&provider) as Arc<dyn SandboxProvider>,
    );
    let tools = sandboxed(&root, &rejecting);
    let refused = call(
        tool(&tools, "bash"),
        json!({ "command": "touch ran.txt", "sandbox_permissions": "workspace-write", "justification": "why" }),
        None,
    )
    .await;
    assert_eq!(
        refused.err().as_deref(),
        Some("the user rejected escalating this command to \"workspace-write\"")
    );
    assert_eq!(provider.modes(), [WW, RO]);
    assert!(!root.join("ran.txt").exists());
}

// bash-sandbox.spec (the provider hand-off): "cancels run while confinement
// is pending without spawning"; foreground-timeout.spec: "preserves
// upstream cancellation when it wins during preparation".
#[tokio::test]
async fn an_abort_while_confining_spawns_nothing() {
    if !have_bash("an_abort_while_confining_spawns_nothing") {
        return;
    }
    let root = temp_root("pending-abort");
    let _cleanup = Cleanup(root.clone());
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(PendingProvider {
        entered: Arc::clone(&entered),
        dropped: Arc::clone(&dropped),
    });
    let tools = sandboxed(&root, &bash_permissions(&root, WW, provider));
    let controller = AbortController::new();
    let (result, ()) = tokio::join!(
        within_a_minute(call(
            tool(&tools, "bash"),
            json!({ "command": "touch ran.txt" }),
            Some(controller.signal())
        )),
        async {
            entered.notified().await;
            controller.abort();
        }
    );
    assert_eq!(result.err().as_deref(), Some("Command aborted"));
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "confinement was cancelled"
    );
    assert!(!root.join("ran.txt").exists());
}

// foreground-timeout.spec: "times out unresolved preparation and prevents
// late success from spawning". Paused time advances to the deadline once
// the call can only wait.
#[tokio::test(start_paused = true)]
async fn the_timeout_covers_confinement() {
    if !have_bash("the_timeout_covers_confinement") {
        return;
    }
    let root = temp_root("pending-timeout");
    let _cleanup = Cleanup(root.clone());
    let dropped = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(PendingProvider {
        entered: Arc::new(Notify::new()),
        dropped: Arc::clone(&dropped),
    });
    let tools = sandboxed(&root, &bash_permissions(&root, WW, provider));
    let result = within_a_minute(call(
        tool(&tools, "bash"),
        json!({ "command": "touch ran.txt", "timeout": 5 }),
        None,
    ))
    .await;
    assert_eq!(
        result.err().as_deref(),
        Some("Command timed out after 5 seconds")
    );
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert!(!root.join("ran.txt").exists());
}

// tool-bash.spec: "rejects an explicit mode with blank justification %j
// before execution" and "runs without escalation for justification %j",
// through the tool.
#[tokio::test]
async fn bash_escalation_arguments_follow_tool_bash() {
    if !have_bash("bash_escalation_arguments_follow_tool_bash") {
        return;
    }
    let root = temp_root("bash-args");
    let _cleanup = Cleanup(root.clone());
    let provider = Arc::new(RecordingProvider::default());
    let tools = sandboxed(
        &root,
        &bash_permissions(&root, WW, Arc::clone(&provider) as Arc<dyn SandboxProvider>),
    );
    let bash = tool(&tools, "bash");
    for justification in ["", " \t\n"] {
        let ok = call(
            bash,
            json!({ "command": "printf ok", "justification": justification }),
            None,
        )
        .await
        .expect("ordinary");
        assert_eq!(text(&ok), "ok");
        let refused = call(
            bash,
            json!({ "command": "printf ok", "sandbox_permissions": "danger-full-access", "justification": justification }),
            None,
        )
        .await;
        assert_eq!(
            refused.err().as_deref(),
            Some("invalid justification: expected a non-empty sentence")
        );
    }
    assert_eq!(provider.modes(), [WW, WW]);
}
