//! Session events as the JSON print mode writes them.
//!
//! Ported from Pi `packages/coding-agent/src/modes/json-event.ts` (v1.1.0):
//! events keep Pi's shape and member order, and `message_update` drops the
//! cumulative `partial` snapshot, keeping the message's usage and, for
//! `toolcall_start`, the call's id and name.

use bake_agent::AgentEvent;
use bake_ai::{AssistantContentBlock, AssistantMessageEvent};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::agent_session::SessionEvent;

fn to_value(value: &impl Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn object(kind: &str) -> Map<String, Value> {
    let mut object = Map::new();
    object.insert("type".into(), kind.into());
    object
}

/// Pi's `toJsonAssistantMessageEvent`. Where Pi throws for a
/// `toolcall_start` whose block is not a tool call, the event is written
/// without `id` and `toolName`.
fn json_assistant_message_event(event: &AssistantMessageEvent) -> Value {
    let mut value = to_value(event);
    if let Value::Object(object) = &mut value {
        object.shift_remove("partial");
        if let AssistantMessageEvent::ToolCallStart {
            content_index,
            partial,
        } = event
            && let Some(AssistantContentBlock::ToolCall(call)) = partial.content.get(*content_index)
        {
            object.insert("id".into(), call.id.clone().into());
            object.insert("toolName".into(), call.name.clone().into());
        }
    }
    value
}

/// Pi's `toJsonEvent` over an agent event; `agent_end` carries
/// `willRetry: false` (a session reports it through
/// [`SessionEvent::AgentEnd`]).
pub fn agent_event_json(event: &AgentEvent) -> Value {
    let mut out = object(event.kind());
    match event {
        AgentEvent::AgentStart | AgentEvent::TurnStart => {}
        AgentEvent::AgentEnd { messages } => {
            out.insert("messages".into(), to_value(messages));
            out.insert("willRetry".into(), false.into());
        }
        AgentEvent::TurnEnd {
            message,
            tool_results,
        } => {
            out.insert("message".into(), to_value(message));
            out.insert("toolResults".into(), to_value(tool_results));
        }
        AgentEvent::MessageStart { message } | AgentEvent::MessageEnd { message } => {
            out.insert("message".into(), to_value(message));
        }
        AgentEvent::MessageUpdate {
            message,
            assistant_message_event,
        } => {
            // Pi throws for a non-assistant update; here usage is omitted.
            if let Some(assistant) = message.as_assistant() {
                out.insert("usage".into(), to_value(&assistant.usage));
            }
            out.insert(
                "assistantMessageEvent".into(),
                json_assistant_message_event(assistant_message_event),
            );
        }
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        } => {
            out.insert("toolCallId".into(), tool_call_id.clone().into());
            out.insert("toolName".into(), tool_name.clone().into());
            out.insert("args".into(), args.clone());
        }
        AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            tool_name,
            args,
            partial_result,
        } => {
            out.insert("toolCallId".into(), tool_call_id.clone().into());
            out.insert("toolName".into(), tool_name.clone().into());
            out.insert("args".into(), args.clone());
            out.insert("partialResult".into(), to_value(partial_result));
        }
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            tool_name,
            result,
            is_error,
            duration_ms,
        } => {
            out.insert("toolCallId".into(), tool_call_id.clone().into());
            out.insert("toolName".into(), tool_name.clone().into());
            out.insert("result".into(), to_value(result));
            out.insert("isError".into(), (*is_error).into());
            if let Some(duration) = duration_ms {
                out.insert("durationMs".into(), (*duration).into());
            }
        }
    }
    Value::Object(out)
}

/// Pi's `toJsonEvent` over a session event.
pub fn session_event_json(event: &SessionEvent<'_>) -> Value {
    match event {
        SessionEvent::Agent(event) => agent_event_json(event),
        SessionEvent::AgentSettled { aborted } => {
            let mut out = object("agent_settled");
            out.insert("aborted".into(), (*aborted).into());
            Value::Object(out)
        }
        SessionEvent::ThinkingLevelChanged(level) => {
            let mut out = object("thinking_level_changed");
            out.insert("level".into(), level.as_str().into());
            Value::Object(out)
        }
        SessionEvent::SessionInfoChanged(name) => {
            let mut out = object("session_info_changed");
            if let Some(name) = name {
                out.insert("name".into(), name.clone().into());
            }
            Value::Object(out)
        }
        SessionEvent::AgentEnd {
            messages,
            will_retry,
        } => {
            let mut out = object("agent_end");
            out.insert("messages".into(), to_value(messages));
            out.insert("willRetry".into(), (*will_retry).into());
            Value::Object(out)
        }
        SessionEvent::CompactionStart { reason } => {
            let mut out = object("compaction_start");
            out.insert("reason".into(), reason.as_str().into());
            Value::Object(out)
        }
        SessionEvent::CompactionEnd {
            reason,
            result,
            aborted,
            will_retry,
            error_message,
        } => {
            // Pi's `undefined` members are left out.
            let mut out = object("compaction_end");
            out.insert("reason".into(), reason.as_str().into());
            if let Some(result) = result {
                out.insert("result".into(), to_value(result));
            }
            out.insert("aborted".into(), (*aborted).into());
            out.insert("willRetry".into(), (*will_retry).into());
            if let Some(message) = error_message {
                out.insert("errorMessage".into(), message.clone().into());
            }
            Value::Object(out)
        }
        SessionEvent::AutoRetryStart {
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        } => {
            let mut out = object("auto_retry_start");
            out.insert("attempt".into(), (*attempt).into());
            out.insert("maxAttempts".into(), (*max_attempts).into());
            out.insert("delayMs".into(), (*delay_ms).into());
            out.insert("errorMessage".into(), error_message.clone().into());
            Value::Object(out)
        }
        SessionEvent::AutoRetryEnd {
            success,
            attempt,
            final_error,
        } => {
            let mut out = object("auto_retry_end");
            out.insert("success".into(), (*success).into());
            out.insert("attempt".into(), (*attempt).into());
            if let Some(error) = final_error {
                out.insert("finalError".into(), error.clone().into());
            }
            Value::Object(out)
        }
        SessionEvent::SummarizationRetryScheduled {
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        } => {
            let mut out = object("summarization_retry_scheduled");
            out.insert("attempt".into(), (*attempt).into());
            out.insert("maxAttempts".into(), (*max_attempts).into());
            out.insert("delayMs".into(), (*delay_ms).into());
            out.insert("errorMessage".into(), error_message.clone().into());
            Value::Object(out)
        }
        SessionEvent::SummarizationRetryAttemptStart { reason } => {
            let mut out = object("summarization_retry_attempt_start");
            out.insert("source".into(), "compaction".into());
            out.insert("reason".into(), reason.as_str().into());
            Value::Object(out)
        }
        SessionEvent::SummarizationRetryFinished => {
            Value::Object(object("summarization_retry_finished"))
        }
        SessionEvent::QueueUpdate {
            steering,
            follow_up,
        } => {
            let mut out = object("queue_update");
            out.insert("steering".into(), (*steering).into());
            out.insert("followUp".into(), (*follow_up).into());
            Value::Object(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use bake_ai::{AssistantMessage, StopReason, TextContent, ToolCall};
    use serde_json::json;

    use super::*;

    fn partial() -> AssistantMessage {
        let mut message = AssistantMessage::pending(&bake_ai::providers::faux::faux_model("m"));
        message.content = vec![
            AssistantContentBlock::Text(TextContent::new("hi")),
            AssistantContentBlock::ToolCall(ToolCall {
                id: "call_1".into(),
                name: "read".into(),
                arguments: Default::default(),
                ..Default::default()
            }),
        ];
        message
    }

    // Pi `test/json-event.test.ts` covers the same reductions.
    #[test]
    fn updates_drop_the_partial_and_keep_usage_and_call_names() {
        let message = partial();
        let update = AgentEvent::MessageUpdate {
            message: message.clone().into(),
            assistant_message_event: AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: "hi".into(),
                partial: message.clone(),
            },
        };
        let value = agent_event_json(&update);
        assert_eq!(
            value["assistantMessageEvent"],
            json!({ "type": "text_delta", "contentIndex": 0, "delta": "hi" })
        );
        assert_eq!(value["usage"], to_value(&message.usage));
        let keys: Vec<&String> = value
            .as_object()
            .map(|o| o.keys().collect())
            .unwrap_or_default();
        assert_eq!(keys, ["type", "usage", "assistantMessageEvent"]);

        let start = AgentEvent::MessageUpdate {
            message: message.clone().into(),
            assistant_message_event: AssistantMessageEvent::ToolCallStart {
                content_index: 1,
                partial: message.clone(),
            },
        };
        assert_eq!(
            agent_event_json(&start)["assistantMessageEvent"],
            json!({ "type": "toolcall_start", "contentIndex": 1, "id": "call_1", "toolName": "read" })
        );
        let mut done = message.clone();
        done.stop_reason = StopReason::Stop;
        let finished = AgentEvent::MessageUpdate {
            message: message.into(),
            assistant_message_event: AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: done.clone(),
            },
        };
        assert_eq!(
            agent_event_json(&finished)["assistantMessageEvent"]["message"],
            to_value(&done)
        );
    }

    #[test]
    fn lifecycle_events_follow_pi_shapes() {
        assert_eq!(
            agent_event_json(&AgentEvent::AgentEnd { messages: vec![] }).to_string(),
            r#"{"type":"agent_end","messages":[],"willRetry":false}"#
        );
        assert_eq!(
            agent_event_json(&AgentEvent::ToolExecutionStart {
                tool_call_id: "c".into(),
                tool_name: "t".into(),
                args: json!({ "a": 1 }),
            })
            .to_string(),
            r#"{"type":"tool_execution_start","toolCallId":"c","toolName":"t","args":{"a":1}}"#
        );
        assert_eq!(
            session_event_json(&SessionEvent::AgentSettled { aborted: false }).to_string(),
            r#"{"type":"agent_settled","aborted":false}"#
        );
    }

    // Pi's session events pass through `toJsonEvent` unchanged, so
    // `undefined` members are left out.
    #[test]
    fn compaction_and_retry_events_follow_pi_shapes() {
        use crate::agent_session::CompactionReason;
        let line = |event: SessionEvent<'_>| session_event_json(&event).to_string();
        assert_eq!(
            line(SessionEvent::AgentEnd {
                messages: &[],
                will_retry: true
            }),
            r#"{"type":"agent_end","messages":[],"willRetry":true}"#
        );
        assert_eq!(
            line(SessionEvent::CompactionStart {
                reason: CompactionReason::Threshold
            }),
            r#"{"type":"compaction_start","reason":"threshold"}"#
        );
        assert_eq!(
            line(SessionEvent::CompactionEnd {
                reason: CompactionReason::Overflow,
                result: None,
                aborted: false,
                will_retry: false,
                error_message: Some("x".into()),
            }),
            r#"{"type":"compaction_end","reason":"overflow","aborted":false,"willRetry":false,"errorMessage":"x"}"#
        );
        let result = crate::compaction::CompactionResult {
            summary: "s".into(),
            first_kept_entry_id: "e1".into(),
            tokens_before: 9,
            estimated_tokens_after: Some(3),
            usage: None,
            details: Some(json!({ "readFiles": [], "modifiedFiles": [] })),
        };
        assert_eq!(
            line(SessionEvent::CompactionEnd {
                reason: CompactionReason::Manual,
                result: Some(&result),
                aborted: false,
                will_retry: false,
                error_message: None,
            }),
            r#"{"type":"compaction_end","reason":"manual","result":{"summary":"s","firstKeptEntryId":"e1","tokensBefore":9,"estimatedTokensAfter":3,"details":{"readFiles":[],"modifiedFiles":[]}},"aborted":false,"willRetry":false}"#
        );
        assert_eq!(
            line(SessionEvent::AutoRetryStart {
                attempt: 1,
                max_attempts: 3,
                delay_ms: 2000,
                error_message: "overloaded".into(),
            }),
            r#"{"type":"auto_retry_start","attempt":1,"maxAttempts":3,"delayMs":2000,"errorMessage":"overloaded"}"#
        );
        assert_eq!(
            line(SessionEvent::AutoRetryEnd {
                success: false,
                attempt: 2,
                final_error: Some("Retry cancelled".into())
            }),
            r#"{"type":"auto_retry_end","success":false,"attempt":2,"finalError":"Retry cancelled"}"#
        );
        assert_eq!(
            line(SessionEvent::AutoRetryEnd {
                success: true,
                attempt: 1,
                final_error: None
            }),
            r#"{"type":"auto_retry_end","success":true,"attempt":1}"#
        );
        assert_eq!(
            line(SessionEvent::SummarizationRetryScheduled {
                attempt: 1,
                max_attempts: 3,
                delay_ms: 5,
                error_message: "terminated".into(),
            }),
            r#"{"type":"summarization_retry_scheduled","attempt":1,"maxAttempts":3,"delayMs":5,"errorMessage":"terminated"}"#
        );
        assert_eq!(
            line(SessionEvent::SummarizationRetryAttemptStart {
                reason: CompactionReason::Manual
            }),
            r#"{"type":"summarization_retry_attempt_start","source":"compaction","reason":"manual"}"#
        );
        assert_eq!(
            line(SessionEvent::SummarizationRetryFinished),
            r#"{"type":"summarization_retry_finished"}"#
        );
    }
}
