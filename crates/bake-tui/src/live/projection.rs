//! Turns a live session's events and messages into transcript rows.
//!
//! Adapted from Pi v1.2.0 `packages/coding-agent/src/modes/interactive/
//! interactive-mode.ts` (`handleEvent` and `renderSessionContext`): an
//! assistant message streams as live rows, its tool calls join it as
//! cards that settle when their results arrive, and the turn commits once
//! its tool results are in, so a streamed row is drawn once. A user
//! message commits when the agent starts it, which is when the runtime has
//! admitted the prompt. An aborted or failed response marks its unfinished
//! calls failed with the response's error, as Pi's `message_end` handler
//! does.
//!
//! A `codemode` call reads as one block: its script, then the calls the
//! script made from its result's `details.calls`, which partial results
//! publish while it runs, so the live block and a replayed one come from
//! the same record. The nested calls' own events never reach the
//! transcript.
//!
//! Background work reads as cards that pair up: the call that starts a
//! background job or subagent, the calls that reach it later, and the
//! notice it posts when it ends each carry its id, such as `bash-1`, and
//! name it by the command or label its start recorded.
//!
//! Nothing here touches the terminal or the session: [`Projector::apply`]
//! takes one event and returns the view updates it causes.

use std::collections::HashMap;
use std::time::Duration;

use bake_agent::{AgentEvent, AgentMessage, AgentToolResult};
use bake_ai::{AssistantContentBlock, AssistantMessage, StopReason, UserContent, UserContentBlock};
use bake_coding_agent::agent_session::job_notices::NOTICE_TYPE;
use bake_coding_agent::agent_session::{
    BackgroundCompactionStatus, CompactionReason, SessionEvent, SummarizationSource,
};
use bake_coding_agent::hooks;
use bake_coding_agent::schedules;
use bake_coding_agent::session::messages::CustomMessage;
use bake_coding_agent::subagents::SUBAGENT_DETAIL;
use bake_coding_agent::subagents::messages as subagents;
use bake_coding_agent::tools::bash::BACKGROUND_JOB_DETAIL;
use bake_coding_agent::tools::codemode::CODEMODE_TOOL_NAME;
use bake_coding_agent::tools::job_tools::STILL_RUNNING;
use bake_tui_view::codemode::{self, Codemode, NestedCall, NestedStatus};
use bake_tui_view::copy;
use bake_tui_view::jobs::{self, JobState};
use bake_tui_view::runtime::{Pending, RuntimeUpdate, Target};
use bake_tui_view::transcript::{CallState, Row};
use serde_json::Value;

mod calls;
mod replay;
#[cfg(test)]
mod tests;

use calls::{call_row, learn, partial, settled};
use replay::notice_row;
pub use replay::{replay, replay_compacted};

/// Pi's text for a response the user aborted.
pub const ABORTED: &str = "Operation aborted";
/// What an unfinished call says once its turn ended without a result.
pub const INTERRUPTED: &str = "interrupted";

/// The running turn as rows: the assistant message's thinking, text, and
/// tool calls, then any error it ended with.
#[derive(Debug, Default)]
pub struct Projector {
    rows: Vec<Row>,
    /// Each tool call's row in `rows`, by call id.
    calls: HashMap<String, usize>,
    /// What each call's result said, kept across re-projections of the
    /// streaming message.
    settled: HashMap<String, Outcome>,
    /// What each background job and subagent the session started is called.
    labels: Labels,
    /// The calls running now, as call id and tool name, in start order.
    running: Vec<(String, String)>,
    phase: Option<String>,
    /// Whether a user message started since [`Projector::begin`]: the
    /// runtime admitted the prompt.
    admitted: bool,
    /// The composer's id for the prompt, reported once it is admitted.
    prompt_id: Option<u64>,
    /// The frontend's own waiting prompts, which the pending panel lists
    /// after the session's queues.
    waiting: Vec<String>,
    /// The last response's stop reason, for the turn's outcome.
    last_stop: Option<StopReason>,
    last_error: Option<String>,
}

/// What each background job and subagent is called, by its id: a job's
/// command and a subagent's label, as the results that started them
/// recorded.
type Labels = HashMap<String, String>;

/// A call's result as its card shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Outcome {
    state: CallState,
    /// The background job or subagent the call started.
    id: Option<String>,
    summary: Option<String>,
    output: Vec<String>,
    /// A `codemode` call's record of the calls its script made.
    calls: Vec<NestedCall>,
    /// The file a `codemode` result's whole output went to, when cut.
    full_output_path: Option<String>,
    /// How long a `codemode` script ran, from its result's header.
    wall_time: Option<Duration>,
}

impl Outcome {
    fn running(output: Vec<String>) -> Self {
        Self {
            state: CallState::Running,
            id: None,
            summary: None,
            output,
            calls: Vec::new(),
            full_output_path: None,
            wall_time: None,
        }
    }

    fn failed(output: Vec<String>) -> Self {
        Self {
            state: CallState::Failed,
            ..Self::running(output)
        }
    }
}

impl Projector {
    /// A projector for a session that already holds `messages`, so a
    /// background job or subagent they started is named by its label when
    /// a later call or notice reaches it.
    pub fn with_history(messages: &[AgentMessage]) -> Self {
        let mut labels = Labels::new();
        for result in messages.iter().filter_map(AgentMessage::as_tool_result) {
            learn(&mut labels, result.details.as_ref());
        }
        Self {
            labels,
            ..Self::default()
        }
    }

    /// Starts tracking a new prompt, which has the composer's `id` when the
    /// composer sent it.
    pub fn begin(&mut self, id: Option<u64>) {
        self.admitted = false;
        self.prompt_id = id;
        self.last_stop = None;
        self.last_error = None;
        self.phase = None;
        self.running.clear();
    }

    /// Whether the prompt since [`Projector::begin`] reached the agent.
    pub fn admitted(&self) -> bool {
        self.admitted
    }

    /// How the last response of the prompt stopped.
    pub fn last_stop(&self) -> Option<StopReason> {
        self.last_stop
    }

    pub(super) fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Sets the frontend's waiting prompts and returns the pending panel
    /// with them, after the session's `steering` and `follow_up`.
    pub fn set_waiting(
        &mut self,
        waiting: Vec<String>,
        steering: &[String],
        follow_up: &[String],
    ) -> RuntimeUpdate {
        self.waiting = waiting;
        RuntimeUpdate::Pending(pending(steering, follow_up, &self.waiting))
    }

    /// The updates one session event causes.
    pub fn apply(&mut self, event: &SessionEvent<'_>) -> Vec<RuntimeUpdate> {
        match event {
            SessionEvent::Agent(event) => self.agent(event),
            SessionEvent::AgentEnd { .. } => self.settle(),
            SessionEvent::QueueUpdate {
                steering,
                follow_up,
            } => vec![RuntimeUpdate::Pending(pending(
                steering,
                follow_up,
                &self.waiting,
            ))],
            SessionEvent::CompactionStart { .. } => self.set_phase("compacting"),
            SessionEvent::BackgroundCompaction {
                status: BackgroundCompactionStatus::Committed,
                result: Some(result),
                ..
            } => vec![self.commit_now(Row::Info(format!(
                "Compacted the conversation in the background ({} tokens before)",
                result.tokens_before
            )))],
            // `/compact` reports its own outcome, with its draft.
            SessionEvent::CompactionEnd {
                reason: CompactionReason::Manual,
                ..
            } => Vec::new(),
            SessionEvent::CompactionEnd {
                result,
                aborted,
                error_message,
                ..
            } => {
                let row = if *aborted {
                    Some(Row::Info("Compaction cancelled".into()))
                } else if let Some(result) = result {
                    Some(Row::Info(format!(
                        "Compacted the conversation ({} tokens before)",
                        result.tokens_before
                    )))
                } else {
                    error_message.clone().map(Row::Error)
                };
                // Pi clears its compaction indicator: the run, retried or
                // about to settle, reads as it did before.
                let mut updates: Vec<_> = row.map(|row| self.commit_now(row)).into_iter().collect();
                let phase = self.running_phase();
                updates.extend(self.set_phase(&phase));
                updates
            }
            SessionEvent::AutoRetryStart {
                attempt,
                max_attempts,
                delay_ms,
                error_message,
            } => {
                let mut updates = vec![self.commit_now(Row::Info(format!(
                    "Retrying ({attempt}/{max_attempts}) in {}s: {error_message}",
                    delay_ms.div_ceil(1000)
                )))];
                updates.extend(self.set_phase("waiting to retry"));
                updates
            }
            SessionEvent::AutoRetryEnd {
                success: false,
                attempt,
                final_error,
            } => vec![self.commit_now(Row::Error(format!(
                "Retry failed after {attempt} attempts: {}",
                final_error.as_deref().unwrap_or("Unknown error")
            )))],
            SessionEvent::SummarizationRetryScheduled { error_message, .. } => {
                let mut updates = vec![self.commit_now(Row::Error(error_message.clone()))];
                updates.extend(self.show_phase("waiting to retry"));
                updates
            }
            SessionEvent::SummarizationRetryAttemptStart { source } => match source {
                SummarizationSource::Compaction(_) => self.show_phase("compacting"),
                SummarizationSource::BranchSummary => self.show_phase(copy::SUMMARIZING_BRANCH),
            },
            // A successful retry shows its response, as Pi's does. A
            // discarded background summary changes nothing on screen: the
            // foreground compaction it would have spared reports itself,
            // and a failed write stops the run with its own error. Masking
            // shows only as the status line's context.
            _ => Vec::new(),
        }
    }

    fn agent(&mut self, event: &AgentEvent) -> Vec<RuntimeUpdate> {
        match event {
            AgentEvent::MessageStart { message } => {
                // A user's message, or a background job's or subagent's
                // notice or a subagent's message, in a run it opened or
                // one already streaming.
                let row = match notice_row(message, &self.labels) {
                    Some(row) => row,
                    None => match message.as_user() {
                        Some(user) => Row::User(user_text(&user.content)),
                        None => return Vec::new(),
                    },
                };
                let mut updates: Vec<RuntimeUpdate> = Vec::new();
                if !self.admitted {
                    self.admitted = true;
                    updates.extend(self.prompt_id.take().map(RuntimeUpdate::Admitted));
                }
                // Steering arrives between turns, after the last one
                // committed; anything still live settles first.
                updates.extend(self.settle());
                updates.push(RuntimeUpdate::Commit(vec![row]));
                updates
            }
            AgentEvent::MessageUpdate { message, .. } => match message.as_assistant() {
                Some(assistant) => {
                    let mut updates = self.set_phase(streaming_phase(assistant));
                    self.project(assistant, false);
                    updates.push(self.live());
                    updates
                }
                None => Vec::new(),
            },
            AgentEvent::MessageEnd { message } => match message.as_assistant() {
                Some(assistant) => {
                    self.last_stop = Some(assistant.stop_reason);
                    self.last_error = (assistant.stop_reason == StopReason::Error).then(|| {
                        assistant
                            .error_message
                            .clone()
                            .filter(|message| !message.is_empty())
                            .unwrap_or_else(|| "Error".into())
                    });
                    self.project(assistant, true);
                    vec![self.live()]
                }
                None => Vec::new(),
            },
            // A nested call is not the model's call; Pi's interactive mode
            // skips its events too.
            AgentEvent::ToolExecutionStart {
                parent_tool_call_id: Some(_),
                ..
            }
            | AgentEvent::ToolExecutionUpdate {
                parent_tool_call_id: Some(_),
                ..
            }
            | AgentEvent::ToolExecutionEnd {
                parent_tool_call_id: Some(_),
                ..
            } => Vec::new(),
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
                parent_tool_call_id: None,
            } => {
                if !self.running.iter().any(|(id, _)| id == tool_call_id) {
                    self.running.push((tool_call_id.clone(), tool_name.clone()));
                }
                let mut updates = self.set_phase(&self.running_phase());
                let row = call_row(tool_name, args, &self.labels, Outcome::running(Vec::new()));
                match self.calls.get(tool_call_id) {
                    Some(&index) => self.rows[index] = row,
                    None => {
                        self.calls.insert(tool_call_id.clone(), self.rows.len());
                        self.rows.push(row);
                    }
                }
                updates.push(self.live());
                updates
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                partial_result,
                ..
            } => {
                self.update_call(tool_call_id, partial(tool_name, partial_result));
                vec![self.live()]
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
                ..
            } => {
                learn(&mut self.labels, result.details.as_ref());
                self.update_call(tool_call_id, settled(tool_name, *is_error, result));
                self.running.retain(|(id, _)| id != tool_call_id);
                let mut updates = vec![self.live()];
                updates.extend(self.set_phase(&self.running_phase()));
                updates
            }
            AgentEvent::TurnEnd { .. } => self.settle(),
            _ => Vec::new(),
        }
    }

    /// Rebuilds the assistant's rows from its message so far, keeping each
    /// settled call's result. A finished response that failed or was
    /// aborted fails its unfinished calls with its error, and says it.
    fn project(&mut self, assistant: &AssistantMessage, finished: bool) {
        let failure = finished
            .then(|| match assistant.stop_reason {
                StopReason::Aborted => Some(ABORTED.to_owned()),
                StopReason::Error => Some(
                    assistant
                        .error_message
                        .clone()
                        .filter(|message| !message.is_empty())
                        .unwrap_or_else(|| "Error".into()),
                ),
                _ => None,
            })
            .flatten();
        let (rows, calls) =
            assistant_rows(assistant, &self.settled, &self.labels, failure.as_deref());
        // Calls that started without appearing in the message keep their rows.
        let mut extra: Vec<(String, Row)> = self
            .calls
            .iter()
            .filter(|(id, _)| !calls.contains_key(*id))
            .map(|(id, &index)| (id.clone(), self.rows[index].clone()))
            .collect();
        extra.sort_by_key(|(id, _)| self.calls.get(id).copied());
        self.rows = rows;
        self.calls = calls;
        for (id, row) in extra {
            self.calls.insert(id, self.rows.len());
            self.rows.push(row);
        }
        if let Some(failure) = failure {
            self.rows.push(Row::Error(failure));
        }
    }

    /// Shows `outcome` on call `id`'s card. A running call's partial output
    /// keeps the card's status, such as `waiting`, and a settled call's
    /// outcome is kept for the streaming message's next projection.
    fn update_call(&mut self, id: &str, outcome: Outcome) {
        let Some(&index) = self.calls.get(id) else {
            return;
        };
        let running = outcome.state == CallState::Running;
        if !running {
            self.settled.insert(id.to_owned(), outcome.clone());
        }
        match &mut self.rows[index] {
            Row::Call {
                state,
                id,
                summary,
                output,
                ..
            } => {
                *state = outcome.state;
                *output = outcome.output;
                if outcome.id.is_some() {
                    *id = outcome.id;
                }
                if !running {
                    *summary = outcome.summary;
                }
            }
            // Each partial result holds every call so far, so the latest
            // one replaces the record.
            Row::Codemode(script) => {
                script.state = outcome.state;
                script.calls = outcome.calls;
                script.output = outcome.output;
                script.full_output_path = outcome.full_output_path;
                script.wall_time = outcome.wall_time;
                if !running {
                    script.summary = outcome.summary;
                }
            }
            _ => {}
        }
    }

    fn live(&self) -> RuntimeUpdate {
        RuntimeUpdate::Live(self.rows.clone())
    }

    /// Commits whatever the prompt left live, once it has settled.
    pub fn finish(&mut self) -> Vec<RuntimeUpdate> {
        self.settle()
    }

    /// Commits the turn's rows, failing any call left running, and starts
    /// the next turn empty.
    fn settle(&mut self) -> Vec<RuntimeUpdate> {
        if self.rows.is_empty() {
            self.calls.clear();
            self.settled.clear();
            self.running.clear();
            return Vec::new();
        }
        self.running.clear();
        let mut rows = std::mem::take(&mut self.rows);
        for row in &mut rows {
            match row {
                Row::Call { state, summary, .. } if *state == CallState::Running => {
                    *state = CallState::Failed;
                    *summary = Some(INTERRUPTED.into());
                }
                // No result was logged, so the replay knows no calls: the
                // block drops the ones its partial results showed, to read
                // as the session log replays it.
                Row::Codemode(script) if script.state == CallState::Running => {
                    script.state = CallState::Failed;
                    script.summary = Some(INTERRUPTED.into());
                    script.calls.clear();
                }
                _ => {}
            }
        }
        self.calls.clear();
        self.settled.clear();
        vec![RuntimeUpdate::Commit(rows)]
    }

    /// Adds `row` after the running turn's rows: live with them while the
    /// turn runs, so a note never takes their place, and committed at once
    /// when nothing is live.
    fn commit_now(&mut self, row: Row) -> RuntimeUpdate {
        if self.rows.is_empty() {
            RuntimeUpdate::Commit(vec![row])
        } else {
            self.rows.push(row);
            self.live()
        }
    }

    /// The phase while calls run: `running bash`, `running subagent +2`
    /// with two more beside it, or `thinking` once none does.
    fn running_phase(&self) -> String {
        match self.running.as_slice() {
            [] => copy::FIRST_PHASE.to_owned(),
            [(_, tool)] => format!("running {tool}"),
            [(_, tool), rest @ ..] => format!("running {tool} +{}", rest.len()),
        }
    }

    /// Sends `phase` even when it was the last this projector sent: a
    /// summary retries under `/compact` or `/tree`, whose commands send
    /// phases of their own in between.
    fn show_phase(&mut self, phase: &str) -> Vec<RuntimeUpdate> {
        self.phase = None;
        self.set_phase(phase)
    }

    fn set_phase(&mut self, phase: &str) -> Vec<RuntimeUpdate> {
        if self.phase.as_deref() == Some(phase) {
            return Vec::new();
        }
        self.phase = Some(phase.to_owned());
        vec![RuntimeUpdate::Phase(phase.to_owned())]
    }
}

/// Whether `event` can change what the status line reads: the model or
/// thinking level, the context a response measured or a compaction or
/// masking batch reduced, or the session's name. The driver reads the
/// status again after one.
pub fn moves_status(event: &SessionEvent<'_>) -> bool {
    matches!(
        event,
        SessionEvent::Agent(AgentEvent::MessageEnd { .. } | AgentEvent::TurnEnd { .. })
            | SessionEvent::AgentEnd { .. }
            | SessionEvent::AgentSettled { .. }
            | SessionEvent::ThinkingLevelChanged(_)
            | SessionEvent::SessionInfoChanged(_)
            | SessionEvent::CompactionEnd { .. }
            | SessionEvent::BackgroundCompaction {
                status: BackgroundCompactionStatus::Committed,
                ..
            }
            | SessionEvent::ContextMasking { .. }
    )
}

/// The pending-input panel's items: steering for the next step, then
/// follow-ups and the frontend's own waiting prompts for the next turn.
pub fn pending(steering: &[String], follow_up: &[String], waiting: &[String]) -> Vec<Pending> {
    let item = |target| {
        move |text: &String| Pending {
            text: text.clone(),
            target,
        }
    };
    steering
        .iter()
        .map(item(Target::NextStep))
        .chain(follow_up.iter().chain(waiting).map(item(Target::NextTurn)))
        .collect()
}

/// What the bar says the response is doing, from its newest block.
fn streaming_phase(assistant: &AssistantMessage) -> &'static str {
    match assistant.content.last() {
        Some(AssistantContentBlock::Text(_)) => "writing",
        Some(AssistantContentBlock::ToolCall(_)) => "preparing a tool call",
        _ => "thinking",
    }
}

/// A user message as its row shows it: its text, and how many images went
/// with it.
fn user_text(content: &UserContent) -> String {
    let text = bake_ai::utils::text::user_content_text(content, "");
    let images = match content {
        UserContent::Blocks(blocks) => blocks
            .iter()
            .filter(|block| matches!(block, UserContentBlock::Image(_)))
            .count(),
        UserContent::Text(_) => 0,
    };
    match images {
        0 => text,
        1 => format!("{text} (+1 image)"),
        n => format!("{text} (+{n} images)"),
    }
}

/// The rows of an assistant message: reasoning, text, and its tool calls,
/// each call settled from `settled` when its result is known, and failed
/// with `failure` when the response failed before it ran.
fn assistant_rows(
    assistant: &AssistantMessage,
    settled: &HashMap<String, Outcome>,
    labels: &Labels,
    failure: Option<&str>,
) -> (Vec<Row>, HashMap<String, usize>) {
    let mut rows = Vec::new();
    let mut calls = HashMap::new();
    for block in &assistant.content {
        match block {
            AssistantContentBlock::Thinking(thinking)
                if !thinking.thinking.trim().is_empty() && thinking.redacted != Some(true) =>
            {
                rows.push(Row::Reasoning(thinking.thinking.trim().to_owned()));
            }
            AssistantContentBlock::Text(text) if !text.text.trim().is_empty() => {
                rows.push(Row::Answer(text.text.trim().to_owned()));
            }
            AssistantContentBlock::ToolCall(call) => {
                let args = Value::Object(call.arguments.clone());
                let outcome = match (settled.get(&call.id), failure) {
                    (Some(outcome), _) => outcome.clone(),
                    (None, Some(failure)) => Outcome::failed(vec![failure.to_owned()]),
                    (None, None) => Outcome::running(Vec::new()),
                };
                calls.insert(call.id.clone(), rows.len());
                rows.push(call_row(&call.name, &args, labels, outcome));
            }
            _ => {}
        }
    }
    (rows, calls)
}
