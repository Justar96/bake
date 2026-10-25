//! The session's approval recorder: writes the permission engine's
//! approval entries ([`crate::permissions::history`]) at the session's write
//! boundary, as the plugin veto writes its decision (`plugin_veto.rs`).
//!
//! Each entry is a durable group of one (`SessionManager::append_durable_group`),
//! written and synced before the engine goes on, and kept in memory only
//! once written. Cancellation is linearized at the write's admission, under
//! the session lock, as [`ApprovalEntry::admitted`] states. A failed write
//! records the session's gap and stops the session, as a failed plugin
//! decision write does; the engine then refuses the call with the text
//! returned, before any effect. No write is admitted after
//! `AgentSession::shutdown` returns, and a recorder whose session is gone
//! records nothing and refuses. A session that does not persist binds no
//! recorder (`AgentSession::record_approvals`).

use std::sync::{Arc, Mutex, Weak};

use bake_agent::BoxFuture;
use bake_ai::AbortSignal;

use super::recovery::Core;
use super::{WriteRefused, write_session};
use crate::permissions::history::{APPROVAL_CUSTOM_TYPE, ApprovalEntry, ApprovalRecorder};
use crate::session::GroupEntry;

/// The refusal of a call whose session is shut down.
const SHUT_DOWN: &str = "The session is shut down.";

/// Records one session's approval entries.
pub(super) struct SessionRecorder {
    core: Weak<Core>,
    persist_error: Arc<Mutex<Option<String>>>,
}

impl SessionRecorder {
    pub(super) fn new(core: &Arc<Core>, persist_error: Arc<Mutex<Option<String>>>) -> Self {
        Self {
            core: Arc::downgrade(core),
            persist_error,
        }
    }
}

impl ApprovalRecorder for SessionRecorder {
    fn record(
        &self,
        entry: ApprovalEntry,
        cancel: Option<AbortSignal>,
    ) -> BoxFuture<'static, Result<Option<ApprovalEntry>, String>> {
        let core = self.core.clone();
        let persist_error = Arc::clone(&self.persist_error);
        Box::pin(async move {
            let Some(core) = core.upgrade() else {
                return Err(SHUT_DOWN.to_owned());
            };
            persist_entry(&core, &persist_error, entry, cancel).await
        })
    }
}

/// Writes `entry` as [`ApprovalEntry::admitted`] turns it at admission and
/// returns what was written. The write is blocking work of the session's
/// gate, admitted under a permit taken before it starts, so shutdown waits
/// for it even if the call is cancelled meanwhile. A failure records the
/// gap, stops the run, and returns the call's refusal.
async fn persist_entry(
    core: &Core,
    persist_error: &Mutex<Option<String>>,
    entry: ApprovalEntry,
    signal: Option<AbortSignal>,
) -> Result<Option<ApprovalEntry>, String> {
    let Some(permit) = core.work.enter() else {
        return Err(SHUT_DOWN.to_owned());
    };
    let session = Arc::clone(&core.session);
    let gap = Arc::clone(&core.gap);
    #[cfg(test)]
    let (before_admission, admitted, written_hook) = {
        let hooks = core.hooks();
        (
            hooks.before_approval_admission.clone(),
            hooks.approval_admitted.clone(),
            hooks.approval_written.clone(),
        )
    };
    let write = permit.spawn_blocking(move || {
        #[cfg(test)]
        if let Some(hook) = &before_admission {
            hook(&entry);
        }
        let written = write_session(&session, &gap, |session| {
            // The admission: what an abort by now turns the entry into.
            let aborted = signal.as_ref().is_some_and(AbortSignal::aborted);
            let Some(entry) = entry.admitted(aborted) else {
                return Ok(None);
            };
            #[cfg(test)]
            if let Some(hook) = &admitted {
                hook(&entry, session);
            }
            session
                .append_durable_group([GroupEntry::Custom {
                    custom_type: APPROVAL_CUSTOM_TYPE.to_owned(),
                    data: Some(entry.to_data()),
                }])
                .map(|_| Some(entry))
                .map_err(|error| error.to_string())
        });
        #[cfg(test)]
        if let (Ok(Some(entry)), Some(hook)) = (&written, &written_hook) {
            hook(entry);
        }
        written
    });
    drop(permit);
    // Refused only outside a runtime: nothing ran, as when the gate is shut.
    let written = write.map_err(|refused| refused.to_string())?.await;
    let error = match written {
        Ok(Ok(written)) => return Ok(written),
        Ok(Err(WriteRefused::AfterGap(error) | WriteRefused::Failed(error))) => error,
        Err(error) => {
            // Whether the entry landed is unknown, so later writes stop.
            let error = format!("Session I/O task failed: {error}");
            super::lock_gap(&core.gap).get_or_insert_with(|| error.clone());
            error
        }
    };
    super::plugin_veto::stop(core, persist_error, error.clone());
    Err(format!(
        "The approval could not be written to the session file ({error}), so the session stopped and takes no more prompts."
    ))
}
