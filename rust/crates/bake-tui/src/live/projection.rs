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
//! Background work reads as cards that pair up: the call that starts a
//! background job or subagent, the calls that reach it later, and the
//! notice it posts when it ends each carry its id, such as `bash-1`, and
//! name it by the command or label its start recorded.
//!
//! Nothing here touches the terminal or the session: [`Projector::apply`]
//! takes one event and returns the view updates it causes.

use std::collections::HashMap;

use bake_agent::{AgentEvent, AgentMessage, AgentToolResult};
use bake_ai::{AssistantContentBlock, AssistantMessage, StopReason, UserContent, UserContentBlock};
use bake_coding_agent::agent_session::job_notices::NOTICE_TYPE;
use bake_coding_agent::agent_session::{
    BackgroundCompactionStatus, CompactionReason, SessionEvent,
};
use bake_coding_agent::schedules;
use bake_coding_agent::session::messages::CustomMessage;
use bake_coding_agent::subagents::SUBAGENT_DETAIL;
use bake_coding_agent::subagents::messages as subagents;
use bake_coding_agent::tools::bash::BACKGROUND_JOB_DETAIL;
use bake_coding_agent::tools::job_tools::STILL_RUNNING;
use bake_tui_view::copy;
use bake_tui_view::jobs::{self, JobState};
use bake_tui_view::runtime::{Pending, RuntimeUpdate, Target};
use bake_tui_view::transcript::{CallState, Row};
use serde_json::Value;

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
}

impl Outcome {
    fn running(output: Vec<String>) -> Self {
        Self {
            state: CallState::Running,
            id: None,
            summary: None,
            output,
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
                row.map(|row| vec![self.commit_now(row)])
                    .unwrap_or_default()
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
                vec![self.commit_now(Row::Error(error_message.clone()))]
            }
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
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
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
                partial_result,
                ..
            } => {
                let lines = result_lines(&partial_result.content, partial_result.details.as_ref());
                self.update_call(tool_call_id, Outcome::running(lines));
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
        if let Row::Call {
            state,
            id,
            summary,
            output,
            ..
        } = &mut self.rows[index]
        {
            *state = outcome.state;
            *output = outcome.output;
            if outcome.id.is_some() {
                *id = outcome.id;
            }
            if !running {
                *summary = outcome.summary;
            }
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
            if let Row::Call { state, summary, .. } = row
                && *state == CallState::Running
            {
                *state = CallState::Failed;
                *summary = Some(INTERRUPTED.into());
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

    fn set_phase(&mut self, phase: &str) -> Vec<RuntimeUpdate> {
        if self.phase.as_deref() == Some(phase) {
            return Vec::new();
        }
        self.phase = Some(phase.to_owned());
        vec![RuntimeUpdate::Phase(phase.to_owned())]
    }
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
                    (None, Some(failure)) => Outcome {
                        state: CallState::Failed,
                        id: None,
                        summary: None,
                        output: vec![failure.to_owned()],
                    },
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

/// A tool call's card: its title, its main argument, the job or agent it
/// reached, and its outcome. A running `job_output` that waits says so.
fn call_row(tool: &str, args: &Value, labels: &Labels, outcome: Outcome) -> Row {
    let waits = tool == "job_output" && args.get("wait").and_then(Value::as_bool) == Some(true);
    let summary = outcome.summary.or_else(|| {
        (outcome.state == CallState::Running && waits).then(|| copy::WAITING.to_owned())
    });
    Row::Call {
        tool: display_name(tool),
        argument: argument(tool, args, labels),
        id: outcome.id.or_else(|| target(tool, args)),
        state: outcome.state,
        summary,
        output: outcome.output,
    }
}

/// A tool's card title: its entry in [`copy::TOOL_TITLES`], or its name
/// with the first letter capitalized.
fn display_name(tool: &str) -> String {
    if let Some((_, title)) = copy::TOOL_TITLES.iter().find(|(name, _)| *name == tool) {
        return (*title).to_owned();
    }
    let mut chars = tool.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The background job or subagent a call names in its arguments.
fn target(tool: &str, args: &Value) -> Option<String> {
    let key = match tool {
        "job_output" | "job_kill" => "job_id",
        "send_message" | "interrupt_agent" => "agent_id",
        _ => return None,
    };
    args.get(key)
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// The argument a card shows: the command, the path, or the pattern Pi's
/// tool renderers lead with; for a call that reaches a background job or
/// subagent, its label, since its id is tagged; other tools show their
/// arguments as JSON.
fn argument(tool: &str, args: &Value, labels: &Labels) -> String {
    let field = |name: &str| args.get(name).and_then(Value::as_str).map(str::to_owned);
    let label = || {
        Some(
            target(tool, args)
                .and_then(|id| labels.get(&id).cloned())
                .unwrap_or_default(),
        )
    };
    let lead = match tool {
        "bash" => field("command"),
        "read" | "write" | "edit" => field("path"),
        "ls" => Some(field("path").unwrap_or_else(|| ".".into())),
        "grep" | "find" => field("pattern").map(|pattern| match field("path") {
            Some(path) => format!("{pattern} in {path}"),
            None => pattern,
        }),
        // 0.3 `presentation.ts`: a subagent by its label and route, a
        // message by its first line.
        "subagent" => field("description").map(|label| {
            let label = clip(first_line(&label), 80);
            match (field("provider"), field("model")) {
                (Some(provider), Some(model)) => format!("{label} · {provider}/{model}"),
                _ => label,
            }
        }),
        "send_message" => Some(clip(first_line(&field("message").unwrap_or_default()), 48)),
        // 0.3 `tool-web`: a fetch by its URL, a search by its queries.
        "web_fetch" => field("url"),
        "web_search" => args
            .get("queries")
            .and_then(Value::as_array)
            .map(|queries| {
                queries
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            }),
        "job_output" | "job_kill" | "interrupt_agent" => label(),
        "list_agents" | "list_subagent_models" => Some(field("agent_id").unwrap_or_else(|| {
            [field("provider"), field("model"), field("scope")]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("/")
        })),
        _ => None,
    };
    lead.unwrap_or_else(|| match args {
        Value::Object(map) if map.is_empty() => String::new(),
        other => other.to_string(),
    })
}

/// Records the label of the background job or subagent a result started.
fn learn(labels: &mut Labels, details: Option<&Value>) {
    let Some(details) = details else {
        return;
    };
    for (key, id) in [(BACKGROUND_JOB_DETAIL, "id"), (SUBAGENT_DETAIL, "agentId")] {
        let field = |name| details.get(key)?.get(name)?.as_str();
        if let (Some(id), Some(label)) = (field(id), field("label")) {
            labels.insert(id.to_owned(), label.to_owned());
        }
    }
}

/// A settled call's outcome from its result.
fn settled(tool: &str, failed: bool, result: &AgentToolResult) -> Outcome {
    outcome(tool, failed, &result.content, result.details.as_ref())
}

/// What a settled call's card shows. A call that started a background job
/// or subagent is tagged with its id and says it went to the background; a
/// call that reached one says what came of it, such as `exit 0` or
/// `delivered`, rather than repeating the text the model reads. A failed
/// call shows its error as it came.
fn outcome(
    tool: &str,
    failed: bool,
    content: &[UserContentBlock],
    details: Option<&Value>,
) -> Outcome {
    let mut outcome = Outcome {
        state: if failed {
            CallState::Failed
        } else {
            CallState::Done
        },
        id: None,
        summary: None,
        output: result_lines(content, details),
    };
    let record = |key: &str| details.and_then(|details| details.get(key));
    let started = |key: &str, id: &str| {
        record(key)
            .and_then(|record| record.get(id))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
    };
    let background = |outcome: &mut Outcome| {
        if !failed {
            outcome.summary = Some(copy::BACKGROUND.to_owned());
            outcome.output.clear();
        }
    };
    match tool {
        "bash" => {
            outcome.id = started(BACKGROUND_JOB_DETAIL, "id");
            if outcome.id.is_some() {
                background(&mut outcome);
            }
        }
        "subagent" => {
            outcome.id = started(SUBAGENT_DETAIL, "agentId");
            let foreground = record(SUBAGENT_DETAIL)
                .and_then(|record| record.get("foreground"))
                .and_then(Value::as_bool)
                == Some(true);
            if outcome.id.is_some() && !foreground {
                background(&mut outcome);
            }
        }
        _ if failed => {}
        "job_output" => {
            if outcome
                .output
                .last()
                .is_some_and(|line| line == STILL_RUNNING)
            {
                outcome.output.pop();
            }
            if let Some(state) = outcome.output.last().and_then(|line| job_state(line)) {
                outcome.output.pop();
                outcome.summary = Some(jobs::word(&state));
            }
            if outcome.output == ["(no new output)"] {
                outcome.output.clear();
            }
        }
        "job_kill" => {
            let text = outcome.output.join("\n");
            let finished = text
                .rfind("[status: ")
                .and_then(|at| job_state(&text[at..]));
            outcome.summary = Some(match finished {
                Some(state) => format!(
                    "{}{}{}",
                    copy::ALREADY_FINISHED,
                    copy::STATUS_SEP,
                    jobs::word(&state)
                ),
                None => copy::JOB_STOPPING.to_owned(),
            });
            outcome.output.clear();
        }
        "interrupt_agent" | "send_message" => {
            let word = if tool == "send_message" {
                copy::DELIVERED
            } else {
                copy::JOB_STOPPING
            };
            outcome.summary = Some(word.to_owned());
            outcome.output.clear();
        }
        // An empty listing, `(no background jobs)`, says only that.
        "job_list" | "list_agents" => {
            if let [only] = outcome.output.as_slice()
                && only.starts_with('(')
                && only.ends_with(')')
            {
                outcome.summary = Some(copy::NONE.to_owned());
                outcome.output.clear();
            }
        }
        _ => {}
    }
    outcome
}

/// The job state a `[status: …]` line names, such as `[status: completed,
/// exit code: 0]`.
fn job_state(line: &str) -> Option<JobState> {
    let inner = line.strip_prefix("[status: ")?.strip_suffix(']')?;
    let (status, detail) = match inner.split_once(", ") {
        Some((status, detail)) => (status, Some(detail.to_owned())),
        None => (inner, None),
    };
    Some(match status {
        "running" => JobState::Running,
        "stopping" => JobState::Stopping,
        "completed" => JobState::Completed(detail),
        "killed" => JobState::Killed(detail),
        "failed" => JobState::Failed(detail),
        _ => return None,
    })
}

/// `text`'s first nonblank line with runs of whitespace collapsed, and
/// ` …` when more lines follow.
fn first_line(text: &str) -> String {
    let mut lines = text
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty());
    let first = lines.next().unwrap_or_default();
    if lines.next().is_some() {
        format!("{first} …")
    } else {
        first
    }
}

/// `text` kept to `limit` characters, the last one an ellipsis when cut.
fn clip(text: String, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text;
    }
    let kept: String = text.chars().take(limit.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// A result's lines: its text, an image as a note, and an edit's unified
/// patch, which the card draws as a diff.
fn result_lines(content: &[UserContentBlock], details: Option<&Value>) -> Vec<String> {
    if let Some(patch) = details
        .and_then(|details| details.get("patch"))
        .and_then(Value::as_str)
    {
        // The hunks only: the card numbers lines from their headers.
        let lines: Vec<String> = patch
            .lines()
            .skip_while(|line| !line.starts_with("@@"))
            .map(str::to_owned)
            .collect();
        if !lines.is_empty() {
            return lines;
        }
    }
    let mut lines = Vec::new();
    for block in content {
        match block {
            UserContentBlock::Text(text) => {
                lines.extend(text.text.trim_end().lines().map(str::to_owned));
            }
            UserContentBlock::Image(image) => lines.push(format!("[image: {}]", image.mime_type)),
        }
    }
    lines
}

/// The transcript a resumed session opens with, Pi's `renderSessionContext`
/// over the restored messages: prompts, responses with their calls settled
/// by their results, notices paired with the work they report, and
/// compaction summaries as a note.
pub fn replay(messages: &[AgentMessage]) -> Vec<Row> {
    let labels = Projector::with_history(messages).labels;
    let settled: HashMap<String, Outcome> = messages
        .iter()
        .filter_map(AgentMessage::as_tool_result)
        .map(|result| {
            (
                result.tool_call_id.clone(),
                outcome(
                    &result.tool_name,
                    result.is_error,
                    &result.content,
                    result.details.as_ref(),
                ),
            )
        })
        .collect();
    let mut rows = Vec::new();
    for message in messages {
        if let Some(user) = message.as_user() {
            rows.push(Row::User(user_text(&user.content)));
        } else if let Some(assistant) = message.as_assistant() {
            let failure = match assistant.stop_reason {
                StopReason::Aborted => Some(ABORTED.to_owned()),
                StopReason::Error => Some(
                    assistant
                        .error_message
                        .clone()
                        .filter(|message| !message.is_empty())
                        .unwrap_or_else(|| "Error".into()),
                ),
                _ => None,
            };
            let (mut assistant_rows, _) = assistant_rows(
                assistant,
                &settled,
                &labels,
                Some(failure.as_deref().unwrap_or(INTERRUPTED)),
            );
            for row in &mut assistant_rows {
                // A call with no result in the log never finished.
                if let Row::Call {
                    state: CallState::Failed,
                    output,
                    summary,
                    ..
                } = row
                    && output.len() == 1
                    && output[0] == INTERRUPTED
                {
                    output.clear();
                    *summary = Some(INTERRUPTED.into());
                }
            }
            rows.extend(assistant_rows);
            if let Some(failure) = failure {
                rows.push(Row::Error(failure));
            }
        } else if message.role() == "compactionSummary" {
            rows.push(Row::Info("Earlier conversation was compacted".into()));
        } else if message.role() == "branchSummary" {
            rows.push(Row::branch_summary());
        } else if let Some(row) = notice_row(message, &labels) {
            rows.push(row);
        }
    }
    rows
}

/// The transcript after `/compact`, Pi's `compaction_end` rendering: the
/// kept history the compacted context holds, without the latest summary,
/// which the caller notes below it at its place in time. Without a summary
/// it is [`replay`].
pub fn replay_compacted(messages: &[AgentMessage]) -> Vec<Row> {
    match messages
        .iter()
        .position(|message| message.role() == "compactionSummary")
    {
        Some(index) => {
            let mut kept = messages.to_vec();
            kept.remove(index);
            replay(&kept)
        }
        None => replay(messages),
    }
}

/// The row of a notice: a background job's notice ([`NOTICE_TYPE`]) and a
/// subagent's settlement as the cards of the work they report
/// ([`job_notice`], [`agent_notice`]), or as information when their text
/// cannot be read that way; a subagent's message as `← agent {id}: {first
/// line}`, its first line kept to 48 characters as 0.3's `presentation.ts`
/// keeps titles; and a schedule reminder as `reminder {id}: {first line}`,
/// or a batch as `reminders {id}, {id}`.
fn notice_row(message: &AgentMessage, labels: &Labels) -> Option<Row> {
    let custom = message.as_custom()?;
    let custom = custom.as_any().downcast_ref::<CustomMessage>()?;
    let text = user_text(&custom.content);
    let details = custom.details.as_ref();
    let info =
        match custom.custom_type.as_str() {
            NOTICE_TYPE => return Some(job_notice(&text).unwrap_or(Row::Info(text))),
            subagents::NOTICE_TYPE => {
                return Some(agent_notice(&text, details, labels).unwrap_or_else(|| {
                    Row::Info(text.lines().next().unwrap_or_default().to_owned())
                }));
            }
            subagents::MESSAGE_TYPE => {
                let from = details
                    .and_then(|details| details.get("from"))
                    .and_then(Value::as_str)?;
                let body = text
                    .strip_prefix(&format!("Agent {from} sent a message: "))
                    .unwrap_or(&text);
                format!("← agent {from}: {}", title(body))
            }
            schedules::REMINDER_TYPE => {
                let reminders = details
                    .and_then(|details| details.get("reminders"))
                    .and_then(Value::as_array)?;
                match reminders.as_slice() {
                    [one] => format!(
                        "reminder {}: {}",
                        one.get("id").and_then(Value::as_str)?,
                        title(one.get("prompt").and_then(Value::as_str)?)
                    ),
                    many => format!(
                        "reminders {}",
                        many.iter()
                            .filter_map(|reminder| reminder.get("id").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                }
            }
            _ => return None,
        };
    Some(Row::Info(info))
}

/// A background job's notice as the card of the job, 0.3's `jobDone`: the
/// title of its kind, its label, its id, and how it ended, read from
/// `background job {id} ({kind}: {label}) … [status: {status}]`. A job an
/// earlier process ran says so.
fn job_notice(text: &str) -> Option<Row> {
    let rest = text.strip_prefix("background job ")?;
    let (id, rest) = rest.split_once(" (")?;
    let (kind, rest) = rest.split_once(": ")?;
    let at = rest.rfind("[status: ")?;
    // A label can hold `) ` itself; the last one before the status ends it.
    let end = rest[..at].rfind(") ")?;
    let label = &rest[..end];
    let earlier = rest[end..at].contains("earlier process");
    let status = &rest[at..=at + rest[at..].rfind(']')?];
    let (state, word) = if status == "[status: lost]" {
        (CallState::Failed, copy::JOB_LOST.to_owned())
    } else {
        let job = job_state(status)?;
        let state = if job.failed() {
            CallState::Failed
        } else {
            CallState::Done
        };
        (state, jobs::word(&job))
    };
    let summary = if earlier {
        format!("{word}{}{}", copy::STATUS_SEP, copy::EARLIER_PROCESS)
    } else {
        word
    };
    Some(Row::Call {
        tool: display_name(kind),
        argument: label.to_owned(),
        id: Some(id.to_owned()),
        state,
        summary: Some(summary),
        output: Vec::new(),
    })
}

/// A subagent's settlement as the card of the agent: its label, its id,
/// how its run ended, and its closing message as the output.
fn agent_notice(text: &str, details: Option<&Value>, labels: &Labels) -> Option<Row> {
    let field = |name| details?.get(name)?.as_str();
    let id = field("agentId")?;
    let (state, word) = match subagents::StopReason::parse(field("stopReason")?)? {
        subagents::StopReason::Completed => (CallState::Done, copy::JOB_DONE),
        subagents::StopReason::Aborted => (CallState::Done, copy::JOB_STOPPED),
        subagents::StopReason::MaxTokens => (CallState::Failed, copy::TOKEN_LIMIT),
        subagents::StopReason::Error => (CallState::Failed, copy::JOB_FAILED),
        subagents::StopReason::Interrupted => (CallState::Failed, copy::INTERRUPTED_NOTE),
    };
    let output = text
        .split_once("\nIts closing message:\n")
        .map(|(_, closing)| closing.trim_end().lines().map(str::to_owned).collect())
        .unwrap_or_default();
    Some(Row::Call {
        tool: display_name("subagent"),
        argument: labels.get(id).cloned().unwrap_or_default(),
        id: Some(id.to_owned()),
        state,
        summary: Some(word.to_owned()),
        output,
    })
}

/// A notice's first line, kept to 48 characters.
fn title(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    let mut title: String = line.chars().take(48).collect();
    if title.len() < line.len() {
        title.push('…');
    }
    title
}

#[cfg(test)]
mod tests {
    use bake_ai::providers::faux::{faux_assistant_blocks, faux_text, faux_tool_call};
    use bake_ai::{Message, TextContent, ToolResultMessage, UserMessage};
    use serde_json::json;

    use super::*;

    #[test]
    fn background_commit_reports_context_without_replacing_foreground_phase() {
        let mut projector = Projector {
            phase: Some("running".into()),
            ..Projector::default()
        };
        let result = bake_coding_agent::compaction::CompactionResult {
            summary: "summary".into(),
            first_kept_entry_id: "kept".into(),
            tokens_before: 8000,
            estimated_tokens_after: Some(1000),
            usage: None,
            details: None,
        };
        let started = SessionEvent::BackgroundCompaction {
            status: BackgroundCompactionStatus::Started,
            source_leaf_id: "source",
            result: None,
            reason: None,
            elapsed_ms: 0,
            attempts: &[],
        };
        assert!(projector.apply(&started).is_empty());
        let committed = SessionEvent::BackgroundCompaction {
            status: BackgroundCompactionStatus::Committed,
            source_leaf_id: "source",
            result: Some(&result),
            reason: None,
            elapsed_ms: 10,
            attempts: &[],
        };
        assert_eq!(
            projector.apply(&committed),
            vec![RuntimeUpdate::Commit(vec![Row::Info(
                "Compacted the conversation in the background (8000 tokens before)".into()
            )])]
        );
        assert_eq!(projector.phase.as_deref(), Some("running"));
    }

    fn user(text: &str) -> AgentMessage {
        Message::User(UserMessage {
            content: UserContent::Text(text.into()),
            timestamp: 0,
        })
        .into()
    }

    fn result(id: &str, text: &str, is_error: bool, details: Option<Value>) -> AgentMessage {
        tool_result(id, "bash", text, is_error, details)
    }

    fn tool_result(
        id: &str,
        tool: &str,
        text: &str,
        is_error: bool,
        details: Option<Value>,
    ) -> AgentMessage {
        Message::ToolResult(ToolResultMessage {
            tool_call_id: id.into(),
            tool_name: tool.into(),
            content: vec![UserContentBlock::Text(TextContent::new(text))],
            details,
            usage: None,
            nested_calls: None,
            is_error,
            timestamp: 0,
            duration_ms: None,
        })
        .into()
    }

    #[test]
    fn replay_settles_calls_by_their_results_and_marks_unfinished_ones() {
        let assistant = faux_assistant_blocks(
            vec![
                faux_text("Looking."),
                faux_tool_call("bash", json!({"command": "ls"}), Some("a")),
                faux_tool_call("edit", json!({"path": "src/x.rs"}), Some("b")),
                faux_tool_call("read", json!({"path": "gone.txt"}), Some("c")),
            ],
            StopReason::ToolUse,
        );
        let patch =
            "Index: src/x.rs\n===\n--- src/x.rs\n+++ src/x.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n";
        let messages = vec![
            user("go"),
            Message::Assistant(assistant).into(),
            result("a", "one\ntwo\n", false, None),
            result("b", "Edited", false, Some(json!({"patch": patch}))),
        ];
        let rows = replay(&messages);
        assert_eq!(
            rows,
            [
                Row::User("go".into()),
                Row::Answer("Looking.".into()),
                Row::Call {
                    tool: "Bash".into(),
                    argument: "ls".into(),
                    id: None,
                    state: CallState::Done,
                    summary: None,
                    output: vec!["one".into(), "two".into()],
                },
                Row::Call {
                    tool: "Edit".into(),
                    argument: "src/x.rs".into(),
                    id: None,
                    state: CallState::Done,
                    summary: None,
                    output: vec!["@@ -1,1 +1,1 @@".into(), "-old".into(), "+new".into()],
                },
                Row::Call {
                    tool: "Read".into(),
                    argument: "gone.txt".into(),
                    id: None,
                    state: CallState::Failed,
                    summary: Some(INTERRUPTED.into()),
                    output: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn a_failed_response_fails_its_unrun_calls_with_its_error() {
        let mut failed = faux_assistant_blocks(
            vec![faux_tool_call(
                "grep",
                json!({"pattern": "x", "path": "src"}),
                Some("a"),
            )],
            StopReason::Error,
        );
        failed.error_message = Some("rate limited".into());
        let mut projector = Projector::default();
        let updates = projector.apply(&SessionEvent::Agent(&AgentEvent::MessageEnd {
            message: Message::Assistant(failed.clone()).into(),
        }));
        let failed_call = Row::Call {
            tool: "Grep".into(),
            argument: "x in src".into(),
            id: None,
            state: CallState::Failed,
            summary: None,
            output: vec!["rate limited".into()],
        };
        assert_eq!(
            updates,
            [RuntimeUpdate::Live(vec![
                failed_call.clone(),
                Row::Error("rate limited".into())
            ])]
        );
        assert_eq!(projector.last_stop(), Some(StopReason::Error));
        assert_eq!(
            replay(&[Message::Assistant(failed).into()]),
            [failed_call, Row::Error("rate limited".into())]
        );
    }

    #[test]
    fn a_user_message_marks_the_prompt_admitted_and_commits_once() {
        let mut projector = Projector::default();
        projector.begin(Some(7));
        assert!(!projector.admitted());
        let updates = projector.apply(&SessionEvent::Agent(&AgentEvent::MessageStart {
            message: user("hello"),
        }));
        assert_eq!(
            updates,
            [
                RuntimeUpdate::Admitted(7),
                RuntimeUpdate::Commit(vec![Row::User("hello".into())])
            ]
        );
        assert!(projector.admitted());
        // A steer's message later in the prompt is not the prompt's admission.
        let updates = projector.apply(&SessionEvent::Agent(&AgentEvent::MessageStart {
            message: user("steer"),
        }));
        assert_eq!(
            updates,
            [RuntimeUpdate::Commit(vec![Row::User("steer".into())])]
        );
        // A turn's end commits what was live, failing a call left running.
        projector.apply(&SessionEvent::Agent(&AgentEvent::ToolExecutionStart {
            tool_call_id: "x".into(),
            tool_name: "bash".into(),
            args: json!({"command": "sleep 9"}),
        }));
        let updates = projector.finish();
        assert_eq!(
            updates,
            [RuntimeUpdate::Commit(vec![Row::Call {
                tool: "Bash".into(),
                argument: "sleep 9".into(),
                id: None,
                state: CallState::Failed,
                summary: Some(INTERRUPTED.into()),
                output: Vec::new(),
            }])]
        );
        assert_eq!(projector.finish(), []);
    }

    #[test]
    fn pending_lists_steering_first_then_the_next_turn() {
        assert_eq!(
            pending(&["s".into()], &["f".into()], &["w".into()]),
            [
                Pending {
                    text: "s".into(),
                    target: Target::NextStep
                },
                Pending {
                    text: "f".into(),
                    target: Target::NextTurn
                },
                Pending {
                    text: "w".into(),
                    target: Target::NextTurn
                },
            ]
        );
    }

    fn custom(message: CustomMessage) -> AgentMessage {
        AgentMessage::Custom(std::sync::Arc::new(message))
    }

    fn info(row: Option<Row>) -> Option<String> {
        match row? {
            Row::Info(text) => Some(text),
            _ => None,
        }
    }

    fn card(
        tool: &str,
        argument: &str,
        id: Option<&str>,
        state: CallState,
        summary: Option<&str>,
        output: &[&str],
    ) -> Row {
        Row::Call {
            tool: tool.into(),
            argument: argument.into(),
            id: id.map(Into::into),
            state,
            summary: summary.map(Into::into),
            output: output.iter().map(|line| (*line).into()).collect(),
        }
    }

    fn labels(pairs: &[(&str, &str)]) -> Labels {
        pairs
            .iter()
            .map(|(id, label)| ((*id).to_owned(), (*label).to_owned()))
            .collect()
    }

    /// A job notice as `completion_notice` and `recovered_completion` write
    /// it, which need a registry's job id to call.
    fn job_notice_message(text: &str) -> AgentMessage {
        custom(CustomMessage {
            custom_type: NOTICE_TYPE.into(),
            content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))]),
            display: true,
            details: Some(json!({ "jobId": "bash-1", "status": "completed" })),
            timestamp: 0,
        })
    }

    fn run(projector: &mut Projector, event: AgentEvent) -> Vec<RuntimeUpdate> {
        projector.apply(&SessionEvent::Agent(&event))
    }

    fn start(id: &str, tool: &str, args: Value) -> AgentEvent {
        AgentEvent::ToolExecutionStart {
            tool_call_id: id.into(),
            tool_name: tool.into(),
            args,
        }
    }

    fn end(id: &str, tool: &str, text: &str, details: Option<Value>) -> AgentEvent {
        AgentEvent::ToolExecutionEnd {
            tool_call_id: id.into(),
            tool_name: tool.into(),
            result: AgentToolResult {
                details,
                ..AgentToolResult::text(text)
            },
            is_error: false,
            duration_ms: None,
        }
    }

    /// The live rows the updates end with.
    fn live_rows(updates: Vec<RuntimeUpdate>) -> Vec<Row> {
        updates
            .into_iter()
            .rev()
            .find_map(|update| match update {
                RuntimeUpdate::Live(rows) => Some(rows),
                _ => None,
            })
            .expect("a live update")
    }

    fn phases(updates: &[RuntimeUpdate]) -> Vec<&str> {
        updates
            .iter()
            .filter_map(|update| match update {
                RuntimeUpdate::Phase(phase) => Some(phase.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_subagent_settlement_is_the_card_of_the_agent_it_reports() {
        use bake_coding_agent::subagents::messages::{
            StopReason as Stop, agent_message, settlement_notice,
        };
        let known = labels(&[("child-1", "read the docs")]);
        let notice = |stop, closing| custom(settlement_notice("child-1", 1, stop, closing));
        let agent = |state, word, output: &[&str]| {
            Some(card(
                "Agent",
                "read the docs",
                Some("child-1"),
                state,
                Some(word),
                output,
            ))
        };
        assert_eq!(
            notice_row(&notice(Stop::Completed, Some("long\nanswer\n")), &known),
            agent(CallState::Done, "done", &["long", "answer"])
        );
        assert_eq!(
            notice_row(&notice(Stop::Aborted, None), &known),
            agent(CallState::Done, "stopped", &[])
        );
        assert_eq!(
            notice_row(&notice(Stop::MaxTokens, Some("partial")), &known),
            agent(CallState::Failed, "token limit", &["partial"])
        );
        assert_eq!(
            notice_row(&notice(Stop::Error, None), &known),
            agent(CallState::Failed, "failed", &[])
        );
        assert_eq!(
            notice_row(&notice(Stop::Interrupted, None), &known),
            agent(CallState::Failed, "interrupted", &[])
        );
        // An agent the session never saw start has no label to show.
        assert_eq!(
            notice_row(&notice(Stop::Completed, None), &Labels::new()),
            Some(card(
                "Agent",
                "",
                Some("child-1"),
                CallState::Done,
                Some("done"),
                &[]
            ))
        );
        // Without its details the notice reads as its first line.
        let mut bare = settlement_notice("child-1", 1, Stop::Completed, Some("answer"));
        bare.details = None;
        assert_eq!(
            info(notice_row(&custom(bare), &known)).as_deref(),
            Some(
                "Background subagent child-1 finished and will do no further work unless you send it more."
            )
        );
        let message = agent_message(
            "m",
            "child-1",
            "parent",
            &format!("{}\nsecond line", "x".repeat(60)),
        );
        assert_eq!(
            info(notice_row(&custom(message), &known)).as_deref(),
            Some(format!("← agent child-1: {}…", "x".repeat(48)).as_str())
        );
    }

    #[test]
    fn calls_that_reach_a_job_or_agent_name_it_by_its_label_and_tag_its_id() {
        let known = labels(&[("bash-1", "sleep 30"), ("child-1", "read the docs")]);
        assert_eq!(
            argument(
                "subagent",
                &json!({ "description": "read\n a  file", "prompt": "p", "provider": "x", "model": "y" }),
                &known
            ),
            "read … · x/y"
        );
        let message = json!({ "agent_id": "c", "message": "hi\nthere" });
        assert_eq!(argument("send_message", &message, &known), "hi …");
        assert_eq!(target("send_message", &message).as_deref(), Some("c"));
        assert_eq!(argument("list_agents", &json!({}), &known), "");
        let output = json!({ "job_id": "bash-1" });
        assert_eq!(argument("job_output", &output, &known), "sleep 30");
        assert_eq!(target("job_output", &output).as_deref(), Some("bash-1"));
        assert_eq!(
            argument("job_kill", &json!({ "job_id": "bash-9" }), &known),
            ""
        );
        assert_eq!(
            argument("interrupt_agent", &json!({ "agent_id": "child-1" }), &known),
            "read the docs"
        );
        assert_eq!(target("job_list", &json!({})), None);
        assert_eq!(
            argument("web_search", &json!({ "queries": ["one", "two"] }), &known),
            "one, two"
        );
        assert_eq!(
            argument("web_fetch", &json!({ "url": "https://a.test/x" }), &known),
            "https://a.test/x"
        );
        assert_eq!(display_name("web_search"), "Web search");
        assert_eq!(display_name("job_output"), "Job output");
        assert_eq!(display_name("subagent"), "Agent");
        assert_eq!(display_name("bash"), "Bash");
    }

    #[test]
    fn background_work_is_tagged_from_its_start_to_what_came_of_it() {
        let mut projector = Projector::default();
        projector.begin(None);
        run(
            &mut projector,
            start(
                "a",
                "bash",
                json!({ "command": "sleep 30", "run_in_background": true }),
            ),
        );
        let rows = live_rows(run(
            &mut projector,
            end(
                "a",
                "bash",
                "Started background job bash-1.",
                Some(json!({ BACKGROUND_JOB_DETAIL: { "id": "bash-1", "label": "sleep 30" } })),
            ),
        ));
        assert_eq!(
            rows,
            [card(
                "Bash",
                "sleep 30",
                Some("bash-1"),
                CallState::Done,
                Some("background"),
                &[]
            )]
        );
        let waiting = |state, summary, output: &[&str]| {
            card(
                "Job output",
                "sleep 30",
                Some("bash-1"),
                state,
                summary,
                output,
            )
        };
        let wait = json!({ "job_id": "bash-1", "wait": true });
        let rows = live_rows(run(&mut projector, start("b", "job_output", wait.clone())));
        assert_eq!(rows[1], waiting(CallState::Running, Some("waiting"), &[]));
        // Partial output keeps the card waiting.
        let rows = live_rows(run(
            &mut projector,
            AgentEvent::ToolExecutionUpdate {
                tool_call_id: "b".into(),
                tool_name: "job_output".into(),
                args: wait,
                partial_result: AgentToolResult::text("tick"),
            },
        ));
        assert_eq!(
            rows[1],
            waiting(CallState::Running, Some("waiting"), &["tick"])
        );
        let rows = live_rows(run(
            &mut projector,
            end(
                "b",
                "job_output",
                &format!("hello\n[status: running]\n{STILL_RUNNING}"),
                None,
            ),
        ));
        assert_eq!(
            rows[1],
            waiting(CallState::Done, Some("running"), &["hello"])
        );
        // A read with nothing new says only how the job ended.
        run(
            &mut projector,
            start("c", "job_output", json!({ "job_id": "bash-1" })),
        );
        let rows = live_rows(run(
            &mut projector,
            end(
                "c",
                "job_output",
                "(no new output)\n[status: completed, exit code: 1]",
                None,
            ),
        ));
        assert_eq!(rows[2], waiting(CallState::Done, Some("exit 1"), &[]));
        // A foreground subagent's answer is its output, under its id.
        run(
            &mut projector,
            start("d", "subagent", json!({ "description": "look" })),
        );
        let rows = live_rows(run(
            &mut projector,
            end(
                "d",
                "subagent",
                "the answer",
                Some(
                    json!({ SUBAGENT_DETAIL: { "agentId": "child-1", "label": "look", "foreground": true } }),
                ),
            ),
        ));
        assert_eq!(
            rows[3],
            card(
                "Agent",
                "look",
                Some("child-1"),
                CallState::Done,
                None,
                &["the answer"]
            )
        );
        assert_eq!(
            projector.labels.get("child-1").map(String::as_str),
            Some("look")
        );
    }

    #[test]
    fn job_and_agent_calls_say_what_came_of_them() {
        let say = |tool: &str, text: &str| {
            let content = [UserContentBlock::Text(TextContent::new(text))];
            let outcome = outcome(tool, false, &content, None);
            (outcome.summary, outcome.output)
        };
        let said = |summary: &str| (Some(summary.to_owned()), Vec::<String>::new());
        assert_eq!(
            say("job_kill", "requested cancellation of job bash-1"),
            said("stopping")
        );
        assert_eq!(
            say(
                "job_kill",
                "job bash-1 had already finished [status: completed, exit code: 0]"
            ),
            said("already finished · exit 0")
        );
        assert_eq!(
            say(
                "job_kill",
                "job bash-1 had already finished [status: killed, signal: SIGTERM]"
            ),
            said("already finished · stopped")
        );
        assert_eq!(
            say("send_message", "message delivered to agent child-1"),
            said("delivered")
        );
        assert_eq!(
            say("interrupt_agent", "interrupt requested for agent child-1"),
            said("stopping")
        );
        assert_eq!(say("job_list", "(no background jobs)"), said("none"));
        assert_eq!(say("list_agents", "(no subagents)"), said("none"));
        assert_eq!(
            say("job_list", "bash-1 [bash] running — sleep 30"),
            (None, vec!["bash-1 [bash] running — sleep 30".to_owned()])
        );
        // A failed call keeps the error the model read.
        let content = [UserContentBlock::Text(TextContent::new(
            "unknown job bash-9",
        ))];
        let failed = outcome("job_kill", true, &content, None);
        assert_eq!(failed.state, CallState::Failed);
        assert_eq!(failed.summary, None);
        assert_eq!(failed.output, ["unknown job bash-9"]);
    }

    #[test]
    fn a_job_notice_is_the_card_of_the_job_it_reports() {
        use bake_coding_agent::agent_session::job_notices::lost_notice;
        let row = |text: &str| notice_row(&job_notice_message(text), &Labels::new());
        let bash = |label: &str, state, summary: &str| {
            Some(card(
                "Bash",
                label,
                Some("bash-1"),
                state,
                Some(summary),
                &[],
            ))
        };
        assert_eq!(
            row(
                "background job bash-1 (bash: sleep 30) finished [status: completed, exit code: 0]. Read its output with job_output."
            ),
            bash("sleep 30", CallState::Done, "exit 0")
        );
        assert_eq!(
            row(
                "background job bash-1 (bash: make) finished [status: completed, exit code: 2]. Read its output with job_output."
            ),
            bash("make", CallState::Failed, "exit 2")
        );
        assert_eq!(
            row(
                "background job bash-1 (bash: sleep 30) finished [status: killed]. The user stopped it. Read its output with job_output."
            ),
            bash("sleep 30", CallState::Done, "stopped")
        );
        // A label may hold `) ` and brackets of its own.
        assert_eq!(
            row(
                "background job bash-1 (bash: (cd x) && ls [a]) finished [status: failed, spawn error]. Read its output with job_output."
            ),
            bash("(cd x) && ls [a]", CallState::Failed, "failed")
        );
        assert_eq!(
            row(
                "background job bash-1 (bash: sleep 30) finished in its earlier process [status: completed, exit code: 0]. Its output was not retained across processes and is unavailable."
            ),
            bash("sleep 30", CallState::Done, "exit 0 · earlier process")
        );
        assert_eq!(
            notice_row(
                &custom(lost_notice("bash-1", "bash", "sleep 30")),
                &Labels::new()
            ),
            bash(
                "sleep 30",
                CallState::Failed,
                "outcome unknown · earlier process"
            )
        );
        // Text it cannot read stays information.
        assert_eq!(
            info(row("background job bash-1 finished")).as_deref(),
            Some("background job bash-1 finished")
        );
    }

    #[test]
    fn a_resumed_session_names_its_background_work_by_label() {
        let call = faux_assistant_blocks(
            vec![
                faux_tool_call(
                    "bash",
                    json!({"command": "sleep 30", "run_in_background": true}),
                    Some("a"),
                ),
                faux_tool_call("job_kill", json!({"job_id": "bash-1"}), Some("b")),
            ],
            StopReason::ToolUse,
        );
        let kill = tool_result(
            "b",
            "job_kill",
            "requested cancellation of job bash-1",
            false,
            None,
        );
        let messages = vec![
            user("go"),
            Message::Assistant(call).into(),
            result(
                "a",
                "Started background job bash-1.",
                false,
                Some(json!({ BACKGROUND_JOB_DETAIL: { "id": "bash-1", "label": "sleep 30" } })),
            ),
            kill,
            job_notice_message(
                "background job bash-1 (bash: sleep 30) finished [status: killed]. Read its output with job_output.",
            ),
        ];
        let tagged = |tool, summary| {
            card(
                tool,
                "sleep 30",
                Some("bash-1"),
                CallState::Done,
                Some(summary),
                &[],
            )
        };
        assert_eq!(
            replay(&messages),
            [
                Row::User("go".into()),
                tagged("Bash", "background"),
                tagged("Stop job", "stopping"),
                tagged("Bash", "stopped"),
            ]
        );
        // A live session resumed on them names the job too.
        let projector = Projector::with_history(&messages);
        assert_eq!(
            argument(
                "job_output",
                &json!({"job_id": "bash-1"}),
                &projector.labels
            ),
            "sleep 30"
        );
    }

    #[test]
    fn the_phase_counts_the_calls_still_running() {
        let mut projector = Projector::default();
        projector.begin(None);
        let mut phase = |event| phases(&run(&mut projector, event)).join("|");
        assert_eq!(
            phase(start("a", "bash", json!({"command": "sleep 1"}))),
            "running bash"
        );
        assert_eq!(
            phase(start("b", "read", json!({"path": "x"}))),
            "running bash +1"
        );
        assert_eq!(phase(end("b", "read", "x", None)), "running bash");
        assert_eq!(phase(end("a", "bash", "done", None)), "thinking");
    }

    #[test]
    fn schedule_reminders_are_information_rows() {
        use bake_coding_agent::schedules::{Due, Record, Rule, delivery};
        let record = |id: &str, prompt: &str| Record {
            id: id.into(),
            rule: Rule::Every { seconds: 300 },
            prompt: prompt.into(),
            scheduled_at: 0,
        };
        let row = |due: Due| {
            let (message, _) = delivery(&due).expect("a delivery");
            info(notice_row(&custom(message), &Labels::new()))
        };
        assert_eq!(
            row(Due::OneShot(record(
                "schedule-1",
                &format!(
                    "{}
more",
                    "y".repeat(50)
                )
            )))
            .as_deref(),
            Some(format!("reminder schedule-1: {}…", "y".repeat(48)).as_str())
        );
        assert_eq!(
            row(Due::Every {
                reminders: vec![
                    (record("schedule-1", "a"), 0),
                    (record("schedule-2", "b"), 0)
                ],
                accepted_at: 0,
            })
            .as_deref(),
            Some("reminders schedule-1, schedule-2")
        );
    }
}
