//! Immutable job facts in Pi custom entries. Each write belongs to the
//! registry's driver, which waits through it even during session shutdown.

use std::sync::Arc;

use serde_json::json;

use super::{Core, lock, lock_gap, write_session};
use crate::jobs::{JobRegistry, JobSnapshot};
use crate::session::GroupEntry;

pub(super) const JOURNAL_TYPE: &str = "bake.job.state";

pub(super) fn attach(core: &Arc<Core>, jobs: &JobRegistry) -> Result<(), String> {
    let core = Arc::downgrade(core);
    jobs.attach_recorder(Arc::new(move |snapshot| {
        let core = core.clone();
        Box::pin(async move {
            let core = core.upgrade().ok_or("The job's session is gone.")?;
            let owner = Arc::clone(&core);
            // The admitted job driver owns and awaits this write. A new
            // root-gate admission would fail during shutdown, before the
            // terminal fact could be written.
            let result = tokio::task::spawn_blocking(move || record(&owner, &snapshot))
                .await
                .map_err(|error| format!("Job journal I/O task failed: {error}"))
                .and_then(|result| result);
            if let Err(error) = &result {
                // A failed worker also closes the write boundary, even
                // when it could not return through write_session.
                {
                    let _session = lock(&core.session);
                    lock_gap(&core.gap).get_or_insert_with(|| error.clone());
                }
                core.abort_requested
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                if let Some(agent) = core.agent.get().and_then(std::sync::Weak::upgrade) {
                    agent.abort();
                }
            }
            result
        })
    }))
}

fn record(core: &Core, snapshot: &JobSnapshot) -> Result<(), String> {
    let terminal = snapshot.status.is_terminal();
    let data = json!({
        "version": 1,
        "phase": if terminal { "terminal" } else { "started" },
        "jobId": snapshot.id.to_string(),
        "kind": snapshot.id.kind(),
        "label": snapshot.label,
        "status": if terminal { snapshot.status.as_str() } else { "running" },
        "detail": snapshot.detail,
        "startedAt": snapshot.started_at,
        "finishedAt": snapshot.finished_at,
        "outputRetained": false,
    });
    write_session(&core.session, &core.gap, |session| {
        session.append_durable_group([GroupEntry::Custom {
            custom_type: JOURNAL_TYPE.into(),
            data: Some(data),
        }])
    })
    .map_err(|error| error.message())?;
    #[cfg(test)]
    {
        let hook = core.hooks().after_job_record.clone();
        if let Some(hook) = hook {
            hook(snapshot);
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "job_journal_tests.rs"]
mod tests;
