//! Strict selected-branch recovery. A durable debit is never refunded and
//! a missing result is never evidence that an external effect did not occur.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::session::entry::SessionEntry;
use bake_agent::{BudgetLimit, ModelCallKind};
use bake_ai::{AssistantContentBlock, StopReason};

pub(super) struct Selected {
    pub recovery: RunRecovery,
    pub max_model_calls: Option<u64>,
    pub job_wait: Duration,
    pub terminal: bool,
    pub final_message: Option<bake_ai::AssistantMessage>,
}

impl Selected {
    pub fn refusal(&self) -> String {
        match &self.recovery.disposition {
            RunRecoveryDisposition::NeedsReconciliation { reason } => format!(
                "Run {} needs reconciliation: {reason}",
                self.recovery.run_id
            ),
            RunRecoveryDisposition::Refused { reason } => {
                format!("Run {} cannot resume: {reason}", self.recovery.run_id)
            }
            _ => "Run is no longer resumable".into(),
        }
    }
}

fn read(entry: &SessionEntry) -> Result<Option<Record>, String> {
    if entry.entry_type() != "custom"
        || entry.get("customType").and_then(serde_json::Value::as_str) != Some(CUSTOM_TYPE)
    {
        return Ok(None);
    }
    let record: Record = serde_json::from_value(entry.get("data").cloned().unwrap_or_default())
        .map_err(|error| format!("Invalid run journal entry {}: {error}", entry.id()))?;
    if record.version != 1 || record.run_id.is_empty() || record.run_id.len() > 128 {
        return Err(format!("Unsupported run journal entry {}", entry.id()));
    }
    Ok(Some(record))
}

pub(super) fn project(session: &SessionManager, run_id: &str) -> Result<Selected, String> {
    let entries = session.branch_entries(None);
    let selected_ids: HashSet<_> = entries.iter().map(|entry| entry.id()).collect();
    let mut off_branch_facts = false;
    for entry in session.entries() {
        if !selected_ids.contains(entry.id())
            && read(entry)?.is_some_and(|record| record.run_id == run_id)
        {
            off_branch_facts = true;
        }
    }
    let mut started = None;
    let mut records = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if let Some(record) = read(entry)?
            && record.run_id == run_id
        {
            if matches!(record.event, Event::Started { .. }) && started.replace(index).is_some() {
                return Err("Run journal contains duplicate starts on this branch".into());
            }
            records.push((index, record.event));
        }
    }
    let start = started
        .ok_or_else(|| format!("Run {run_id} was not found on the selected session branch"))?;
    let Some((
        _,
        Event::Started {
            session_id,
            max_model_calls,
            deadline_remaining_ms,
            job_wait_ms,
        },
    )) = records.first()
    else {
        return Err("Run journal has facts before its start".into());
    };
    let max_model_calls = *max_model_calls;
    let foreign_session = session_id != session.session_id();
    let timed = deadline_remaining_ms.is_some();
    let job_wait = Duration::from_millis(*job_wait_ms);
    let mut job_wait_started = false;
    let mut calls = 0_u64;
    let mut last_foreground = start;
    let mut acknowledged = HashSet::new();
    let mut dispatches = HashSet::new();
    let mut terminal = None;
    let by_id: HashMap<_, _> = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| (entry.id(), (i, *entry)))
        .collect();
    for (index, event) in &records {
        if terminal.is_some() {
            return Err("Run journal has facts after its terminal record".into());
        }
        match event {
            Event::Started { .. } => {}
            Event::Admitted { admission } => {
                calls = calls.checked_add(1).ok_or("Run admission count overflow")?;
                if admission.ordinal != calls || max_model_calls.is_some_and(|limit| calls > limit)
                {
                    return Err("Run journal has invalid or over-budget admission ordinals".into());
                }
                if admission.kind == ModelCallKind::Agent {
                    last_foreground = *index;
                }
            }
            Event::Message { entry_id } => {
                let Some((at, entry)) = by_id.get(entry_id.as_str()) else {
                    return Err("Run message refers outside the selected branch".into());
                };
                if *at <= start
                    || *at >= *index
                    || (entry.message().is_none() && !job_notice(entry))
                    || !acknowledged.insert(entry_id.clone())
                {
                    return Err(
                        "Run message reference is not an earlier unique message of this interval"
                            .into(),
                    );
                }
            }
            Event::Dispatch {
                assistant_entry_id,
                tool_call_id,
                tool_name,
            } => {
                let Some((at, entry)) = by_id.get(assistant_entry_id.as_str()) else {
                    return Err("Run dispatch refers outside the selected branch".into());
                };
                let message = entry.message().map(|message| message.to_agent());
                let assistant = message
                    .as_ref()
                    .and_then(LiveMessage::as_assistant)
                    .ok_or("Run dispatch does not reference an assistant")?;
                let matching = assistant.content.iter().filter(|block| matches!(block, AssistantContentBlock::ToolCall(call) if &call.id == tool_call_id && &call.name == tool_name)).count();
                if *at <= start
                    || *at >= *index
                    || !acknowledged.contains(assistant_entry_id)
                    || matching != 1
                    || !dispatches.insert((assistant_entry_id.clone(), tool_call_id.clone()))
                {
                    return Err("Run dispatch has an invalid or duplicate tool identity".into());
                }
            }
            Event::Resumed { model_calls } => {
                if *model_calls != calls || timed || job_wait_started {
                    return Err("Run resume changed its original allowance".into());
                }
            }
            Event::JobWaitStarted => {
                if job_wait.is_zero() || job_wait_started || last_foreground == start {
                    return Err("Run has an invalid job-wait boundary".into());
                }
                job_wait_started = true;
            }
            Event::Terminal { outcome } => terminal = Some(outcome.clone()),
        }
    }

    let mut pending: HashMap<(String, String), String> = HashMap::new();
    let mut results = HashSet::new();
    let mut last_assistant = None;
    let mut final_assistant = None;
    let mut cancelled_assistant = false;
    let mut error_assistant = false;
    let mut final_message = None;
    let mut saw_user = false;
    let mut unowned = false;
    let mut superseded = false;
    let end = records.last().map_or(entries.len(), |(index, event)| {
        if matches!(event, Event::Terminal { .. }) {
            *index
        } else {
            entries.len()
        }
    });
    for (index, entry) in entries.iter().enumerate().take(end).skip(start + 1) {
        if read(entry)?.is_some_and(|record| record.run_id != run_id) {
            superseded = true;
        }
        if entry.entry_type() == "custom_message" {
            if job_notice(entry) {
                saw_user = true;
                final_assistant = None;
                final_message = None;
                cancelled_assistant = false;
                error_assistant = false;
                unowned |= !acknowledged.contains(entry.id());
                continue;
            }
            match entry.get("customType").and_then(serde_json::Value::as_str) {
                Some(NOTICE_TYPE)
                    if entry
                        .get("details")
                        .and_then(|details| details.get("runId"))
                        .and_then(serde_json::Value::as_str)
                        == Some(run_id) => {}
                Some(crate::task_documents::CONTEXT_TYPE) => {}
                _ => unowned = true,
            }
        }
        let Some(message) = entry.message().map(|message| message.to_agent()) else {
            continue;
        };
        if message.as_system().is_some() {
            continue;
        }
        let ack = acknowledged.contains(entry.id());
        if message.as_user().is_some() {
            saw_user = true;
            final_assistant = None;
            cancelled_assistant = false;
            error_assistant = false;
            final_message = None;
            unowned |= !ack;
        } else if let Some(assistant) = message.as_assistant() {
            cancelled_assistant = assistant.stop_reason == StopReason::Aborted;
            error_assistant = assistant.stop_reason == StopReason::Error;
            final_message = Some(assistant.clone());
            last_assistant = Some(entry.id().to_owned());
            let mut has_calls = false;
            for block in &assistant.content {
                if let AssistantContentBlock::ToolCall(call) = block {
                    has_calls = true;
                    if pending
                        .insert((entry.id().into(), call.id.clone()), call.name.clone())
                        .is_some()
                    {
                        return Err("Assistant contains duplicate tool-call identities".into());
                    }
                }
            }
            if assistant.stop_reason == StopReason::Stop
                && !has_calls
                && index > last_foreground
                && last_foreground > start
                && saw_user
            {
                final_assistant = Some(entry.id());
            } else {
                final_assistant = None;
                unowned |= !ack;
            }
        } else if let Some(result) = message.as_tool_result() {
            let key = (
                last_assistant.clone().unwrap_or_default(),
                result.tool_call_id.clone(),
            );
            if pending.get(&key) != Some(&result.tool_name) || !results.insert(key.clone()) {
                return Err(
                    "Tool result has no unique matching assistant call in this interval".into(),
                );
            }
            // A result synced immediately before its journal acknowledgement
            // is sufficient evidence for the recorded dispatch.
            unowned |= !ack && !dispatches.contains(&key);
        } else {
            unowned |= !ack;
        }
    }
    let uncertain = dispatches.iter().find(|key| !results.contains(*key));
    let incomplete = pending.keys().any(|key| !results.contains(key));
    if matches!(terminal, Some(RunOutcome::FinishedTurn))
        && (uncertain.is_some()
            || incomplete
            || unowned
            || superseded
            || !saw_user
            || last_foreground == start
            || cancelled_assistant
            || error_assistant)
    {
        return Err("Run FinishedTurn record contradicts its effect or message checkpoints".into());
    }
    if matches!(
        terminal,
        Some(RunOutcome::BudgetExhausted {
            limit: BudgetLimit::Deadline
        })
    ) && !timed
    {
        return Err("Run deadline outcome has no original timed limit".into());
    }
    if matches!(
        terminal,
        Some(RunOutcome::BudgetExhausted {
            limit: BudgetLimit::ModelCalls
        })
    ) && max_model_calls.is_none_or(|limit| calls < limit)
    {
        return Err("Run model-call outcome contradicts its original allowance".into());
    }
    let unobserved_jobs = !job_wait.is_zero() && !job_notices::unobserved(session).is_empty();
    let disposition = if off_branch_facts {
        RunRecoveryDisposition::Refused { reason: "the run has journal facts outside the selected branch; switching branches cannot refund calls or hide uncertain effects".into() }
    } else if let Some(outcome) = &terminal {
        RunRecoveryDisposition::Completed {
            outcome: outcome.clone(),
        }
    } else if foreign_session {
        RunRecoveryDisposition::Refused {
            reason: "the run belongs to another session; copied or forked runs cannot continue"
                .into(),
        }
    } else if superseded || unowned {
        RunRecoveryDisposition::Refused {
            reason: "later or uncheckpointed conversation entries cannot be attributed to this run"
                .into(),
        }
    } else if uncertain.is_some() {
        RunRecoveryDisposition::NeedsReconciliation { reason: "a recorded tool dispatch has no durable result; its external effect may already have occurred".into() }
    } else if incomplete {
        RunRecoveryDisposition::NeedsReconciliation { reason: "the durable assistant has an incomplete tool exchange; recovery will not replay its calls".into() }
    } else if cancelled_assistant {
        RunRecoveryDisposition::Refused {
            reason: "the durable assistant was cancelled; recovery will not restart cancelled work"
                .into(),
        }
    } else if final_assistant.is_some() && !unobserved_jobs {
        RunRecoveryDisposition::Completed {
            outcome: RunOutcome::FinishedTurn,
        }
    } else if !saw_user {
        RunRecoveryDisposition::Refused {
            reason: "the run has no durable input checkpoint".into(),
        }
    } else if timed {
        RunRecoveryDisposition::Refused { reason: "the original monotonic deadline cannot be proven after restart; timed runs cannot continue".into() }
    } else if job_wait_started {
        RunRecoveryDisposition::Refused { reason: "the original absolute job-wait deadline cannot be proven after restart; an interrupted job wait cannot continue".into() }
    } else if max_model_calls.is_some_and(|limit| calls >= limit) {
        RunRecoveryDisposition::Completed {
            outcome: RunOutcome::BudgetExhausted {
                limit: BudgetLimit::ModelCalls,
            },
        }
    } else {
        RunRecoveryDisposition::Resumable
    };
    Ok(Selected {
        recovery: RunRecovery {
            run_id: run_id.into(),
            model_calls: calls,
            disposition,
        },
        max_model_calls,
        job_wait,
        terminal: terminal.is_some(),
        final_message,
    })
}

fn job_notice(entry: &SessionEntry) -> bool {
    entry.entry_type() == "custom_message"
        && entry.get("customType").and_then(serde_json::Value::as_str)
            == Some(job_notices::NOTICE_TYPE)
        && job_notices::notice_id(entry.get("details")).is_some()
        && matches!(
            entry.typed(),
            Ok(crate::session::entry::TypedEntry::CustomMessage(_))
        )
}
