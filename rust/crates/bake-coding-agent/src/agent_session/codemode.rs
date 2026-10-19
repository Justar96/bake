//! The session's side of the `codemode` tool ([`crate::tools::codemode`]):
//! its store on the session's branch, the namespaces of MCP tools, the
//! tools' prompt guidelines, and the loadout the active tool applies to
//! each request.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use bake_agent::{AgentTool, BoxFuture};
use bake_ai::Message;
use bake_codemode::StoreValues;

use super::recovery::Core;
use super::write_session;
use crate::mcp::McpServers;
use crate::owned_work::WorkGate;
use crate::settings::Settings;
use crate::tools::codemode::{
    CODEMODE_STORE_ENTRY_TYPE, CODEMODE_TOOL_NAME, CodemodeHost, CodemodeNamespace,
    CodemodeStoreEntryData, ToolLoadout, prepare_codemode_loadout, read_codemode_store,
};

/// Tools whose declarations requests leave out, Pi's
/// `_hiddenDeclarations`: set by each request's loadout, read when the
/// transcript becomes the request.
pub(super) type HiddenDeclarations = Arc<Mutex<Vec<String>>>;

/// What the session gives its `codemode` tool.
pub(super) struct SessionCodemodeHost {
    pub(super) core: Arc<Core>,
    pub(super) guidelines: Arc<HashMap<String, Vec<String>>>,
    pub(super) mcp: Arc<OnceLock<McpServers>>,
}

impl CodemodeHost for SessionCodemodeHost {
    fn tool_namespace(&self, name: &str) -> Option<CodemodeNamespace> {
        self.mcp.get()?.tool_namespace(name)
    }

    fn tool_guidelines(&self, name: &str) -> Vec<String> {
        self.guidelines.get(name).cloned().unwrap_or_default()
    }

    fn load_store(&self) -> BoxFuture<'static, Result<StoreValues, String>> {
        let core = Arc::clone(&self.core);
        Box::pin(async move {
            core.with_session(|session| read_codemode_store(&session.branch_entries(None)))
                .await
        })
    }

    fn append_store(&self, data: CodemodeStoreEntryData) -> BoxFuture<'static, Result<(), String>> {
        let core = Arc::clone(&self.core);
        Box::pin(async move {
            let written = core
                .owned_context_commit(move |core| {
                    write_session(&core.session, &core.gap, |session| {
                        session
                            .append_custom_entry(CODEMODE_STORE_ENTRY_TYPE, Some(data.to_value()))
                    })
                    .map(drop)
                    .map_err(|error| error.message())
                })
                .map_err(|refused| refused.to_string())?;
            match written.await {
                Ok(result) => result,
                Err(error) => Err(format!("The codemode store could not be written: {error}")),
            }
        })
    }

    fn work(&self) -> Option<WorkGate> {
        Some(self.core.work.clone())
    }
}

/// The tools of one request with the active `codemode` tool's loadout
/// applied: Pi's `_applyToolLoadout`. Without an active `codemode` tool the
/// tools are returned unchanged and nothing is hidden, so requests are what
/// they would be without code mode.
pub(super) fn apply_codemode_loadout(
    tools: Vec<Arc<AgentTool>>,
    settings: &Settings,
    host: &SessionCodemodeHost,
) -> (Vec<Arc<AgentTool>>, Vec<String>) {
    if !tools.iter().any(|tool| tool.name == CODEMODE_TOOL_NAME) {
        return (tools, Vec::new());
    }
    let namespace = |name: &str| host.tool_namespace(name);
    let guidelines = |name: &str| host.tool_guidelines(name);
    let changes = prepare_codemode_loadout(
        &ToolLoadout {
            declared: &tools,
            namespace: &namespace,
            guidelines: &guidelines,
        },
        settings.codemode_mode,
        settings.codemode_inline_budget,
    );
    let tools = tools
        .into_iter()
        .map(|tool| {
            match changes
                .descriptions
                .iter()
                .find(|(name, _)| name == &tool.name)
            {
                Some((_, description)) if description != &tool.description => {
                    let mut described = (*tool).clone();
                    described.description = description.clone();
                    Arc::new(described)
                }
                _ => tool,
            }
        })
        .collect();
    (tools, changes.hidden_declarations)
}

/// Pi's `_installHiddenDeclarationsProjection`: the transcript's tool
/// declarations without the hidden ones, so the request declares what the
/// loadout shows while the transcript still records every active tool.
pub(super) fn hide_declarations(
    messages: Vec<Message>,
    hidden: &HiddenDeclarations,
) -> Vec<Message> {
    let hidden = hidden.lock().unwrap_or_else(PoisonError::into_inner);
    if hidden.is_empty() {
        return messages;
    }
    messages
        .into_iter()
        .map(|message| match message {
            Message::System(mut system)
                if system.tools_added.is_some() || system.tools_removed.is_some() =>
            {
                system.tools_added = system
                    .tools_added
                    .map(|added| {
                        added
                            .into_iter()
                            .filter(|tool| !hidden.contains(&tool.name))
                            .collect::<Vec<_>>()
                    })
                    .filter(|added| !added.is_empty());
                system.tools_removed = system
                    .tools_removed
                    .map(|removed| {
                        removed
                            .into_iter()
                            .filter(|tool| !hidden.contains(&tool.name))
                            .collect::<Vec<_>>()
                    })
                    .filter(|removed| !removed.is_empty());
                Message::System(system)
            }
            other => other,
        })
        .collect()
}
