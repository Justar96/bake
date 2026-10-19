//! Approval requests over the RPC protocol (D24).
//!
//! A tool call that asks for wider access reaches the session's approval
//! broker ([`crate::permissions::ApprovalBroker`]), the same queue the
//! terminal answers (`bake-tui`'s live session). RPC mode connects to that
//! broker as its one frontend, so the engine writes each request and its
//! answer to the session file before the call runs, exactly as the
//! interactive path does ([`crate::permissions::history`]).
//!
//! The wire record is Pi's own `extension_ui_request` with `method:
//! "confirm"` (`src/modes/rpc/rpc-types.ts`), so a client written for Pi
//! can answer it with `{"type":"extension_ui_response","id":…,
//! "confirmed":true}`. Bake adds one member, `approval`, with the tool, the
//! call, the reason, and the call's arguments, so a client can show what
//! would run; a Pi client ignores it. `confirmed: true` allows that one
//! call, `confirmed: false` rejects it, and `cancelled: true` withdraws it.
//! Nothing else is granted: no standing permission, preset, or later call
//! changes.
//!
//! A request waits until the client answers it. A client that disconnects,
//! or never answers, leaves the call refused: shutdown closes the broker,
//! which settles every queued request as cancelled, and the engine refuses
//! the call before any effect.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use serde_json::{Value, json};

use crate::permissions::{ApprovalAnswer, ApprovalBroker, ApprovalPrompt};
use crate::print_mode::{SharedWriter, write_out};
use crate::rpc::jsonl::serialize_json_line;
use crate::rpc::types::ExtensionUiResponse;

/// The `method` of the request this bridge sends.
const METHOD: &str = "confirm";

/// What the request's `title` says.
const TITLE: &str = "Approve this tool call?";

/// Announces approval requests on stdout and answers them from stdin.
#[derive(Debug)]
pub struct ApprovalBridge {
    broker: ApprovalBroker,
    /// The request id each announced generation was sent under, so an
    /// answer settles the request it names and nothing else.
    announced: Mutex<HashMap<String, u64>>,
}

impl ApprovalBridge {
    /// Connects to `broker` and announces its queue on `out`. Returns
    /// `None` when another frontend is already connected or the broker is
    /// closed, as [`ApprovalBroker::connect`] reports.
    pub fn connect(broker: &ApprovalBroker, out: &SharedWriter) -> Option<std::sync::Arc<Self>> {
        let bridge = std::sync::Arc::new(Self {
            broker: broker.clone(),
            announced: Mutex::new(HashMap::new()),
        });
        let listener = {
            let bridge = std::sync::Arc::clone(&bridge);
            let out = std::sync::Arc::clone(out);
            move |queue: Vec<ApprovalPrompt>| {
                for record in bridge.announce(&queue) {
                    write_out(&out, &serialize_json_line(&record));
                }
                true
            }
        };
        broker.connect(Box::new(listener)).then_some(bridge)
    }

    /// The records to send for `queue`: one for its oldest request, the
    /// one being decided, the first time it is seen. Ids of requests that
    /// have left the queue are forgotten, so they answer nothing.
    fn announce(&self, queue: &[ApprovalPrompt]) -> Vec<Value> {
        let mut announced = self
            .announced
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        announced
            .retain(|_, generation| queue.iter().any(|prompt| prompt.generation == *generation));
        let Some(prompt) = queue.first() else {
            return Vec::new();
        };
        let id = request_id(prompt.generation);
        if announced.insert(id.clone(), prompt.generation).is_some() {
            return Vec::new();
        }
        vec![request(&id, prompt)]
    }

    /// Settles the request `response` names, if it is still the one being
    /// decided. Returns whether it did; an answer for an unknown or
    /// already settled request settles nothing, as in Pi.
    pub fn answer(&self, response: &ExtensionUiResponse) -> bool {
        let generation = {
            let mut announced = self
                .announced
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match announced.remove(&response.id) {
                Some(generation) => generation,
                None => return false,
            }
        };
        self.broker.answer(generation, answer_of(response))
    }
}

/// The `extension_ui_request` id a generation is announced under.
fn request_id(generation: u64) -> String {
    format!("approval-{generation}")
}

/// What `response` decides: a withdrawal outranks a `confirmed` member, as
/// Pi's `confirm` parser takes `cancelled` first, and a response with
/// neither rejects, as Pi's `false` default does.
fn answer_of(response: &ExtensionUiResponse) -> ApprovalAnswer {
    if response.cancelled == Some(true) {
        return ApprovalAnswer::Cancel;
    }
    match response.confirmed {
        Some(true) => ApprovalAnswer::AllowOnce,
        _ => ApprovalAnswer::Reject,
    }
}

/// Pi's `confirm` request for one queued approval, with Bake's `approval`
/// member.
fn request(id: &str, prompt: &ApprovalPrompt) -> Value {
    let mut approval = serde_json::Map::new();
    approval.insert("toolName".into(), prompt.tool_name.clone().into());
    approval.insert("callId".into(), prompt.call_id.clone().into());
    if let Some(parent) = &prompt.parent_tool_call_id {
        approval.insert("parentToolCallId".into(), parent.clone().into());
    }
    approval.insert("reason".into(), prompt.reason.clone().into());
    approval.insert("arguments".into(), prompt.arguments.clone());
    json!({
        "type": "extension_ui_request",
        "id": id,
        "method": METHOD,
        "title": TITLE,
        "message": message(prompt),
        "approval": Value::Object(approval),
    })
}

/// The one-line question a Pi client shows: the tool, its call, and why.
fn message(prompt: &ApprovalPrompt) -> String {
    format!(
        "{} (call {}) requests: {}",
        prompt.tool_name, prompt.call_id, prompt.reason
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(generation: u64) -> ApprovalPrompt {
        ApprovalPrompt {
            generation,
            tool_name: "bash".into(),
            call_id: "call_1".into(),
            parent_tool_call_id: None,
            reason: "escalate sandbox to danger-full-access: install".into(),
            arguments: json!({ "command": "ls" }),
        }
    }

    fn bridge() -> std::sync::Arc<ApprovalBridge> {
        std::sync::Arc::new(ApprovalBridge {
            broker: ApprovalBroker::new(),
            announced: Mutex::new(HashMap::new()),
        })
    }

    #[test]
    fn announces_the_oldest_request_once_as_pis_confirm_record() {
        let bridge = bridge();
        let records = bridge.announce(&[prompt(1), prompt(2)]);
        assert_eq!(
            records,
            vec![json!({
                "type": "extension_ui_request",
                "id": "approval-1",
                "method": "confirm",
                "title": TITLE,
                "message": "bash (call call_1) requests: escalate sandbox to danger-full-access: install",
                "approval": {
                    "toolName": "bash",
                    "callId": "call_1",
                    "reason": "escalate sandbox to danger-full-access: install",
                    "arguments": { "command": "ls" },
                },
            })]
        );
        assert!(bridge.announce(&[prompt(1), prompt(2)]).is_empty());
        // The next request is announced once the first has been decided.
        assert_eq!(bridge.announce(&[prompt(2)]).len(), 1);
    }

    #[test]
    fn a_withdrawn_requests_id_answers_nothing() {
        let bridge = bridge();
        assert_eq!(bridge.announce(&[prompt(1)]).len(), 1);
        assert!(bridge.announce(&[]).is_empty());
        assert!(!bridge.answer(&ExtensionUiResponse {
            id: "approval-1".into(),
            value: None,
            confirmed: Some(true),
            cancelled: None,
        }));
    }

    #[test]
    fn reads_pis_confirm_answers() {
        let answer = |confirmed, cancelled| {
            answer_of(&ExtensionUiResponse {
                id: "approval-1".into(),
                value: None,
                confirmed,
                cancelled,
            })
        };
        assert_eq!(answer(Some(true), None), ApprovalAnswer::AllowOnce);
        assert_eq!(answer(Some(false), None), ApprovalAnswer::Reject);
        assert_eq!(answer(None, None), ApprovalAnswer::Reject);
        assert_eq!(answer(Some(true), Some(true)), ApprovalAnswer::Cancel);
    }
}
