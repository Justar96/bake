//! Pi's `write` and `edit` tools under the sandbox.
//!
//! Ported from Bake's TypeScript `packages/fs/fs-sandbox/src/index.ts`
//! (`SandboxedFileSystem.checkedTarget`: `read-only` denies every
//! mutation, `workspace-write` re-canonicalizes the target immediately
//! before the mutation and requires it under a writable root,
//! `danger-full-access` passes) and `packages/fs/tool-fs/src/{sandbox,write,edit}.ts`
//! (`FsSandboxController`: the escalation parameters, the per-call policy,
//! and `mapError`, which turns `FS_SANDBOX_DENIED` into the denial marker
//! and the escalation hint). The parameters and the hint appear only when
//! the session can reach a wider mode
//! ([`ToolPermissions::escalation_targets`]); TypeScript always added them.
//!
//! The check runs inside the file's mutation queue, after Pi's first abort
//! check and before any effect (for `edit`, before the file is checked or
//! read), on the path's resolved form at that moment, so a link swapped
//! before the queue admits the call is judged as it then is. Tool
//! descriptions stay Pi's; the parameters gain the two escalation
//! parameters with the `tool-fs` descriptions.
//!
//! Pi writes in place, following links, where the TypeScript fence writes a
//! temporary file and renames it over the checked name. So the guard judges
//! the path the write will land on: it follows every link in the path,
//! dangling ones included ([`write_target`]), and refuses a chain it cannot
//! resolve. The call then mutates that checked target, not the path it was
//! given: under `workspace-write` the guard returns a
//! [`BeneathTarget`], which opens the file
//! relative to the writable root without following any link, so a link
//! swapped in between the check and the write is refused instead of
//! followed (the TypeScript fence accepted that race). A link inside the
//! workspace that points inside it still writes its target, which the check
//! resolved first.
//!
//! The roots are the policy's, with the workspace root taken as the policy
//! carries it rather than resolved again: [`StandingPermissions`](super::StandingPermissions)
//! pins it, canonical, at session start, so moving the workspace and
//! leaving a link in its place does not redirect later writes.
//!
//! A hard link inside the workspace to a file outside it is still written
//! through, as Pi's in-place write does; the TypeScript rename would have
//! replaced the link instead. Creating such a link needs write access to
//! both places, which confined bash lacks.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use bake_agent::BoxFuture;
use bake_ai::AbortSignal;
use bake_sandbox::escalation::{escalation_hint_marker, sandbox_denial_marker};
use bake_sandbox::{SandboxMode, SandboxPolicy, canonical_path, is_path_under};
use serde_json::Value;

use super::ToolPermissions;
use super::beneath::BeneathTarget;
use super::engine::{BlankJustification, ToolCall};
use super::with_escalation_parameters;
use crate::owned_work::WorkGate;
use crate::tools::definition::{MutationGuard, MutationTarget, PathTarget, ToolDefinition};
use crate::tools::edit::{create_edit_tool_definition, validate_edit_input};
use crate::tools::write::create_write_tool_definition;

/// `tool-fs`'s `sandbox_permissions` description.
pub const FS_SANDBOX_PERMISSIONS_DESCRIPTION: &str =
    "One retry after a sandbox denial; asks the user.";
/// `tool-fs`'s `justification` description.
pub const FS_JUSTIFICATION_DESCRIPTION: &str =
    "One sentence for the user; required with sandbox_permissions.";

/// The text of a refused mutation (`mapError`): the denial marker, then,
/// when `escalation_hint` says a wider mode is reachable, the escalation
/// hint.
pub fn fs_denial_text(policy: &SandboxPolicy, escalation_hint: bool) -> String {
    let marker = sandbox_denial_marker(policy.mode);
    if escalation_hint {
        format!("{marker}\n{}", escalation_hint_marker("operation"))
    } else {
        marker
    }
}

/// The most links [`write_target`] follows, as Linux's `MAXSYMLINKS`.
const MAX_LINK_HOPS: usize = 40;

/// The file a write to `path` lands on: every link in the absolute path
/// `path` (made absolute against the process directory) is followed, a
/// dangling one included, which `canonicalize` cannot do. `None` when a link
/// cannot be read, the chain is longer than 40 links, or a link
/// target is drive- or root-relative in a way the walk cannot place.
pub fn write_target(path: &Path) -> Option<PathBuf> {
    let path = std::path::absolute(path).ok()?;
    let mut pending: Vec<std::ffi::OsString> = Vec::new();
    let mut resolved = PathBuf::new();
    // `pending` is a stack: the next component is last.
    push_components(&mut pending, &mut resolved, &path);
    let mut hops = 0;
    // Once a component is missing nothing below it exists, so the rest is
    // appended without looking until a `..` climbs back out: this bounds
    // the walk's lookups by the existing depth.
    let mut missing = false;
    while let Some(name) = pending.pop() {
        if name == ".." {
            // `resolved` holds no links, so `..` is its parent, which
            // exists again if anything does.
            resolved.pop();
            missing = false;
            continue;
        }
        if missing {
            resolved.push(&name);
            continue;
        }
        let candidate = resolved.join(&name);
        match std::fs::symlink_metadata(&candidate) {
            Ok(meta) if meta.file_type().is_symlink() => {
                hops += 1;
                if hops > MAX_LINK_HOPS {
                    return None;
                }
                let target = std::fs::read_link(&candidate).ok()?;
                if target.is_absolute() {
                    resolved = PathBuf::new();
                } else if target.has_root()
                    || matches!(target.components().next(), Some(Component::Prefix(_)))
                {
                    return None;
                }
                push_components(&mut pending, &mut resolved, &target);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing = true;
                resolved = candidate;
            }
            _ => resolved = candidate,
        }
    }
    Some(resolved)
}

/// Queues `path`'s normal and `..` components on `pending` (last first) and
/// starts `resolved` at its prefix and root, if any.
fn push_components(pending: &mut Vec<std::ffi::OsString>, resolved: &mut PathBuf, path: &Path) {
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => resolved.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => names.push("..".into()),
            Component::Normal(name) => names.push(name.to_os_string()),
        }
    }
    pending.extend(names.into_iter().rev());
}

/// The directories `policy` lets the file tools write under: the
/// contract's [`SandboxPolicy::writable_roots`], except that the workspace
/// root is the policy's own, as pinned at session start, never resolved
/// again (a link left in its place must not redirect a write).
pub fn file_writable_roots(policy: &SandboxPolicy) -> Vec<PathBuf> {
    if policy.mode != SandboxMode::WorkspaceWrite {
        return Vec::new();
    }
    // The OS temporary directory stands in for the workspace, so the
    // contract's list holds only the other roots.
    let others = SandboxPolicy::new(SandboxMode::WorkspaceWrite, std::env::temp_dir())
        .with_extra_writable_roots(policy.extra_writable_roots.iter().cloned())
        .writable_roots();
    let mut roots = vec![policy.workspace_root.clone()];
    for root in others {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

/// The containment check of one call (`checkedTarget`). Under
/// `workspace-write` the file the write lands on (see [`write_target`]),
/// judged when the guard runs, must lie under one of
/// [`file_writable_roots`]; the call then writes through a
/// [`BeneathTarget`] that reaches that file from the root without following
/// links. An unresolvable target is refused, `read-only` refuses every
/// call, and `danger-full-access` writes Pi's way, to the path itself. A
/// denial carries the escalation hint when `escalation_hint` is set.
pub fn containment_guard(policy: SandboxPolicy, escalation_hint: bool) -> MutationGuard {
    Arc::new(move |path: &Path| {
        let denial = || fs_denial_text(&policy, escalation_hint);
        match policy.mode {
            SandboxMode::DangerFullAccess => {
                Ok(Box::new(PathTarget(path.to_path_buf())) as Box<dyn MutationTarget>)
            }
            SandboxMode::ReadOnly => Err(denial()),
            SandboxMode::WorkspaceWrite => {
                let target = write_target(path).ok_or_else(denial)?;
                // Link-free already; this settles case and short names as
                // the roots were settled.
                let target = canonical_path(&target);
                let beneath = file_writable_roots(&policy)
                    .iter()
                    .filter(|root| is_path_under(&target, root))
                    .find_map(|root| BeneathTarget::new(root, &target, denial()))
                    .ok_or_else(denial)?;
                #[cfg(test)]
                seam::after_check(path);
                Ok(Box::new(beneath) as Box<dyn MutationTarget>)
            }
        }
    })
}

fn fs_parameters(parameters: Value, permissions: &ToolPermissions) -> Value {
    with_escalation_parameters(
        parameters,
        permissions.escalation_targets,
        FS_SANDBOX_PERMISSIONS_DESCRIPTION,
        FS_JUSTIFICATION_DESCRIPTION,
    )
}

fn call<'a>(tool_name: &'a str, id: &'a str, args: &'a Value) -> ToolCall<'a> {
    ToolCall {
        tool_name,
        call_id: id,
        args,
        subject: "operation",
        blank_justification: BlankJustification::Checked,
    }
}

/// Pi's `write` under the permission engine. Its blocking work has no
/// owner.
pub fn create_sandboxed_write_tool_definition(
    cwd: &Path,
    permissions: &ToolPermissions,
) -> ToolDefinition {
    owned_sandboxed_write_tool_definition(cwd, permissions, None)
}

/// [`create_sandboxed_write_tool_definition`] whose blocking work `owner`
/// owns, as [`crate::tools::index::ToolsOptions::owner`] owns `write`'s.
pub(crate) fn owned_sandboxed_write_tool_definition(
    cwd: &Path,
    permissions: &ToolPermissions,
    owner: Option<WorkGate>,
) -> ToolDefinition {
    let mut definition = create_write_tool_definition(cwd);
    let cwd = cwd.to_path_buf();
    let permissions = permissions.clone();
    let tool = &mut definition.tool;
    tool.parameters = fs_parameters(std::mem::take(&mut tool.parameters), &permissions);
    tool.execute = Arc::new(
        move |id: String, args: Value, signal: Option<AbortSignal>, _update, _ctx| {
            let cwd = cwd.clone();
            let permissions = permissions.clone();
            let owner = owner.clone();
            Box::pin(async move {
                let policy = permissions
                    .engine
                    .resolve(&call("write", &id, &args), signal.as_ref())
                    .await?;
                let hint = permissions.offers_escalation_beyond(policy.mode);
                crate::tools::write::execute_owned(
                    cwd,
                    args,
                    signal,
                    Some(containment_guard(policy, hint)),
                    owner,
                )
                .await
            }) as BoxFuture<'static, _>
        },
    );
    definition
}

/// Pi's `edit` under the permission engine. Pi's input check runs before
/// the escalation step, so an invalid edit never asks for approval. Its
/// blocking work has no owner.
pub fn create_sandboxed_edit_tool_definition(
    cwd: &Path,
    permissions: &ToolPermissions,
) -> ToolDefinition {
    owned_sandboxed_edit_tool_definition(cwd, permissions, None)
}

/// [`create_sandboxed_edit_tool_definition`] whose blocking work `owner`
/// owns, as [`crate::tools::index::ToolsOptions::owner`] owns `edit`'s.
pub(crate) fn owned_sandboxed_edit_tool_definition(
    cwd: &Path,
    permissions: &ToolPermissions,
    owner: Option<WorkGate>,
) -> ToolDefinition {
    let mut definition = create_edit_tool_definition(cwd);
    let cwd = cwd.to_path_buf();
    let permissions = permissions.clone();
    let tool = &mut definition.tool;
    tool.parameters = fs_parameters(std::mem::take(&mut tool.parameters), &permissions);
    tool.execute = Arc::new(
        move |id: String, args: Value, signal: Option<AbortSignal>, _update, _ctx| {
            let cwd = cwd.clone();
            let permissions = permissions.clone();
            let owner = owner.clone();
            Box::pin(async move {
                validate_edit_input(&args)?;
                let policy = permissions
                    .engine
                    .resolve(&call("edit", &id, &args), signal.as_ref())
                    .await?;
                let hint = permissions.offers_escalation_beyond(policy.mode);
                crate::tools::edit::execute_owned(
                    cwd,
                    args,
                    signal,
                    Some(containment_guard(policy, hint)),
                    owner,
                )
                .await
            }) as BoxFuture<'static, _>
        },
    );
    definition
}

/// Test hooks into the write path.
#[cfg(test)]
pub(crate) mod seam {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, PoisonError};

    type Hook = Box<dyn FnOnce() + Send>;

    static AFTER_CHECK: Mutex<Option<HashMap<PathBuf, Hook>>> = Mutex::new(None);

    /// Runs `hook` once, after the guard has checked `path` and before the
    /// call touches it. The swap and shutdown tests use it on Unix only.
    #[cfg(unix)]
    pub(crate) fn on_after_check(path: &Path, hook: impl FnOnce() + Send + 'static) {
        AFTER_CHECK
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(HashMap::new)
            .insert(path.to_path_buf(), Box::new(hook));
    }

    pub(super) fn after_check(path: &Path) {
        let hook = AFTER_CHECK
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
            .and_then(|hooks| hooks.remove(path));
        if let Some(hook) = hook {
            hook();
        }
    }

    /// While held, the Linux opens of targets under `base` behave as if
    /// the kernel had no `openat2` and take the per-component walk.
    #[cfg(unix)]
    pub(crate) struct NoOpenat2(PathBuf);

    static NO_OPENAT2: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    #[cfg(unix)]
    impl NoOpenat2 {
        pub(crate) fn hold(base: &Path) -> Self {
            NO_OPENAT2
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(base.to_path_buf());
            Self(base.to_path_buf())
        }
    }

    #[cfg(unix)]
    impl Drop for NoOpenat2 {
        fn drop(&mut self) {
            let mut held = NO_OPENAT2.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(index) = held.iter().position(|base| *base == self.0) {
                held.swap_remove(index);
            }
        }
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn openat2_disabled(root: &Path) -> bool {
        NO_OPENAT2
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|base| root.starts_with(base))
    }
}
