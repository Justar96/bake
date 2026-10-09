//! `assertReleasedPayloadSemantics(event, version)` from
//! `packages/session/session-format-v0-to-v1/src/payload-validation.ts`, the
//! frozen nested payload rules that V2 migration admission reuses at version
//! 2 and the v0→v1 edge reuses at version 0.
//!
//! The version decides only which `session-log-deepseek/delivery-accepted`
//! markers carry checked coordinates, whether a session reference admits
//! `capturedFormatVersion` and its upper bound, and whether a user message
//! may take the legacy goal form, which this crate does not port: before
//! version 2 such a message reports the `legacy-goal-message` limit.
//! `assistant/chunk` with its stream chunk, finish, and replay helpers is not
//! ported; neither caller validates it. Checks run in TypeScript's order and
//! the first failure wins, so a [`StageError::NativeLimit`] may hide a later
//! TypeScript rejection but never an earlier one.
//!
//! Numbers: serde_json with `float_roundtrip` holds the double `JSON.parse`
//! produces, so threshold comparisons on finite numbers are exact. Integer
//! checks decide an `f64` spelled as `JSON.stringify` writes its value
//! (`is_writer_spelling`) by that value, which is a fraction or lies outside
//! the integer range serde_json stores exactly, so the check fails as it
//! fails in TypeScript. Any other non-negative `f64` spelling (or -0 for safe
//! integers), such as `3.0` or `1e3`, reports a native limit, because no
//! writer produces it.

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::StageError;
use crate::json_text::{is_writer_spelling, json_text};
use crate::{Count, MAX_SAFE_INTEGER};

pub(crate) type Checked<T = ()> = Result<T, StageError>;
type Record = Map<String, Value>;

pub(crate) fn invalid<T>(message: String) -> Checked<T> {
    Err(StageError::Invalid(message))
}

/// An integer check met an `f64` other than a negative value or -0.
const FLOAT_LEXEME: &str = "payload-float-lexeme";
/// A pre-v2 user message with a goal source carrying `change`, whose check
/// compares the content with a `JSON.stringify` rendering of the change.
const LEGACY_GOAL_MESSAGE: &str = "legacy-goal-message";

fn float_lexeme() -> StageError {
    StageError::NativeLimit(FLOAT_LEXEME.to_owned())
}

/// Whether `value` is a number whose spelling a writer produces. Such an
/// `f64` is a fraction or an integer at least 2^63 in magnitude, neither of
/// which is a safe integer or equals a count.
fn is_writer_float(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Number(number)) if is_writer_spelling(number))
}

/// `sessionFormatCount`.
pub(crate) fn count(value: Option<&Value>, label: &str) -> Checked<u64> {
    match value.and_then(crate::count) {
        Some(Count::Safe(number)) => Ok(number),
        Some(Count::Undecided) if !is_writer_float(value) => Err(float_lexeme()),
        _ => invalid(format!("{label} must be a non-negative safe integer")),
    }
}

/// `sessionFormatSafeInteger`.
pub(super) fn safe_integer(value: Option<&Value>, label: &str) -> Checked<i64> {
    let refused = || invalid(format!("{label} must be a safe integer"));
    let Some(Value::Number(number)) = value else {
        return refused();
    };
    if let Some(number) = number.as_i64() {
        return if number.unsigned_abs() <= MAX_SAFE_INTEGER {
            Ok(number)
        } else {
            refused()
        };
    }
    if number.is_u64() || number.as_f64().is_some_and(is_negative_zero) || is_writer_float(value) {
        return refused();
    }
    Err(float_lexeme())
}

fn is_negative_zero(number: f64) -> bool {
    number == 0.0 && number.is_sign_negative()
}

/// Own enumerable keys in JavaScript order: array-index keys ascending, then
/// the rest in insertion order, which `preserve_order` keeps.
pub(super) fn js_keys(fields: &Record) -> Vec<&str> {
    let mut indices: Vec<(u32, &str)> = Vec::new();
    let mut names = Vec::new();
    for key in fields.keys() {
        match array_index(key) {
            Some(index) => indices.push((index, key)),
            None => names.push(key.as_str()),
        }
    }
    indices.sort_unstable_by_key(|(index, _)| *index);
    indices
        .into_iter()
        .map(|(_, key)| key)
        .chain(names)
        .collect()
}

/// The canonical decimal spelling of an integer below `2^32 - 1`.
fn array_index(key: &str) -> Option<u32> {
    let canonical = key == "0"
        || (!key.is_empty() && !key.starts_with('0') && key.bytes().all(|b| b.is_ascii_digit()));
    key.parse::<u32>()
        .ok()
        .filter(|index| canonical && *index != u32::MAX)
}

/// `JSON.stringify` of a string, which escapes exactly what serde_json does.
pub(crate) fn quote(text: &str) -> String {
    Value::from(text).to_string()
}

/// `JSON.stringify(value)` concatenated into a message, `undefined` when
/// absent. Every number prints as JavaScript prints its value.
pub(crate) fn stringify(value: Option<&Value>) -> String {
    value.map_or_else(|| "undefined".to_owned(), json_text)
}

/// `releasedV0Record`.
pub(crate) fn released_record<'a>(value: Option<&'a Value>, label: &str) -> Checked<&'a Record> {
    match value {
        Some(Value::Object(fields)) => Ok(fields),
        _ => invalid(format!("{label} must be a JSON object")),
    }
}

/// `assertReleasedV0Keys`: the first unexpected member in JavaScript order,
/// then the first missing required member.
pub(crate) fn released_keys(
    record: &Record,
    required: &[&str],
    optional: &[&str],
    label: &str,
) -> Checked {
    if let Some(key) = js_keys(record)
        .into_iter()
        .find(|key| !required.contains(key) && !optional.contains(key))
    {
        return invalid(format!("{label} has unexpected member {}", quote(key)));
    }
    if let Some(key) = required.iter().find(|key| !record.contains_key(**key)) {
        return invalid(format!("{label} lacks required member {}", quote(key)));
    }
    Ok(())
}

fn exact_record<'a>(
    value: Option<&'a Value>,
    label: &str,
    required: &[&str],
    optional: &[&str],
) -> Checked<&'a Record> {
    let record = released_record(value, label)?;
    released_keys(record, required, optional, label)?;
    Ok(record)
}

fn string_value<'a>(value: Option<&'a Value>, label: &str) -> Checked<&'a str> {
    match value {
        Some(Value::String(text)) => Ok(text),
        _ => invalid(format!("{label} must be a string")),
    }
}

fn non_empty_string<'a>(value: Option<&'a Value>, label: &str) -> Checked<&'a str> {
    match value {
        Some(Value::String(text)) if !text.is_empty() => Ok(text),
        _ => invalid(format!("{label} must be a non-empty string")),
    }
}

fn boolean_value(value: Option<&Value>, label: &str) -> Checked<bool> {
    match value {
        Some(Value::Bool(flag)) => Ok(*flag),
        _ => invalid(format!("{label} must be a boolean")),
    }
}

fn positive_integer(value: Option<&Value>, label: &str) -> Checked<u64> {
    let number = count(value, label)?;
    if number == 0 {
        return invalid(format!("{label} must be positive"));
    }
    Ok(number)
}

/// `finiteNumberValue`. serde_json holds only finite numbers.
fn finite_number(value: Option<&Value>, label: &str) -> Checked<f64> {
    match value {
        Some(Value::Number(number)) => match number.as_f64() {
            Some(number) if !is_negative_zero(number) => Ok(number),
            _ => invalid(format!("{label} must be a finite number")),
        },
        _ => invalid(format!("{label} must be a finite number")),
    }
}

#[derive(Debug, Clone, Copy)]
enum Literal {
    Text(&'static str),
    Flag(bool),
    Int(u64),
}

use Literal::{Flag, Int, Text};

/// `literalValue`: `===` against each candidate, then a message listing them.
fn literal_value(value: Option<&Value>, allowed: &[Literal], label: &str) -> Checked {
    for candidate in allowed {
        let equal = match (candidate, value) {
            (Text(text), Some(Value::String(value))) => text == value,
            (Flag(flag), Some(Value::Bool(value))) => flag == value,
            (Int(expected), Some(Value::Number(number))) => {
                if let Some(number) = number.as_u64() {
                    number == *expected
                } else if number.is_i64() {
                    false
                } else {
                    match number.as_f64() {
                        // -0 === 0; a negative never equals a count.
                        Some(number) if number.is_sign_negative() => number == *expected as f64,
                        _ if is_writer_float(value) => false,
                        _ => return Err(float_lexeme()),
                    }
                }
            }
            _ => false,
        };
        if equal {
            return Ok(());
        }
    }
    let listed: Vec<String> = allowed
        .iter()
        .map(|candidate| match candidate {
            Text(text) => (*text).to_owned(),
            Flag(flag) => flag.to_string(),
            Int(number) => number.to_string(),
        })
        .collect();
    invalid(format!("{label} must be one of {}", listed.join(", ")))
}

fn texts(values: &'static [&'static str]) -> Vec<Literal> {
    values.iter().copied().map(Text).collect()
}

fn array_value<'a>(
    value: Option<&'a Value>,
    label: &str,
    mut validate: impl FnMut(&'a Value, &str) -> Checked,
) -> Checked<&'a [Value]> {
    let Some(Value::Array(members)) = value else {
        return invalid(format!("{label} must be an array"));
    };
    for (index, member) in members.iter().enumerate() {
        validate(member, &format!("{label}[{index}]"))?;
    }
    Ok(members)
}

fn coordinate_pair(data: &Record, label: &str) -> Checked {
    count(data.get("turn"), &format!("{label} turn"))?;
    count(data.get("step"), &format!("{label} step"))?;
    Ok(())
}

fn earlier_seq(value: Option<&Value>, event_seq: u64, label: &str) -> Checked<u64> {
    let seq = count(value, label)?;
    if seq >= event_seq {
        return invalid(format!("{label} must identify an earlier event"));
    }
    Ok(seq)
}

fn seq_array(
    value: Option<&Value>,
    event_seq: u64,
    label: &str,
    require_non_empty: bool,
) -> Checked<Vec<u64>> {
    let mut seqs = Vec::new();
    let mut seen = HashSet::new();
    array_value(value, label, |member, member_label| {
        let seq = earlier_seq(Some(member), event_seq, member_label)?;
        if !seen.insert(seq) {
            return invalid(format!("{label} repeats seq {seq}"));
        }
        seqs.push(seq);
        Ok(())
    })?;
    if require_non_empty && seqs.is_empty() {
        return invalid(format!("{label} must be non-empty"));
    }
    Ok(seqs)
}

/// Validate one released payload at payload generation `version`. The
/// caller has checked the event's top-level data members against its
/// disposition for that generation.
pub(crate) fn assert_released_payload_semantics(
    event_type: &str,
    seq: u64,
    data: Option<&Value>,
    version: u8,
) -> Checked {
    let label = format!("{event_type} {seq}");
    let label = label.as_str();
    let raw = data;
    let data = released_record(data, &format!("{label} data"))?;
    let field = |key: &str| data.get(key);
    let at = |key: &str| format!("{label} {key}");
    match event_type {
        "agent-preset/selected" => {
            string_value(field("agentPreset"), &at("agentPreset"))?;
        }
        "agent/inbox/spliced" => {
            literal_value(
                field("target"),
                &texts(&["next-turn", "next-step"]),
                &at("target"),
            )?;
            count(field("start"), &at("start"))?;
            if let Some(removed) = field("removedCount") {
                count(Some(removed), &at("removedCount"))?;
            }
            let message_label = at("inserted message");
            array_value(field("inserted"), &at("inserted"), |value, _| {
                message_value(Some(value), &message_label, version, Some(Expected::User))
            })?;
            if let Some(outcome) = field("outcome") {
                literal_value(Some(outcome), &[Text("canceled")], &at("outcome"))?;
            }
        }
        "approval/asked" => {
            non_empty_string(field("id"), &at("id"))?;
            non_empty_string(field("toolName"), &at("toolName"))?;
            if let Some(call) = field("callId") {
                non_empty_string(Some(call), &at("callId"))?;
            }
            if let Some(reason) = field("reason") {
                string_value(Some(reason), &at("reason"))?;
            }
        }
        "approval/decided" => {
            non_empty_string(field("id"), &at("id"))?;
            let outcomes = texts(&["allowed-once", "rejected", "cancelled", "unavailable"]);
            literal_value(field("outcome"), &outcomes, &at("outcome"))?;
        }
        "approval/policy" => {
            literal_value(field("policy"), &texts(&["ask", "never"]), &at("policy"))?;
            if let Some(source) = field("source") {
                literal_value(Some(source), &[Text("delegation")], &at("source"))?;
            }
        }
        "assistant/message" => {
            coordinate_pair(data, label)?;
            message_value(
                field("message"),
                &at("message"),
                version,
                Some(Expected::Assistant),
            )?;
            if let Some(usage) = field("usage") {
                token_usage(Some(usage), &at("usage"))?;
            }
            if let Some(interrupted) = field("interrupted") {
                literal_value(Some(interrupted), &[Flag(true)], &at("interrupted"))?;
            }
        }
        "command/done" => {
            non_empty_string(field("commandId"), &at("commandId"))?;
            literal_value(field("kind"), &texts(&["success", "error"]), &at("kind"))?;
            if let Some(text) = field("text") {
                string_value(Some(text), &at("text"))?;
            }
            if let Some(source) = field("sourceEventSeq") {
                earlier_seq(Some(source), seq, &at("sourceEventSeq"))?;
            }
        }
        "command/run" => {
            non_empty_string(field("commandId"), &at("commandId"))?;
            non_empty_string(field("name"), &at("name"))?;
            if let Some(args) = field("args") {
                string_value(Some(args), &at("args"))?;
            }
            let source = exact_record(field("source"), &at("source"), &["kind"], &[])?;
            literal_value(source.get("kind"), &[Text("user")], &at("source kind"))?;
        }
        "compaction/start" | "compaction/end" => {
            non_empty_string(field("compactionId"), &at("compactionId"))?;
            if let Some(command) = field("sourceCommandId") {
                non_empty_string(Some(command), &at("sourceCommandId"))?;
            }
            if field("turn") != Some(&Value::Null) {
                count(field("turn"), &at("turn"))?;
            }
            if let Some(error) = field("error") {
                string_value(Some(error), &at("error"))?;
            }
        }
        "compaction/prune" => shadowed_value(data, seq, label)?,
        "compaction/summary" => {
            if field("llmStreamCall") == Some(&Value::Bool(true)) && field("rawOutput").is_none() {
                return invalid(format!("{label} llmStreamCall requires rawOutput"));
            }
            non_empty_string(field("compactionId"), &at("compactionId"))?;
            if let Some(command) = field("sourceCommandId") {
                non_empty_string(Some(command), &at("sourceCommandId"))?;
            }
            content_blocks(field("summary"), &at("summary"))?;
            shadowed_value(data, seq, label)?;
            non_empty_string(field("provider"), &at("provider"))?;
            non_empty_string(field("model"), &at("model"))?;
            if let Some(tokens) = field("maxTokens") {
                count(Some(tokens), &at("maxTokens"))?;
            }
            if let Some(usage) = field("usage") {
                token_usage(Some(usage), &at("usage"))?;
            }
            if let Some(raw) = field("rawOutput") {
                content_blocks(Some(raw), &at("rawOutput"))?;
            }
            if let Some(stream) = field("llmStreamCall") {
                literal_value(Some(stream), &[Flag(true)], &at("llmStreamCall"))?;
            }
        }
        "feedback/record" => {
            non_empty_string(field("text"), &at("text"))?;
        }
        "goal/change" => goal_change(data, label)?,
        "hook/invoked" => {
            count(field("turn"), &at("turn"))?;
            non_empty_string(field("point"), &at("point"))?;
            literal_value(
                field("dialect"),
                &texts(&["claude-code", "codex"]),
                &at("dialect"),
            )?;
            if let Some(matcher) = field("matcher") {
                string_value(Some(matcher), &at("matcher"))?;
            }
            non_empty_string(field("handlerId"), &at("handlerId"))?;
        }
        "hook/result" => {
            count(field("turn"), &at("turn"))?;
            non_empty_string(field("point"), &at("point"))?;
            non_empty_string(field("handlerId"), &at("handlerId"))?;
            non_empty_string(field("decision"), &at("decision"))?;
            if let Some(code) = field("exitCode") {
                safe_integer(Some(code), &at("exitCode"))?;
            }
            if let Some(summary) = field("stderrSummary") {
                string_value(Some(summary), &at("stderrSummary"))?;
            }
            if finite_number(field("durationMs"), &at("durationMs"))? < 0.0 {
                return invalid(format!("{label} durationMs must be non-negative"));
            }
        }
        "llm/retry" => {
            non_empty_string(field("retryId"), &at("retryId"))?;
            coordinate_pair(data, label)?;
            non_empty_string(field("provider"), &at("provider"))?;
            literal_value(field("mode"), &texts(&["normal", "always"]), &at("mode"))?;
            non_empty_string(field("policyKey"), &at("policyKey"))?;
            let retry = positive_integer(field("retry"), &at("retry"))?;
            if field("mode") == Some(&Value::from("normal")) {
                let max_retries = positive_integer(field("maxRetries"), &at("maxRetries"))?;
                if retry > max_retries {
                    return invalid(format!("{label} retry exceeds maxRetries"));
                }
            } else if field("maxRetries").is_some() {
                return invalid(format!("{label} always mode must omit maxRetries"));
            }
            let delay = finite_number(field("delayMs"), &at("delayMs"))?;
            if delay < 0.0 {
                return invalid(format!("{label} delayMs must be non-negative"));
            }
            if delay > 2_147_483_647.0 {
                return invalid(format!("{label} delayMs exceeds the timer range"));
            }
            llm_failure(field("failure"), &at("failure"))?;
        }
        "llm/retry-started" => {
            non_empty_string(field("retryId"), &at("retryId"))?;
            coordinate_pair(data, label)?;
            positive_integer(field("retry"), &at("retry"))?;
        }
        "model/selection" => {
            non_empty_string(field("provider"), &at("provider"))?;
            non_empty_string(field("model"), &at("model"))?;
            if let Some(effort) = field("reasoningEffort") {
                non_empty_string(Some(effort), &at("reasoningEffort"))?;
            }
        }
        "permission/preset" => {
            non_empty_string(field("preset"), &at("preset"))?;
        }
        "plan/mode" => {
            boolean_value(field("active"), &at("active"))?;
        }
        "request/context" => {
            non_empty_string(field("provider"), &at("provider"))?;
            non_empty_string(field("model"), &at("model"))?;
            if let Some(window) = field("contextWindow") {
                positive_integer(Some(window), &at("contextWindow"))?;
            }
        }
        "request/header" => {
            request_header(field("header"), &at("header"))?;
            let reasons = texts(&["initial", "resume", "change", "series"]);
            literal_value(field("reason"), &reasons, &at("reason"))?;
            if let Some(starts) = field("startsSeries") {
                literal_value(Some(starts), &[Flag(true)], &at("startsSeries"))?;
            }
        }
        "sandbox/mode" => {
            let modes = texts(&["read-only", "workspace-write", "danger-full-access"]);
            literal_value(field("mode"), &modes, &at("mode"))?;
            if let Some(source) = field("source") {
                literal_value(Some(source), &[Text("delegation")], &at("source"))?;
            }
        }
        "schedule/change" => schedule_change(data, label)?,
        "session-log-deepseek/delivery-accepted" => {
            let accepted = match field("sessionFormatVersion") {
                None => 0,
                Some(accepted) => count(Some(accepted), &at("sessionFormatVersion"))?,
            };
            if accepted == u64::from(version) {
                non_empty_string(field("sessionId"), &at("sessionId"))?;
                earlier_seq(field("throughSeq"), seq, &at("throughSeq"))?;
            }
        }
        "session/end-seed" => {}
        "session/title" => {
            non_empty_string(field("title"), &at("title"))?;
            seq_array(field("messageSeqs"), seq, &at("messageSeqs"), false)?;
            title_source(field("source"), &at("source"))?;
        }
        "session/title-llm-request" => {
            non_empty_string(field("titleProvider"), &at("titleProvider"))?;
            seq_array(field("messageSeqs"), seq, &at("messageSeqs"), true)?;
            model_route(field("route"), &at("route"))?;
            string_value(field("system"), &at("system"))?;
            let message_label = at("message");
            array_value(field("messages"), &at("messages"), |value, _| {
                message_value(Some(value), &message_label, version, None)
            })?;
            positive_integer(field("maxTokens"), &at("maxTokens"))?;
        }
        "step/end" | "step/start" => coordinate_pair(data, label)?,
        "subagent/descriptor" => subagent_descriptor(data, label)?,
        "subagent/model-selection-policy" => {
            allowed_models(field("allowedModels"), &at("allowedModels"))?;
        }
        "team/member" => {
            team_selector(data, label)?;
            team_member(field("member"), &at("member"))?;
        }
        "team/message/delivered" => {
            team_selector(data, label)?;
            non_empty_string(field("messageId"), &at("messageId"))?;
            non_empty_string(field("targetId"), &at("targetId"))?;
        }
        "team/message/queued" => {
            team_selector(data, label)?;
            team_message(field("message"), &at("message"))?;
        }
        "team/task" => {
            team_selector(data, label)?;
            team_task(field("task"), &at("task"))?;
        }
        "todo/write" => {
            array_value(field("todos"), &at("todos"), |value, item_label| {
                let item = exact_record(Some(value), item_label, &["content", "status"], &[])?;
                string_value(item.get("content"), &format!("{item_label} content"))?;
                let statuses = texts(&["pending", "in_progress", "completed"]);
                literal_value(
                    item.get("status"),
                    &statuses,
                    &format!("{item_label} status"),
                )
            })?;
        }
        "tool-workflow/agent-end" => {
            workflow_identity(data, label)?;
            let outcomes = texts(&["completed", "failed", "cancelled"]);
            literal_value(field("outcome"), &outcomes, &at("outcome"))?;
        }
        "tool-workflow/agent-start" => {
            workflow_identity(data, label)?;
            string_value(field("label"), &at("label"))?;
            if let Some(phase) = field("phase") {
                string_value(Some(phase), &at("phase"))?;
            }
            non_empty_string(field("childId"), &at("childId"))?;
        }
        "tool-workflow/run-end" => {
            non_empty_string(field("runId"), &at("runId"))?;
            let reasons = texts(&["completed", "cancelled", "error"]);
            literal_value(field("stopReason"), &reasons, &at("stopReason"))?;
        }
        "tool-workflow/run-start" => {
            non_empty_string(field("runId"), &at("runId"))?;
            non_empty_string(field("name"), &at("name"))?;
        }
        "tool/call" => {
            coordinate_pair(data, label)?;
            non_empty_string(field("callId"), &at("callId"))?;
            non_empty_string(field("name"), &at("name"))?;
            string_value(field("arguments"), &at("arguments"))?;
        }
        // `arguments` stays opaque JSON.
        "tool/code-dispatch" | "tool/code-dispatch-start" => {
            for key in ["rootCallId", "parentCallId", "subCallId", "name"] {
                non_empty_string(field(key), &at(key))?;
            }
            if event_type == "tool/code-dispatch" {
                boolean_value(field("isError"), &at("isError"))?;
                content_blocks(field("content"), &at("content"))?;
            }
        }
        // `meta` stays opaque JSON.
        "tool/result" => {
            coordinate_pair(data, label)?;
            message_value(
                field("message"),
                &at("message"),
                version,
                Some(Expected::Tool),
            )?;
            if let Some(error) = field("error") {
                let error = exact_record(Some(error), &at("error"), &["name", "code"], &[])?;
                non_empty_string(error.get("name"), &at("error name"))?;
                non_empty_string(error.get("code"), &at("error code"))?;
            }
        }
        "turn/end" => {
            count(field("turn"), &at("turn"))?;
            turn_end_reason(field("reason"), &at("reason"))?;
        }
        "turn/start" => {
            count(field("turn"), &at("turn"))?;
        }
        "user/message" => {
            message_value(raw, label, version, Some(Expected::User))?;
        }
        "web/deepseek-search-llm-request" => {
            non_empty_string(field("endpoint"), &at("endpoint"))?;
            non_empty_string(field("apiVersion"), &at("apiVersion"))?;
            deep_seek_search_body(field("body"), &at("body"))?;
        }
        _ => {
            return invalid(format!(
                "released payload validator is missing event {}",
                quote(event_type)
            ));
        }
    }
    Ok(())
}

fn llm_failure(value: Option<&Value>, label: &str) -> Checked {
    let failure = exact_record(
        value,
        label,
        &["message", "code"],
        &["status", "providerRetryAfterMs", "requestId"],
    )?;
    non_empty_string(failure.get("message"), &format!("{label} message"))?;
    non_empty_string(failure.get("code"), &format!("{label} code"))?;
    if let Some(status) = failure.get("status") {
        let status = safe_integer(Some(status), &format!("{label} status"))?;
        if !(100..=599).contains(&status) {
            return invalid(format!("{label} status must be 100 through 599"));
        }
    }
    if let Some(after) = failure.get("providerRetryAfterMs")
        && finite_number(Some(after), &format!("{label} providerRetryAfterMs"))? <= 0.0
    {
        return invalid(format!("{label} providerRetryAfterMs must be positive"));
    }
    if let Some(request) = failure.get("requestId") {
        non_empty_string(Some(request), &format!("{label} requestId"))?;
    }
    Ok(())
}

fn token_usage(value: Option<&Value>, label: &str) -> Checked {
    let usage = exact_record(
        value,
        label,
        &["inputTokens", "outputTokens"],
        &[
            "totalTokens",
            "cacheReadTokens",
            "cacheWriteTokens",
            "reasoningTokens",
        ],
    )?;
    for key in js_keys(usage) {
        count(usage.get(key), &format!("{label} {key}"))?;
    }
    Ok(())
}

fn content_blocks(value: Option<&Value>, label: &str) -> Checked {
    let Some(Value::Array(blocks)) = value else {
        return invalid(format!("{label} must be an array"));
    };
    walk_content_blocks(
        blocks,
        label,
        " content",
        |block, label| {
            let fields = lazily(label, |label| released_record(Some(block), label))?;
            lazily(label, |label| content_block_head(fields, label))?;
            if fields.get("type").and_then(Value::as_str) != Some("tool-result") {
                return Ok(None);
            }
            match fields.get("content") {
                Some(Value::Array(nested)) => Ok(Some(nested)),
                _ => invalid(format!("{} content must be an array", label())),
            }
        },
        |block, label| match block {
            Value::Object(fields) => lazily(label, |label| content_block_tail(fields, label)),
            _ => Ok(()),
        },
    )
}

/// Runs `check` with an empty label and formats the real label only when the
/// check refuses, so a deep `tool-result` chain costs no label per level. The
/// validators are pure, and whether they refuse never depends on the label.
pub(super) fn lazily<T>(
    label: &dyn Fn() -> String,
    check: impl Fn(&str) -> Checked<T>,
) -> Checked<T> {
    match check("") {
        Ok(value) => Ok(value),
        Err(_) => check(&label()),
    }
}

/// Visits `blocks` and the `tool-result` content they nest in the recursive
/// validators' depth-first order, from an explicit stack, so a chain of any
/// depth cannot overflow. `enter` checks a block before its nested content
/// and returns that content to descend into; `leave` runs after it. Both get
/// the block's label, `{base}[i]` then `{separator}[j]` per level, built only
/// when asked for.
pub(super) fn walk_content_blocks<'a>(
    blocks: &'a [Value],
    base: &str,
    separator: &str,
    mut enter: impl FnMut(&'a Value, &dyn Fn() -> String) -> Checked<Option<&'a [Value]>>,
    mut leave: impl FnMut(&'a Value, &dyn Fn() -> String) -> Checked,
) -> Checked {
    // Each frame holds one content list and the index of its next block.
    let mut stack: Vec<(&'a [Value], usize)> = vec![(blocks, 0)];
    let label_of = |stack: &[(&'a [Value], usize)]| {
        let mut label = base.to_owned();
        for (depth, (_, next)) in stack.iter().enumerate() {
            if depth > 0 {
                label.push_str(separator);
            }
            label.push_str(&format!("[{}]", next.saturating_sub(1)));
        }
        label
    };
    while let Some(&(members, next)) = stack.last() {
        let Some(block) = members.get(next) else {
            stack.pop();
            if let Some(&(owners, owner_next)) = stack.last()
                && let Some(owner) = owners.get(owner_next.saturating_sub(1))
            {
                leave(owner, &|| label_of(&stack))?;
            }
            continue;
        };
        if let Some(top) = stack.last_mut() {
            top.1 = next + 1;
        }
        let nested = enter(block, &|| label_of(&stack))?;
        match nested {
            Some(nested) => stack.push((nested, 0)),
            None => leave(block, &|| label_of(&stack))?,
        }
    }
    Ok(())
}

/// `contentBlockValue` after its object check. Unknown block kinds need only
/// a non-empty `type`.
pub(super) fn content_block_fields(block: &Record, label: &str) -> Checked {
    content_block_head(block, label)?;
    if block.get("type").and_then(Value::as_str) == Some("tool-result") {
        content_blocks(block.get("content"), &format!("{label} content"))?;
    }
    content_block_tail(block, label)
}

/// The checks `contentBlockValue` makes before a `tool-result` block's
/// nested content.
fn content_block_head(block: &Record, label: &str) -> Checked {
    match block.get("type").and_then(Value::as_str) {
        Some("text" | "reasoning") => {
            released_keys(block, &["type", "text"], &[], label)?;
            string_value(block.get("text"), &format!("{label} text"))?;
        }
        Some("image") => {
            released_keys(block, &["type", "attachment"], &[], label)?;
            image_attachment(block.get("attachment"), &format!("{label} attachment"))?;
        }
        Some("tool-call") => {
            released_keys(block, &["type", "id", "name", "arguments"], &[], label)?;
            non_empty_string(block.get("id"), &format!("{label} id"))?;
            non_empty_string(block.get("name"), &format!("{label} name"))?;
            string_value(block.get("arguments"), &format!("{label} arguments"))?;
        }
        Some("tool-result") => {
            released_keys(
                block,
                &["type", "toolCallId", "content"],
                &["isError"],
                label,
            )?;
            non_empty_string(block.get("toolCallId"), &format!("{label} toolCallId"))?;
        }
        _ => {
            non_empty_string(block.get("type"), &format!("{label} type"))?;
        }
    }
    Ok(())
}

/// The check `contentBlockValue` makes after a `tool-result` block's nested
/// content.
fn content_block_tail(block: &Record, label: &str) -> Checked {
    if block.get("type").and_then(Value::as_str) == Some("tool-result")
        && let Some(error) = block.get("isError")
    {
        boolean_value(Some(error), &format!("{label} isError"))?;
    }
    Ok(())
}

fn image_attachment(value: Option<&Value>, label: &str) -> Checked {
    let attachment = exact_record(
        value,
        label,
        &["attachmentId", "mediaType", "bytes", "width", "height"],
        &["name", "originalDimensions"],
    )?;
    let at = |key: &str| format!("{label} {key}");
    non_empty_string(attachment.get("attachmentId"), &at("attachmentId"))?;
    let media = texts(&["image/png", "image/jpeg", "image/webp", "image/gif"]);
    literal_value(attachment.get("mediaType"), &media, &at("mediaType"))?;
    count(attachment.get("bytes"), &at("bytes"))?;
    positive_integer(attachment.get("width"), &at("width"))?;
    positive_integer(attachment.get("height"), &at("height"))?;
    if let Some(name) = attachment.get("name") {
        string_value(Some(name), &at("name"))?;
    }
    if let Some(dimensions) = attachment.get("originalDimensions") {
        let dimensions = exact_record(
            Some(dimensions),
            &at("originalDimensions"),
            &["width", "height"],
            &[],
        )?;
        positive_integer(dimensions.get("width"), &at("original width"))?;
        positive_integer(dimensions.get("height"), &at("original height"))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    User,
    Assistant,
    Tool,
}

fn message_value(
    value: Option<&Value>,
    label: &str,
    version: u8,
    expected: Option<Expected>,
) -> Checked {
    let message = exact_record(value, label, &["id", "role", "content", "source"], &[])?;
    non_empty_string(message.get("id"), &format!("{label} id"))?;
    let roles: &[Literal] = match expected {
        Some(Expected::Assistant) => &[Text("assistant")],
        Some(Expected::User | Expected::Tool) => &[Text("user")],
        None => &[Text("system"), Text("user"), Text("assistant")],
    };
    literal_value(message.get("role"), roles, &format!("{label} role"))?;
    content_blocks(message.get("content"), &format!("{label} content"))?;
    let source = released_record(message.get("source"), &format!("{label} source"))?;
    if version < 2
        && expected == Some(Expected::User)
        && source.get("kind") == Some(&Value::from("goal"))
        && source.contains_key("change")
    {
        return Err(StageError::NativeLimit(LEGACY_GOAL_MESSAGE.to_owned()));
    }
    message_source(source, &format!("{label} source"), version, expected)?;
    if expected == Some(Expected::Tool) {
        // Content blocks were proven objects above.
        let block = match message.get("content") {
            Some(Value::Array(content)) if content.len() == 1 => content[0].as_object(),
            _ => None,
        };
        let matches = block.is_some_and(|block| {
            block.get("type") == Some(&Value::from("tool-result"))
                && block.get("toolCallId") == source.get("callId")
        });
        if !matches {
            return invalid(format!(
                "{label} must contain exactly one tool-result block"
            ));
        }
    }
    Ok(())
}

fn message_source(
    source: &Record,
    label: &str,
    version: u8,
    expected: Option<Expected>,
) -> Checked {
    let kind = source.get("kind");
    if expected == Some(Expected::Assistant) && kind != Some(&Value::from("model")) {
        return invalid(format!("{label} must be model source"));
    }
    if expected == Some(Expected::Tool) && kind != Some(&Value::from("tool")) {
        return invalid(format!("{label} must be tool source"));
    }
    let at = |key: &str| format!("{label} {key}");
    let keys =
        |required: &[&str], optional: &[&str]| released_keys(source, required, optional, label);
    match kind.and_then(Value::as_str) {
        Some("user") => {
            keys(&["kind"], &["rpcId", "clientTimeZone"])?;
            for key in ["rpcId", "clientTimeZone"] {
                if let Some(value) = source.get(key) {
                    non_empty_string(Some(value), &at(key))?;
                }
            }
        }
        Some("plugin") => plugin_source(source, label)?,
        Some("model") => {
            keys(&["kind", "provider", "model"], &["replayState"])?;
            non_empty_string(source.get("provider"), &at("provider"))?;
            non_empty_string(source.get("model"), &at("model"))?;
        }
        Some("tool") => {
            keys(&["kind", "callId"], &[])?;
            non_empty_string(source.get("callId"), &at("callId"))?;
        }
        Some("agent-instructions") => {
            keys(
                &["kind", "form", "changes"],
                &["baseline", "baselineIdentity"],
            )?;
            literal_value(source.get("form"), &[Text("instructions")], &at("form"))?;
            if let Some(baseline) = source.get("baseline") {
                literal_value(Some(baseline), &[Flag(true)], &at("baseline"))?;
            }
            if let Some(identity) = source.get("baselineIdentity") {
                non_empty_string(Some(identity), &at("baselineIdentity"))?;
            }
            array_value(
                source.get("changes"),
                &at("changes"),
                |member, member_label| {
                    let change = exact_record(
                        Some(member),
                        member_label,
                        &["action", "scope", "path"],
                        &["digest"],
                    )?;
                    let actions = texts(&["set", "replace", "remove"]);
                    literal_value(
                        change.get("action"),
                        &actions,
                        &format!("{member_label} action"),
                    )?;
                    string_value(change.get("scope"), &format!("{member_label} scope"))?;
                    string_value(change.get("path"), &format!("{member_label} path"))?;
                    if let Some(digest) = change.get("digest") {
                        string_value(Some(digest), &format!("{member_label} digest"))?;
                    }
                    Ok(())
                },
            )?;
        }
        Some("session-reference") => session_reference_source(source, label, version)?,
        Some("team-message") => {
            keys(
                &["kind", "teamId", "messageId", "senderId", "senderName"],
                &[],
            )?;
            for key in ["teamId", "messageId", "senderId"] {
                non_empty_string(source.get(key), &at(key))?;
            }
            string_value(source.get("senderName"), &at("senderName"))?;
        }
        Some("goal") => {
            keys(&["kind", "goalId", "revision", "round"], &[])?;
            non_empty_string(source.get("goalId"), &at("goalId"))?;
            positive_integer(source.get("revision"), &at("revision"))?;
            positive_integer(source.get("round"), &at("round"))?;
        }
        Some("skill-invocation") => {
            keys(&["kind", "name", "form"], &[])?;
            non_empty_string(source.get("name"), &at("name"))?;
            literal_value(source.get("form"), &[Text("instructions")], &at("form"))?;
        }
        Some("skill-catalog") => {
            keys(&["kind", "form", "entries"], &["update"])?;
            literal_value(source.get("form"), &[Text("catalog")], &at("form"))?;
            if let Some(update) = source.get("update") {
                literal_value(Some(update), &[Flag(true)], &at("update"))?;
            }
            array_value(
                source.get("entries"),
                &at("entries"),
                |member, member_label| {
                    let entry =
                        exact_record(Some(member), member_label, &["name", "description"], &[])?;
                    non_empty_string(entry.get("name"), &format!("{member_label} name"))?;
                    string_value(
                        entry.get("description"),
                        &format!("{member_label} description"),
                    )?;
                    Ok(())
                },
            )?;
        }
        Some("coordinator" | "subagent-report") => {
            keys(&["kind", "form", "senderSessionId"], &[])?;
            literal_value(source.get("form"), &[Text("relay")], &at("form"))?;
            non_empty_string(source.get("senderSessionId"), &at("senderSessionId"))?;
        }
        Some("subagent-settled") => {
            keys(&["kind", "form", "summary", "senderSessionId"], &[])?;
            literal_value(source.get("form"), &[Text("notice")], &at("form"))?;
            string_value(source.get("summary"), &at("summary"))?;
            non_empty_string(source.get("senderSessionId"), &at("senderSessionId"))?;
        }
        Some("webhook") => {
            keys(
                &[
                    "kind",
                    "provider",
                    "source",
                    "deliveryId",
                    "ruleId",
                    "form",
                    "summary",
                ],
                &[],
            )?;
            for key in ["provider", "source", "deliveryId", "ruleId"] {
                non_empty_string(source.get(key), &at(key))?;
            }
            literal_value(source.get("form"), &[Text("notice")], &at("form"))?;
            string_value(source.get("summary"), &at("summary"))?;
        }
        _ => {
            non_empty_string(kind, &at("kind"))?;
        }
    }
    Ok(())
}

fn plugin_source(source: &Record, label: &str) -> Checked {
    let compact = source.get("plugin") == Some(&Value::from("compact"));
    let mut optional = vec!["form", "sections", "summary"];
    if compact {
        optional.extend(["compactionId", "sourceCommandId"]);
    }
    released_keys(source, &["kind", "plugin"], &optional, label)?;
    let at = |key: &str| format!("{label} {key}");
    non_empty_string(source.get("plugin"), &at("plugin"))?;
    if compact {
        non_empty_string(source.get("compactionId"), &at("compactionId"))?;
        if let Some(command) = source.get("sourceCommandId") {
            non_empty_string(Some(command), &at("sourceCommandId"))?;
        }
    }
    let Some(form) = source.get("form") else {
        return Ok(());
    };
    let forms = texts(&[
        "instructions",
        "catalog",
        "snapshot",
        "notice",
        "relay",
        "recall",
    ]);
    literal_value(Some(form), &forms, &at("form"))?;
    if form == "snapshot" {
        array_value(
            source.get("sections"),
            &at("sections"),
            |member, member_label| {
                let section = exact_record(Some(member), member_label, &["name", "text"], &[])?;
                non_empty_string(section.get("name"), &format!("{member_label} name"))?;
                string_value(section.get("text"), &format!("{member_label} text"))?;
                Ok(())
            },
        )?;
    } else if source.contains_key("sections") {
        return invalid(format!("{label} sections require snapshot form"));
    }
    if form == "notice" {
        string_value(source.get("summary"), &at("summary"))?;
    } else if source.contains_key("summary") {
        return invalid(format!("{label} summary requires notice form"));
    }
    Ok(())
}

/// `sessionReferenceSourceValue`: `capturedFormatVersion` is admitted from
/// version 1 and bounded by the version.
fn session_reference_source(source: &Record, label: &str, version: u8) -> Checked {
    released_keys(
        source,
        &["kind", "form", "version", "references"],
        &[],
        label,
    )?;
    literal_value(
        source.get("form"),
        &[Text("recall")],
        &format!("{label} form"),
    )?;
    literal_value(
        source.get("version"),
        &[Int(1)],
        &format!("{label} version"),
    )?;
    let mut expected_input_index = 0;
    let mut session_ids = HashSet::new();
    let references = array_value(
        source.get("references"),
        &format!("{label} references"),
        |member, member_label| {
            let reference = exact_record(
                Some(member),
                member_label,
                &[
                    "sessionId",
                    "label",
                    "capturedThroughSeq",
                    "compacted",
                    "originalMessages",
                    "retainedMessages",
                    "omittedMessages",
                    "omittedBytes",
                    "truncated",
                    "inputIndex",
                ],
                if version >= 1 {
                    &["capturedFormatVersion"]
                } else {
                    &[]
                },
            )?;
            let at = |key: &str| format!("{member_label} {key}");
            let session_id = non_empty_string(reference.get("sessionId"), &at("sessionId"))?;
            string_value(reference.get("label"), &at("label"))?;
            let captured = reference.get("capturedThroughSeq");
            if captured != Some(&Value::Null) {
                count(captured, &at("capturedThroughSeq"))?;
            }
            if let Some(captured) = reference.get("capturedFormatVersion") {
                let captured_version = count(Some(captured), &at("capturedFormatVersion"))?;
                if !(1..=u64::from(version)).contains(&captured_version) {
                    return invalid(format!(
                        "{member_label} capturedFormatVersion must be between 1 and {version}"
                    ));
                }
            }
            boolean_value(reference.get("compacted"), &at("compacted"))?;
            let original = count(reference.get("originalMessages"), &at("originalMessages"))?;
            let retained = count(reference.get("retainedMessages"), &at("retainedMessages"))?;
            let omitted = count(reference.get("omittedMessages"), &at("omittedMessages"))?;
            let omitted_bytes = count(reference.get("omittedBytes"), &at("omittedBytes"))?;
            let input_index = count(reference.get("inputIndex"), &at("inputIndex"))?;
            let truncated = boolean_value(reference.get("truncated"), &at("truncated"))?;
            if retained > original || omitted != original - retained {
                return invalid(format!("{member_label} message counts are inconsistent"));
            }
            if truncated != (omitted > 0 || omitted_bytes > 0) {
                return invalid(format!(
                    "{member_label} truncated disagrees with omitted content"
                ));
            }
            if input_index != expected_input_index {
                return invalid(format!("{label} inputIndex must match reference position"));
            }
            expected_input_index += 1;
            if !session_ids.insert(session_id) {
                return invalid(format!("{label} repeats sessionId {session_id}"));
            }
            Ok(())
        },
    )?;
    if references.is_empty() {
        return invalid(format!("{label} references must be non-empty"));
    }
    Ok(())
}

fn turn_end_reason(value: Option<&Value>, label: &str) -> Checked {
    let reason = released_record(value, label)?;
    match reason.get("kind").and_then(Value::as_str) {
        Some("completed" | "blocked" | "max-tokens" | "interrupted") => {
            released_keys(reason, &["kind"], &[], label)?;
        }
        Some("aborted") => {
            released_keys(reason, &["kind", "reason"], &[], label)?;
            let cause_label = format!("{label} abort cause");
            let cause = released_record(reason.get("reason"), &cause_label)?;
            if cause.get("kind") == Some(&Value::from("hook")) {
                released_keys(cause, &["kind", "reason"], &[], &cause_label)?;
                string_value(cause.get("reason"), &format!("{label} abort reason"))?;
            } else {
                released_keys(cause, &["kind"], &[], &cause_label)?;
                let kinds = texts(&["user", "parent", "disposed", "legacy"]);
                literal_value(cause.get("kind"), &kinds, &format!("{label} abort kind"))?;
            }
        }
        Some("error") => {
            released_keys(reason, &["kind", "error"], &[], label)?;
            llm_failure(reason.get("error"), &format!("{label} error"))?;
        }
        _ => {
            non_empty_string(reason.get("kind"), &format!("{label} kind"))?;
        }
    }
    Ok(())
}

fn request_header(value: Option<&Value>, label: &str) -> Checked {
    let header = exact_record(
        value,
        label,
        &["config"],
        &["adapterDefaults", "system", "tools"],
    )?;
    let config = exact_record(
        header.get("config"),
        &format!("{label} config"),
        &["provider", "model"],
        &["reasoningEffort", "temperature", "maxTokens", "stop"],
    )?;
    let at = |key: &str| format!("{label} {key}");
    non_empty_string(config.get("provider"), &at("provider"))?;
    non_empty_string(config.get("model"), &at("model"))?;
    if let Some(effort) = config.get("reasoningEffort") {
        non_empty_string(Some(effort), &at("reasoningEffort"))?;
    }
    if let Some(temperature) = config.get("temperature") {
        finite_number(Some(temperature), &at("temperature"))?;
    }
    if let Some(tokens) = config.get("maxTokens") {
        positive_integer(Some(tokens), &at("maxTokens"))?;
    }
    if let Some(stop) = config.get("stop") {
        array_value(Some(stop), &at("stop"), |member, member_label| {
            string_value(Some(member), member_label).map(drop)
        })?;
    }
    if let Some(defaults) = header.get("adapterDefaults") {
        let defaults = exact_record(
            Some(defaults),
            &at("adapterDefaults"),
            &[],
            &["reasoningEffort", "maxTokens"],
        )?;
        for key in js_keys(defaults) {
            literal_value(
                defaults.get(key),
                &[Flag(true)],
                &format!("{label} adapterDefaults {key}"),
            )?;
            if !config.contains_key(key) {
                return invalid(format!("{label} adapter default {key} lacks config value"));
            }
        }
    }
    if let Some(system) = header.get("system") {
        string_value(Some(system), &at("system"))?;
    }
    if let Some(tools) = header.get("tools") {
        array_value(Some(tools), &at("tools"), |member, member_label| {
            let schema = exact_record(
                Some(member),
                member_label,
                &["name", "description", "parameters"],
                &[],
            )?;
            non_empty_string(schema.get("name"), &format!("{member_label} name"))?;
            string_value(
                schema.get("description"),
                &format!("{member_label} description"),
            )?;
            released_record(
                schema.get("parameters"),
                &format!("{member_label} parameters"),
            )?;
            Ok(())
        })?;
    }
    Ok(())
}

fn shadowed_value(data: &Record, event_seq: u64, label: &str) -> Checked {
    let range_label = format!("{label} shadowedRange");
    let range = exact_record(
        data.get("shadowedRange"),
        &range_label,
        &["start", "end"],
        &[],
    )?;
    let start = earlier_seq(
        range.get("start"),
        event_seq,
        &format!("{range_label} start"),
    )?;
    let end = earlier_seq(range.get("end"), event_seq, &format!("{range_label} end"))?;
    let seqs = seq_array(
        data.get("shadowedSeqs"),
        event_seq,
        &format!("{label} shadowedSeqs"),
        true,
    )?;
    if seqs.first() != Some(&start) || seqs.last() != Some(&end) {
        return invalid(format!(
            "{label} shadowedRange must match shadowedSeqs endpoints"
        ));
    }
    count(
        data.get("shadowedTokenCount"),
        &format!("{label} shadowedTokenCount"),
    )?;
    Ok(())
}

fn goal_change(data: &Record, label: &str) -> Checked {
    let at = |key: &str| format!("{label} {key}");
    literal_value(data.get("kind"), &[Text("goal/change")], &at("kind"))?;
    literal_value(data.get("version"), &[Int(1)], &at("version"))?;
    if data.get("operation") == Some(&Value::from("clear")) {
        released_keys(
            data,
            &["kind", "version", "operation", "cleared", "clearedAt"],
            &[],
            &at("data"),
        )?;
        let cleared = exact_record(
            data.get("cleared"),
            &at("cleared"),
            &["id", "revision"],
            &[],
        )?;
        non_empty_string(cleared.get("id"), &at("cleared id"))?;
        positive_integer(cleared.get("revision"), &at("cleared revision"))?;
        count(data.get("clearedAt"), &at("clearedAt"))?;
        return Ok(());
    }
    released_keys(
        data,
        &[
            "kind",
            "version",
            "operation",
            "goal",
            "roundsStarted",
            "createdAt",
            "updatedAt",
        ],
        &[],
        &at("data"),
    )?;
    let operations = texts(&["create", "edit", "pause", "resume", "complete", "block"]);
    literal_value(data.get("operation"), &operations, &at("operation"))?;
    goal_snapshot(data.get("goal"), &at("goal"))?;
    count(data.get("roundsStarted"), &at("roundsStarted"))?;
    count(data.get("createdAt"), &at("createdAt"))?;
    count(data.get("updatedAt"), &at("updatedAt"))?;
    Ok(())
}

fn goal_snapshot(value: Option<&Value>, label: &str) -> Checked {
    let goal = exact_record(
        value,
        label,
        &["id", "revision", "objective", "phase", "maxGoalRounds"],
        &["blockedReason"],
    )?;
    let at = |key: &str| format!("{label} {key}");
    non_empty_string(goal.get("id"), &at("id"))?;
    positive_integer(goal.get("revision"), &at("revision"))?;
    non_empty_string(goal.get("objective"), &at("objective"))?;
    let phases = texts(&["active", "paused", "blocked", "complete"]);
    literal_value(goal.get("phase"), &phases, &at("phase"))?;
    positive_integer(goal.get("maxGoalRounds"), &at("maxGoalRounds"))?;
    if goal.get("phase") == Some(&Value::from("blocked")) {
        let reason = exact_record(
            goal.get("blockedReason"),
            &at("blockedReason"),
            &["code", "message"],
            &[],
        )?;
        non_empty_string(reason.get("code"), &at("blocked code"))?;
        non_empty_string(reason.get("message"), &at("blocked message"))?;
    } else if goal.contains_key("blockedReason") {
        return invalid(format!("{label} blockedReason requires blocked phase"));
    }
    Ok(())
}

fn schedule_change(data: &Record, label: &str) -> Checked {
    let at = |key: &str| format!("{label} {key}");
    literal_value(data.get("version"), &[Int(1)], &at("version"))?;
    if data.get("operation") == Some(&Value::from("create")) {
        released_keys(
            data,
            &["version", "operation", "schedule"],
            &[],
            &at("data"),
        )?;
        return schedule_record(data.get("schedule"), &at("schedule"));
    }
    let optional: &[&str] = if data.get("operation") == Some(&Value::from("dispatch")) {
        &["acceptedAt"]
    } else {
        &[]
    };
    released_keys(data, &["version", "operation", "id"], optional, &at("data"))?;
    literal_value(
        data.get("operation"),
        &texts(&["delete", "dispatch"]),
        &at("operation"),
    )?;
    schedule_id(data.get("id"), &at("id"))?;
    if let Some(accepted) = data.get("acceptedAt") {
        instant(Some(accepted), &at("acceptedAt"))?;
    }
    Ok(())
}

fn schedule_record(value: Option<&Value>, label: &str) -> Checked {
    let record = released_record(value, label)?;
    let at = |key: &str| format!("{label} {key}");
    match record.get("kind").and_then(Value::as_str) {
        Some("after") => {
            released_keys(
                record,
                &["id", "kind", "prompt", "afterSeconds", "scheduledAt"],
                &[],
                label,
            )?;
            positive_integer(record.get("afterSeconds"), &at("afterSeconds"))?;
        }
        Some("at") => {
            released_keys(record, &["id", "kind", "prompt", "scheduledAt"], &[], label)?;
        }
        Some("every") => {
            released_keys(
                record,
                &["id", "kind", "prompt", "everySeconds", "scheduledAt"],
                &[],
                label,
            )?;
            if positive_integer(record.get("everySeconds"), &at("everySeconds"))? < 300 {
                return invalid(format!("{label} everySeconds must be at least 300"));
            }
        }
        _ => return invalid(format!("{label} has unknown schedule kind")),
    }
    schedule_id(record.get("id"), &at("id"))?;
    non_empty_string(record.get("prompt"), &at("prompt"))?;
    instant(record.get("scheduledAt"), &at("scheduledAt"))
}

fn schedule_id(value: Option<&Value>, label: &str) -> Checked {
    let id = non_empty_string(value, label)?;
    if id.starts_with(is_js_whitespace) || id.ends_with(is_js_whitespace) {
        return invalid(format!("{label} must not have surrounding whitespace"));
    }
    Ok(())
}

/// What `String.prototype.trim` removes: WhiteSpace and LineTerminator.
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}' | '\u{a}' | '\u{b}' | '\u{c}' | '\u{d}' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `instantValue`: the `YYYY-MM-DDTHH:mm:ss.sssZ` pattern with a year other
/// than 0000, which `Date` round-trips through `toISOString` exactly when the
/// day exists in that month of the proleptic Gregorian calendar.
fn instant(value: Option<&Value>, label: &str) -> Checked {
    let canonical = value.and_then(Value::as_str).is_some_and(|text| {
        let b = text.as_bytes();
        let digits = |range: std::ops::Range<usize>| b[range].iter().all(u8::is_ascii_digit);
        let number = |range: std::ops::Range<usize>| {
            b[range]
                .iter()
                .fold(0_u32, |total, digit| total * 10 + u32::from(digit - b'0'))
        };
        if b.len() != 24
            || !digits(0..4)
            || b[4] != b'-'
            || !digits(5..7)
            || b[7] != b'-'
            || !digits(8..10)
            || b[10] != b'T'
            || !digits(11..13)
            || b[13] != b':'
            || !digits(14..16)
            || b[16] != b':'
            || !digits(17..19)
            || b[19] != b'.'
            || !digits(20..23)
            || b[23] != b'Z'
        {
            return false;
        }
        let (year, month, day) = (number(0..4), number(5..7), number(8..10));
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days = match month {
            2 if leap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        year != 0
            && (1..=12).contains(&month)
            && (1..=days).contains(&day)
            && number(11..13) <= 23
            && number(14..16) <= 59
            && number(17..19) <= 59
    });
    if canonical {
        Ok(())
    } else {
        invalid(format!("{label} must be a canonical UTC instant"))
    }
}

fn title_source(value: Option<&Value>, label: &str) -> Checked {
    let source = released_record(value, label)?;
    if source.get("kind") == Some(&Value::from("provider")) {
        released_keys(source, &["kind", "provider"], &["model"], label)?;
        non_empty_string(source.get("provider"), &format!("{label} provider"))?;
        if let Some(model) = source.get("model") {
            model_route(Some(model), &format!("{label} model"))?;
        }
        return Ok(());
    }
    released_keys(source, &["kind"], &[], label)?;
    literal_value(
        source.get("kind"),
        &texts(&["fallback", "user"]),
        &format!("{label} kind"),
    )
}

fn model_route(value: Option<&Value>, label: &str) -> Checked {
    let route = exact_record(value, label, &["provider", "model"], &[])?;
    non_empty_string(route.get("provider"), &format!("{label} provider"))?;
    non_empty_string(route.get("model"), &format!("{label} model"))?;
    Ok(())
}

fn subagent_descriptor(data: &Record, label: &str) -> Checked {
    let at = |key: &str| format!("{label} {key}");
    literal_value(data.get("version"), &[Int(3)], &at("version"))?;
    non_empty_string(data.get("provider"), &at("provider"))?;
    if data.get("mode") == Some(&Value::from("one-shot")) {
        released_keys(
            data,
            &["mode", "version", "provider"],
            &["label"],
            &at("data"),
        )?;
        if let Some(name) = data.get("label") {
            string_value(Some(name), &at("label"))?;
        }
        return Ok(());
    }
    literal_value(data.get("mode"), &[Text("continuable")], &at("mode"))?;
    non_empty_string(data.get("label"), &at("label"))?;
    for key in [
        "agentProvider",
        "agentModel",
        "agentReasoningEffort",
        "persona",
    ] {
        if let Some(value) = data.get(key) {
            non_empty_string(Some(value), &at(key))?;
        }
    }
    if data.contains_key("agentProvider") != data.contains_key("agentModel") {
        return invalid(format!(
            "{label} agentProvider and agentModel must be paired"
        ));
    }
    if let Some(filter) = data.get("toolFilter") {
        let filter = exact_record(Some(filter), &at("toolFilter"), &[], &["allow", "deny"])?;
        if filter.is_empty() {
            return invalid(format!("{label} toolFilter requires allow or deny"));
        }
        for key in ["allow", "deny"] {
            if let Some(list) = filter.get(key) {
                array_value(Some(list), &at(key), |member, member_label| {
                    non_empty_string(Some(member), member_label).map(drop)
                })?;
            }
        }
    }
    Ok(())
}

fn allowed_models(value: Option<&Value>, label: &str) -> Checked {
    let mut seen = HashSet::new();
    let routes = array_value(value, label, |member, member_label| {
        let route = exact_record(Some(member), member_label, &["provider", "model"], &[])?;
        let provider =
            non_empty_string(route.get("provider"), &format!("{member_label} provider"))?;
        let model = non_empty_string(route.get("model"), &format!("{member_label} model"))?;
        let key = format!("{provider}\0{model}");
        if seen.contains(&key) {
            return invalid(format!("{label} repeats route {key}"));
        }
        seen.insert(key);
        Ok(())
    })?;
    if routes.is_empty() {
        return invalid(format!("{label} must be non-empty"));
    }
    Ok(())
}

fn team_selector(data: &Record, label: &str) -> Checked {
    literal_value(data.get("version"), &[Int(1)], &format!("{label} version"))?;
    non_empty_string(data.get("teamId"), &format!("{label} teamId"))?;
    Ok(())
}

fn team_member(value: Option<&Value>, label: &str) -> Checked {
    let member = exact_record(
        value,
        label,
        &["id", "name", "description", "provider", "context", "phase"],
        &["error"],
    )?;
    let at = |key: &str| format!("{label} {key}");
    non_empty_string(member.get("id"), &at("id"))?;
    for key in ["name", "description", "provider"] {
        string_value(member.get(key), &at(key))?;
    }
    literal_value(
        member.get("context"),
        &texts(&["fresh", "fork"]),
        &at("context"),
    )?;
    let phases = texts(&["provisioning", "active", "failed"]);
    literal_value(member.get("phase"), &phases, &at("phase"))?;
    if let Some(error) = member.get("error") {
        string_value(Some(error), &at("error"))?;
    }
    Ok(())
}

fn team_task(value: Option<&Value>, label: &str) -> Checked {
    let task = exact_record(
        value,
        label,
        &[
            "id",
            "revision",
            "subject",
            "description",
            "status",
            "blockedBy",
            "writeScopes",
        ],
        &["ownerId"],
    )?;
    let at = |key: &str| format!("{label} {key}");
    non_empty_string(task.get("id"), &at("id"))?;
    positive_integer(task.get("revision"), &at("revision"))?;
    string_value(task.get("subject"), &at("subject"))?;
    string_value(task.get("description"), &at("description"))?;
    let statuses = texts(&["pending", "in_progress", "completed", "deleted"]);
    literal_value(task.get("status"), &statuses, &at("status"))?;
    if let Some(owner) = task.get("ownerId") {
        non_empty_string(Some(owner), &at("ownerId"))?;
    }
    array_value(
        task.get("blockedBy"),
        &at("blockedBy"),
        |member, member_label| non_empty_string(Some(member), member_label).map(drop),
    )?;
    array_value(
        task.get("writeScopes"),
        &at("writeScopes"),
        |member, member_label| string_value(Some(member), member_label).map(drop),
    )?;
    Ok(())
}

fn team_message(value: Option<&Value>, label: &str) -> Checked {
    let message = exact_record(
        value,
        label,
        &[
            "id",
            "senderId",
            "senderName",
            "targetId",
            "delivery",
            "content",
        ],
        &[],
    )?;
    let at = |key: &str| format!("{label} {key}");
    for key in ["id", "senderId", "targetId"] {
        non_empty_string(message.get(key), &at(key))?;
    }
    string_value(message.get("senderName"), &at("senderName"))?;
    literal_value(
        message.get("delivery"),
        &texts(&["quiet", "wakeup"]),
        &at("delivery"),
    )?;
    content_blocks(message.get("content"), &at("content"))
}

fn workflow_identity(data: &Record, label: &str) -> Checked {
    non_empty_string(data.get("runId"), &format!("{label} runId"))?;
    positive_integer(data.get("seq"), &format!("{label} seq"))?;
    Ok(())
}

fn deep_seek_search_body(value: Option<&Value>, label: &str) -> Checked {
    let body = exact_record(
        value,
        label,
        &["model", "max_tokens", "messages", "tools"],
        &[],
    )?;
    non_empty_string(body.get("model"), &format!("{label} model"))?;
    positive_integer(body.get("max_tokens"), &format!("{label} max_tokens"))?;
    let messages = array_value(
        body.get("messages"),
        &format!("{label} messages"),
        |member, member_label| {
            let message = exact_record(Some(member), member_label, &["role", "content"], &[])?;
            literal_value(
                message.get("role"),
                &[Text("user")],
                &format!("{member_label} role"),
            )?;
            let content = array_value(
                message.get("content"),
                &format!("{member_label} content"),
                |block, block_label| {
                    let text = exact_record(Some(block), block_label, &["type", "text"], &[])?;
                    literal_value(
                        text.get("type"),
                        &[Text("text")],
                        &format!("{block_label} type"),
                    )?;
                    string_value(text.get("text"), &format!("{block_label} text")).map(drop)
                },
            )?;
            if content.len() != 1 {
                return invalid(format!(
                    "{member_label} content must contain one text block"
                ));
            }
            Ok(())
        },
    )?;
    if messages.len() != 1 {
        return invalid(format!("{label} messages must contain one user message"));
    }
    let tools = array_value(
        body.get("tools"),
        &format!("{label} tools"),
        |member, member_label| {
            let tool = exact_record(
                Some(member),
                member_label,
                &["type", "name", "max_uses"],
                &[],
            )?;
            let at = |key: &str| format!("{member_label} {key}");
            literal_value(
                tool.get("type"),
                &[Text("web_search_20250305")],
                &at("type"),
            )?;
            literal_value(tool.get("name"), &[Text("web_search")], &at("name"))?;
            positive_integer(tool.get("max_uses"), &at("max_uses")).map(drop)
        },
    )?;
    if tools.len() != 1 {
        return invalid(format!("{label} tools must contain one web search tool"));
    }
    Ok(())
}
