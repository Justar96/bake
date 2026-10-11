//! The `write` tool, ported from Pi `src/core/tools/write.ts` (v1.1.0; see
//! the crate's `NOTICE`): creates parent directories, then writes the
//! content as UTF-8, inside the file's mutation queue on Tokio's blocking
//! pool. An abort is observed between steps and never releases the queue
//! while a step runs. Pi's pluggable `WriteOperations` are not ported.

use std::path::{Path, PathBuf};

use bake_agent::{AgentTool, AgentToolResult};
use bake_ai::{AbortSignal, ConstrainedSampling, StrictPreference};
use serde_json::{Value, json};

use crate::tools::definition::{
    ABORTED, ToolDefinition, boxed, run_blocking, string_arg, text_result,
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

/// Pi's `createWriteToolDefinition`.
pub fn create_write_tool_definition(cwd: &Path) -> ToolDefinition {
    let cwd = cwd.to_path_buf();
    let mut tool = AgentTool::new(
        "write",
        "write",
        "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.",
        parameters(),
        move |_id, args, signal, _update| boxed(execute(cwd.clone(), args, signal)),
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

async fn execute(
    cwd: PathBuf,
    args: Value,
    signal: Option<AbortSignal>,
) -> Result<AgentToolResult, String> {
    let path = string_arg(&args, "path");
    let content = string_arg(&args, "content");
    let absolute = resolve_to_cwd(&path, &cwd)?;
    run_blocking(move || {
        let check = || {
            if signal.as_ref().is_some_and(AbortSignal::aborted) {
                Err(ABORTED.to_owned())
            } else {
                Ok(())
            }
        };
        let outcome =
            with_file_mutation_queue(&absolute, || Ok(write(&absolute, &content, &check)))
                .map_err(|error| node_error::message(&error, "realpath", Some(&absolute)))?;
        outcome?;
        Ok(text_result(format!("Successfully wrote to {path}"), None))
    })
    .await
}

fn write(path: &Path, content: &str, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
    check()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|error| node_error::message(&error, "mkdir", Some(dir)))?;
    }
    check()?;
    std::fs::write(path, content)
        .map_err(|error| node_error::message(&error, "open", Some(path)))?;
    check()
}
