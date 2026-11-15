//! What a `bake -p --output-format stream-json` run printed.
//!
//! The output is untrusted text: a line that is not a JSON object, or an
//! event without the members read here, is skipped, never an error.

use serde_json::Value;

/// The measures read from one run's standard output.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Measures {
    /// The result object, the last line of a run that ended by itself.
    pub result: Option<RunResult>,
    /// Tool calls the model issued.
    pub tool_calls: u64,
    /// Tool calls a tool made for the model, as `codemode` does.
    pub nested_calls: u64,
    /// Calls of either kind that ended in an error.
    pub tool_errors: u64,
    /// Model responses that called a tool.
    pub tool_responses: u64,
    /// Model responses that called more than one tool.
    pub batched_responses: u64,
}

/// The members of the result object the runner keeps.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunResult {
    /// `success`, `error`, `truncated`, `budget_exhausted`, or `cancelled`.
    pub subtype: String,
    /// The final answer's text.
    pub answer: String,
    /// What went wrong, when something did.
    pub error: Option<String>,
    /// Model requests admitted.
    pub model_calls: u64,
    /// Agent turns.
    pub turns: u64,
    /// From the first prompt to the end of the run.
    pub duration_ms: u64,
    /// Uncached input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Input tokens read from the prompt cache.
    pub cache_read: u64,
    /// Input tokens written to the prompt cache.
    pub cache_write: u64,
    /// Every token the run used.
    pub total_tokens: u64,
    /// The cost from the model's prices; 0 when it declares none.
    pub cost_usd: f64,
}

fn count(value: Option<&Value>) -> u64 {
    value.and_then(Value::as_u64).unwrap_or(0)
}

fn text(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn result(event: &Value) -> RunResult {
    let usage = event.get("usage");
    let member = |name: &str| count(usage.and_then(|usage| usage.get(name)));
    RunResult {
        subtype: text(event.get("subtype")),
        answer: text(event.get("result")),
        error: event
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_owned),
        model_calls: count(event.get("modelCalls")),
        turns: count(event.get("numTurns")),
        duration_ms: count(event.get("durationMs")),
        input: member("input"),
        output: member("output"),
        cache_read: member("cacheRead"),
        cache_write: member("cacheWrite"),
        total_tokens: member("totalTokens"),
        cost_usd: event
            .get("costUsd")
            .and_then(Value::as_f64)
            .filter(|cost| cost.is_finite() && *cost >= 0.0)
            .unwrap_or(0.0),
    }
}

/// The tool calls of an assistant message, 0 for any other message.
fn calls_in(message: Option<&Value>) -> u64 {
    let Some(message) = message else { return 0 };
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return 0;
    }
    message
        .get("content")
        .and_then(Value::as_array)
        .map_or(0, |blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("toolCall"))
                .count() as u64
        })
}

/// Reads the measures from a run's standard output.
pub fn parse(stdout: &[u8]) -> Measures {
    let mut measures = Measures::default();
    for line in stdout.split(|byte| *byte == b'\n') {
        let Ok(event) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        match event.get("type").and_then(Value::as_str) {
            Some("tool_execution_end") => {
                if event
                    .get("parentToolCallId")
                    .is_some_and(|parent| !parent.is_null())
                {
                    measures.nested_calls += 1;
                } else {
                    measures.tool_calls += 1;
                }
                if event.get("isError").and_then(Value::as_bool) == Some(true) {
                    measures.tool_errors += 1;
                }
            }
            Some("message_end") => {
                let calls = calls_in(event.get("message"));
                measures.tool_responses += u64::from(calls > 0);
                measures.batched_responses += u64::from(calls > 1);
            }
            Some("result") => measures.result = Some(result(&event)),
            _ => {}
        }
    }
    measures
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN: &str = r#"{"type":"session","version":3,"id":"s"}
{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}
{"type":"message_end","message":{"role":"assistant","content":[{"type":"toolCall","id":"a","name":"read"},{"type":"toolCall","id":"b","name":"codemode"}]}}
{"type":"tool_execution_end","toolCallId":"a","toolName":"read","isError":true}
{"type":"tool_execution_end","toolCallId":"b/1","toolName":"read","isError":false,"parentToolCallId":"b"}
{"type":"tool_execution_end","toolCallId":"b/2","toolName":"bash","isError":true,"parentToolCallId":"b"}
{"type":"tool_execution_end","toolCallId":"b","toolName":"codemode","isError":false}
{"type":"message_end","message":{"role":"assistant","content":[{"type":"toolCall","id":"c","name":"edit"}]}}
{"type":"tool_execution_end","toolCallId":"c","toolName":"edit","isError":false,"parentToolCallId":null}
not json
{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}
{"type":"result","subtype":"success","isError":false,"result":"done","error":null,"numTurns":3,"modelCalls":4,"durationMs":900,"usage":{"input":10,"output":2,"cacheRead":30,"cacheWrite":1,"totalTokens":43},"costUsd":0.25}
"#;

    #[test]
    fn a_run_is_measured() {
        let measures = parse(RUN.as_bytes());
        assert_eq!(
            measures,
            Measures {
                result: Some(RunResult {
                    subtype: "success".into(),
                    answer: "done".into(),
                    error: None,
                    model_calls: 4,
                    turns: 3,
                    duration_ms: 900,
                    input: 10,
                    output: 2,
                    cache_read: 30,
                    cache_write: 1,
                    total_tokens: 43,
                    cost_usd: 0.25,
                }),
                tool_calls: 3,
                nested_calls: 2,
                tool_errors: 2,
                tool_responses: 2,
                batched_responses: 1,
            }
        );
    }

    #[test]
    fn a_run_cut_short_has_no_result() {
        let cut = RUN.rsplit_once("{\"type\":\"result\"").expect("result").0;
        let measures = parse(cut.as_bytes());
        assert_eq!(measures.result, None);
        assert_eq!(measures.tool_calls, 3);
    }

    #[test]
    fn a_failed_start_keeps_its_error_and_odd_numbers_read_as_zero() {
        let line = r#"{"type":"result","subtype":"error","result":null,"error":"no model","modelCalls":-1,"usage":{"input":"many"},"costUsd":-3}"#;
        let result = parse(line.as_bytes()).result.expect("result");
        assert_eq!(result.subtype, "error");
        assert_eq!(result.error.as_deref(), Some("no model"));
        assert_eq!(result.answer, "");
        assert_eq!((result.model_calls, result.input), (0, 0));
        assert_eq!(result.cost_usd, 0.0);
    }

    proptest::proptest! {
        #[test]
        fn any_bytes_parse_without_a_panic(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..512)) {
            let measures = parse(&bytes);
            proptest::prop_assert!(measures.batched_responses <= measures.tool_responses);
        }

        #[test]
        fn any_json_lines_parse_without_a_panic(
            kinds in proptest::collection::vec(0u8..5, 0..24),
            number in proptest::num::i64::ANY,
        ) {
            let lines: Vec<String> = kinds.iter().map(|kind| match kind {
                0 => format!(r#"{{"type":"tool_execution_end","isError":{},"parentToolCallId":{number}}}"#, number % 2 == 0),
                1 => format!(r#"{{"type":"message_end","message":{{"role":"assistant","content":[{{"type":"toolCall"}},{number}]}}}}"#),
                2 => format!(r#"{{"type":"result","modelCalls":{number},"usage":{number},"costUsd":{number}}}"#),
                3 => format!("[{number}]"),
                _ => r#"{"type":7,"message":"x"}"#.to_owned(),
            }).collect();
            let measures = parse(lines.join("\n").as_bytes());
            let ends = kinds.iter().filter(|kind| **kind == 0).count() as u64;
            proptest::prop_assert_eq!(measures.tool_calls + measures.nested_calls, ends);
            proptest::prop_assert!(measures.tool_errors <= ends);
        }
    }
}
