//! The opt-in task document tool and its between-turn context exposure.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use bake_agent::{AgentContext, AgentTool, AgentToolResult, ToolExecutionMode};
use bake_ai::{AbortSignal, TextContent, UserContentBlock};
use serde::Deserialize;
use serde_json::{Value, json};

use super::recovery::Core;
use super::write_session;
use crate::task_documents::{self, DocumentError, DocumentRef, DocumentUpdate};

pub(crate) const NAME: &str = "task_document";

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    List,
    Get {
        id: String,
    },
    Revise {
        expected: Option<DocumentRef>,
        document: DocumentUpdate,
    },
}

pub(super) fn tool(core: Arc<Core>) -> Arc<AgentTool> {
    let mut tool = AgentTool::new(
        NAME,
        "Task document",
        "Read or revise a versioned plan, todo, or goal. List returns revision references; get returns the complete document. Revise requires the exact current reference, or null to create. Create a goal only when the user requested one. claimed_complete records a claim and does not verify artifacts.",
        json!({
            "type":"object", "additionalProperties":false,
            "properties": {
                "operation":{"type":"string","enum":["list","get","revise"]},
                "id":{"type":"string"},
                "expected":{"anyOf":[{"type":"null"},{"type":"object","additionalProperties":false,"required":["id","revision","entryId"],"properties":{"id":{"type":"string"},"revision":{"type":"integer","minimum":1},"entryId":{"type":"string"}}}]},
                "document":{"type":"object","additionalProperties":false,"required":["id","kind","body","state"],"properties":{
                    "id":{"type":"string","pattern":"^[A-Za-z0-9_-]{1,64}$"},
                    "kind":{"type":"string","enum":["plan","todo","goal"]},
                    "body":{"type":"string","maxLength":65536},
                    "state":{"type":"string","enum":["active","paused","blocked","claimed_complete","archived"]}
                }}
            }, "required":["operation"]
        }),
        move |_id, arguments, signal, _update| {
            let core = Arc::clone(&core);
            Box::pin(async move { execute(core, arguments, signal).await })
        },
    );
    tool.execution_mode = Some(ToolExecutionMode::Sequential);
    // `revise` appends a revision: a rerun of one that landed would report
    // its own revision as a conflict.
    tool.replay = Some(bake_agent::ToolReplay::Never);
    Arc::new(tool)
}

fn apply(
    session: &mut crate::session::SessionManager,
    request: Request,
) -> Result<Value, DocumentError> {
    match request {
        Request::List => Ok(Value::Array(task_documents::project(session)?.values().map(|document| json!({
            "reference": document.reference(), "kind":document.snapshot.kind, "state":document.snapshot.state
        })).collect())),
        Request::Get { id } => {
            let documents = task_documents::project(session)?;
            let document = documents.get(&id).ok_or_else(|| DocumentError::Invalid("document does not exist on this branch".into()))?;
            Ok(json!({"reference":document.reference(),"document":document.snapshot}))
        }
        Request::Revise { expected, document } => {
            let reference = task_documents::revise_snapshot(session, expected.as_ref(), document)?;
            Ok(json!({"reference":reference}))
        }
    }
}

async fn execute(
    core: Arc<Core>,
    arguments: Value,
    signal: Option<AbortSignal>,
) -> Result<AgentToolResult, String> {
    let request: Request = serde_json::from_value(arguments)
        .map_err(|error| format!("Invalid task document arguments: {error}"))?;
    let work = core
        .owned_context_commit(move |core| {
            let result = write_session(&core.session, &core.gap, |session| {
                if signal.as_ref().is_some_and(AbortSignal::aborted) {
                    return Ok(Err(DocumentError::Invalid("Operation aborted".into())));
                }
                match apply(session, request) {
                    Err(DocumentError::Persistence(error)) => Err(error),
                    other => Ok(other),
                }
            });
            // A dropped tool caller cannot abandon the failed-write outcome.
            match result {
                Ok(result) => result.map_err(|error| error.to_string()),
                Err(error) => Err(stop(core, error.message())),
            }
        })
        .map_err(|error| error.to_string())?;
    let value = match work.await {
        Ok(result) => result?,
        Err(error) => {
            let error = format!("Task document I/O failed: {error}");
            return Err(stop(&core, error));
        }
    };
    Ok(AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent::new(value.to_string()))],
        details: Some(value),
        ..AgentToolResult::default()
    })
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

pub(super) async fn expose(
    core: Arc<Core>,
    tools: Vec<Arc<AgentTool>>,
    signal: Option<AbortSignal>,
) -> Option<AgentContext> {
    let work = core.owned_context_commit(move |core| {
        let result = write_session(&core.session, &core.gap, |session| {
            if signal.as_ref().is_some_and(AbortSignal::aborted) {
                return Ok(None);
            }
            task_documents::expose_pending(session)
                .map(|count| (count > 0).then(|| session.build_session_projection()))
        });
        match result {
            Ok(Some(projection)) => {
                // Install inside the owned operation, even if its awaiting
                // request was cancelled after the write.
                core.refresh_context(&projection);
                Ok(Some(projection.messages))
            }
            Ok(None) => Ok(None),
            Err(error) => Err(stop(core, error.message())),
        }
    });
    match work {
        Ok(work) => match work.await {
            Ok(Ok(Some(messages))) => Some(AgentContext {
                messages: messages
                    .iter()
                    .map(crate::session::AgentMessage::to_agent)
                    .collect(),
                tools,
            }),
            Ok(Ok(None)) | Ok(Err(_)) => None,
            Err(error) => {
                stop(&core, format!("Task document I/O failed: {error}"));
                None
            }
        },
        Err(error) => {
            stop(&core, error.to_string());
            None
        }
    }
}

#[cfg(test)]
mod tests;
