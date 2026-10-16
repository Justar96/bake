//! The layered permission engine: resolves each tool call's
//! [`SandboxPolicy`] before the tool runs.
//!
//! Bake's TypeScript resolved a call's policy in two fixed steps, the
//! session's standing policy (`SandboxPolicyService.resolve` in
//! `packages/sandbox/sandbox-policy/src/index.ts`) and a one-call escalation
//! (`approveEscalation` in `packages/sandbox/sandbox/src/escalation.ts`, with
//! the argument handling of `packages/shell/tool-bash/src/index.ts` and
//! `packages/fs/tool-fs/src/sandbox.ts`). Here those steps are the two
//! shipped [`PolicyLayer`]s, [`StandingModeLayer`] and [`EscalationLayer`],
//! and the engine runs any ordered list of layers.
//!
//! # Adding a layer
//!
//! A layer sees the call ([`ToolCall`]: tool name, call id, raw arguments)
//! and the policy the earlier layers produced, and answers with a
//! [`LayerDecision`]: pass, replace the policy (narrower or wider), refuse
//! with model-visible text, or require approval for a policy. The engine
//! asks the [`Approver`] for that last one, so every approval goes through
//! the session's `ask`/`never` policy. The wrapped tools only call
//! [`PermissionEngine::resolve`], so new layers need no tool change:
//!
//! - command-prefix rules for bash, or path rules for the file tools: a
//!   layer between the two shipped ones that reads `ToolCall::args` and
//!   widens (a pre-approved prefix) or refuses;
//! - per-tool allow and deny: a layer that refuses by `ToolCall::tool_name`;
//! - an auto-reviewer: a layer after [`EscalationLayer`] whose future asks
//!   a reviewer and refuses or passes.
//!
//! Install one with [`PermissionEngine::insert_layer`]. A refusal ends the
//! resolution; later layers do not run.

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};

use bake_agent::BoxFuture;
use bake_ai::AbortSignal;
use bake_sandbox::escalation::{
    EscalationCheck, check_escalation, escalation_reason, validate_escalation_args,
};
use bake_sandbox::{EscalationOutcome, SandboxMode, SandboxPolicy};
use serde_json::Value;

use super::approval::{ApprovalPolicy, ApprovalRequest, Approver};

/// The session's standing permission: the knobs in force, the preset they
/// came from, the workspace root (the session's working directory), and
/// the session id. Shared by the standing layer and the approval policy;
/// a live switch (a future `/permission` command) updates it through
/// [`StandingPermissions::set`].
pub struct StandingPermissions {
    state: RwLock<Standing>,
    workspace_root: PathBuf,
    session_id: Option<String>,
}

#[derive(Debug, Clone)]
struct Standing {
    sandbox: SandboxMode,
    approval: ApprovalPolicy,
    preset: String,
}

impl StandingPermissions {
    /// The standing permission of the session `session_id` working in
    /// `workspace_root`. The root is canonicalized here, once, and every
    /// policy built from it carries that pinned path: moving the workspace
    /// later and leaving a link in its place does not move the policy.
    pub fn new(
        sandbox: SandboxMode,
        approval: ApprovalPolicy,
        preset: impl Into<String>,
        workspace_root: impl Into<PathBuf>,
        session_id: Option<String>,
    ) -> Self {
        Self {
            state: RwLock::new(Standing {
                sandbox,
                approval,
                preset: preset.into(),
            }),
            workspace_root: bake_sandbox::canonical_path(&workspace_root.into()),
            session_id,
        }
    }

    fn read(&self) -> Standing {
        self.state
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The standing sandbox mode.
    pub fn sandbox(&self) -> SandboxMode {
        self.read().sandbox
    }

    /// The standing approval policy.
    pub fn approval(&self) -> ApprovalPolicy {
        self.read().approval
    }

    /// The preset the knobs derive to, or `custom`.
    pub fn preset(&self) -> String {
        self.read().preset
    }

    /// The workspace root, canonicalized at construction.
    pub fn workspace_root(&self) -> &std::path::Path {
        &self.workspace_root
    }

    /// The standing policy for one call (`SandboxPolicyService.resolve`).
    pub fn policy(&self) -> SandboxPolicy {
        let policy = SandboxPolicy::new(self.sandbox(), self.workspace_root.clone());
        match &self.session_id {
            Some(id) => policy.with_session_id(id.clone()),
            None => policy,
        }
    }

    /// Replaces the knobs; later calls resolve under them. The tools'
    /// escalation offer was fixed when they were built
    /// ([`super::ToolPermissions::new`]), so a switch that changes which
    /// modes are reachable must rebuild them.
    pub fn set(&self, sandbox: SandboxMode, approval: ApprovalPolicy, preset: impl Into<String>) {
        *self.state.write().unwrap_or_else(PoisonError::into_inner) = Standing {
            sandbox,
            approval,
            preset: preset.into(),
        };
    }
}

impl fmt::Debug for StandingPermissions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let standing = self.read();
        f.debug_struct("StandingPermissions")
            .field("sandbox", &standing.sandbox)
            .field("approval", &standing.approval)
            .field("preset", &standing.preset)
            .field("workspace_root", &self.workspace_root)
            .finish_non_exhaustive()
    }
}

/// How a tool reads a blank `justification` sent without
/// `sandbox_permissions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlankJustification {
    /// As absent: `tool-bash`'s `validateBashArgs`.
    IgnoredAlone,
    /// As given, so the pairing check refuses it: `tool-fs`'s
    /// `resolvePolicy`.
    Checked,
}

/// One tool call, as layers see it.
#[derive(Debug, Clone, Copy)]
pub struct ToolCall<'a> {
    /// The tool's name, recorded with an approval request.
    pub tool_name: &'a str,
    /// The tool call's id.
    pub call_id: &'a str,
    /// The call's validated arguments.
    pub args: &'a Value,
    /// The noun the escalation texts use: `command` for bash, `operation`
    /// for the file tools.
    pub subject: &'a str,
    /// How the tool reads a lone blank justification.
    pub blank_justification: BlankJustification,
}

/// Turns a non-grant outcome into the call's refusal text.
pub type RefusalText = Arc<dyn Fn(EscalationOutcome) -> String + Send + Sync>;

/// A request for approval: on `allowed-once` the call runs under `grant`,
/// otherwise it is refused with `refusal(outcome)`.
pub struct ApprovalNeed {
    /// The reason recorded with the request.
    pub reason: String,
    /// The policy the call runs under once approved.
    pub grant: SandboxPolicy,
    /// The refusal text for each other outcome.
    pub refusal: RefusalText,
}

impl fmt::Debug for ApprovalNeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApprovalNeed")
            .field("reason", &self.reason)
            .field("grant", &self.grant)
            .finish_non_exhaustive()
    }
}

/// A layer's answer for one call.
#[derive(Debug)]
pub enum LayerDecision {
    /// Keep the policy so far.
    Pass,
    /// Replace the policy, narrower or wider.
    SetPolicy(SandboxPolicy),
    /// Refuse the call; the text is the tool's error.
    Refuse(String),
    /// Ask the approver; the answer decides the policy or the refusal.
    RequireApproval(ApprovalNeed),
}

/// One policy layer. See the module documentation for where layers go.
pub trait PolicyLayer: Send + Sync {
    /// A short name, for diagnostics.
    fn name(&self) -> &str;

    /// Decides `call` given `policy`, the policy so far (`None` before any
    /// layer set one). Dropping the future cancels the decision.
    fn evaluate<'a>(
        &'a self,
        call: &'a ToolCall<'a>,
        policy: Option<&'a SandboxPolicy>,
    ) -> BoxFuture<'a, LayerDecision>;
}

/// Sets the session's standing policy: its mode, its workspace root, and its
/// session id.
pub struct StandingModeLayer {
    standing: Arc<StandingPermissions>,
}

impl StandingModeLayer {
    /// The layer for `standing`.
    pub fn new(standing: Arc<StandingPermissions>) -> Self {
        Self { standing }
    }
}

impl PolicyLayer for StandingModeLayer {
    fn name(&self) -> &str {
        "standing-mode"
    }

    fn evaluate<'a>(
        &'a self,
        _call: &'a ToolCall<'a>,
        _policy: Option<&'a SandboxPolicy>,
    ) -> BoxFuture<'a, LayerDecision> {
        let policy = self.standing.policy();
        Box::pin(std::future::ready(LayerDecision::SetPolicy(policy)))
    }
}

/// The one-call escalation: `sandbox_permissions` with a `justification`
/// asks for a strictly wider mode for this call only. A request for access
/// the call already has passes without approval or argument checks; the
/// pairing is checked next; a strict widening needs approval; anything
/// else is refused with the TypeScript text.
#[derive(Debug, Clone, Copy, Default)]
pub struct EscalationLayer;

/// The escalation arguments of `args`: a missing or `null` member is
/// absent; a member of another type is refused, since the schema promises
/// strings.
fn escalation_arg<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(format!("invalid escalation: {key} must be a string")),
    }
}

impl EscalationLayer {
    fn decide(call: &ToolCall<'_>, policy: Option<&SandboxPolicy>) -> LayerDecision {
        let (requested, justification) = match (
            escalation_arg(call.args, "sandbox_permissions"),
            escalation_arg(call.args, "justification"),
        ) {
            (Ok(requested), Ok(justification)) => (requested, justification),
            (Err(text), _) | (_, Err(text)) => return LayerDecision::Refuse(text),
        };
        let Some(policy) = policy else {
            return if requested.is_none() && justification.is_none() {
                LayerDecision::Pass
            } else {
                // tool-bash's and tool-fs's texts; `subject` tells them apart.
                LayerDecision::Refuse(format!(
                    "sandbox_permissions is not available in this composition (no sandboxing {} to escalate)",
                    if call.subject == "command" {
                        "executor"
                    } else {
                        "filesystem"
                    }
                ))
            };
        };
        if let Some(requested) = requested
            && check_escalation(policy.mode, requested) == EscalationCheck::NoOp
        {
            return LayerDecision::Pass;
        }
        let justification = match (call.blank_justification, requested, justification) {
            (BlankJustification::IgnoredAlone, None, Some(text))
                if crate::tools::js::trim(text).is_empty() =>
            {
                None
            }
            (_, _, justification) => justification,
        };
        if let Err(text) = validate_escalation_args(requested, justification) {
            return LayerDecision::Refuse(text);
        }
        let (Some(requested), Some(justification)) = (requested, justification) else {
            return LayerDecision::Pass;
        };
        match check_escalation(policy.mode, requested) {
            EscalationCheck::NoOp => LayerDecision::Pass,
            EscalationCheck::Refused(text) => LayerDecision::Refuse(text),
            EscalationCheck::Widen(mode) => {
                let subject = call.subject.to_owned();
                LayerDecision::RequireApproval(ApprovalNeed {
                    reason: escalation_reason(mode, justification),
                    grant: policy.clone().with_mode(mode),
                    refusal: Arc::new(move |outcome| {
                        outcome.refusal(&subject, mode).unwrap_or_default()
                    }),
                })
            }
        }
    }
}

impl PolicyLayer for EscalationLayer {
    fn name(&self) -> &str {
        "escalation"
    }

    fn evaluate<'a>(
        &'a self,
        call: &'a ToolCall<'a>,
        policy: Option<&'a SandboxPolicy>,
    ) -> BoxFuture<'a, LayerDecision> {
        Box::pin(std::future::ready(Self::decide(call, policy)))
    }
}

/// Resolves tool calls' policies through ordered layers.
pub struct PermissionEngine {
    layers: Vec<Arc<dyn PolicyLayer>>,
    approver: Arc<dyn Approver>,
}

impl fmt::Debug for PermissionEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PermissionEngine")
            .field("layers", &self.layer_names())
            .finish_non_exhaustive()
    }
}

impl PermissionEngine {
    /// An engine with no layers; every call is refused until a layer sets
    /// a policy.
    pub fn new(approver: Arc<dyn Approver>) -> Self {
        Self {
            layers: Vec::new(),
            approver,
        }
    }

    /// The shipped engine: [`StandingModeLayer`], then [`EscalationLayer`].
    /// `approver` should apply the session's policy, as
    /// [`super::approval::PolicyApprover`] does.
    pub fn standard(standing: Arc<StandingPermissions>, approver: Arc<dyn Approver>) -> Self {
        Self::new(approver)
            .with_layer(Arc::new(StandingModeLayer::new(standing)))
            .with_layer(Arc::new(EscalationLayer))
    }

    /// The engine with `layer` last.
    #[must_use]
    pub fn with_layer(mut self, layer: Arc<dyn PolicyLayer>) -> Self {
        self.layers.push(layer);
        self
    }

    /// Inserts `layer` at `index`, clamped to the end.
    pub fn insert_layer(&mut self, index: usize, layer: Arc<dyn PolicyLayer>) {
        let index = index.min(self.layers.len());
        self.layers.insert(index, layer);
    }

    /// The layers' names, in order.
    pub fn layer_names(&self) -> Vec<&str> {
        self.layers.iter().map(|layer| layer.name()).collect()
    }

    /// The policy `call` runs under, or its refusal text. `signal` is the
    /// call's abort signal; approval requests ride it.
    pub async fn resolve(
        &self,
        call: &ToolCall<'_>,
        signal: Option<&AbortSignal>,
    ) -> Result<SandboxPolicy, String> {
        let mut policy: Option<SandboxPolicy> = None;
        for layer in &self.layers {
            match layer.evaluate(call, policy.as_ref()).await {
                LayerDecision::Pass => {}
                LayerDecision::SetPolicy(next) => policy = Some(next),
                LayerDecision::Refuse(text) => return Err(text),
                LayerDecision::RequireApproval(need) => {
                    let outcome = self
                        .approver
                        .request(
                            ApprovalRequest {
                                tool_name: call.tool_name.to_owned(),
                                call_id: call.call_id.to_owned(),
                                reason: need.reason,
                                arguments: call.args.clone(),
                            },
                            signal.cloned(),
                        )
                        .await;
                    match outcome {
                        EscalationOutcome::AllowedOnce => policy = Some(need.grant),
                        outcome => return Err((need.refusal)(outcome)),
                    }
                }
            }
        }
        policy.ok_or_else(|| {
            format!(
                "permission: no policy layer resolved a sandbox policy for this {} call",
                call.tool_name
            )
        })
    }
}
