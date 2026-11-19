//! The model's subagent tools: `subagent`, `list_subagent_models`,
//! `send_message`, `interrupt_agent`, and `list_agents` for a parent, and
//! `send_message` alone for a child.
//!
//! Children are continuable: a background child returns its id at once.
//! The model-choice texts, which every request carries once more than one
//! model is available, are cut to what the model needs, and say that
//! omitted fields inherit the caller's route, since Bake has no task router
//! yet; `list_agents` has no `scope`, since with no nested children its
//! `descendants` are its `children`, and adds each child's route to its
//! row. The model-choice fields and
//! `list_subagent_models` are shown when more than one model is available.

use bake_agent::{AgentTool, AgentToolResult, ToolReplay};
use bake_ai::AbortSignal;
use serde_json::{Value, json};

use super::messages;
use super::models::{self, RouteRequest};
use super::{Delivery, Interrupt, ParentLink, Status, SubagentHost};
use crate::tools::definition::ToolDefinition;

/// A parent's tools, in the order they are declared.
pub const PARENT_TOOL_NAMES: [&str; 5] = [
    "subagent",
    "list_subagent_models",
    "send_message",
    "interrupt_agent",
    "list_agents",
];

/// A child's tools.
pub const CHILD_TOOL_NAMES: [&str; 1] = ["send_message"];

const DESCRIPTION: &str = "Delegate a self-contained task to a subagent with its own context; it does not see this conversation and returns only its result. It runs in the background by default, returning its agent id: start independent ones together and keep working; you are notified when one finishes. `send_message` continues it later or steers it while running.";

const CHOICE: &str = " Omit `provider`, `model`, and `reasoning_effort` to use your own route; `list_subagent_models` lists the others.";

const SEND_MESSAGE: &str = "Message a direct continuable subagent, or your parent if you are one. A busy target reads it next step; an idle one starts a new turn. Confirms delivery; no reply.";

fn definition(tool: AgentTool) -> ToolDefinition {
    ToolDefinition {
        tool,
        prompt_snippet: None,
        prompt_guidelines: None,
    }
}

fn text(text: impl Into<String>, details: Option<Value>) -> AgentToolResult {
    let mut result = AgentToolResult::text(text);
    result.details = details;
    result
}

/// Runs `work` until it ends or `signal` aborts first, which drops it:
/// `None` then.
async fn unless_aborted<T>(
    signal: Option<&AbortSignal>,
    work: impl std::future::Future<Output = T>,
) -> Option<T> {
    let Some(signal) = signal else {
        return Some(work.await);
    };
    tokio::select! {
        biased;
        value = work => Some(value),
        () = signal.cancelled() => None,
    }
}

fn string_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            format!(
                "invalid {key}: expected a non-empty string, got {}",
                args.get(key).cloned().unwrap_or(Value::Null)
            )
        })
}

/// The parent's tools over `host`. With `model_choice`, `subagent` takes
/// a route and `list_subagent_models` is included.
pub fn parent_tools(host: &SubagentHost, model_choice: bool) -> Vec<ToolDefinition> {
    let mut tools = vec![subagent_tool(host.clone(), model_choice)];
    if model_choice {
        tools.push(list_models_tool(host.clone()));
    }
    tools.push(parent_send_message_tool(host.clone()));
    tools.push(interrupt_tool(host.clone()));
    tools.push(list_agents_tool(host.clone()));
    tools
}

/// A child's tools: `send_message` to its parent over `link`.
pub fn child_tools(link: &ParentLink) -> Vec<ToolDefinition> {
    let link = link.clone();
    let mut tool = AgentTool::new(
        "send_message",
        "send_message",
        SEND_MESSAGE,
        send_message_parameters(),
        move |_id, args, _signal, _update, _ctx| {
            let result = (|| {
                let agent = string_arg(&args, "agent_id")?;
                let message = args.get("message").and_then(Value::as_str).unwrap_or("");
                let message_id = link.send(agent, message)?;
                Ok(text(
                    format!("message delivered to agent {agent}"),
                    Some(json!({ "agentId": agent, "messageId": message_id })),
                ))
            })();
            Box::pin(async move { result })
        },
    );
    tool.replay = Some(ToolReplay::Never);
    // Coordination with no workspace effect of its own: runs beside other calls.
    tool.execution_mode = Some(bake_agent::ToolExecutionMode::Parallel);
    vec![definition(tool)]
}

fn subagent_tool(host: SubagentHost, model_choice: bool) -> ToolDefinition {
    let mut properties = json!({
        "description": { "type": "string", "description": "3-5 words, for display." },
        "prompt": { "type": "string", "description": "Standalone, with everything it needs." },
    });
    if model_choice {
        properties["provider"] = json!({
            "type": "string",
            "description": "Pass with model."
        });
        properties["model"] = json!({
            "type": "string",
            "description": "Pass with provider."
        });
        properties["reasoning_effort"] = json!({
            "type": "string",
            "description": "Omit for your own effort, or a new route's default."
        });
    }
    properties["run_in_background"] = json!({
        "type": "boolean",
        "description": "false only when your next step needs the result."
    });
    let parameters = json!({
        "type": "object",
        "required": ["description", "prompt"],
        "properties": properties,
    });
    let description = if model_choice {
        format!("{DESCRIPTION}{CHOICE}")
    } else {
        DESCRIPTION.to_owned()
    };
    let mut tool = AgentTool::new(
        "subagent",
        "subagent",
        description,
        parameters,
        move |_id, args, signal, _update, _ctx| {
            let host = host.clone();
            Box::pin(async move { delegate(&host, model_choice, &args, signal).await })
        },
    );
    tool.replay = Some(ToolReplay::Never);
    // Coordination with no workspace effect of its own: runs beside other calls.
    tool.execution_mode = Some(bake_agent::ToolExecutionMode::Parallel);
    definition(tool)
}

/// Interrupts a foreground child whose call is dropped before it settles.
struct Foreground<'a> {
    host: &'a SubagentHost,
    id: &'a str,
    settled: bool,
}

impl Drop for Foreground<'_> {
    fn drop(&mut self) {
        if !self.settled {
            let _ = self.host.interrupt(self.id);
        }
    }
}

async fn delegate(
    host: &SubagentHost,
    model_choice: bool,
    args: &Value,
    signal: Option<AbortSignal>,
) -> Result<AgentToolResult, String> {
    let label = string_arg(args, "description")?;
    let prompt = string_arg(args, "prompt")?;
    let request = RouteRequest::from_args(args);
    if !model_choice && !request.is_empty() {
        return Err("child model selection is disabled for this tool instance".to_owned());
    }
    let foreground = args.get("run_in_background").and_then(Value::as_bool) == Some(false);
    let route = host.resolve(&request)?;
    if signal.as_ref().is_some_and(AbortSignal::aborted) {
        return Err("subagent run was cancelled".to_owned());
    }
    // A start dropped by the abort leaves no child behind.
    let started = unless_aborted(
        signal.as_ref(),
        host.start(label, prompt, route, foreground),
    )
    .await
    .ok_or_else(|| "subagent run was cancelled".to_owned())??;
    let details = json!({ super::SUBAGENT_DETAIL: {
        "agentId": started.id,
        "label": label,
        "file": started.file.as_ref().map(|file| file.to_string_lossy()),
        "provider": started.provider,
        "model": started.model,
        "thinkingLevel": started.thinking_level,
        "runId": started.run,
        "foreground": foreground,
    } });
    let Some(settled) = started.settled else {
        let shown = if started.queued {
            format!(
                "queued subagent {}: {} subagents are busy. It starts when one finishes; you are notified when it is done.",
                started.id,
                super::MAX_ACTIVE
            )
        } else {
            format!("started subagent {}", started.id)
        };
        return Ok(text(shown, Some(details)));
    };
    // Dropped unsettled, as when the abort ends the wait, it interrupts the
    // child, whose run then settles with no one to tell.
    let mut guard = Foreground {
        host,
        id: &started.id,
        settled: false,
    };
    let Some(settled) = unless_aborted(signal.as_ref(), settled).await else {
        let mut result = text(
            format!(
                "subagent run was cancelled; agent {} was stopped and keeps its context. send_message continues it.",
                started.id
            ),
            Some(details),
        );
        result.is_error = true;
        return Ok(result);
    };
    guard.settled = true;
    let Ok((settlement, leftovers)) = settled else {
        let mut result = text("subagent run was cancelled", Some(details));
        result.is_error = true;
        return Ok(result);
    };
    let left = leftovers.lines();
    Ok(match messages::foreground_error(settlement.stop) {
        None => text(
            format!("{}{left}", settlement.closing.unwrap_or_default())
                .trim_end()
                .to_owned(),
            Some(details),
        ),
        Some(headline) => {
            let mut shown = messages::foreground_failure(
                headline,
                settlement.diagnostic.as_deref(),
                settlement.closing.as_deref(),
            );
            if !left.is_empty() {
                shown.push('\n');
                shown.push_str(left.trim_end());
            }
            let mut result = text(shown, Some(details));
            result.is_error = true;
            result
        }
    })
}

fn list_models_tool(host: SubagentHost) -> ToolDefinition {
    let parameters = json!({
        "type": "object",
        "properties": {
            "provider": { "type": "string", "description": "Omit to list providers." },
            "model": { "type": "string", "description": "Requires provider." }
        }
    });
    let mut tool = AgentTool::new(
        "list_subagent_models",
        "list_subagent_models",
        "List the routes a subagent can run on: providers; with `provider`, its models; with `provider` and `model`, that model's efforts.",
        parameters,
        move |_id, args, _signal, _update, _ctx| {
            let result = models::list(&args, &host.available_models(), host.settings())
                .map(|listing| text(listing, None));
            Box::pin(async move { result })
        },
    );
    tool.replay = Some(ToolReplay::Safe);
    tool.effect = Some(bake_agent::ToolEffect::Read);
    definition(tool)
}

fn send_message_parameters() -> Value {
    json!({
        "type": "object",
        "required": ["agent_id", "message"],
        "properties": {
            "agent_id": { "type": "string" },
            "message": { "type": "string" }
        }
    })
}

fn parent_send_message_tool(host: SubagentHost) -> ToolDefinition {
    let mut tool = AgentTool::new(
        "send_message",
        "send_message",
        SEND_MESSAGE,
        send_message_parameters(),
        move |_id, args, signal, _update, _ctx| {
            let host = host.clone();
            Box::pin(async move {
                let agent = string_arg(&args, "agent_id")?.to_owned();
                let message = args.get("message").and_then(Value::as_str).unwrap_or("");
                // Only resuming a `ready` child waits; dropped by the abort,
                // the child stays `ready` with the message.
                let sent = unless_aborted(signal.as_ref(), host.message_child(&agent, message))
                    .await
                    .ok_or_else(|| {
                        format!(
                            "send_message was cancelled while agent {agent} resumed; the message waits for its next run"
                        )
                    })??;
                let mut details = json!({ "agentId": agent, "messageId": sent.message_id });
                if let Some(run) = sent.run() {
                    details["runId"] = json!(run);
                }
                let shown = match sent.delivery {
                    Delivery::Running => {
                        format!("message delivered to agent {agent}; it reads it at its next step")
                    }
                    Delivery::Started(_) => {
                        format!("message delivered to agent {agent}; it starts a new run")
                    }
                    Delivery::Queued(_) => format!(
                        "message queued for agent {agent}; it reads it when its next run starts"
                    ),
                };
                Ok(text(shown, Some(details)))
            })
        },
    );
    tool.replay = Some(ToolReplay::Never);
    // Coordination with no workspace effect of its own: runs beside other calls.
    tool.execution_mode = Some(bake_agent::ToolExecutionMode::Parallel);
    definition(tool)
}

fn interrupt_tool(host: SubagentHost) -> ToolDefinition {
    let parameters = json!({
        "type": "object",
        "required": ["agent_id"],
        "properties": { "agent_id": { "type": "string" } }
    });
    let mut tool = AgentTool::new(
        "interrupt_agent",
        "interrupt_agent",
        "Request a stop of a continuable subagent's current turn; it stays available.",
        parameters,
        move |_id, args, _signal, _update, _ctx| {
            let result = string_arg(&args, "agent_id").and_then(|agent| {
                Ok(match host.interrupt(agent)? {
                    Interrupt::Requested => {
                        text(format!("interrupt requested for agent {agent}"), None)
                    }
                    Interrupt::NotRunning(status) => {
                        text(format!("agent {agent} is {status}; nothing to stop"), None)
                    }
                })
            });
            Box::pin(async move { result })
        },
    );
    tool.replay = Some(ToolReplay::Never);
    // Coordination with no workspace effect of its own: runs beside other calls.
    tool.execution_mode = Some(bake_agent::ToolExecutionMode::Parallel);
    definition(tool)
}

fn list_agents_tool(host: SubagentHost) -> ToolDefinition {
    let mut tool = AgentTool::new(
        "list_agents",
        "list_agents",
        "List your continuable subagents: agent id, status (running, idle, or ready = saved, resumable, not a result), label, and model. For ids only; you are notified on finish.",
        json!({ "type": "object", "properties": {} }),
        move |_id, _args, _signal, _update, _ctx| {
            let rows = host.listing();
            let listing = if rows.is_empty() {
                "(no subagents)".to_owned()
            } else {
                rows.iter()
                    .map(|(row, queued)| match row.status {
                        Status::Unavailable => format!("{} [diagnostic: unavailable]", row.id),
                        status => format!(
                            "{} [{}] — {} · {}",
                            row.id,
                            if *queued { "queued" } else { status.word() },
                            row.label,
                            row.route
                        ),
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            Box::pin(async move { Ok(text(listing, None)) })
        },
    );
    tool.replay = Some(ToolReplay::Safe);
    tool.effect = Some(bake_agent::ToolEffect::Read);
    definition(tool)
}
