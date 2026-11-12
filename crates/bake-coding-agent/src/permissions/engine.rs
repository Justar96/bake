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
//! the session's `ask`/`never` policy, and records the request and its
//! answer in the session file once a session binds its recorder
//! ([`super::history`]). Under `ask`, a session grant of the same tool for
//! the same mode decides the request without asking; an
//! `allowed-full-access` answer switches the standing permission once it is
//! written, so it applies from the next call to resolve. The wrapped tools
//! only call
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

use super::approval::{
    ApprovalDecision, ApprovalOffers, ApprovalPolicy, ApprovalRequest, Approver, with_note,
};
use super::history::{ApprovalAsked, ApprovalEntry, RecorderSlot, SessionGrant};
use super::state::PermissionChange;

/// The session's standing permission: the knobs in force, the preset they
/// came from, the workspace root (the session's working directory), the
/// session id, the session grants, and the preset a full-access answer
/// switches to. Shared by the standing layer, the approval policy, and the
/// engine; a live switch (`/permission`, or a full-access answer) updates
/// it through [`StandingPermissions::set`], and the next call to resolve
/// reads the change.
pub struct StandingPermissions {
    state: RwLock<Standing>,
    workspace_root: PathBuf,
    session_id: Option<String>,
    grants: RwLock<Vec<SessionGrant>>,
    full_access: Option<String>,
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
            grants: RwLock::new(Vec::new()),
            full_access: None,
        }
    }

    /// These permissions with `preset`, a full-access preset of the
    /// session's table ([`super::PresetTable::full_access_preset`]), as
    /// what an `allowed-full-access` answer switches to; `None` offers no
    /// such answer.
    #[must_use]
    pub fn with_full_access_preset(mut self, preset: Option<String>) -> Self {
        self.full_access = preset;
        self
    }

    /// The preset a full-access answer switches to, when one is offered.
    pub fn full_access_preset(&self) -> Option<&str> {
        self.full_access.as_deref()
    }

    /// Adds `grant` unless the same tool and mode are granted already.
    pub fn grant(&self, grant: SessionGrant) {
        let mut grants = self.grants.write().unwrap_or_else(PoisonError::into_inner);
        if !grants
            .iter()
            .any(|held| held.tool_name == grant.tool_name && held.sandbox == grant.sandbox)
        {
            grants.push(grant);
        }
    }

    /// The session grants, in the order given.
    pub fn grants(&self) -> Vec<SessionGrant> {
        self.grants
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The grant that decides a request of `tool_name` for `mode`: one for
    /// exactly that tool and mode, and only under the `ask` policy.
    pub fn granted(&self, tool_name: &str, mode: SandboxMode) -> Option<SessionGrant> {
        if self.approval() != ApprovalPolicy::Ask {
            return None;
        }
        self.grants
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|grant| grant.tool_name == tool_name && grant.sandbox == mode)
            .cloned()
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

    /// Replaces the knobs; later calls resolve under them, and a call
    /// already resolved keeps its policy. The tools read their escalation
    /// offer from these knobs ([`super::ToolPermissions::escalation_targets`]);
    /// a session refreshes its tools' schemas at its next prompt
    /// ([`super::refresh_escalation_offer`]). A running MCP server keeps the
    /// confinement it was started with (see `crate::mcp`'s lifetime
    /// boundaries).
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
    /// The tool call that made this one, when it is a nested call.
    pub parent_tool_call_id: Option<&'a str>,
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
    recorder: RecorderSlot,
    /// The session's standing permission, which holds its grants and takes
    /// a full-access answer's switch; `None` offers neither wider answer.
    standing: Option<Arc<StandingPermissions>>,
}

impl fmt::Debug for PermissionEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PermissionEngine")
            .field("layers", &self.layer_names())
            .field("recorder", &self.recorder)
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
            recorder: RecorderSlot::default(),
            standing: None,
        }
    }

    /// The shipped engine: [`StandingModeLayer`], then [`EscalationLayer`],
    /// with `standing`'s session grants and full-access switch.
    /// `approver` should apply the session's policy, as
    /// [`super::approval::PolicyApprover`] does.
    pub fn standard(standing: Arc<StandingPermissions>, approver: Arc<dyn Approver>) -> Self {
        let mut engine = Self::new(approver)
            .with_layer(Arc::new(StandingModeLayer::new(Arc::clone(&standing))))
            .with_layer(Arc::new(EscalationLayer));
        engine.standing = Some(standing);
        engine
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

    /// Where this engine records its approval requests and answers: empty
    /// until a session binds its recorder
    /// ([`crate::agent_session::AgentSession::record_approvals`]).
    pub fn recorder_slot(&self) -> &RecorderSlot {
        &self.recorder
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
                    match self
                        .approve(call, need.reason, need.grant.mode, signal)
                        .await
                    {
                        Ok(decision) if decision.outcome.allows() => policy = Some(need.grant),
                        Ok(decision) => {
                            return Err(with_note(
                                (need.refusal)(decision.outcome),
                                decision.note.as_deref(),
                            ));
                        }
                        Err(text) => return Err(text),
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

    /// Asks the approver whether `call` may run under `mode`, recording the
    /// request and its answer when a recorder is bound (`ApprovalService.request`
    /// in `user-approval/src/index.ts`, lines 208-227): the `asked` entry is
    /// written before the approver is asked, and the `decided` entry before
    /// the answer is returned, so an allowed answer is on disk before the
    /// call can run. The answer returned is the one written; a request whose
    /// `asked` entry an abort kept out is `cancelled` without asking. `Err`
    /// is the call's refusal when a write failed; the session has stopped.
    ///
    /// Under `ask`, a session grant for the call's tool and `mode` answers
    /// `allowed-session` in the approver's place, recorded with the grant's
    /// request id. Once an answer is written, an `allowed-session` adds its
    /// grant and an `allowed-full-access` switches the standing permission,
    /// its switch written in the decision's group.
    async fn approve(
        &self,
        call: &ToolCall<'_>,
        reason: String,
        mode: SandboxMode,
        signal: Option<&AbortSignal>,
    ) -> Result<ApprovalDecision, String> {
        let standing = self.standing.as_deref();
        let offers = ApprovalOffers {
            session: standing.is_some(),
            full_access: standing
                .and_then(StandingPermissions::full_access_preset)
                .is_some(),
        };
        let request = ApprovalRequest {
            tool_name: call.tool_name.to_owned(),
            call_id: call.call_id.to_owned(),
            parent_tool_call_id: call.parent_tool_call_id.map(str::to_owned),
            reason,
            arguments: call.args.clone(),
            sandbox: Some(mode),
            offers,
        };
        let granted = standing.and_then(|standing| standing.granted(call.tool_name, mode));
        let recorder = self.recorder.get();
        let id = crate::session::id::uuid_v4();
        if let Some(recorder) = recorder {
            let asked = ApprovalEntry::Asked(ApprovalAsked {
                id: id.clone(),
                tool_name: request.tool_name.clone(),
                call_id: Some(request.call_id.clone()),
                reason: Some(request.reason.clone()),
                sandbox: Some(mode),
            });
            if recorder.record(asked, signal.cloned()).await?.is_none() {
                return Ok(EscalationOutcome::Cancelled.into());
            }
        }
        let decision = match &granted {
            Some(_) if signal.is_some_and(AbortSignal::aborted) => {
                EscalationOutcome::Cancelled.into()
            }
            Some(_) => EscalationOutcome::AllowedSession.into(),
            None => self.approver.decide(request, signal.cloned()).await,
        };
        // What a full-access answer changes, from the knobs in force now.
        let switch = standing
            .filter(|_| decision.outcome == EscalationOutcome::AllowedFullAccess)
            .and_then(|standing| {
                let preset = standing.full_access_preset()?;
                let spec = super::presets::PresetSpec {
                    sandbox: SandboxMode::DangerFullAccess,
                    approval: ApprovalPolicy::Never,
                    name: None,
                    description: None,
                };
                Some((
                    preset.to_owned(),
                    PermissionChange::to_preset(
                        (&standing.preset(), standing.sandbox(), standing.approval()),
                        preset,
                        &spec,
                    ),
                ))
            });
        let outcome = match recorder {
            None => decision.outcome,
            Some(recorder) => {
                let decided = ApprovalEntry::Decided {
                    id: id.clone(),
                    outcome: decision.outcome,
                    note: decision.note.clone(),
                    granted_by: granted.as_ref().map(|grant| grant.granted_by.clone()),
                };
                let written = match &switch {
                    Some((_, change)) => {
                        recorder
                            .record_switch(decided, change.clone(), signal.cloned())
                            .await?
                    }
                    None => recorder.record(decided, signal.cloned()).await?,
                };
                written
                    .and_then(|entry| entry.outcome())
                    .unwrap_or(EscalationOutcome::Cancelled)
            }
        };
        match (outcome, standing) {
            (EscalationOutcome::AllowedSession, Some(standing)) if granted.is_none() => {
                standing.grant(SessionGrant {
                    tool_name: call.tool_name.to_owned(),
                    sandbox: mode,
                    granted_by: id,
                });
            }
            (EscalationOutcome::AllowedFullAccess, Some(standing)) => {
                if let Some((preset, _)) = switch {
                    standing.set(SandboxMode::DangerFullAccess, ApprovalPolicy::Never, preset);
                }
            }
            _ => {}
        }
        Ok(ApprovalDecision {
            outcome,
            note: decision
                .note
                .filter(|_| outcome == EscalationOutcome::Rejected),
        })
    }
}
