//! An experimental, deterministic reduction of old tool observations. Every
//! replacement is a Pi context edit before it reaches a model request.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use bake_agent::AgentContext;
use bake_ai::AbortSignal;
use serde_json::{Value, json};

use super::recovery::Core;
use super::{SessionEvent, read_settings, write_session};
use crate::session::{EditContent, GroupEntry, ProjectedSessionEntry, SessionProjection};

const RETAINED_BATCHES: usize = 8;
const REPLACEMENT: &str = "[Older tool output masked.]";
const PROVENANCE: &str = "bake.contextObservation";

fn eligible_name(name: Option<&str>) -> bool {
    matches!(name, Some("read" | "ls" | "find" | "grep"))
}

/// Record the executing host capability, not a tool name guessed on replay.
/// This detail never enters model context. Non-object tool details retain
/// their shape and are deliberately ineligible for this experiment. Strip
/// tool-supplied provenance even while disabled: later sessions may enable it.
pub(super) fn observation_details(
    context: &bake_agent::AfterToolCallContext<'_>,
    enabled: bool,
) -> Option<Value> {
    let name = &context.tool_call.name;
    let eligible = enabled
        && !context.is_error
        && eligible_name(Some(name))
        && context
            .context
            .tools
            .iter()
            .any(|tool| tool.name == *name && tool.read_only);
    let mut details = match context.result.details.as_ref() {
        Some(Value::Object(details)) => details.clone(),
        None if eligible => serde_json::Map::new(),
        _ => return None,
    };
    if eligible {
        details.insert(PROVENANCE.into(), json!({"version":1,"kind":name}));
    } else if details.remove(PROVENANCE).is_none() {
        return None;
    }
    Some(Value::Object(details))
}

fn attested(observation: &Observation<'_>) -> bool {
    let Some(marker) = observation
        .message
        .get("details")
        .and_then(|details| details.get(PROVENANCE))
    else {
        return false;
    };
    marker.get("version").and_then(Value::as_u64) == Some(1)
        && marker.get("kind") == observation.call.get("name")
        && eligible_name(observation.call.get("name").and_then(Value::as_str))
}

struct Observation<'a> {
    entry: &'a ProjectedSessionEntry,
    call: &'a Value,
    message: &'a serde_json::Map<String, Value>,
}

/// Only complete, contiguous call/result batches are eligible. Ambiguous
/// imported history and incomplete batches retain their full observations.
fn batches(projection: &SessionProjection) -> Vec<Vec<Observation<'_>>> {
    let entries = &projection.entries;
    let mut batches = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let [assistant] = entry.messages.as_slice() else {
            continue;
        };
        let message = assistant.as_json();
        if assistant.role() != Some("assistant")
            || matches!(
                message.get("stopReason").and_then(Value::as_str),
                Some("error" | "aborted")
            )
        {
            continue;
        }
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        let mut calls = HashMap::new();
        let mut valid = true;
        for call in content
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("toolCall"))
        {
            let Some(id) = call
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
            else {
                valid = false;
                break;
            };
            if calls.insert(id, call).is_some() {
                valid = false;
                break;
            }
        }
        if !valid || calls.is_empty() {
            continue;
        }
        let mut seen = HashSet::new();
        let mut observations = Vec::new();
        for result in &entries[index + 1..] {
            // State entries carry no model content and do not split a batch.
            if result.messages.is_empty() {
                continue;
            }
            let [message] = result.messages.as_slice() else {
                break;
            };
            if message.role() != Some("toolResult") {
                break;
            }
            let raw = message.as_json();
            let Some(id) = raw.get("toolCallId").and_then(Value::as_str) else {
                valid = false;
                break;
            };
            let Some(call) = calls.get(id) else {
                valid = false;
                break;
            };
            if !seen.insert(id) || raw.get("toolName") != call.get("name") {
                valid = false;
                break;
            }
            observations.push(Observation {
                entry: result,
                call,
                message: raw,
            });
        }
        if valid && seen.len() == calls.len() {
            batches.push(observations);
        }
    }
    batches
}

fn read_key(observation: &Observation<'_>) -> Option<String> {
    if !attested(observation)
        || observation.call.get("name").and_then(Value::as_str) != Some("read")
        || observation.message.get("isError").and_then(Value::as_bool) != Some(false)
        || observation
            .entry
            .source_entry
            .get("message")
            .and_then(|message| message.get("content"))
            != observation.message.get("content")
    {
        return None;
    }
    let args = observation.call.get("arguments")?.as_object()?;
    let path = args.get("path")?.as_str()?;
    // Exact recorded names/ranges deliberately preserve aliases separately:
    // consulting today's filesystem would make replay nondeterministic.
    Some(json!([path, args.get("offset"), args.get("limit")]).to_string())
}

struct Plan {
    edits: Vec<GroupEntry>,
    removed_bytes: u64,
}

fn plan(projection: &SessionProjection) -> Plan {
    let batches = batches(projection);
    let mut latest_reads = HashMap::new();
    for observation in batches.iter().flatten() {
        if let Some(key) = read_key(observation) {
            latest_reads.insert(key, observation.entry.source_entry.id());
        }
    }
    let replacement = json!([{ "type": "text", "text": REPLACEMENT }]);
    let replacement_bytes = replacement.to_string().len();
    let mut result = Plan {
        edits: Vec::new(),
        removed_bytes: 0,
    };
    for observation in batches
        .iter()
        .take(batches.len().saturating_sub(RETAINED_BATCHES))
        .flatten()
    {
        let message = observation.message;
        if message.get("isError").and_then(Value::as_bool) != Some(false)
            || !attested(observation)
            || observation.entry.source_entry.entry_type() != "message"
        {
            continue;
        }
        if observation.call.get("name").and_then(Value::as_str) == Some("read")
            && read_key(observation).is_none_or(|key| {
                latest_reads.get(&key) == Some(&observation.entry.source_entry.id())
            })
        {
            continue;
        }
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        if content.is_empty()
            || !content.iter().all(|block| {
                block.get("type").and_then(Value::as_str) == Some("text")
                    && block.get("text").is_some_and(Value::is_string)
            })
        {
            continue;
        }
        // Keep explicit earlier edits and make repeated boundaries a no-op.
        if observation
            .entry
            .source_entry
            .get("message")
            .and_then(|message| message.get("content"))
            != message.get("content")
        {
            continue;
        }
        let old_bytes = Value::Array(content.clone()).to_string().len();
        if old_bytes <= replacement_bytes {
            continue;
        }
        result.removed_bytes = result
            .removed_bytes
            .saturating_add((old_bytes - replacement_bytes) as u64);
        result.edits.push(GroupEntry::context_edit(
            observation.entry.source_entry.id(),
            Some(EditContent::Text(REPLACEMENT.into())),
        ));
    }
    result
}

fn stop(core: &Core, message: String) -> String {
    super::lock_gap(&core.gap).get_or_insert_with(|| message.clone());
    if let Some(control) = core.control() {
        control.fail(message.clone());
    }
    core.abort_requested.store(true, Ordering::SeqCst);
    if let Some(agent) = core.agent() {
        agent.abort();
    }
    message
}

impl Core {
    /// Reduces context before deciding whether a foreground summary is
    /// needed, and again at the request boundary after context exposure.
    pub(super) async fn mask_context(
        self: &Arc<Self>,
        signal: Option<AbortSignal>,
    ) -> Result<Option<SessionProjection>, String> {
        if !read_settings(&self.settings).context_masking || self.aborted() {
            return Ok(None);
        }
        let work = self
            .owned_context_commit(move |core| {
                #[cfg(test)]
                {
                    let hook = core.hooks().before_context_masking.take();
                    if let Some(hook) = hook {
                        hook();
                    }
                }
                let result = write_session::<_, crate::session::SessionError>(
                    &core.session,
                    &core.gap,
                    |session| {
                        if core.aborted() || signal.as_ref().is_some_and(AbortSignal::aborted) {
                            return Ok(None);
                        }
                        let projection = session.build_session_projection();
                        let plan = plan(&projection);
                        if plan.edits.is_empty() {
                            return Ok(None);
                        }
                        let source_leaf = session.leaf_id().unwrap_or_default().to_owned();
                        let masked = plan.edits.len() as u64;
                        session.append_group(plan.edits)?;
                        #[cfg(test)]
                        {
                            let hook = core.hooks().after_context_masking.take();
                            if let Some(hook) = hook {
                                hook();
                            }
                        }
                        Ok(Some((
                            session.build_session_projection(),
                            source_leaf,
                            masked,
                            plan.removed_bytes,
                        )))
                    },
                );
                match result {
                    Ok(Some((projection, source_leaf, masked, removed_bytes))) => {
                        // The owned lease spans both persistence and publication,
                        // even when the provider request's waiter was cancelled.
                        core.refresh_context(&projection);
                        core.emit(&SessionEvent::ContextMasking {
                            source_leaf_id: &source_leaf,
                            masked_results: masked,
                            removed_bytes,
                            retained_recent_batches: RETAINED_BATCHES as u64,
                        });
                        Ok(Some(projection))
                    }
                    Ok(None) => Ok(None),
                    Err(error) => Err(stop(core, error.message())),
                }
            })
            .map_err(|error| stop(self, error.to_string()))?;
        work.await
            .map_err(|error| stop(self, format!("Context masking I/O failed: {error}")))?
    }
}

/// Both policies run on the agent's owned request boundary. Document
/// exposure precedes masking; custom document messages are never masked.
pub(super) fn request_hook(core: Arc<Core>, task_documents: bool) -> bake_agent::PrepareRequest {
    bake_agent::hook::prepare_request(move |request, signal| {
        let core = Arc::clone(&core);
        let tools = request.context.tools.clone();
        Box::pin(async move {
            let exposed = if task_documents {
                super::task_documents::expose(Arc::clone(&core), tools.clone(), signal.clone())
                    .await
            } else {
                None
            };
            let masked = core.mask_context(signal).await.ok().flatten();
            let context = masked
                .map(|projection| AgentContext {
                    messages: projection
                        .messages
                        .iter()
                        .map(crate::session::AgentMessage::to_agent)
                        .collect(),
                    tools,
                })
                .or(exposed);
            context.map(|context| bake_agent::AgentRequestUpdate {
                context: Some(context),
                ..Default::default()
            })
        })
    })
}

#[cfg(test)]
mod tests;
