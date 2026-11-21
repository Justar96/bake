//! The session's side of the `codemode` tool ([`crate::tools::codemode`]):
//! its store on the session's branch, the namespaces of MCP tools, the
//! tools' prompt guidelines, and the loadout the active tool applies to
//! each request.
//!
//! The tools a request hides are derived from the `codemode.mode` setting
//! and not written to the session file, as in Pi. The log still
//! reconstructs them: `only` is in effect exactly when the recorded
//! `codemode` declaration contains `Nested tools:`, and then every other
//! declared tool is hidden, since every session tool but `codemode` is
//! callable from scripts. Otherwise none is hidden; with `only` and no
//! other tool, the declaration lists none and nothing is hidden either.

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
    CODEMODE_STORE_ENTRY_TYPE, CODEMODE_TOOL_NAME, CodemodeHost, CodemodeMode, CodemodeNamespace,
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
    /// The tools' own descriptions that the last request's loadout
    /// replaced, by tool name: scripts see these, as Pi's `ctx.tools` keeps
    /// the registry's descriptions.
    pub(super) own_descriptions: Mutex<HashMap<String, String>>,
    /// The session is a subagent child: its file starts with a
    /// [`crate::subagents::CHILD_ENTRY`].
    pub(super) subagent: bool,
}

impl CodemodeHost for SessionCodemodeHost {
    fn tool_namespace(&self, name: &str) -> Option<CodemodeNamespace> {
        self.mcp.get()?.tool_namespace(name)
    }

    fn tool_guidelines(&self, name: &str) -> Vec<String> {
        self.guidelines.get(name).cloned().unwrap_or_default()
    }

    fn tool_description(&self, name: &str) -> Option<String> {
        self.own_descriptions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned()
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
                Err(error) => Err(error.to_string()),
            }
        })
    }

    fn work(&self) -> Option<WorkGate> {
        Some(self.core.work.clone())
    }

    fn is_subagent(&self) -> bool {
        self.subagent
    }
}

/// Whether `session` is a subagent child's, whose file starts with its
/// [`crate::subagents::CHILD_ENTRY`].
pub(super) fn is_subagent_session(session: &crate::session::SessionManager) -> bool {
    session.branch_entries(None).iter().any(|entry| {
        entry.entry_type() == "custom"
            && entry.get("customType").and_then(serde_json::Value::as_str)
                == Some(crate::subagents::CHILD_ENTRY)
    })
}

/// The result of preparing the active `codemode` tool's loadout without
/// mutating the session host.
pub(super) struct PreparedCodemodeLoadout {
    pub(super) tools: Vec<Arc<AgentTool>>,
    pub(super) own_descriptions: HashMap<String, String>,
    pub(super) hidden_declarations: Vec<String>,
}

/// The tools of one request with the active `codemode` tool's loadout
/// computed without mutating `host`: Pi's `_applyToolLoadout`, pure.
/// Without an active `codemode` tool the tools are returned unchanged and
/// nothing is hidden, so requests are what they would be without code mode.
pub(super) fn preview_codemode_loadout(
    tools: Vec<Arc<AgentTool>>,
    settings: &Settings,
    mode: CodemodeMode,
    host: &SessionCodemodeHost,
) -> PreparedCodemodeLoadout {
    if !tools.iter().any(|tool| tool.name == CODEMODE_TOOL_NAME) {
        return PreparedCodemodeLoadout {
            tools,
            own_descriptions: HashMap::new(),
            hidden_declarations: Vec::new(),
        };
    }
    let namespace = |name: &str| host.tool_namespace(name);
    let guidelines = |name: &str| host.tool_guidelines(name);
    let changes = prepare_codemode_loadout(
        &ToolLoadout {
            declared: &tools,
            namespace: &namespace,
            guidelines: &guidelines,
        },
        mode,
        settings.codemode_inline_budget,
    );
    let mut own_descriptions = HashMap::new();
    let tools = tools
        .into_iter()
        .map(|tool| {
            match changes
                .descriptions
                .iter()
                .find(|(name, _)| name == &tool.name)
            {
                Some((_, description)) if description != &tool.description => {
                    own_descriptions.insert(tool.name.clone(), tool.description.clone());
                    let mut described = (*tool).clone();
                    described.description = description.clone();
                    Arc::new(described)
                }
                _ => tool,
            }
        })
        .collect();
    PreparedCodemodeLoadout {
        tools,
        own_descriptions,
        hidden_declarations: changes.hidden_declarations,
    }
}

/// Records the tools' own descriptions replaced by the last request's
/// loadout so scripts see them in `ALL_TOOLS`, `describeTool()`, and
/// `searchTools()`.
pub(super) fn commit_codemode_descriptions(
    host: &SessionCodemodeHost,
    descriptions: HashMap<String, String>,
) {
    *host
        .own_descriptions
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = descriptions;
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
