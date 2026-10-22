//! Cooperative cancellation, the counterpart of the web `AbortSignal` Pi's
//! stream options carry (see Pi `packages/ai/src/utils/abort.ts`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

#[derive(Debug, Default)]
struct AbortState {
    aborted: AtomicBool,
    notify: Notify,
}

/// Aborts every [`AbortSignal`] it hands out.
#[derive(Debug, Clone, Default)]
pub struct AbortController {
    state: Arc<AbortState>,
}

impl AbortController {
    /// A controller that has not aborted.
    pub fn new() -> Self {
        Self::default()
    }

    /// A signal that observes this controller.
    pub fn signal(&self) -> AbortSignal {
        AbortSignal {
            state: Arc::clone(&self.state),
        }
    }

    /// Abort. Later calls do nothing.
    pub fn abort(&self) {
        if !self.state.aborted.swap(true, Ordering::SeqCst) {
            self.state.notify.notify_waiters();
        }
    }
}

/// Observes an [`AbortController`].
#[derive(Debug, Clone)]
pub struct AbortSignal {
    state: Arc<AbortState>,
}

impl AbortSignal {
    /// Whether the controller aborted.
    pub fn aborted(&self) -> bool {
        self.state.aborted.load(Ordering::SeqCst)
    }

    /// Completes once the controller aborts; at once if it already did.
    pub async fn cancelled(&self) {
        loop {
            // `notify_waiters` wakes every `Notified` created before it, so
            // creating the future before the check cannot miss an abort.
            let notified = self.state.notify.notified();
            if self.aborted() {
                return;
            }
            notified.await;
        }
    }
}

/// Completes when `signal` aborts, or never without one.
pub(crate) async fn cancelled(signal: Option<&AbortSignal>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

/// Whether `signal` is present and aborted.
pub(crate) fn is_aborted(signal: Option<&AbortSignal>) -> bool {
    signal.is_some_and(AbortSignal::aborted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_resolves_after_abort_and_immediately_when_already_aborted() {
        let controller = AbortController::new();
        let signal = controller.signal();
        assert!(!signal.aborted());
        let waiter = tokio::spawn({
            let signal = signal.clone();
            async move { signal.cancelled().await }
        });
        tokio::task::yield_now().await;
        controller.abort();
        waiter.await.unwrap();
        assert!(signal.aborted());
        signal.cancelled().await;
    }
}
