//! Explicitly selected recovery intervals in Pi custom entries.
//!
//! Journal writes are required execution work. Tool dispatch is recorded
//! before the tool's current permission check, so an interrupted dispatch
//! is conservative evidence of uncertainty, never permission to replay.

mod projection;

use super::*;
use bake_agent::{AdmissionObserver, ModelCallAdmission, RunLimits};
use serde::{Deserialize, Serialize};

const CUSTOM_TYPE: &str = "bake.run.v1";
const NOTICE_TYPE: &str = "bake.run.recovery.v1";

/// The result of one explicitly journaled execution interval.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoverableRunResult {
    /// Durable identity, independent of session-local execution identities.
    pub run_id: String,
    /// Execution outcome after owned work and required writes settled.
    pub outcome: RunOutcome,
    /// The selected interval's last durable assistant, never a later run's tail.
    pub final_message: Option<bake_ai::AssistantMessage>,
}

/// Recovery evidence for one run on the selected session branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecovery {
    /// The explicitly selected durable run.
    pub run_id: String,
    /// Already-spent model admissions, including interrupted requests.
    pub model_calls: u64,
    /// Whether continuation is supported by durable evidence.
    pub disposition: RunRecoveryDisposition,
}

/// A recovery decision does not verify the user's task or external effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunRecoveryDisposition {
    /// A terminal record or a durable final assistant settled execution.
    Completed { outcome: RunOutcome },
    /// Untimed execution can continue with its original remaining allowance.
    Resumable,
    /// An effect or incomplete tool exchange needs explicit reconciliation.
    NeedsReconciliation { reason: String },
    /// Continuation cannot preserve the original execution contract.
    Refused { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    version: u32,
    run_id: String,
    event: Event,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Event {
    Started {
        session_id: String,
        #[serde(deserialize_with = "present_option")]
        max_model_calls: Option<u64>,
        #[serde(deserialize_with = "present_option")]
        deadline_remaining_ms: Option<u64>,
    },
    Admitted {
        admission: ModelCallAdmission,
    },
    Message {
        entry_id: String,
    },
    Dispatch {
        assistant_entry_id: String,
        tool_call_id: String,
        tool_name: String,
    },
    Resumed {
        model_calls: u64,
    },
    Terminal {
        outcome: RunOutcome,
    },
}

fn present_option<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::deserialize(deserializer)
}

pub(super) struct Journal {
    pub run_id: String,
    core: Arc<Core>,
    permit: Mutex<Option<WorkPermit>>,
    latest_assistant: Mutex<Option<String>>,
    final_message: Mutex<Option<bake_ai::AssistantMessage>>,
    active: AtomicBool,
    pub start: Start,
}

pub(super) enum Start {
    New(RunLimits),
    Resume {
        expected_leaf: Option<String>,
    },
    Reconcile {
        expected_leaf: Option<String>,
        outcome: RunOutcome,
    },
}

struct JournalLease(tokio::sync::watch::Sender<Activity>);

impl Drop for JournalLease {
    fn drop(&mut self) {
        self.0.send_modify(|activity| activity.context_commits -= 1);
    }
}

impl Journal {
    fn new(core: Arc<Core>, run_id: String, start: Start) -> Arc<Self> {
        Arc::new(Self {
            run_id,
            core,
            start,
            permit: Mutex::new(None),
            latest_assistant: Mutex::new(None),
            final_message: Mutex::new(None),
            active: AtomicBool::new(false),
        })
    }

    pub fn bind(&self, permit: &WorkPermit) {
        *self.permit.lock().unwrap_or_else(PoisonError::into_inner) = Some(permit.share());
    }

    pub fn release(&self) {
        self.permit
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }

    fn record(&self, event: Event) -> GroupEntry {
        GroupEntry::Custom {
            custom_type: CUSTOM_TYPE.into(),
            data: Some(
                serde_json::to_value(Record {
                    version: 1,
                    run_id: self.run_id.clone(),
                    event,
                })
                .expect("run journal contains only JSON-compatible fields"),
            ),
        }
    }

    async fn commit<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Core) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let permit = self
            .permit
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(WorkPermit::share)
            .ok_or_else(|| "Recoverable run has no execution owner".to_owned())?;
        self.core
            .activity
            .send_modify(|activity| activity.context_commits += 1);
        let lease = JournalLease(self.core.activity.clone());
        let core = Arc::clone(&self.core);
        let task = permit
            .spawn_blocking(move || {
                let _lease = lease;
                let result = work(&core);
                if let Err(error) = &result
                    && let Some(control) = core.control()
                {
                    control.fail(error.clone());
                }
                result
            })
            .map_err(|error| error.to_string())?;
        task.await
            .map_err(|error| format!("Run journal I/O failed: {error}"))?
    }

    async fn append(&self, event: Event) -> Result<(), String> {
        let entry = self.record(event);
        self.commit(move |core| {
            write_session(&core.session, &core.gap, |session| {
                session.append_durable_group(vec![entry])
            })
            .map(|_| ())
            .map_err(|error| error.message())
        })
        .await
    }

    pub fn observer(self: &Arc<Self>) -> AdmissionObserver {
        let journal = Arc::clone(self);
        Arc::new(move |admission| {
            let journal = Arc::clone(&journal);
            Box::pin(async move { journal.append(Event::Admitted { admission }).await })
        })
    }

    pub async fn begin(&self) -> Result<Option<RunOutcome>, String> {
        let mut reconciled = None;
        match &self.start {
            Start::New(limits) => {
                let session_id = lock(&self.core.session).session_id().to_owned();
                self.append(Event::Started {
                    session_id,
                    max_model_calls: limits.max_model_calls,
                    deadline_remaining_ms: limits.deadline.map(|deadline| {
                        u64::try_from(
                            deadline
                                .saturating_duration_since(tokio::time::Instant::now())
                                .as_millis(),
                        )
                        .unwrap_or(u64::MAX)
                    }),
                })
                .await?;
            }
            Start::Resume { expected_leaf } => {
                let expected_leaf = expected_leaf.clone();
                let run_id = self.run_id.clone();
                self.commit(move |core| {
                    let projection = write_session(&core.session, &core.gap, |session| {
                        if session.leaf_id() != expected_leaf.as_deref() {
                            return Ok::<_, String>(Err("Session branch changed before recovery admission".into()));
                        }
                        let selected = projection::project(session, &run_id)?;
                        if selected.recovery.disposition != RunRecoveryDisposition::Resumable {
                            return Ok(Err(selected.refusal()));
                        }
                        let resumed = Record { version: 1, run_id: run_id.clone(), event: Event::Resumed { model_calls: selected.recovery.model_calls } };
                        session.append_durable_group(vec![
                            GroupEntry::Custom { custom_type: CUSTOM_TYPE.into(), data: Some(serde_json::to_value(resumed).map_err(|error| error.to_string())?) },
                            GroupEntry::CustomMessage {
                                custom_type: NOTICE_TYPE.into(),
                                content: UserContent::Text("Execution resumed from its durable checkpoint. Completed tool results are already recorded. Continue the task using the current tools and permission policy; do not repeat completed effects.".into()),
                                display: true,
                                details: Some(serde_json::json!({"runId":run_id})),
                            },
                        ]).map_err(|error| error.to_string())?;
                        Ok(Ok(session.build_session_projection()))
                    }).map_err(|error| error.message())??;
                    core.refresh_context(&projection);
                    Ok(())
                }).await?;
            }
            Start::Reconcile {
                expected_leaf,
                outcome,
            } => {
                let expected_leaf = expected_leaf.clone();
                let run_id = self.run_id.clone();
                let expected_outcome = outcome.clone();
                self.commit(move |core| {
                    let session = lock(&core.session);
                    if session.leaf_id() != expected_leaf.as_deref() {
                        return Err("Session branch changed before recovery admission".into());
                    }
                    let selected = projection::project(&session, &run_id)?;
                    if selected.recovery.disposition
                        != (RunRecoveryDisposition::Completed {
                            outcome: expected_outcome,
                        })
                    {
                        return Err("Run completion evidence changed before reconciliation".into());
                    }
                    Ok(())
                })
                .await?;
                reconciled = Some(outcome.clone());
            }
        }
        self.active.store(true, Ordering::SeqCst);
        self.core.emit(&SessionEvent::RecoverableRunStarted {
            run_id: self.run_id.clone(),
        });
        Ok(reconciled)
    }

    pub async fn message(
        &self,
        message: &LiveMessage,
        entry_id: Option<String>,
    ) -> Result<(), String> {
        let entry_id =
            entry_id.ok_or_else(|| "Recoverable run message was not persisted".to_owned())?;
        self.append(Event::Message {
            entry_id: entry_id.clone(),
        })
        .await?;
        if message.as_assistant().is_some() {
            *self
                .latest_assistant
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(entry_id);
        }
        Ok(())
    }

    async fn dispatch(&self, tool_call: &bake_ai::ToolCall) -> Result<(), String> {
        let assistant_entry_id = self
            .latest_assistant
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| "Tool dispatch has no durable assistant checkpoint".to_owned())?;
        self.append(Event::Dispatch {
            assistant_entry_id,
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
        })
        .await
    }

    pub async fn finish(
        &self,
        outcome: RunOutcome,
        control: RunControl,
    ) -> Result<RunOutcome, String> {
        if !self.active.load(Ordering::SeqCst) {
            return Ok(control.finish_interval(outcome));
        }
        let run_id = self.run_id.clone();
        let (outcome, final_message) = self
            .commit(move |core| {
                #[cfg(test)]
                if let Some(before) = core
                    .hooks
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .before_run_terminal
                    .take()
                {
                    before();
                }
                write_session(&core.session, &core.gap, |session| {
                    let outcome = control.finish_interval(outcome);
                    #[cfg(test)]
                    if let Some(after) = core
                        .hooks
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .after_run_terminal_admission
                        .take()
                    {
                        after();
                    }
                    let record = Record {
                        version: 1,
                        run_id: run_id.clone(),
                        event: Event::Terminal {
                            outcome: outcome.clone(),
                        },
                    };
                    let data = serde_json::to_value(record).map_err(|error| error.to_string())?;
                    session
                        .append_durable_group(vec![GroupEntry::Custom {
                            custom_type: CUSTOM_TYPE.into(),
                            data: Some(data),
                        }])
                        .map_err(|error| error.to_string())?;
                    let final_message = projection::project(session, &run_id)?.final_message;
                    Ok::<_, String>((outcome, final_message))
                })
                .map_err(|error| error.message())
            })
            .await?;
        *self
            .final_message
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = final_message;
        Ok(outcome)
    }
}

pub(super) fn current(executions: &Mutex<prompt_execution::Executions>) -> Option<Arc<Journal>> {
    executions
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .current
        .as_ref()
        .and_then(|execution| execution.journal.clone())
}

pub(super) fn tool_hook(
    executions: prompt_execution::SharedExecutions,
    previous: Option<bake_agent::BeforeToolCall>,
) -> bake_agent::BeforeToolCall {
    hook::before_tool_call(move |context, signal| {
        let journal = current(&executions);
        let previous = previous.clone();
        Box::pin(async move {
            let call = context.tool_call.clone();
            let decision = if let Some(previous) = previous {
                previous(context, signal).await?
            } else {
                None
            };
            if decision.as_ref().is_some_and(|decision| decision.block) {
                return Ok(decision);
            }
            if let Some(journal) = journal {
                journal.dispatch(&call).await?;
            }
            Ok(decision)
        })
    })
}

impl AgentSession {
    /// Starts one opt-in durable interval. Its original allowance is journaled;
    /// dropping the waiter leaves the session-owned execution running.
    pub async fn prompt_recoverable(
        &self,
        text: &str,
        images: Vec<ImageContent>,
        limits: RunLimits,
    ) -> Result<RecoverableRunResult, String> {
        if lock(&self.session).session_file().is_none() {
            return Err("Recoverable runs require a persistent session".into());
        }
        let journal = Journal::new(
            Arc::clone(&self.core),
            crate::session::id::uuid_v7(),
            Start::New(limits),
        );
        let control = RunControl::with_admission_observer(limits, Some(journal.observer()));
        let run_id = journal.run_id.clone();
        let outcome = self
            .execute_prompt(
                Some((text.to_owned(), images)),
                control,
                Some(Arc::clone(&journal)),
            )
            .await?;
        let final_message = journal
            .final_message
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        Ok(RecoverableRunResult {
            run_id,
            outcome,
            final_message,
        })
    }

    /// Inspects only the named run on the selected branch; never calls a provider.
    pub fn inspect_run(&self, run_id: &str) -> Result<RunRecovery, String> {
        projection::project(&lock(&self.session), run_id).map(|selected| selected.recovery)
    }

    /// Continues a selected untimed run without refunding spent calls or
    /// replaying uncertain effects. Current tools and approvals apply.
    pub async fn resume_run(&self, run_id: &str) -> Result<RecoverableRunResult, String> {
        let (selected, expected_leaf) = {
            let session = lock(&self.session);
            (
                projection::project(&session, run_id)?,
                session.leaf_id().map(str::to_owned),
            )
        };
        let start = match &selected.recovery.disposition {
            RunRecoveryDisposition::Completed { outcome } if selected.terminal => {
                return Ok(RecoverableRunResult {
                    run_id: run_id.into(),
                    outcome: outcome.clone(),
                    final_message: selected.final_message,
                });
            }
            RunRecoveryDisposition::Completed { outcome } => Start::Reconcile {
                expected_leaf,
                outcome: outcome.clone(),
            },
            RunRecoveryDisposition::Resumable => Start::Resume { expected_leaf },
            _ => return Err(selected.refusal()),
        };
        let limits = RunLimits {
            deadline: None,
            max_model_calls: selected.max_model_calls,
        };
        let journal = Journal::new(Arc::clone(&self.core), run_id.into(), start);
        let control = RunControl::restore(
            limits,
            selected.recovery.model_calls,
            Some(journal.observer()),
        );
        let outcome = self
            .execute_prompt(None, control, Some(Arc::clone(&journal)))
            .await?;
        Ok(RecoverableRunResult {
            run_id: run_id.into(),
            outcome,
            final_message: journal
                .final_message
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        })
    }
}
