//! What the driver sends the view besides a run's own rows: MCP notices,
//! background shells and subagents, the pending-input panel, and the
//! status line.

use super::*;
use bake_coding_agent::spawned::{SpawnHost, SpawnedState};
use bake_tui_view::crew::{CrewMember, CrewState};

/// The host's agents as the crew panel draws them.
pub(super) fn crew(host: &SpawnHost) -> Vec<CrewMember> {
    let now = bake_ai::now_ms();
    host.list()
        .into_iter()
        .map(|agent| CrewMember {
            id: agent.id,
            name: agent.name,
            state: match agent.state {
                SpawnedState::Starting => CrewState::Starting,
                SpawnedState::Working => CrewState::Working,
                SpawnedState::Asking => CrewState::Asking,
                SpawnedState::Idle => CrewState::Idle,
                SpawnedState::Failed(reason) => CrewState::Failed(reason),
                // An agent that ended unsuccessfully shows as failed, with
                // why; one that was stopped or finished shows as exited.
                SpawnedState::Exited(Some(reason)) => CrewState::Failed(reason),
                SpawnedState::Exited(None) => CrewState::Exited(None),
            },
            tool: agent.tool,
            line: agent.last_line,
            task: agent.task,
            age: std::time::Duration::from_millis(
                u64::try_from(now.saturating_sub(agent.since)).unwrap_or(0),
            ),
        })
        .collect()
}

impl Driver {
    pub(super) fn publish_mcp(&mut self) {
        if let Some(mcp) = &mut self.mcp {
            let rows = mcp.take_rows();
            if !rows.is_empty() {
                self.send(RuntimeUpdate::Commit(rows));
            }
        }
    }

    /// Sends the background shells when more than their clocks changed,
    /// and the subagents when any changed; a replaced session's empty lists
    /// close what the view showed.
    pub(super) fn publish_background(&mut self) {
        let now = jobs::shells(self.session.jobs(), bake_ai::now_ms());
        if jobs::changed(&self.shells, &now) {
            self.shells = now.clone();
            self.send(RuntimeUpdate::Jobs(now));
        }
        let agents = agents::agents(self.session.subagents());
        if agents != self.agents {
            self.agents = agents.clone();
            self.send(RuntimeUpdate::Agents(agents));
        }
        // The spawned agents, when the host changed or a replaced session
        // has none; the view ages them by its own clock.
        let version = self.session.spawned().map(SpawnHost::version);
        if version != self.crew_version {
            self.crew_version = version;
            let crew = self.session.spawned().map(crew).unwrap_or_default();
            self.send(RuntimeUpdate::Crew(crew));
        }
    }

    /// Sends the pending panel: the session's queues and the driver's own.
    pub(super) fn publish_pending(&self) {
        let queued = self.session.queued_messages();
        let waiting: Vec<String> = self.waiting.iter().map(|q| q.text.clone()).collect();
        let update = self
            .projector
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_waiting(waiting, &queued.steering, &queued.follow_up);
        self.send(update);
    }

    /// Sends the status line's model, thinking level, and context use, Pi's
    /// footer reading ([`AgentSession::context_usage`]), the token totals
    /// and prompt cache when `statusLine` turns them on
    /// ([`AgentSession::usage_totals`]), and the session's name for the
    /// terminal's title.
    pub(super) fn status(&mut self) {
        let status = self.read_status();
        self.status = Some(status.clone());
        self.send(RuntimeUpdate::Status(status));
    }

    /// Sends the status line during a run, when it changed since it was
    /// last sent.
    pub(super) fn refresh_status(&mut self) {
        let status = self.read_status();
        if self.status.as_ref() != Some(&status) {
            self.status = Some(status.clone());
            self.send(RuntimeUpdate::Status(status));
        }
    }

    pub(super) fn read_status(&self) -> SessionStatus {
        read_status(&self.session)
    }
}

/// The status line's readings of `session`, as the driver sends them
/// ([`Driver::status`]); `/settings` sends them too when it changes what
/// the line shows.
pub(super) fn read_status(session: &AgentSession) -> SessionStatus {
    let model = session.model();
    let context = session.context_usage().map(|usage| ContextUsage {
        used: usage.tokens,
        window: usage.context_window,
    });
    let line = session.settings().status_line;
    let totals = line.any().then(|| session.usage_totals());
    SessionStatus {
        thinking: model
            .as_ref()
            .filter(|model| model.reasoning)
            .map(|_| session.thinking_level().as_str().to_owned()),
        model: model.map(|model| format!("{}/{}", model.provider, model.id)),
        context,
        tokens: totals.filter(|_| line.tokens).map(|totals| TokenTotals {
            input: totals.input,
            output: totals.output,
        }),
        cache: totals.filter(|_| line.cache).map(|totals| CacheReading {
            read: totals.cache_read,
            write: totals.cache_write,
            hit: totals
                .latest_cache_hit
                .map(|share| (share * 100.0).floor().clamp(0.0, 100.0) as u64),
        }),
        name: session.session_name(),
    }
}
