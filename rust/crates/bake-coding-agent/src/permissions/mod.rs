//! Bake's permission layer over the sandbox contract (D24): permission
//! presets, the approval policy and answerers, a layered engine that
//! resolves each tool call's [`SandboxPolicy`], and Pi's `bash`, `write`,
//! and `edit` tools wrapped by it. `read`, `grep`, `find`, and `ls` are
//! unchanged, as in Bake's TypeScript.
//!
//! A retained Bake contract, ported from Bake's TypeScript and proved by
//! fixture tests derived from its sources and specs (each test names the
//! one it follows):
//!
//! | Module | TypeScript source |
//! |---|---|
//! | [`presets`] | `packages/interaction/permission-presets/src/{index,types}.ts` |
//! | [`approval`] | `packages/interaction/user-approval/src/{index,types}.ts` |
//! | [`state`] | the `permission/preset`, `sandbox/mode`, and `approval/policy` events and their folds (`permission-presets/src/index.ts`, `packages/sandbox/sandbox-policy/src/session-mode.ts`, `user-approval/src/index.ts`) |
//! | [`engine`] | `packages/sandbox/sandbox-policy/src/index.ts` (`resolve`), `packages/sandbox/sandbox/src/escalation.ts` (`approveEscalation`), the escalation steps of `packages/shell/tool-bash/src/index.ts` and `packages/fs/tool-fs/src/sandbox.ts` |
//! | [`context`] | `renderPolicyContext` in `sandbox-policy/src/index.ts`, the `approval:policy` contribution in `user-approval/src/index.ts` |
//! | [`bash`] | `packages/shell/bash-sandbox/src/{index,helpers}.ts`, the sandbox parts of `packages/shell/tool-bash/src/{index,render}.ts` |
//! | [`fs`](mod@fs) | `packages/fs/fs-sandbox/src/index.ts`, the sandbox parts of `packages/fs/tool-fs/src/{sandbox,write,edit}.ts` |
//! | [`beneath`] | Bake's own: the link-refusing open of a checked write target, closing the race `fs-sandbox/src/index.ts` accepted |
//! | [`import`](mod@import) | the D25 read of `permission.defaultPreset` from the 0.3 `settings.yaml` |
//!
//! Every model-visible string (tool parameters and the sandbox paragraph,
//! denial and hint markers, refusals, and policy sentences) is the
//! TypeScript text byte for byte; where it meets Pi's text, Pi's stays and
//! Bake's is added as the TypeScript tool added it.
//!
//! # Fail closed
//!
//! A call runs only under the policy the engine resolved. A confined mode
//! no backend can enforce refuses the bash call ([`bake_sandbox::SandboxUnavailable`]);
//! a file mutation outside the policy is refused before any effect; an
//! escalation nobody approves is refused before anything runs.
//!
//! # Not ported
//!
//! The `approval/asked` and `approval/decided` audit events (no interactive
//! answerer exists yet, and the headless run's requests resolve without
//! one); the live `/permission` command and its "approval policy changed"
//! notice; the Auto review integration (`registerAuto`); background bash
//! jobs and their `runnerFailed` notice; the Cordis projection, catalog,
//! and invariant companions.

pub mod approval;
pub mod bash;
pub mod beneath;
pub mod context;
pub mod engine;
pub mod fs;
pub mod import;
pub mod presets;
pub mod state;

use std::path::Path;
use std::sync::Arc;

use bake_sandbox::{ESCALATION_TARGETS, SandboxMode, SandboxPolicy, SandboxProvider};
use serde_json::{Value, json};

pub use approval::{
    ASK_SENTENCE, ApprovalPolicy, ApprovalRequest, Approver, NEVER_SENTENCE, NeverApprover,
    NoApprover, PolicyApprover,
};
pub use engine::{
    BlankJustification, EscalationLayer, LayerDecision, PermissionEngine, PolicyLayer,
    StandingModeLayer, StandingPermissions, ToolCall,
};
pub use presets::{CUSTOM_PRESET, KnobDefaults, KnobState, PresetSpec, PresetTable};
pub use state::PERMISSION_CUSTOM_TYPE;

use crate::session::SessionManager;
use crate::tools::bash::BashToolOptions;
use crate::tools::definition::ToolDefinition;

/// What the wrapped tools share: the engine that resolves each call and the
/// provider that confines bash.
#[derive(Clone)]
pub struct ToolPermissions {
    /// Resolves each call's policy.
    pub engine: Arc<PermissionEngine>,
    /// Confines bash under that policy.
    pub provider: Arc<dyn SandboxProvider>,
}

impl std::fmt::Debug for ToolPermissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolPermissions")
            .field("engine", &self.engine)
            .field("provider", &self.provider.name())
            .finish()
    }
}

impl ToolPermissions {
    /// The shipped composition for `standing`: the standard engine whose
    /// approvals go through the session's policy to `answerer`.
    pub fn new(
        standing: Arc<StandingPermissions>,
        answerer: Arc<dyn Approver>,
        provider: Arc<dyn SandboxProvider>,
    ) -> Self {
        let approver = Arc::new(PolicyApprover::new(Arc::clone(&standing), answerer));
        Self {
            engine: Arc::new(PermissionEngine::standard(standing, approver)),
            provider,
        }
    }
}

/// `parameters` with the two escalation parameters appended to its
/// properties: `sandbox_permissions` (enum [`ESCALATION_TARGETS`]) and
/// `justification`, neither required.
pub fn with_escalation_parameters(
    mut parameters: Value,
    sandbox_permissions: &str,
    justification: &str,
) -> Value {
    let targets: Vec<&str> = ESCALATION_TARGETS
        .iter()
        .map(|mode| mode.as_str())
        .collect();
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

/// Replaces `write` and `edit` in `definitions` with their sandboxed forms;
/// other tools are unchanged.
pub fn sandbox_file_tools(
    definitions: Vec<ToolDefinition>,
    cwd: &Path,
    permissions: &ToolPermissions,
) -> Vec<ToolDefinition> {
    definitions
        .into_iter()
        .map(|definition| match definition.name() {
            "write" => fs::create_sandboxed_write_tool_definition(cwd, permissions),
            "edit" => fs::create_sandboxed_edit_tool_definition(cwd, permissions),
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
    /// `permission.defaultPreset` imported from the 0.3 `settings.yaml`.
    pub imported_default_preset: Option<&'a str>,
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
    /// `workspace_root`.
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
    }

    /// The system-prompt section stating this permission for a session in
    /// `workspace_root`: ([`context::PERMISSIONS_SECTION`], text).
    pub fn prompt_section(&self, workspace_root: &Path) -> (String, String) {
        let policy = SandboxPolicy::new(self.sandbox, workspace_root);
        (
            context::PERMISSIONS_SECTION.to_owned(),
            context::permissions_section_text(&policy, self.approval),
        )
    }
}

/// Settles a session's permission at startup, writing any change to its
/// session file: the table from `settings.json`; the default preset from
/// `settings.json`, else the 0.3 import (a stale imported name is a
/// warning), else the composition defaults' preset; then a fresh session
/// pins `--permission` or that default, a resumed one keeps its own, and
/// `--permission`, `--sandbox`, and `--approval` switch it in that order.
pub fn prepare_session_permission(
    session: &mut SessionManager,
    inputs: &PermissionInputs<'_>,
) -> Result<SessionPermission, String> {
    let defaults = KnobDefaults::default();
    let table = PresetTable::from_settings(inputs.settings)?;
    let mut warnings = Vec::new();
    let mut configured = presets::settings_default_preset(inputs.settings)?;
    if configured.is_none()
        && let Some(imported) = inputs.imported_default_preset
    {
        if table.spec(imported).is_some() {
            configured = Some(imported.to_owned());
        } else {
            warnings.push(format!(
                "Ignoring the imported default permission preset \"{imported}\": it names no preset (available: {})",
                table.names().join(", ")
            ));
        }
    }
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
