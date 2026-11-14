//! The `ask_user` tool: up to [`MAX_QUESTIONS`] questions to the person,
//! each answered by picking options or typing.

use bake_agent::{AgentTool, AgentToolResult, ToolReplay};
use serde_json::{Value, json};

use super::{MAX_OPTIONS, MAX_QUESTIONS, Question, QuestionAnswer, QuestionBroker};
use crate::tools::definition::ToolDefinition;

/// The tool's name.
pub const TOOL_NAME: &str = "ask_user";

const DESCRIPTION: &str = "Ask the person one to four questions and wait for the answers. Use it when a decision is theirs and cannot be settled from the request, the code, or a sensible default; not to ask for permission or to confirm a plan. Give each question 2 to 6 short options when the choices are known (the person may still type their own), or none for a typed answer. multiSelect lets them pick several.";

const SNIPPET: &str = "Ask the person questions with options when a decision is genuinely theirs";

fn parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "questions": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_QUESTIONS,
                "items": {
                    "type": "object",
                    "properties": {
                        "question": { "type": "string", "description": "The question, ending with a question mark." },
                        "header": { "type": "string", "description": "A label of at most 16 characters." },
                        "options": {
                            "type": "array",
                            "maxItems": MAX_OPTIONS,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "label": { "type": "string", "description": "1 to 5 words." },
                                    "description": { "type": "string", "description": "What choosing it means." }
                                },
                                "required": ["label"],
                                "additionalProperties": false
                            }
                        },
                        "multiSelect": { "type": "boolean" }
                    },
                    "required": ["question"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["questions"],
        "additionalProperties": false
    })
}

/// The questions in `args`, checked and trimmed.
pub(crate) fn questions(args: &Value) -> Result<Vec<Question>, String> {
    let list = args
        .get("questions")
        .and_then(Value::as_array)
        .ok_or("questions: expected an array of questions")?;
    if list.is_empty() || list.len() > MAX_QUESTIONS {
        return Err(format!("questions: ask 1 to {MAX_QUESTIONS}"));
    }
    list.iter()
        .enumerate()
        .map(|(index, item)| {
            let mut question: Question = serde_json::from_value(item.clone())
                .map_err(|error| format!("questions[{index}]: {error}"))?;
            question.question = question.question.trim().to_owned();
            question.header = question.header.trim().chars().take(16).collect();
            if question.question.is_empty() {
                return Err(format!("questions[{index}].question: write the question"));
            }
            if question.options.len() == 1 || question.options.len() > MAX_OPTIONS {
                return Err(format!(
                    "questions[{index}].options: give 2 to {MAX_OPTIONS}, or none for a typed answer"
                ));
            }
            for option in &mut question.options {
                option.label = option.label.trim().to_owned();
                option.description = option.description.trim().to_owned();
            }
            if question.options.iter().any(|option| option.label.is_empty()) {
                return Err(format!("questions[{index}].options: every option needs a label"));
            }
            let mut labels: Vec<&str> = question.options.iter().map(|o| o.label.as_str()).collect();
            labels.sort_unstable();
            labels.dedup();
            if labels.len() != question.options.len() {
                return Err(format!("questions[{index}].options: labels must differ"));
            }
            if question.options.is_empty() {
                question.multi_select = false;
            }
            Ok(question)
        })
        .collect()
}

/// The answer as the model reads it.
pub(crate) fn answer_text(questions: &[Question], answers: &[Vec<String>]) -> String {
    let mut lines = vec!["The person answered:".to_owned()];
    for (question, answer) in questions.iter().zip(answers) {
        let label = if question.header.is_empty() {
            &question.question
        } else {
            &question.header
        };
        let chosen = if answer.is_empty() {
            "(no answer)".to_owned()
        } else {
            answer.join(", ")
        };
        lines.push(format!("- {label}: {chosen}"));
    }
    lines.join("\n")
}

/// The `ask_user` tool over `broker`.
pub fn tool(broker: QuestionBroker) -> ToolDefinition {
    let mut tool = AgentTool::new(
        TOOL_NAME,
        "Ask user",
        DESCRIPTION,
        parameters(),
        move |call_id, args, signal, _update, _context| {
            let broker = broker.clone();
            Box::pin(async move {
                let questions = questions(&args)?;
                match broker
                    .ask(&call_id, None, questions.clone(), signal)
                    .await?
                {
                    QuestionAnswer::Answered(answers) => {
                        let mut result = AgentToolResult::text(answer_text(&questions, &answers));
                        result.details = Some(json!({ "answers": answers }));
                        Ok(result)
                    }
                    QuestionAnswer::Declined => {
                        let mut result = AgentToolResult::text(
                            "The person dismissed the questions without answering. Do not ask again; go on with your best judgment and say what you assumed.",
                        );
                        result.details = Some(json!({ "declined": true }));
                        Ok(result)
                    }
                }
            })
        },
    );
    // A replayed call would ask again; the recorded answer stands.
    tool.replay = Some(ToolReplay::Never);
    tool.read_only = true;
    ToolDefinition {
        tool,
        prompt_snippet: Some(SNIPPET.to_owned()),
        prompt_guidelines: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_are_checked() {
        let ok = json!({ "questions": [
            { "question": " Which? ", "header": "a very long header text", "options": [{ "label": "A" }, { "label": "B", "description": "b" }] },
            { "question": "Name?", "multiSelect": true },
        ]});
        let parsed = questions(&ok).unwrap();
        assert_eq!(parsed[0].question, "Which?");
        assert_eq!(parsed[0].header.chars().count(), 16);
        assert!(!parsed[1].multi_select, "a typed answer picks nothing");

        for bad in [
            json!({ "questions": [] }),
            json!({ "questions": [{ "question": "" }] }),
            json!({ "questions": [{ "question": "q", "options": [{ "label": "A" }] }] }),
            json!({ "questions": [{ "question": "q", "options": [{ "label": "A" }, { "label": "A" }] }] }),
            json!({ "questions": [{ "question": "q", "extra": 1 }] }),
        ] {
            assert!(questions(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn answers_read_by_header() {
        let parsed = questions(&json!({ "questions": [
            { "question": "Which db?", "header": "Database", "options": [{ "label": "Postgres" }, { "label": "SQLite" }] },
            { "question": "Name?" },
        ]}))
        .unwrap();
        assert_eq!(
            answer_text(&parsed, &[vec!["SQLite".into()], vec![]]),
            "The person answered:\n- Database: SQLite\n- Name?: (no answer)"
        );
    }
}
