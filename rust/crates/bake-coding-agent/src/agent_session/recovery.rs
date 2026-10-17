//! Compaction and auto-retry inside a session.
//!
//! Ported from Pi `packages/coding-agent/src/core/agent-session.ts`
//! (v1.1.0): `_checkCompaction`, `_runAutoCompaction`, `compact`,
//! `_compactBeforeNextAssistantResponse`, `_omitRecoveryAttempt`,
//! `_isRetryableError`, `_prepareRetry`, `_finishCancelledRetry`,
//! `_willRetryAfterAgentEnd`, and `_summarizationRetryCallbacks`.
//!
//! [`decide_compaction`] is `_checkCompaction`'s decision without its
//! effects, so the cases Pi's tests check by spying on
//! `_runAutoCompaction` are checked against the decision. [`Core`] holds
//! what the agent's listener and its next-turn hook share with the
//! session, so neither keeps the session alive.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::Duration;

use bake_agent::{
    Agent, AgentContext, AgentLoopTurnUpdate, AgentMessage as LiveMessage, AgentTool, StreamFn,
};
use bake_ai::utils::overflow::{is_context_overflow, is_recoverable_length};
use bake_ai::utils::retry::{RetryEvent, is_retryable_assistant_error, retry_delay_ms};
use bake_ai::{AbortController, AbortSignal, AssistantMessage, Model, StopReason};
use serde_json::Value;
use tokio::sync::watch;

use super::{Listeners, SessionEvent, SharedSession, lock};
use crate::compaction::{
    CompactionResult, CompactionSettings, RetryObserver, SummarizationOptions, compact,
    estimate_context_tokens, estimate_projected_context_tokens, estimate_tokens,
    prepare_compaction, should_compact,
};
use crate::owned_work::WorkGate;
use crate::session::time::js_date_ms;
use crate::session::{
    AgentMessage, GroupEntry, SessionEntry, SessionError, SessionManager, SessionProjection,
    get_latest_compaction_entry,
};

/// A recovery omission whose write failed, with the projection of the
/// known synced prefix when part of it was written.
struct Omission {
    error: SessionError,
    projection: Option<Box<SessionProjection>>,
}

impl std::fmt::Display for Omission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

/// What started a compaction, Pi's `reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionReason {
    /// [`super::AgentSession::compact`].
    Manual,
    /// The context crossed the threshold.
    Threshold,
    /// The model reported a context overflow or a recoverable length stop.
    Overflow,
}

impl CompactionReason {
    /// Pi's spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Threshold => "threshold",
            Self::Overflow => "overflow",
        }
    }
}

/// The refusal of an idle-only manual compaction while a response runs
/// ([`super::AgentSession::compact_when_idle`]).
pub const COMPACTION_WHILE_STREAMING: &str =
    "Wait for the current response to finish before compacting the session.";

/// The refusal of an idle-only manual compaction while another compaction
/// or a tree navigation runs.
pub const COMPACTION_WHILE_BUSY: &str =
    "Wait for the current compaction or tree navigation to finish before compacting the session.";

/// Pi's error for a manual compaction cancelled before its entry was
/// written.
pub const COMPACTION_CANCELLED: &str = "Compaction cancelled";

/// Why [`Core::run_compaction`] or a manual compaction wrote no entry.
#[derive(Debug)]
pub(super) enum CompactionError {
    /// Cancelled before the entry was written.
    Cancelled,
    /// Nothing was written: a preparation, summary, or session-read error.
    Failed(String),
    /// Writing the entry failed: it stays in the session's memory, the
    /// session has a gap, and nothing more is written.
    WriteFailed(String),
}

impl CompactionError {
    fn message(self) -> String {
        match self {
            Self::Cancelled => COMPACTION_CANCELLED.to_owned(),
            Self::Failed(message) | Self::WriteFailed(message) => message,
        }
    }
}

/// Pi's error when overflow recovery already ran once.
pub(super) const OVERFLOW_RECOVERY_FAILED: &str = "Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model.";
/// Pi's error when truncated-response recovery already ran once.
pub(super) const LENGTH_RECOVERY_FAILED: &str =
    "Truncated response recovery failed after one compact-and-retry attempt.";

/// What `_checkCompaction` decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Decision {
    /// Nothing to do.
    Skip,
    /// Run automatic compaction; with `will_retry`, omit the failed attempt
    /// first and continue the turn after.
    Run {
        reason: CompactionReason,
        will_retry: bool,
    },
    /// Overflow recovery already ran once: report the error.
    RecoveryExhausted(&'static str),
}

/// The inputs of `_checkCompaction`.
pub(super) struct CheckInput<'a> {
    pub settings: CompactionSettings,
    /// The selected model.
    pub model: Option<&'a Model>,
    pub assistant: &'a AssistantMessage,
    /// The assistant's entry, when it was persisted.
    pub assistant_entry_id: Option<&'a str>,
    pub branch: &'a [SessionEntry],
    pub projection: &'a SessionProjection,
    /// The agent's transcript.
    pub messages: &'a [AgentMessage],
    /// Pi's `skipAbortedCheck`: false before a prompt.
    pub skip_aborted_check: bool,
    pub overflow_recovery_attempted: bool,
}

fn compaction_time(branch: &[SessionEntry]) -> Option<Option<i64>> {
    get_latest_compaction_entry(branch).map(|entry| js_date_ms(entry.get("timestamp")))
}

/// Pi's `_modelForMessage` for a non-virtual selection.
fn model_for_message<'a>(
    model: Option<&'a Model>,
    message: &AssistantMessage,
) -> Option<&'a Model> {
    model.filter(|model| model.provider == message.provider && model.id == message.model)
}

/// Pi's `_checkCompaction` decision.
pub(super) fn decide_compaction(input: &CheckInput<'_>) -> Decision {
    let assistant = input.assistant;
    if !input.settings.enabled {
        return Decision::Skip;
    }
    if input.skip_aborted_check && assistant.stop_reason == StopReason::Aborted {
        return Decision::Skip;
    }
    // An overflow from another model does not apply to this one.
    let message_model = model_for_message(input.model, assistant);
    let same_model = message_model.is_some();
    let context_window = message_model
        .or(input.model)
        .map_or(0, |model| model.context_window);
    // A response older than the latest compaction must not retrigger it.
    let compacted_at = compaction_time(input.branch);
    if let Some(Some(time)) = compacted_at
        && assistant.timestamp <= time
    {
        return Decision::Skip;
    }

    let entry_id = input.assistant_entry_id;
    let projected = entry_id.is_none_or(|id| {
        input.projection.entries.iter().any(|entry| {
            entry.source_entry.id() == id
                && entry
                    .messages
                    .iter()
                    .any(|message| message.role() == Some("assistant"))
        })
    });
    let index = entry_id.and_then(|id| input.branch.iter().position(|entry| entry.id() == id));
    let after = index
        .and_then(|index| input.branch.get(index + 1..))
        .unwrap_or(&[]);
    let post_edit = after
        .iter()
        .any(|entry| entry.entry_type() == "context_edit");
    let latest_edit = after.iter().rfind(|entry| {
        entry.entry_type() == "context_edit"
            && entry.get("targetId").and_then(Value::as_str) == entry_id
    });
    let retained = entry_id.is_none()
        || (!after.iter().any(|entry| entry.entry_type() == "compaction")
            && !latest_edit
                .is_some_and(|edit| matches!(edit.get("replacement"), Some(Value::Null))));
    let usage_matches = projected && !post_edit;
    let explicit =
        assistant.stop_reason == StopReason::Error && is_context_overflow(assistant, None);
    let context_overflow = same_model
        && ((explicit && retained)
            || (usage_matches && is_context_overflow(assistant, Some(context_window))));
    let recoverable_length = message_model
        .is_some_and(|model| projected && is_recoverable_length(assistant, model.max_tokens));
    if context_overflow || recoverable_length {
        let will_retry = assistant.stop_reason != StopReason::Stop;
        if !will_retry {
            return Decision::Run {
                reason: CompactionReason::Overflow,
                will_retry: false,
            };
        }
        if input.overflow_recovery_attempted {
            return Decision::RecoveryExhausted(if context_overflow {
                OVERFLOW_RECOVERY_FAILED
            } else {
                LENGTH_RECOVERY_FAILED
            });
        }
        return Decision::Run {
            reason: CompactionReason::Overflow,
            will_retry: true,
        };
    }

    // Threshold: errors and all-zero usage fall back to the last valid
    // response's usage or an estimate.
    let has_edits = input
        .projection
        .entries
        .iter()
        .any(|entry| entry.source_entry.entry_type() == "context_edit");
    let direct = crate::compaction::calculate_context_tokens(&assistant.usage);
    let tokens = if has_edits {
        estimate_projected_context_tokens(input.projection, input.branch).tokens
    } else if assistant.stop_reason == StopReason::Error || direct == 0 {
        let estimate = estimate_context_tokens(input.messages);
        if let Some(index) = estimate.last_usage_index
            && let Some(Some(time)) = compacted_at
            && let Some(message) = input.messages.get(index)
            && message.role() == Some("assistant")
            && message
                .as_json()
                .get("timestamp")
                .and_then(Value::as_f64)
                .is_some_and(|stamp| stamp <= time as f64)
        {
            // Kept pre-compaction usage reflects the old, larger context.
            return Decision::Skip;
        }
        estimate.tokens
    } else {
        direct
    };
    if should_compact(tokens, context_window, &input.settings) {
        Decision::Run {
            reason: CompactionReason::Threshold,
            will_retry: false,
        }
    } else {
        Decision::Skip
    }
}

/// Whether the session is busy, Pi's `isIdle` inverted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Activity {
    /// A prompt and its post-run work, Pi's `_isAgentRunActive`.
    pub run: bool,
    /// [`super::AgentSession::compact`].
    pub manual_compaction: bool,
    /// Automatic compaction.
    pub auto_compaction: bool,
    /// [`super::AgentSession::navigate_tree`], which Pi counts in
    /// `isCompacting` through `_branchSummaryAbortController`.
    pub navigation: bool,
}

impl Activity {
    pub fn idle(self) -> bool {
        !self.run && !self.manual_compaction && !self.auto_compaction && !self.navigation
    }
}

/// Per-run bookkeeping of Pi's `AgentSession` fields.
#[derive(Default)]
pub(super) struct RunState {
    /// `_retryAttempt`.
    pub retry_attempt: u32,
    /// `_overflowRecoveryAttempted`.
    pub overflow_recovery_attempted: bool,
    /// `_lastAssistantMessage` and its entry.
    pub last_assistant: Option<(AssistantMessage, Option<String>)>,
    /// Entries of the tool results since the last assistant message.
    pub turn_tool_results: Vec<Option<String>>,
    /// `_lastAssistantToolResults`' entries, set at `turn_end`.
    pub last_tool_results: Vec<Option<String>>,
    /// `_retryAbortController`.
    pub retry_abort: Option<AbortController>,
    /// `_compactionAbortController`.
    pub compaction_abort: Option<AbortController>,
    /// `_autoCompactionAbortController`.
    pub auto_compaction_abort: Option<AbortController>,
}

/// Selects one of [`RunState`]'s abort controllers.
type AbortSlotOf = fn(&mut RunState) -> &mut Option<AbortController>;

/// Clears a registered abort controller however its work ends.
pub(super) struct AbortSlot<'a> {
    core: &'a Core,
    slot: AbortSlotOf,
}

impl Drop for AbortSlot<'_> {
    fn drop(&mut self) {
        *(self.slot)(&mut self.core.state()) = None;
    }
}

/// What the session, its agent listener, and its next-turn hook share.
pub(super) struct Core {
    pub session: SharedSession,
    pub settings: super::SharedSettings,
    pub listeners: Arc<Listeners>,
    /// The session's request path, also used for summaries.
    pub stream_fn: StreamFn,
    pub work: WorkGate,
    pub agent: OnceLock<Weak<Agent>>,
    pub has_model: AtomicBool,
    /// Pi's `_agentRunAbortRequested`.
    pub abort_requested: AtomicBool,
    pub state: Mutex<RunState>,
    pub activity: watch::Sender<Activity>,
    /// Why the session file lacks an entry, once a write to it failed.
    /// Set and read only while the session lock is held
    /// ([`super::write_session`]), so no write lands after a failed one,
    /// whichever task gets the lock next.
    pub gap: Arc<Mutex<Option<String>>>,
    /// Failures tests inject at the session's write boundary.
    #[cfg(test)]
    pub hooks: Mutex<super::TestHooks>,
    /// Counts [`Self::with_session`] calls, so tests can tell when the
    /// session's I/O has started.
    #[cfg(test)]
    pub session_io: watch::Sender<u64>,
}

/// Clears an activity flag however its work ends.
pub(super) struct ActivityGuard<'a> {
    activity: &'a watch::Sender<Activity>,
    clear: fn(&mut Activity),
}

impl Drop for ActivityGuard<'_> {
    fn drop(&mut self) {
        self.activity.send_modify(self.clear);
    }
}

impl Core {
    pub fn state(&self) -> MutexGuard<'_, RunState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    pub fn hooks(&self) -> MutexGuard<'_, super::TestHooks> {
        self.hooks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn agent(&self) -> Option<Arc<Agent>> {
        self.agent.get().and_then(Weak::upgrade)
    }

    pub fn emit(&self, event: &SessionEvent<'_>) {
        self.listeners.emit(event);
    }

    pub fn aborted(&self) -> bool {
        self.abort_requested.load(Ordering::SeqCst)
    }

    /// The selected model, unless none was available.
    pub fn model(&self) -> Option<Model> {
        if !self.has_model.load(Ordering::SeqCst) {
            return None;
        }
        self.agent().map(|agent| agent.model())
    }

    /// Registers a recovery's abort controller until the guard drops.
    ///
    /// Pi creates `_retryAbortController` and
    /// `_autoCompactionAbortController` before its first `await`; here
    /// session I/O is awaited before the wait or request the controller
    /// cancels. A controller registered after an abort therefore starts
    /// aborted: [`super::AgentSession::abort`] stores the flag before it
    /// reads the slots, and this sets the slot before it reads the flag, so
    /// one of the two sees the other.
    fn register_abort(&self, slot: AbortSlotOf) -> (AbortController, AbortSlot<'_>) {
        let controller = AbortController::new();
        *slot(&mut self.state()) = Some(controller.clone());
        if self.aborted() {
            controller.abort();
        }
        (controller, AbortSlot { core: self, slot })
    }

    /// Claims the session for a manual compaction only while it is idle,
    /// atomically with the prompt's and the navigation's claims, so none of
    /// them overlap; the guard releases it. `streaming` is whether the
    /// agent still streams.
    pub fn claim_manual_compaction(&self, streaming: bool) -> Result<ActivityGuard<'_>, String> {
        let mut refusal = None;
        self.activity.send_if_modified(|activity| {
            if activity.run || streaming {
                refusal = Some(COMPACTION_WHILE_STREAMING);
                false
            } else if !activity.idle() {
                refusal = Some(COMPACTION_WHILE_BUSY);
                false
            } else {
                activity.manual_compaction = true;
                true
            }
        });
        if let Some(refusal) = refusal {
            return Err(refusal.to_owned());
        }
        Ok(ActivityGuard {
            activity: &self.activity,
            clear: |activity| activity.manual_compaction = false,
        })
    }

    /// Marks work active until the guard drops.
    pub fn busy(&self, set: fn(&mut Activity), clear: fn(&mut Activity)) -> ActivityGuard<'_> {
        self.activity.send_modify(set);
        ActivityGuard {
            activity: &self.activity,
            clear,
        }
    }

    /// Waits until no prompt or compaction is running.
    pub async fn wait_for_idle(&self) {
        let mut idle = self.activity.subscribe();
        let _ = idle.wait_for(|activity| activity.idle()).await;
    }

    /// Runs `work` on the session off the Tokio workers, holding a permit
    /// of the session's gate: after shutdown nothing is written, and after
    /// a failed write ([`Self::gap`]) nothing more is.
    pub async fn with_session<R: Send + 'static>(
        &self,
        work: impl FnOnce(&mut SessionManager) -> R + Send + 'static,
    ) -> Result<R, String> {
        let Some(permit) = self.work.enter() else {
            return Err("The session is shut down".to_owned());
        };
        #[cfg(test)]
        self.session_io.send_modify(|count| *count += 1);
        let session = Arc::clone(&self.session);
        let gap = Arc::clone(&self.gap);
        let task = permit
            .spawn_blocking(move || {
                let mut session = lock(&session);
                if let Some(error) = super::lock_gap(&gap).clone() {
                    return Err(format!(
                        "The session file could not be written ({error}), so nothing more is written to it"
                    ));
                }
                Ok(work(&mut session))
            })
            .map_err(|refused| format!("Session I/O task failed: {refused}"))?;
        task.await
            .map_err(|error| format!("Session I/O task failed: {error}"))?
    }

    fn compaction_settings(&self, model: Option<&Model>) -> Result<CompactionSettings, String> {
        super::read_settings(&self.settings)
            .compaction_settings(model.map(|model| (model.provider.as_str(), model.id.as_str())))
    }

    /// Pi's `_refreshFinalizedContext`: the agent's transcript becomes the
    /// session projection.
    pub fn refresh_context(&self, projection: &SessionProjection) {
        if let Some(agent) = self.agent() {
            agent.set_messages(
                projection
                    .messages
                    .iter()
                    .map(AgentMessage::to_agent)
                    .collect(),
            );
        }
    }

    /// Pi's `_isRetryableError`: transient, and not an overflow, which
    /// compaction handles.
    pub fn is_retryable_error(&self, message: &AssistantMessage) -> bool {
        let model = self.model();
        let window = model_for_message(model.as_ref(), message)
            .or(model.as_ref())
            .map_or(0, |model| model.context_window);
        !is_context_overflow(message, Some(window)) && is_retryable_assistant_error(message)
    }

    /// Pi's `_willRetryAfterAgentEnd`.
    pub fn will_retry_after_agent_end(&self, messages: &[LiveMessage]) -> bool {
        if self.aborted() {
            return false;
        }
        let retry = super::read_settings(&self.settings).retry;
        if !retry.enabled || self.state().retry_attempt >= retry.max_retries {
            return false;
        }
        messages
            .iter()
            .rev()
            .find(|message| message.role() == "assistant")
            .and_then(LiveMessage::as_assistant)
            .is_some_and(|message| self.is_retryable_error(message))
    }

    /// Pi's `_finishCancelledRetry`.
    pub fn finish_cancelled_retry(&self) {
        let attempt = std::mem::take(&mut self.state().retry_attempt);
        if attempt == 0 {
            return;
        }
        self.emit(&SessionEvent::AutoRetryEnd {
            success: false,
            attempt,
            final_error: Some("Retry cancelled".to_owned()),
        });
    }

    /// The entry of a persisted assistant message: the tracked one, else
    /// the latest equal message on the branch.
    pub async fn assistant_entry_id(
        &self,
        message: &AssistantMessage,
    ) -> Result<Option<String>, String> {
        let target = message.clone();
        self.with_session(move |session| {
            session
                .branch_entries(None)
                .iter()
                .rev()
                .filter(|entry| {
                    entry.entry_type() == "message" && entry.message_role() == Some("assistant")
                })
                // Only a message with the same timestamp is deserialized.
                .filter(|entry| {
                    entry
                        .get("message")
                        .and_then(|value| value.get("timestamp"))
                        .and_then(Value::as_f64)
                        == Some(target.timestamp as f64)
                })
                .find(|entry| {
                    entry
                        .get("message")
                        .and_then(|value| {
                            serde_json::from_value::<bake_ai::Message>(value.clone()).ok()
                        })
                        .is_some_and(|stored| stored == bake_ai::Message::Assistant(target.clone()))
                })
                .map(|entry| entry.id().to_owned())
        })
        .await
    }

    /// Whether the agent's transcript holds `message`.
    fn in_transcript(&self, message: &AssistantMessage) -> bool {
        self.agent().is_some_and(|agent| {
            agent
                .messages()
                .iter()
                .any(|live| live.as_assistant() == Some(message))
        })
    }

    /// Pi's `_omitRecoveryAttempt`: context edits omit the failed attempt
    /// and its tool results from model context; the raw history keeps them.
    /// Each target is its entry and whether the transcript holds it; one in
    /// the transcript without an entry cannot be omitted.
    ///
    /// The edits are one group ([`SessionManager::append_group`]), the
    /// attempt's last, written through the session's write boundary ([`super::write_session`]), and
    /// the transcript is refreshed from the session projection once the
    /// group is written, as Pi refreshes it after its edits. When the write
    /// fails, the gap is recorded so nothing more is written, the edits
    /// known to be synced are refreshed into the transcript, and the
    /// failure is returned, which ends the run before another provider
    /// request: no retry wait, no compaction. An immediate reopen may read
    /// one edit more, an unsynced line or a torn one whose whole JSON was
    /// written ([`crate::session::GroupWriteError`]).
    pub(super) async fn omit_recovery_attempt(
        &self,
        targets: Vec<(Option<String>, bool)>,
    ) -> Result<(), String> {
        if targets
            .iter()
            .any(|(id, projected)| id.is_none() && *projected)
        {
            return Err(
                "Cannot persist recovery omission because a projected message has no source entry"
                    .to_owned(),
            );
        }
        // Pi writes the attempt's edit first. Here its tool results' edits
        // come first: a crash after any line then leaves no tool result in
        // context whose call is omitted, which a provider would refuse,
        // while a call whose result is omitted gets Pi's synthetic result
        // in the request. The written group projects as Pi's order does.
        let mut targets = targets;
        targets.rotate_left(1);
        let edits: Vec<GroupEntry> = targets
            .into_iter()
            .filter_map(|(id, _)| id)
            .map(|id| GroupEntry::context_edit(id, None))
            .collect();
        let Some(permit) = self.work.enter() else {
            return Err("The session is shut down".to_owned());
        };
        #[cfg(test)]
        self.session_io.send_modify(|count| *count += 1);
        let session = Arc::clone(&self.session);
        let gap = Arc::clone(&self.gap);
        let write = permit.spawn_blocking(move || {
            super::write_session(&session, &gap, |session| {
                match session.append_group(edits) {
                    Ok(_) => Ok(session.build_session_projection()),
                    Err(error) => {
                        let projection = matches!(
                            &error,
                            SessionError::GroupWrite(failure) if !failure.written.is_empty()
                        )
                        .then(|| Box::new(session.build_session_projection()));
                        Err(Omission { error, projection })
                    }
                }
            })
        });
        drop(permit);
        let written = write
            .map_err(|refused| format!("Session I/O task failed: {refused}"))?
            .await
            .map_err(|error| format!("Session I/O task failed: {error}"))?;
        match written {
            Ok(projection) => {
                self.refresh_context(&projection);
                Ok(())
            }
            Err(super::WriteRefused::AfterGap(error)) => Err(format!(
                "The session file could not be written ({error}), so nothing more is written to it"
            )),
            Err(super::WriteRefused::Failed(failed)) => {
                if let Some(projection) = &failed.projection {
                    self.refresh_context(projection);
                }
                Err(failed.error.to_string())
            }
        }
    }

    /// Pi's `_summarizationRetryCallbacks` for compaction.
    fn summarization_observer(self: &Arc<Self>, reason: CompactionReason) -> RetryObserver {
        let listeners = Arc::clone(&self.listeners);
        Arc::new(move |event| match event {
            RetryEvent::Scheduled {
                attempt,
                max_attempts,
                delay_ms,
                error_message,
            } => listeners.emit(&SessionEvent::SummarizationRetryScheduled {
                attempt,
                max_attempts,
                delay_ms,
                error_message,
            }),
            RetryEvent::AttemptStart => {
                listeners.emit(&SessionEvent::SummarizationRetryAttemptStart { reason });
            }
            RetryEvent::Finished { .. } => {
                listeners.emit(&SessionEvent::SummarizationRetryFinished);
            }
        })
    }

    /// Pi's `_runDefaultCompaction`, appending the result and refreshing
    /// the transcript.
    ///
    /// The cancellation (`controller` or `cancel`) is checked again under
    /// the session lock, immediately before the append, so an abort that
    /// arrives while the write waits for the blocking pool or the lock
    /// writes nothing ([`CompactionError::Cancelled`]). Once the append
    /// starts it completes, and the result reports it. The append goes
    /// through the session's write boundary ([`super::write_session`]): a
    /// failed write leaves the entry in memory, as every failed append
    /// does, records the session's gap so nothing more is written and later
    /// prompts are refused, stops a run, and is reported as
    /// [`CompactionError::WriteFailed`], never as a cancellation.
    async fn run_compaction(
        self: &Arc<Self>,
        preparation: &crate::compaction::CompactionPreparation,
        model: &Model,
        custom_instructions: Option<&str>,
        controller: &AbortController,
        cancel: Option<&AbortSignal>,
        reason: CompactionReason,
    ) -> Result<CompactionResult, CompactionError> {
        let options = SummarizationOptions {
            signal: Some(controller.signal()),
            thinking_level: self.agent().map(|agent| agent.thinking_level()),
            retry: Some(super::read_settings(&self.settings).retry.policy()),
            on_retry: Some(self.summarization_observer(reason)),
            ..SummarizationOptions::default()
        };
        let mut result = compact(
            preparation,
            model,
            custom_instructions,
            &self.stream_fn,
            &options,
        )
        .await
        .map_err(CompactionError::Failed)?;
        let signals = [Some(controller.signal()), cancel.cloned()];
        let cancelled = move || signals.iter().flatten().any(AbortSignal::aborted);
        if cancelled() {
            return Err(CompactionError::Cancelled);
        }
        let Some(permit) = self.work.enter() else {
            return Err(CompactionError::Failed(
                "The session is shut down".to_owned(),
            ));
        };
        #[cfg(test)]
        self.session_io.send_modify(|count| *count += 1);
        let entry = result.clone();
        let core = Arc::clone(self);
        let commit = permit.spawn_blocking(move || {
            #[cfg(test)]
            {
                let hook = core.hooks().before_compaction_commit.take();
                if let Some(hook) = hook {
                    hook();
                }
            }
            let written = super::write_session::<_, String>(&core.session, &core.gap, |session| {
                // The commit boundary: a cancel seen here writes nothing.
                if cancelled() {
                    return Ok(None);
                }
                #[cfg(test)]
                {
                    let hook = core.hooks().fail_compaction_entry.take();
                    if let Some(hook) = hook {
                        hook();
                        return Err(super::injected_failure().to_string());
                    }
                }
                session
                    .append_compaction(
                        &entry.summary,
                        Some(&entry.first_kept_entry_id),
                        entry.tokens_before,
                        entry.details.clone(),
                        Some(false),
                        entry.usage.as_ref(),
                    )
                    .map_err(|error| error.to_string())?;
                let projection = session.build_session_projection();
                #[cfg(test)]
                {
                    let hook = core.hooks().after_compaction_commit.take();
                    if let Some(hook) = hook {
                        hook();
                    }
                }
                Ok(Some(projection))
            });
            // As every failed write does: a run stops here, as soon as the
            // session lock is released.
            if matches!(written, Err(super::WriteRefused::Failed(_))) {
                core.abort_requested.store(true, Ordering::SeqCst);
                if let Some(agent) = core.agent() {
                    agent.abort();
                }
            }
            written
        });
        let committed = commit
            .map_err(|refused| {
                CompactionError::Failed(format!("Session I/O task failed: {refused}"))
            })?
            .await
            .map_err(|error| {
                CompactionError::Failed(format!("Session I/O task failed: {error}"))
            })?;
        let projection = match committed {
            Ok(Some(projection)) => projection,
            Ok(None) => return Err(CompactionError::Cancelled),
            Err(super::WriteRefused::AfterGap(error)) => {
                return Err(CompactionError::Failed(format!(
                    "The session file could not be written ({error}), so nothing more is written to it"
                )));
            }
            Err(super::WriteRefused::Failed(error)) => {
                return Err(CompactionError::WriteFailed(format!(
                    "The compaction could not be written to the session file ({error}), so the session stopped and takes no more prompts."
                )));
            }
        };
        self.refresh_context(&projection);
        result.estimated_tokens_after = Some(projection.messages.iter().map(estimate_tokens).sum());
        Ok(result)
    }

    /// Pi's `_runAutoCompaction`: whether the post-run loop continues.
    /// `run_signal`, the signal of the run between whose turns it
    /// compacts, also cancels it.
    pub async fn run_auto_compaction(
        self: &Arc<Self>,
        reason: CompactionReason,
        will_retry: bool,
        run_signal: Option<&AbortSignal>,
    ) -> Result<bool, String> {
        // Registered before the branch read, so an abort during it cancels
        // the request.
        let (controller, slot) = self.register_abort(|state| &mut state.auto_compaction_abort);
        // A run already aborted sends no summarization request: its signal
        // is only watched once the request has started.
        if run_signal.is_some_and(AbortSignal::aborted) {
            controller.abort();
        }
        let model = self.model();
        let settings = self.compaction_settings(model.as_ref())?;
        let Some(model) = model else {
            return Ok(false);
        };
        let branch = self
            .with_session(|session| {
                session
                    .branch_entries(None)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .await?;
        let Some(preparation) = prepare_compaction(&branch, settings) else {
            return Ok(false);
        };
        let _busy = self.busy(
            |activity| activity.auto_compaction = true,
            |activity| activity.auto_compaction = false,
        );
        self.emit(&SessionEvent::CompactionStart { reason });
        let result = if controller.signal().aborted() {
            Err(CompactionError::Failed(
                "This operation was aborted".to_owned(),
            ))
        } else {
            let work = self.run_compaction(&preparation, &model, None, &controller, None, reason);
            match run_signal {
                None => work.await,
                Some(run_signal) => {
                    tokio::pin!(work);
                    tokio::select! {
                        biased;
                        result = &mut work => result,
                        () = run_signal.cancelled() => {
                            controller.abort();
                            work.await
                        }
                    }
                }
            }
        };
        // Automatic compactions never overlap: the post-run loop, the
        // pre-prompt check, and the next-turn hook run one at a time.
        drop(slot);
        match result {
            Ok(result) => {
                self.emit(&SessionEvent::CompactionEnd {
                    reason,
                    result: Some(&result),
                    aborted: false,
                    will_retry,
                    error_message: None,
                });
                if will_retry {
                    return Ok(true);
                }
                // Queued messages are delivered by one more run.
                Ok(self
                    .agent()
                    .is_some_and(|agent| agent.has_queued_messages()))
            }
            Err(error) => {
                // A failed write is reported as one, whatever aborted.
                let aborted = !matches!(error, CompactionError::WriteFailed(_))
                    && (matches!(error, CompactionError::Cancelled)
                        || controller.signal().aborted());
                let message = error.message();
                let error_message = (!aborted).then(|| match reason {
                    CompactionReason::Overflow => {
                        format!("Context overflow recovery failed: {message}")
                    }
                    _ => format!("Auto-compaction failed: {message}"),
                });
                self.emit(&SessionEvent::CompactionEnd {
                    reason,
                    result: None,
                    aborted,
                    will_retry: false,
                    error_message,
                });
                Ok(false)
            }
        }
    }

    /// Pi's `compact`: manual compaction, never retried or continued. The
    /// caller claimed the session for it (`busy`); for Pi's `compact`, it
    /// aborted the current run first.
    ///
    /// Its abort controller is registered before anything is awaited, so
    /// [`super::AgentSession::abort_compaction`], `abort`, and `shutdown`
    /// cancel it from here on; shutdown closes the listeners before it reads
    /// the slot, so a compaction registered after that starts aborted.
    /// `cancel`, the caller's own signal, cancels it as well: before the
    /// branch is read, before the summary request, during it, and at the
    /// commit boundary under the session lock. A cancelled compaction
    /// writes nothing and fails with [`COMPACTION_CANCELLED`]; one whose
    /// entry was written succeeds, however late the cancel came. A failed
    /// write of the entry is reported as that failure, never as a cancel,
    /// and stops the session ([`Self::run_compaction`]).
    pub async fn manual_compaction(
        self: &Arc<Self>,
        custom_instructions: Option<&str>,
        no_model_message: String,
        cancel: Option<AbortSignal>,
        busy: ActivityGuard<'_>,
    ) -> Result<CompactionResult, String> {
        let controller = AbortController::new();
        self.state().compaction_abort = Some(controller.clone());
        if self.listeners.closed.load(Ordering::SeqCst) {
            controller.abort();
        }
        self.emit(&SessionEvent::CompactionStart {
            reason: CompactionReason::Manual,
        });
        let cancelled =
            || controller.signal().aborted() || cancel.as_ref().is_some_and(AbortSignal::aborted);
        let work = async {
            let Some(model) = self.model() else {
                return Err(CompactionError::Failed(no_model_message));
            };
            let settings = self
                .compaction_settings(Some(&model))
                .map_err(CompactionError::Failed)?;
            if cancelled() {
                return Err(CompactionError::Cancelled);
            }
            let branch = self
                .with_session(|session| {
                    session
                        .branch_entries(None)
                        .into_iter()
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .await
                .map_err(CompactionError::Failed)?;
            let Some(preparation) = prepare_compaction(&branch, settings) else {
                let message = if branch
                    .last()
                    .is_some_and(|entry| entry.entry_type() == "compaction")
                {
                    "Already compacted"
                } else {
                    "Nothing to compact (session too small)"
                };
                return Err(CompactionError::Failed(message.to_owned()));
            };
            // No summary request is sent once cancelled.
            if cancelled() {
                return Err(CompactionError::Cancelled);
            }
            self.run_compaction(
                &preparation,
                &model,
                custom_instructions,
                &controller,
                cancel.as_ref(),
                CompactionReason::Manual,
            )
            .await
        };
        let result = match &cancel {
            None => work.await,
            Some(cancel) => {
                tokio::pin!(work);
                tokio::select! {
                    biased;
                    result = &mut work => result,
                    () = cancel.cancelled() => {
                        controller.abort();
                        work.await
                    }
                }
            }
        };
        // `compaction_end` listeners may prompt, so the session is idle
        // before they run.
        self.state().compaction_abort = None;
        drop(busy);
        match result {
            Ok(result) => {
                self.emit(&SessionEvent::CompactionEnd {
                    reason: CompactionReason::Manual,
                    result: Some(&result),
                    aborted: false,
                    will_retry: false,
                    error_message: None,
                });
                Ok(result)
            }
            Err(error) => {
                // A failed write is reported as one even when a cancel came
                // after it; a provider error after a cancel is the cancel.
                let aborted = match &error {
                    CompactionError::Cancelled => true,
                    CompactionError::WriteFailed(_) => false,
                    CompactionError::Failed(_) => cancelled(),
                };
                let message = error.message();
                self.emit(&SessionEvent::CompactionEnd {
                    reason: CompactionReason::Manual,
                    result: None,
                    aborted,
                    will_retry: false,
                    error_message: (!aborted).then(|| format!("Compaction failed: {message}")),
                });
                Err(if aborted {
                    COMPACTION_CANCELLED.to_owned()
                } else {
                    message
                })
            }
        }
    }

    /// Pi's `_checkCompaction`: whether the post-run loop continues.
    pub async fn check_compaction(
        self: &Arc<Self>,
        assistant: &AssistantMessage,
        entry_id: Option<String>,
        skip_aborted_check: bool,
        tool_results: Vec<Option<String>>,
    ) -> Result<bool, String> {
        let model = self.model();
        let settings = self.compaction_settings(model.as_ref())?;
        if !settings.enabled {
            return Ok(false);
        }
        let entry_id = match entry_id {
            Some(id) => Some(id),
            None => self.assistant_entry_id(assistant).await?,
        };
        let (branch, projection) = self
            .with_session(|session| {
                (
                    session
                        .branch_entries(None)
                        .into_iter()
                        .cloned()
                        .collect::<Vec<_>>(),
                    session.build_session_projection(),
                )
            })
            .await?;
        let messages: Vec<AgentMessage> = self
            .agent()
            .map(|agent| agent.messages().iter().map(AgentMessage::from).collect())
            .unwrap_or_default();
        let overflow_recovery_attempted = self.state().overflow_recovery_attempted;
        let decision = decide_compaction(&CheckInput {
            settings,
            model: model.as_ref(),
            assistant,
            assistant_entry_id: entry_id.as_deref(),
            branch: &branch,
            projection: &projection,
            messages: &messages,
            skip_aborted_check,
            overflow_recovery_attempted,
        });
        match decision {
            Decision::Skip => Ok(false),
            Decision::RecoveryExhausted(message) => {
                self.emit(&SessionEvent::CompactionEnd {
                    reason: CompactionReason::Overflow,
                    result: None,
                    aborted: false,
                    will_retry: false,
                    error_message: Some(message.to_owned()),
                });
                Ok(false)
            }
            Decision::Run {
                reason,
                will_retry: true,
            } => {
                self.state().overflow_recovery_attempted = true;
                let mut targets = vec![(entry_id, self.in_transcript(assistant))];
                targets.extend(tool_results.into_iter().map(|id| (id, true)));
                self.omit_recovery_attempt(targets).await?;
                self.run_auto_compaction(reason, true, None).await
            }
            Decision::Run { reason, .. } => self.run_auto_compaction(reason, false, None).await,
        }
    }

    /// Pi's `_compactBeforeNextAssistantResponse`: threshold compaction
    /// between the turns of a run, replacing the loop's context.
    ///
    /// Unlike Pi, the loop keeps its own context below the threshold
    /// rather than taking the session projection: the two differ only
    /// after a failed session write, when the projection lacks the
    /// messages that were not written. A settings error skips the
    /// compaction where Pi's throw would end the run, because the hook
    /// cannot fail the run.
    pub async fn compact_before_next_response(
        self: &Arc<Self>,
        tools: Vec<Arc<AgentTool>>,
        signal: Option<AbortSignal>,
    ) -> Option<AgentLoopTurnUpdate> {
        if self.aborted() || signal.as_ref().is_some_and(AbortSignal::aborted) {
            return None;
        }
        let model = self.model()?;
        if model.context_window == 0 {
            return None;
        }
        let settings = self.compaction_settings(Some(&model)).ok()?;
        let (branch, projection) = self
            .with_session(|session| {
                (
                    session
                        .branch_entries(None)
                        .into_iter()
                        .cloned()
                        .collect::<Vec<_>>(),
                    session.build_session_projection(),
                )
            })
            .await
            .ok()?;
        let tokens = estimate_projected_context_tokens(&projection, &branch).tokens;
        if !should_compact(tokens, model.context_window, &settings) {
            return None;
        }
        // Failures are reported as events; the run goes on either way.
        let _ = self
            .run_auto_compaction(CompactionReason::Threshold, false, signal.as_ref())
            .await;
        let projection = self
            .with_session(|session| session.build_session_projection())
            .await
            .ok()?;
        Some(AgentLoopTurnUpdate {
            context: Some(AgentContext {
                messages: projection
                    .messages
                    .iter()
                    .map(AgentMessage::to_agent)
                    .collect(),
                tools,
            }),
            ..AgentLoopTurnUpdate::default()
        })
    }

    /// Pi's `_prepareRetry`: count the attempt, omit the failed response,
    /// and wait out the backoff. Whether the run continues.
    pub async fn prepare_retry(
        &self,
        message: &AssistantMessage,
        entry_id: Option<String>,
    ) -> Result<bool, String> {
        let retry = super::read_settings(&self.settings).retry;
        if !retry.enabled {
            return Ok(false);
        }
        let attempt = {
            let mut state = self.state();
            state.retry_attempt += 1;
            if state.retry_attempt > retry.max_retries {
                // Kept so post-run handling reports the final failure.
                state.retry_attempt -= 1;
                return Ok(false);
            }
            state.retry_attempt
        };
        let delay_ms = retry_delay_ms(retry.base_delay_ms, Some(retry.max_agent_delay_ms), attempt);
        self.emit(&SessionEvent::AutoRetryStart {
            attempt,
            max_attempts: retry.max_retries,
            delay_ms,
            error_message: message
                .error_message
                .clone()
                .filter(|error| !error.is_empty())
                .unwrap_or_else(|| "Unknown error".to_owned()),
        });
        // Registered before the omission's session I/O, so an abort
        // during it cancels the wait.
        let (controller, slot) = self.register_abort(|state| &mut state.retry_abort);
        let entry_id = match entry_id {
            Some(id) => Some(id),
            None => self.assistant_entry_id(message).await?,
        };
        let projected = self.in_transcript(message);
        self.omit_recovery_attempt(vec![(entry_id, projected)])
            .await?;
        let signal = controller.signal();
        let aborted = signal.aborted()
            || tokio::select! {
                () = tokio::time::sleep(Duration::from_millis(delay_ms)) => false,
                () = signal.cancelled() => true,
            };
        drop(slot);
        if aborted {
            self.finish_cancelled_retry();
            return Ok(false);
        }
        Ok(true)
    }

    /// Pi's `abortRetry` and `abortCompaction`.
    pub fn abort_retry_and_compaction(&self) {
        let state = self.state();
        for controller in [
            &state.retry_abort,
            &state.compaction_abort,
            &state.auto_compaction_abort,
        ]
        .into_iter()
        .flatten()
        {
            controller.abort();
        }
    }
}
