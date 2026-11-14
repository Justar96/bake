//! Reading a spawned agent's RPC stream: what each event changes in its
//! row, and what the host must do about it.
//!
//! The stream is Bake's own (`bake docs rpc`): session events, command
//! responses, and `extension_ui_request` records. Everything is read
//! defensively; a record that does not have the expected shape changes
//! nothing.

use serde_json::{Value, json};

use super::{SpawnedAgent, SpawnedState};
use crate::questions::{Question, QuestionAnswer, QuestionBroker, QuestionOption};
use crate::telecom::wire::one_line;

/// What the host does after an event.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// A run settled: tell the parent, with its last answer or failure.
    Settled {
        text: Option<String>,
        failed: Option<String>,
    },
    /// The agent reported its session id.
    SessionId,
    /// The agent asks its person something.
    Ask(Request),
}

/// An `extension_ui_request` to put to the parent's person.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    id: String,
    kind: Kind,
}

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    /// An approval: answered `confirmed`.
    Confirm { question: Question },
    /// A question: answered with `value` and `values`.
    Choose { question: Question },
}

const ALLOW: &str = "Allow once";
const REJECT: &str = "Reject";

impl Request {
    /// The response record for the agent: the person's answer, or a
    /// withdrawal when nobody can answer or the session ends.
    pub async fn forward(
        self,
        name: &str,
        questions: Option<&QuestionBroker>,
        signal: bake_ai::AbortSignal,
    ) -> Value {
        let cancelled =
            json!({ "type": "extension_ui_response", "id": self.id, "cancelled": true });
        let Some(broker) = questions else {
            return cancelled;
        };
        let question = match &self.kind {
            Kind::Confirm { question } | Kind::Choose { question } => question.clone(),
        };
        let call = format!("spawn:{name}:{}", self.id);
        let answer = broker
            .ask(&call, Some(name.to_owned()), vec![question], Some(signal))
            .await;
        let entry = match answer {
            Ok(QuestionAnswer::Answered(mut entries)) if !entries.is_empty() => entries.remove(0),
            _ => return cancelled,
        };
        match self.kind {
            Kind::Confirm { .. } => json!({
                "type": "extension_ui_response",
                "id": self.id,
                "confirmed": entry.first().is_some_and(|choice| choice == ALLOW),
            }),
            Kind::Choose { .. } => json!({
                "type": "extension_ui_response",
                "id": self.id,
                "value": entry.first().cloned().unwrap_or_default(),
                "values": entry,
            }),
        }
    }
}

/// The run being read: its current assistant text.
#[derive(Debug, Default)]
pub struct Follower {
    text: String,
    /// The last assistant message's text.
    last: Option<String>,
}

fn str_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn last_line(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|line| one_line(line, 160))
}

fn message_text(message: &Value) -> Option<String> {
    let content = message.get("content")?;
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| str_of(block, "type") == Some("text"))
            .filter_map(|block| str_of(block, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    (!text.trim().is_empty()).then_some(text)
}

impl Follower {
    /// Applies `event` to `row`; returns what the host must do.
    pub fn follow(&mut self, event: &Value, row: &mut SpawnedAgent) -> Vec<Effect> {
        let now = bake_ai::now_ms();
        let mut effects = Vec::new();
        match str_of(event, "type").unwrap_or_default() {
            "agent_start" => {
                if row.state != SpawnedState::Working {
                    row.state = SpawnedState::Working;
                    row.since = now;
                }
                self.text.clear();
                self.last = None;
            }
            "tool_execution_start" if event.get("parentToolCallId").is_none_or(Value::is_null) => {
                row.tool = str_of(event, "toolName").map(|name| one_line(name, 40));
            }
            "tool_execution_end" if event.get("parentToolCallId").is_none_or(Value::is_null) => {
                row.tool = None;
            }
            "message_start" => self.text.clear(),
            "message_update" => {
                let update = event.get("assistantMessageEvent");
                if let Some(update) = update
                    && str_of(update, "type") == Some("text_delta")
                    && let Some(delta) = str_of(update, "delta")
                {
                    self.text.push_str(delta);
                    if let Some(line) = last_line(&self.text) {
                        row.last_line = Some(line);
                    }
                }
            }
            "message_end" => {
                if let Some(message) = event.get("message")
                    && str_of(message, "role") == Some("assistant")
                    && let Some(text) = message_text(message)
                {
                    row.last_line = last_line(&text);
                    self.last = Some(text);
                }
            }
            "agent_settled" => {
                let outcome = event.get("outcome");
                let kind = outcome.and_then(|outcome| str_of(outcome, "kind"));
                let failed = match kind {
                    Some("failed") => Some(
                        outcome
                            .and_then(|outcome| str_of(outcome, "message"))
                            .map_or_else(|| "the run failed".to_owned(), |m| one_line(m, 300)),
                    ),
                    Some("budget_exhausted") => Some("its budget ran out".to_owned()),
                    _ => None,
                };
                row.tool = None;
                row.since = now;
                row.runs += 1;
                row.state = match &failed {
                    Some(reason) => SpawnedState::Failed(reason.clone()),
                    None => SpawnedState::Idle,
                };
                effects.push(Effect::Settled {
                    text: self.last.take(),
                    failed,
                });
            }
            "response" => {
                if str_of(event, "command") == Some("get_state")
                    && let Some(data) = event.get("data")
                {
                    if let Some(session) = str_of(data, "sessionId") {
                        row.session_id = Some(session.to_owned());
                        effects.push(Effect::SessionId);
                    }
                    if let Some(model) = data.get("model")
                        && let (Some(provider), Some(id)) =
                            (str_of(model, "provider"), str_of(model, "id"))
                    {
                        row.model = Some(one_line(&format!("{provider}/{id}"), 120));
                    }
                }
                if str_of(event, "command") == Some("prompt")
                    && event.get("success") == Some(&Value::Bool(false))
                {
                    let error = str_of(event, "error").unwrap_or("the prompt was refused");
                    row.state = SpawnedState::Failed(one_line(error, 300));
                    row.since = now;
                }
            }
            "extension_ui_request" => {
                if let Some(request) = request(event) {
                    effects.push(Effect::Ask(request));
                }
            }
            _ => {}
        }
        effects
    }
}

/// The request in `event`, read as a question for the parent's person.
fn request(event: &Value) -> Option<Request> {
    let id = str_of(event, "id")?.to_owned();
    if !crate::telecom::wire::is_id(&id) {
        return None;
    }
    let title = one_line(str_of(event, "title").unwrap_or("A question"), 400);
    let kind = match str_of(event, "method")? {
        "confirm" => {
            let approval = event.get("approval");
            let tool = approval
                .and_then(|a| str_of(a, "toolName"))
                .unwrap_or("a tool");
            let reason = approval
                .and_then(|a| str_of(a, "reason"))
                .or_else(|| str_of(event, "message"))
                .unwrap_or_default();
            let arguments = approval
                .and_then(|a| a.get("arguments"))
                .map(|arguments| one_line(&arguments.to_string(), 200))
                .unwrap_or_default();
            Kind::Confirm {
                question: Question {
                    question: one_line(&format!("Allow {tool}? {reason}"), 400),
                    header: "Approval".to_owned(),
                    options: vec![
                        QuestionOption {
                            label: ALLOW.to_owned(),
                            description: arguments,
                        },
                        QuestionOption {
                            label: REJECT.to_owned(),
                            description: "Refuse this call".to_owned(),
                        },
                    ],
                    multi_select: false,
                },
            }
        }
        "select" | "input" => {
            // Bake's own `question` member carries the whole question; a
            // plain Pi request has its title and option labels.
            let question = event
                .get("question")
                .and_then(|question| {
                    let mut question: Question = serde_json::from_value(json!({
                        "question": question.get("question")?,
                        "header": question.get("header").cloned().unwrap_or(Value::Null).as_str().unwrap_or_default(),
                        "options": question.get("options").cloned().unwrap_or(json!([])),
                        "multiSelect": question.get("multiSelect").and_then(Value::as_bool).unwrap_or(false),
                    }))
                    .ok()?;
                    question.question = one_line(&question.question, 400);
                    Some(question)
                })
                .unwrap_or_else(|| Question {
                    question: title,
                    header: String::new(),
                    options: event
                        .get("options")
                        .and_then(Value::as_array)
                        .map(|options| {
                            options
                                .iter()
                                .filter_map(Value::as_str)
                                .take(crate::questions::MAX_OPTIONS)
                                .map(|label| QuestionOption {
                                    label: one_line(label, 80),
                                    description: String::new(),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    multi_select: false,
                });
            Kind::Choose { question }
        }
        _ => return None,
    };
    Some(Request { id, kind })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> SpawnedAgent {
        super::super::unreachable_row(1)
    }

    #[test]
    fn a_run_is_followed_to_its_settlement() {
        let mut follower = Follower::default();
        let mut row = row();
        row.state = SpawnedState::Starting;
        let events = [
            json!({ "type": "agent_start" }),
            json!({ "type": "tool_execution_start", "toolName": "bash", "toolCallId": "c" }),
            json!({ "type": "tool_execution_start", "toolName": "read", "toolCallId": "d", "parentToolCallId": "c" }),
            json!({ "type": "tool_execution_end", "toolName": "read", "toolCallId": "d", "parentToolCallId": "c" }),
            json!({ "type": "message_update", "assistantMessageEvent": { "type": "text_delta", "delta": "All tests\npass" } }),
        ];
        for event in &events {
            assert!(follower.follow(event, &mut row).is_empty());
        }
        assert_eq!(row.state, SpawnedState::Working);
        assert_eq!(row.tool.as_deref(), Some("bash"));
        assert_eq!(row.last_line.as_deref(), Some("pass"));
        follower.follow(
            &json!({ "type": "message_end", "message": { "role": "assistant", "content": [{ "type": "text", "text": "Done.\nAll green." }] } }),
            &mut row,
        );
        let effects = follower.follow(
            &json!({ "type": "agent_settled", "aborted": false, "outcome": { "kind": "finished_turn" } }),
            &mut row,
        );
        assert_eq!(
            effects,
            [Effect::Settled {
                text: Some("Done.\nAll green.".into()),
                failed: None
            }]
        );
        assert_eq!(row.state, SpawnedState::Idle);
        assert_eq!(row.tool, None);
        assert_eq!(row.runs, 1);

        let effects = follower.follow(
            &json!({ "type": "agent_settled", "outcome": { "kind": "failed", "message": "no key" } }),
            &mut row,
        );
        assert_eq!(
            effects,
            [Effect::Settled {
                text: None,
                failed: Some("no key".into())
            }]
        );
        assert_eq!(row.state, SpawnedState::Failed("no key".into()));
    }

    #[test]
    fn state_and_requests_are_read() {
        let mut follower = Follower::default();
        let mut row = row();
        let effects = follower.follow(
            &json!({ "type": "response", "command": "get_state", "success": true, "data": { "sessionId": "abcd-1", "model": { "provider": "p", "id": "m" } } }),
            &mut row,
        );
        assert_eq!(effects, [Effect::SessionId]);
        assert_eq!(row.session_id.as_deref(), Some("abcd-1"));
        assert_eq!(row.model.as_deref(), Some("p/m"));

        let effects = follower.follow(
            &json!({ "type": "extension_ui_request", "id": "approval-1-x", "method": "confirm", "title": "Approve?", "approval": { "toolName": "bash", "reason": "escalate", "arguments": { "command": "ls" } } }),
            &mut row,
        );
        let [Effect::Ask(asked)] = effects.as_slice() else {
            panic!("{effects:?}")
        };
        let Kind::Confirm { question } = &asked.kind else {
            panic!()
        };
        assert_eq!(question.question, "Allow bash? escalate");
        assert_eq!(question.options[0].description, r#"{"command":"ls"}"#);

        let plain = request(&json!({ "type": "extension_ui_request", "id": "q1", "method": "select", "title": "Pick", "options": ["a", "b"] })).unwrap();
        let Kind::Choose { question } = plain.kind else {
            panic!()
        };
        assert_eq!(question.options.len(), 2);
        assert!(
            request(&json!({ "type": "extension_ui_request", "id": "../x", "method": "select" }))
                .is_none()
        );
    }

    #[tokio::test]
    async fn nobody_to_ask_withdraws_the_request() {
        let asked = request(&json!({ "type": "extension_ui_request", "id": "q1", "method": "input", "title": "Name?" })).unwrap();
        let controller = bake_ai::AbortController::new();
        let response = asked.forward("alpha-1", None, controller.signal()).await;
        assert_eq!(
            response,
            json!({ "type": "extension_ui_response", "id": "q1", "cancelled": true })
        );
    }
}
