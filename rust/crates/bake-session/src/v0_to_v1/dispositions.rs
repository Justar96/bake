//! `RELEASED_V0_EVENT_DISPOSITIONS` from
//! `packages/session/session-format-v0-to-v1/src/dispositions.ts`: the exact
//! top-level `data` members of every released-v0 event type, and the members
//! kept as owner-opaque JSON.

use crate::v2_to_v3::OBJECT_PROTOTYPE_NAMES;

/// One released-v0 event type's payload members.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Disposition {
    pub(super) required: &'static [&'static str],
    pub(super) optional: &'static [&'static str],
    /// Members checked only as lossless JSON.
    pub(super) opaque: &'static [&'static str],
}

/// How the frozen object literal answers `RELEASED_V0_EVENT_DISPOSITIONS[type]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Lookup {
    Own(Disposition),
    /// An `Object.prototype` member the literal inherits: defined, but with no
    /// `required` list.
    Inherited,
    Absent,
}

const fn own(required: &'static [&'static str], optional: &'static [&'static str]) -> Lookup {
    opaque(required, optional, &[])
}

const fn opaque(
    required: &'static [&'static str],
    optional: &'static [&'static str],
    opaque: &'static [&'static str],
) -> Lookup {
    Lookup::Own(Disposition {
        required,
        optional,
        opaque,
    })
}

pub(super) fn lookup(event_type: &str) -> Lookup {
    match event_type {
        "agent-preset/selected" => own(&["agentPreset"], &[]),
        "agent/inbox/spliced" => own(
            &["target", "start", "inserted"],
            &["removedCount", "outcome"],
        ),
        "approval/asked" => own(&["id", "toolName"], &["callId", "reason"]),
        "approval/decided" => own(&["id", "outcome"], &[]),
        "approval/policy" => own(&["policy"], &["source"]),
        "assistant/chunk" => own(&["turn", "step", "chunk"], &[]),
        "assistant/message" => own(&["turn", "step", "message"], &["usage", "interrupted"]),
        "command/done" => own(&["commandId", "kind"], &["text", "sourceEventSeq"]),
        "command/run" => own(&["commandId", "name", "source"], &["args"]),
        "compaction/end" => own(&["compactionId", "turn"], &["sourceCommandId", "error"]),
        "compaction/prune" => own(
            &["shadowedRange", "shadowedSeqs", "shadowedTokenCount"],
            &[],
        ),
        "compaction/start" => own(&["compactionId", "turn"], &["sourceCommandId"]),
        "compaction/summary" => own(
            &[
                "compactionId",
                "summary",
                "shadowedRange",
                "shadowedSeqs",
                "shadowedTokenCount",
                "provider",
                "model",
            ],
            &[
                "sourceCommandId",
                "maxTokens",
                "usage",
                "rawOutput",
                "llmStreamCall",
            ],
        ),
        "feedback/record" => own(&["text"], &[]),
        "goal/change" => own(
            &["kind", "version", "operation"],
            &[
                "goal",
                "roundsStarted",
                "createdAt",
                "updatedAt",
                "cleared",
                "clearedAt",
            ],
        ),
        "hook/invoked" => own(&["turn", "point", "dialect", "handlerId"], &["matcher"]),
        "hook/result" => own(
            &["turn", "point", "handlerId", "decision", "durationMs"],
            &["exitCode", "stderrSummary"],
        ),
        "llm/retry" => own(
            &[
                "retryId",
                "turn",
                "step",
                "provider",
                "mode",
                "policyKey",
                "retry",
                "delayMs",
                "failure",
            ],
            &["maxRetries"],
        ),
        "llm/retry-started" => own(&["retryId", "turn", "step", "retry"], &[]),
        "model/selection" => own(&["provider", "model"], &["reasoningEffort"]),
        "permission/preset" => own(&["preset"], &[]),
        "plan/mode" => own(&["active"], &[]),
        "request/context" => own(&["provider", "model"], &["contextWindow"]),
        "request/header" => own(&["header", "reason"], &["startsSeries"]),
        "sandbox/mode" => own(&["mode"], &["source"]),
        "schedule/change" => own(&["version", "operation"], &["schedule", "id", "acceptedAt"]),
        "session-log-deepseek/delivery-accepted" => own(&["sessionId", "throughSeq"], &[]),
        "session/end-seed" => own(&[], &[]),
        "session/title" => own(&["title", "messageSeqs", "source"], &[]),
        "session/title-llm-request" => own(
            &[
                "titleProvider",
                "messageSeqs",
                "route",
                "system",
                "messages",
                "maxTokens",
            ],
            &[],
        ),
        "step/end" => own(&["turn", "step"], &[]),
        "step/start" => own(&["turn", "step"], &[]),
        "subagent/descriptor" => own(
            &["mode", "version", "provider"],
            &[
                "label",
                "agentProvider",
                "agentModel",
                "agentReasoningEffort",
                "persona",
                "toolFilter",
            ],
        ),
        "subagent/model-selection-policy" => own(&["allowedModels"], &[]),
        "team/member" => own(&["version", "teamId", "member"], &[]),
        "team/message/delivered" => own(&["version", "teamId", "messageId", "targetId"], &[]),
        "team/message/queued" => own(&["version", "teamId", "message"], &[]),
        "team/task" => own(&["version", "teamId", "task"], &[]),
        "todo/write" => own(&["todos"], &[]),
        "tool-workflow/agent-end" => own(&["runId", "seq", "outcome"], &[]),
        "tool-workflow/agent-start" => own(&["runId", "seq", "label", "childId"], &["phase"]),
        "tool-workflow/run-end" => own(&["runId", "stopReason"], &[]),
        "tool-workflow/run-start" => own(&["runId", "name"], &[]),
        "tool/call" => own(&["turn", "step", "callId", "name", "arguments"], &[]),
        "tool/code-dispatch" => opaque(
            &[
                "rootCallId",
                "parentCallId",
                "subCallId",
                "name",
                "arguments",
                "isError",
                "content",
            ],
            &[],
            &["arguments"],
        ),
        "tool/code-dispatch-start" => opaque(
            &[
                "rootCallId",
                "parentCallId",
                "subCallId",
                "name",
                "arguments",
            ],
            &[],
            &["arguments"],
        ),
        "tool/result" => opaque(&["turn", "step", "message"], &["error", "meta"], &["meta"]),
        "turn/end" => own(&["turn", "reason"], &[]),
        "turn/start" => own(&["turn"], &[]),
        "user/message" => own(&["role", "id", "content", "source"], &[]),
        "web/deepseek-search-llm-request" => own(&["endpoint", "apiVersion", "body"], &[]),
        _ if OBJECT_PROTOTYPE_NAMES.contains(&event_type) => Lookup::Inherited,
        _ => Lookup::Absent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn names(value: &Value) -> Vec<&str> {
        value
            .as_array()
            .expect("member list")
            .iter()
            .map(|name| name.as_str().expect("member name"))
            .collect()
    }

    #[test]
    fn own_types_equal_the_shared_v0_to_v1_table() {
        // The v0→v1 conformance spec checks this table against the real export.
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../conformance/session/v0-to-v1-cases.json"
        );
        let text = std::fs::read_to_string(path).expect("read v0-to-v1-cases.json");
        let table: Value = serde_json::from_str(&text).expect("parse v0-to-v1-cases.json");
        let shared = table["dispositions"].as_object().expect("dispositions");
        assert_eq!(shared.len(), 51);
        for (name, entry) in shared {
            let Lookup::Own(disposition) = lookup(name) else {
                panic!("{name} is not an own disposition");
            };
            assert_eq!(disposition.required, names(&entry["required"]), "{name}");
            assert_eq!(disposition.optional, names(&entry["optional"]), "{name}");
            assert_eq!(disposition.opaque, names(&entry["opaque"]), "{name}");
        }
        for name in OBJECT_PROTOTYPE_NAMES {
            assert_eq!(lookup(name), Lookup::Inherited, "{name}");
        }
        for absent in [
            "steering/message",
            "compact/start",
            "system/message",
            "assistant/attempt",
        ] {
            assert_eq!(lookup(absent), Lookup::Absent, "{absent}");
        }
    }
}
