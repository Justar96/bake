//! Evidence for an admitted native bash check and its declared input versions.
//!
//! The trusted session owner prepares a ticket before dispatching the check,
//! using the command actually admitted by the native executor. After awaiting
//! that executor and persisting its call and result, it captures the same inputs
//! again and consumes the ticket. This module neither executes commands nor
//! grants permission. A model claim or an extension-modified `isError` flag does
//! not establish actual process success: the owner must attest native success
//! before calling [`record`]. Imported session metadata is not authenticated.
//!
//! Equal captures describe declared file contents at two reads. They do not
//! prove continuous stability during the check, cover undeclared inputs, or
//! establish that the user's goal is complete. Compaction and context edits do
//! not erase historical evidence; selecting a branch without it does.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::artifacts::ArtifactSet;
use super::{DOCUMENT_TYPE, DocumentError, DocumentRef, DocumentSnapshot, invalid};
use crate::session::{GroupEntry, SessionEntry, SessionManager};

/// Pi custom-entry type for evidence about one exact document revision.
pub const EVIDENCE_TYPE: &str = "bake.task.verification.v1";
/// Maximum UTF-8 command bytes retained in an evidence record.
pub const MAX_COMMAND_BYTES: usize = 16 * 1024;

/// Command identity supplied by the trusted native-check admission boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CheckSpec {
    /// Exact admitted bash command, including whitespace.
    pub command: String,
}

impl CheckSpec {
    fn validate(&self) -> Result<(), DocumentError> {
        if self.command.trim().is_empty()
            || self.command.len() > MAX_COMMAND_BYTES
            || self.command.contains('\0')
        {
            return Err(invalid(
                "check command must contain 1–16384 bytes without NUL",
            ));
        }
        Ok(())
    }
}

/// Owned admission state, deliberately neither deserializable nor clonable.
/// Preparing a ticket does not itself admit or execute a command.
#[derive(Debug)]
pub struct VerificationTicket {
    session_id: String,
    document: DocumentRef,
    admission_entry: String,
    check: CheckSpec,
    before: ArtifactSet,
}

impl VerificationTicket {
    /// Bind a validated capture and command to the current document and branch.
    /// The owner must retain this ticket through native execution and cleanup.
    pub fn prepare(
        session: &SessionManager,
        document: &DocumentRef,
        check: CheckSpec,
        before: &ArtifactSet,
    ) -> Result<Self, DocumentError> {
        check.validate()?;
        validate_artifacts(before)?;
        require_current(session, document)?;
        // Refuse to extend a branch whose recognized evidence is invalid.
        project(session)?;
        // Pi forks recreate label IDs. Anchor to the preceding retained entry
        // so copying history cannot detach otherwise valid check evidence.
        let admission_entry = session
            .branch_entries(None)
            .into_iter()
            .rev()
            .find(|entry| entry.entry_type() != "label")
            .ok_or(DocumentError::Conflict)?
            .id()
            .to_owned();
        Ok(Self {
            session_id: session.session_id().into(),
            document: document.clone(),
            admission_entry,
            check,
            before: before.clone(),
        })
    }
}

/// Versioned evidence payload stored in a Pi custom entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EvidenceSnapshot {
    /// Payload version, currently one.
    pub version: u32,
    /// Exact document revision to which the check was admitted.
    pub document: DocumentRef,
    /// Last selected non-label entry when the host prepared the check.
    pub admission_entry: String,
    /// Persisted assistant message containing the unique bash call.
    pub assistant_entry: String,
    /// Persisted successful tool result for that call.
    pub result_entry: String,
    /// Unique tool call identity on this branch.
    pub call_id: String,
    /// Exact native command admitted by the host.
    pub check: CheckSpec,
    /// Equal input versions captured before and after the check.
    pub artifacts: ArtifactSet,
}

/// Evidence and its selected-branch entry identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    /// Entry storing the evidence payload.
    pub entry_id: String,
    /// The bounded payload and exact provenance references.
    pub snapshot: EvidenceSnapshot,
}

/// Freshness of declared-check evidence, independent of document progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceStatus {
    /// The current document and declared content versions match the evidence.
    Fresh,
    /// The document or declared content versions differ from the evidence.
    Stale,
    /// No evidence exists, or current declared inputs could not be captured.
    Unavailable,
}

fn validate_artifacts(artifacts: &ArtifactSet) -> Result<(), DocumentError> {
    artifacts
        .validate()
        .map_err(|error| invalid(format!("check artifacts: {error}")))
}

fn require_current(session: &SessionManager, expected: &DocumentRef) -> Result<(), DocumentError> {
    if super::project(session)?
        .get(&expected.id)
        .map(super::Document::reference)
        .as_ref()
        != Some(expected)
    {
        return Err(DocumentError::Conflict);
    }
    Ok(())
}

/// Consume a ticket after the trusted owner has observed native success,
/// persisted the call/result, and captured the same declared inputs again.
/// Changed documents, inputs, branches, or ambiguous tool identities reject
/// the record without appending. The single custom entry is synced before
/// acknowledgement; any persistence error requires the owner to stop writes.
pub fn record(
    session: &mut SessionManager,
    ticket: VerificationTicket,
    result_entry: &str,
    after: &ArtifactSet,
) -> Result<Evidence, DocumentError> {
    validate_artifacts(after)?;
    if session.session_id() != ticket.session_id || ticket.before != *after {
        return Err(DocumentError::Conflict);
    }
    require_current(session, &ticket.document)?;
    project(session)?;
    let entries = session.branch_entries(None);
    let index = BranchIndex::new(&entries);
    let (_, result) = index.entry(result_entry)?;
    let call_id = result
        .get("message")
        .and_then(|message| message.get("toolCallId"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("check result has no tool call identity"))?;
    let (call_index, _) = index.call(call_id)?;
    let snapshot = EvidenceSnapshot {
        version: 1,
        document: ticket.document,
        admission_entry: ticket.admission_entry,
        assistant_entry: entries[call_index].id().into(),
        result_entry: result_entry.into(),
        call_id: call_id.into(),
        check: ticket.check,
        artifacts: ticket.before,
    };
    validate_snapshot(&snapshot)?;
    index.validate_provenance(&snapshot, entries.len())?;
    let data = serde_json::to_value(&snapshot).map_err(|error| invalid(error.to_string()))?;
    let ids = session.append_durable_group(vec![GroupEntry::Custom {
        custom_type: EVIDENCE_TYPE.into(),
        data: Some(data),
    }])?;
    let entry_id = ids
        .into_iter()
        .next()
        .ok_or_else(|| invalid("evidence append returned no entry identity"))?;
    Ok(Evidence { entry_id, snapshot })
}

/// Rebuild the latest evidence for each document on the selected branch.
/// Every recognized record must have valid bounded metadata and an exact,
/// unambiguous preceding document, admission, call, and successful result.
/// Later document revisions retain historical evidence but make it stale.
pub fn project(session: &SessionManager) -> Result<BTreeMap<String, Evidence>, DocumentError> {
    super::project(session)?;
    let entries = session.branch_entries(None);
    let index = BranchIndex::new(&entries);
    let mut documents = BTreeMap::new();
    let mut evidence = BTreeMap::new();
    for (position, entry) in entries.iter().enumerate() {
        if entry.entry_type() != "custom" {
            continue;
        }
        match entry.get("customType").and_then(Value::as_str) {
            Some(DOCUMENT_TYPE) => {
                let snapshot: DocumentSnapshot = decode(entry)?;
                documents.insert(
                    snapshot.id.clone(),
                    DocumentRef {
                        id: snapshot.id,
                        revision: snapshot.revision,
                        entry_id: entry.id().into(),
                    },
                );
            }
            Some(EVIDENCE_TYPE) => {
                let snapshot: EvidenceSnapshot = decode(entry)?;
                validate_snapshot(&snapshot)?;
                if documents.get(&snapshot.document.id) != Some(&snapshot.document) {
                    return Err(invalid(
                        "evidence does not follow its current document revision",
                    ));
                }
                index.validate_provenance(&snapshot, position)?;
                evidence.insert(
                    snapshot.document.id.clone(),
                    Evidence {
                        entry_id: entry.id().into(),
                        snapshot,
                    },
                );
            }
            _ => {}
        }
    }
    Ok(evidence)
}

/// Assess current evidence using a fresh capture supplied by the owner.
/// Pass `None` for missing or unreadable declared inputs. An invalid capture or
/// recognized log record is an error, never fresh evidence.
pub fn status(
    session: &SessionManager,
    document: &DocumentRef,
    current_artifacts: Option<&ArtifactSet>,
) -> Result<EvidenceStatus, DocumentError> {
    let evidence = project(session)?;
    let current = super::project(session)?;
    if current
        .get(&document.id)
        .map(super::Document::reference)
        .as_ref()
        != Some(document)
    {
        return Ok(EvidenceStatus::Stale);
    }
    let Some(evidence) = evidence.get(&document.id) else {
        return Ok(EvidenceStatus::Unavailable);
    };
    if evidence.snapshot.document != *document {
        return Ok(EvidenceStatus::Stale);
    }
    let Some(current_artifacts) = current_artifacts else {
        return Ok(EvidenceStatus::Unavailable);
    };
    validate_artifacts(current_artifacts)?;
    Ok(if evidence.snapshot.artifacts == *current_artifacts {
        EvidenceStatus::Fresh
    } else {
        EvidenceStatus::Stale
    })
}

fn decode<T: serde::de::DeserializeOwned>(entry: &SessionEntry) -> Result<T, DocumentError> {
    serde_json::from_value(entry.get("data").cloned().unwrap_or(Value::Null))
        .map_err(|error| invalid(format!("entry {}: {error}", entry.id())))
}

fn validate_snapshot(snapshot: &EvidenceSnapshot) -> Result<(), DocumentError> {
    if snapshot.version != 1 {
        return Err(invalid("unsupported verification evidence version"));
    }
    for id in [
        &snapshot.document.entry_id,
        &snapshot.admission_entry,
        &snapshot.assistant_entry,
        &snapshot.result_entry,
        &snapshot.call_id,
    ] {
        if id.is_empty() || id.len() > 256 || id.contains('\0') {
            return Err(invalid(
                "evidence identities must contain 1–256 bytes without NUL",
            ));
        }
    }
    snapshot.check.validate()?;
    validate_artifacts(&snapshot.artifacts)
}

// None marks duplicate identities: provenance must never choose one silently.
struct BranchIndex<'a> {
    entries: BTreeMap<&'a str, Option<(usize, &'a SessionEntry)>>,
    calls: BTreeMap<&'a str, Option<(usize, &'a Value)>>,
    results: BTreeMap<&'a str, Option<(usize, &'a SessionEntry)>>,
}

impl<'a> BranchIndex<'a> {
    fn new(entries: &[&'a SessionEntry]) -> Self {
        let mut index = Self {
            entries: BTreeMap::new(),
            calls: BTreeMap::new(),
            results: BTreeMap::new(),
        };
        for (position, &entry) in entries.iter().enumerate() {
            index
                .entries
                .entry(entry.id())
                .and_modify(|old| *old = None)
                .or_insert(Some((position, entry)));
            if entry.entry_type() != "message" {
                continue;
            }
            let Some(message) = entry.get("message") else {
                continue;
            };
            match message.get("role").and_then(Value::as_str) {
                Some("assistant") => {
                    if let Some(content) = message.get("content").and_then(Value::as_array) {
                        for call in content {
                            if call.get("type").and_then(Value::as_str) != Some("toolCall") {
                                continue;
                            }
                            if let Some(id) = call.get("id").and_then(Value::as_str) {
                                index
                                    .calls
                                    .entry(id)
                                    .and_modify(|old| *old = None)
                                    .or_insert(Some((position, call)));
                            }
                        }
                    }
                }
                Some("toolResult") => {
                    if let Some(id) = message.get("toolCallId").and_then(Value::as_str) {
                        index
                            .results
                            .entry(id)
                            .and_modify(|old| *old = None)
                            .or_insert(Some((position, entry)));
                    }
                }
                _ => {}
            }
        }
        index
    }

    fn entry(&self, id: &str) -> Result<(usize, &'a SessionEntry), DocumentError> {
        self.entries
            .get(id)
            .copied()
            .flatten()
            .ok_or_else(|| invalid("evidence entry is absent or ambiguous on this branch"))
    }

    fn call(&self, id: &str) -> Result<(usize, &'a Value), DocumentError> {
        self.calls
            .get(id)
            .copied()
            .flatten()
            .ok_or_else(|| invalid("check tool call is absent or ambiguous on this branch"))
    }

    fn validate_provenance(
        &self,
        snapshot: &EvidenceSnapshot,
        before: usize,
    ) -> Result<(), DocumentError> {
        let (document, _) = self.entry(&snapshot.document.entry_id)?;
        let (admission, admission_entry) = self.entry(&snapshot.admission_entry)?;
        let (assistant, _) = self.entry(&snapshot.assistant_entry)?;
        let (result, result_entry) = self.entry(&snapshot.result_entry)?;
        let (call_position, call) = self.call(&snapshot.call_id)?;
        let unique_result = self
            .results
            .get(snapshot.call_id.as_str())
            .copied()
            .flatten();
        if !(document <= admission
            && admission < assistant
            && assistant < result
            && result < before)
            || admission_entry.entry_type() == "label"
            || call_position != assistant
            || unique_result.map(|(position, _)| position) != Some(result)
            || call.get("name").and_then(Value::as_str) != Some("bash")
            || call
                .get("arguments")
                .and_then(|args| args.get("command"))
                .and_then(Value::as_str)
                != Some(snapshot.check.command.as_str())
        {
            return Err(invalid(
                "check provenance does not match its admitted command and order",
            ));
        }
        let message = result_entry
            .get("message")
            .ok_or_else(|| invalid("check result is not a message"))?;
        if message.get("toolName").and_then(Value::as_str) != Some("bash")
            || message.get("isError").and_then(Value::as_bool) != Some(false)
        {
            return Err(invalid(
                "check requires an explicitly successful bash result",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "verification_tests.rs"]
mod tests;
