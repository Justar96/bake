//! Pi's session store: conversations as append-only trees in JSON Lines.
//!
//! Ported from Pi `packages/coding-agent/src/core/session-manager.ts`, with
//! what it uses from `session-cwd.ts`, `messages.ts`, `utils/paths.ts`, and
//! `packages/ai/src/utils/uuid.ts`, at release v1.1.0 (revision
//! `abe508e1b89912adde45528136c3221eb69acdd7`, MIT; see the crate's
//! `NOTICE`).
//!
//! | Module | Pi source |
//! |---|---|
//! | [`manager`] | `core/session-manager.ts` (`SessionManager`) |
//! | [`context`] | `core/session-manager.ts` (`buildSessionPath` to `buildSessionContext`) |
//! | [`entry`] | `core/session-manager.ts` (entry and header types) |
//! | [`file`](mod@file) | `core/session-manager.ts` (parsing, loading, migration, discovery) |
//! | [`list`] | `core/session-manager.ts` (`buildSessionInfo`, `list`, `listAll`) |
//! | [`search`] | `modes/interactive/components/session-selector-search.ts` (v1.2.0) |
//! | [`messages`] | `core/messages.ts` |
//! | [`cwd`] | `core/session-cwd.ts` |
//! | [`paths`] | `utils/paths.ts` (`normalizePath`, `resolvePath`) |
//! | [`id`] | `core/session-manager.ts` (ids), `ai/src/utils/uuid.ts` |
//! | [`json`] | `JSON.stringify` and JavaScript value semantics |
//! | [`json_line`] | `JSON.parse` of one line |
//! | [`time`] | `Date.prototype.toISOString` and `Date.parse` |
//!
//! # Format
//!
//! Files are byte-compatible with Pi v1.1.0: the same header (`type`,
//! `version` 3, `id`, `timestamp`, `cwd`, `parentSession`), entry kinds,
//! eight-hex-digit entry ids with `parentId` links, one `JSON.stringify` line
//! per entry ending in `\n`, `<ISO timestamp with : and . as ->_<id>.jsonl`
//! file names, and `--<encoded cwd>--` directories. Pi's own older versions
//! (1 and 2) are migrated on open, and the file is rewritten, as Pi does. Bake
//! Session logs of any format are not read (D32).
//!
//! # Deviations from Pi
//!
//! - **Home.** Default session directories live under
//!   `$BAKE_HOME/sessions` ([`crate::home`]) instead of
//!   `~/.pi/agent/sessions`; an explicit session directory is used as given.
//! - **Name.** The invalid-file error says `bake session` where Pi says `pi
//!   session`.
//! - **Listing.** [`SessionManager::list`] and [`SessionManager::list_all`]
//!   are synchronous and load one file at a time ([`list`]).
//! - **Trees.** [`SessionManager::tree`] returns a flat arena of nodes with
//!   child indexes instead of nested nodes, so deep sessions need no
//!   recursion to build, compare, or drop. Children are ordered by a stable
//!   merge sort on their timestamps, and an unparseable timestamp compares
//!   equal, as JavaScript's `sort` treats a `NaN` comparison.
//! - **Values Rust holds differently.** Every line `JSON.parse` reads is
//!   read, but in memory a value nested deeper than
//!   [`json_line::MAX_HELD_DEPTH`] is `null`, a lone UTF-16 surrogate is
//!   U+FFFD, and a number beyond the double range is `null`; rewrites,
//!   forks, and branched sessions still write the bytes Pi writes
//!   ([`json_line`]).
//! - **Malformed input never panics or hangs**, where Pi may throw or loop:
//!   see [`context`] and [`file`](mod@file) for each case. Parent cycles stop at the
//!   first repeated entry.
//! - **Results.** Operations that write return [`SessionError`] where Pi
//!   throws. As in Pi, an append that fails to write keeps its entry in
//!   memory and advances the leaf.
//! - **Paths.** `file://` URLs are not accepted ([`paths`]); non-UTF-8 paths
//!   are stored lossily in headers, which are JSON strings.
//!
//! # Durability and concurrency
//!
//! Pi writes without `fsync` and truncates a file to rewrite it, and two Pi
//! managers appending to one file interleave their lines. Bake keeps Pi's
//! file contents and adds both guarantees (the `durable` module has the
//! details):
//!
//! - **One writer per file.** A persisted manager takes the file's writer
//!   lock before it loads an existing session file and before a new
//!   session's first write, and holds it until it moves to another file, is
//!   dropped, or [`SessionManager::close_writer`] is called. The lock is an
//!   exclusive `File::try_lock` on a sidecar beside the session file, named
//!   by appending `.lock` to its name and created only for a session being
//!   written: a second writer, in this process or another, fails with
//!   [`SessionError::Locked`] after waiting up to a second for the lock, and
//!   takes over once the holder releases the lock, exits, or is killed. The
//!   sidecar is left in place and is never listed. Where the sidecar cannot
//!   be created, an existing one is locked read-only, or on Unix the session
//!   file itself is locked and rewritten in place. On NFS and CIFS, two
//!   writers in one process are not excluded. Readers (listing, discovery,
//!   and [`load_entries_from_file`]) need no lock and never modify a file.
//! - **Durable writes.** Each append is synced, and a rewrite (migration, an
//!   empty file's header, a branched session) goes through a synced
//!   temporary file beside the real file, with its permissions, renamed
//!   over it, so a crash never leaves a truncated or half-rewritten file. A
//!   symbolic link is followed to the file it names; a file with other hard
//!   links is rewritten in place, as Pi does, rather than split from them.
//!   Directory entries are synced on Unix, best effort.
//! - **Torn tails.** A writer killed mid-append leaves a partial last line,
//!   which loading skips as Pi does. The next append starts on a new line,
//!   so the torn fragment cannot join the new entry.

pub mod context;
pub mod cwd;
mod durable;
pub mod entry;
pub mod file;
pub mod id;
pub mod json;
pub mod json_line;
pub mod list;
pub mod manager;
pub mod messages;
pub mod paths;
pub mod search;
pub mod time;

use std::fmt;
use std::io;
use std::path::PathBuf;

pub use context::{
    LeafSelector, ModelRef, ProjectedSessionEntry, SessionContext, SessionProjection,
    build_context_entries, build_session_context, build_session_projection,
    get_latest_compaction_entry, session_entry_to_context_messages,
};
pub use entry::{
    CURRENT_SESSION_VERSION, EditContent, FileEntry, SessionEntry, SessionHeader, TypedEntry,
};
pub use file::{
    find_most_recent_session, load_entries_from_file, migrate_session_entries,
    parse_session_entries,
};
pub use list::{ListProgress, SessionInfo};
pub use manager::{NewSessionOptions, SessionManager, SessionTree, SessionTreeNode};
pub use messages::{AgentMessage, OpaqueMessage};

/// A session operation's failure.
#[derive(Debug)]
pub enum SessionError {
    /// A filesystem operation failed.
    Io {
        /// The path involved.
        path: PathBuf,
        /// The failure.
        source: io::Error,
    },
    /// A non-empty file whose first parsed line is not a session header.
    NotASession(PathBuf),
    /// A session id outside Pi's allowed form.
    InvalidSessionId,
    /// No entry has this id.
    EntryNotFound(String),
    /// A context edit names an entry off the active branch.
    NotOnActiveBranch(String),
    /// A context edit names an entry without editable model content.
    NotEditable(String),
    /// A fork source with no entries or no header.
    InvalidForkSource(PathBuf),
    /// A listing was cancelled.
    Aborted,
    /// [`SessionManager::append_message`] was given a summary message, which
    /// Pi's types exclude: summaries are entries of their own.
    SummaryMessage(String),
    /// Another writer holds this session file's writer lock.
    Locked(PathBuf),
    /// A write after [`SessionManager::close_writer`].
    WriterClosed(PathBuf),
    /// No session directory was given and the Bake home is unknown: no
    /// `BAKE_HOME` or `DSH_HOME` is set and the user's home directory cannot
    /// be found.
    NoHome,
}

impl SessionError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::NotASession(path) => write!(
                formatter,
                "Session file is not a valid bake session: {}",
                path.display()
            ),
            Self::InvalidSessionId => formatter.write_str(
                "Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character",
            ),
            Self::EntryNotFound(id) => write!(formatter, "Entry {id} not found"),
            Self::NotOnActiveBranch(id) => {
                write!(formatter, "Entry {id} is not on the active branch")
            }
            Self::NotEditable(id) => write!(
                formatter,
                "Entry {id} does not contribute editable model content"
            ),
            Self::InvalidForkSource(path) => write!(
                formatter,
                "Cannot fork: source session file is empty or invalid: {}",
                path.display()
            ),
            Self::Aborted => formatter.write_str("This operation was aborted"),
            Self::SummaryMessage(role) => write!(
                formatter,
                "A {role} message is not appended as a message; append a compaction or branch summary entry"
            ),
            Self::Locked(path) => write!(
                formatter,
                "Session file is in use by another writer: {}",
                path.display()
            ),
            Self::WriterClosed(path) => write!(
                formatter,
                "Session file is closed for writing: {}",
                path.display()
            ),
            Self::NoHome => formatter.write_str(
                "Cannot find the Bake home: set BAKE_HOME or give a session directory",
            ),
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
