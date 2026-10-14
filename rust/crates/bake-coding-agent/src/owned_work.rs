//! Work a session owns that runs outside the task awaiting it: a
//! `spawn_blocking` session write keeps running when the run awaiting it is
//! cancelled, and a model request streams on a task of its own. Each piece
//! of work holds a [`WorkPermit`] until it finishes; [`WorkGate::close`]
//! refuses new work and waits for every permit, so teardown leaves nothing
//! running behind it.
//!
//! Bake's own; Pi's Node runtime has no detached work to await.

use std::sync::Arc;

use tokio::sync::{OnceCell, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

/// Admits work until closed, then waits for it.
#[derive(Debug, Clone, Default)]
pub struct WorkGate {
    lock: Arc<RwLock<()>>,
    /// Held once closed, so no permit is granted again.
    closed: Arc<OnceCell<OwnedRwLockWriteGuard<()>>>,
}

/// Admission for one piece of work; dropping it ends that work's claim.
#[derive(Debug)]
pub struct WorkPermit {
    _guard: OwnedRwLockReadGuard<()>,
}

impl WorkGate {
    /// A permit, or `None` once the gate is closing or closed.
    pub fn enter(&self) -> Option<WorkPermit> {
        Arc::clone(&self.lock)
            .try_read_owned()
            .ok()
            .map(|guard| WorkPermit { _guard: guard })
    }

    /// Refuses new work and waits until every permit is dropped. Every
    /// caller, concurrent or later, returns once the gate is closed.
    pub async fn close(&self) {
        self.closed
            .get_or_init(|| Arc::clone(&self.lock).write_owned())
            .await;
    }

    /// Whether [`Self::close`] finished.
    pub fn is_closed(&self) -> bool {
        self.closed.initialized()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn close_waits_for_permits_and_refuses_new_ones() {
        let gate = WorkGate::default();
        let permit = gate.enter().expect("open gate admits work");
        let closing = tokio::spawn({
            let gate = gate.clone();
            async move { gate.close().await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!closing.is_finished(), "close waits for the permit");
        assert!(gate.enter().is_none(), "a closing gate admits nothing");
        drop(permit);
        closing.await.expect("close finishes");
        assert!(gate.is_closed());
        assert!(gate.enter().is_none(), "a closed gate admits nothing");
        // Closing again returns at once.
        tokio::time::timeout(Duration::from_secs(1), gate.close())
            .await
            .expect("a second close returns");
    }
}
