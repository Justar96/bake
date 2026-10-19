//! Tool calls a tool makes while it runs, Pi's `ctx.executeTool()`.
//!
//! Ported from Pi `packages/coding-agent/src/core/nested-tool-calls.ts`
//! (v1.3.0, revision `71023aec1546d6d677a65669c5665c0927ce0dd0`, MIT; see
//! the crate's `NOTICE`).

use bake_ai::ToolCall;
use serde_json::Value;

use crate::agent_loop::create_error_tool_result;
use crate::types::AgentToolCallOutcome;

/// Runs session tools on behalf of one calling tool call, the
/// `executeTool` of Pi's `ExtensionToolContext`. Reached through
/// [`crate::ToolContext::nested`].
#[derive(Debug, Clone)]
pub struct NestedToolExecutor {
    caller_id: String,
}

impl NestedToolExecutor {
    /// An executor that refuses every call, for a tool run outside an agent.
    pub(crate) fn unavailable(caller_id: impl Into<String>) -> Self {
        Self {
            caller_id: caller_id.into(),
        }
    }

    /// The id of the calling tool call; empty when detached.
    pub fn caller_id(&self) -> &str {
        &self.caller_id
    }

    /// Runs the tool `name` with `args`; failures come back as `is_error`
    /// outcomes, never as `Err`.
    pub async fn execute(&self, name: &str, _args: Value) -> AgentToolCallOutcome {
        AgentToolCallOutcome {
            tool_call: ToolCall {
                id: format!("{}/0", self.caller_id),
                name: name.to_owned(),
                ..ToolCall::default()
            },
            result: create_error_tool_result(NESTED_CALLS_UNAVAILABLE),
            is_error: true,
            duration_ms: None,
        }
    }
}

/// Pi's error text for a nested call where no agent runs the tool.
pub const NESTED_CALLS_UNAVAILABLE: &str = "Nested tool calls are not available in this context";
