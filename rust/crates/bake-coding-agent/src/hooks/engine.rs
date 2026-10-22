//! A session's hooks: which hooks an event runs, the stdin payload of each
//! dialect, and what the merged outcome does at each hook point.
//!
//! Ported from Bake 0.3's bridges, `packages/hooks/hooks-claude-code/src/
//! index.ts` and `packages/hooks/hooks-codex/src/index.ts` (`runPoint`, the
//! per-event payload builders, and each listener's decision mapping), and
//! the durable record of `hook-protocol/src/events.ts`
//! (`appendHookResult`'s decision rule and stderr summary). The texts the
//! model or the user sees are 0.3's.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use bake_ai::AbortSignal;
use bake_sandbox::EscalationOutcome;
use serde_json::{Map, Value, json};

use super::codec::HookOutput;
use super::config::MatcherGroup;
use super::matcher::matches_matcher;
use super::merge::{MergedDecision, MergedOutcome, merge_hook_outputs};
use super::runner::{HookProcesses, HookRun, run_hook};
use super::{HookDialect, HookEvent, HookFile, HookScope, HooksOptions};
use crate::owned_work::WorkGate;
use crate::permissions::{ApprovalRequest, Approver, PolicyApprover};

/// The model's text for a call a `PreToolUse` hook denied without a reason.
pub const PRE_TOOL_USE_BLOCKED: &str = "blocked by PreToolUse hook";
/// The model's text for a result a `PostToolUse` hook blocked without a
/// reason.
pub const POST_TOOL_USE_BLOCKED: &str = "blocked by PostToolUse hook";
/// The model's text for a `Stop` hook's block without a reason.
pub const STOP_BLOCKED: &str = "continue: blocked by Stop hook";
/// The most `Stop` continuations one prompt takes in a row. A `Stop` hook
/// that blocks again after them is overruled: the run ends with
/// [`stop_cap_note`]. 0.3 had no bound (its `TODO(stop-loop-guard)`).
pub const MAX_STOP_HOOK_CONTINUATIONS: u32 = 8;

/// The note the model and the user see when a run ends at
/// [`MAX_STOP_HOOK_CONTINUATIONS`].
pub fn stop_cap_note() -> String {
    format!(
        "Stop hooks kept this run going {MAX_STOP_HOOK_CONTINUATIONS} times in a row, so it ended without continuing again."
    )
}

/// The error a prompt a `UserPromptSubmit` hook blocked is refused with,
/// followed by `: <reason>` when the hook gave one.
pub const PROMPT_BLOCKED: &str = "Prompt blocked by UserPromptSubmit hook";

/// The most characters of a hook's standard error its record keeps
/// (0.3's `DEFAULT_STDERR_SUMMARY_MAX_CHARS`).
pub const STDERR_SUMMARY_MAX_CHARS: usize = 500;

/// The `source` of `SessionStart`: a session with no conversation yet
/// starts, or one with a conversation resumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSource {
    Startup,
    Resume,
}

impl SessionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Resume => "resume",
        }
    }
}

/// What an event's hooks are told, beside the session.
#[derive(Debug, Clone, Copy)]
pub enum EventInput<'a> {
    SessionStart {
        source: SessionSource,
    },
    UserPromptSubmit {
        prompt: &'a str,
    },
    PreToolUse {
        tool_name: &'a str,
        tool_use_id: &'a str,
        tool_input: &'a Value,
    },
    PostToolUse {
        tool_name: &'a str,
        tool_use_id: &'a str,
        tool_input: &'a Value,
        tool_response: &'a str,
    },
    Stop {
        stop_hook_active: bool,
    },
}

impl EventInput<'_> {
    pub fn event(&self) -> HookEvent {
        match self {
            Self::SessionStart { .. } => HookEvent::SessionStart,
            Self::UserPromptSubmit { .. } => HookEvent::UserPromptSubmit,
            Self::PreToolUse { .. } => HookEvent::PreToolUse,
            Self::PostToolUse { .. } => HookEvent::PostToolUse,
            Self::Stop { .. } => HookEvent::Stop,
        }
    }

    /// What matchers select: the tool name, or the session's source.
    fn subject(&self) -> &str {
        match self {
            Self::SessionStart { source } => source.as_str(),
            Self::PreToolUse { tool_name, .. } | Self::PostToolUse { tool_name, .. } => tool_name,
            Self::UserPromptSubmit { .. } | Self::Stop { .. } => "",
        }
    }
}

/// The session facts every payload carries.
#[derive(Debug, Clone)]
pub struct PayloadBase {
    pub session_id: String,
    pub cwd: PathBuf,
    /// The model's id, Codex's `model`.
    pub model: String,
    /// Codex's `turn_id`.
    pub turn_id: String,
}

/// `dialect`'s stdin payload for `input`. Claude Code's
/// `transcript_path` is `""` and Codex's `null`, as in 0.3: the session
/// file is not in either tool's transcript format.
pub fn payload(dialect: HookDialect, input: &EventInput<'_>, base: &PayloadBase) -> Value {
    let event = input.event();
    let mut object = Map::new();
    object.insert("session_id".into(), base.session_id.clone().into());
    object.insert(
        "transcript_path".into(),
        match dialect {
            HookDialect::ClaudeCode => "".into(),
            HookDialect::Codex => Value::Null,
        },
    );
    object.insert("cwd".into(), base.cwd.to_string_lossy().into_owned().into());
    object.insert("hook_event_name".into(), event.as_str().into());
    if dialect == HookDialect::Codex {
        object.insert("model".into(), base.model.clone().into());
        object.insert("permission_mode".into(), "default".into());
        if event != HookEvent::SessionStart {
            object.insert("turn_id".into(), base.turn_id.clone().into());
        }
    }
    // Codex shows a tool's input as its shell `command`, or `""`.
    let tool_input = |input: &Value| match dialect {
        HookDialect::ClaudeCode => input.clone(),
        HookDialect::Codex => json!({
            "command": input.get("command").and_then(Value::as_str).unwrap_or_default(),
        }),
    };
    match input {
        EventInput::SessionStart { source } => {
            object.insert("source".into(), source.as_str().into());
        }
        EventInput::UserPromptSubmit { prompt } => {
            object.insert("prompt".into(), (*prompt).into());
        }
        EventInput::PreToolUse {
            tool_name,
            tool_use_id,
            tool_input: input,
        } => {
            object.insert("tool_name".into(), (*tool_name).into());
            object.insert("tool_input".into(), tool_input(input));
            object.insert("tool_use_id".into(), (*tool_use_id).into());
        }
        EventInput::PostToolUse {
            tool_name,
            tool_use_id,
            tool_input: input,
            tool_response,
        } => {
            object.insert("tool_name".into(), (*tool_name).into());
            object.insert("tool_input".into(), tool_input(input));
            object.insert("tool_use_id".into(), (*tool_use_id).into());
            object.insert("tool_response".into(), (*tool_response).into());
        }
        EventInput::Stop { stop_hook_active } => {
            object.insert("stop_hook_active".into(), (*stop_hook_active).into());
            if dialect == HookDialect::Codex {
                object.insert("last_assistant_message".into(), Value::Null);
            }
        }
    }
    Value::Object(object)
}

/// Per-session hook state.
#[derive(Debug, Default)]
pub struct HookState {
    /// `SessionStart`'s decision was written in this process, or no
    /// hook matched it.
    pub session_started: bool,
    /// A Claude Code tool hook's `continue: false` ends the current run
    /// once its batch's calls finish.
    pub halted: bool,
    /// The run was last continued by a `Stop` hook.
    pub stop_active: bool,
    /// The `Stop` continuations of the current prompt, at most
    /// [`MAX_STOP_HOOK_CONTINUATIONS`].
    pub stop_continuations: u32,
}

/// Every hook an event ran, with each dialect's merged outcome.
#[derive(Debug, Clone)]
pub struct Evaluated {
    /// Each dialect that ran a hook, in order, and its merged outcome.
    pub outcomes: Vec<(HookDialect, MergedOutcome)>,
    /// Each hook's record, for the decision entry.
    pub hooks: Vec<Value>,
}

/// A session's hooks.
pub struct Hooks {
    files: Vec<HookFile>,
    scope: HookScope,
    processes: HookProcesses,
    approver: PolicyApprover,
    state: Mutex<HookState>,
}

impl std::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooks")
            .field("files", &self.files)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

/// Whether `merged` ends the event before `dialect`'s successors run:
/// the outcomes on which 0.3's listener returned without calling `next()`.
fn ends_event(event: HookEvent, dialect: HookDialect, merged: &MergedOutcome) -> bool {
    let deny = merged.decision == MergedDecision::Deny;
    match (event, dialect) {
        (HookEvent::UserPromptSubmit, _) => deny || merged.stop,
        (HookEvent::PreToolUse, HookDialect::ClaudeCode) => {
            deny || merged.stop || merged.decision == MergedDecision::Ask
        }
        (HookEvent::PreToolUse, HookDialect::Codex) => deny,
        (HookEvent::PostToolUse, HookDialect::ClaudeCode) => deny,
        (HookEvent::PostToolUse, HookDialect::Codex) => deny || merged.stop,
        (HookEvent::SessionStart | HookEvent::Stop, _) => false,
    }
}

/// 0.3's `summarizeStderr`: trimmed, `None` when empty, cut with `…`.
fn summarize_stderr(stderr: &str) -> Option<String> {
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut chars = trimmed.char_indices();
    match chars.nth(STDERR_SUMMARY_MAX_CHARS) {
        Some((end, _)) => Some(format!("{}…", &trimmed[..end])),
        None => Some(trimmed.to_owned()),
    }
}

/// A hook's stable id in its file, `<event>/<group>/<hook>`: the indexes
/// of its matcher group among the event's groups and of the hook in that
/// group. Like 0.3's opaque `handlerId`, it stands in for the command,
/// whose text may hold secrets and is never recorded.
fn handler_id(event: HookEvent, group: usize, hook: usize) -> String {
    format!("{}/{group}/{hook}", event.as_str())
}

/// One hook's record: 0.3's `hook/invoked` and `hook/result` together.
/// It holds neither the command text nor the hook's `systemMessage`.
fn record(dialect: HookDialect, group: &MatcherGroup, handler_id: String, run: &HookRun) -> Value {
    let output = &run.output;
    let decision = match (output.decision, output.continue_) {
        (Some(decision), _) => decision.as_str(),
        (None, Some(false)) => "stop",
        (None, _) => "pass",
    };
    let mut record = Map::new();
    record.insert("dialect".into(), dialect.as_str().into());
    if let Some(matcher) = &group.matcher {
        record.insert("matcher".into(), matcher.clone().into());
    }
    record.insert("handlerId".into(), handler_id.into());
    record.insert("decision".into(), decision.into());
    if let Some(code) = output.exit_code {
        record.insert("exitCode".into(), code.into());
    }
    if run.timed_out {
        record.insert("timedOut".into(), true.into());
    }
    record.insert("durationMs".into(), run.duration_ms.into());
    if let Some(stderr) = summarize_stderr(&output.stderr) {
        record.insert("stderr".into(), stderr.into());
    }
    if output.updated_input {
        record.insert("updatedInput".into(), "ignored".into());
    }
    Value::Object(record)
}

/// Codex reads a clean, plain standard output as context on these events
/// when no structured context was given.
fn plain_stdout_as_context(dialect: HookDialect, event: HookEvent, output: &mut HookOutput) {
    if dialect == HookDialect::Codex
        && matches!(event, HookEvent::SessionStart | HookEvent::UserPromptSubmit)
        && output.exit_code == Some(0)
        && output.additional_context.is_none()
        && !output.stdout.is_empty()
        && !output.stdout.starts_with('{')
    {
        output.additional_context = Some(output.stdout.clone());
    }
}

impl Hooks {
    /// The hooks of `options`, whose processes `owner` owns.
    pub fn new(options: HooksOptions, owner: WorkGate) -> Self {
        let HooksOptions {
            files,
            scope,
            standing,
            sandbox,
            approver,
            default_timeout,
        } = options;
        Self {
            files,
            scope,
            approver: PolicyApprover::new(std::sync::Arc::clone(&standing), approver),
            processes: HookProcesses {
                owner,
                standing,
                sandbox,
                default_timeout,
            },
            state: Mutex::default(),
        }
    }

    /// Whether `event` has a hook this session runs.
    pub fn runs(&self, event: HookEvent) -> bool {
        let in_scope = match self.scope {
            HookScope::Session => true,
            HookScope::ToolsOnly => {
                matches!(event, HookEvent::PreToolUse | HookEvent::PostToolUse)
            }
        };
        in_scope
            && self
                .files
                .iter()
                .any(|file| !file.groups.of(event).is_empty())
    }

    pub fn state(&self) -> MutexGuard<'_, HookState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs the hooks `input`'s event matches, dialect by dialect, each
    /// with its payload, until a dialect's outcome ends the event. `None`
    /// when no hook matched. `signal` kills a running hook and leaves the
    /// rest unrun; the caller then acts on nothing.
    pub async fn evaluate(
        &self,
        input: &EventInput<'_>,
        base: &PayloadBase,
        signal: Option<&AbortSignal>,
    ) -> Option<Evaluated> {
        let event = input.event();
        if !self.runs(event) {
            return None;
        }
        let mut evaluated = Evaluated {
            outcomes: Vec::new(),
            hooks: Vec::new(),
        };
        for file in &self.files {
            let dialect = file.dialect;
            let mut outputs = Vec::new();
            for (group_index, group) in file.groups.of(event).iter().enumerate() {
                if !matches_matcher(group.matcher.as_deref(), input.subject(), dialect) {
                    continue;
                }
                for (hook_index, hook) in group.hooks.iter().enumerate() {
                    let mut stdin = payload(dialect, input, base).to_string().into_bytes();
                    // Claude Code ends the payload with a newline; Codex
                    // does not.
                    if dialect == HookDialect::ClaudeCode {
                        stdin.push(b'\n');
                    }
                    let env: Vec<(OsString, OsString)> = match dialect {
                        HookDialect::ClaudeCode => vec![(
                            "CLAUDE_PROJECT_DIR".into(),
                            base.cwd.clone().into_os_string(),
                        )],
                        HookDialect::Codex => Vec::new(),
                    };
                    let mut run = run_hook(
                        &self.processes,
                        hook,
                        &base.cwd,
                        &env,
                        stdin,
                        event.as_str(),
                        signal,
                    )
                    .await;
                    plain_stdout_as_context(dialect, event, &mut run.output);
                    let id = handler_id(event, group_index, hook_index);
                    evaluated.hooks.push(record(dialect, group, id, &run));
                    outputs.push(run.output);
                }
            }
            if outputs.is_empty() {
                continue;
            }
            let merged = merge_hook_outputs(&outputs);
            let ends = ends_event(event, dialect, &merged);
            evaluated.outcomes.push((dialect, merged));
            if ends {
                break;
            }
        }
        (!evaluated.hooks.is_empty()).then_some(evaluated)
    }

    /// Asks the user about a call a Claude Code `PreToolUse` hook asked
    /// about, under the session's approval policy.
    pub async fn ask(
        &self,
        request: ApprovalRequest,
        signal: Option<AbortSignal>,
    ) -> EscalationOutcome {
        self.approver.request(request, signal).await
    }
}

/// What a `UserPromptSubmit` or `SessionStart` evaluation does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptAction {
    /// Send it, with this context after it.
    Proceed { context: Vec<String> },
    /// Refuse it, with the hook's reason.
    Block { reason: Option<String> },
}

/// A `UserPromptSubmit` block or `continue: false` refuses the prompt.
pub fn prompt_action(outcomes: &[(HookDialect, MergedOutcome)]) -> PromptAction {
    let mut context = Vec::new();
    for (_, merged) in outcomes {
        if merged.decision == MergedDecision::Deny || merged.stop {
            return PromptAction::Block {
                reason: merged.reason.clone().or_else(|| merged.stop_reason.clone()),
            };
        }
        context.extend(merged.additional_context.iter().cloned());
    }
    PromptAction::Proceed { context }
}

/// `SessionStart` only adds context; a stop is recorded without effect.
pub fn session_start_context(outcomes: &[(HookDialect, MergedOutcome)]) -> Vec<String> {
    outcomes
        .iter()
        .flat_map(|(_, merged)| merged.additional_context.iter().cloned())
        .collect()
}

/// What a `PreToolUse` evaluation does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreToolAction {
    Proceed,
    /// Deny the call with this text; `halt` ends the run after its batch.
    Deny {
        reason: String,
        halt: bool,
    },
    /// Ask the user (Claude Code only).
    Ask {
        reason: Option<String>,
    },
}

/// Claude Code: `continue: false` denies with its `stopReason` and halts,
/// a deny denies, an `ask` asks. Codex: only a deny counts.
pub fn pre_tool_action(outcomes: &[(HookDialect, MergedOutcome)]) -> PreToolAction {
    for (dialect, merged) in outcomes {
        let deny = merged.decision == MergedDecision::Deny;
        match dialect {
            HookDialect::ClaudeCode if merged.stop => {
                return PreToolAction::Deny {
                    reason: merged
                        .stop_reason
                        .clone()
                        .or_else(|| merged.reason.clone())
                        .unwrap_or_else(|| PRE_TOOL_USE_BLOCKED.to_owned()),
                    halt: true,
                };
            }
            HookDialect::ClaudeCode if merged.decision == MergedDecision::Ask => {
                return PreToolAction::Ask {
                    reason: merged.reason.clone(),
                };
            }
            _ if deny => {
                return PreToolAction::Deny {
                    reason: merged
                        .reason
                        .clone()
                        .unwrap_or_else(|| PRE_TOOL_USE_BLOCKED.to_owned()),
                    halt: false,
                };
            }
            _ => {}
        }
    }
    PreToolAction::Proceed
}

/// What a `PostToolUse` evaluation does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PostToolAction {
    /// Replace the result with this error text.
    pub feedback: Option<String>,
    /// Context for the next request.
    pub context: Vec<String>,
    /// End the run once the batch's calls finish.
    pub halt: bool,
}

/// Claude Code: a deny blocks with feedback, and `continue: false` keeps
/// the result, halts, and passes its `stopReason` on as context. Codex: a
/// deny or `continue: false` blocks with the reason, else the stop reason,
/// and the run goes on. Context accumulates either way.
pub fn post_tool_action(outcomes: &[(HookDialect, MergedOutcome)]) -> PostToolAction {
    let mut action = PostToolAction::default();
    for (dialect, merged) in outcomes {
        action
            .context
            .extend(merged.additional_context.iter().cloned());
        let deny = merged.decision == MergedDecision::Deny;
        match dialect {
            HookDialect::ClaudeCode => {
                if deny && action.feedback.is_none() {
                    action.feedback = Some(
                        merged
                            .reason
                            .clone()
                            .unwrap_or_else(|| POST_TOOL_USE_BLOCKED.to_owned()),
                    );
                }
                if merged.stop {
                    action.halt = true;
                    action.context.extend(merged.stop_reason.iter().cloned());
                }
            }
            HookDialect::Codex => {
                if (deny || merged.stop) && action.feedback.is_none() {
                    action.feedback = Some(
                        merged
                            .reason
                            .clone()
                            .or_else(|| merged.stop_reason.clone())
                            .unwrap_or_else(|| POST_TOOL_USE_BLOCKED.to_owned()),
                    );
                }
            }
        }
    }
    action
}

/// A `Stop` block without `continue: false` continues the run with its
/// reason; every dialect's reason is kept, in order.
pub fn stop_continuation(outcomes: &[(HookDialect, MergedOutcome)]) -> Option<String> {
    let reasons: Vec<String> = outcomes
        .iter()
        .filter(|(_, merged)| merged.decision == MergedDecision::Deny && !merged.stop)
        .map(|(_, merged)| {
            merged
                .reason
                .clone()
                .unwrap_or_else(|| STOP_BLOCKED.to_owned())
        })
        .collect();
    (!reasons.is_empty()).then(|| reasons.join("\n\n"))
}

/// The decision entry's data: the event, its decision, the event's facts,
/// and each hook's record.
pub fn decision_entry(
    event: HookEvent,
    decision: &str,
    facts: Map<String, Value>,
    hooks: Vec<Value>,
) -> Value {
    let mut data = Map::new();
    data.insert("event".into(), event.as_str().into());
    data.insert("decision".into(), decision.into());
    data.extend(facts);
    data.insert("hooks".into(), Value::Array(hooks));
    Value::Object(data)
}

#[cfg(test)]
mod tests {
    //! The decision mapping of 0.3's bridges (`hooks-claude-code/src/
    //! index.ts`, `hooks-codex/src/index.ts`) and their coverage cases.
    use super::*;

    const CLAUDE: HookDialect = HookDialect::ClaudeCode;
    const CODEX: HookDialect = HookDialect::Codex;

    fn merged(decision: MergedDecision) -> MergedOutcome {
        MergedOutcome {
            decision,
            reason: None,
            stop: false,
            stop_reason: None,
            additional_context: Vec::new(),
            system_messages: Vec::new(),
        }
    }

    fn with_reason(mut outcome: MergedOutcome, reason: &str) -> MergedOutcome {
        outcome.reason = Some(reason.into());
        outcome
    }

    fn stopping(reason: Option<&str>) -> MergedOutcome {
        MergedOutcome {
            stop: true,
            stop_reason: reason.map(str::to_owned),
            ..merged(MergedDecision::None)
        }
    }

    fn context(text: &str) -> MergedOutcome {
        MergedOutcome {
            additional_context: vec![text.into()],
            ..merged(MergedDecision::None)
        }
    }

    #[test]
    fn a_prompt_is_blocked_by_a_deny_or_a_stop_in_either_dialect() {
        for dialect in [CLAUDE, CODEX] {
            assert_eq!(
                prompt_action(&[(dialect, with_reason(merged(MergedDecision::Deny), "no"))]),
                PromptAction::Block {
                    reason: Some("no".into())
                }
            );
            assert_eq!(
                prompt_action(&[(dialect, stopping(Some("halt")))]),
                PromptAction::Block {
                    reason: Some("halt".into())
                }
            );
            assert_eq!(
                prompt_action(&[(dialect, context("ctx")), (CODEX, context("more"))]),
                PromptAction::Proceed {
                    context: vec!["ctx".into(), "more".into()]
                }
            );
        }
    }

    #[test]
    fn claude_pre_tool_use_denies_asks_and_halts() {
        assert_eq!(
            pre_tool_action(&[(CLAUDE, merged(MergedDecision::Deny))]),
            PreToolAction::Deny {
                reason: PRE_TOOL_USE_BLOCKED.into(),
                halt: false
            }
        );
        assert_eq!(
            pre_tool_action(&[(CLAUDE, with_reason(merged(MergedDecision::Ask), "sure?"))]),
            PreToolAction::Ask {
                reason: Some("sure?".into())
            }
        );
        let mut halting = with_reason(merged(MergedDecision::Allow), "ignored");
        halting.stop = true;
        halting.stop_reason = Some("budget".into());
        assert_eq!(
            pre_tool_action(&[(CLAUDE, halting)]),
            PreToolAction::Deny {
                reason: "budget".into(),
                halt: true
            }
        );
        assert_eq!(
            pre_tool_action(&[(CLAUDE, merged(MergedDecision::Allow))]),
            PreToolAction::Proceed
        );
    }

    #[test]
    fn codex_pre_tool_use_honors_only_a_deny() {
        assert_eq!(
            pre_tool_action(&[(CODEX, with_reason(merged(MergedDecision::Deny), "no"))]),
            PreToolAction::Deny {
                reason: "no".into(),
                halt: false
            }
        );
        for ignored in [
            merged(MergedDecision::Ask),
            merged(MergedDecision::Allow),
            stopping(Some("halt")),
        ] {
            assert_eq!(pre_tool_action(&[(CODEX, ignored)]), PreToolAction::Proceed);
        }
    }

    #[test]
    fn claude_post_tool_use_blocks_and_halts_keeping_the_result() {
        let action = post_tool_action(&[(CLAUDE, merged(MergedDecision::Deny))]);
        assert_eq!(action.feedback.as_deref(), Some(POST_TOOL_USE_BLOCKED));
        assert!(!action.halt);
        let action = post_tool_action(&[(CLAUDE, stopping(Some("enough")))]);
        assert_eq!(
            action,
            PostToolAction {
                feedback: None,
                context: vec!["enough".into()],
                halt: true
            }
        );
        let action = post_tool_action(&[(CLAUDE, context("note"))]);
        assert_eq!(action.context, ["note"]);
    }

    #[test]
    fn codex_post_tool_use_blocks_on_a_stop_and_goes_on() {
        let action = post_tool_action(&[(CODEX, stopping(Some("enough")))]);
        assert_eq!(
            action,
            PostToolAction {
                feedback: Some("enough".into()),
                context: Vec::new(),
                halt: false
            }
        );
        let mut both = with_reason(merged(MergedDecision::Deny), "reason");
        both.stop = true;
        both.stop_reason = Some("stop".into());
        assert_eq!(
            post_tool_action(&[(CODEX, both)]).feedback.as_deref(),
            Some("reason")
        );
        assert_eq!(
            post_tool_action(&[(CODEX, stopping(None))])
                .feedback
                .as_deref(),
            Some(POST_TOOL_USE_BLOCKED)
        );
    }

    #[test]
    fn a_stop_block_continues_unless_a_hook_stops() {
        assert_eq!(
            stop_continuation(&[(CODEX, merged(MergedDecision::Deny))]).as_deref(),
            Some(STOP_BLOCKED)
        );
        let mut overridden = with_reason(merged(MergedDecision::Deny), "more");
        overridden.stop = true;
        assert_eq!(stop_continuation(&[(CLAUDE, overridden)]), None);
        assert_eq!(
            stop_continuation(&[
                (CLAUDE, with_reason(merged(MergedDecision::Deny), "a")),
                (CODEX, with_reason(merged(MergedDecision::Deny), "b")),
            ])
            .as_deref(),
            Some("a\n\nb")
        );
        assert_eq!(
            stop_continuation(&[(CLAUDE, merged(MergedDecision::Allow))]),
            None
        );
    }

    #[test]
    fn which_outcomes_end_an_event_early() {
        let deny = merged(MergedDecision::Deny);
        let ask = merged(MergedDecision::Ask);
        let stop = stopping(None);
        assert!(ends_event(HookEvent::PreToolUse, CLAUDE, &ask));
        assert!(ends_event(HookEvent::PreToolUse, CLAUDE, &stop));
        assert!(!ends_event(HookEvent::PreToolUse, CODEX, &stop));
        assert!(ends_event(HookEvent::PreToolUse, CODEX, &deny));
        assert!(!ends_event(HookEvent::PostToolUse, CLAUDE, &stop));
        assert!(ends_event(HookEvent::PostToolUse, CODEX, &stop));
        assert!(ends_event(HookEvent::UserPromptSubmit, CODEX, &stop));
        assert!(!ends_event(HookEvent::Stop, CLAUDE, &deny));
        assert!(!ends_event(HookEvent::SessionStart, CLAUDE, &deny));
    }

    #[test]
    fn payloads_follow_each_dialect() {
        let base = PayloadBase {
            session_id: "s1".into(),
            cwd: PathBuf::from("/ws"),
            model: "m1".into(),
            turn_id: "3".into(),
        };
        let input = json!({ "command": "ls", "timeout": 5 });
        let pre = EventInput::PreToolUse {
            tool_name: "bash",
            tool_use_id: "c1",
            tool_input: &input,
        };
        assert_eq!(
            payload(CLAUDE, &pre, &base),
            json!({
                "session_id": "s1", "transcript_path": "", "cwd": "/ws",
                "hook_event_name": "PreToolUse", "tool_name": "bash",
                "tool_input": { "command": "ls", "timeout": 5 }, "tool_use_id": "c1",
            })
        );
        assert_eq!(
            payload(CODEX, &pre, &base),
            json!({
                "session_id": "s1", "transcript_path": null, "cwd": "/ws",
                "hook_event_name": "PreToolUse", "model": "m1", "permission_mode": "default",
                "turn_id": "3", "tool_name": "bash", "tool_input": { "command": "ls" },
                "tool_use_id": "c1",
            })
        );
        let start = EventInput::SessionStart {
            source: SessionSource::Resume,
        };
        assert_eq!(
            payload(CODEX, &start, &base),
            json!({
                "session_id": "s1", "transcript_path": null, "cwd": "/ws",
                "hook_event_name": "SessionStart", "model": "m1", "permission_mode": "default",
                "source": "resume",
            })
        );
        let stop = EventInput::Stop {
            stop_hook_active: true,
        };
        assert_eq!(
            payload(CLAUDE, &stop, &base),
            json!({
                "session_id": "s1", "transcript_path": "", "cwd": "/ws",
                "hook_event_name": "Stop", "stop_hook_active": true,
            })
        );
        assert_eq!(
            payload(CODEX, &stop, &base)["last_assistant_message"],
            Value::Null
        );
        let post = EventInput::PostToolUse {
            tool_name: "read",
            tool_use_id: "c2",
            tool_input: &json!({ "path": "a" }),
            tool_response: "text",
        };
        assert_eq!(
            payload(CODEX, &post, &base)["tool_input"],
            json!({ "command": "" })
        );
        assert_eq!(payload(CLAUDE, &post, &base)["tool_response"], "text");
    }

    #[test]
    fn codex_plain_stdout_is_context_on_prompt_events_only() {
        let plain = |exit_code, stdout: &str| HookOutput {
            exit_code,
            stdout: stdout.into(),
            ..HookOutput::default()
        };
        let mut output = plain(Some(0), "remember");
        plain_stdout_as_context(CODEX, HookEvent::UserPromptSubmit, &mut output);
        assert_eq!(output.additional_context.as_deref(), Some("remember"));
        for (dialect, event, mut output) in [
            (CLAUDE, HookEvent::UserPromptSubmit, plain(Some(0), "x")),
            (CODEX, HookEvent::PreToolUse, plain(Some(0), "x")),
            (CODEX, HookEvent::SessionStart, plain(Some(1), "x")),
            (CODEX, HookEvent::SessionStart, plain(Some(0), "{ bad json")),
        ] {
            plain_stdout_as_context(dialect, event, &mut output);
            assert_eq!(output.additional_context, None);
        }
    }

    #[test]
    fn stderr_summaries_are_trimmed_and_capped() {
        assert_eq!(summarize_stderr("  \n"), None);
        assert_eq!(summarize_stderr(" oops \n").as_deref(), Some("oops"));
        let long = "é".repeat(STDERR_SUMMARY_MAX_CHARS + 3);
        let summary = summarize_stderr(&long).expect("summary");
        assert_eq!(summary.chars().count(), STDERR_SUMMARY_MAX_CHARS + 1);
        assert!(summary.ends_with('…'));
    }
}
