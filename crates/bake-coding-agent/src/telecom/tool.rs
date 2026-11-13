//! The `telecom` tool: `board`, `send`, `ask`, `reply`, `broadcast`, and
//! `status`.
//!
//! Its name, description, and parameters are fixed, so they stay in the
//! cached prompt prefix: nothing in them names a callsign or a count.
//! Adapted from pi-telecom's `tool.ts` (`Justar96/pi-telecom` at
//! `b6fe4d8`, MIT), the actions this port keeps; claims, locks, waits, and
//! the log are not part of it.

use std::time::Duration;

use bake_agent::{AgentTool, AgentToolResult, ToolReplay};
use serde_json::{Value, json};

use super::station::{ASK_TIMEOUT, AskOutcome, Station};
use crate::tools::definition::ToolDefinition;

/// The tool's name.
pub const TOOL_NAME: &str = "telecom";

const DESCRIPTION: &str = "Coordinate with the other Bake agents working in this repository, each known by a callsign (alpha, bravo, ...; a spawned agent is <parent>-1, ...).
board: who works here, on what, and the asks waiting on you.
send: message agents (to = callsign, or several comma-separated); an idle one starts a turn. ask: message one agent and wait for its answer (wait false: return at once; the answer arrives later as a message). reply: answer an ask put to you (askId; optional when only one waits).
broadcast: tell every agent here; it starts no turn and reaches them at their next prompt. status: set the one-line task the others see.";

const SNIPPET: &str =
    "Coordinate with parallel Bake agents: team board, send/ask/reply, broadcast, status";

const GUIDELINES: [&str; 3] = [
    "When telecom shows other agents in this repository, reply promptly to an ask put to you: the asker is blocked until you answer.",
    "Message other agents only for real coordination (requests, handoffs, shared-interface changes), never to acknowledge; each message costs the recipient a turn.",
    "To hear when another agent's long task ends, ask with wait false, rather than waiting with a long timeout or polling the board.",
];

fn parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["board", "send", "ask", "reply", "broadcast", "status"],
                "description": "What to do."
            },
            "to": {
                "type": "string",
                "description": "send/ask/reply: a callsign or session id as the board shows it. send: several, comma-separated."
            },
            "message": {
                "type": "string",
                "description": "The text; for status, your task in one line."
            },
            "askId": {
                "type": "string",
                "description": "reply: the ask to answer (optional when only one waits)."
            },
            "timeoutSec": {
                "type": "number",
                "description": "ask: seconds to wait for the answer (default 300, at most 3600)."
            },
            "wait": {
                "type": "boolean",
                "description": "ask: false returns at once; the answer arrives later as a message."
            }
        },
        "required": ["action"],
        "additionalProperties": false
    })
}

fn ok(text: String, details: Value) -> AgentToolResult {
    let mut result = AgentToolResult::text(text);
    result.details = Some(details);
    result
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn required<'a>(args: &'a Value, key: &str, action: &str) -> Result<&'a str, String> {
    str_arg(args, key).ok_or_else(|| format!("telecom: {action} needs `{key}`"))
}

/// The `telecom` tool over `station`.
pub fn tool(station: Station) -> ToolDefinition {
    let mut tool = AgentTool::new(
        TOOL_NAME,
        "Telecom",
        DESCRIPTION,
        parameters(),
        move |_id, args, signal, _update, _context| {
            let station = station.clone();
            Box::pin(async move {
                run(&station, &args, signal)
                    .await
                    .map_err(|error| format!("telecom: {error}"))
            })
        },
    );
    // A message sent cannot be unsent: a replay must not repeat it.
    tool.replay = Some(ToolReplay::Never);
    ToolDefinition {
        tool,
        prompt_snippet: Some(SNIPPET.to_owned()),
        prompt_guidelines: Some(GUIDELINES.iter().map(|line| (*line).to_owned()).collect()),
    }
}

async fn run(
    station: &Station,
    args: &Value,
    signal: Option<bake_ai::AbortSignal>,
) -> Result<AgentToolResult, String> {
    let action = str_arg(args, "action").ok_or("`action` is required")?;
    let callsign = station.callsign();
    match action {
        "board" => Ok(ok(
            station.board(),
            json!({ "action": "board", "callsign": callsign }),
        )),
        "send" => {
            let to = required(args, "to", action)?;
            let message = required(args, "message", action)?;
            let report = station.send(to, message, false).await?;
            Ok(ok(report, json!({ "action": "send", "to": to })))
        }
        "broadcast" => {
            let message = required(args, "message", action)?;
            let report = station.broadcast(message).await?;
            Ok(ok(report, json!({ "action": "broadcast" })))
        }
        "status" => {
            let message = required(args, "message", action)?;
            let task = station.set_status(message);
            Ok(ok(
                format!("{callsign}'s task is now: {task}"),
                json!({ "action": "status", "task": task }),
            ))
        }
        "reply" => {
            let message = required(args, "message", action)?;
            let report = station
                .reply(message, str_arg(args, "askId"), str_arg(args, "to"))
                .await?;
            Ok(ok(report, json!({ "action": "reply" })))
        }
        "ask" => {
            let to = required(args, "to", action)?;
            let message = required(args, "message", action)?;
            let wait = args.get("wait").and_then(Value::as_bool).unwrap_or(true);
            let timeout = args
                .get("timeoutSec")
                .and_then(Value::as_f64)
                .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
                .map_or(ASK_TIMEOUT, |seconds| {
                    Duration::from_secs_f64(seconds.min(3600.0))
                });
            let outcome = station.ask(to, message, timeout, wait, signal).await?;
            Ok(ask_result(outcome))
        }
        other => Err(format!(
            "unknown action \"{other}\"; use board, send, ask, reply, broadcast, or status"
        )),
    }
}

fn ask_result(outcome: AskOutcome) -> AgentToolResult {
    match outcome {
        AskOutcome::Answered { ask_id, from, text } => ok(
            format!("{from} answered:\n{text}"),
            json!({ "action": "ask", "askId": ask_id, "status": "answered", "from": from }),
        ),
        AskOutcome::Sent { ask_id, to } => ok(
            format!("asked {to} (ask {ask_id}); the answer will arrive as a message"),
            json!({ "action": "ask", "askId": ask_id, "status": "sent", "to": to }),
        ),
        AskOutcome::TimedOut { ask_id, to } => ok(
            format!(
                "{to} has not answered yet (ask {ask_id}); a later answer will arrive as a message"
            ),
            json!({ "action": "ask", "askId": ask_id, "status": "timeout", "to": to }),
        ),
        AskOutcome::Gone { ask_id, to } => {
            let mut result = ok(
                format!("{to} left before answering (ask {ask_id}); decide without it"),
                json!({ "action": "ask", "askId": ask_id, "status": "gone", "to": to }),
            );
            result.is_error = true;
            result
        }
        AskOutcome::Cancelled { ask_id } => ok(
            format!("the ask was withdrawn (ask {ask_id})"),
            json!({ "action": "ask", "askId": ask_id, "status": "cancelled" }),
        ),
    }
}
