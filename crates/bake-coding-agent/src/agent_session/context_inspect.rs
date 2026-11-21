//! Read-only inspection of the current/next request's prompt sections and tool
//! loadout ([`AgentSession::inspect_context`], [`SessionContextReport`]),
//! sharing its pure assembly with [`super::prompt_execution`].

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use bake_agent::AgentTool;
use bake_ai::Model;
use bake_ai::utils::estimate::estimate_text_tokens;

use super::codemode::{SessionCodemodeHost, preview_codemode_loadout};
use super::{AgentSession, SharedSettings, model_profile, profile_tool_names, read_settings};
use crate::mcp::McpServers;
use crate::permissions::LivePermission;
use crate::system_prompt::{SystemPromptOptions, build_system_prompt_sections};
use crate::tools::codemode::{CODEMODE_TOOL_NAME, codemode_callable_tools};

/// Summary of the system prompt sections and tool loadout the next model
/// request would send, without prompt text or credentials.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionContextReport {
    /// Estimated tokens of each system prompt section, in prompt order:
    /// `(section_name, estimated_tokens)`.
    pub sections: Vec<(String, usize)>,
    /// Tools directly declared in the next model request, in loadout order
    /// (after a mid-session profile or mode switch, wire replay order may
    /// differ while the tool set and schema token total match).
    pub direct_tools: Vec<String>,
    /// Active tools callable from `codemode` scripts (empty when `codemode` is
    /// not active), in loadout order. Overlaps `direct_tools` when
    /// `codemode.mode` is `on`.
    pub codemode_tools: Vec<String>,
    /// Tools registered on the session that are not active for the next
    /// request, in registration order.
    pub inactive_tools: Vec<String>,
    /// Estimated tokens of the direct tool declarations payload sent to the
    /// model, separate from `sections`.
    pub tool_schema_tokens: usize,
}

/// Borrowed inputs to [`assemble_prompt_and_tools`].
pub(super) struct PromptAndToolsInputs<'a> {
    pub(super) model: Option<&'a Model>,
    pub(super) settings: &'a SharedSettings,
    pub(super) tools: &'a [Arc<AgentTool>],
    pub(super) active_tool_names: &'a [String],
    pub(super) permission: Option<&'a LivePermission>,
    pub(super) mcp: &'a OnceLock<McpServers>,
    pub(super) codemode: &'a SessionCodemodeHost,
    pub(super) prompt_options: &'a SystemPromptOptions,
    pub(super) nested_section: Option<String>,
}

/// Pure output of assembling one request's tools and system prompt sections,
/// before any mutation of agent state, codemode host, or transcript.
pub(super) struct PreparedPromptAndTools {
    pub(super) active_names: Vec<String>,
    pub(super) active_tools: Vec<Arc<AgentTool>>,
    pub(super) codemode_own_descriptions: HashMap<String, String>,
    pub(super) hidden_declarations: Vec<String>,
    pub(super) sections: Vec<(String, String)>,
}

/// Assembles the active tools, codemode loadout, and system prompt sections
/// for a request without mutating session, agent, codemode host, or MCP state.
pub(super) fn assemble_prompt_and_tools(
    inputs: PromptAndToolsInputs<'_>,
) -> Result<PreparedPromptAndTools, String> {
    let profile = model_profile(inputs.settings, inputs.model);
    let active_names = profile_tool_names(&profile, inputs.active_tool_names, inputs.tools);
    let mut active: Vec<Arc<AgentTool>> = active_names
        .iter()
        .filter_map(|name| inputs.tools.iter().find(|tool| &tool.name == name))
        .cloned()
        .collect();
    // The escalation offer the permission makes now, which a switch since the
    // tools were built may have changed.
    if let Some(permission) = inputs.permission {
        let targets = permission.escalation_targets();
        active = active
            .into_iter()
            .map(|tool| crate::permissions::refresh_escalation_offer(tool, targets))
            .collect();
    }
    // The MCP tools published now, one generation per server; never starts MCP.
    if let Some(mcp) = inputs.mcp.get() {
        active.extend(mcp.tools());
    }
    // The active `codemode` tool's descriptions and hidden declarations;
    // without it, the tools as they are. Pure: does not mutate `codemode`.
    let loadout = {
        let settings = read_settings(inputs.settings);
        let mode = profile.codemode_mode.unwrap_or(settings.codemode_mode);
        preview_codemode_loadout(active, &settings, mode, inputs.codemode)
    };
    let mut options = inputs.prompt_options.clone();
    options.selected_tools = active_names.clone();
    // The profile's notes follow `environment`, the last of Bake's own
    // sections, so everything before them stays the same across models.
    if let Some(prompt) = profile.prompt.filter(|prompt| !prompt.is_empty()) {
        options
            .sections
            .insert(0, (crate::settings::MODEL_NOTES_SECTION.to_owned(), prompt));
    }
    if let Some(permission) = inputs.permission {
        options.permissions = permission.prompt_text();
    }
    // The system-prompt rules leave out both the caller's `hidden_tools`
    // (guidelines only) and codemode's `hidden_declarations` (which requests
    // also withhold from provider tool declarations).
    options
        .hidden_tools
        .extend(loadout.hidden_declarations.iter().cloned());
    // The `mcp_servers` section lists the servers connected now, as Pi's
    // `before_agent_start` handler does; a session without one adds no section
    // and its requests are unchanged.
    if let Some(section) = inputs
        .mcp
        .get()
        .and_then(crate::mcp::McpServers::servers_prompt_section)
    {
        options
            .sections
            .push((crate::mcp::MCP_SERVERS_SECTION.to_owned(), section));
    }
    // The context files found while working, last: a section a patch adds is
    // replayed after the others.
    if let Some(section) = inputs.nested_section {
        options
            .sections
            .push((super::nested_context::SECTION.to_owned(), section));
    }
    let sections = build_system_prompt_sections(&options).map_err(|error| error.to_string())?;
    Ok(PreparedPromptAndTools {
        active_names,
        active_tools: loadout.tools,
        codemode_own_descriptions: loadout.own_descriptions,
        hidden_declarations: loadout.hidden_declarations,
        sections,
    })
}

fn tokens_usize(text: &str) -> usize {
    usize::try_from(estimate_text_tokens(text)).unwrap_or(usize::MAX)
}

impl AgentSession {
    /// Previews the system prompt section token estimates and tool loadout
    /// that the next model request would send, without mutating session,
    /// agent, transcript, permission, hook, codemode, or MCP state.
    pub fn inspect_context(&self) -> Result<SessionContextReport, String> {
        let model = self.model();
        let prepared = assemble_prompt_and_tools(PromptAndToolsInputs {
            model: model.as_ref(),
            settings: &self.settings,
            tools: &self.tools,
            active_tool_names: &self.active_tool_names,
            permission: self.permission.as_ref(),
            mcp: &self.mcp,
            codemode: &self.codemode,
            prompt_options: &self.prompt_options,
            nested_section: self.core.nested.section(),
        })?;
        let sections = prepared
            .sections
            .iter()
            .map(|(name, body)| (name.clone(), tokens_usize(body)))
            .collect();
        // Provider requests withhold only `hidden_declarations` (from
        // `codemode.mode = only`); `prompt_options.hidden_tools` suppresses
        // system-prompt rules only.
        let direct: Vec<&Arc<AgentTool>> = prepared
            .active_tools
            .iter()
            .filter(|tool| !prepared.hidden_declarations.contains(&tool.name))
            .collect();
        let direct_tools = direct.iter().map(|tool| tool.name.clone()).collect();
        let direct_declarations: Vec<bake_ai::Tool> =
            direct.iter().map(|tool| tool.declaration()).collect();
        let tool_schema_tokens = if direct_declarations.is_empty() {
            0
        } else {
            let serialized = serde_json::to_string(&direct_declarations).map_err(|error| {
                format!("Failed to serialize direct tool declarations: {error}")
            })?;
            tokens_usize(&serialized)
        };
        let codemode_active = prepared
            .active_tools
            .iter()
            .any(|tool| tool.name == CODEMODE_TOOL_NAME);
        let codemode_tools = if codemode_active {
            codemode_callable_tools(&prepared.active_tools)
                .into_iter()
                .filter(|tool| tool.nested_callable)
                .map(|tool| tool.name.clone())
                .collect()
        } else {
            Vec::new()
        };
        let inactive_tools = self
            .tools
            .iter()
            .filter(|tool| !prepared.active_names.contains(&tool.name))
            .map(|tool| tool.name.clone())
            .collect();
        Ok(SessionContextReport {
            sections,
            direct_tools,
            codemode_tools,
            inactive_tools,
            tool_schema_tokens,
        })
    }
}
