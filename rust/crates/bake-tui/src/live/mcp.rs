//! MCP diagnostics follow the registry's current state, independently of
//! model turns. The driver owns the subscription and stops reading it
//! before shutting down or replacing the session.

use bake_coding_agent::mcp::{McpServerState, McpServerStatus, McpServers};
use bake_tui_view::transcript::Row;
use tokio::sync::watch;

pub(super) struct McpNotices {
    servers: McpServers,
    changes: Option<watch::Receiver<u64>>,
    previous: Vec<McpServerStatus>,
}

impl McpNotices {
    pub(super) fn new(servers: &McpServers) -> Self {
        Self {
            servers: servers.clone(),
            changes: Some(servers.subscribe()),
            previous: Vec::new(),
        }
    }

    pub(super) async fn changed(&mut self) {
        match &mut self.changes {
            Some(changes) => {
                if changes.changed().await.is_err() {
                    self.changes = None;
                }
            }
            None => std::future::pending().await,
        }
    }

    pub(super) fn take_rows(&mut self) -> Vec<Row> {
        if let Some(changes) = &mut self.changes {
            changes.borrow_and_update();
        }
        let current = self.servers.statuses();
        let rows = changed_rows(&self.previous, &current);
        self.previous = current;
        rows
    }
}

fn changed_rows(previous: &[McpServerStatus], current: &[McpServerStatus]) -> Vec<Row> {
    current
        .iter()
        .filter(|status| !previous.contains(status))
        .filter_map(|status| {
            // Server errors can contain stderr, credentials, and terminal
            // controls. Only the configured name reaches this diagnostic.
            let name = super::visible(&status.name);
            let message = match &status.state {
                McpServerState::Refused { .. } => {
                    format!("MCP server \"{name}\" was refused; its tools are unavailable.")
                }
                McpServerState::Failed { .. } => {
                    format!("MCP server \"{name}\" failed to start; its tools are unavailable.")
                }
                McpServerState::Disconnected { .. } => {
                    format!("MCP server \"{name}\" disconnected; its tools are unavailable.")
                }
                McpServerState::Connected {
                    refresh_error: Some(_),
                    ..
                } => format!(
                    "MCP server \"{name}\" could not refresh its tools; using its previous tool list."
                ),
                _ => return None,
            };
            Some(Row::Error(message))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(state: McpServerState) -> Vec<McpServerStatus> {
        vec![McpServerStatus {
            name: "fixture".into(),
            state,
        }]
    }

    #[test]
    fn initial_and_distinct_failures_are_visible_but_repeated_snapshots_are_quiet() {
        let failed = snapshot(McpServerState::Failed {
            error: "first".into(),
        });
        assert_eq!(changed_rows(&[], &failed).len(), 1);
        assert!(changed_rows(&failed, &failed).is_empty());
        let distinct = snapshot(McpServerState::Failed {
            error: "second".into(),
        });
        assert_eq!(changed_rows(&failed, &distinct).len(), 1);
        let connecting = snapshot(McpServerState::Connecting);
        assert!(changed_rows(&failed, &connecting).is_empty());
        assert_eq!(changed_rows(&connecting, &failed).len(), 1);
        let refused = snapshot(McpServerState::Refused {
            reason: "private reason".into(),
        });
        assert_eq!(changed_rows(&[], &refused).len(), 1);
    }

    #[test]
    fn a_new_generation_can_report_the_same_refresh_failure() {
        let failed = |generation| {
            snapshot(McpServerState::Connected {
                tools: 1,
                generation,
                refresh_error: Some("same error".into()),
            })
        };
        assert!(changed_rows(&failed(1), &failed(1)).is_empty());
        assert_eq!(changed_rows(&failed(1), &failed(2)).len(), 1);
    }

    #[test]
    fn server_output_is_omitted_and_names_cannot_control_the_terminal() {
        let status = McpServerStatus {
            name: "host\u{1b}[2J\n\u{202e}name".into(),
            state: McpServerState::Failed {
                error: "SYNTHETIC_SECRET\u{1b}[2J".into(),
            },
        };
        assert_eq!(
            changed_rows(&[], &[status]),
            [Row::Error(
                "MCP server \"host\\u{1b}[2J\\u{a}\\u{202e}name\" failed to start; its tools are unavailable."
                    .into()
            )]
        );
    }

    #[test]
    fn ordinary_transitions_and_shutdown_are_quiet() {
        for state in [
            McpServerState::Disabled,
            McpServerState::Connecting,
            McpServerState::Connected {
                tools: 1,
                generation: 1,
                refresh_error: None,
            },
            McpServerState::Stopping { retrying: false },
            McpServerState::Stopping { retrying: true },
            McpServerState::Closed,
        ] {
            assert!(changed_rows(&[], &snapshot(state)).is_empty());
        }
    }
}
