//! The `write` tool, ported from Pi `src/core/tools/write.ts` (v1.1.0; see
//! the crate's `NOTICE`): creates parent directories, then writes the
//! content as UTF-8, inside the file's mutation queue on Tokio's blocking
//! pool. An abort is observed between steps and never releases the queue
//! while a step runs. Under the session's owner that blocking work holds a
//! permit, so shutdown waits for a write even when its run was dropped
//! ([`crate::tools::index::ToolsOptions::owner`]). Pi's pluggable
//! `WriteOperations` are not ported;
//! [`execute_guarded`] is Bake's seam for the sandbox's containment check.

use std::io::Write;
use std::path::{Path, PathBuf};

use bake_agent::{AgentTool, AgentToolResult};
use bake_ai::{AbortSignal, ConstrainedSampling, StrictPreference};
use serde_json::{Value, json};

use crate::tools::definition::{
    ABORTED, MutationGuard, ToolDefinition, boxed, mutation_target, run_blocking, string_arg,
    target_error, text_result,
};
use crate::tools::file_mutation_queue::with_file_mutation_queue;
use crate::tools::node_error;
use crate::tools::path_utils::resolve_to_cwd;

/// Pi's `writeToolSystemPromptContribution` snippet.
pub const WRITE_PROMPT_SNIPPET: &str = "Create or overwrite files";
/// Pi's `writeToolSystemPromptContribution` guidelines.
pub const WRITE_PROMPT_GUIDELINES: &[&str] =
    &["Use write only for new files or complete rewrites."];

fn parameters() -> Value {
    json!({
        "type": "object",
        "required": ["path", "content"],
        "properties": {
            "path": { "type": "string", "description": "Path to the file to write (relative or absolute)" },
            "content": { "type": "string", "description": "Content to write to the file" },
        },
    })
}

/// Pi's `createWriteToolDefinition`. Its blocking work has no owner: a
/// dropped call's write runs to its end unawaited.
pub fn create_write_tool_definition(cwd: &Path) -> ToolDefinition {
    owned_write_tool_definition(cwd, None)
}

/// [`create_write_tool_definition`] whose blocking work `owner` owns: its
/// close waits for a call's write, even one whose run was dropped, and a
/// call made once it is closing is refused.
pub(crate) fn owned_write_tool_definition(
    cwd: &Path,
    owner: Option<crate::owned_work::WorkGate>,
) -> ToolDefinition {
    let cwd = cwd.to_path_buf();
    let mut tool = AgentTool::new(
        "write",
        "write",
        "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.",
        parameters(),
        move |_id, args, signal, _update| {
            boxed(execute_owned(
                cwd.clone(),
                args,
                signal,
                None,
                owner.clone(),
            ))
        },
    );
    tool.constrained_sampling = Some(ConstrainedSampling::JsonSchema {
        strict: StrictPreference::Prefer,
    });
    ToolDefinition {
        tool,
        prompt_snippet: Some(WRITE_PROMPT_SNIPPET.into()),
        prompt_guidelines: Some(
            WRITE_PROMPT_GUIDELINES
                .iter()
                .map(|g| (*g).to_owned())
                .collect(),
        ),
    }
}

/// Pi's `createWriteTool`.
pub fn create_write_tool(cwd: &Path) -> AgentTool {
    create_write_tool_definition(cwd).tool
}

/// Runs a `write` call, first passing the target to `guard` inside its
/// mutation queue, after the first abort check and before any effect; the
/// directories and the file are then made through the target the guard
/// returns. The blocking work has no owner.
pub async fn execute_guarded(
    cwd: PathBuf,
    args: Value,
    signal: Option<AbortSignal>,
    guard: Option<MutationGuard>,
) -> Result<AgentToolResult, String> {
    execute_owned(cwd, args, signal, guard, None).await
}

/// [`execute_guarded`] with the blocking work under `owner`.
pub(crate) async fn execute_owned(
    cwd: PathBuf,
    args: Value,
    signal: Option<AbortSignal>,
    guard: Option<MutationGuard>,
    owner: Option<crate::owned_work::WorkGate>,
) -> Result<AgentToolResult, String> {
    let path = string_arg(&args, "path");
    let content = string_arg(&args, "content");
    let absolute = resolve_to_cwd(&path, &cwd)?;
    run_blocking(owner, move || {
        let check = || {
            if signal.as_ref().is_some_and(AbortSignal::aborted) {
                Err(ABORTED.to_owned())
            } else {
                Ok(())
            }
        };
        let outcome = with_file_mutation_queue(&absolute, || {
            Ok(write(&absolute, &content, &check, guard.as_ref()))
        })
        .map_err(|error| node_error::message(&error, "realpath", Some(&absolute)))?;
        outcome?;
        Ok(text_result(format!("Successfully wrote to {path}"), None))
    })
    .await
}

fn write(
    path: &Path,
    content: &str,
    check: &dyn Fn() -> Result<(), String>,
    guard: Option<&MutationGuard>,
) -> Result<(), String> {
    check()?;
    // The guard's target carries every later step, so the call writes what
    // the guard checked.
    let target = mutation_target(guard, path)?;
    let target = target.as_ref();
    if let Some(dir) = path.parent() {
        target
            .create_parent_dirs()
            .map_err(|error| target_error(target, &error, "mkdir", dir))?;
    }
    check()?;
    target
        .create()
        .and_then(|mut file| file.write_all(content.as_bytes()))
        .map_err(|error| target_error(target, &error, "open", path))?;
    check()
}
