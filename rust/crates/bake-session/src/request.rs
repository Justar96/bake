//! Development-only request derivation over a closed subset of current-format
//! events.
//!
//! [`RequestFold`] folds one event at a time into the state a model request
//! needs: the current surface nodes, the request header, and the Session id.
//! It reads no codec types or diagnostics; [`crate::replay`] admits events and
//! converts them to [`Fact`]s first. The fold owns its node payloads so each
//! request snapshot outlives the parsed rows it came from.
//!
//! Every fold transition matches `packages/core/session` for the subset the
//! import side admits: surface appends, positional surface replacements, and
//! at most one `request/header`. A replacement is planned against the current
//! nodes and refused, leaving the fold unchanged, unless it passes the
//! replacement checks of `planSurfaceEvent` in
//! `packages/core/session/src/surface.ts` that remain for this subset:
//! endpoints located among the current nodes, source coverage of every
//! shadowed node, the `tool/result` rewrite rule, and the protected system
//! head. The codec already proved sequence contiguity and the event's own
//! surface metadata. Message projections, header changes, and tool updates are
//! refused before they reach the fold.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

/// A surface event type, which fixes how its message derives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SurfaceKind {
    System,
    User,
    Assistant,
    ToolResult,
}

/// How a surface event joins the current nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SurfaceOp {
    /// Enter at the tail.
    Append,
    /// Shadow the current nodes from `start` through `end`, inclusive by node
    /// position, with this node. `sources` are the event's decoded
    /// `sourceEventSeqs`, empty when the row omits them.
    Replace {
        start: u64,
        end: u64,
        sources: Vec<u64>,
    },
}

/// One admitted event's effect on request derivation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Fact {
    /// A surface event. `payload` is the event's whole `data` for a tool
    /// result, whose replacement rule compares members outside the message,
    /// and the message object for every other kind.
    Surface {
        seq: u64,
        kind: SurfaceKind,
        op: SurfaceOp,
        payload: Map<String, Value>,
    },
    /// A `request/header` in canonical form: `config` is copied verbatim and
    /// `tools` is absent or non-empty.
    Header {
        config: Map<String, Value>,
        tools: Option<Vec<Value>>,
    },
    /// An event derivation does not read.
    LogOnly,
}

/// Why the fold refused a fact. The fold is unchanged after a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FoldRefusal {
    /// A second `request/header`. Tool history after a header change depends
    /// on order-sensitive schema comparisons this fold does not implement.
    HeaderChange,
    /// The replacement's `startSeq` is not a current node.
    ReplaceStart,
    /// The replacement's `endSeq` is not a current node.
    ReplaceEnd,
    /// The start node sits after the end node.
    ReplaceOrder,
    /// A shadowed node is missing from the replacement's sources.
    ReplaceSources,
    /// A `tool/result` replacement shadows more than one node.
    ToolResultSpan,
    /// A `tool/result` replacement shadows a node of another kind.
    ToolResultTarget,
    /// A `tool/result` replacement changes more than its result's content.
    ToolResultRest,
    /// The replacement covers a system head without being one `system/message`
    /// over exactly that node.
    SystemHead,
}

#[derive(Debug, Clone, PartialEq)]
struct Header {
    config: Map<String, Value>,
    tools: Option<Vec<Value>>,
}

/// One current surface node.
#[derive(Debug, Clone, PartialEq)]
struct Node {
    seq: u64,
    kind: SurfaceKind,
    /// As [`Fact::Surface`] carries it.
    payload: Map<String, Value>,
}

impl Node {
    /// The logged message object.
    fn message(&self) -> &Map<String, Value> {
        match self.kind {
            SurfaceKind::ToolResult => self.payload["message"]
                .as_object()
                .expect("admitted tool results carry a message object"),
            _ => &self.payload,
        }
    }

    /// `deriveEventMessage`: an empty system message records "no system
    /// prompt", and an empty assistant message only hosts usage; neither
    /// reaches the model, but both keep their surface position.
    fn derived(&self) -> Option<&Map<String, Value>> {
        let message = self.message();
        let empty = message
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty);
        (!(empty && matches!(self.kind, SurfaceKind::System | SurfaceKind::Assistant)))
            .then_some(message)
    }
}

/// The derivation state of one Session prefix.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RequestFold {
    session_id: String,
    nodes: Vec<Node>,
    header: Option<Header>,
}

impl RequestFold {
    pub(crate) const fn new(session_id: String) -> Self {
        Self {
            session_id,
            nodes: Vec::new(),
            header: None,
        }
    }

    /// Apply one fact, or refuse it without changing the fold.
    pub(crate) fn append(&mut self, fact: Fact) -> Result<(), FoldRefusal> {
        match fact {
            Fact::Surface {
                seq,
                kind,
                op,
                payload,
            } => {
                let node = Node { seq, kind, payload };
                match op {
                    SurfaceOp::Append => self.nodes.push(node),
                    SurfaceOp::Replace {
                        start,
                        end,
                        sources,
                    } => {
                        let range = self.plan_replacement(&node, start, end, &sources)?;
                        self.nodes.splice(range, [node]);
                    }
                }
            }
            Fact::Header { config, tools } => {
                if self.header.is_some() {
                    return Err(FoldRefusal::HeaderChange);
                }
                self.header = Some(Header { config, tools });
            }
            Fact::LogOnly => {}
        }
        Ok(())
    }

    /// The node positions a replacement shadows, after the remaining
    /// replacement checks of `planSurfaceEvent`, in its order. Endpoints are
    /// located by current position, so a replacement node's later seq can open
    /// a range that ends at a smaller seq.
    fn plan_replacement(
        &self,
        node: &Node,
        start: u64,
        end: u64,
        sources: &[u64],
    ) -> Result<std::ops::RangeInclusive<usize>, FoldRefusal> {
        let position = |seq| self.nodes.iter().position(|node| node.seq == seq);
        let start_index = position(start).ok_or(FoldRefusal::ReplaceStart)?;
        let end_index = position(end).ok_or(FoldRefusal::ReplaceEnd)?;
        if start_index > end_index {
            return Err(FoldRefusal::ReplaceOrder);
        }
        let shadowed = &self.nodes[start_index..=end_index];
        // One membership set per replacement, as TypeScript builds a `Set`; the
        // caller's source budget, not this fold, bounds its size.
        let sources: BTreeSet<u64> = sources.iter().copied().collect();
        if shadowed.iter().any(|old| !sources.contains(&old.seq)) {
            return Err(FoldRefusal::ReplaceSources);
        }
        if node.kind == SurfaceKind::ToolResult {
            let [original] = shadowed else {
                return Err(FoldRefusal::ToolResultSpan);
            };
            if original.kind != SurfaceKind::ToolResult {
                return Err(FoldRefusal::ToolResultTarget);
            }
            if !same_outside_result_content(&original.payload, &node.payload) {
                return Err(FoldRefusal::ToolResultRest);
            }
        }
        if start_index == 0
            && self.nodes[0].kind == SurfaceKind::System
            && (node.kind != SurfaceKind::System || shadowed.len() != 1)
        {
            return Err(FoldRefusal::SystemHead);
        }
        Ok(start_index..=end_index)
    }

    /// The request a step would send after this prefix, or `None` before the
    /// first `request/header`.
    pub(crate) fn request(&self) -> Option<Request> {
        let header = self.header.as_ref()?;
        Some(Request {
            config: header.config.clone(),
            messages: self
                .nodes
                .iter()
                .filter_map(Node::derived)
                .cloned()
                .collect(),
            tools: header.tools.clone(),
            session_id: self.session_id.clone(),
        })
    }
}

/// `assertToolResultRewrite`'s comparison of two tool-result `data` objects
/// with `message.content[0].content` set aside on both sides. Session
/// construction proved that each message's `content` is exactly one block
/// object with array `content`, so only member sets and the remaining member
/// values decide. `Value` equality ignores member order, as `isDeepEqualJson`
/// does, and the import side admits only safe-integer numbers.
fn same_outside_result_content(
    original: &Map<String, Value>,
    replacement: &Map<String, Value>,
) -> bool {
    fn same_except(a: &Map<String, Value>, b: &Map<String, Value>, key: &str) -> bool {
        a.len() == b.len()
            && a.iter().all(|(name, value)| {
                b.get(name)
                    .is_some_and(|other| name == key || other == value)
            })
    }
    fn message(data: &Map<String, Value>) -> Option<&Map<String, Value>> {
        data.get("message")?.as_object()
    }
    fn block(data: &Map<String, Value>) -> Option<&Map<String, Value>> {
        data.get("message")?["content"][0].as_object()
    }
    let levels = [
        (Some(original), Some(replacement), "message"),
        (message(original), message(replacement), "content"),
        (block(original), block(replacement), "content"),
    ];
    levels
        .iter()
        .all(|level| matches!(level, (Some(a), Some(b), key) if same_except(a, b, key)))
}

/// One provider-neutral model request derived from a Session prefix.
///
/// Messages are the logged message objects, unchanged: original IDs, sources,
/// unknown members, and `null` members are kept, and an omitted member stays
/// omitted.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    config: Map<String, Value>,
    messages: Vec<Map<String, Value>>,
    tools: Option<Vec<Value>>,
    session_id: String,
}

impl Request {
    /// The request as the JSON value `replayRequests` returns.
    ///
    /// The members are the header's `config` members, then `messages`,
    /// `toolHistory`, `tools` when the header declares tools, and `sessionId`.
    /// With one request header, the tool history is that header's tools and
    /// no updates. Equal values do not imply equal provider wire bytes or
    /// member order.
    pub fn to_json(&self) -> Value {
        let tools = self.tools.clone();
        let mut request = self.config.clone();
        request.insert(
            "messages".to_owned(),
            Value::Array(self.messages.iter().cloned().map(Value::Object).collect()),
        );
        let mut history = Map::new();
        history.insert(
            "tools".to_owned(),
            Value::Array(tools.clone().unwrap_or_default()),
        );
        history.insert("updates".to_owned(), Value::Array(Vec::new()));
        request.insert("toolHistory".to_owned(), Value::Object(history));
        if let Some(tools) = tools {
            request.insert("tools".to_owned(), Value::Array(tools));
        }
        request.insert(
            "sessionId".to_owned(),
            Value::String(self.session_id.clone()),
        );
        Value::Object(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(fields) => fields,
            _ => panic!("object literal"),
        }
    }

    fn header() -> Fact {
        Fact::Header {
            config: object(json!({"provider": "p", "model": "m"})),
            tools: None,
        }
    }

    fn message(id: &str, content: Value) -> Map<String, Value> {
        object(json!({"id": id, "content": content}))
    }

    fn surface(seq: u64, kind: SurfaceKind, op: SurfaceOp, text: &str) -> Fact {
        let content = if text.is_empty() {
            json!([])
        } else {
            json!([{"type": "text", "text": text}])
        };
        let payload = match kind {
            SurfaceKind::ToolResult => object(json!({
                "turn": 1,
                // A rewrite keeps the result's message identity.
                "message": {
                    "id": "tool",
                    "content": [{"type": "tool-result", "toolCallId": "c", "content": content}],
                },
            })),
            _ => message(&format!("m{seq}"), content),
        };
        Fact::Surface {
            seq,
            kind,
            op,
            payload,
        }
    }

    fn replace(start: u64, end: u64, sources: &[u64]) -> SurfaceOp {
        SurfaceOp::Replace {
            start,
            end,
            sources: sources.to_vec(),
        }
    }

    /// A header, then a system head, user, assistant, and tool result at seqs 1-4.
    fn conversation() -> RequestFold {
        let mut fold = RequestFold::new("s".to_owned());
        fold.append(header()).expect("header");
        for (seq, kind) in [
            (1, SurfaceKind::System),
            (2, SurfaceKind::User),
            (3, SurfaceKind::Assistant),
            (4, SurfaceKind::ToolResult),
        ] {
            fold.append(surface(seq, kind, SurfaceOp::Append, "text"))
                .expect("append");
        }
        fold
    }

    fn ids(fold: &RequestFold) -> Vec<Value> {
        let request = fold.request().expect("request");
        request
            .messages
            .iter()
            .map(|message| message["id"].clone())
            .collect()
    }

    #[test]
    fn a_refused_header_change_leaves_the_fold_unchanged() {
        let mut fold = conversation();
        let before = fold.clone();
        assert_eq!(fold.append(header()), Err(FoldRefusal::HeaderChange));
        assert_eq!(fold, before);
    }

    #[test]
    fn empty_system_and_assistant_messages_keep_their_nodes_but_derive_nothing() {
        let mut fold = RequestFold::new("s".to_owned());
        fold.append(header()).expect("header");
        for (seq, kind) in [
            (1, SurfaceKind::System),
            (2, SurfaceKind::User),
            (3, SurfaceKind::Assistant),
            (4, SurfaceKind::ToolResult),
        ] {
            fold.append(surface(seq, kind, SurfaceOp::Append, ""))
                .expect("surface");
        }
        assert_eq!(fold.nodes.len(), 4);
        assert_eq!(ids(&fold), [json!("m2"), json!("tool")]);
        // The empty head is still protected; the empty assistant is still a target.
        assert_eq!(
            fold.append(surface(5, SurfaceKind::User, replace(1, 1, &[1]), "x")),
            Err(FoldRefusal::SystemHead)
        );
        fold.append(surface(5, SurfaceKind::User, replace(3, 3, &[3]), "x"))
            .expect("replace the empty assistant");
        assert_eq!(ids(&fold), [json!("m2"), json!("m5"), json!("tool")]);
    }

    #[test]
    fn replacements_locate_endpoints_by_current_position() {
        let mut fold = conversation();
        fold.append(surface(
            5,
            SurfaceKind::User,
            replace(2, 3, &[2, 3]),
            "summary",
        ))
        .expect("shadow user and assistant");
        assert_eq!(ids(&fold), [json!("m1"), json!("m5"), json!("tool")]);
        // Seq 5 now precedes seq 4, so a range may open at the larger seq.
        fold.append(surface(
            6,
            SurfaceKind::User,
            replace(5, 4, &[4, 5]),
            "again",
        ))
        .expect("nested replacement");
        assert_eq!(ids(&fold), [json!("m1"), json!("m6")]);
        // Shadowed seqs are no longer current endpoints.
        assert_eq!(
            fold.append(surface(7, SurfaceKind::User, replace(2, 6, &[2, 6]), "x")),
            Err(FoldRefusal::ReplaceStart)
        );
    }

    #[test]
    fn every_replacement_refusal_leaves_the_fold_unchanged() {
        let mut changed_rest = surface(5, SurfaceKind::ToolResult, replace(4, 4, &[4]), "pruned");
        if let Fact::Surface { payload, .. } = &mut changed_rest {
            payload.insert("turn".to_owned(), json!(2));
        }
        let refusals = [
            (
                surface(5, SurfaceKind::User, replace(0, 2, &[2]), "x"),
                FoldRefusal::ReplaceStart,
            ),
            (
                surface(5, SurfaceKind::User, replace(2, 9, &[2]), "x"),
                FoldRefusal::ReplaceEnd,
            ),
            (
                surface(5, SurfaceKind::User, replace(3, 2, &[2, 3]), "x"),
                FoldRefusal::ReplaceOrder,
            ),
            (
                surface(5, SurfaceKind::User, replace(2, 3, &[2]), "x"),
                FoldRefusal::ReplaceSources,
            ),
            (
                surface(5, SurfaceKind::ToolResult, replace(3, 4, &[3, 4]), "x"),
                FoldRefusal::ToolResultSpan,
            ),
            (
                surface(5, SurfaceKind::ToolResult, replace(2, 2, &[2]), "x"),
                FoldRefusal::ToolResultTarget,
            ),
            (changed_rest, FoldRefusal::ToolResultRest),
            (
                surface(5, SurfaceKind::System, replace(1, 2, &[1, 2]), "x"),
                FoldRefusal::SystemHead,
            ),
        ];
        for (fact, refusal) in refusals {
            let mut fold = conversation();
            let before = fold.clone();
            assert_eq!(fold.append(fact), Err(refusal));
            assert_eq!(fold, before, "{refusal:?}");
        }
    }

    #[test]
    fn a_tool_result_rewrite_may_change_only_its_result_content() {
        let mut fold = conversation();
        fold.append(surface(
            5,
            SurfaceKind::ToolResult,
            replace(4, 4, &[4]),
            "pruned",
        ))
        .expect("content rewrite");
        let request = fold.request().expect("request");
        assert_eq!(request.messages.len(), 4);
        assert_eq!(
            request.messages[3]["content"][0]["content"][0]["text"],
            "pruned"
        );
    }

    #[test]
    fn no_request_before_a_header() {
        let mut fold = RequestFold::new("s".to_owned());
        fold.append(Fact::LogOnly).expect("log-only");
        assert_eq!(fold.request(), None);
    }
}
