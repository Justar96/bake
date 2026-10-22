//! Session tree navigation, branch summaries, and forking.
//!
//! Ported from Pi v1.3.0 (`71023aec1546d6d677a65669c5665c0927ce0dd0`)
//! `packages/coding-agent/src/core/agent-session.ts` (`navigateTree` with
//! its branch summary and label, `abortBranchSummary`,
//! `getUserMessagesForForking`, `_refreshFinalizedContext`), and from Pi
//! v1.2.0 (`06a466935536f8c9bcacb2d5efc0fc021be1bdff`)
//! `core/agent-session-runtime.ts` (`fork` with its default `before`
//! position), and Pi v1.3.0 (`71023aec1546d6d677a65669c5665c0927ce0dd0`)
//! `core/agent-session-runtime.ts` (`fork` with its `at` position, which
//! `/clone` uses). The extension events (`session_before_tree`,
//! `session_tree`, `session_before_fork`) are not ported, so a summary is
//! never an extension's. Pi is MIT licensed; see the crate's `NOTICE`.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use bake_ai::{AbortController, AbortSignal, Usage};
use serde_json::Value;

use super::recovery::{Core, SummarizationSource};
use super::{AgentSession, lock};
use crate::compaction::branch_summarization::{
    GenerateBranchSummaryOptions, collect_entries_for_branch_summary, generate_branch_summary,
};
use crate::session::{SessionEntry, SessionManager, SessionTree};

/// Pi's refusal while a response runs.
pub const NAVIGATION_WHILE_STREAMING: &str =
    "Wait for the current response to finish before navigating the session tree.";

/// Pi's refusal while a compaction or another navigation runs.
pub const NAVIGATION_WHILE_BUSY: &str = "Wait for the current compaction or tree navigation to finish before navigating the session tree.";

/// The refusal of a navigation whose signal fired before the leaf moved.
pub const NAVIGATION_CANCELLED: &str = "Navigation cancelled";

/// The refusal of a navigation whose branch summary was cancelled, Pi's
/// `aborted` result and its status text: nothing was written and the leaf
/// stayed.
pub const BRANCH_SUMMARY_CANCELLED: &str = "Branch summarization cancelled";

/// Pi's refusal of a summary without a model.
pub const NO_SUMMARY_MODEL: &str = "No model available for summarization";

/// Pi's `navigateTree` options.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeNavigationOptions {
    /// Whether to summarize the branch left, Pi's `summarize`.
    pub summarize: bool,
    /// Focus for the summary, Pi's `customInstructions`.
    pub custom_instructions: Option<String>,
    /// Whether `custom_instructions` replaces the summary prompt, Pi's
    /// `replaceInstructions`.
    pub replace_instructions: bool,
    /// A label for the summary entry, or for the target without a summary,
    /// Pi's `label`.
    pub label: Option<String>,
}

/// What [`AgentSession::navigate_tree`] did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TreeNavigation {
    /// Whether the leaf moved; `false` when the target was the leaf.
    pub moved: bool,
    /// The text of a chosen user or custom message, Pi's `editorText`, to
    /// edit and send again; the message itself is no longer on the path.
    pub editor_text: Option<String>,
    /// The `branch_summary` entry written, Pi's `summaryEntry`.
    pub summary_entry: Option<SessionEntry>,
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

/// A branch summary ready to write: Pi's `summaryText`, `summaryDetails`,
/// and `summaryUsage`.
struct BranchSummary {
    text: String,
    details: Value,
    usage: Option<Usage>,
}

/// What the summary step of [`AgentSession::navigate_tree`] found.
enum Prepared {
    /// The target is the leaf: nothing to do.
    AtTarget,
    /// The summary to write, `None` when there was nothing to summarize.
    Summary(Option<Box<BranchSummary>>),
}

/// Clears the session's branch summary abort controller however the
/// navigation ends.
struct BranchSummarySlot<'a>(&'a Core);

impl Drop for BranchSummarySlot<'_> {
    fn drop(&mut self) {
        self.0.state().branch_summary_abort = None;
    }
}

/// Pi's branch summary step of `navigateTree`: refuses a navigation to a
/// missing target or without a model, collects the entries the navigation
/// leaves ([`collect_entries_for_branch_summary`]), and summarizes them
/// unless there are none. The request is cancelled by `controller` and by
/// `signal`; a cancel is refused with [`BRANCH_SUMMARY_CANCELLED`].
async fn summarize_branch(
    control: &super::Control,
    target_id: &str,
    options: &TreeNavigationOptions,
    controller: &AbortController,
    signal: Option<&AbortSignal>,
) -> Result<Prepared, String> {
    let core = &control.core;
    let target = target_id.to_owned();
    let read = move |session: &mut SessionManager| {
        let old_leaf = session.leaf_id().map(str::to_owned);
        if old_leaf.as_deref() == Some(target.as_str()) {
            return None;
        }
        let exists = session.entry(&target).is_some();
        let collected = collect_entries_for_branch_summary(session, old_leaf.as_deref(), &target);
        Some((exists, collected.entries))
    };
    let Some((exists, entries)) = core.with_session(read).await? else {
        return Ok(Prepared::AtTarget);
    };
    let Some(model) = core.model() else {
        return Err(NO_SUMMARY_MODEL.to_owned());
    };
    if !exists {
        return Err(format!("Entry {target_id} not found"));
    }
    if entries.is_empty() {
        return Ok(Prepared::Summary(None));
    }
    let cancelled = || controller.signal().aborted() || signal.is_some_and(AbortSignal::aborted);
    // No summary request is sent once cancelled.
    if cancelled() {
        return Err(BRANCH_SUMMARY_CANCELLED.to_owned());
    }
    let (reserve_tokens, retry) = {
        let settings = super::read_settings(&core.settings);
        (
            settings.branch_summary_reserve_tokens,
            settings.retry.policy(),
        )
    };
    let request = GenerateBranchSummaryOptions {
        model,
        stream_fn: core.stream_fn.clone(),
        api_key: None,
        headers: None,
        env: None,
        signal: controller.signal(),
        custom_instructions: options.custom_instructions.clone(),
        replace_instructions: options.replace_instructions,
        reserve_tokens: Some(reserve_tokens),
        retry: Some(retry),
        on_retry: Some(core.summarization_observer(SummarizationSource::BranchSummary)),
    };
    let work = generate_branch_summary(&entries, &request);
    let result = match signal {
        None => work.await,
        Some(signal) => {
            tokio::pin!(work);
            tokio::select! {
                biased;
                result = &mut work => result,
                () = signal.cancelled() => {
                    controller.abort();
                    work.await
                }
            }
        }
    };
    let result = match result {
        Ok(result) if !result.aborted => result,
        // A failure after a cancel is the cancel, as Pi's aborted request is.
        Ok(_) => return Err(BRANCH_SUMMARY_CANCELLED.to_owned()),
        Err(_) if cancelled() => return Err(BRANCH_SUMMARY_CANCELLED.to_owned()),
        Err(error) => return Err(error),
    };
    if let Some(error) = result.error {
        return Err(error);
    }
    let details = serde_json::json!({
        "readFiles": result.read_files.unwrap_or_default(),
        "modifiedFiles": result.modified_files.unwrap_or_default(),
    });
    // Pi writes a summary only when its text is not empty (`if (summaryText)`).
    let text = result.summary.filter(|text| !text.is_empty());
    Ok(Prepared::Summary(text.map(|text| {
        Box::new(BranchSummary {
            text,
            details,
            usage: result.usage,
        })
    })))
}

impl AgentSession {
    /// Pi's `navigateTree`: moves the session's leaf within the same file
    /// and rebuilds the agent's transcript from the new path. A user or
    /// custom message moves the leaf to its parent (before the first entry
    /// at a root) and returns its text for the editor, so it can be edited
    /// and sent again; any other entry becomes the leaf. The current leaf
    /// is a no-op.
    ///
    /// With [`TreeNavigationOptions::summarize`], the entries from the old
    /// leaf back to the common ancestor with the target
    /// ([`collect_entries_for_branch_summary`]) are summarized first
    /// ([`generate_branch_summary`], with `branchSummary.reserveTokens`, the
    /// retry settings, and `summarization_retry_*` events of source
    /// `branchSummary`), and the summary is written as a `branch_summary`
    /// entry at the new position, which becomes the leaf, Pi's
    /// `branchWithSummary`. When there is nothing to summarize no request is
    /// sent and no summary is written. A label is set on the summary entry,
    /// or on the target when there is none (Pi's `appendLabelChange`).
    /// Without a summary or a label nothing is written: the next entry
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
    /// are refused until it ends, as Pi's `isCompacting` counts a branch
    /// summary. A missing target or parent changes nothing; so does a
    /// summary requested without a model, refused with Pi's message.
    ///
    /// `signal` cancels it until the leaf moves: it is checked again under
    /// the session lock, immediately before the move, so a cancel that
    /// arrives while the change waits its turn or for the blocking pool is
    /// refused with [`NAVIGATION_CANCELLED`] and changes nothing. A cancel
    /// during the summary, from `signal`, [`Self::abort_branch_summary`],
    /// [`Self::abort`], or shutdown, aborts the request and is refused with
    /// [`BRANCH_SUMMARY_CANCELLED`], Pi's `aborted` result: nothing is
    /// written and the leaf stays. Once the summary is made, a cancel no
    /// longer stops the navigation, as in Pi: the summary is written and the
    /// leaf moves. A failed summary is refused with its
    /// error and changes nothing. Once the move starts it completes; a
    /// failed write of the summary or label stops the session as every
    /// failed write does.
    pub async fn navigate_tree(
        &self,
        target_id: &str,
        options: TreeNavigationOptions,
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
            // Pi's `_branchSummaryAbortController`, registered before
            // anything is awaited. Shutdown closes the listeners before it
            // reads the slot, so one registered after that starts aborted.
            let controller = AbortController::new();
            control.core.state().branch_summary_abort = Some(controller.clone());
            let _slot = BranchSummarySlot(&control.core);
            if control.core.listeners.closed.load(Ordering::SeqCst) {
                controller.abort();
            }
            control
                .core
                .cancel_background_compaction("navigation")
                .await;
            let summary = if options.summarize {
                match summarize_branch(&control, &target_id, &options, &controller, signal.as_ref())
                    .await?
                {
                    Prepared::AtTarget => return Ok(TreeNavigation::default()),
                    Prepared::Summary(summary) => summary,
                }
            } else {
                None
            };
            let core = Arc::clone(&control.core);
            let label = options.label.clone().filter(|label| !label.is_empty());
            let navigate = move || {
                #[cfg(test)]
                {
                    let hook = core.hooks().before_navigation.take();
                    if let Some(hook) = hook {
                        hook();
                    }
                }
                super::write_session::<_, crate::session::SessionError>(&core.session, &core.gap, |session| {
                    // The commit boundary: a cancel seen here moves nothing,
                    // unless a summary was made, which Pi writes however late
                    // the cancel comes.
                    let cancelled = controller.signal().aborted()
                        || signal.as_ref().is_some_and(AbortSignal::aborted);
                    if cancelled && summary.is_none() {
                        return Ok(Err(NAVIGATION_CANCELLED.to_owned()));
                    }
                    if session.leaf_id() == Some(target_id.as_str()) {
                        return Ok(Ok(TreeNavigation::default()));
                    }
                    let Some(target) = session.entry(&target_id).cloned() else {
                        return Ok(Err(format!("Entry {target_id} not found")));
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
                    // A missing parent is refused before anything is
                    // written, so the write boundary sees only write errors.
                    if let Some(leaf) = &new_leaf
                        && session.entry(leaf).is_none()
                    {
                        return Ok(Err(format!("Entry {leaf} not found")));
                    }
                    // Pi attaches the summary at the navigation target's
                    // position, not on the branch left.
                    let summary_entry = match &summary {
                        Some(summary) => {
                            let id = session.branch_with_summary(
                                new_leaf.as_deref(),
                                &summary.text,
                                Some(summary.details.clone()),
                                Some(false),
                                summary.usage.as_ref(),
                            )?;
                            let entry = session.entry(&id).cloned();
                            if let Some(label) = &label {
                                session.append_label_change(&id, Some(label))?;
                            }
                            entry
                        }
                        None => {
                            match &new_leaf {
                                Some(leaf) => session.branch(leaf)?,
                                None => session.reset_leaf(),
                            }
                            if let Some(label) = &label {
                                session.append_label_change(&target_id, Some(label))?;
                            }
                            None
                        }
                    };
                    // Under the session lock, so no reader sees the new leaf
                    // with the old transcript.
                    core.refresh_context(&session.build_session_projection());
                    Ok(Ok(TreeNavigation {
                        moved: true,
                        editor_text,
                        summary_entry,
                    }))
                })
            };
            match control.blocking(navigate, "Session I/O").await? {
                Ok(navigated) => navigated,
                Err(super::WriteRefused::AfterGap(error)) => Err(format!(
                    "The session file could not be written ({error}), so nothing more is written to it"
                )),
                Err(super::WriteRefused::Failed(error)) => {
                    // As every failed write does: nothing more is written
                    // and later prompts are refused.
                    control.core.abort_requested.store(true, Ordering::SeqCst);
                    control.agent.abort();
                    Err(control.stop_after_failed_write(&error.to_string(), "The navigation"))
                }
            }
        })
        .await
    }

    /// Pi's `abortBranchSummary`: cancels a running branch summary of
    /// [`Self::navigate_tree`], which then writes nothing.
    pub fn abort_branch_summary(&self) {
        if let Some(controller) = &self.core.state().branch_summary_abort {
            controller.abort();
        }
    }

    /// The session's tree and its leaf, Pi's `getTree` and `getLeafId`,
    /// as `/tree` lists them. Reads only.
    pub fn session_tree(&self) -> (SessionTree, Option<String>) {
        let session = lock(&self.session);
        (session.tree(), session.leaf_id().map(str::to_owned))
    }

    /// The session file's entries, in file order, and its leaf, Pi's
    /// `getEntries` and `getLeafId`. Reads only.
    pub fn session_entries(&self) -> (Vec<SessionEntry>, Option<String>) {
        let session = lock(&self.session);
        (
            session.entries().into_iter().cloned().collect(),
            session.leaf_id().map(str::to_owned),
        )
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
