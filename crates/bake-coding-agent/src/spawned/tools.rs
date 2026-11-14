//! The model's tools over a [`SpawnHost`]: `spawn_agent` and
//! `stop_agent`. Messages to a spawned agent go through `telecom`.

use bake_agent::{AgentTool, AgentToolResult, ToolReplay};
use serde_json::{Value, json};

use super::{SpawnHost, SpawnedState};
use crate::tools::definition::ToolDefinition;

/// The tools' names, in the order they are declared.
pub const TOOL_NAMES: [&str; 2] = ["spawn_agent", "stop_agent"];

const SPAWN: &str = "Start another Bake agent as a separate headless process in this repository, with its own context, on a self-contained task. It does not see this conversation: put everything it needs in `task`. It runs in the background under your permission and joins telecom under the callsign this returns; you are notified each time it finishes a run, with its answer. Message or ask it with telecom; stop it with stop_agent when its work is done. Start independent agents together.";

const STOP: &str = "Stop a spawned agent: end its process (default), or with interrupt true only stop its current run and keep it for more messages.";

const SNIPPET: &str = "Start parallel headless Bake agents on self-contained tasks, and stop them";

fn ok(text: String, details: Value) -> AgentToolResult {
    let mut result = AgentToolResult::text(text);
    result.details = Some(details);
    result
}

/// The spawn tools over `host`.
pub fn tools(host: &SpawnHost) -> Vec<ToolDefinition> {
    let spawn = {
        let host = host.clone();
        let mut tool = AgentTool::new(
            "spawn_agent",
            "Spawn agent",
            SPAWN,
            json!({
                "type": "object",
                "properties": {
                    "task": { "type": "string", "description": "The whole task: goal, files, constraints, and what to report back." },
                    "model": { "type": "string", "description": "provider/model to run it on; omit for yours." }
                },
                "required": ["task"],
                "additionalProperties": false
            }),
            move |_id, args, _signal, _update, _context| {
                let result = (|| {
                    let task = args
                        .get("task")
                        .and_then(Value::as_str)
                        .ok_or("spawn_agent: `task` is required")?;
                    let model = args
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|model| !model.is_empty())
                        .map(str::to_owned);
                    let agent = host
                        .spawn(task, model, super::Origin::Model)
                        .map_err(|error| format!("spawn_agent: {error}"))?;
                    Ok(ok(
                        format!(
                            "started {name}; it works in the background and you are notified when it finishes a run. Reach it with telecom (to: \"{name}\").",
                            name = agent.name
                        ),
                        json!({ "agent": agent.name, "id": agent.id, "model": agent.model }),
                    ))
                })();
                Box::pin(async move { result })
            },
        );
        // A replay would start a second process.
        tool.replay = Some(ToolReplay::Never);
        tool
    };
    let stop = {
        let host = host.clone();
        let mut tool = AgentTool::new(
            "stop_agent",
            "Stop agent",
            STOP,
            json!({
                "type": "object",
                "properties": {
                    "agent": { "type": "string", "description": "Its callsign, as spawn_agent returned it." },
                    "interrupt": { "type": "boolean", "description": "true: stop only its current run." }
                },
                "required": ["agent"],
                "additionalProperties": false
            }),
            move |_id, args, _signal, _update, _context| {
                let result = (|| {
                    let who = args
                        .get("agent")
                        .and_then(Value::as_str)
                        .ok_or("stop_agent: `agent` is required")?;
                    let agent = host
                        .find(who)
                        .map_err(|error| format!("stop_agent: {error}"))?;
                    if let SpawnedState::Exited(_) = agent.state {
                        return Ok(ok(
                            format!("{} has already exited", agent.name),
                            json!({ "agent": agent.name, "status": "exited" }),
                        ));
                    }
                    let interrupt = args.get("interrupt").and_then(Value::as_bool) == Some(true);
                    if interrupt {
                        host.interrupt(agent.id)
                            .map_err(|error| format!("stop_agent: {error}"))?;
                        Ok(ok(
                            format!(
                                "stopped {}'s run; it stays idle for more messages",
                                agent.name
                            ),
                            json!({ "agent": agent.name, "status": "interrupted" }),
                        ))
                    } else {
                        host.stop(agent.id)
                            .map_err(|error| format!("stop_agent: {error}"))?;
                        Ok(ok(
                            format!("{} is shutting down", agent.name),
                            json!({ "agent": agent.name, "status": "stopping" }),
                        ))
                    }
                })();
                Box::pin(async move { result })
            },
        );
        tool.replay = Some(ToolReplay::Never);
        tool
    };
    vec![
        ToolDefinition {
            tool: spawn,
            prompt_snippet: Some(SNIPPET.to_owned()),
            prompt_guidelines: None,
        },
        ToolDefinition {
            tool: stop,
            prompt_snippet: None,
            prompt_guidelines: None,
        },
    ]
}
