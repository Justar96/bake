//! Group appends: entries that belong together, written in order.
//!
//! Pi appends one line per entry, so a crash can leave any prefix of
//! entries that belong together. Bake keeps Pi's lines (D32) and adds the
//! recoverable group of the durable-execution direction (rule 4, scope 03
//! of the Rust 0.4 roadmap): [`SessionManager::append_group`] checks every
//! entry before it writes any, writes them in order under one hold of the
//! manager, and reports how far a failed write got. A group is never
//! atomic: loading reads every prefix of one, as Pi's loader reads any
//! file, and an owner that sees a failed group writes nothing more.

use std::fmt;
use std::io;
use std::path::PathBuf;

use super::{SessionManager, lines_of};
use crate::session::SessionError;
use crate::session::durable::{self, FailedLine, LinesFailure};
use crate::session::entry::{EditContent, FileEntry};
use crate::session::json::JsonObject;

/// One entry of a group ([`SessionManager::append_group`]): the entry its
/// single append writes, with the same checks.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum GroupEntry {
    /// [`SessionManager::append_model_change`].
    ModelChange {
        /// The provider.
        provider: String,
        /// The model id.
        model_id: String,
    },
    /// [`SessionManager::append_thinking_level_change`].
    ThinkingLevelChange {
        /// The level.
        thinking_level: String,
    },
    /// [`SessionManager::append_context_edit`].
    ContextEdit {
        /// The entry whose model content is edited.
        target_id: String,
        /// The new content; `None` omits the target from context.
        replacement: Option<EditContent>,
    },
}

impl GroupEntry {
    /// A model change.
    pub fn model_change(provider: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self::ModelChange {
            provider: provider.into(),
            model_id: model_id.into(),
        }
    }

    /// A thinking-level change.
    pub fn thinking_level_change(thinking_level: impl Into<String>) -> Self {
        Self::ThinkingLevelChange {
            thinking_level: thinking_level.into(),
        }
    }

    /// A context edit.
    pub fn context_edit(target_id: impl Into<String>, replacement: Option<EditContent>) -> Self {
        Self::ContextEdit {
            target_id: target_id.into(),
            replacement,
        }
    }
}

/// A group write that stopped part-way ([`SessionManager::append_group`]).
#[derive(Debug)]
pub struct GroupWriteError {
    /// The session file.
    pub path: PathBuf,
    /// The ids of the group's entries written and synced, in order: the
    /// known durable prefix, which the manager keeps. The file may show a
    /// reader one entry more ([`Self::failed`]).
    pub written: Vec<String>,
    /// How many entries the group had.
    pub len: usize,
    /// What happened to the entry after `written`. No later entry was
    /// written. When it is [`FailedLine::Unsynced`], or
    /// [`FailedLine::Torn`] with all of its JSON written, a reader of the
    /// file, such as an immediate reopen, may read it as an entry the
    /// manager does not keep, while a crash may still lose it.
    pub failed: FailedLine,
    /// The failure.
    pub source: io::Error,
}

impl fmt::Display for GroupWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match self.failed {
            FailedLine::NotWritten => "was not written",
            FailedLine::Torn => "may be partly written, without its newline or a sync",
            FailedLine::Unsynced => "was written but not synced, so a reader may see it",
        };
        write!(
            formatter,
            "{}: {} ({} of {} grouped entries written; the next {state})",
            self.path.display(),
            self.source,
            self.written.len(),
            self.len,
        )
    }
}

/// Why [`SessionManager::persist_group`] stopped.
enum PersistFailure {
    /// Before any line of the group: the lock, or the file's creation.
    Before(SessionError),
    /// At a line of the group.
    At(PathBuf, LinesFailure),
}

impl SessionManager {
    /// Append `entries` as one group: the first a child of the leaf, each
    /// later one a child of the one before, and the leaf moved to the last.
    /// Returns their ids in order. Each entry is the one its single append
    /// writes ([`GroupEntry`]).
    ///
    /// Every entry is built and checked before anything is written; a
    /// failed check returns its error (such as
    /// [`SessionError::NotOnActiveBranch`]) and changes nothing. The entries
    /// are then written in order, each line written and synced before the
    /// next, while the caller holds the manager, so no other entry of this
    /// manager lands between them, and a caller that reads through the
    /// same manager, or under the same lock as `AgentSession`'s session
    /// lock, sees none of the group before the write ends. A process that
    /// reads the file meanwhile, which the writer lock does not exclude,
    /// sees whatever prefix has reached it. The write is not atomic: a crash leaves any prefix of the
    /// group, possibly ending in a torn line, and loading reads each prefix
    /// as Pi's loader does. As for every append, a session without a file,
    /// or one whose file is not created yet because it holds no
    /// conversation, writes nothing; a session whose file is due creates it
    /// with the entries before the group, then appends the group.
    ///
    /// When writing fails, the manager keeps exactly the entries known to
    /// be written, unlike a single append, which keeps its entry in memory
    /// as Pi does:
    ///
    /// - Any error but [`SessionError::GroupWrite`] (the writer lock is
    ///   held or closed, the file cannot be created) is raised before the
    ///   group's first line: nothing of the group is written or kept.
    /// - [`SessionError::GroupWrite`] names the entries written and synced,
    ///   which are kept with the leaf on the last of them, and says what
    ///   happened to the next one ([`FailedLine`]): not written, perhaps
    ///   torn, or written without a sync, so a reader may see it now while
    ///   a crash may lose it. It and the later entries are not kept. The
    ///   manager then holds the known durable prefix, which is not always
    ///   what an immediate reopen reads: an unsynced line, or a torn line
    ///   whose whole JSON reached the file, reads as one entry more.
    ///
    /// A failed group must not be appended again: the file may hold its
    /// prefix, and a second copy would follow it under new ids. An owner
    /// treats the failure as a gap and writes nothing more, as
    /// [`crate::agent_session::AgentSession`] does.
    pub fn append_group(
        &mut self,
        entries: impl IntoIterator<Item = GroupEntry>,
    ) -> Result<Vec<String>, SessionError> {
        let start = self.file_entries.len();
        let start_ids = self.by_id.len();
        let leaf = self.leaf_id.clone();
        let mut ids = Vec::new();
        for entry in entries {
            match self.group_entry(entry) {
                Ok((id, raw)) => {
                    self.push_entry(id.clone(), raw);
                    ids.push(id);
                }
                Err(error) => {
                    self.keep_group_prefix(start, start_ids, leaf, &ids, 0);
                    return Err(error);
                }
            }
        }
        if ids.is_empty() {
            return Ok(ids);
        }
        match self.persist_group(start) {
            Ok(()) => Ok(ids),
            Err(PersistFailure::Before(error)) => {
                self.keep_group_prefix(start, start_ids, leaf, &ids, 0);
                Err(error)
            }
            Err(PersistFailure::At(path, failure)) => {
                let kept = failure.synced.min(ids.len());
                self.keep_group_prefix(start, start_ids, leaf, &ids, kept);
                let len = ids.len();
                ids.truncate(kept);
                Err(SessionError::GroupWrite(GroupWriteError {
                    path,
                    written: ids,
                    len,
                    failed: failure.line,
                    source: failure.error,
                }))
            }
        }
    }

    /// The entry `entry` stands for, as a child of the leaf, checked.
    fn group_entry(&self, entry: GroupEntry) -> Result<(String, JsonObject), SessionError> {
        match entry {
            GroupEntry::ModelChange { provider, model_id } => {
                Ok(self.model_change(&provider, &model_id))
            }
            GroupEntry::ThinkingLevelChange { thinking_level } => {
                Ok(self.thinking_level_change(&thinking_level))
            }
            GroupEntry::ContextEdit {
                target_id,
                replacement,
            } => self.context_edit(&target_id, replacement),
        }
    }

    /// Keep the first `kept` of the group's entries `ids`, pushed from file
    /// index `start`, and drop the rest from memory. The group's ids are
    /// new, so each push added one id at the end of `by_id`.
    fn keep_group_prefix(
        &mut self,
        start: usize,
        start_ids: usize,
        leaf: Option<String>,
        ids: &[String],
        kept: usize,
    ) {
        self.file_entries.truncate(start + kept);
        self.by_id.truncate(start_ids + kept);
        self.leaf_id = match kept.checked_sub(1).and_then(|last| ids.get(last)) {
            Some(last) => Some(last.clone()),
            None => leaf,
        };
    }

    /// Write the entries from file index `start`, the group, in order.
    fn persist_group(&mut self, start: usize) -> Result<(), PersistFailure> {
        let Some(path) = self.session_file.clone().filter(|_| self.persist) else {
            return Ok(());
        };
        if !self.flushed && !self.has_conversation() {
            return Ok(());
        }
        self.hold_writer(&path).map_err(PersistFailure::Before)?;
        if !self.flushed {
            let before = self.file_entries.get(..start).unwrap_or_default();
            durable::create_new(&path, &lines_of(before))
                .map_err(|error| PersistFailure::Before(SessionError::io(&path, error)))?;
            self.flushed = true;
        }
        let lines: Vec<Vec<u8>> = self
            .file_entries
            .get(start..)
            .unwrap_or_default()
            .iter()
            .map(|entry: &FileEntry| lines_of([entry]))
            .collect();
        durable::append_lines(&path, &lines, &mut self.faults)
            .map_err(|failure| PersistFailure::At(path, failure))
    }

    /// Arm a fault for the next group write.
    #[cfg(test)]
    pub(crate) fn arm_group_fault(&mut self, fault: durable::Fault) {
        self.faults.arm(fault);
    }
}

#[cfg(test)]
#[path = "group_tests.rs"]
mod tests;
