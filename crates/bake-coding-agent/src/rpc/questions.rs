//! `ask_user` questions over the RPC protocol.
//!
//! The session's [`QuestionBroker`] is answered by the client, one
//! question at a time, as Pi's `extension_ui_request`: `select` with the
//! option labels when the question offers options, `input` when it takes
//! a typed answer. Bake adds a `question` member with the whole question
//! (header, options with descriptions, `multiSelect`, its `index` among
//! `count` questions, and `from`, the spawned agent asking through this
//! session), which a Pi client ignores.
//!
//! The answer is Pi's `extension_ui_response` with `value`: a label or any
//! text the person typed. Bake also reads `values`, a list, for a
//! multiple-choice question. `cancelled: true` declines the whole request.
//! Once every question has an answer, the request settles.

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use crate::print_mode::{SharedWriter, write_out};
use crate::questions::{QuestionAnswer, QuestionBroker, QuestionPrompt};
use crate::rpc::jsonl::serialize_json_line;
use crate::rpc::types::ExtensionUiResponse;

/// The request being answered: its prompt, the question shown, the
/// answers so far, and the id the shown question went out under.
struct Current {
    prompt: QuestionPrompt,
    answers: Vec<Vec<String>>,
    id: String,
}

/// Announces questions on stdout and settles them from stdin.
pub struct QuestionBridge {
    broker: QuestionBroker,
    out: SharedWriter,
    current: Mutex<Option<Current>>,
}

impl std::fmt::Debug for QuestionBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuestionBridge").finish_non_exhaustive()
    }
}

impl QuestionBridge {
    /// Connects to `broker`, or `None` when it already has a frontend.
    pub fn connect(broker: &QuestionBroker, out: &SharedWriter) -> Option<Arc<Self>> {
        let bridge = Arc::new(Self {
            broker: broker.clone(),
            out: Arc::clone(out),
            current: Mutex::new(None),
        });
        let listener = {
            let bridge = Arc::downgrade(&bridge);
            move |queue: Vec<QuestionPrompt>| match bridge.upgrade() {
                Some(bridge) => {
                    bridge.show(queue.first());
                    true
                }
                None => false,
            }
        };
        broker.connect(Box::new(listener)).then_some(bridge)
    }

    /// Shows the first question of `front` if it is not already being
    /// answered; forgets a request that left the queue.
    fn show(&self, front: Option<&QuestionPrompt>) {
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(front) = front else {
            *current = None;
            return;
        };
        if current
            .as_ref()
            .is_some_and(|shown| shown.prompt.generation == front.generation)
        {
            return;
        }
        let next = Current {
            prompt: front.clone(),
            answers: Vec::new(),
            id: String::new(),
        };
        *current = Some(next);
        if let Some(shown) = current.as_mut() {
            self.announce(shown);
        }
    }

    /// Writes the request for the next unanswered question of `shown`.
    fn announce(&self, shown: &mut Current) {
        let index = shown.answers.len();
        let Some(question) = shown.prompt.questions.get(index) else {
            return;
        };
        shown.id = format!(
            "question-{}-{index}-{}",
            shown.prompt.generation,
            crate::session::id::uuid_v4()
        );
        let mut record = json!({
            "type": "extension_ui_request",
            "id": shown.id,
            "method": if question.options.is_empty() { "input" } else { "select" },
            "title": question.question,
            "question": {
                "header": question.header,
                "question": question.question,
                "options": question.options,
                "multiSelect": question.multi_select,
                "index": index,
                "count": shown.prompt.questions.len(),
                "from": shown.prompt.from,
                "callId": shown.prompt.call_id,
            },
        });
        if !question.options.is_empty() {
            record["options"] = Value::from(
                question
                    .options
                    .iter()
                    .map(|option| option.label.clone())
                    .collect::<Vec<_>>(),
            );
        }
        write_out(&self.out, &serialize_json_line(&record));
    }

    /// Takes `response` if it answers the shown question. Returns whether
    /// it did.
    pub fn answer(&self, response: &ExtensionUiResponse) -> bool {
        let settle = {
            let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(shown) = current.as_mut().filter(|shown| shown.id == response.id) else {
                return false;
            };
            if response.cancelled == Some(true) {
                let generation = shown.prompt.generation;
                *current = None;
                Some((generation, QuestionAnswer::Declined))
            } else {
                let entry: Vec<String> = match (&response.values, &response.value) {
                    (Some(values), _) => values.clone(),
                    (None, Some(value)) => vec![value.clone()],
                    (None, None) => Vec::new(),
                };
                let entry = entry
                    .into_iter()
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty())
                    .collect();
                shown.answers.push(entry);
                if shown.answers.len() < shown.prompt.questions.len() {
                    self.announce(shown);
                    None
                } else {
                    let generation = shown.prompt.generation;
                    let answers = std::mem::take(&mut shown.answers);
                    *current = None;
                    Some((generation, QuestionAnswer::Answered(answers)))
                }
            }
        };
        if let Some((generation, answer)) = settle {
            self.broker.answer(generation, answer);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::questions::{Question, QuestionOption};

    fn response(id: &str, value: Option<&str>, values: Option<Vec<&str>>) -> ExtensionUiResponse {
        ExtensionUiResponse {
            id: id.to_owned(),
            value: value.map(str::to_owned),
            values: values.map(|values| values.into_iter().map(str::to_owned).collect()),
            confirmed: None,
            cancelled: None,
        }
    }

    fn records(out: &Arc<Mutex<Vec<u8>>>) -> Vec<Value> {
        let bytes = out.lock().unwrap().clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn questions_go_out_one_at_a_time_and_settle_together() {
        let broker = QuestionBroker::new();
        let out = Arc::new(Mutex::new(Vec::new()));
        let writer: SharedWriter = out.clone();
        let bridge = QuestionBridge::connect(&broker, &writer).unwrap();
        let asking = tokio::spawn({
            let broker = broker.clone();
            async move {
                let questions = vec![
                    Question {
                        question: "Which db?".into(),
                        header: "Database".into(),
                        options: vec![
                            QuestionOption {
                                label: "Postgres".into(),
                                description: "server".into(),
                            },
                            QuestionOption {
                                label: "SQLite".into(),
                                description: String::new(),
                            },
                        ],
                        multi_select: true,
                    },
                    Question {
                        question: "Name?".into(),
                        header: String::new(),
                        options: Vec::new(),
                        multi_select: false,
                    },
                ];
                broker
                    .ask("call_1", Some("alpha-1".into()), questions, None)
                    .await
            }
        });
        while records(&out).is_empty() {
            tokio::task::yield_now().await;
        }
        let first = records(&out).remove(0);
        assert_eq!(first["method"], "select");
        assert_eq!(first["options"], json!(["Postgres", "SQLite"]));
        assert_eq!(first["question"]["from"], "alpha-1");
        assert_eq!(first["question"]["count"], 2);
        let id = first["id"].as_str().unwrap();
        assert!(!bridge.answer(&response("other", Some("x"), None)));
        assert!(bridge.answer(&response(id, None, Some(vec!["Postgres", "SQLite"]))));
        let second = records(&out).remove(1);
        assert_eq!(second["method"], "input");
        assert!(second.get("options").is_none());
        assert!(bridge.answer(&response(
            second["id"].as_str().unwrap(),
            Some(" users "),
            None
        )));
        assert_eq!(
            asking.await.unwrap(),
            Ok(QuestionAnswer::Answered(vec![
                vec!["Postgres".into(), "SQLite".into()],
                vec!["users".into()]
            ]))
        );
    }
}
