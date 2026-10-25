//! Branch-local plan, todo, and goal snapshots in Pi custom entries (D37).
//!
//! Bake's revision and compare-and-set rules draw on
//! `packages/goal/goal/src/{types,fold}.ts`; these entries do not read that
//! runtime's logs. A document's completion state is a claim. Verification
//! is separate evidence about a revision and a declared set of artifacts.
//! Callers opt in, serialize mutations through the session's write boundary,
//! and stop writing after any persistence failure.

use std::collections::BTreeMap;
use std::fmt;

use bake_ai::UserContent;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::session::{GroupEntry, SessionError, SessionManager};

pub mod artifacts;
pub mod verification;

/// Pi custom-entry type for a complete task document snapshot.
pub const DOCUMENT_TYPE: &str = "bake.task.document.v1";
/// Pi custom-message type exposing one exact revision to the model.
pub const CONTEXT_TYPE: &str = "bake.task.document.context.v1";
/// The largest document body, measured in UTF-8 bytes.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// The largest number of document identities on one branch.
pub const MAX_DOCUMENTS: usize = 64;
/// Aggregate body bound across the latest revisions on one branch.
pub const MAX_TOTAL_BODY_BYTES: usize = 128 * 1024;

/// The purpose of a document; fixed across its revisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    /// An ordered approach to the requested work.
    Plan,
    /// Work items and their stated progress.
    Todo,
    /// A user-requested completion objective.
    Goal,
}

/// Document status. None of these states asserts artifact verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentState {
    /// Work remains eligible to run within its separately admitted budget.
    Active,
    /// Work has been paused.
    Paused,
    /// Work needs an external change or user input.
    Blocked,
    /// The author claims the work is complete.
    ClaimedComplete,
    /// Work is archived; its revisions remain in branch history.
    Archived,
}

/// A full, bounded revision; no patch needs to be reapplied during replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DocumentSnapshot {
    /// Payload version, currently one.
    pub version: u32,
    /// Stable branch-local identity, at most 64 ASCII letters, digits, `_`, or `-`.
    pub id: String,
    /// The document's purpose.
    pub kind: DocumentKind,
    /// Positive, consecutive revision number.
    pub revision: u64,
    /// Entry id of the preceding revision, absent for creation.
    pub previous_entry: Option<String>,
    /// Complete document body.
    pub body: String,
    /// Stated progress, independent of verification evidence.
    pub state: DocumentState,
}

/// Exact identity required when replacing a document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DocumentRef {
    /// Stable document identity.
    pub id: String,
    /// Exact expected revision.
    pub revision: u64,
    /// Exact expected snapshot entry on the selected branch.
    pub entry_id: String,
}

/// A snapshot and its durable identity on the selected branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// The snapshot entry id.
    pub entry_id: String,
    /// Its full state.
    pub snapshot: DocumentSnapshot,
}

impl Document {
    /// The compare-and-set token for this exact revision.
    pub fn reference(&self) -> DocumentRef {
        DocumentRef {
            id: self.snapshot.id.clone(),
            revision: self.snapshot.revision,
            entry_id: self.entry_id.clone(),
        }
    }
}

/// A requested complete replacement. The writer supplies the revision chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DocumentUpdate {
    /// Stable identity, new only when the expected reference is absent.
    pub id: String,
    /// Document purpose; cannot change on an existing identity.
    pub kind: DocumentKind,
    /// Complete replacement body.
    pub body: String,
    /// The author's completion or progress claim.
    pub state: DocumentState,
}

/// A rejected mutation, invalid replay, or failed durable append.
#[derive(Debug)]
pub enum DocumentError {
    /// Malformed or unsupported data; nothing is written.
    Invalid(String),
    /// The expected revision is absent or differs on the selected branch.
    Conflict,
    /// Persistence failed; the owner must stop subsequent writes.
    Persistence(SessionError),
}

impl fmt::Display for DocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "Invalid task document: {message}"),
            Self::Conflict => {
                f.write_str("Task document changed; read its current revision before updating it")
            }
            Self::Persistence(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for DocumentError {}

impl From<SessionError> for DocumentError {
    fn from(error: SessionError) -> Self {
        Self::Persistence(error)
    }
}

fn invalid(message: impl Into<String>) -> DocumentError {
    DocumentError::Invalid(message.into())
}

fn validate(snapshot: &DocumentSnapshot) -> Result<(), DocumentError> {
    if snapshot.version != 1 {
        return Err(invalid("unsupported version"));
    }
    if snapshot.id.is_empty()
        || snapshot.id.len() > 64
        || !snapshot
            .id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err(invalid(
            "id must contain 1–64 ASCII letters, digits, underscores, or hyphens",
        ));
    }
    if snapshot.body.trim().is_empty() || snapshot.body.len() > MAX_BODY_BYTES {
        return Err(invalid(
            "body must contain 1–65536 bytes and some non-whitespace text",
        ));
    }
    // Pi parses numbers as JavaScript doubles; keep revisions exact there too.
    if snapshot.revision == 0 || snapshot.revision > 9_007_199_254_740_991 {
        return Err(invalid(
            "revision is outside the positive safe-integer range",
        ));
    }
    Ok(())
}

fn payload(snapshot: &DocumentSnapshot) -> Result<Value, DocumentError> {
    serde_json::to_value(snapshot).map_err(|error| invalid(error.to_string()))
}

/// Rebuild the latest revisions from the selected branch, including archived
/// documents. Invalid recognized entries fail closed; unrelated custom entries
/// and revisions on other branches have no effect.
pub fn project(session: &SessionManager) -> Result<BTreeMap<String, Document>, DocumentError> {
    let mut documents: BTreeMap<String, Document> = BTreeMap::new();
    let mut total_body_bytes = 0;
    for entry in session.branch_entries(None) {
        if entry.entry_type() != "custom"
            || entry.get("customType").and_then(Value::as_str) != Some(DOCUMENT_TYPE)
        {
            continue;
        }
        let snapshot: DocumentSnapshot =
            serde_json::from_value(entry.get("data").cloned().unwrap_or(Value::Null))
                .map_err(|error| invalid(format!("entry {}: {error}", entry.id())))?;
        validate(&snapshot)?;
        let prior_body_bytes = documents
            .get(&snapshot.id)
            .map_or(0, |old| old.snapshot.body.len());
        total_body_bytes = total_body_bytes - prior_body_bytes + snapshot.body.len();
        if total_body_bytes > MAX_TOTAL_BODY_BYTES {
            return Err(invalid("current document bodies exceed 128 KiB"));
        }
        match documents.get(&snapshot.id) {
            Some(previous) => {
                if snapshot.revision != previous.snapshot.revision + 1
                    || snapshot.previous_entry.as_deref() != Some(previous.entry_id.as_str())
                    || snapshot.kind != previous.snapshot.kind
                {
                    return Err(invalid(format!(
                        "entry {} does not follow its document revision",
                        entry.id()
                    )));
                }
            }
            None => {
                if snapshot.revision != 1 || snapshot.previous_entry.is_some() {
                    return Err(invalid(format!(
                        "entry {} has no creation revision",
                        entry.id()
                    )));
                }
                if documents.len() == MAX_DOCUMENTS {
                    return Err(invalid("too many document identities on this branch"));
                }
            }
        }
        documents.insert(
            snapshot.id.clone(),
            Document {
                entry_id: entry.id().to_owned(),
                snapshot,
            },
        );
    }
    Ok(documents)
}

fn context_entry(snapshot: &DocumentSnapshot) -> Result<GroupEntry, DocumentError> {
    let data = payload(snapshot)?;
    Ok(GroupEntry::CustomMessage {
        custom_type: CONTEXT_TYPE.into(),
        content: UserContent::Text(context_text(&data)?),
        display: false,
        details: Some(data),
    })
}

fn context_text(data: &Value) -> Result<String, DocumentError> {
    let text = serde_json::to_string(data).map_err(|error| invalid(error.to_string()))?;
    Ok(format!(
        "Task document revision (status is a claim; verification is separate):\n{text}"
    ))
}

/// Compare-and-set a complete revision and expose its exact contents to model
/// context. Both entries are synced before acknowledgement, even in a new
/// session. A crash can leave the snapshot alone; [`expose_pending`] repairs
/// that missing exposure without recreating the snapshot or repeating work.
/// Call only between tool turns: a custom user message must not separate an
/// assistant's tool calls from their results.
pub fn revise(
    session: &mut SessionManager,
    expected: Option<&DocumentRef>,
    update: DocumentUpdate,
) -> Result<DocumentRef, DocumentError> {
    revise_inner(session, expected, update, true)
}

/// Tool execution records state alone; the next-turn hook exposes it after
/// every tool result, preserving provider-valid call/result ordering.
pub fn revise_snapshot(
    session: &mut SessionManager,
    expected: Option<&DocumentRef>,
    update: DocumentUpdate,
) -> Result<DocumentRef, DocumentError> {
    revise_inner(session, expected, update, false)
}

fn revise_inner(
    session: &mut SessionManager,
    expected: Option<&DocumentRef>,
    update: DocumentUpdate,
    expose: bool,
) -> Result<DocumentRef, DocumentError> {
    let documents = project(session)?;
    let previous = documents.get(&update.id);
    if previous.map(Document::reference).as_ref() != expected {
        return Err(DocumentError::Conflict);
    }
    if previous.is_some_and(|old| old.snapshot.kind != update.kind) {
        return Err(invalid("kind cannot change across revisions"));
    }
    if previous.is_none() && documents.len() >= MAX_DOCUMENTS {
        return Err(invalid("too many document identities on this branch"));
    }
    let total_body_bytes: usize = documents
        .values()
        .map(|document| document.snapshot.body.len())
        .sum();
    if total_body_bytes - previous.map_or(0, |old| old.snapshot.body.len()) + update.body.len()
        > MAX_TOTAL_BODY_BYTES
    {
        return Err(invalid("current document bodies exceed 128 KiB"));
    }
    let snapshot = DocumentSnapshot {
        version: 1,
        id: update.id,
        kind: update.kind,
        revision: previous.map_or(1, |old| old.snapshot.revision + 1),
        previous_entry: previous.map(|old| old.entry_id.clone()),
        body: update.body,
        state: update.state,
    };
    validate(&snapshot)?;
    let mut group = vec![GroupEntry::Custom {
        custom_type: DOCUMENT_TYPE.into(),
        data: Some(payload(&snapshot)?),
    }];
    if expose {
        group.push(context_entry(&snapshot)?);
    }
    let ids = session.append_durable_group(group)?;
    let Some(entry_id) = ids.first() else {
        return Err(invalid("snapshot append returned no entry"));
    };
    Ok(DocumentRef {
        id: snapshot.id,
        revision: snapshot.revision,
        entry_id: entry_id.clone(),
    })
}

/// Sync missing context messages for current revisions. The owner rebuilds its
/// live transcript afterward, so the provider receives the logged exposure.
/// Repeating this operation after a successful append writes nothing.
pub fn expose_pending(session: &mut SessionManager) -> Result<usize, DocumentError> {
    let documents = project(session)?;
    let projection = session.build_session_projection();
    let mut pending = Vec::new();
    for document in documents.values() {
        let data = payload(&document.snapshot)?;
        let content = Value::String(context_text(&data)?);
        let exposed = projection.messages.iter().any(|message| {
            let raw = message.as_json();
            raw.get("role").and_then(Value::as_str) == Some("custom")
                && raw.get("customType").and_then(Value::as_str) == Some(CONTEXT_TYPE)
                && raw.get("details") == Some(&data)
                && raw.get("content") == Some(&content)
        });
        if !exposed {
            pending.push(context_entry(&document.snapshot)?);
        }
    }
    let count = pending.len();
    session.append_durable_group(pending)?;
    Ok(count)
}

#[cfg(test)]
mod tests;
