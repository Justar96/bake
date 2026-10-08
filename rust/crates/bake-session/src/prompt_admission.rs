//! Development-only prompt admission inputs over a restored Session: the
//! system-prompt reconciliation `SystemPromptProjection.project` decides in
//! `packages/core/agent-loop/src/runtime-context.ts`, the surface's
//! `contentGeneration`, and the private `toolsChanged` test of
//! `ReactLoopAgent` in `packages/core/agent-loop/src/agent.ts`.
//!
//! [`system_prompt_commits`] reads the current `system/message` nodes in
//! surface order and returns the ordered commits the projection would return,
//! without committing them. A node's text is `''` for empty content and a
//! single text block's text otherwise; several blocks or a non-text block are
//! active content that is not text, which no rendering equals and which is
//! never dormant. The first prompt, even empty, is appended. An incapable
//! route, a new series, or an empty rendering empties each later non-empty
//! node in surface order and then rewrites the head when its text differs; a
//! continuing capable series appends a changed rendering and otherwise
//! commits nothing.
//!
//! [`content_generation`] counts the restored fold's committed replacements
//! and `image/offload` events, inherited ones included, as Session
//! construction does. Appends, closers, and the end seed do not count.
//!
//! [`tools_changed`] reports whether a candidate tool list differs from the
//! restored request header's, as `!headerEquals(baseline,
//! canonicalHeader({...baseline, tools}))` does. The candidate spreads the
//! baseline, so its config and adapter defaults always equal the baseline's
//! and only the tools decide: in order, each pair compared by its
//! `JSON.stringify` text. Member order counts, with JavaScript's
//! array-index keys enumerated first, and numbers compare by double value,
//! so `1.0` and `1e0` equal `1` and -0 equals 0, as their text does. The
//! comparison descends only where both sides are containers, so the
//! restored header's qualified depth bounds it. A candidate too deep for
//! JavaScript's own stack is outside what is claimed.
//!
//! [`starts_request_series`] composes these with the caller's own state as
//! the agent's `startsSeries` disjunction does: a declared series, a
//! generation that moved since the last request, or changed tools on a route
//! without native tool updates. The generation at the last request is agent
//! state no log reconstructs, so the caller supplies it.
//!
//! These functions refuse nothing: restoration's native limits bound the
//! restored side, and every candidate value compares exactly.

use serde_json::{Map, Value};

use crate::RestoredLog;
use crate::request::{SurfaceKind, js_members};

/// The route and series facts one prompt decision is made under,
/// `SystemPromptDecisionInput`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptDecision {
    /// The prepared route reads a later `system` message as the effective
    /// prompt.
    pub in_history: bool,
    /// This step's request starts a new model-message series.
    pub starts_series: bool,
}

/// How one [`SystemPromptCommit`] joins the surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptIntent {
    /// `{surfaceOp: 'append'}`: a new system node.
    Append,
    /// A replacement of exactly the surviving system node `seq`, with
    /// `sourceEventSeqs: [seq]`.
    Replace { seq: u64 },
}

/// One uncommitted `system/message`. Its message is
/// `createSystemMessage(text, '@deepseek-ai/dsh-system-prompt')`: role
/// `system`, content `[]` for empty text and one text block otherwise, and
/// a freshly minted id this port does not produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPromptCommit {
    pub text: String,
    pub intent: PromptIntent,
}

/// One current `system/message` node; `None` text is active content that is
/// not text.
struct SystemNode<'a> {
    seq: u64,
    text: Option<&'a str>,
}

/// `SystemPromptProjection.project(rendered, input)` over the restored
/// Session: the ordered commits, empty when no update is needed.
pub fn system_prompt_commits(
    restored: &RestoredLog,
    rendered: &str,
    input: PromptDecision,
) -> Vec<SystemPromptCommit> {
    let nodes: Vec<SystemNode<'_>> = restored
        .fold()
        .current_nodes()
        .filter(|(_, kind, _)| *kind == SurfaceKind::System)
        .map(|(seq, _, message)| SystemNode {
            seq,
            text: text_of(message),
        })
        .collect();
    let append = || SystemPromptCommit {
        text: rendered.to_owned(),
        intent: PromptIntent::Append,
    };
    let replace = |seq, text: &str| SystemPromptCommit {
        text: text.to_owned(),
        intent: PromptIntent::Replace { seq },
    };
    let Some((head, tails)) = nodes.split_first() else {
        return vec![append()];
    };
    if !input.in_history || input.starts_series || rendered.is_empty() {
        let mut updates: Vec<SystemPromptCommit> = tails
            .iter()
            .filter(|node| node.text != Some(""))
            .map(|node| replace(node.seq, ""))
            .collect();
        if head.text != Some(rendered) {
            updates.push(replace(head.seq, rendered));
        }
        return updates;
    }
    let latest = nodes
        .iter()
        .rev()
        .find(|node| node.text != Some(""))
        .unwrap_or(head);
    if latest.text == Some(rendered) {
        return Vec::new();
    }
    vec![append()]
}

/// The projection's node text: `''` for empty content, a lone text block's
/// text, otherwise `None`. A content member that is not an array, or a text
/// block whose text is not a string, also reads as `None`, as JavaScript
/// compares it unequal to every string.
fn text_of(message: &Map<String, Value>) -> Option<&str> {
    match message.get("content")?.as_array()?.as_slice() {
        [] => Some(""),
        [block] if block.get("type").and_then(Value::as_str) == Some("text") => {
            block.get("text")?.as_str()
        }
        _ => None,
    }
}

/// `session.surface.contentGeneration` of the restored Session.
pub fn content_generation(restored: &RestoredLog) -> u64 {
    restored.fold().content_generation()
}

/// The agent's `toolsChanged(tools)`: false without a restored request
/// header, otherwise whether the candidate tools differ from its tools.
pub fn tools_changed(restored: &RestoredLog, tools: &[Value]) -> bool {
    let Some(header) = restored.request_header() else {
        return false;
    };
    let baseline = header
        .get("tools")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    baseline.len() != tools.len()
        || baseline
            .iter()
            .zip(tools)
            .any(|(baseline, tool)| !same_json_text(baseline, tool))
}

/// The agent's `startsSeries` for the restored Session: `declared`, or a
/// content generation other than `generation_at_last_request`, or, when the
/// route has no native tool updates, [`tools_changed`].
pub fn starts_request_series(
    declared: bool,
    generation_at_last_request: u64,
    restored: &RestoredLog,
    tool_update_route: bool,
    tools: &[Value],
) -> bool {
    declared
        || generation_at_last_request != content_generation(restored)
        || (!tool_update_route && tools_changed(restored, tools))
}

/// Whether `JSON.stringify` writes the same text for two parsed values.
/// JavaScript prints each double distinctly except ±0, and `as_f64` rounds
/// a decimal integer as `JSON.parse` does.
fn same_json_text(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_json_text(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && js_members(a)
                    .into_iter()
                    .zip(js_members(b))
                    .all(|((ka, va), (kb, vb))| ka == kb && same_json_text(va, vb))
        }
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_compare_as_their_javascript_text() {
        let parse = |text: &str| serde_json::from_str::<Value>(text).expect("JSON");
        for (a, b, same) in [
            ("1", "1.0", true),
            ("1", "1e0", true),
            ("0", "-0", true),
            ("0", "-0.0", true),
            ("1", "1.5", false),
            ("9007199254740992", "9007199254740993", true),
            ("1", "\"1\"", false),
            ("null", "null", true),
        ] {
            assert_eq!(same_json_text(&parse(a), &parse(b)), same, "{a} {b}");
        }
    }

    #[test]
    fn object_members_compare_in_javascript_order() {
        let a = json!({"x": 1, "1": 2});
        let b = json!({"1": 2, "x": 1});
        let c = json!({"y": 1, "x": 1});
        let d = json!({"x": 1, "y": 1});
        assert!(same_json_text(&a, &b));
        assert!(!same_json_text(&c, &d));
    }

    #[test]
    fn node_text_follows_the_projection() {
        let message = |content: Value| match json!({ "content": content }) {
            Value::Object(fields) => fields,
            _ => unreachable!("object literal"),
        };
        assert_eq!(text_of(&message(json!([]))), Some(""));
        assert_eq!(
            text_of(&message(json!([{"type": "text", "text": ""}]))),
            Some("")
        );
        assert_eq!(
            text_of(&message(json!([{"type": "text", "text": "a"}]))),
            Some("a")
        );
        assert_eq!(
            text_of(&message(json!([{"type": "reasoning", "text": "a"}]))),
            None
        );
        assert_eq!(
            text_of(&message(
                json!([{"type": "text", "text": "a"}, {"type": "text", "text": "b"}])
            )),
            None
        );
        assert_eq!(
            text_of(&message(json!([{"type": "text", "text": 1}]))),
            None
        );
        assert_eq!(text_of(&message(json!("a"))), None);
    }
}
