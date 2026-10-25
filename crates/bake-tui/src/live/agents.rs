//! The session's subagents as the view shows them
//! ([`bake_tui_view::agents`]): each child [`SubagentHost::list`] reports,
//! with how its last run in this process ended and what that run said. A
//! session without the subagent tools has none, and a resumed session
//! lists the children its file records as saved.

use bake_coding_agent::subagents::messages::StopReason;
use bake_coding_agent::subagents::{ChildRow, Status, SubagentHost};
use bake_tui_view::agents::{Agent, AgentState, Ended};

/// The children `host` keeps, in start order; none without a host.
pub fn agents(host: Option<&SubagentHost>) -> Vec<Agent> {
    host.map(|host| host.list().iter().map(agent).collect())
        .unwrap_or_default()
}

fn agent(row: &ChildRow) -> Agent {
    let ended = row.settled.as_ref().map(|settled| match settled.stop {
        StopReason::Completed => Ended::Completed,
        StopReason::Error | StopReason::MaxTokens => Ended::Failed,
        StopReason::Aborted | StopReason::Interrupted => Ended::Stopped,
    });
    let state = match row.status {
        Status::Starting => AgentState::Starting,
        Status::Running => AgentState::Working,
        Status::Idle => AgentState::Idle(ended),
        Status::Ready => AgentState::Saved(ended),
        Status::Unavailable => AgentState::Unavailable,
    };
    // A failure's diagnostic says more than whatever it last wrote.
    let detail = row.settled.as_ref().and_then(|settled| {
        settled
            .diagnostic
            .clone()
            .filter(|_| ended == Some(Ended::Failed))
            .or_else(|| settled.closing.clone())
    });
    Agent {
        id: row.id.clone(),
        name: row.label.clone(),
        state,
        route: Some(row.route.clone()),
        detail,
        file: row.file.as_ref().map(|file| file.display().to_string()),
    }
}

/// Stops the current run of the child `id` names for the user, as
/// `interrupt_agent` does; its settlement notice reaches the model. An id no
/// longer kept is ignored.
pub fn stop(host: Option<&SubagentHost>, id: &str) {
    if let Some(host) = host {
        let _ = host.interrupt(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bake_coding_agent::subagents::Settlement;
    use std::path::PathBuf;

    fn row(status: Status, stop: Option<StopReason>) -> ChildRow {
        ChildRow {
            id: "child-1".into(),
            status,
            label: "count things".into(),
            route: "faux/faux-2".into(),
            run: 1,
            settled: stop.map(|stop| Settlement {
                run: 1,
                stop,
                closing: Some("12 files".into()),
                diagnostic: Some("request failed".into()),
            }),
            file: Some(PathBuf::from("/s/subagents/child-1.jsonl")),
        }
    }

    // Every status and stop reason maps to the view's state, and a failed
    // run shows its diagnostic, any other its closing message.
    #[test]
    fn rows_map_to_states_and_last_words() {
        let cases = [
            (row(Status::Starting, None), AgentState::Starting, None),
            (row(Status::Running, None), AgentState::Working, None),
            (
                row(Status::Idle, Some(StopReason::Completed)),
                AgentState::Idle(Some(Ended::Completed)),
                Some("12 files"),
            ),
            (
                row(Status::Idle, Some(StopReason::MaxTokens)),
                AgentState::Idle(Some(Ended::Failed)),
                Some("request failed"),
            ),
            (
                row(Status::Ready, Some(StopReason::Error)),
                AgentState::Saved(Some(Ended::Failed)),
                Some("request failed"),
            ),
            (
                row(Status::Idle, Some(StopReason::Aborted)),
                AgentState::Idle(Some(Ended::Stopped)),
                Some("12 files"),
            ),
            (
                row(Status::Ready, Some(StopReason::Interrupted)),
                AgentState::Saved(Some(Ended::Stopped)),
                Some("12 files"),
            ),
            (row(Status::Ready, None), AgentState::Saved(None), None),
            (
                row(Status::Unavailable, None),
                AgentState::Unavailable,
                None,
            ),
        ];
        for (row, state, detail) in cases {
            let shown = agent(&row);
            assert_eq!(
                (shown.state, shown.detail.as_deref()),
                (state, detail),
                "{row:?}"
            );
            assert_eq!(
                (
                    shown.id.as_str(),
                    shown.name.as_str(),
                    shown.route.as_deref()
                ),
                ("child-1", "count things", Some("faux/faux-2"))
            );
            assert_eq!(shown.file.as_deref(), Some("/s/subagents/child-1.jsonl"));
        }
        assert!(agents(None).is_empty());
        stop(None, "child-1");
    }
}
