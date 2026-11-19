//! Writing the live conversation to the session file: one message or
//! group at a time, with the gap that stops writing after a failed write.

use super::*;

/// Persists a finished message as Pi's `_handleAgentEvent` does: custom
/// messages as `custom_message` entries, LLM messages as `message`
/// entries, and the other kinds not at all (they are written elsewhere).
/// Returns the entry id.
fn persist(session: &mut SessionManager, message: &LiveMessage) -> Result<Option<String>, String> {
    let result = match message {
        LiveMessage::Llm(llm) => session.append_message(llm.clone()).map(Some),
        LiveMessage::Custom(custom) => match custom.as_any().downcast_ref::<CustomMessage>() {
            Some(custom) => session
                .append_custom_message_entry(
                    &custom.custom_type,
                    custom.content.clone(),
                    custom.display,
                    custom.details.clone(),
                )
                .map(Some),
            None => Ok(None),
        },
    };
    result.map_err(|error| error.to_string())
}

/// Pi's `prepareNextTurnWithContext` hook (`_installAgentNextTurnRefresh`):
/// threshold compaction between turns, then the model and thinking level
/// selected now, so a change made while a run streams takes effect at its
/// next model request. The request already streaming keeps the model and
/// level it started with.
pub(super) fn next_turn_hook(
    core: Arc<Core>,
    selection: SessionSelection,
    jobs: JobRegistry,
    hooks: hooks::SharedHooks,
) -> bake_agent::agent::AgentPrepareNextTurnWithContext {
    Arc::new(move |turn, signal| {
        let tools = turn.context.tools.clone();
        let core = Arc::clone(&core);
        let selection = selection.clone();
        let jobs = jobs.clone();
        let hooks = hooks.get().cloned();
        Box::pin(async move {
            let mut update = core
                .compact_before_next_response(tools, signal)
                .await
                .unwrap_or_default();
            // Jobs that settled during the step, and notices other agents
            // posted, reach its next request: `tool-jobs`' injection into
            // a busy owner.
            update
                .messages
                .extend(job_notices::claim(&jobs).into_iter().map(job_notices::live));
            // The team update (telecom's) rides on the request made anyway.
            update
                .messages
                .extend(jobs.take_update().map(job_notices::live));
            // Context `PostToolUse` hooks added during the step.
            if let Some(hooks) = &hooks {
                update.messages.extend(hooks.take_pending());
            }
            // Context files the step's calls found, before the next request.
            let systems: Vec<Message> = update
                .context
                .as_ref()
                .unwrap_or(turn.context)
                .messages
                .iter()
                .chain(update.messages.iter())
                .filter_map(|message| message.as_system().cloned().map(Message::System))
                .collect();
            if let Some(patch) =
                nested_context::section_patch(&systems, core.nested.section().as_deref())
            {
                update.messages.push(patch.into());
            }
            // One read, so the model and level come from one change.
            let selected = selection.get();
            if let Some(model) = selected.model {
                update.model = Some(model);
                update.thinking_level = Some(selected.thinking_level);
            }
            Some(update)
        })
    })
}

/// Persists a finished message unless an earlier write failed or the
/// session is shut down (Pi's `dispose` disconnects without persisting the
/// aborted message). `Err` when this write failed.
pub(super) async fn persist_message(
    core: &Core,
    persist_error: &Mutex<Option<String>>,
    message: &LiveMessage,
) -> Result<Option<String>, ()> {
    let message = message.clone();
    let session = Arc::clone(&core.session);
    let gap = Arc::clone(&core.gap);
    // The gate owns the write, which keeps running if this run is
    // cancelled.
    let Ok(write) = core
        .work
        .spawn_blocking(move || persist_after_gap_check(&session, &gap, &message))
    else {
        return Ok(None);
    };
    let result = write
        .await
        .unwrap_or_else(|error| Err(format!("Session I/O task failed: {error}")));
    match result {
        Ok(Persisted::Written(entry_id)) => Ok(entry_id),
        // After a failed write, a later message would cross the gap.
        Ok(Persisted::AfterGap) => Err(()),
        Err(error) => {
            persist_error
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get_or_insert(error);
            Err(())
        }
    }
}

/// What [`persist_after_gap_check`] did.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Persisted {
    Written(Option<String>),
    /// Refused: an earlier write failed.
    AfterGap,
}

/// Writes `message` through [`write_session`]. Blocks.
pub(super) fn persist_after_gap_check(
    session: &SharedSession,
    gap: &Mutex<Option<String>>,
    message: &LiveMessage,
) -> Result<Persisted, String> {
    match write_session(session, gap, |session| persist(session, message)) {
        Ok(entry_id) => Ok(Persisted::Written(entry_id)),
        Err(WriteRefused::AfterGap(_)) => Ok(Persisted::AfterGap),
        Err(WriteRefused::Failed(error)) => Err(error),
    }
}

/// Why [`write_session`] wrote nothing more.
#[derive(Debug)]
pub(super) enum WriteRefused<E = String> {
    /// An earlier write failed; its reason.
    AfterGap(String),
    /// This write failed; the session now has a gap.
    Failed(E),
}

impl<E: std::fmt::Display> WriteRefused<E> {
    /// The reason, as the session reports it.
    pub(super) fn message(&self) -> String {
        match self {
            Self::AfterGap(error) => error.clone(),
            Self::Failed(error) => error.to_string(),
        }
    }
}

/// The session's write boundary: under the session lock, refuses once a
/// write has failed, and records this write's failure before the lock is
/// released, so a write queued behind a failing one sees it. A write that
/// fails part-way through a group of entries
/// ([`SessionManager::append_group`]) is a failure too: nothing is written
/// after its prefix, and the group is never written again. Blocks.
pub(super) fn write_session<R, E: std::fmt::Display>(
    session: &SharedSession,
    gap: &Mutex<Option<String>>,
    write: impl FnOnce(&mut SessionManager) -> Result<R, E>,
) -> Result<R, WriteRefused<E>> {
    let mut session = lock(session);
    if let Some(error) = lock_gap(gap).clone() {
        return Err(WriteRefused::AfterGap(error));
    }
    write(&mut session).map_err(|error| {
        lock_gap(gap).get_or_insert_with(|| error.to_string());
        WriteRefused::Failed(error)
    })
}

/// The gap's lock, taken after the session lock and never before it.
pub(super) fn lock_gap(gap: &Mutex<Option<String>>) -> std::sync::MutexGuard<'_, Option<String>> {
    gap.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Pi's bookkeeping after persisting: the last assistant message, overflow
/// recovery, and the end of a successful retry.
pub(super) fn track_message(core: &Core, message: &LiveMessage, entry_id: Option<String>) {
    let Some(assistant) = message.as_assistant() else {
        return;
    };
    let finished_retry = {
        let mut state = core.state();
        state.last_assistant = Some((assistant.clone(), entry_id));
        if !matches!(
            assistant.stop_reason,
            bake_ai::StopReason::Error | bake_ai::StopReason::Length
        ) {
            state.overflow_recovery_attempted = false;
        }
        // A successful response ends the retry, so attempts do not
        // accumulate across the turns of a run.
        if assistant.stop_reason != bake_ai::StopReason::Error && state.retry_attempt > 0 {
            Some(std::mem::take(&mut state.retry_attempt))
        } else {
            None
        }
    };
    if let Some(attempt) = finished_retry {
        core.emit(&SessionEvent::AutoRetryEnd {
            success: true,
            attempt,
            final_error: None,
        });
    }
}
