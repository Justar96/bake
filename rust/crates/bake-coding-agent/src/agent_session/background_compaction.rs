//! Speculative summaries have no authority to change live context. The
//! session owner validates and commits their candidates at turn boundaries.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};

use bake_agent::RunControl;
use bake_ai::{AbortSignal, Model, ModelThinkingLevel};
use tokio::time::Instant;

use super::recovery::Core;
use super::{SessionEvent, WriteRefused, read_settings, write_session};
use crate::compaction::{
    CompactionPreparation, CompactionResult, CompactionSettings, SummarizationOptions,
    SummaryAttempt, compact, estimate_projected_context_tokens, estimate_tokens,
    prepare_compaction, should_compact,
};
use crate::owned_work::OwnedTask;
use crate::session::{SessionEntry, SessionManager};

/// The owner-observed lifecycle of one speculative summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundCompactionStatus {
    /// The session admitted its one background summary task.
    Started,
    /// Its result was cancelled, failed, or no longer matched the context.
    Discarded,
    /// Its validated summary was durably appended and installed.
    Committed,
}

impl BackgroundCompactionStatus {
    /// Its JSON event spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Discarded => "discarded",
            Self::Committed => "committed",
        }
    }
}

/// Holds the task, including a completed candidate awaiting an owner boundary.
/// The task captures its snapshot and request path, never this owning Core.
#[derive(Default)]
pub(super) struct BackgroundCompaction {
    pending: Mutex<Option<Pending>>,
}

struct Pending {
    snapshot: Arc<Snapshot>,
    task: OwnedTask<Result<CompactionResult, String>>,
    signal: AbortSignal,
    control: RunControl,
    started: Instant,
    completed: tokio::sync::watch::Receiver<bool>,
    attempts: Arc<Mutex<Vec<SummaryAttempt>>>,
}

struct SummaryFinished(tokio::sync::watch::Sender<bool>);

impl Drop for SummaryFinished {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

#[derive(Clone)]
struct Snapshot {
    branch: Vec<SessionEntry>,
    source_leaf: String,
    context_revision: Vec<String>,
    preparation: CompactionPreparation,
    model: Model,
    thinking: ModelThinkingLevel,
}

fn context_revision(branch: &[SessionEntry]) -> Vec<String> {
    branch
        .iter()
        .filter(|entry| {
            matches!(
                entry.entry_type(),
                "compaction" | "context_edit" | "model_change" | "thinking_level_change"
            )
        })
        .map(|entry| entry.id().to_owned())
        .collect()
}

impl Snapshot {
    fn capture(
        session: &SessionManager,
        model: Model,
        thinking: ModelThinkingLevel,
        settings: CompactionSettings,
    ) -> Option<Self> {
        let branch: Vec<_> = session.branch_entries(None).into_iter().cloned().collect();
        let source_leaf = session.leaf_id()?.to_owned();
        let preparation = prepare_compaction(&branch, settings)?;
        Some(Self {
            context_revision: context_revision(&branch),
            branch,
            source_leaf,
            preparation,
            model,
            thinking,
        })
    }

    fn calls(&self) -> u64 {
        if self.preparation.is_split_turn
            && !self.preparation.turn_prefix_messages.is_empty()
            && !self.preparation.messages_to_summarize.is_empty()
        {
            2
        } else {
            1
        }
    }

    /// Exact inputs are the fingerprint: no probabilistic hash admits a
    /// candidate. Appended conversation is allowed; projection edits are not.
    fn matches(
        &self,
        session: &SessionManager,
        model: &Model,
        thinking: ModelThinkingLevel,
        settings: CompactionSettings,
    ) -> bool {
        if self.model != *model
            || self.thinking != thinking
            || self.preparation.settings != settings
        {
            return false;
        }
        let branch: Vec<_> = session.branch_entries(None).into_iter().cloned().collect();
        if branch.len() < self.branch.len()
            || branch[..self.branch.len()] != self.branch
            || context_revision(&branch) != self.context_revision
        {
            return false;
        }
        let retained = &self.preparation.first_kept_entry_id;
        branch.iter().any(|entry| entry.id() == retained)
            && prepare_compaction(&branch[..self.branch.len()], settings)
                .is_some_and(|preparation| preparation == self.preparation)
    }
}

impl Core {
    /// Called only by the foreground owner between complete turns. Never
    /// awaits an unfinished summary unless foreground recovery needs it gone.
    pub(super) async fn background_compaction_boundary(
        self: &Arc<Self>,
        allow_start: bool,
    ) -> Result<bool, String> {
        let enabled = {
            let settings = read_settings(&self.settings);
            settings.background_compaction && settings.compaction_enabled()
        };
        if !enabled {
            self.cancel_background_compaction("disabled").await;
            return Ok(false);
        }
        let finished = {
            let mut slot = self
                .background_compaction
                .pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if slot
                .as_ref()
                .is_some_and(|pending| pending.task.is_finished())
            {
                slot.take()
            } else {
                None
            }
        };
        let committed = match finished {
            Some(pending) => self.commit_background_compaction(pending).await?,
            None => false,
        };
        if !allow_start || self.aborted() {
            return Ok(committed);
        }
        if self
            .background_compaction
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
        {
            return Ok(committed);
        }
        let Some(control) = self.control() else {
            return Ok(committed);
        };
        let Some(model) = self.model() else {
            return Ok(committed);
        };
        let settings = self.compaction_settings(Some(&model))?;
        if !settings.enabled || model.context_window <= settings.reserve_tokens.saturating_mul(2) {
            return Ok(committed);
        }
        let thinking = self
            .agent()
            .map(|agent| agent.thinking_level())
            .unwrap_or(ModelThinkingLevel::Off);
        let snapshot = self
            .with_session(move |session| {
                let branch: Vec<_> = session.branch_entries(None).into_iter().cloned().collect();
                let tokens =
                    estimate_projected_context_tokens(&session.build_session_projection(), &branch)
                        .tokens;
                let threshold = model.context_window - settings.reserve_tokens.saturating_mul(2);
                if tokens < threshold || should_compact(tokens, model.context_window, &settings) {
                    return None;
                }
                Snapshot::capture(session, model, thinking, settings)
            })
            .await?;
        let Some(snapshot) = snapshot else {
            return Ok(committed);
        };
        // Check the split summary's total before spending its first debit;
        // every actual attempt also checks the foreground reserve atomically.
        if !control.has_model_call_allowance(snapshot.calls().saturating_add(1)) {
            return Ok(committed);
        }
        let snapshot = Arc::new(snapshot);
        let work_snapshot = Arc::clone(&snapshot);
        let stream_fn = Arc::clone(&self.stream_fn);
        let run_control = control.clone();
        let retry = read_settings(&self.settings).retry.policy();
        let (completed_tx, completed) = tokio::sync::watch::channel(false);
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let observations = Arc::clone(&attempts);
        let mut signal = None;
        let task = self
            .work
            .spawn(|scope| {
                signal = Some(scope.signal().clone());
                async move {
                    let _finished = SummaryFinished(completed_tx);
                    let options = SummarizationOptions {
                        run_control: Some(run_control),
                        foreground_call_reserve: 1,
                        signal: Some(scope.signal().clone()),
                        thinking_level: Some(work_snapshot.thinking),
                        retry: Some(retry),
                        on_attempt: Some(Arc::new(move |attempt| {
                            observations
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .push(attempt.clone());
                        })),
                        ..SummarizationOptions::default()
                    };
                    compact(
                        &work_snapshot.preparation,
                        &work_snapshot.model,
                        None,
                        &stream_fn,
                        &options,
                    )
                    .await
                }
            })
            .map_err(|error| error.to_string())?;
        let Some(signal) = signal else {
            task.cancel();
            let _ = task.await;
            return Ok(committed);
        };
        *self
            .background_compaction
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Pending {
            snapshot: Arc::clone(&snapshot),
            task,
            signal,
            control,
            started: Instant::now(),
            completed,
            attempts,
        });
        self.emit(&SessionEvent::BackgroundCompaction {
            status: BackgroundCompactionStatus::Started,
            source_leaf_id: &snapshot.source_leaf,
            result: None,
            reason: None,
            elapsed_ms: 0,
            attempts: &[],
        });
        Ok(committed)
    }

    /// Branch switching and manual compaction take the slot, cancel it and
    /// await provider cleanup before reading the branch they will change.
    pub(super) async fn cancel_background_compaction(&self, reason: &str) {
        // Keep the slot occupied while cancellation drains the provider.
        // A dropped waiter must not permit a second summary to overlap it.
        let completed = self
            .background_compaction
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|pending| {
                pending.task.cancel();
                (pending.completed.clone(), Arc::clone(&pending.snapshot))
            });
        let Some((mut completed, snapshot)) = completed else {
            return;
        };
        let _ = completed.wait_for(|finished| *finished).await;
        let pending = {
            let mut slot = self
                .background_compaction
                .pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if slot
                .as_ref()
                .is_some_and(|pending| Arc::ptr_eq(&pending.snapshot, &snapshot))
            {
                slot.take()
            } else {
                None
            }
        };
        if let Some(pending) = pending {
            pending.task.cancel();
            let result = pending.task.await.ok().and_then(Result::ok);
            let attempts = pending
                .attempts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            self.emit(&SessionEvent::BackgroundCompaction {
                status: BackgroundCompactionStatus::Discarded,
                source_leaf_id: &pending.snapshot.source_leaf,
                result: result.as_ref(),
                reason: Some(reason),
                elapsed_ms: elapsed_ms(pending.started),
                attempts: &attempts,
            });
        }
    }

    async fn commit_background_compaction(
        self: &Arc<Self>,
        pending: Pending,
    ) -> Result<bool, String> {
        let result = pending
            .task
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result);
        let attempts = pending
            .attempts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let mut result = match result {
            Ok(result) => result,
            Err(reason) => {
                self.emit(&SessionEvent::BackgroundCompaction {
                    status: BackgroundCompactionStatus::Discarded,
                    source_leaf_id: &pending.snapshot.source_leaf,
                    result: None,
                    reason: Some(&reason),
                    elapsed_ms: elapsed_ms(pending.started),
                    attempts: &attempts,
                });
                return Ok(false);
            }
        };
        let snapshot = Arc::clone(&pending.snapshot);
        let signal = pending.signal;
        let control = pending.control;
        let started = pending.started;
        self.owned_context_commit(move |core| {
            #[cfg(test)]
            {
                let hook = core.hooks().before_compaction_commit.take();
                if let Some(hook) = hook {
                    hook();
                }
            }
            // Hold settings through the commit. Model and thinking changes
            // also append entries, which the snapshot revision rejects.
            let settings = read_settings(&core.settings);
            let current_model = core.model();
            let current_settings = settings.compaction_settings(
                current_model
                    .as_ref()
                    .map(|model| (model.provider.as_str(), model.id.as_str())),
            );
            let current_thinking = core
                .agent()
                .map(|agent| agent.thinking_level())
                .unwrap_or(ModelThinkingLevel::Off);
            let commit = write_session::<_, String>(&core.session, &core.gap, |session| {
                if signal.aborted()
                    || control.stopped().is_some()
                    || core.aborted()
                    || !settings.background_compaction
                    || !current_model
                        .as_ref()
                        .zip(current_settings.as_ref().ok())
                        .is_some_and(|(model, settings)| {
                            snapshot.matches(session, model, current_thinking, *settings)
                        })
                {
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
                let branch: Vec<_> = session.branch_entries(None).into_iter().cloned().collect();
                result.tokens_before =
                    estimate_projected_context_tokens(&session.build_session_projection(), &branch)
                        .tokens;
                session
                    .append_compaction(
                        &result.summary,
                        Some(&result.first_kept_entry_id),
                        result.tokens_before,
                        result.details.clone(),
                        Some(false),
                        result.usage.as_ref(),
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
            // Observers may change settings; no settings or session lock is
            // held while publishing either context or lifecycle events.
            drop(settings);
            let projection = match commit {
                Ok(Some(projection)) => projection,
                Ok(None) => {
                    core.emit(&SessionEvent::BackgroundCompaction {
                        status: BackgroundCompactionStatus::Discarded,
                        source_leaf_id: &snapshot.source_leaf,
                        result: Some(&result),
                        reason: Some("snapshot changed or cancelled"),
                        elapsed_ms: elapsed_ms(started),
                        attempts: &attempts,
                    });
                    return Ok(false);
                }
                Err(error) => {
                    let message = match error {
                        WriteRefused::AfterGap(error) | WriteRefused::Failed(error) => error,
                    };
                    core.abort_requested.store(true, Ordering::SeqCst);
                    control.fail(message.clone());
                    if let Some(active) = core.control() {
                        active.fail(message.clone());
                    }
                    if let Some(agent) = core.agent() {
                        agent.abort();
                    }
                    core.emit(&SessionEvent::BackgroundCompaction {
                        status: BackgroundCompactionStatus::Discarded,
                        source_leaf_id: &snapshot.source_leaf,
                        result: Some(&result),
                        reason: Some(&message),
                        elapsed_ms: elapsed_ms(started),
                        attempts: &attempts,
                    });
                    return Err(message);
                }
            };
            core.refresh_context(&projection);
            result.estimated_tokens_after =
                Some(projection.messages.iter().map(estimate_tokens).sum());
            core.emit(&SessionEvent::BackgroundCompaction {
                status: BackgroundCompactionStatus::Committed,
                source_leaf_id: &snapshot.source_leaf,
                result: Some(&result),
                reason: None,
                elapsed_ms: elapsed_ms(started),
                attempts: &attempts,
            });
            Ok(true)
        })
        .map_err(|error| error.to_string())?
        .await
        .map_err(|error| error.to_string())?
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
