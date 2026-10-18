//! Session tree navigation and forking.
//!
//! Ported from Pi v1.2.0 (`06a466935536f8c9bcacb2d5efc0fc021be1bdff`)
//! `packages/coding-agent/src/core/agent-session.ts` (`navigateTree`,
//! `getUserMessagesForForking`, `_refreshFinalizedContext`) and
//! `core/agent-session-runtime.ts` (`fork` with its default `before`
//! position), and Pi v1.3.0 (`71023aec1546d6d677a65669c5665c0927ce0dd0`)
//! `core/agent-session-runtime.ts` (`fork` with its `at` position, which
//! `/clone` uses). Branch summaries, labels, and the extension events
//! (`session_before_tree`, `session_tree`, `session_before_fork`) are not
//! ported: navigation only moves the leaf, and the branch it leaves stays
//! in the session file as it was.

use std::sync::Arc;

use bake_ai::AbortSignal;
use serde_json::Value;

use super::{AgentSession, lock};
use crate::session::{SessionEntry, SessionManager, SessionTree};

/// Pi's refusal while a response runs.
pub const NAVIGATION_WHILE_STREAMING: &str =
    "Wait for the current response to finish before navigating the session tree.";

/// Pi's refusal while a compaction or another navigation runs.
pub const NAVIGATION_WHILE_BUSY: &str = "Wait for the current compaction or tree navigation to finish before navigating the session tree.";

/// The refusal of a navigation whose signal fired before the leaf moved.
pub const NAVIGATION_CANCELLED: &str = "Navigation cancelled";

/// What [`AgentSession::navigate_tree`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeNavigation {
    /// Whether the leaf moved; `false` when the target was the leaf.
    pub moved: bool,
    /// The text of a chosen user or custom message, Pi's `editorText`, to
    /// edit and send again; the message itself is no longer on the path.
    pub editor_text: Option<String>,
}

/// A user message to fork from, Pi's `getUserMessagesForForking` item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkPoint {
    /// The message's entry id.
    pub entry_id: String,
    /// Its text.
    pub text: String,
}

/// Where a fork starts relative to its entry, Pi's runtime `fork`
/// `position` option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkPosition {
    /// Before a user message: the path up to its parent, as `/fork` does.
    Before,
    /// At any entry: the path up to and including it, as `/clone` does.
    At,
}

/// A forked session's store, built and not yet in use
/// ([`AgentSession::fork_session_manager`]).
#[derive(Debug)]
pub struct ForkedSession {
    /// The new session.
    pub session: SessionManager,
    /// The chosen message's text, Pi's `selectedText`; `None` for a fork
    /// [`ForkPosition::At`].
    pub selected_text: Option<String>,
}

/// Pi's `contentText(content, "")`: a string, or its text blocks joined.
/// Anything else, as in a malformed entry, is no text.
pub(crate) fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

/// The user message an entry holds: its text, when it is one.
fn user_message_text(entry: &SessionEntry) -> Option<String> {
    if entry.entry_type() != "message" {
        return None;
    }
    let message = entry.get("message")?;
    (message.get("role").and_then(Value::as_str) == Some("user"))
        .then(|| content_text(message.get("content")))
}

/// The parent an entry names, `None` for a root; an empty id links nowhere,
/// as in JavaScript.
fn parent_of(entry: &SessionEntry) -> Option<String> {
    entry
        .parent_id()
        .filter(|parent| !parent.is_empty())
        .map(str::to_owned)
}

impl AgentSession {
    /// Pi's `navigateTree` without a summary: moves the session's leaf
    /// within the same file and rebuilds the agent's transcript from the new
    /// path. A user or custom message moves the leaf to its parent (before
    /// the first entry at a root) and returns its text for the editor, so
    /// it can be edited and sent again; any other entry becomes the leaf.
    /// The current leaf is a no-op. Nothing is written: the next entry
    /// starts a branch there, and the branch left stays in the file.
    ///
    /// As in Pi, the model, thinking level, and the permission stay as they
    /// are; the next prompt re-derives the system prompt and tools against
    /// the new path's transcript.
    ///
    /// Serialized with the other changes and refused once the session is
    /// shut down or its file failed a write. It claims the session only
    /// while it is idle, refusing with Pi's messages while a response,
    /// compaction, or other navigation runs, and prompts and queued input
    /// are refused until it ends. A missing target or parent changes
    /// nothing.
    ///
    /// `signal` cancels it until the leaf moves: it is checked again under
    /// the session lock, immediately before the move, so a cancel that
    /// arrives while the change waits its turn or for the blocking pool is
    /// refused with [`NAVIGATION_CANCELLED`] and changes nothing. Once the
    /// move starts it completes; it writes nothing and cannot fail midway.
    pub async fn navigate_tree(
        &self,
        target_id: &str,
        signal: Option<AbortSignal>,
    ) -> Result<TreeNavigation, String> {
        let target_id = target_id.to_owned();
        self.run_change(move |control| async move {
            if signal.as_ref().is_some_and(AbortSignal::aborted) {
                return Err(NAVIGATION_CANCELLED.to_owned());
            }
            let mut refusal = None;
            let streaming = control.agent.is_streaming();
            control.core.activity.send_if_modified(|activity| {
                if activity.run || streaming {
                    refusal = Some(NAVIGATION_WHILE_STREAMING);
                    false
                } else if !activity.idle() {
                    refusal = Some(NAVIGATION_WHILE_BUSY);
                    false
                } else {
                    activity.navigation = true;
                    true
                }
            });
            if let Some(refusal) = refusal {
                return Err(refusal.to_owned());
            }
            // Cleared however this ends; the change runs on its own task, so
            // it ends.
            let _claim = control
                .core
                .busy(|_| {}, |activity| activity.navigation = false);
            control
                .core
                .cancel_background_compaction("navigation")
                .await;
            let core = Arc::clone(&control.core);
            let navigate = move || {
                #[cfg(test)]
                {
                    let hook = core.hooks().before_navigation.take();
                    if let Some(hook) = hook {
                        hook();
                    }
                }
                let mut session = lock(&core.session);
                // The commit boundary: a cancel seen here moves nothing.
                if signal.as_ref().is_some_and(AbortSignal::aborted) {
                    return Err(NAVIGATION_CANCELLED.to_owned());
                }
                if session.leaf_id() == Some(target_id.as_str()) {
                    return Ok(TreeNavigation::default());
                }
                let Some(target) = session.entry(&target_id).cloned() else {
                    return Err(format!("Entry {target_id} not found"));
                };
                let (new_leaf, editor_text) = match target.entry_type() {
                    "custom_message" => (
                        parent_of(&target),
                        Some(content_text(target.get("content"))),
                    ),
                    _ => match user_message_text(&target) {
                        Some(text) => (parent_of(&target), Some(text)),
                        None => (Some(target_id.clone()), None),
                    },
                };
                match &new_leaf {
                    Some(leaf) => session.branch(leaf).map_err(|error| error.to_string())?,
                    None => session.reset_leaf(),
                }
                // Under the session lock, so no reader sees the new leaf
                // with the old transcript.
                core.refresh_context(&session.build_session_projection());
                Ok(TreeNavigation {
                    moved: true,
                    editor_text,
                })
            };
            control.blocking(navigate, "Session I/O").await?
        })
        .await
    }

    /// The session's tree and its leaf, Pi's `getTree` and `getLeafId`,
    /// as `/tree` lists them. Reads only.
    pub fn session_tree(&self) -> (SessionTree, Option<String>) {
        let session = lock(&self.session);
        (session.tree(), session.leaf_id().map(str::to_owned))
    }

    /// The leaf entry's id, Pi's `getLeafId`.
    pub fn leaf_id(&self) -> Option<String> {
        lock(&self.session).leaf_id().map(str::to_owned)
    }

    /// Pi's `getUserMessagesForForking`: every user message in the file,
    /// on any branch, in file order, that has text.
    pub fn user_messages_for_forking(&self) -> Vec<ForkPoint> {
        lock(&self.session)
            .entries()
            .into_iter()
            .filter_map(|entry| {
                let text = user_message_text(entry)?;
                (!text.is_empty()).then(|| ForkPoint {
                    entry_id: entry.id().to_owned(),
                    text,
                })
            })
            .collect()
    }

    /// The store of a session forked at `entry_id`, Pi's runtime `fork`.
    /// [`ForkPosition::Before`] takes a user message and copies the path up
    /// to its parent into a new session, or starts an empty session when
    /// the message is a root; [`ForkPosition::At`] takes any entry and
    /// copies the path up to and including it. The running session, its
    /// file, and its writer lock are not touched
    /// ([`SessionManager::branched_copy`]);
    /// a persisted fork goes in the same session directory, names this
    /// session's file as its parent, and holds its own file's lock, and an
    /// in-memory session forks in memory. Refused after shutdown.
    pub async fn fork_session_manager(
        &self,
        entry_id: &str,
        position: ForkPosition,
    ) -> Result<ForkedSession, String> {
        let permit = self.change_permit()?;
        let session = Arc::clone(&self.session);
        let entry_id = entry_id.to_owned();
        let fork = permit.spawn_blocking(move || {
            let source = lock(&session);
            let invalid = || "Invalid entry ID for forking".to_owned();
            let entry = source.entry(&entry_id).ok_or_else(invalid)?;
            let (leaf, selected_text) = match position {
                ForkPosition::At => (Some(entry.id().to_owned()), None),
                ForkPosition::Before => {
                    let text = user_message_text(entry).ok_or_else(invalid)?;
                    (parent_of(entry), Some(text))
                }
            };
            if source.is_persisted()
                && leaf.is_some()
                && !source.session_file().is_some_and(std::path::Path::exists)
            {
                return Err(
                    "This session has not been saved yet. Send a message before cloning or forking it."
                        .to_owned(),
                );
            }
            let forked = source
                .branched_copy(leaf.as_deref())
                .map_err(|error| error.to_string())?;
            Ok(ForkedSession {
                session: forked,
                selected_text,
            })
        });
        fork.map_err(|refused| format!("Session I/O task failed: {refused}"))?
            .await
            .map_err(|error| format!("Session I/O task failed: {error}"))?
    }
}
