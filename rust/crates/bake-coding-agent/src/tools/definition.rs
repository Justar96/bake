//! Tool definitions, ported from the parts of Pi
//! `src/core/tools/tool-definition-wrapper.ts` that shape a tool (v1.1.0;
//! see the crate's `NOTICE`), and the shared execution helpers of the file
//! tools.
//!
//! Pi's `ToolDefinition` is an `AgentTool` plus what the system prompt says
//! about it and how the terminal renders it. Bake's [`ToolDefinition`] holds
//! the [`AgentTool`] and the prompt text; renderers are the terminal's.

use std::future::Future;

use bake_agent::{AgentTool, AgentToolResult};
use bake_ai::AbortSignal;
use serde_json::Value;

use crate::agent_session::ToolPrompt;

/// The message every tool rejects with when aborted.
pub(crate) const ABORTED: &str = "Operation aborted";

/// Pi's `ToolDefinition` without renderers.
#[derive(Clone, Debug)]
pub struct ToolDefinition {
    /// The executable tool.
    pub tool: AgentTool,
    /// Pi's `promptSnippet`: the tool's line under the prompt's tools.
    pub prompt_snippet: Option<String>,
    /// Pi's `promptGuidelines`: bullets for the prompt's guidelines.
    pub prompt_guidelines: Option<Vec<String>>,
}

impl ToolDefinition {
    /// The tool's name.
    pub fn name(&self) -> &str {
        &self.tool.name
    }

    /// What the session's prompt says about the tool.
    pub fn prompt(&self) -> ToolPrompt {
        ToolPrompt {
            name: self.tool.name.clone(),
            snippet: self.prompt_snippet.clone(),
            guidelines: self.prompt_guidelines.clone().unwrap_or_default(),
        }
    }
}

/// Pi's `wrapToolDefinition`: the executable tool of a definition.
pub fn wrap_tool_definition(definition: &ToolDefinition) -> AgentTool {
    definition.tool.clone()
}

/// Pi's `wrapToolDefinitions`.
pub fn wrap_tool_definitions(definitions: &[ToolDefinition]) -> Vec<AgentTool> {
    definitions.iter().map(wrap_tool_definition).collect()
}

/// Pi's `createToolDefinitionFromAgentTool`: a definition without prompt
/// text.
pub fn create_tool_definition_from_agent_tool(tool: AgentTool) -> ToolDefinition {
    ToolDefinition {
        tool,
        prompt_snippet: None,
        prompt_guidelines: None,
    }
}

/// A result holding one text block and optional details.
pub(crate) fn text_result(text: impl Into<String>, details: Option<Value>) -> AgentToolResult {
    AgentToolResult {
        details,
        ..AgentToolResult::text(text)
    }
}

/// Runs blocking work on Tokio's blocking pool and waits for it, even when
/// the caller's run is aborted: the work checks `signal` itself, as Pi's
/// write and edit tools do inside their mutation queue.
pub(crate) async fn run_blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|error| Err(format!("Tool task failed: {error}")))
}

/// Runs blocking work that stops at an abort, as Pi's tools that reject
/// from their abort listener do: on abort the result is [`ABORTED`], but
/// only after the work, which polls the signal, has returned.
pub(crate) async fn run_blocking_abortable<T: Send + 'static>(
    signal: Option<AbortSignal>,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    if signal.as_ref().is_some_and(AbortSignal::aborted) {
        return Err(ABORTED.into());
    }
    let mut task = tokio::task::spawn_blocking(work);
    let joined = match &signal {
        Some(signal) => {
            tokio::select! {
                joined = &mut task => joined,
                () = signal.cancelled() => {
                    let _ = task.await;
                    return Err(ABORTED.into());
                }
            }
        }
        None => task.await,
    };
    match joined {
        Ok(_) if signal.as_ref().is_some_and(AbortSignal::aborted) => Err(ABORTED.into()),
        Ok(result) => result,
        Err(error) => Err(format!("Tool task failed: {error}")),
    }
}

/// Whether `signal` has aborted.
pub(crate) fn aborted(signal: Option<&AbortSignal>) -> bool {
    signal.is_some_and(AbortSignal::aborted)
}

/// Boxes a tool's future.
pub(crate) fn boxed<F>(future: F) -> bake_agent::BoxFuture<'static, Result<AgentToolResult, String>>
where
    F: Future<Output = Result<AgentToolResult, String>> + Send + 'static,
{
    Box::pin(future)
}

/// A string argument, `""` when absent; validation has checked types.
pub(crate) fn string_arg(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// An optional string argument.
pub(crate) fn optional_string_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// An optional number argument.
pub(crate) fn number_arg(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(Value::as_f64)
}

/// An optional boolean argument, JavaScript truthiness for other values.
pub(crate) fn bool_arg(args: &Value, key: &str) -> bool {
    match args.get(key) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}
