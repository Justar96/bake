//! Observation of actual summary attempts, including discarded partial work.

use std::sync::Arc;

use bake_agent::ModelCallAdmission;
use bake_ai::{AssistantMessage, StopReason, Usage};
use serde::Serialize;
use tokio::time::Instant;

/// One admitted summary attempt, independently of whether its complete
/// compaction candidate was eventually committed.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryAttempt {
    /// Its shared debit, when the summary belongs to a controlled run.
    pub admission: Option<ModelCallAdmission>,
    /// Whether the provider was called. Local cancellation after a mandatory
    /// debit write keeps that debit without dispatching a request.
    pub dispatched: bool,
    /// Provider usage when it is distinguishable from the message's empty
    /// default. Missing usage and an all-zero default remain `None`.
    pub usage: Option<Usage>,
    /// The provider's terminal reason, absent if dispatch or streaming failed
    /// without producing a terminal response.
    pub stop_reason: Option<StopReason>,
    /// The request's failure, if any.
    pub error: Option<String>,
    /// Time from dispatch through owned provider cleanup.
    pub elapsed_ms: u64,
}

/// An ordinary observer. Its panic cannot fail or repeat a model request.
pub type SummaryAttemptObserver = Arc<dyn Fn(&SummaryAttempt) + Send + Sync>;

pub(super) fn observe(
    observer: Option<&SummaryAttemptObserver>,
    admission: Option<ModelCallAdmission>,
    dispatched: bool,
    result: &Result<AssistantMessage, String>,
    started: Instant,
) {
    let Some(observer) = observer else {
        return;
    };
    let (usage, stop_reason, error) = match result {
        Ok(response) => (
            (response.usage != Usage::default()).then_some(response.usage),
            Some(response.stop_reason),
            response.error_message.clone(),
        ),
        Err(error) => (None, None, Some(error.clone())),
    };
    let attempt = SummaryAttempt {
        admission,
        dispatched,
        usage,
        stop_reason,
        error,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer(&attempt)));
}
