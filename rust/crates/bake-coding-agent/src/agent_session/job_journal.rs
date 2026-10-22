//! Immutable job facts in Pi custom entries. Each write belongs to the
//! registry's driver, which waits through it even during session shutdown.
//! Other work's outcomes ([`JobRegistry::record_fact`]) are written the same
//! way, awaited by the gate task that then posts their notice.

use std::sync::Arc;

use serde_json::json;

use super::{Core, lock, lock_gap, write_session};
use crate::jobs::{JobRegistry, JobSnapshot};
use crate::session::GroupEntry;

pub(super) const JOURNAL_TYPE: &str = "bake.job.state";

pub(super) fn attach(core: &Arc<Core>, jobs: &JobRegistry) -> Result<(), String> {
    let weak = Arc::downgrade(core);
    jobs.attach_recorder(Arc::new(move |snapshot| {
        let core = weak.clone();
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
                write_failed(&core, error);
            }
            result
        })
    }))?;
    let weak = Arc::downgrade(core);
    jobs.attach_fact_recorder(Arc::new(move |custom_type, data| {
        let core = weak.clone();
        Box::pin(async move {
            let core = core.upgrade().ok_or("The session is gone.")?;
            let owner = Arc::clone(&core);
            // Awaited by the gate task that posts the matching notice, as
            // a job's terminal fact is, so shutdown's drain waits for it.
            let result = tokio::task::spawn_blocking(move || {
                write_session(&owner.session, &owner.gap, |session| {
                    session.append_durable_group([GroupEntry::Custom {
                        custom_type: custom_type.into(),
                        data: Some(data),
                    }])
                })
                .map(|_| ())
                .map_err(|error| error.message())
            })
            .await
            .map_err(|error| format!("Session fact I/O task failed: {error}"))
            .and_then(|result| result);
            if let Err(error) = &result {
                write_failed(&core, error);
            }
            result
        })
    }))
}

/// Stops the session after a journal write failed.
fn write_failed(core: &Core, error: &str) {
    // A failed worker also closes the write boundary, even when it could
    // not return through write_session.
    {
        let _session = lock(&core.session);
        lock_gap(&core.gap).get_or_insert_with(|| error.to_owned());
    }
    core.abort_requested
        .store(true, std::sync::atomic::Ordering::SeqCst);
    if let Some(control) = core.control() {
        control.fail(error.to_owned());
    }
    if let Some(agent) = core.agent.get().and_then(std::sync::Weak::upgrade) {
        agent.abort();
    }
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
