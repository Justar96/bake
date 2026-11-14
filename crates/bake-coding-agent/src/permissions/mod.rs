//! Bake's permission layer over the sandbox contract: permission
//! presets, the approval policy and answerers, a layered engine that
//! resolves each tool call's [`SandboxPolicy`], and Pi's `bash`, `write`,
//! and `edit` tools wrapped by it. `read`, `grep`, `find`, and `ls` are
//! unchanged.
//!
//! | Module | Contents |
//! |---|---|
//! | [`presets`] | named presets of a sandbox mode and an approval policy |
//! | [`approval`] | the approval policy and its answerers |
//! | [`broker`] | the interactive approval queue |
//! | [`state`] | the session's preset, sandbox-mode, and approval-policy entries and their fold |
//! | [`history`] | the asked and decided approval records, as `bake.approval` session entries, and their fold |
//! | [`engine`] | each call's policy: the standing mode, then a one-call escalation |
//! | [`context`] | the model-facing statement of the permission |
//! | [`bash`] | the sandboxed `bash` tool |
//! | [`fs`](mod@fs) | the sandboxed `write` and `edit` tools |
//! | [`beneath`] | the link-refusing open of a checked write target |
//!
//! Where a model-visible string (tool parameters and the sandbox paragraph,
//! denial and hint markers, refusals, and policy sentences) meets Pi's
//! text, Pi's stays and Bake's is added to it.
//!
//! # Fail closed
//!
//! A call runs only under the policy the engine resolved. A confined mode
//! no backend can enforce refuses the bash call ([`bake_sandbox::SandboxUnavailable`]);
//! a file mutation outside the policy is refused before any effect; an
//! escalation nobody approves is refused before anything runs. In a
//! session that persists, a request and its answer are written to the
//! session file before the call can run ([`history`]); a failed write
//! refuses the call and stops the session.
//!
//! # Defaults and switching
//!
//! A session's permission defaults to Pi's full access, the
//! `danger-full-access` mode with the `never` policy
//! ([`presets::KnobDefaults`], [`presets::PresetTable::default_preset`]).
//! A running session switches presets through its [`LivePermission`],
//! written as the
//! `bake.permission` entry ([`state::preset_change`]) and applied to the
//! [`StandingPermissions`] the engine reads, so the next call to resolve
//! uses it and a call already resolved keeps its policy. At the next
//! prompt the session restates the `permissions` system-prompt section and
//! refreshes the tools' escalation offer ([`refresh_escalation_offer`]).
//!
//! Besides a one-call grant, an approval can answer with a session grant of one tool for one mode, and a switch to full access
//! ([`ApprovalOffers`], [`history`]); and a rejection can carry a note the
//! model reads ([`approval::with_note`]). A policy change is not queued as
//! a user message: the restated `permissions` section tells the model.

pub mod approval;
pub mod bash;
pub mod beneath;
pub mod broker;
pub mod context;
pub mod engine;
pub mod fs;
pub mod history;
pub mod presets;
pub mod state;

use std::path::Path;
use std::sync::Arc;

use bake_sandbox::{SandboxMode, SandboxPolicy, SandboxProvider};
use serde_json::{Value, json};

pub use approval::{
    ASK_SENTENCE, ApprovalDecision, ApprovalOffers, ApprovalPolicy, ApprovalRequest, Approver,
    NEVER_SENTENCE, NeverApprover, NoApprover, PolicyApprover,
};
pub use broker::{ApprovalAnswer, ApprovalBroker, ApprovalListener, ApprovalPrompt};
pub use engine::{
    BlankJustification, EscalationLayer, LayerDecision, PermissionEngine, PolicyLayer,
    StandingModeLayer, StandingPermissions, ToolCall,
};
pub use history::{
    APPROVAL_CUSTOM_TYPE, ApprovalEntry, ApprovalHistory, ApprovalRecorder, RecorderSlot,
    SessionGrant, approval_history,
};
pub use presets::{CUSTOM_PRESET, KnobDefaults, KnobState, PresetSpec, PresetTable};
pub use state::PERMISSION_CUSTOM_TYPE;

use crate::session::SessionManager;
use crate::tools::bash::BashToolOptions;
use crate::tools::definition::ToolDefinition;

/// What the wrapped tools share: the engine that resolves each call, the
/// provider that confines bash, and the escalations the tools offer.
#[derive(Clone)]
pub struct ToolPermissions {
    /// Resolves each call's policy.
    pub engine: Arc<PermissionEngine>,
    /// Confines bash under that policy.
    pub provider: Arc<dyn SandboxProvider>,
    /// The standing permission the offer follows.
    standing: Arc<StandingPermissions>,
    /// Whether anyone can answer an approval prompt, so escalation is
    /// offered at all.
    escalates: bool,
}

impl std::fmt::Debug for ToolPermissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolPermissions")
            .field("engine", &self.engine)
            .field("provider", &self.provider.name())
            .field("escalation_targets", &self.escalation_targets())
            .finish()
    }
}

impl ToolPermissions {
    /// The shipped composition for `standing`: the standard engine whose
    /// approvals go through the session's policy to `answerer`, offering
    /// the escalations `standing` can reach as it changes.
    pub fn new(
        standing: Arc<StandingPermissions>,
        answerer: Arc<dyn Approver>,
        provider: Arc<dyn SandboxProvider>,
    ) -> Self {
        let approver = Arc::new(PolicyApprover::new(Arc::clone(&standing), answerer));
        Self {
            engine: Arc::new(PermissionEngine::standard(Arc::clone(&standing), approver)),
            provider,
            standing,
            escalates: true,
        }
    }

    /// These permissions offering no escalation: for a composition with no
    /// one to answer an approval prompt, such as `-p`, where every request
    /// would fail.
    pub fn without_escalation(mut self) -> Self {
        self.escalates = false;
        self
    }

    /// The modes a call can escalate to now, narrowest first; see
    /// [`reachable_escalation_targets`]. The tools advertise the
    /// escalation parameters and add the denial hint only for these, so
    /// when the set is empty the model sees neither. A tool's schema takes
    /// them when it is built, and a session's at each prompt
    /// ([`refresh_escalation_offer`]); the hint reads them at each call.
    pub fn escalation_targets(&self) -> &'static [SandboxMode] {
        if self.escalates {
            reachable_escalation_targets(self.standing.sandbox(), self.standing.approval())
        } else {
            &[]
        }
    }

    /// Whether a call denied under `mode` can still be retried wider, so its
    /// denial carries the escalation hint.
    pub fn offers_escalation_beyond(&self, mode: SandboxMode) -> bool {
        self.escalation_targets()
            .iter()
            .any(|&target| mode.is_widened_by(target))
    }
}

/// `tool` with the escalation offer for `targets` when it is one of the
/// sandboxed tools (`bash`, `write`, `edit`), so a permission switch reaches
/// its schema; any other tool, or one already offering `targets`, is
/// returned as it is. `bash`'s description and parameters, and the file
/// tools' parameters, are rebuilt as [`sandboxed_bash_tool`] and
/// [`sandbox_file_tools`] build them.
pub fn refresh_escalation_offer(
    tool: Arc<bake_agent::AgentTool>,
    targets: &[SandboxMode],
) -> Arc<bake_agent::AgentTool> {
    let strip = |parameters: &Value| {
        let mut parameters = parameters.clone();
        if let Some(properties) = parameters
            .get_mut("properties")
            .and_then(Value::as_object_mut)
        {
            properties.remove("sandbox_permissions");
            properties.remove("justification");
        }
        parameters
    };
    let (description, parameters) = match tool.name.as_str() {
        "bash" => {
            let background = tool
                .parameters
                .get("properties")
                .and_then(|properties| properties.get("run_in_background"))
                .is_some();
            (
                bash::sandboxed_bash_tool_description(targets, background),
                bash::sandboxed_bash_tool_parameters(targets, background),
            )
        }
        "write" | "edit" => (
            tool.description.clone(),
            with_escalation_parameters(
                strip(&tool.parameters),
                targets,
                fs::FS_SANDBOX_PERMISSIONS_DESCRIPTION,
                fs::FS_JUSTIFICATION_DESCRIPTION,
            ),
        ),
        _ => return tool,
    };
    if description == tool.description && parameters == tool.parameters {
        return tool;
    }
    let mut refreshed = (*tool).clone();
    refreshed.description = description;
    refreshed.parameters = parameters;
    Arc::new(refreshed)
}

/// What a running session holds of its permission: the preset table, the
/// standing permission its tools resolve under, and whether anyone can
/// answer an approval prompt.
#[derive(Clone)]
pub struct LivePermission {
    /// The preset table in force.
    pub table: PresetTable,
    /// The standing permission, shared with the session's engine.
    pub standing: Arc<StandingPermissions>,
    /// Whether an approval prompt can be answered, as in an interactive or
    /// RPC session; `-p` cannot.
    pub can_ask: bool,
}

impl std::fmt::Debug for LivePermission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LivePermission")
            .field("standing", &self.standing)
            .field("can_ask", &self.can_ask)
            .finish_non_exhaustive()
    }
}

impl LivePermission {
    /// The modes the tools offer to escalate to now.
    pub fn escalation_targets(&self) -> &'static [SandboxMode] {
        if self.can_ask {
            reachable_escalation_targets(self.standing.sandbox(), self.standing.approval())
        } else {
            &[]
        }
    }

    /// The `permissions` section as the standing permission states it now
    /// ([`SessionPermission::prompt_text`]).
    pub fn prompt_text(&self) -> String {
        let policy = SandboxPolicy::new(self.standing.sandbox(), self.standing.workspace_root());
        let approval = if self.can_ask {
            self.standing.approval()
        } else {
            ApprovalPolicy::Never
        };
        context::permissions_section_text(&policy, approval)
    }
}

/// The modes a session can escalate a call to: those strictly wider than
/// its standing `sandbox` mode, and none under the `never` approval policy,
/// which rejects every request: only what an approval could grant is
/// offered, not every
/// [`ESCALATION_TARGETS`](bake_sandbox::ESCALATION_TARGETS) mode.
pub fn reachable_escalation_targets(
    sandbox: SandboxMode,
    approval: ApprovalPolicy,
) -> &'static [SandboxMode] {
    match approval {
        ApprovalPolicy::Ask => sandbox.wider_modes(),
        ApprovalPolicy::Never => &[],
    }
}

/// `parameters` with the two escalation parameters appended to its
/// properties: `sandbox_permissions` (enum `targets`) and `justification`,
/// neither required. Without targets `parameters` is returned unchanged.
pub fn with_escalation_parameters(
    mut parameters: Value,
    targets: &[SandboxMode],
    sandbox_permissions: &str,
    justification: &str,
) -> Value {
    if targets.is_empty() {
        return parameters;
    }
    let targets: Vec<&str> = targets.iter().map(|mode| mode.as_str()).collect();
    if let Some(properties) = parameters
        .get_mut("properties")
        .and_then(Value::as_object_mut)
    {
        properties.insert(
            "sandbox_permissions".into(),
            json!({ "type": "string", "enum": targets, "description": sandbox_permissions }),
        );
        properties.insert(
            "justification".into(),
            json!({ "type": "string", "description": justification }),
        );
    }
    parameters
}

/// Replaces `write` and `edit` in `definitions` with their sandboxed forms,
/// whose blocking work has no owner; other tools are unchanged.
pub fn sandbox_file_tools(
    definitions: Vec<ToolDefinition>,
    cwd: &Path,
    permissions: &ToolPermissions,
) -> Vec<ToolDefinition> {
    owned_sandbox_file_tools(definitions, cwd, permissions, None)
}

/// [`sandbox_file_tools`] with the sandboxed forms' blocking work under
/// `owner`, the session's gate.
pub(crate) fn owned_sandbox_file_tools(
    definitions: Vec<ToolDefinition>,
    cwd: &Path,
    permissions: &ToolPermissions,
    owner: Option<crate::owned_work::WorkGate>,
) -> Vec<ToolDefinition> {
    definitions
        .into_iter()
        .map(|definition| match definition.name() {
            "write" => fs::owned_sandboxed_write_tool_definition(cwd, permissions, owner.clone()),
            "edit" => fs::owned_sandboxed_edit_tool_definition(cwd, permissions, owner.clone()),
            _ => definition,
        })
        .collect()
}

/// The sandboxed bash tool; see [`bash::create_sandboxed_bash_tool_definition`].
pub fn sandboxed_bash_tool(
    cwd: &Path,
    options: BashToolOptions,
    permissions: &ToolPermissions,
) -> Result<ToolDefinition, String> {
    bash::create_sandboxed_bash_tool_definition(cwd, options, permissions)
}

/// Where a session's permission comes from at startup.
#[derive(Debug, Clone, Default)]
pub struct PermissionInputs<'a> {
    /// `settings.json` `permission`: `presets` and `defaultPreset`.
    pub settings: Option<&'a Value>,
    /// `--permission`: the preset to select.
    pub preset: Option<&'a str>,
    /// `--sandbox`: the sandbox mode to set.
    pub sandbox: Option<SandboxMode>,
    /// `--approval`: the approval policy to set.
    pub approval: Option<ApprovalPolicy>,
}

/// A session's permission after startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPermission {
    /// The preset table in force.
    pub table: PresetTable,
    /// The sandbox mode in force.
    pub sandbox: SandboxMode,
    /// The approval policy in force.
    pub approval: ApprovalPolicy,
    /// The preset the knobs derive to, or `custom`.
    pub preset: String,
    /// Problems that did not stop startup.
    pub warnings: Vec<String>,
}

impl SessionPermission {
    /// The standing permission of the session `session_id` in
    /// `workspace_root`, offering the table's full-access preset to an
    /// approval's full-access answer.
    pub fn standing(
        &self,
        workspace_root: &Path,
        session_id: Option<String>,
    ) -> StandingPermissions {
        StandingPermissions::new(
            self.sandbox,
            self.approval,
            self.preset.clone(),
            workspace_root,
            session_id,
        )
        .with_full_access_preset(self.table.full_access_preset().map(str::to_owned))
    }

    /// The text of the system prompt's `permissions` section for a session
    /// in `workspace_root`. Without anyone to answer an approval prompt,
    /// as in `-p`, every escalation fails, so the section states the
    /// `never` policy whatever the preset's.
    pub fn prompt_text(&self, workspace_root: &Path, can_ask: bool) -> String {
        let policy = SandboxPolicy::new(self.sandbox, workspace_root);
        context::permissions_section_text(&policy, self.stated_approval(can_ask))
    }

    /// The approval policy the model is told: the session's, or `never`
    /// when no one can answer.
    pub fn stated_approval(&self, can_ask: bool) -> ApprovalPolicy {
        if can_ask {
            self.approval
        } else {
            ApprovalPolicy::Never
        }
    }
}

/// Settles a session's permission at startup, writing any change to its
/// session file: the table from `settings.json`; the default preset from
/// `settings.json`, else the composition defaults' preset; then a fresh session
/// pins `--permission` or that default, a resumed one keeps its own, and
/// `--permission`, `--sandbox`, and `--approval` switch it in that order.
pub fn prepare_session_permission(
    session: &mut SessionManager,
    inputs: &PermissionInputs<'_>,
) -> Result<SessionPermission, String> {
    let defaults = KnobDefaults::default();
    let table = PresetTable::from_settings(inputs.settings)?;
    let warnings = Vec::new();
    let configured = presets::settings_default_preset(inputs.settings)?;
    let default_preset = table.default_preset(configured.as_deref(), defaults)?;
    if let Some(preset) = inputs.preset
        && table.spec(preset).is_none()
    {
        return Err(format!(
            "Unknown preset \"{preset}\" (available: {})",
            table.names().join(", ")
        ));
    }
    state::pin_initial_permission(
        session,
        &table,
        inputs.preset.unwrap_or(&default_preset),
        defaults,
    )?;
    if let Some(preset) = inputs.preset {
        state::set_preset(session, &table, defaults, preset)?;
    }
    if let Some(mode) = inputs.sandbox {
        state::set_sandbox_mode(session, defaults, mode)?;
    }
    if let Some(policy) = inputs.approval {
        state::set_approval_policy(session, defaults, policy)?;
    }
    let knobs = state::permission_state(session)?.knobs;
    let (sandbox, approval) = knobs.effective(defaults);
    let preset = table.derive(&knobs, defaults);
    Ok(SessionPermission {
        table,
        sandbox,
        approval,
        preset,
        warnings,
    })
}

#[cfg(all(test, unix))]
mod race_tests;
#[cfg(test)]
mod tests;
