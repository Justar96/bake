//! What the driver sends the view besides a run's own rows: MCP notices,
//! background shells and subagents, the pending-input panel, and the
//! status line.

use super::*;

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
    /// footer reading ([`AgentSession::context_usage`]).
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
        let model = self.session.model();
        let context = self.session.context_usage().map(|usage| ContextUsage {
            used: usage.tokens,
            window: usage.context_window,
        });
        SessionStatus {
            thinking: model
                .as_ref()
                .filter(|model| model.reasoning)
                .map(|_| self.session.thinking_level().as_str().to_owned()),
            model: model.map(|model| format!("{}/{}", model.provider, model.id)),
            context,
        }
    }
}
