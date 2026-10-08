//! `RELEASED_V2_EVENT_DISPOSITIONS` from
//! `packages/session/session-format-v1-to-v2/src/dispositions.ts`: the
//! released-v0 inventory without `assistant/chunk`, with the v2 forms of
//! `assistant/message`, `session-log-deepseek/delivery-accepted`, and
//! `session/end-seed`, and with `assistant/attempt`. Opaque members need no
//! entry here: a parsed [`serde_json::Value`] is always lossless JSON.

/// Exact top-level `data` members of one released-v2 event type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Disposition {
    pub(super) required: &'static [&'static str],
    pub(super) optional: &'static [&'static str],
}

/// How the frozen object literal answers `RELEASED_V2_EVENT_DISPOSITIONS[type]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Lookup {
    Own(Disposition),
    /// An `Object.prototype` member the literal inherits: defined, but with no
    /// `required` list.
    Inherited,
    Absent,
}

/// `Object.getOwnPropertyNames(Object.prototype)`.
const OBJECT_PROTOTYPE_NAMES: [&str; 12] = [
    "constructor",
    "__defineGetter__",
    "__defineSetter__",
    "hasOwnProperty",
    "__lookupGetter__",
    "__lookupSetter__",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toString",
    "valueOf",
    "__proto__",
    "toLocaleString",
];

const fn own(required: &'static [&'static str], optional: &'static [&'static str]) -> Lookup {
    Lookup::Own(Disposition { required, optional })
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
        "assistant/attempt" => own(&["turn", "step", "stream"], &[]),
        "assistant/message" => own(
            &["turn", "step", "message", "stream"],
            &["usage", "interrupted"],
        ),
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
        "session-log-deepseek/delivery-accepted" => {
            own(&["sessionId", "throughSeq"], &["sessionFormatVersion"])
        }
        "session/end-seed" => own(&[], &["inherited"]),
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
        "step/end" | "step/start" => own(&["turn", "step"], &[]),
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
        "tool/code-dispatch" => own(
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
        ),
        "tool/code-dispatch-start" => own(
            &[
                "rootCallId",
                "parentCallId",
                "subCallId",
                "name",
                "arguments",
            ],
            &[],
        ),
        "tool/result" => own(&["turn", "step", "message"], &["error", "meta"]),
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
    use std::collections::BTreeSet;

    fn shared_list(table: &Value, list: &str) -> Vec<String> {
        table["vocabulary"][list]
            .as_array()
            .unwrap_or_else(|| panic!("vocabulary.{list} array"))
            .iter()
            .map(|name| name.as_str().expect("type name").to_owned())
            .collect()
    }

    #[test]
    fn own_and_inherited_types_equal_the_shared_v3_row_table() {
        // The v3 row conformance spec checks these lists against the real exports.
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../conformance/session/v3-row-cases.json"
        );
        let text = std::fs::read_to_string(path).expect("read v3-row-cases.json");
        let table: Value = serde_json::from_str(&text).expect("parse v3-row-cases.json");
        let owned = shared_list(&table, "dispositionTypes");
        assert!(
            owned
                .iter()
                .all(|name| matches!(lookup(name), Lookup::Own(_)))
        );
        let inherited = shared_list(&table, "objectPrototypeNames");
        assert!(
            inherited
                .iter()
                .all(|name| lookup(name) == Lookup::Inherited)
        );
        assert_eq!(
            inherited.iter().cloned().collect::<BTreeSet<_>>(),
            OBJECT_PROTOTYPE_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect()
        );
        for absent in [
            "assistant/chunk",
            "feedback/message-put",
            "system/message",
            "tool/ptc-dispatch",
        ] {
            assert_eq!(lookup(absent), Lookup::Absent, "{absent}");
        }
    }
}
