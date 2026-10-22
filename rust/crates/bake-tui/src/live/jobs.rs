//! The session's background shells as the view shows them
//! ([`bake_tui_view::jobs`]): each retained `bash` job's state, time, and
//! newest output, read without consuming what the model has not read
//! ([`JobRegistry::views`]). Jobs belong to this process; a new session
//! starts with none, and none is shown after a restart.

use std::time::Duration;

use bake_coding_agent::jobs::{JobRegistry, JobStatus, JobView};
use bake_tui_view::jobs::{BackgroundJob, JobState};

/// The shells `jobs` keeps, in start order, at `now_ms`.
pub fn shells(jobs: &JobRegistry, now_ms: i64) -> Vec<BackgroundJob> {
    jobs.views()
        .iter()
        .filter(|view| view.snapshot.id.kind() == "bash")
        .map(|view| shell(view, now_ms))
        .collect()
}

fn shell(view: &JobView, now_ms: i64) -> BackgroundJob {
    let snapshot = &view.snapshot;
    let detail = snapshot.detail.clone();
    let state = match snapshot.status {
        JobStatus::Running => JobState::Running,
        // A producer that returned is still draining; it reads as stopping
        // only when a stop was asked for.
        JobStatus::Completing if snapshot.stop.is_none() => JobState::Running,
        JobStatus::Stopping | JobStatus::Completing => JobState::Stopping,
        JobStatus::Completed => JobState::Completed(detail),
        JobStatus::Killed => JobState::Killed(detail),
        JobStatus::Failed => JobState::Failed(detail),
    };
    let end = snapshot.finished_at.unwrap_or(now_ms);
    BackgroundJob {
        id: snapshot.id.to_string(),
        command: snapshot.label.clone(),
        state,
        elapsed: Duration::from_millis(u64::try_from(end - snapshot.started_at).unwrap_or(0)),
        output: view.tail.clone(),
        omitted_bytes: view.omitted_bytes,
    }
}

/// Whether two reports differ in more than their running clocks, which the
/// view advances itself.
pub fn changed(old: &[BackgroundJob], new: &[BackgroundJob]) -> bool {
    old.len() != new.len()
        || old.iter().zip(new).any(|(a, b)| {
            a.id != b.id
                || a.state != b.state
                || a.output != b.output
                || a.omitted_bytes != b.omitted_bytes
                || (!b.state.is_live() && a.elapsed != b.elapsed)
        })
}

/// Stops the shell `id` names for the user, so the model hears of it
/// ([`JobRegistry::stop`]). An id no longer kept is ignored.
pub fn stop(jobs: &JobRegistry, id: &str) {
    if let Ok(id) = jobs.id(id) {
        let _ = jobs.stop(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bake_coding_agent::jobs::{JobLimits, JobOutcome, JobSpec, OutputMode};
    use bake_coding_agent::jobs::{StopCause, USER_STOP_REASON};
    use bake_coding_agent::owned_work::WorkGate;

    /// Whether a stop the user asked for is what ended a job.
    fn by_user(cause: Option<&StopCause>) -> bool {
        matches!(cause, Some(StopCause::Kill { reason: Some(reason) }) if reason == USER_STOP_REASON)
    }

    // A running shell shows its output without consuming it, a user's
    // stop leaves its report for the model, and the stopped shell reads as
    // such; only `bash` jobs are shells.
    #[tokio::test]
    async fn shells_follow_the_registry_and_a_stop_is_the_users() {
        let jobs = JobRegistry::new(WorkGate::default(), JobLimits::default());
        let (wrote_tx, wrote) = tokio::sync::oneshot::channel();
        let id = jobs
            .start(
                JobSpec {
                    kind: "bash",
                    label: "bun run build".into(),
                    output: OutputMode::Stream,
                },
                move |context| async move {
                    context.output().expect("stream").append(b"compiling\n");
                    let _ = wrote_tx.send(());
                    context.signal().cancelled().await;
                    JobOutcome::killed("killed before exit")
                },
            )
            .expect("started");
        wrote.await.expect("wrote");
        let now = bake_ai::now_ms();
        let shown = shells(&jobs, now);
        assert_eq!(shown.len(), 1);
        assert_eq!(
            (
                shown[0].id.as_str(),
                shown[0].command.as_str(),
                &shown[0].state
            ),
            ("bash-1", "bun run build", &JobState::Running)
        );
        assert_eq!(shown[0].output, "compiling\n");
        assert!(
            !changed(&shown, &shells(&jobs, now + 5_000)),
            "a clock is not a change"
        );
        stop(&jobs, "bash-1");
        stop(&jobs, "bash-99");
        let mut events = jobs.subscribe();
        while !jobs.get(id).expect("kept").status.is_terminal() {
            events.changed().await;
        }
        let after = shells(&jobs, bake_ai::now_ms());
        assert!(changed(&shown, &after));
        assert_eq!(
            after[0].state,
            JobState::Killed(Some("killed before exit".into()))
        );
        assert!(by_user(jobs.get(id).expect("kept").stop.as_ref()));
        assert_eq!(jobs.claim_unreported().len(), 1, "the model is told");
        assert_eq!(
            jobs.read(id).expect("read").text,
            "compiling\n",
            "nothing consumed"
        );
        jobs.close().await;
    }
}
