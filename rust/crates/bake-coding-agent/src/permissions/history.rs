//! A session's approval history, stored in its Pi session file.
//!
//! Bake's TypeScript approval service appended two log-only audit events
//! around every request (`ApprovalService.request` in
//! `packages/interaction/user-approval/src/index.ts`, lines 208-227, with
//! the payloads of `types.ts`, lines 13-60): `approval/asked` with a fresh
//! request id, the tool, the tool call, and the asker's reason, before the
//! answerers were asked, and exactly one `approval/decided` with that id
//! and the outcome. A failure to append either rejected the request. Here
//! each event is one Pi `custom` entry of type [`APPROVAL_CUSTOM_TYPE`]
//! whose data holds `version` ([`APPROVAL_ENTRY_VERSION`]), `kind`
//! (`asked` or `decided`), and the event's fields under their TypeScript
//! names. An `asked` entry also names the sandbox mode the request would
//! grant (`sandbox`), which the TypeScript runtime kept only inside the
//! reason's text. The call's arguments are not copied: `callId` names the
//! call in the assistant message before the entry. Pi's `custom` entries
//! never reach the model, as the TypeScript events did not, so recording a
//! request changes no request the model sees.
//!
//! The outcome is the TypeScript vocabulary, `allowed-once`, `rejected`,
//! `cancelled`, or `unavailable`, and `allowed-once` is the only grant: it
//! is for the one call its request names, so its scope is that call. The
//! TypeScript runtime had no answer wider than one call, and the Rust agent
//! has none either; a later call, a resumed session, or a replayed call
//! asks again. Nothing reads this history to decide a call: the engine
//! ([`super::engine::PermissionEngine`]) records into it and never consults
//! it, and the permission fold ([`super::state`]) ignores these entries, so
//! a recorded `allowed-once` grants nothing after a reopen.
//!
//! An engine records through its [`RecorderSlot`] once a session that
//! persists binds one
//! ([`crate::agent_session::AgentSession::record_approvals`]): `asked` is
//! written and synced before the approver is asked, and `decided` before an
//! `allowed-once` answer lets the call run. A failed write refuses the call
//! before any effect and stops the session. [`ApprovalEntry::admitted`]
//! states what an abort that lands before a write's admission does to it.
//!
//! [`approval_history`] is the projection: a pure fold of the entries into
//! one [`ApprovalRecord`] per request. Unlike the permission fold, which
//! refuses a damaged entry because it could read as a wider mode, this fold
//! skips an entry it cannot read, of an unknown version, or out of order,
//! and reports it in [`ApprovalHistory::skipped`]: the history is an audit
//! trail that grants nothing, so a damaged entry cannot widen anything, and
//! one bad line should not hide the rest.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, OnceLock};

use bake_agent::BoxFuture;
use bake_ai::AbortSignal;
use bake_sandbox::{EscalationOutcome, SandboxMode};
use serde_json::{Map, Value};

use crate::session::{SessionEntry, SessionManager};

/// The `customType` of an approval entry.
pub const APPROVAL_CUSTOM_TYPE: &str = "bake.approval";

/// The `version` of the approval entries this build writes and reads.
pub const APPROVAL_ENTRY_VERSION: u64 = 1;

/// Every outcome, in the TypeScript `ApprovalOutcome` order.
const OUTCOMES: [EscalationOutcome; 4] = [
    EscalationOutcome::AllowedOnce,
    EscalationOutcome::Rejected,
    EscalationOutcome::Cancelled,
    EscalationOutcome::Unavailable,
];

/// A request put to the approver (`approval/asked`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalAsked {
    /// The request's id, fresh for each request; its `decided` entry
    /// carries the same one.
    pub id: String,
    /// The tool the request is about.
    pub tool_name: String,
    /// The exact tool call being decided, when the asker had one.
    pub call_id: Option<String>,
    /// The asker's reason, such as `escalate sandbox to …: <justification>`.
    pub reason: Option<String>,
    /// The sandbox mode an `allowed-once` answer runs the call under: the
    /// escalation being asked.
    pub sandbox: Option<SandboxMode>,
}

/// One approval entry's data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalEntry {
    /// A request, written before the approver is asked.
    Asked(ApprovalAsked),
    /// Its answer, written before an `allowed-once` answer takes effect.
    Decided {
        /// The request's id.
        id: String,
        /// The outcome.
        outcome: EscalationOutcome,
    },
}

impl ApprovalEntry {
    /// The request id the entry belongs to.
    pub fn id(&self) -> &str {
        match self {
            Self::Asked(asked) => &asked.id,
            Self::Decided { id, .. } => id,
        }
    }

    /// The entry's `kind`: `asked` or `decided`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Asked(_) => "asked",
            Self::Decided { .. } => "decided",
        }
    }

    /// The entry to write at a write's admission, under the session lock,
    /// when the call's signal has `aborted` by then or not. Cancellation is
    /// linearized there: aborted before its `asked` entry is admitted, a
    /// request is not asked and nothing is written; aborted before its
    /// `decided` entry is admitted, the request is answered `cancelled`
    /// whatever the approver said, so the recorded outcome is always the one
    /// that takes effect.
    pub fn admitted(self, aborted: bool) -> Option<Self> {
        match self {
            Self::Asked(_) if aborted => None,
            Self::Decided { id, .. } if aborted => Some(Self::Decided {
                id,
                outcome: EscalationOutcome::Cancelled,
            }),
            entry => Some(entry),
        }
    }

    /// The entry's `data`, members in a fixed order; an absent optional
    /// field is omitted, as the TypeScript event omitted it.
    pub fn to_data(&self) -> Value {
        let mut data = Map::new();
        data.insert("version".into(), APPROVAL_ENTRY_VERSION.into());
        data.insert("kind".into(), self.kind().into());
        data.insert("id".into(), self.id().into());
        match self {
            Self::Asked(asked) => {
                data.insert("toolName".into(), asked.tool_name.as_str().into());
                if let Some(call_id) = &asked.call_id {
                    data.insert("callId".into(), call_id.as_str().into());
                }
                if let Some(reason) = &asked.reason {
                    data.insert("reason".into(), reason.as_str().into());
                }
                if let Some(sandbox) = asked.sandbox {
                    data.insert("sandbox".into(), sandbox.as_str().into());
                }
            }
            Self::Decided { outcome, .. } => {
                data.insert("outcome".into(), outcome.as_str().into());
            }
        }
        Value::Object(data)
    }

    /// Reads an entry's `data`. Members this version does not know are
    /// ignored, and `null` reads as absent; a missing or mistyped field, an
    /// empty `id` or `toolName`, an unknown `kind`, mode, or outcome, or a
    /// `version` other than [`APPROVAL_ENTRY_VERSION`] is an error naming
    /// the problem.
    pub fn from_data(data: Option<&Value>) -> Result<Self, String> {
        let Some(Value::Object(data)) = data else {
            return Err("has no data object".to_owned());
        };
        match data.get("version") {
            Some(Value::Number(version)) if version.as_u64() == Some(APPROVAL_ENTRY_VERSION) => {}
            Some(version) => return Err(format!("has unknown version {version}")),
            None => return Err("has no version".to_owned()),
        }
        let text = |name: &str| match data.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.as_str())),
            Some(_) => Err(format!("has a non-string {name}")),
        };
        let required = |name: &str| text(name)?.ok_or_else(|| format!("has no {name}"));
        let id = required("id")?;
        if id.is_empty() {
            return Err("has an empty id".to_owned());
        }
        match required("kind")? {
            "asked" => {
                let tool_name = required("toolName")?;
                if tool_name.is_empty() {
                    return Err("has an empty toolName".to_owned());
                }
                let sandbox = text("sandbox")?
                    .map(|mode| {
                        mode.parse::<SandboxMode>()
                            .map_err(|_| format!("names unknown sandbox mode \"{mode}\""))
                    })
                    .transpose()?;
                Ok(Self::Asked(ApprovalAsked {
                    id: id.to_owned(),
                    tool_name: tool_name.to_owned(),
                    call_id: text("callId")?.map(str::to_owned),
                    reason: text("reason")?.map(str::to_owned),
                    sandbox,
                }))
            }
            "decided" => {
                let outcome = required("outcome")?;
                let outcome = OUTCOMES
                    .into_iter()
                    .find(|known| known.as_str() == outcome)
                    .ok_or_else(|| format!("names unknown outcome \"{outcome}\""))?;
                Ok(Self::Decided {
                    id: id.to_owned(),
                    outcome,
                })
            }
            kind => Err(format!("has unknown kind \"{kind}\"")),
        }
    }
}

/// One request and its answer, folded from the session file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRecord {
    /// The request.
    pub asked: ApprovalAsked,
    /// The id of its `asked` entry.
    pub asked_entry_id: String,
    /// Its answer; `None` when none was recorded, because the writer stopped
    /// or was killed between the two writes. Such a request granted nothing.
    pub outcome: Option<EscalationOutcome>,
    /// The id of its `decided` entry.
    pub decided_entry_id: Option<String>,
}

/// An approval entry the fold could not use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedApprovalEntry {
    /// The entry's id.
    pub entry_id: String,
    /// Why it was skipped.
    pub reason: String,
}

impl fmt::Display for SkippedApprovalEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the {APPROVAL_CUSTOM_TYPE} session entry {} {}",
            self.entry_id, self.reason
        )
    }
}

/// A session's approval history.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApprovalHistory {
    /// One record per request, in the order the requests were asked.
    pub records: Vec<ApprovalRecord>,
    /// The approval entries skipped, in file order.
    pub skipped: Vec<SkippedApprovalEntry>,
}

impl ApprovalHistory {
    /// The records of the requests about tool call `call_id`, oldest first.
    pub fn for_call<'a>(&'a self, call_id: &'a str) -> impl Iterator<Item = &'a ApprovalRecord> {
        self.records
            .iter()
            .filter(move |record| record.asked.call_id.as_deref() == Some(call_id))
    }
}

/// Folds `entries`, in order, into their approval history; other entries
/// are ignored. Pure: it reads only the entries given. An entry that cannot
/// be read, a request id asked twice, an answer to no earlier request, and a
/// second answer are skipped and reported.
pub fn fold_approval_history<'a>(
    entries: impl IntoIterator<Item = &'a SessionEntry>,
) -> ApprovalHistory {
    let mut history = ApprovalHistory::default();
    let mut by_id: HashMap<String, usize> = HashMap::new();
    for entry in entries {
        if entry.entry_type() != "custom"
            || entry.get("customType").and_then(Value::as_str) != Some(APPROVAL_CUSTOM_TYPE)
        {
            continue;
        }
        let mut skip = |reason: String| {
            history.skipped.push(SkippedApprovalEntry {
                entry_id: entry.id().to_owned(),
                reason,
            });
        };
        match ApprovalEntry::from_data(entry.get("data")) {
            Err(reason) => skip(reason),
            Ok(ApprovalEntry::Asked(asked)) => {
                if by_id.contains_key(&asked.id) {
                    skip(format!("asks request {} again", asked.id));
                    continue;
                }
                by_id.insert(asked.id.clone(), history.records.len());
                history.records.push(ApprovalRecord {
                    asked,
                    asked_entry_id: entry.id().to_owned(),
                    outcome: None,
                    decided_entry_id: None,
                });
            }
            Ok(ApprovalEntry::Decided { id, outcome }) => {
                let record = by_id
                    .get(&id)
                    .and_then(|&index| history.records.get_mut(index));
                match record {
                    None => skip(format!(
                        "answers request {id}, which was not asked before it"
                    )),
                    Some(record) if record.outcome.is_some() => {
                        skip(format!("answers request {id} again"));
                    }
                    Some(record) => {
                        record.outcome = Some(outcome);
                        record.decided_entry_id = Some(entry.id().to_owned());
                    }
                }
            }
        }
    }
    history
}

/// The session's approval history, folded over its current branch. Reads
/// only the manager's entries in memory.
pub fn approval_history(session: &SessionManager) -> ApprovalHistory {
    fold_approval_history(session.branch_entries(None))
}

/// Writes approval entries for an engine: a session's file, through
/// `AgentSession`'s write boundary.
pub trait ApprovalRecorder: Send + Sync {
    /// Writes `entry` durably, as [`ApprovalEntry::admitted`] turns it for
    /// `cancel` at the write's admission. Returns the entry written, `None`
    /// when nothing was, or the call's refusal text when the write failed or
    /// could not start; the session has then stopped or is shut down.
    fn record(
        &self,
        entry: ApprovalEntry,
        cancel: Option<AbortSignal>,
    ) -> BoxFuture<'static, Result<Option<ApprovalEntry>, String>>;
}

/// Where an engine's approval entries go: empty until a session binds its
/// recorder, then that recorder for the engine's life. An empty slot
/// records nothing, so an engine outside a session, such as a test's,
/// decides as it would with no history.
#[derive(Default)]
pub struct RecorderSlot {
    recorder: OnceLock<Arc<dyn ApprovalRecorder>>,
}

impl fmt::Debug for RecorderSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecorderSlot")
            .field("bound", &self.is_bound())
            .finish()
    }
}

impl RecorderSlot {
    /// Binds `recorder`; a slot binds once, so a second binding is refused
    /// and changes nothing.
    pub fn bind(&self, recorder: Arc<dyn ApprovalRecorder>) -> Result<(), String> {
        self.recorder
            .set(recorder)
            .map_err(|_| "these tools already record their approvals in a session".to_owned())
    }

    /// Whether a recorder is bound.
    pub fn is_bound(&self) -> bool {
        self.recorder.get().is_some()
    }

    pub(crate) fn get(&self) -> Option<&Arc<dyn ApprovalRecorder>> {
        self.recorder.get()
    }
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
