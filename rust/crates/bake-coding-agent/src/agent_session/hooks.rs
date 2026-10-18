//! The session's external hooks ([`crate::hooks`]) at Pi v1.3.0's
//! extension points (`src/core/agent-session.ts`): `SessionStart` and
//! `UserPromptSubmit` where `_prepareRun` and `_runInputHandlers` run
//! (`session_start`, `input`), `PreToolUse` in the agent's
//! `before_tool_call` after the plugins' vetoes (`_beforeToolCall`,
//! `tool_call`), `PostToolUse` in its `after_tool_call` before image
//! normalization (`_afterToolCall`, `tool_result`), and `Stop` where the
//! run would settle (`_runBeforeSettleBoundary`, `agent_before_settle`).
//!
//! Every evaluation that ran a hook is written as a `custom` entry
//! ([`DECISION_ENTRY_TYPE`]) through the session's write boundary before
//! its decision takes effect, as plugin decisions are
//! (`agent_session/plugin_veto.rs`): before a denial or replaced result,
//! before a prompt is refused or sent with context, and before a `Stop`
//! continuation. The write holds a work permit, so shutdown waits for it,
//! and is admitted only if the evaluation was not cancelled by then; a
//! failed write records the gap, aborts the run, and fails the call.
//! Context, and a `Stop` hook's reason, reach the model as
//! `custom_message` entries ([`CONTEXT_MESSAGE_TYPE`]) the agent persists
//! before it sends them, so a resumed session rebuilds what the model saw.
//!
//! Hook processes run under the session's [`WorkGate`] ([`crate::hooks::runner`]);
//! a cancelled call, Esc, or shutdown kills the running hook and drops the
//! event's other hooks, and its outcome is not acted on.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use bake_agent::{
    AfterToolCall, AfterToolCallResult, AgentMessage as LiveMessage, AgentTurnDecision,
    BeforeToolCall, BeforeToolCallContext, BeforeToolCallResult, FinishTurn, RunControl, hook,
};
use bake_ai::{AbortController, AbortSignal, Message, TextContent, UserContent, UserContentBlock};
use bake_sandbox::EscalationOutcome;
use serde_json::{Map, Value, json};

use super::recovery::Core;
use super::{WriteRefused, write_session};
use crate::hooks::engine::{
    Evaluated, EventInput, Hooks, PROMPT_BLOCKED, PayloadBase, PreToolAction, PromptAction,
    SessionSource, decision_entry, post_tool_action, pre_tool_action, prompt_action,
    session_start_context, stop_continuation,
};
use crate::hooks::{CONTEXT_MESSAGE_TYPE, DECISION_ENTRY_TYPE, HookEvent};
use crate::permissions::ApprovalRequest;
use crate::session::messages::CustomMessage;

/// The text a cancelled call is blocked with, as the agent loop answers
/// an aborted call.
const ABORTED: &str = "Operation aborted";

/// A session's hooks and the facts their payloads carry.
pub(crate) struct SessionHooks {
    pub engine: Hooks,
    pub session_id: String,
    pub cwd: PathBuf,
    /// Context a `PostToolUse` hook added, for the next request.
    pending: Mutex<Vec<String>>,
}

impl SessionHooks {
    pub fn new(engine: Hooks, session_id: String, cwd: PathBuf) -> Self {
        Self {
            engine,
            session_id,
            cwd,
            pending: Mutex::default(),
        }
    }

    fn base(&self, core: &Core, messages: &[LiveMessage], next_prompt: bool) -> PayloadBase {
        let prompts = messages
            .iter()
            .filter(|message| matches!(message.as_llm(), Some(Message::User(_))))
            .count();
        PayloadBase {
            session_id: self.session_id.clone(),
            cwd: self.cwd.clone(),
            model: core.model().map(|model| model.id).unwrap_or_default(),
            turn_id: (prompts + usize::from(next_prompt)).to_string(),
        }
    }

    /// Takes the context `PostToolUse` hooks left for the next request.
    pub fn take_pending(&self) -> Option<LiveMessage> {
        let texts =
            std::mem::take(&mut *self.pending.lock().unwrap_or_else(PoisonError::into_inner));
        context_message(HookEvent::PostToolUse, texts)
    }
}

/// The cell [`super::AgentSession::set_hooks`] fills.
pub(crate) type SharedHooks = Arc<OnceLock<Arc<SessionHooks>>>;

/// `texts` as the model's context message, or `None` when empty.
pub(crate) fn context_message(event: HookEvent, texts: Vec<String>) -> Option<LiveMessage> {
    if texts.is_empty() {
        return None;
    }
    Some(LiveMessage::Custom(Arc::new(CustomMessage {
        custom_type: CONTEXT_MESSAGE_TYPE.to_owned(),
        content: UserContent::Blocks(
            texts
                .into_iter()
                .map(|text| UserContentBlock::Text(TextContent::new(text)))
                .collect(),
        ),
        display: true,
        details: Some(json!({ "event": event.as_str() })),
        timestamp: bake_ai::now_ms(),
    })))
}

/// Writes a decision entry at the session's write boundary unless
/// `cancelled` reports true at its admission, under the session lock;
/// `Ok(false)` then. A failure records the gap, aborts the run, and
/// returns the reason.
async fn write_decision(
    core: &Core,
    persist_error: &Mutex<Option<String>>,
    data: Value,
    cancelled: impl Fn() -> bool + Send + 'static,
) -> Result<bool, String> {
    let Some(permit) = core.work.enter() else {
        return Err("The session is shut down.".to_owned());
    };
    let session = Arc::clone(&core.session);
    let gap = Arc::clone(&core.gap);
    let write = permit.spawn_blocking(move || {
        write_session(&session, &gap, |session| {
            if cancelled() {
                return Ok(false);
            }
            session
                .append_custom_entry(DECISION_ENTRY_TYPE, Some(data))
                .map(|_| true)
                .map_err(|error| error.to_string())
        })
    });
    drop(permit);
    let written = write.map_err(|refused| refused.to_string())?.await;
    let error = match written {
        Ok(Ok(written)) => return Ok(written),
        Ok(Err(WriteRefused::AfterGap(error) | WriteRefused::Failed(error))) => error,
        Err(error) => {
            let error = format!("Session I/O task failed: {error}");
            super::lock_gap(&core.gap).get_or_insert_with(|| error.clone());
            error
        }
    };
    core.abort_requested.store(true, Ordering::SeqCst);
    if let Some(agent) = core.agent() {
        agent.abort();
    }
    persist_error
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(|| error.clone());
    Err(format!(
        "The hook decision could not be written to the session file ({error}), so the session stopped and takes no more prompts."
    ))
}

fn aborted() -> BeforeToolCallResult {
    BeforeToolCallResult {
        block: true,
        reason: Some(ABORTED.to_owned()),
        terminate: false,
    }
}

fn is_aborted(signal: Option<&AbortSignal>) -> bool {
    signal.is_some_and(AbortSignal::aborted)
}

fn tool_facts(call_id: &str, tool: &str) -> Map<String, Value> {
    let mut facts = Map::new();
    facts.insert("toolCallId".into(), call_id.into());
    facts.insert("toolName".into(), tool.into());
    facts
}

/// The text a Claude Code `ask` puts to the user.
fn ask_text(reason: Option<&str>) -> String {
    match reason {
        Some(reason) => format!("A PreToolUse hook asks to confirm this call: {reason}"),
        None => "A PreToolUse hook asks to confirm this call".to_owned(),
    }
}

/// The model's text for an `ask` that was not approved.
fn refusal(outcome: EscalationOutcome, reason: Option<&str>) -> String {
    let base = match outcome {
        EscalationOutcome::Unavailable => {
            "a PreToolUse hook asked to confirm this call, but no approval channel is available"
        }
        EscalationOutcome::Cancelled => "approval for this call was cancelled",
        EscalationOutcome::Rejected | EscalationOutcome::AllowedOnce => {
            "the user rejected this call, which a PreToolUse hook asked to confirm"
        }
    };
    match reason {
        Some(reason) => format!("{base}: {reason}"),
        None => base.to_owned(),
    }
}

/// The agent's `before_tool_call`: `plugins` first, then the `PreToolUse`
/// hooks. A plugin block or error ends there; a hook's deny, or an `ask`
/// the user does not approve, becomes the call's error result once its
/// decision entry is written.
pub(super) fn before_tool_call(
    core: Arc<Core>,
    persist_error: Arc<Mutex<Option<String>>>,
    hooks: SharedHooks,
    plugins: Option<BeforeToolCall>,
) -> BeforeToolCall {
    hook::before_tool_call(move |context, signal| {
        let core = Arc::clone(&core);
        let persist_error = Arc::clone(&persist_error);
        let hooks = hooks.get().cloned();
        let plugins = plugins.clone();
        Box::pin(async move {
            let BeforeToolCallContext {
                assistant_message,
                tool_call,
                args,
                context: agent_context,
            } = context;
            if let Some(plugins) = plugins {
                let decision = plugins(
                    BeforeToolCallContext {
                        assistant_message,
                        tool_call,
                        args: &mut *args,
                        context: agent_context,
                    },
                    signal.clone(),
                )
                .await?;
                if decision.as_ref().is_some_and(|decision| decision.block) {
                    return Ok(decision);
                }
            }
            let Some(hooks) = hooks.filter(|hooks| hooks.engine.runs(HookEvent::PreToolUse)) else {
                return Ok(None);
            };
            let args: &Value = args;
            let input = EventInput::PreToolUse {
                tool_name: &tool_call.name,
                tool_use_id: &tool_call.id,
                tool_input: args,
            };
            let base = hooks.base(&core, &agent_context.messages, false);
            let Some(evaluated) = hooks.engine.evaluate(&input, &base, signal.as_ref()).await
            else {
                return Ok(None);
            };
            if is_aborted(signal.as_ref()) {
                return Ok(Some(aborted()));
            }
            let mut facts = tool_facts(&tool_call.id, &tool_call.name);
            let (decision, result, halt) = match pre_tool_action(&evaluated.outcomes) {
                PreToolAction::Proceed => ("pass", None, false),
                PreToolAction::Deny { reason, halt } => {
                    facts.insert("reason".into(), reason.clone().into());
                    if halt {
                        facts.insert("halt".into(), true.into());
                    }
                    let result = BeforeToolCallResult {
                        block: true,
                        reason: Some(reason),
                        terminate: halt,
                    };
                    ("deny", Some(result), halt)
                }
                PreToolAction::Ask { reason } => {
                    let request = ApprovalRequest {
                        tool_name: tool_call.name.clone(),
                        call_id: tool_call.id.clone(),
                        reason: ask_text(reason.as_deref()),
                        arguments: args.clone(),
                    };
                    let outcome = hooks.engine.ask(request, signal.clone()).await;
                    if is_aborted(signal.as_ref()) {
                        return Ok(Some(aborted()));
                    }
                    facts.insert("approval".into(), outcome.as_str().into());
                    if outcome == EscalationOutcome::AllowedOnce {
                        ("allow", None, false)
                    } else {
                        let text = refusal(outcome, reason.as_deref());
                        facts.insert("reason".into(), text.clone().into());
                        let result = BeforeToolCallResult {
                            block: true,
                            reason: Some(text),
                            terminate: false,
                        };
                        ("deny", Some(result), false)
                    }
                }
            };
            let data = decision_entry(HookEvent::PreToolUse, decision, facts, evaluated.hooks);
            let cancel = signal.clone();
            if !write_decision(&core, &persist_error, data, move || {
                is_aborted(cancel.as_ref())
            })
            .await?
            {
                return Ok(Some(aborted()));
            }
            if halt {
                hooks.engine.state().halted = true;
            }
            Ok(result)
        })
    })
}

/// The agent's `after_tool_call`: the `PostToolUse` hooks, then `inner`
/// (image normalization and masking details) unless a hook replaced the
/// result with its feedback.
pub(super) fn after_tool_call(
    core: Arc<Core>,
    persist_error: Arc<Mutex<Option<String>>>,
    hooks: SharedHooks,
    inner: AfterToolCall,
) -> AfterToolCall {
    hook::after_tool_call(move |context, signal| {
        let core = Arc::clone(&core);
        let persist_error = Arc::clone(&persist_error);
        let hooks = hooks
            .get()
            .cloned()
            .filter(|hooks| hooks.engine.runs(HookEvent::PostToolUse));
        let inner = Arc::clone(&inner);
        Box::pin(async move {
            let Some(hooks) = hooks else {
                return inner(context, signal).await;
            };
            let response: String = context
                .result
                .content
                .iter()
                .filter_map(|block| match block {
                    UserContentBlock::Text(text) => Some(text.text.as_str()),
                    UserContentBlock::Image(_) => None,
                })
                .collect();
            let input = EventInput::PostToolUse {
                tool_name: &context.tool_call.name,
                tool_use_id: &context.tool_call.id,
                tool_input: context.args,
                tool_response: &response,
            };
            let base = hooks.base(&core, &context.context.messages, false);
            let Some(evaluated) = hooks.engine.evaluate(&input, &base, signal.as_ref()).await
            else {
                return inner(context, signal).await;
            };
            if is_aborted(signal.as_ref()) {
                return inner(context, signal).await;
            }
            let action = post_tool_action(&evaluated.outcomes);
            let mut facts = tool_facts(&context.tool_call.id, &context.tool_call.name);
            if let Some(feedback) = &action.feedback {
                facts.insert("reason".into(), feedback.clone().into());
            }
            if action.halt {
                facts.insert("halt".into(), true.into());
            }
            let decision = if action.feedback.is_some() {
                "block"
            } else if action.context.is_empty() {
                "pass"
            } else {
                "context"
            };
            let data = decision_entry(HookEvent::PostToolUse, decision, facts, evaluated.hooks);
            let cancel = signal.clone();
            if !write_decision(&core, &persist_error, data, move || {
                is_aborted(cancel.as_ref())
            })
            .await?
            {
                return inner(context, signal).await;
            }
            hooks
                .pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend(action.context);
            if action.halt {
                hooks.engine.state().halted = true;
            }
            match action.feedback {
                Some(feedback) => Ok(Some(AfterToolCallResult {
                    content: Some(vec![UserContentBlock::Text(TextContent::new(feedback))]),
                    is_error: Some(true),
                    ..AfterToolCallResult::default()
                })),
                None => inner(context, signal).await,
            }
        })
    })
}

/// The agent's `finish_turn`: a Claude Code tool hook's
/// `continue: false` ends the run once its turn's calls have finished.
pub(super) fn finish_turn(hooks: SharedHooks) -> FinishTurn {
    hook::finish_turn(move |_turn, _signal| {
        let halted = hooks.get().is_some_and(|hooks| hooks.engine.state().halted);
        Box::pin(async move { halted.then_some(AgentTurnDecision::End) })
    })
}

/// Runs an event's hooks inside a prompt's execution: a stop of `control`
/// (Esc or shutdown) kills a running hook and yields `None`.
async fn evaluate_in_run(
    hooks: &SessionHooks,
    input: &EventInput<'_>,
    base: &PayloadBase,
    control: &RunControl,
) -> Option<Evaluated> {
    let controller = AbortController::new();
    let signal = controller.signal();
    let evaluation = hooks.engine.evaluate(input, base, Some(&signal));
    tokio::pin!(evaluation);
    tokio::select! {
        evaluated = &mut evaluation => evaluated,
        _ = control.wait_stopped() => {
            controller.abort();
            let _ = evaluation.await;
            None
        }
    }
}

/// What a prompt's hooks decided.
pub(super) enum PromptHooks {
    /// Send it; the messages go before and after it.
    Send {
        before: Vec<LiveMessage>,
        after: Vec<LiveMessage>,
    },
    /// Refuse it with this error.
    Refuse(String),
    /// The run was stopped meanwhile.
    Stopped,
}

/// `SessionStart`, the first time, then `UserPromptSubmit` over `prompt`,
/// the expanded text, each written before it takes effect, and the
/// context `PostToolUse` hooks left; `messages` is the transcript.
pub(super) async fn before_prompt(
    hooks: &SessionHooks,
    core: &Core,
    persist_error: &Mutex<Option<String>>,
    messages: &[LiveMessage],
    prompt: &str,
    control: &RunControl,
) -> Result<PromptHooks, String> {
    let mut before = Vec::new();
    let mut after = Vec::new();
    let starting = hooks.engine.runs(HookEvent::SessionStart)
        && !std::mem::replace(&mut hooks.engine.state().session_started, true);
    if starting {
        let source = if messages
            .iter()
            .any(|message| message.as_user().is_some() || message.as_assistant().is_some())
        {
            SessionSource::Resume
        } else {
            SessionSource::Startup
        };
        let input = EventInput::SessionStart { source };
        let base = hooks.base(core, messages, true);
        if let Some(evaluated) = evaluate_in_run(hooks, &input, &base, control).await {
            if control.stopped().is_some() {
                return Ok(PromptHooks::Stopped);
            }
            let context = session_start_context(&evaluated.outcomes);
            let mut facts = Map::new();
            facts.insert("source".into(), source.as_str().into());
            let decision = if context.is_empty() {
                "pass"
            } else {
                "context"
            };
            let data = decision_entry(HookEvent::SessionStart, decision, facts, evaluated.hooks);
            let stop = control.clone();
            if !write_decision(core, persist_error, data, move || stop.stopped().is_some()).await? {
                return Ok(PromptHooks::Stopped);
            }
            before.extend(context_message(HookEvent::SessionStart, context));
        } else if control.stopped().is_some() {
            return Ok(PromptHooks::Stopped);
        }
    }
    before.extend(hooks.take_pending());
    if !hooks.engine.runs(HookEvent::UserPromptSubmit) {
        return Ok(PromptHooks::Send { before, after });
    }
    let input = EventInput::UserPromptSubmit { prompt };
    let base = hooks.base(core, messages, true);
    let Some(evaluated) = evaluate_in_run(hooks, &input, &base, control).await else {
        if control.stopped().is_some() {
            return Ok(PromptHooks::Stopped);
        }
        return Ok(PromptHooks::Send { before, after });
    };
    if control.stopped().is_some() {
        return Ok(PromptHooks::Stopped);
    }
    let action = prompt_action(&evaluated.outcomes);
    let (decision, facts, refused) = prompt_decision(&action);
    let data = decision_entry(
        HookEvent::UserPromptSubmit,
        decision,
        facts,
        evaluated.hooks,
    );
    let stop = control.clone();
    if !write_decision(core, persist_error, data, move || stop.stopped().is_some()).await? {
        return Ok(PromptHooks::Stopped);
    }
    if let Some(refused) = refused {
        return Ok(PromptHooks::Refuse(refused));
    }
    if let PromptAction::Proceed { context } = action {
        after.extend(context_message(HookEvent::UserPromptSubmit, context));
    }
    Ok(PromptHooks::Send { before, after })
}

/// The entry's decision and facts for a prompt's action, and the error a
/// refused prompt gets.
fn prompt_decision(action: &PromptAction) -> (&'static str, Map<String, Value>, Option<String>) {
    let mut facts = Map::new();
    match action {
        PromptAction::Block { reason } => {
            let refused = match reason {
                Some(reason) => {
                    facts.insert("reason".into(), reason.clone().into());
                    format!("{PROMPT_BLOCKED}: {reason}")
                }
                None => PROMPT_BLOCKED.to_owned(),
            };
            ("block", facts, Some(refused))
        }
        PromptAction::Proceed { context } if context.is_empty() => ("pass", facts, None),
        PromptAction::Proceed { .. } => ("context", facts, None),
    }
}

/// `UserPromptSubmit` over a steered or follow-up prompt, run on a task
/// of the session's gate, whose close kills it. `Ok(Some(context))` to
/// queue after the prompt, `Err` to refuse it.
pub(super) async fn before_queued_prompt(
    hooks: Arc<SessionHooks>,
    core: Arc<Core>,
    persist_error: Arc<Mutex<Option<String>>>,
    messages: Vec<LiveMessage>,
    prompt: String,
) -> Result<Option<LiveMessage>, String> {
    if !hooks.engine.runs(HookEvent::UserPromptSubmit) {
        return Ok(None);
    }
    let work = core.work.clone();
    let task = work
        .spawn(move |scope| async move {
            let input = EventInput::UserPromptSubmit { prompt: &prompt };
            let base = hooks.base(&core, &messages, true);
            let signal = scope.signal().clone();
            let Some(evaluated) = hooks.engine.evaluate(&input, &base, Some(&signal)).await else {
                return Ok(None);
            };
            if signal.aborted() {
                return Err("The session is shut down.".to_owned());
            }
            let action = prompt_action(&evaluated.outcomes);
            let (decision, facts, refused) = prompt_decision(&action);
            let data = decision_entry(
                HookEvent::UserPromptSubmit,
                decision,
                facts,
                evaluated.hooks,
            );
            let cancel = signal.clone();
            if !write_decision(&core, &persist_error, data, move || cancel.aborted()).await? {
                return Err("The session is shut down.".to_owned());
            }
            if let Some(refused) = refused {
                return Err(refused);
            }
            Ok(match action {
                PromptAction::Proceed { context } => {
                    context_message(HookEvent::UserPromptSubmit, context)
                }
                PromptAction::Block { .. } => None,
            })
        })
        .map_err(super::refused_message)?;
    task.await
        .map_err(|error| format!("The hook task failed: {error}"))?
}

/// `Stop` where a run would settle: the message continuing it, once its
/// decision is written, or `None` to settle. Not run after a halt.
pub(super) async fn before_settle(
    hooks: &SessionHooks,
    core: &Core,
    persist_error: &Mutex<Option<String>>,
    messages: &[LiveMessage],
    control: &RunControl,
) -> Result<Option<LiveMessage>, String> {
    if !hooks.engine.runs(HookEvent::Stop) || hooks.engine.state().halted {
        return Ok(None);
    }
    let active = hooks.engine.state().stop_active;
    let input = EventInput::Stop {
        stop_hook_active: active,
    };
    let base = hooks.base(core, messages, false);
    let Some(evaluated) = evaluate_in_run(hooks, &input, &base, control).await else {
        return Ok(None);
    };
    if control.stopped().is_some() {
        return Ok(None);
    }
    let continuation = stop_continuation(&evaluated.outcomes);
    let mut facts = Map::new();
    facts.insert("stopHookActive".into(), active.into());
    if let Some(reason) = &continuation {
        facts.insert("reason".into(), reason.clone().into());
    }
    let decision = if continuation.is_some() {
        "continue"
    } else {
        "pass"
    };
    let data = decision_entry(HookEvent::Stop, decision, facts, evaluated.hooks);
    let stop = control.clone();
    if !write_decision(core, persist_error, data, move || stop.stopped().is_some()).await? {
        return Ok(None);
    }
    let Some(reason) = continuation else {
        return Ok(None);
    };
    hooks.engine.state().stop_active = true;
    Ok(context_message(HookEvent::Stop, vec![reason]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_name_the_hook_and_its_reason() {
        assert_eq!(
            refusal(EscalationOutcome::Rejected, Some("prod")),
            "the user rejected this call, which a PreToolUse hook asked to confirm: prod"
        );
        assert_eq!(
            refusal(EscalationOutcome::Unavailable, None),
            "a PreToolUse hook asked to confirm this call, but no approval channel is available"
        );
        assert_eq!(
            ask_text(None),
            "A PreToolUse hook asks to confirm this call"
        );
    }

    #[test]
    fn a_blocked_prompt_is_refused_with_the_hooks_reason() {
        let (decision, facts, refused) = prompt_decision(&PromptAction::Block {
            reason: Some("no secrets".into()),
        });
        assert_eq!(decision, "block");
        assert_eq!(facts["reason"], "no secrets");
        assert_eq!(
            refused.as_deref(),
            Some("Prompt blocked by UserPromptSubmit hook: no secrets")
        );
        let (_, _, refused) = prompt_decision(&PromptAction::Block { reason: None });
        assert_eq!(refused.as_deref(), Some(PROMPT_BLOCKED));
    }
}
