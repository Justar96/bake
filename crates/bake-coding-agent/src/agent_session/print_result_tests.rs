//! Print mode's endings: Bake's result object (`--output-format json` and
//! `stream-json`) and Pi v1.4.0's truncated-response check (revision
//! `4a080252f06caf63a525d7873c5d94d3c23887e0`, issue #10799). Each test
//! owns its in-memory session and faux provider.

use bake_ai::StopReason;
use bake_ai::providers::faux::{faux_assistant_blocks, faux_assistant_message, faux_text};
use serde_json::Value;

use super::control_tests::{harness, in_memory};
use super::*;
use crate::print_mode::{
    BUDGET_EXHAUSTED, ERROR, PrintModeOptions, PrintOutput, PrintRunRecovery, SUCCESS,
    SharedWriter, TRUNCATED, TRUNCATED_MESSAGE, run_print_mode,
};

fn options(mode: PrintOutput, control: RunControl) -> PrintModeOptions {
    PrintModeOptions {
        control,
        recovery: PrintRunRecovery::Disabled,
        mode,
        initial_message: Some("Answer".into()),
        initial_images: Vec::new(),
        messages: Vec::new(),
        job_wait: std::time::Duration::ZERO,
    }
}

fn buffer() -> (SharedWriter, Arc<Mutex<Vec<u8>>>) {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    (bytes.clone() as SharedWriter, bytes)
}

fn text(bytes: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&bytes.lock().expect("output")).into_owned()
}

fn json_lines(bytes: &Arc<Mutex<Vec<u8>>>) -> Vec<Value> {
    text(bytes)
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect()
}

/// Runs print mode in `mode` over one faux response; returns the exit
/// code, standard output, standard error, the session's id and usage, and
/// the provider's call count.
async fn print(
    mode: PrintOutput,
    response: bake_ai::AssistantMessage,
    control: RunControl,
) -> (i32, Arc<Mutex<Vec<u8>>>, String, String, UsageTotals, u64) {
    print_with(mode, response, control, None).await
}

/// [`print`] with the session on `model`.
async fn print_with(
    mode: PrintOutput,
    response: bake_ai::AssistantMessage,
    control: RunControl,
    model: Option<&str>,
) -> (i32, Arc<Mutex<Vec<u8>>>, String, String, UsageTotals, u64) {
    let h = harness();
    let session = h
        .session(in_memory(), Settings::default(), None, Vec::new())
        .await;
    if let Some(model) = model {
        session
            .set_model(h.model(model), false)
            .await
            .expect("model");
    }
    h.faux.set_responses(vec![response.into()]);
    let (stdout, out) = buffer();
    let (stderr, errors) = buffer();
    let code = run_print_mode(&session, options(mode, control), stdout, stderr).await;
    (
        code,
        out,
        text(&errors),
        session.session_id(),
        session.usage_totals(),
        h.faux.call_count(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_result_object_reports_the_answer_the_session_and_the_usage() {
    let (code, out, errors, id, usage, calls) = print(
        PrintOutput::Result,
        faux_assistant_message("The answer."),
        RunControl::default(),
    )
    .await;
    assert_eq!((code, errors.as_str(), calls), (0, "", 1));
    let lines = json_lines(&out);
    assert_eq!(lines.len(), 1, "only the result: {lines:?}");
    let result = &lines[0];
    assert_eq!(result["type"], "result");
    assert_eq!(result["subtype"], SUCCESS);
    assert_eq!(result["isError"], false);
    assert_eq!(result["result"], "The answer.");
    assert_eq!(result["error"], Value::Null);
    assert_eq!(result["stopReason"], "stop");
    assert_eq!(result["sessionId"], id.as_str());
    // An in-memory session has no file to continue from.
    assert_eq!(result["sessionFile"], Value::Null);
    assert_eq!(result["runId"], Value::Null);
    assert_eq!(result["model"], "faux/faux-1");
    assert_eq!(result["numTurns"], 1);
    assert_eq!(result["modelCalls"], 1);
    assert!(result["durationMs"].is_u64());
    assert_eq!(result["usage"]["input"], usage.input);
    assert_eq!(result["usage"]["output"], usage.output);
    assert_eq!(result["usage"]["cacheRead"], usage.cache_read);
    assert_eq!(result["usage"]["cacheWrite"], usage.cache_write);
    assert_eq!(
        result["usage"]["totalTokens"],
        usage.input + usage.output + usage.cache_read + usage.cache_write
    );
    assert!(result["costUsd"].is_number());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_json_ends_its_events_with_the_result_and_json_does_not() {
    for mode in [PrintOutput::Json, PrintOutput::StreamJson] {
        let (code, out, errors, _, _, _) = print(
            mode,
            faux_assistant_message("Streamed."),
            RunControl::default(),
        )
        .await;
        assert_eq!((code, errors.as_str()), (0, ""), "{mode:?}");
        let lines = json_lines(&out);
        let kinds: Vec<&str> = lines
            .iter()
            .map(|line| line["type"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(kinds[0], "session", "{mode:?}");
        if mode == PrintOutput::Json {
            assert_eq!(kinds.last(), Some(&"agent_settled"));
            assert!(!kinds.contains(&"result"));
        } else {
            assert_eq!(kinds[kinds.len() - 2..], ["agent_settled", "result"]);
            assert_eq!(lines[lines.len() - 1]["result"], "Streamed.");
        }
    }
}

// Pi v1.4.0: a response cut off at the output limit is still written, then
// reported, and the run exits 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_truncated_answer_is_written_then_reported_and_exits_one() {
    // On a model whose whole output allowance the answer used, so no
    // compact-and-retry recovers it (`is_recoverable_length`).
    let cut = || faux_assistant_blocks(vec![faux_text("Half an answer")], StopReason::Length);
    let short = Some("short");
    let (code, out, errors, _, _, _) =
        print_with(PrintOutput::Text, cut(), RunControl::default(), short).await;
    assert_eq!(code, 1, "{} / {errors}", text(&out));
    assert_eq!(text(&out), "Half an answer\n");
    assert_eq!(errors, format!("{TRUNCATED_MESSAGE}\n"));

    let (code, out, errors, _, _, _) =
        print_with(PrintOutput::Result, cut(), RunControl::default(), short).await;
    assert_eq!(code, 1);
    assert_eq!(errors, format!("{TRUNCATED_MESSAGE}\n"));
    let result = &json_lines(&out)[0];
    assert_eq!(result["subtype"], TRUNCATED);
    assert_eq!(result["isError"], true);
    assert_eq!(result["result"], "Half an answer");
    assert_eq!(result["error"], TRUNCATED_MESSAGE);
    assert_eq!(result["stopReason"], "length");

    // Pi's JSON mode leaves the ending to its events.
    let (code, _, errors, _, _, _) =
        print_with(PrintOutput::Json, cut(), RunControl::default(), short).await;
    assert_eq!((code, errors.as_str()), (0, ""));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_that_stops_or_fails_says_why_in_its_result() {
    let none = RunControl::new(bake_agent::RunLimits {
        deadline: None,
        max_model_calls: Some(0),
    });
    let (code, out, errors, _, _, calls) =
        print(PrintOutput::Result, faux_assistant_message("unused"), none).await;
    assert_eq!((code, calls), (1, 0));
    assert_eq!(errors, "Run model-call allowance exhausted\n");
    let result = &json_lines(&out)[0];
    assert_eq!(result["subtype"], BUDGET_EXHAUSTED);
    assert_eq!(result["isError"], true);
    assert_eq!(result["error"], "Run model-call allowance exhausted");
    assert_eq!(result["modelCalls"], 0);
    assert_eq!(result["numTurns"], 0);

    let mut failed = faux_assistant_blocks(Vec::new(), StopReason::Error);
    failed.error_message = Some("the provider refused".into());
    let (code, out, errors, _, _, _) =
        print(PrintOutput::Result, failed, RunControl::default()).await;
    assert_eq!(code, 1);
    assert!(errors.contains("the provider refused"), "{errors}");
    let result = &json_lines(&out)[0];
    assert_eq!(result["subtype"], ERROR);
    assert_eq!(result["isError"], true);
    assert!(
        result["error"]
            .as_str()
            .is_some_and(|error| error.contains("the provider refused")),
        "{result}"
    );
}
