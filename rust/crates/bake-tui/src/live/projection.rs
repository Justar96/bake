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
//! Nothing here touches the terminal or the session: [`Projector::apply`]
//! takes one event and returns the view updates it causes.

use std::collections::HashMap;

use bake_agent::{AgentEvent, AgentMessage, AgentToolResult};
use bake_ai::{AssistantContentBlock, AssistantMessage, StopReason, UserContent, UserContentBlock};
use bake_coding_agent::agent_session::job_notices::NOTICE_TYPE;
use bake_coding_agent::agent_session::{CompactionReason, SessionEvent};
use bake_coding_agent::session::messages::CustomMessage;
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
    settled: HashMap<String, (CallState, Vec<String>)>,
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
}

impl Projector {
    /// Starts tracking a new prompt, which has the composer's `id` when the
    /// composer sent it.
    pub fn begin(&mut self, id: Option<u64>) {
        self.admitted = false;
        self.prompt_id = id;
        self.last_stop = None;
        self.phase = None;
    }

    /// Whether the prompt since [`Projector::begin`] reached the agent.
    pub fn admitted(&self) -> bool {
        self.admitted
    }

    /// How the last response of the prompt stopped.
    pub fn last_stop(&self) -> Option<StopReason> {
        self.last_stop
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
            AgentEvent::MessageStart { message } if notice_text(message).is_some() => {
                // A background job's notice, in a run it opened or one
                // already streaming.
                let mut updates: Vec<RuntimeUpdate> = Vec::new();
                if !self.admitted {
                    self.admitted = true;
                    updates.extend(self.prompt_id.take().map(RuntimeUpdate::Admitted));
                }
                updates.extend(self.settle());
                updates.extend(
                    notice_text(message).map(|text| RuntimeUpdate::Commit(vec![Row::Info(text)])),
                );
                updates
            }
            AgentEvent::MessageStart { message } => match message.as_user() {
                Some(user) => {
                    let mut updates: Vec<RuntimeUpdate> = Vec::new();
                    if !self.admitted {
                        self.admitted = true;
                        updates.extend(self.prompt_id.take().map(RuntimeUpdate::Admitted));
                    }
                    // Steering arrives between turns, after the last one
                    // committed; anything still live settles first.
                    updates.extend(self.settle());
                    updates.push(RuntimeUpdate::Commit(vec![Row::User(user_text(
                        &user.content,
                    ))]));
                    updates
                }
                None => Vec::new(),
            },
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
                let mut updates = self.set_phase(&format!("running {tool_name}"));
                let row = call_row(tool_name, args, CallState::Running, Vec::new());
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
                self.update_call(tool_call_id, CallState::Running, partial_result);
                vec![self.live()]
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                result,
                is_error,
                ..
            } => {
                let state = if *is_error {
                    CallState::Failed
                } else {
                    CallState::Done
                };
                self.update_call(tool_call_id, state, result);
                let mut updates = vec![self.live()];
                updates.extend(self.set_phase("thinking"));
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
        let (rows, calls) = assistant_rows(assistant, &self.settled, failure.as_deref());
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

    fn update_call(&mut self, id: &str, state: CallState, result: &AgentToolResult) {
        let Some(&index) = self.calls.get(id) else {
            return;
        };
        let lines = result_lines(&result.content, result.details.as_ref());
        if state != CallState::Running {
            self.settled.insert(id.to_owned(), (state, lines.clone()));
        }
        if let Row::Call {
            state: row_state,
            output,
            ..
        } = &mut self.rows[index]
        {
            *row_state = state;
            *output = lines;
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
            return Vec::new();
        }
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
    settled: &HashMap<String, (CallState, Vec<String>)>,
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
                let (state, output) = match (settled.get(&call.id), failure) {
                    (Some((state, output)), _) => (*state, output.clone()),
                    (None, Some(failure)) => (CallState::Failed, vec![failure.to_owned()]),
                    (None, None) => (CallState::Running, Vec::new()),
                };
                calls.insert(call.id.clone(), rows.len());
                rows.push(call_row(&call.name, &args, state, output));
            }
            _ => {}
        }
    }
    (rows, calls)
}

/// A tool call's card: its name, its main argument, and its output.
fn call_row(tool: &str, args: &Value, state: CallState, output: Vec<String>) -> Row {
    Row::Call {
        tool: display_name(tool),
        argument: argument(tool, args),
        state,
        summary: None,
        output,
    }
}

/// A tool's name with its first letter capitalized, as the cards title it.
fn display_name(tool: &str) -> String {
    let mut chars = tool.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The argument a card shows: the command, the path, or the pattern Pi's
/// tool renderers lead with; other tools show their arguments as JSON.
fn argument(tool: &str, args: &Value) -> String {
    let field = |name: &str| args.get(name).and_then(Value::as_str).map(str::to_owned);
    let lead = match tool {
        "bash" => field("command"),
        "read" | "write" | "edit" => field("path"),
        "ls" => Some(field("path").unwrap_or_else(|| ".".into())),
        "grep" | "find" => field("pattern").map(|pattern| match field("path") {
            Some(path) => format!("{pattern} in {path}"),
            None => pattern,
        }),
        _ => None,
    };
    lead.unwrap_or_else(|| match args {
        Value::Object(map) if map.is_empty() => String::new(),
        other => other.to_string(),
    })
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
/// by their results, and compaction summaries as a note.
pub fn replay(messages: &[AgentMessage]) -> Vec<Row> {
    let mut results: HashMap<&str, (CallState, Vec<String>)> = HashMap::new();
    for message in messages {
        if let Some(result) = message.as_tool_result() {
            let state = if result.is_error {
                CallState::Failed
            } else {
                CallState::Done
            };
            results.insert(
                result.tool_call_id.as_str(),
                (
                    state,
                    result_lines(&result.content, result.details.as_ref()),
                ),
            );
        }
    }
    let settled: HashMap<String, (CallState, Vec<String>)> = results
        .into_iter()
        .map(|(id, result)| (id.to_owned(), result))
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
        } else if let Some(text) = notice_text(message) {
            rows.push(Row::Info(text));
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

/// The text of a background job's notice ([`NOTICE_TYPE`]), shown as an
/// information row.
fn notice_text(message: &AgentMessage) -> Option<String> {
    let custom = message.as_custom()?;
    let custom = custom.as_any().downcast_ref::<CustomMessage>()?;
    (custom.custom_type == NOTICE_TYPE).then(|| user_text(&custom.content))
}

#[cfg(test)]
mod tests {
    use bake_ai::providers::faux::{faux_assistant_blocks, faux_text, faux_tool_call};
    use bake_ai::{Message, TextContent, ToolResultMessage, UserMessage};
    use serde_json::json;

    use super::*;

    fn user(text: &str) -> AgentMessage {
        Message::User(UserMessage {
            content: UserContent::Text(text.into()),
            timestamp: 0,
        })
        .into()
    }

    fn result(id: &str, text: &str, is_error: bool, details: Option<Value>) -> AgentMessage {
        Message::ToolResult(ToolResultMessage {
            tool_call_id: id.into(),
            tool_name: "bash".into(),
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
                    state: CallState::Done,
                    summary: None,
                    output: vec!["one".into(), "two".into()],
                },
                Row::Call {
                    tool: "Edit".into(),
                    argument: "src/x.rs".into(),
                    state: CallState::Done,
                    summary: None,
                    output: vec!["@@ -1,1 +1,1 @@".into(), "-old".into(), "+new".into()],
                },
                Row::Call {
                    tool: "Read".into(),
                    argument: "gone.txt".into(),
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
}
