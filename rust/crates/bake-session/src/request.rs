//! Development-only request derivation over a closed subset of current-format
//! events.
//!
//! [`RequestFold`] folds one event at a time into the state a model request
//! needs: the current surface nodes, the latest request header, the tool
//! history, and the Session id. It reads no codec types or diagnostics;
//! [`crate::replay`] admits events and converts them to [`Fact`]s first. The
//! fold owns its node payloads and tool schemas so each request snapshot
//! outlives the parsed rows it came from.
//!
//! Every fold transition matches `packages/core/session` for the subset the
//! import side admits: surface appends, positional surface replacements,
//! request headers, and tool updates. A replacement is planned against the
//! current nodes and refused, leaving the fold unchanged, unless it passes the
//! replacement checks of `planSurfaceEvent` in
//! `packages/core/session/src/surface.ts` that remain for this subset:
//! endpoints located among the current nodes, source coverage of every
//! shadowed node, the `tool/result` rewrite rule, and the protected system
//! head. The codec already proved sequence contiguity and the event's own
//! surface metadata. A tool update is likewise refused unless it passes
//! `validateToolUpdate` in `packages/core/session/src/tool-history.ts` after
//! its ignorable check, in that function's order. Headers and accepted updates
//! then fold as `ToolHistoryProjection` does, which cannot fail on a validated
//! prefix. Message projections are refused before they reach the fold.
//!
//! Besides request snapshots, the fold exposes the projections a restored
//! Session reads: its messages, which need no header, the latest header in
//! `canonicalHeader` form, and the tool-history snapshot.

use std::collections::{BTreeMap, BTreeSet};

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
    /// A `request/header` in canonical form: `config` and `adapterDefaults`
    /// are copied verbatim and `tools` is absent or a non-empty list of
    /// objects, in logged order. `resets` is set for `reason: "series"` or
    /// `startsSeries: true`.
    Header {
        seq: u64,
        config: Map<String, Value>,
        adapter_defaults: Option<Map<String, Value>>,
        tools: Option<Vec<Value>>,
        resets: bool,
    },
    /// A `request/tool-update` whose data `validateToolUpdateData` accepts.
    ToolUpdate {
        seq: u64,
        header_seq: u64,
        after_message_id: String,
        additions: Vec<String>,
        removals: Vec<String>,
    },
    /// An event derivation does not read.
    LogOnly,
}

/// Why the fold refused a fact. The fold is unchanged after a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FoldRefusal {
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
    /// A tool update's `headerSeq` is not an earlier `request/header`.
    ToolUpdateHeader,
    /// A header or tool update sits between the referenced header and the
    /// tool update.
    ToolUpdateStale,
    /// No `request/header` precedes the referenced one.
    ToolUpdateBaseline,
    /// The additions or removals differ from the referenced header's change.
    ToolUpdateChange,
    /// The last current non-system message is not the user or tool-result
    /// message the update names.
    ToolUpdateAnchor,
}

/// A tool `name` as a JavaScript `Map` or `Set` key. Session construction
/// detaches every event into fresh values, so an object or array name equals
/// only itself, and its one tree occurrence is the header seq and tool index.
/// Numbers are already qualified to safe integers other than -0.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum NameKey {
    Absent,
    Null,
    Bool(bool),
    Number(i64),
    String(String),
    Occurrence(u64, usize),
}

impl NameKey {
    fn of(header_seq: u64, index: usize, tool: &Value) -> Self {
        match tool.get("name") {
            None => Self::Absent,
            Some(Value::Null) => Self::Null,
            Some(Value::Bool(flag)) => Self::Bool(*flag),
            Some(Value::Number(number)) => {
                Self::Number(number.as_i64().expect("qualified safe integer"))
            }
            Some(Value::String(name)) => Self::String(name.clone()),
            Some(Value::Array(_) | Value::Object(_)) => Self::Occurrence(header_seq, index),
        }
    }

    /// Whether `JSON.stringify` writes this name as it writes `name`.
    fn is_string(&self, name: &str) -> bool {
        matches!(self, Self::String(own) if own == name)
    }
}

/// The latest `request/header`.
#[derive(Debug, Clone, PartialEq)]
struct Header {
    seq: u64,
    config: Map<String, Value>,
    adapter_defaults: Option<Map<String, Value>>,
    tools: Option<Vec<Value>>,
    /// Each tool's name key, in tool order.
    names: Vec<NameKey>,
}

impl Header {
    /// `header.tools ?? []` with each tool's name key.
    fn tools(&self) -> impl Iterator<Item = (&NameKey, &Value)> {
        self.names.iter().zip(self.tools.iter().flatten())
    }
}

/// One `ToolHistory` update.
#[derive(Debug, Clone, PartialEq)]
struct Update {
    after_message_id: String,
    additions: Vec<Value>,
    removals: Vec<String>,
}

/// `ToolHistoryProjection`'s state outside its active header, which the fold
/// keeps as [`RequestFold::header`].
#[derive(Debug, Clone, Default, PartialEq)]
struct ToolHistory {
    baseline: Option<u64>,
    declared: BTreeMap<NameKey, Value>,
    available: BTreeSet<NameKey>,
    tools: Vec<Value>,
    updates: Vec<Update>,
}

impl ToolHistory {
    /// Whether the active declarations differ from the available names.
    fn incomplete(&self, active: Option<&Header>) -> bool {
        let names = active.map_or(&[][..], |header| &header.names);
        names.len() != self.available.len()
            || names.iter().any(|name| !self.available.contains(name))
    }
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
    /// The latest `request/header`, which is also the projection's active one.
    header: Option<Header>,
    /// Name keys of the header before [`Self::header`], when there is one.
    previous_names: Option<Vec<NameKey>>,
    header_seqs: BTreeSet<u64>,
    last_update: Option<u64>,
    history: ToolHistory,
}

impl RequestFold {
    pub(crate) fn new(session_id: String) -> Self {
        Self {
            session_id,
            nodes: Vec::new(),
            header: None,
            previous_names: None,
            header_seqs: BTreeSet::new(),
            last_update: None,
            history: ToolHistory::default(),
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
            Fact::Header {
                seq,
                config,
                adapter_defaults,
                tools,
                resets,
            } => {
                let names = tools
                    .iter()
                    .flatten()
                    .enumerate()
                    .map(|(index, tool)| NameKey::of(seq, index, tool))
                    .collect();
                let header = Header {
                    seq,
                    config,
                    adapter_defaults,
                    tools,
                    names,
                };
                self.apply_header(&header, resets);
                self.previous_names = self.header.take().map(|previous| previous.names);
                self.header = Some(header);
                self.header_seqs.insert(seq);
            }
            Fact::ToolUpdate {
                seq,
                header_seq,
                after_message_id,
                additions,
                removals,
            } => {
                self.check_tool_update(seq, header_seq, &after_message_id, &additions, &removals)?;
                self.last_update = Some(seq);
                if self.history.baseline != Some(header_seq) {
                    self.apply_tool_update(after_message_id, &additions, removals);
                }
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

    /// `validateToolUpdate` after its ignorable check, which the import side
    /// runs first. Only header seqs, the latest two headers, the last update,
    /// and the current nodes decide it.
    fn check_tool_update(
        &self,
        seq: u64,
        header_seq: u64,
        after_message_id: &str,
        additions: &[String],
        removals: &[String],
    ) -> Result<(), FoldRefusal> {
        if header_seq >= seq || !self.header_seqs.contains(&header_seq) {
            return Err(FoldRefusal::ToolUpdateHeader);
        }
        // A header after `header_seq` would be the latest one, and every
        // update after the latest header sits between the two.
        let header = self.header.as_ref().expect("a header seq was recorded");
        if header.seq != header_seq || self.last_update.is_some_and(|update| update > header_seq) {
            return Err(FoldRefusal::ToolUpdateStale);
        }
        let Some(previous) = &self.previous_names else {
            return Err(FoldRefusal::ToolUpdateBaseline);
        };
        let before: BTreeSet<&NameKey> = previous.iter().collect();
        let after: BTreeSet<&NameKey> = header.names.iter().collect();
        // `JSON.stringify` compares the lists: order and repeats count, and
        // only a string name can match a logged string.
        let same = |expected: Vec<&NameKey>, logged: &[String]| {
            expected.len() == logged.len()
                && expected
                    .iter()
                    .zip(logged)
                    .all(|(name, text)| name.is_string(text))
        };
        let expected_additions = header.names.iter().filter(|name| !before.contains(name));
        let expected_removals = previous.iter().filter(|name| !after.contains(name));
        if !same(expected_additions.collect(), additions)
            || !same(expected_removals.collect(), removals)
        {
            return Err(FoldRefusal::ToolUpdateChange);
        }
        // The last node that derives a non-system message must be the named
        // user or tool-result message.
        let last = self
            .nodes
            .iter()
            .rev()
            .filter(|node| node.kind != SurfaceKind::System)
            .find_map(|node| node.derived().map(|message| (node.kind, message)));
        match last {
            Some((SurfaceKind::User | SurfaceKind::ToolResult, message))
                if message.get("id").and_then(Value::as_str) == Some(after_message_id) =>
            {
                Ok(())
            }
            _ => Err(FoldRefusal::ToolUpdateAnchor),
        }
    }

    /// `ToolHistoryProjection.apply` for a header, before it becomes active.
    fn apply_header(&mut self, header: &Header, resets: bool) {
        let history = &self.history;
        let redeclared = header.tools().any(|(name, tool)| {
            history
                .declared
                .get(name)
                .is_some_and(|before| !same_js_text(before, tool))
        });
        let incomplete = history.incomplete(self.header.as_ref());
        if history.baseline.is_none() || resets || redeclared || incomplete {
            let mut declared = BTreeMap::new();
            for (name, tool) in header.tools() {
                declared.insert(name.clone(), tool.clone());
            }
            self.history = ToolHistory {
                baseline: Some(header.seq),
                declared,
                available: header.names.iter().cloned().collect(),
                tools: header.tools.clone().unwrap_or_default(),
                updates: Vec::new(),
            };
        }
    }

    /// `ToolHistoryProjection.apply` for a validated update that does not
    /// reference the baseline header.
    fn apply_tool_update(
        &mut self,
        after_message_id: String,
        additions: &[String],
        removals: Vec<String>,
    ) {
        let header = self.header.as_ref().expect("validated against a header");
        let additions = additions
            .iter()
            .map(|name| {
                let (key, tool) = header
                    .tools()
                    .find(|(key, _)| key.is_string(name))
                    .expect("validated additions name active tools");
                self.history.declared.insert(key.clone(), tool.clone());
                self.history.available.insert(key.clone());
                tool.clone()
            })
            .collect();
        for name in &removals {
            self.history
                .available
                .remove(&NameKey::String(name.clone()));
        }
        self.history.updates.push(Update {
            after_message_id,
            additions,
            removals,
        });
    }

    /// `ToolHistoryProjection.snapshot`: when updates are missing, as in a
    /// historical log or a crash tail, the active declarations alone.
    fn tool_history(&self) -> (Vec<Value>, Vec<Update>) {
        if self.history.incomplete(self.header.as_ref()) {
            let tools = self.header.as_ref().and_then(|header| header.tools.clone());
            return (tools.unwrap_or_default(), Vec::new());
        }
        (self.history.tools.clone(), self.history.updates.clone())
    }

    /// The request a step would send after this prefix, or `None` before the
    /// first `request/header`.
    pub(crate) fn request(&self) -> Option<Request> {
        let header = self.header.as_ref()?;
        let (history_tools, updates) = self.tool_history();
        Some(Request {
            config: header.config.clone(),
            messages: self.messages().cloned().collect(),
            history_tools,
            updates,
            tools: header.tools.clone(),
            session_id: self.session_id.clone(),
        })
    }

    /// `deriveMessages`: each current node's derived message, in order.
    pub(crate) fn messages(&self) -> impl Iterator<Item = &Map<String, Value>> {
        self.nodes.iter().filter_map(Node::derived)
    }

    /// `foldRequestHeader`: the latest header's `canonicalHeader` form, or
    /// `None` before the first. `adapterDefaults` stays when it marks
    /// `reasoningEffort` or `maxTokens`, and `tools` when present, which the
    /// codec allows only non-empty.
    pub(crate) fn request_header(&self) -> Option<Value> {
        let header = self.header.as_ref()?;
        let mut canonical = Map::new();
        canonical.insert("config".to_owned(), Value::Object(header.config.clone()));
        if let Some(defaults) = header.adapter_defaults.as_ref().filter(|defaults| {
            ["reasoningEffort", "maxTokens"]
                .iter()
                .any(|key| defaults.get(*key) == Some(&Value::Bool(true)))
        }) {
            canonical.insert(
                "adapterDefaults".to_owned(),
                Value::Object(defaults.clone()),
            );
        }
        if let Some(tools) = &header.tools {
            canonical.insert("tools".to_owned(), Value::Array(tools.clone()));
        }
        Some(Value::Object(canonical))
    }

    /// `ToolHistoryProjection.snapshot` as JSON.
    pub(crate) fn tool_history_json(&self) -> Value {
        let (tools, updates) = self.tool_history();
        history_json(&tools, &updates)
    }
}

/// A `ToolHistory` value: the baseline declarations and ordered updates.
fn history_json(tools: &[Value], updates: &[Update]) -> Value {
    let updates = updates.iter().map(|update| {
        let mut fields = Map::new();
        fields.insert(
            "afterMessageId".to_owned(),
            Value::String(update.after_message_id.clone()),
        );
        fields.insert(
            "additions".to_owned(),
            Value::Array(update.additions.clone()),
        );
        let removals = update.removals.iter().cloned().map(Value::String).collect();
        fields.insert("removals".to_owned(), Value::Array(removals));
        Value::Object(fields)
    });
    let mut history = Map::new();
    history.insert("tools".to_owned(), Value::Array(tools.to_vec()));
    history.insert("updates".to_owned(), Value::Array(updates.collect()));
    Value::Object(history)
}

/// Whether `JSON.stringify` writes the same text for two values the import
/// side qualified: safe-integer numbers and bounded depth. JavaScript
/// enumerates an object's array-index keys first, in ascending numeric order,
/// then its other keys in insertion order, which a parsed [`Map`] keeps.
fn same_js_text(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_js_text(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && js_members(a)
                    .into_iter()
                    .zip(js_members(b))
                    .all(|((ka, va), (kb, vb))| ka == kb && same_js_text(va, vb))
        }
        _ => a == b,
    }
}

/// An object's members in JavaScript enumeration order.
fn js_members(fields: &Map<String, Value>) -> Vec<(&String, &Value)> {
    let mut indices: Vec<(u32, (&String, &Value))> = Vec::new();
    let mut others = Vec::new();
    for member in fields {
        match array_index(member.0) {
            Some(index) => indices.push((index, member)),
            None => others.push(member),
        }
    }
    indices.sort_unstable_by_key(|(index, _)| *index);
    indices
        .into_iter()
        .map(|(_, member)| member)
        .chain(others)
        .collect()
}

/// A key that is an ECMAScript array index: the canonical decimal form of an
/// integer below 2^32 − 1.
fn array_index(key: &str) -> Option<u32> {
    let canonical = key == "0"
        || (!key.is_empty() && !key.starts_with('0') && key.bytes().all(|b| b.is_ascii_digit()));
    key.parse::<u32>()
        .ok()
        .filter(|index| canonical && *index != u32::MAX)
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
    history_tools: Vec<Value>,
    updates: Vec<Update>,
    tools: Option<Vec<Value>>,
    session_id: String,
}

impl Request {
    /// The request as the JSON value `replayRequests` returns.
    ///
    /// The members are the latest header's `config` members, then `messages`,
    /// `toolHistory`, `tools` when that header declares tools, and
    /// `sessionId`. The tool history is the `ToolHistoryProjection` snapshot
    /// of the prefix. Equal values do not imply equal provider wire bytes or
    /// member order.
    pub fn to_json(&self) -> Value {
        let mut request = self.config.clone();
        request.insert(
            "messages".to_owned(),
            Value::Array(self.messages.iter().cloned().map(Value::Object).collect()),
        );
        request.insert(
            "toolHistory".to_owned(),
            history_json(&self.history_tools, &self.updates),
        );
        if let Some(tools) = &self.tools {
            request.insert("tools".to_owned(), Value::Array(tools.clone()));
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
        header_with(0, &[], false)
    }

    fn header_with(seq: u64, tools: &[Value], resets: bool) -> Fact {
        Fact::Header {
            seq,
            config: object(json!({"provider": "p", "model": "m"})),
            adapter_defaults: None,
            tools: (!tools.is_empty()).then(|| tools.to_vec()),
            resets,
        }
    }

    fn tool(name: &str) -> Value {
        json!({"name": name, "parameters": {"type": "object"}})
    }

    fn update(
        seq: u64,
        header_seq: u64,
        after: &str,
        additions: &[&str],
        removals: &[&str],
    ) -> Fact {
        let names = |list: &[&str]| list.iter().map(|name| (*name).to_owned()).collect();
        Fact::ToolUpdate {
            seq,
            header_seq,
            after_message_id: after.to_owned(),
            additions: names(additions),
            removals: names(removals),
        }
    }

    fn tool_history(fold: &RequestFold) -> Value {
        fold.request().expect("request").to_json()["toolHistory"].clone()
    }

    /// Header `[a]` at 0, user `m1` at 1, header `[a, b]` at 2.
    fn changed_tools() -> RequestFold {
        let mut fold = RequestFold::new("s".to_owned());
        fold.append(header_with(0, &[tool("a")], false))
            .expect("header");
        fold.append(surface(1, SurfaceKind::User, SurfaceOp::Append, "go"))
            .expect("user");
        fold.append(header_with(2, &[tool("a"), tool("b")], false))
            .expect("change");
        fold
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

    /// `Value` equality ignores member order, which later redeclaration
    /// checks read, so the full `Debug` text, in member order, must match too.
    fn assert_unchanged(fold: &RequestFold, before: &RequestFold, refusal: FoldRefusal) {
        assert_eq!(fold, before, "{refusal:?}");
        assert_eq!(format!("{fold:?}"), format!("{before:?}"), "{refusal:?}");
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
    fn a_header_change_waits_for_its_update() {
        let mut fold = changed_tools();
        // The added tool is not yet available, so the active header alone.
        assert_eq!(
            tool_history(&fold),
            json!({"tools": [tool("a"), tool("b")], "updates": []})
        );
        fold.append(update(3, 2, "m1", &["b"], &[]))
            .expect("update");
        assert_eq!(
            tool_history(&fold),
            json!({
                "tools": [tool("a")],
                "updates": [{"afterMessageId": "m1", "additions": [tool("b")], "removals": []}],
            })
        );
        // An unchanged header keeps the history; a series header resets it.
        fold.append(header_with(4, &[tool("a"), tool("b")], false))
            .expect("resume");
        assert_eq!(
            tool_history(&fold)["updates"].as_array().map(Vec::len),
            Some(1)
        );
        fold.append(header_with(5, &[tool("a"), tool("b")], true))
            .expect("series");
        assert_eq!(
            tool_history(&fold),
            json!({"tools": [tool("a"), tool("b")], "updates": []})
        );
    }

    #[test]
    fn a_reordered_schema_redeclares_but_index_keys_do_not() {
        let first = json!({"name": "a", "1": 0, "x": 1, "y": 2});
        let index_order = json!({"name": "a", "x": 1, "y": 2, "1": 0});
        let reordered = json!({"name": "a", "1": 0, "y": 2, "x": 1});
        assert!(same_js_text(&first, &index_order));
        assert!(!same_js_text(&first, &reordered));
        assert_eq!(first, reordered);
        for (schema, reset) in [(index_order, false), (reordered, true)] {
            let mut fold = RequestFold::new("s".to_owned());
            fold.append(header_with(0, std::slice::from_ref(&first), false))
                .expect("header");
            fold.append(surface(1, SurfaceKind::User, SurfaceOp::Append, "go"))
                .expect("user");
            fold.append(header_with(2, &[first.clone(), tool("b")], false))
                .expect("change");
            fold.append(update(3, 2, "m1", &["b"], &[]))
                .expect("update");
            fold.append(header_with(4, &[schema, tool("b")], false))
                .expect("redeclare");
            assert_eq!(fold.history.baseline, Some(if reset { 4 } else { 0 }));
        }
    }

    #[test]
    fn array_indices_are_canonical_and_below_the_maximum() {
        for (key, index) in [
            ("0", Some(0)),
            ("10", Some(10)),
            ("4294967294", Some(4_294_967_294)),
            ("4294967295", None),
            ("01", None),
            ("-1", None),
            ("+1", None),
            ("", None),
            ("1.0", None),
        ] {
            assert_eq!(array_index(key), index, "{key}");
        }
    }

    #[test]
    fn every_tool_update_refusal_leaves_the_fold_unchanged() {
        let mut used = changed_tools();
        used.append(update(3, 2, "m1", &["b"], &[]))
            .expect("update");
        let mut single = RequestFold::new("s".to_owned());
        single
            .append(header_with(0, &[tool("a")], false))
            .expect("header");
        single
            .append(surface(1, SurfaceKind::User, SurfaceOp::Append, "go"))
            .expect("user");
        let refusals = [
            (
                changed_tools(),
                update(3, 3, "m1", &["b"], &[]),
                FoldRefusal::ToolUpdateHeader,
            ),
            (
                changed_tools(),
                update(3, 1, "m1", &["b"], &[]),
                FoldRefusal::ToolUpdateHeader,
            ),
            (
                changed_tools(),
                update(3, 0, "m1", &["b"], &[]),
                FoldRefusal::ToolUpdateStale,
            ),
            (
                used,
                update(4, 2, "m1", &["b"], &[]),
                FoldRefusal::ToolUpdateStale,
            ),
            (
                single,
                update(2, 0, "m1", &["a"], &[]),
                FoldRefusal::ToolUpdateBaseline,
            ),
            (
                changed_tools(),
                update(3, 2, "m1", &["b"], &["a"]),
                FoldRefusal::ToolUpdateChange,
            ),
            (
                changed_tools(),
                update(3, 2, "m9", &["b"], &[]),
                FoldRefusal::ToolUpdateAnchor,
            ),
        ];
        for (mut fold, fact, refusal) in refusals {
            let before = fold.clone();
            assert_eq!(fold.append(fact), Err(refusal));
            assert_unchanged(&fold, &before, refusal);
        }
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
            assert_unchanged(&fold, &before, refusal);
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

    #[test]
    fn messages_and_tool_history_need_no_header() {
        let mut fold = RequestFold::new("s".to_owned());
        fold.append(surface(0, SurfaceKind::User, SurfaceOp::Append, "go"))
            .expect("user");
        assert_eq!(fold.request(), None);
        assert_eq!(fold.request_header(), None);
        assert_eq!(fold.messages().count(), 1);
        assert_eq!(
            fold.tool_history_json(),
            json!({"tools": [], "updates": []})
        );
    }

    #[test]
    fn the_header_keeps_its_canonical_members() {
        let mut fold = RequestFold::new("s".to_owned());
        fold.append(Fact::Header {
            seq: 0,
            config: object(json!({"provider": "p", "model": "m", "maxTokens": 8})),
            adapter_defaults: Some(object(json!({"maxTokens": true}))),
            tools: Some(vec![tool("a")]),
            resets: false,
        })
        .expect("header");
        assert_eq!(
            fold.request_header(),
            Some(json!({
                "config": {"provider": "p", "model": "m", "maxTokens": 8},
                "adapterDefaults": {"maxTokens": true},
                "tools": [tool("a")],
            }))
        );
    }
}
