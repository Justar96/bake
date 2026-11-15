//! The session's events and their listeners: what a frontend subscribes to
//! ([`AgentSession::subscribe`]) and the order listeners hear it in.

use super::*;

/// An event of the session, Pi's `AgentSessionEvent` without the deferred
/// features' events.
#[derive(Debug)]
pub enum SessionEvent<'a> {
    /// The run's durable start or continuation checkpoint is committed.
    RecoverableRunStarted { run_id: String },
    /// An agent event other than `agent_end`.
    Agent(&'a AgentEvent),
    /// Pi's `agent_end`, with whether auto-retry will continue the run.
    AgentEnd {
        /// The run's new messages.
        messages: &'a [LiveMessage],
        /// Whether a retry follows.
        will_retry: bool,
    },
    /// A prompt finished, Pi's `agent_settled`.
    AgentSettled {
        /// The final execution outcome, after required writes and cleanup.
        outcome: &'a RunOutcome,
        /// Whether it was aborted.
        aborted: bool,
    },
    /// The thinking level changed, Pi's `thinking_level_changed`.
    ThinkingLevelChanged(ModelThinkingLevel),
    /// The session's name changed, Pi's `session_info_changed`, with the
    /// name it now has.
    SessionInfoChanged(Option<String>),
    /// Pi's `compaction_start`.
    CompactionStart {
        /// What started it.
        reason: CompactionReason,
    },
    /// Bake's own: a compaction's summary request is about to be sent,
    /// from the provider's cache ([`crate::compaction::cached`]) or as Pi's
    /// summary; `fallback` says why the cached summary, while on, was not
    /// used. A cached request whose payload check refuses it as it is sent,
    /// or that fails, is followed by Pi's, with the reason.
    CompactionSummary {
        /// What started the compaction.
        reason: CompactionReason,
        /// Whether the request repeats the cached conversation.
        cached: bool,
        /// The context estimate before the compaction.
        tokens_before: u64,
        /// Why the cached summary was not used.
        fallback: Option<&'a str>,
    },
    /// Pi's `compaction_end`.
    CompactionEnd {
        /// What started it.
        reason: CompactionReason,
        /// The recorded compaction, when it succeeded.
        result: Option<&'a CompactionResult>,
        /// Whether it was aborted.
        aborted: bool,
        /// Whether the interrupted turn continues.
        will_retry: bool,
        /// Why it failed, unless aborted.
        error_message: Option<String>,
    },
    /// The owner-observed lifecycle of an opt-in background summary.
    BackgroundCompaction {
        /// Whether it started, was discarded, or was durably installed.
        status: BackgroundCompactionStatus,
        /// The leaf whose context the summary captured.
        source_leaf_id: &'a str,
        /// A completed candidate, whether committed or discarded.
        result: Option<&'a CompactionResult>,
        /// Why a candidate was discarded.
        reason: Option<&'a str>,
        /// Time since the background task started.
        elapsed_ms: u64,
        /// Every admitted attempt, including retries and partial failures.
        attempts: &'a [crate::compaction::SummaryAttempt],
    },
    /// An opt-in masking batch, after its edits and live context are committed.
    ContextMasking {
        /// The leaf whose visible observations were reduced.
        source_leaf_id: &'a str,
        /// Tool result contents replaced by persisted placeholders.
        masked_results: u64,
        /// Removed UTF-8 bytes of serialized content, excluding entry metadata.
        removed_bytes: u64,
        /// The protected tail of complete assistant tool batches.
        retained_recent_batches: u64,
    },
    /// Pi's `auto_retry_start`.
    AutoRetryStart {
        /// The retry number, from 1.
        attempt: u32,
        /// The retry budget.
        max_attempts: u32,
        /// The backoff before the retry.
        delay_ms: u64,
        /// The error retried.
        error_message: String,
    },
    /// Pi's `auto_retry_end`.
    AutoRetryEnd {
        /// Whether a retry succeeded.
        success: bool,
        /// The last retry number.
        attempt: u32,
        /// The final error, when retries ended without success.
        final_error: Option<String>,
    },
    /// Pi's `summarization_retry_scheduled`.
    SummarizationRetryScheduled {
        /// The retry number, from 1.
        attempt: u32,
        /// The retry budget.
        max_attempts: u32,
        /// The backoff.
        delay_ms: u64,
        /// The error retried.
        error_message: String,
    },
    /// Pi's `summarization_retry_attempt_start`.
    SummarizationRetryAttemptStart {
        /// The compaction, with its reason, or the branch summary retried.
        source: SummarizationSource,
    },
    /// Pi's `summarization_retry_finished`.
    SummarizationRetryFinished,
    /// Bake's own: context files the session's file tool calls found while
    /// working joined the prompt from the next request, or were left out
    /// (`nestedContextFiles`). The warnings are for the user only.
    NestedContext {
        /// The paths of the files added, outermost first.
        loaded: &'a [String],
        /// Why a file was left out or could not be read.
        warnings: &'a [String],
    },
    /// Pi's `queue_update`: the steering and follow-up prompts still
    /// waiting, in the order they will be delivered.
    QueueUpdate {
        /// Prompts for the running turn's next step ([`AgentSession::steer`]).
        steering: &'a [String],
        /// Prompts for when the agent would stop ([`AgentSession::follow_up`]).
        follow_up: &'a [String],
    },
}

type Listener = Arc<dyn Fn(&SessionEvent<'_>) + Send + Sync>;

#[derive(Default)]
pub(super) struct Listeners {
    pub(super) next: AtomicU64,
    pub(super) list: Mutex<Vec<(u64, Listener)>>,
    /// Set by [`AgentSession::shutdown`]: later events reach no listener.
    pub(super) closed: AtomicBool,
    /// Emits in progress, so shutdown can wait until no listener runs.
    emitting: std::sync::atomic::AtomicUsize,
    /// Notified when the last emit in progress ends.
    quiet: tokio::sync::Notify,
}

/// Counts one emit in progress until dropped.
struct Emitting<'a>(&'a Listeners);

impl Drop for Emitting<'_> {
    fn drop(&mut self) {
        if self.0.emitting.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.quiet.notify_waiters();
        }
    }
}

impl Listeners {
    /// Pi's `dispose` clearing `_eventListeners`: the work shutdown
    /// cancels ends without telling anyone.
    pub(super) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.list
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// Waits until no listener call that started before [`Self::close`]
    /// is still running. An emit counts itself before it checks `closed`,
    /// and `close` sets `closed` before this reads the count, so every
    /// emit either sees `closed` or is counted here.
    pub(super) async fn quiesce(&self) {
        loop {
            let notified = self.quiet.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.emitting.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }

    pub(super) fn emit(&self, event: &SessionEvent<'_>) {
        self.emitting.fetch_add(1, Ordering::SeqCst);
        let _emitting = Emitting(self);
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let listeners: Vec<Listener> = self
            .list
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect();
        for listener in listeners {
            // Subscribers observe state; a panic must not prevent another
            // observer or the session's owner from completing its work.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(event)));
        }
    }
}

/// A session listener registration; [`Self::unsubscribe`] removes it.
pub struct SessionSubscription {
    pub(super) listeners: std::sync::Weak<Listeners>,
    pub(super) id: u64,
}

impl SessionSubscription {
    /// Removes the listener.
    pub fn unsubscribe(self) {
        if let Some(listeners) = self.listeners.upgrade() {
            listeners
                .list
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(|(id, _)| *id != self.id);
        }
    }
}
