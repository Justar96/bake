//! `assertReleasedArtifactRelationships` from
//! `packages/session/session-format-v0-to-v1/src/relationships.ts`: the
//! cross-event checks a released Session must pass before a current Session
//! is built from it. The v1→v2 and v2→v3 final checks reuse it with the
//! extensions their formats add.
//!
//! [`check_released_relationships`] walks the events in order, as TypeScript
//! does, and keeps the same state: the open turn and step, the next turn and
//! step numbers, the request header's provider, the model-visible surface,
//! each advertised tool call's lifecycle, the PTC dispatch tree, the retry
//! chains and their started attempts, the command ids, and the open
//! compaction. It skips every event type without a released v0 disposition,
//! inherited `Object.prototype` names included, unless an extension names it
//! a step event.
//!
//! # Precondition
//!
//! TypeScript runs this check only after the artifact's coordinates and
//! payloads were validated, and casts the members it reads. The function
//! claims TypeScript's outcome only for input where every event is an object
//! with a string `type` and a `seq` equal to its index, every event whose
//! type is an own released v0 type passes
//! `assertReleasedEventPayload(event, 1)`, and every surface event's
//! `surfaceOp`, when present, is `"append"` or an object. Where it notices
//! input outside that precondition and the outcome would depend on a cast,
//! it reports [`RelationshipLimit::Precondition`]; elsewhere it still
//! decides what the TypeScript code decides. It does not validate the
//! precondition.
//!
//! JavaScript `===` is reproduced on JSON values: numbers compare as doubles,
//! so `1` equals `1.0`, and serde_json's `float_roundtrip` parse gives the
//! double `JSON.parse` gives. Two objects or arrays compare by reference in
//! JavaScript; on admitted input no such pair is compared, so one is the
//! precondition limit. Nothing is read from or written to a file.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use crate::MAX_SAFE_INTEGER;
use crate::v0_to_v1::has_released_v0_disposition;
use crate::v2_to_v3::quote;

type Record = Map<String, Value>;

/// `SURFACE_TYPES` in `relationships.ts`.
const SURFACE_TYPES: [&str; 3] = ["user/message", "assistant/message", "tool/result"];
const TOOL_NOT_STARTED_TEXT: &str = "The tool call was interrupted before the Harness recorded it as started. Retry it if it is still needed.";
const TITLE_REQUEST_PREFIX: &str =
    "Generate the session title from this JSON array of human messages:\n";

/// `ReleasedRelationshipExtensions`: roles a later format adds while reusing
/// the released check. The default is TypeScript's `{}`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelationshipExtensions {
    /// `stepEvents`: event types that must occur inside the open step.
    pub step_events: Vec<String>,
    /// `preservedSourceTitleRequestText`: the title request's framed text is
    /// not compared with its cited messages.
    pub preserved_source_title_request_text: bool,
    /// `legacyInterruptedTurnRestart`: admit the released resume pattern
    /// whose next-turn inbox insert omitted the prior `turn/end`.
    pub legacy_interrupted_turn_restart: bool,
}

/// Why the relationships of a Session were not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelationshipRefusal {
    /// TypeScript throws `SessionFormatError` with exactly `message` while
    /// checking the event at `seq`.
    Rejected { seq: u64, message: String },
    /// This crate does not decide the TypeScript outcome at the event at
    /// `seq`; nothing is claimed about any event. The envelope check runs
    /// over every event first, so an earlier event may be one TypeScript
    /// rejects.
    NativeSubset { seq: u64, limit: RelationshipLimit },
}

/// Input whose TypeScript outcome this port does not reproduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationshipLimit {
    /// The input is outside the documented precondition, and TypeScript's
    /// outcome depends on a cast: a member of the wrong kind that TypeScript
    /// coerces, prints, or dereferences, or two objects or arrays that
    /// `===` compares by reference.
    Precondition,
    /// `deepEqualJson` reads a PTC argument member `__proto__` holding an
    /// empty object, missing from the other object, through the `in`
    /// operator, which finds the inherited `Object.prototype`, and no other
    /// member decides the comparison.
    PrototypeMember,
}

impl RelationshipLimit {
    /// The limit's name in `conformance/session/relationships-cases.json`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Precondition => "precondition",
            Self::PrototypeMember => "prototype-member",
        }
    }
}

enum Stop {
    Rejected(String),
    Limit(RelationshipLimit),
}

type Checked<T = ()> = Result<T, Stop>;

fn reject<T>(message: String) -> Checked<T> {
    Err(Stop::Rejected(message))
}

fn precondition<T>() -> Checked<T> {
    Err(Stop::Limit(RelationshipLimit::Precondition))
}

/// Check the relationships of a released Session's events, as
/// `assertReleasedArtifactRelationships({header, inheritedEventCount,
/// events}, extensions)` does.
///
/// `header` is read only for a delivery marker: its `version`, `id`, and
/// whether it has a `parentSession`. The events must meet the module's
/// precondition; the result claims nothing about input outside it.
pub fn check_released_relationships(
    header: &Value,
    inherited_event_count: u64,
    events: &[Value],
    extensions: &RelationshipExtensions,
) -> Result<(), RelationshipRefusal> {
    let seq_of = |index: usize| u64::try_from(index).unwrap_or(u64::MAX);
    let mut types = Vec::with_capacity(events.len());
    for (index, event) in events.iter().enumerate() {
        let envelope = event.as_object().and_then(|fields| {
            let event_type = fields.get("type")?.as_str()?;
            let seq = fields.get("seq").and_then(count_index)?;
            (usize::try_from(seq).ok() == Some(index)).then_some((fields, event_type))
        });
        let Some(envelope) = envelope else {
            return Err(RelationshipRefusal::NativeSubset {
                seq: seq_of(index),
                limit: RelationshipLimit::Precondition,
            });
        };
        types.push(envelope);
    }
    let mut check = Check::new(header, inherited_event_count, &types, extensions);
    for (index, (event, event_type)) in types.iter().enumerate() {
        check
            .event(index, event, event_type)
            .map_err(|stop| match stop {
                Stop::Rejected(message) => RelationshipRefusal::Rejected {
                    seq: seq_of(index),
                    message,
                },
                Stop::Limit(limit) => RelationshipRefusal::NativeSubset {
                    seq: seq_of(index),
                    limit,
                },
            })?;
    }
    Ok(())
}

/// `inheritedOrphanCompactionStarts`: the seqs of compactions still open at
/// a later `session/end-seed`.
fn inherited_orphan_compaction_starts(events: &[(&Record, &str)]) -> HashSet<usize> {
    let mut stale = HashSet::new();
    let mut open = None;
    for (index, (_, event_type)) in events.iter().enumerate() {
        match *event_type {
            "compaction/start" => open = Some(index),
            "compaction/end" => open = None,
            "session/end-seed" => {
                if let Some(start) = open.take() {
                    stale.insert(start);
                }
            }
            _ => {}
        }
    }
    stale
}

/// `CompactionState`.
struct Compaction<'a> {
    id: Option<&'a Value>,
    source_command_id: Option<&'a Value>,
    turn: Option<&'a Value>,
    start: usize,
    summarized: bool,
}

/// `ToolLifecycle`; `started` is the `'started'` state.
struct ToolLifecycle<'a> {
    name: Option<&'a Value>,
    arguments: Option<&'a Value>,
    started: bool,
}

/// `PtcStart`.
struct PtcStart<'a> {
    root: &'a str,
    parent: &'a str,
    name: Option<&'a Value>,
    arguments: Option<&'a Value>,
    settled: bool,
}

struct Check<'a> {
    header: &'a Value,
    inherited_event_count: u64,
    events: &'a [(&'a Record, &'a str)],
    extensions: &'a RelationshipExtensions,
    /// `openTurn`. Only a `turn/start` whose turn equals `nextTurn` opens
    /// one, so it is always that number.
    open_turn: Option<u64>,
    /// `openStep`, likewise always the `nextStep` it matched.
    open_step: Option<u64>,
    open_step_provider: Option<&'a Value>,
    next_turn: u64,
    next_step: u64,
    /// The seqs of the model-visible surface, in order.
    surface: Vec<u64>,
    open_compaction: Option<Compaction<'a>>,
    stale_compaction_starts: HashSet<usize>,
    /// Payloads of the `llm/retry` events so far.
    retries: Vec<&'a Record>,
    retry_starts: HashSet<(String, String)>,
    ptc_roots: HashMap<&'a str, &'a str>,
    ptc_starts: HashMap<&'a str, PtcStart<'a>>,
    /// `toolLifecycles` in `Map` insertion order.
    tool_lifecycles: Vec<(&'a str, ToolLifecycle<'a>)>,
    command_runs: HashSet<&'a str>,
}

impl<'a> Check<'a> {
    fn new(
        header: &'a Value,
        inherited_event_count: u64,
        events: &'a [(&'a Record, &'a str)],
        extensions: &'a RelationshipExtensions,
    ) -> Self {
        Self {
            header,
            inherited_event_count,
            events,
            extensions,
            open_turn: None,
            open_step: None,
            open_step_provider: None,
            next_turn: 1,
            next_step: 1,
            surface: Vec::new(),
            open_compaction: None,
            stale_compaction_starts: inherited_orphan_compaction_starts(events),
            retries: Vec::new(),
            retry_starts: HashSet::new(),
            ptc_roots: HashMap::new(),
            ptc_starts: HashMap::new(),
            tool_lifecycles: Vec::new(),
            command_runs: HashSet::new(),
        }
    }

    fn event(&mut self, index: usize, event: &'a Record, event_type: &'a str) -> Checked {
        let extension_step_event = self
            .extensions
            .step_events
            .iter()
            .any(|step_event| step_event == event_type);
        if !has_released_v0_disposition(event_type) && !extension_step_event {
            return Ok(());
        }
        let data = record(event.get("data"), format!("{event_type} {index} data"))?;
        if SURFACE_TYPES.contains(&event_type) {
            self.apply_surface(index, event, event_type)?;
        }
        if matches!(event_type, "turn/start" | "turn/end")
            && self
                .open_compaction
                .as_ref()
                .is_some_and(|open| !self.stale_compaction_starts.contains(&open.start))
        {
            return reject(format!("{event_type} crosses an open compaction"));
        }
        if extension_step_event {
            return self.require_open_step(event_type, data);
        }
        match event_type {
            "turn/start" => self.turn_start(index, data),
            "turn/end" => self.turn_end(data),
            "step/start" => {
                if !turn_eq(data.get("turn"), self.open_turn)
                    || self.open_step.is_some()
                    || !num_eq(data.get("step"), self.next_step)
                {
                    return reject(format!(
                        "{event_type} does not match the open turn and next step"
                    ));
                }
                self.open_step = Some(self.next_step);
                Ok(())
            }
            "step/end" => {
                self.require_open_step(event_type, data)?;
                self.assert_no_unresolved_tools("step/end")?;
                self.tool_lifecycles.clear();
                self.open_step = None;
                self.next_step += 1;
                Ok(())
            }
            "assistant/chunk" => self.require_open_step(event_type, data),
            "assistant/message" => self.assistant_message(index, data),
            "tool/call" => self.tool_call(data),
            "tool/result" => self.tool_result(index, event, data),
            "request/header" => {
                self.require_open_turn(event_type)?;
                let Some(Value::Object(header)) = data.get("header") else {
                    return precondition();
                };
                let Some(Value::Object(config)) = header.get("config") else {
                    return precondition();
                };
                self.open_step_provider = config.get("provider");
                Ok(())
            }
            "request/context" => self.require_open_turn(event_type),
            "tool/code-dispatch-start" | "tool/code-dispatch" => {
                self.code_dispatch(event_type, data)
            }
            "llm/retry" => self.retry(data),
            "llm/retry-started" => self.retry_started(data),
            "session/title" | "session/title-llm-request" => {
                self.title_sources(index, event_type, data)
            }
            "command/run" => {
                let id = string(data.get("commandId"))?;
                if !self.command_runs.insert(id) {
                    return reject(format!("command/run repeats commandId {id}"));
                }
                Ok(())
            }
            "command/done" => self.command_done(data),
            "session-log-deepseek/delivery-accepted" => self.delivery_accepted(index, data),
            "compaction/start" => {
                if self.open_compaction.is_some() {
                    return reject("compaction/start overlaps an open compaction".to_owned());
                }
                self.assert_compaction_turn(data.get("turn"), event_type)?;
                self.open_compaction = Some(Compaction {
                    id: data.get("compactionId"),
                    source_command_id: data.get("sourceCommandId"),
                    turn: data.get("turn"),
                    start: index,
                    summarized: false,
                });
                Ok(())
            }
            "compaction/summary" => {
                let open = self.compaction_owner(data, event_type)?;
                let (turn, summarized) = (open.turn, open.summarized);
                self.assert_compaction_turn(turn, event_type)?;
                if summarized {
                    return reject("compaction/summary repeats".to_owned());
                }
                self.assert_current_surface_span(data, event_type)?;
                if let Some(open) = self.open_compaction.as_mut() {
                    open.summarized = true;
                }
                Ok(())
            }
            "compaction/end" => {
                let open = self.compaction_owner(data, event_type)?;
                let (turn, summarized) = (open.turn, open.summarized);
                if !strict_eq(data.get("turn"), turn)? {
                    return reject("compaction/end changes its owner turn".to_owned());
                }
                self.assert_compaction_turn(turn, event_type)?;
                if data.get("error").is_none() && !summarized {
                    return reject("successful compaction/end requires one summary".to_owned());
                }
                self.open_compaction = None;
                Ok(())
            }
            "compaction/prune" => self.assert_current_surface_span(data, event_type),
            "user/message" => {
                let source = record(data.get("source"), format!("user/message {index} source"))?;
                if !str_eq(event.get("surfaceOp"), "append")
                    && str_eq(source.get("kind"), "plugin")
                    && str_eq(source.get("plugin"), "compact")
                {
                    self.compaction_owner(
                        source,
                        &format!("compaction checkpoint at seq {index}"),
                    )?;
                }
                Ok(())
            }
            "session/end-seed" => {
                // An unmatched inherited transaction belongs to the ended source lifecycle.
                self.open_compaction = None;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// `applySurface`.
    fn apply_surface(&mut self, index: usize, event: &Record, event_type: &str) -> Checked {
        let seq = u64::try_from(index).unwrap_or(u64::MAX);
        let (start, end) = match event.get("surfaceOp") {
            None => return reject(format!("{event_type} requires a surfaceOp marker")),
            Some(Value::String(operation)) if operation == "append" => {
                self.surface.push(seq);
                return Ok(());
            }
            Some(Value::Null) => return precondition(),
            Some(Value::Object(replace)) => (replace.get("start"), replace.get("end")),
            // Any other value has no `start` or `end` member.
            Some(_) => (None, None),
        };
        let start = index_of(&self.surface, start);
        let end = index_of(&self.surface, end);
        let (Some(start), Some(end)) = (start, end) else {
            return reject(format!(
                "{event_type} replacement range is not on the current surface"
            ));
        };
        if end < start {
            return reject(format!(
                "{event_type} replacement range is not on the current surface"
            ));
        }
        let sources: Vec<f64> = match event.get("sourceEventSeqs") {
            Some(Value::Array(sources)) => sources.iter().filter_map(Value::as_f64).collect(),
            _ => Vec::new(),
        };
        // `Set.has` matches numbers by value, as the double comparison does.
        if self.surface[start..=end]
            .iter()
            .any(|shadowed| !sources.contains(&(*shadowed as f64)))
        {
            return reject(format!(
                "{event_type} replacement sourceEventSeqs omit a shadowed surface node"
            ));
        }
        self.surface.splice(start..=end, [seq]);
        Ok(())
    }

    fn require_open_turn(&self, event_type: &str) -> Checked {
        if self.open_turn.is_none() {
            return reject(format!("{event_type} is outside an open turn"));
        }
        Ok(())
    }

    /// `requireOpenStep`.
    fn require_open_step(&self, event_type: &str, data: &Record) -> Checked {
        if !turn_eq(data.get("turn"), self.open_turn)
            || !turn_eq(data.get("step"), self.open_step)
            || self.open_turn.is_none()
            || self.open_step.is_none()
        {
            return reject(format!("{event_type} does not match an open turn and step"));
        }
        Ok(())
    }

    fn turn_start(&mut self, index: usize, data: &Record) -> Checked {
        let turn = data.get("turn");
        if self.extensions.legacy_interrupted_turn_restart
            && let Some(open_turn) = self.open_turn
            && self.open_step.is_none()
            && num_eq(turn, open_turn + 1)
            && self.next_turn == open_turn
            && let Some(previous) = index.checked_sub(1)
            && let Some((previous_event, "agent/inbox/spliced")) = self.events.get(previous)
        {
            let splice = record(
                previous_event.get("data"),
                format!("agent/inbox/spliced {previous} data"),
            )?;
            if str_eq(splice.get("target"), "next-turn")
                && splice
                    .get("inserted")
                    .and_then(Value::as_array)
                    .is_some_and(|inserted| !inserted.is_empty())
            {
                self.open_turn = None;
                self.next_turn += 1;
            }
        }
        if self.open_turn.is_some() || !num_eq(turn, self.next_turn) {
            return reject(format!(
                "turn/start {} does not open expected turn {}",
                stringify(turn)?,
                self.next_turn
            ));
        }
        self.open_turn = Some(self.next_turn);
        self.open_step = None;
        self.tool_lifecycles.clear();
        self.next_step = 1;
        Ok(())
    }

    fn turn_end(&mut self, data: &Record) -> Checked {
        let turn = data.get("turn");
        if !turn_eq(turn, self.open_turn) {
            return reject(format!(
                "turn/end {} has no matching open turn",
                stringify(turn)?
            ));
        }
        self.assert_no_unresolved_tools("turn/end")?;
        if self.open_step.is_some() {
            return reject(format!(
                "turn/end {} crosses an open step",
                stringify(turn)?
            ));
        }
        self.open_turn = None;
        self.next_turn += 1;
        Ok(())
    }

    /// `assertNoUnresolvedTools`: the first lifecycle in insertion order.
    fn assert_no_unresolved_tools(&self, boundary: &str) -> Checked {
        if let Some((call_id, _)) = self.tool_lifecycles.first() {
            return reject(format!("{boundary} leaves unresolved tool call {call_id}"));
        }
        Ok(())
    }

    fn lifecycle(&mut self, call_id: &str) -> Option<&mut ToolLifecycle<'a>> {
        self.tool_lifecycles
            .iter_mut()
            .find(|(id, _)| *id == call_id)
            .map(|(_, lifecycle)| lifecycle)
    }

    fn assistant_message(&mut self, index: usize, data: &'a Record) -> Checked {
        self.require_open_step("assistant/message", data)?;
        let message = record(
            data.get("message"),
            format!("assistant/message {index} message"),
        )?;
        let Some(Value::Array(content)) = message.get("content") else {
            return precondition();
        };
        for block in content {
            let Value::Object(block) = block else {
                return precondition();
            };
            if !str_eq(block.get("type"), "tool-call") {
                continue;
            }
            let call_id = string(block.get("id"))?;
            if self.lifecycle(call_id).is_some() {
                return reject(format!(
                    "assistant/message repeats advertised tool call {call_id}"
                ));
            }
            self.tool_lifecycles.push((
                call_id,
                ToolLifecycle {
                    name: block.get("name"),
                    arguments: block.get("arguments"),
                    started: false,
                },
            ));
        }
        Ok(())
    }

    fn tool_call(&mut self, data: &'a Record) -> Checked {
        self.require_open_step("tool/call", data)?;
        let call_id = string(data.get("callId"))?;
        let mismatch = || {
            reject(format!(
                "tool/call {call_id} does not match one advertised tool call"
            ))
        };
        let Some(lifecycle) = self.lifecycle(call_id) else {
            return mismatch();
        };
        if lifecycle.started
            || !strict_eq(lifecycle.name, data.get("name"))?
            || !strict_eq(lifecycle.arguments, data.get("arguments"))?
        {
            return mismatch();
        }
        lifecycle.started = true;
        Ok(())
    }

    fn tool_result(&mut self, index: usize, event: &Record, data: &'a Record) -> Checked {
        if !str_eq(event.get("surfaceOp"), "append") {
            if self.open_turn.is_none() {
                return reject("tool/result replacement is outside an open turn".to_owned());
            }
            return Ok(());
        }
        self.require_open_step("tool/result", data)?;
        let message = record(data.get("message"), format!("tool/result {index} message"))?;
        let source = record(message.get("source"), format!("tool/result {index} source"))?;
        let call_id = string(source.get("callId"))?;
        let error = match data.get("error") {
            None => None,
            error => Some(record(error, format!("tool/result {index} error"))?),
        };
        let Some(position) = self
            .tool_lifecycles
            .iter()
            .position(|(id, _)| *id == call_id)
        else {
            return reject(format!(
                "tool/result {call_id} has no advertised tool lifecycle"
            ));
        };
        if !self.tool_lifecycles[position].1.started
            && !is_exact_tool_not_started_repair(index, event, message, call_id, error)?
        {
            return reject(format!(
                "tool/result {call_id} is not the exact TOOL_NOT_STARTED repair"
            ));
        }
        self.tool_lifecycles.remove(position);
        Ok(())
    }

    fn code_dispatch(&mut self, event_type: &str, data: &'a Record) -> Checked {
        self.require_open_turn(event_type)?;
        let root = string(data.get("rootCallId"))?;
        let parent = string(data.get("parentCallId"))?;
        let child = string(data.get("subCallId"))?;
        if self
            .ptc_roots
            .get(child)
            .is_some_and(|known| *known != root)
        {
            return reject(format!("{event_type} changes its rootCallId"));
        }
        if parent != root && self.ptc_roots.get(parent) != Some(&root) {
            return reject(format!(
                "{event_type} parentCallId does not belong to rootCallId"
            ));
        }
        if event_type == "tool/code-dispatch-start" {
            if self.ptc_starts.contains_key(child) {
                return reject("tool/code-dispatch-start repeats subCallId".to_owned());
            }
            self.ptc_starts.insert(
                child,
                PtcStart {
                    root,
                    parent,
                    name: data.get("name"),
                    arguments: data.get("arguments"),
                    settled: false,
                },
            );
        } else {
            let Some(start) = self
                .ptc_starts
                .get_mut(child)
                .filter(|start| !start.settled)
            else {
                return reject("tool/code-dispatch has no unique start".to_owned());
            };
            if start.root != root
                || start.parent != parent
                || !strict_eq(start.name, data.get("name"))?
                || !deep_equal_json(start.arguments, data.get("arguments"))?
            {
                return reject("tool/code-dispatch does not match its start".to_owned());
            }
            start.settled = true;
        }
        self.ptc_roots.insert(child, root);
        Ok(())
    }

    fn retry(&mut self, data: &'a Record) -> Checked {
        let step = self
            .open_step
            .unwrap_or_else(|| self.next_step.saturating_sub(1));
        if !turn_eq(data.get("turn"), self.open_turn)
            || !num_eq(data.get("step"), step)
            || self.open_turn.is_none()
        {
            return reject("llm/retry does not match the current turn and step".to_owned());
        }
        if !strict_eq(data.get("provider"), self.open_step_provider)? {
            return reject("llm/retry provider does not match the open request/header".to_owned());
        }
        self.assert_retry_chain(data)?;
        self.retries.push(data);
        Ok(())
    }

    /// `assertRetryChain`.
    fn assert_retry_chain(&self, data: &Record) -> Checked {
        let mut prior = None;
        for candidate in self.retries.iter().rev() {
            if strict_eq(candidate.get("turn"), data.get("turn"))?
                && strict_eq(candidate.get("step"), data.get("step"))?
                && strict_eq(candidate.get("provider"), data.get("provider"))?
                && strict_eq(candidate.get("policyKey"), data.get("policyKey"))?
            {
                prior = Some(*candidate);
                break;
            }
        }
        let expected = match prior.and_then(|prior| prior.get("retry")) {
            None | Some(Value::Null) => 1,
            Some(retry) => match count_index(retry) {
                Some(retry) => retry + 1,
                None => return precondition(),
            },
        };
        if !num_eq(data.get("retry"), expected) {
            return reject(format!("llm/retry must use retry {expected}"));
        }
        match prior {
            Some(prior) => {
                if !strict_eq(prior.get("retryId"), data.get("retryId"))? {
                    return reject(
                        "llm/retry must preserve retryId across one policy chain".to_owned(),
                    );
                }
            }
            None => {
                for candidate in &self.retries {
                    if strict_eq(candidate.get("retryId"), data.get("retryId"))? {
                        return reject(format!(
                            "llm/retry reuses retryId {} across policy chains",
                            stringify(data.get("retryId"))?
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn retry_started(&mut self, data: &Record) -> Checked {
        let mut scheduled = None;
        for candidate in &self.retries {
            if strict_eq(candidate.get("retryId"), data.get("retryId"))?
                && strict_eq(candidate.get("retry"), data.get("retry"))?
            {
                scheduled = Some(*candidate);
                break;
            }
        }
        let Some(prior) = scheduled else {
            return reject("llm/retry-started pairs no prior scheduled attempt".to_owned());
        };
        if !strict_eq(prior.get("turn"), data.get("turn"))?
            || !strict_eq(prior.get("step"), data.get("step"))?
        {
            return reject(
                "llm/retry-started does not match its scheduled turn and step".to_owned(),
            );
        }
        let key = (
            stringify(data.get("retryId"))?,
            stringify(data.get("retry"))?,
        );
        if !self.retry_starts.insert(key) {
            return reject("llm/retry-started repeats one scheduled attempt".to_owned());
        }
        Ok(())
    }

    /// `assertTitleSources`.
    fn title_sources(&self, index: usize, event_type: &str, data: &Record) -> Checked {
        if event_type == "session/title" {
            let source = record(data.get("source"), format!("session/title {index} source"))?;
            let Some(Value::Array(seqs)) = data.get("messageSeqs") else {
                return precondition();
            };
            if seqs.is_empty() != str_eq(source.get("kind"), "user") {
                return reject(format!(
                    "session/title {index} messageSeqs must be empty exactly for a user title"
                ));
            }
        }
        let Some(Value::Array(seqs)) = data.get("messageSeqs") else {
            return precondition();
        };
        let not_human = || {
            reject(format!(
                "{event_type} {index} messageSeqs must cite earlier human user/message events"
            ))
        };
        let mut selected = Vec::with_capacity(seqs.len());
        for seq in seqs {
            let Some((cited, (source, source_type))) = self.event_at(seq)? else {
                return not_human();
            };
            if *source_type != "user/message" {
                return not_human();
            }
            let source_data = record(source.get("data"), format!("{source_type} {cited} data"))?;
            let message_source = record(
                source_data.get("source"),
                format!("{source_type} {cited} source"),
            )?;
            if !str_eq(message_source.get("kind"), "user") {
                return not_human();
            }
            let Some(Value::Array(content)) = source_data.get("content") else {
                return precondition();
            };
            let mut texts = Vec::new();
            for block in content {
                let Value::Object(block) = block else {
                    return precondition();
                };
                if str_eq(block.get("type"), "text")
                    && let Some(Value::String(text)) = block.get("text")
                {
                    texts.push(text.as_str());
                }
            }
            selected.push((cited, texts.join("\n")));
        }
        if event_type == "session/title-llm-request" {
            self.title_request(data, &selected)?;
        }
        Ok(())
    }

    fn title_request(&self, data: &Record, selected: &[(u64, String)]) -> Checked {
        let items: Vec<String> = selected
            .iter()
            .map(|(seq, text)| format!("{{\"seq\":{seq},\"text\":{}}}", quote(text)))
            .collect();
        let expected = format!("{TITLE_REQUEST_PREFIX}[{}]", items.join(","));
        let Some(Value::Array(messages)) = data.get("messages") else {
            return precondition();
        };
        let message = messages.first();
        let content = match message {
            Some(Value::Object(message)) => message.get("content"),
            _ => None,
        };
        let source_label = "session/title-llm-request message source";
        let source = match message {
            None => None,
            Some(Value::Object(message)) => {
                Some(record(message.get("source"), source_label.to_owned())?)
            }
            Some(Value::Null) => return precondition(),
            Some(_) => return reject(format!("{source_label} must be a JSON object")),
        };
        let unrepresented =
            || reject("session/title-llm-request messages do not represent messageSeqs".to_owned());
        let role = match message {
            Some(Value::Object(message)) => message.get("role"),
            _ => None,
        };
        if messages.len() != 1 || !str_eq(role, "user") {
            return unrepresented();
        }
        let content = match content {
            Some(Value::Array(content)) if content.len() == 1 => content,
            Some(Value::String(_) | Value::Object(_)) => return precondition(),
            _ => return unrepresented(),
        };
        let Some(source) = source else {
            return unrepresented();
        };
        if !str_eq(source.get("kind"), "plugin")
            || !str_eq(source.get("plugin"), "dsh-session-title-llm")
        {
            return unrepresented();
        }
        match &content[0] {
            Value::Object(framed) => {
                if !str_eq(framed.get("type"), "text")
                    || (!self.extensions.preserved_source_title_request_text
                        && !str_eq(framed.get("text"), &expected))
                {
                    return unrepresented();
                }
                Ok(())
            }
            Value::Null => precondition(),
            _ => unrepresented(),
        }
    }

    fn command_done(&self, data: &Record) -> Checked {
        let id = string(data.get("commandId"))?;
        if !self.command_runs.contains(id) {
            return reject(format!("command/done {id} has no prior command/run"));
        }
        let Some(source_seq) = data.get("sourceEventSeq") else {
            return Ok(());
        };
        let source_type = self
            .event_at(source_seq)?
            .map(|(_, (_, source_type))| *source_type);
        if !str_eq(data.get("kind"), "success")
            || matches!(source_type, Some("command/run" | "command/done"))
        {
            return reject(format!("command/done {id} has invalid sourceEventSeq"));
        }
        Ok(())
    }

    fn delivery_accepted(&self, index: usize, data: &Record) -> Checked {
        let zero = Value::from(0_u64);
        let accepted_version = match data.get("sessionFormatVersion") {
            None | Some(Value::Null) => &zero,
            Some(version) => version,
        };
        let Value::Object(header) = self.header else {
            return precondition();
        };
        if strict_eq(Some(accepted_version), header.get("version"))? {
            let seq = u64::try_from(index).unwrap_or(u64::MAX);
            let inherited =
                header.get("parentSession").is_some() && seq < self.inherited_event_count;
            if !inherited && !strict_eq(data.get("sessionId"), header.get("id"))? {
                return reject(
                    "current-generation delivery marker names the wrong Session".to_owned(),
                );
            }
        }
        Ok(())
    }

    /// `assertCompactionOwner`, returning the open compaction.
    fn compaction_owner(&self, fields: &Record, subject: &str) -> Checked<&Compaction<'a>> {
        let no_start = || reject(format!("{subject} has no matching compaction/start"));
        let Some(open) = self.open_compaction.as_ref() else {
            return no_start();
        };
        if !strict_eq(fields.get("compactionId"), open.id)?
            || !strict_eq(fields.get("sourceCommandId"), open.source_command_id)?
        {
            return no_start();
        }
        Ok(open)
    }

    /// `assertCompactionTurn`: `owner === null ? openTurn !== null : owner
    /// !== openTurn` is `owner !== openTurn` for a turn that is `null` or a
    /// number.
    fn assert_compaction_turn(&self, owner: Option<&Value>, subject: &str) -> Checked {
        if !turn_eq(owner, self.open_turn) {
            return reject(format!("{subject} does not match the open turn"));
        }
        Ok(())
    }

    /// `assertCurrentSurfaceSpan`.
    fn assert_current_surface_span(&self, data: &Record, subject: &str) -> Checked {
        let Some(Value::Object(range)) = data.get("shadowedRange") else {
            return precondition();
        };
        let Some(Value::Array(seqs)) = data.get("shadowedSeqs") else {
            return precondition();
        };
        let start = index_of(&self.surface, range.get("start"));
        let end = index_of(&self.surface, range.get("end"));
        let expected = match (start, end) {
            (Some(start), Some(end)) if end >= start => &self.surface[start..=end],
            _ => &[],
        };
        if expected.len() != seqs.len()
            || expected
                .iter()
                .zip(seqs)
                .any(|(seq, named)| !num_eq(Some(named), *seq))
        {
            return reject(format!(
                "{subject} shadowedSeqs do not name an exact current surface span"
            ));
        }
        Ok(())
    }

    /// `events[seq]` for a seq read from a payload, with its index; `None`
    /// where JavaScript reads `undefined`.
    fn event_at(&self, seq: &Value) -> Checked<Option<(u64, &'a (&'a Record, &'a str))>> {
        if !seq.is_number() {
            // A string or other value is converted to a property key.
            return precondition();
        }
        Ok(count_index(seq).and_then(|index| {
            let event = self.events.get(usize::try_from(index).ok()?)?;
            Some((index, event))
        }))
    }
}

/// `isExactToolNotStartedRepair`, evaluated in TypeScript's order: `block`
/// and its `content` eagerly, the rest only while the conjunction holds.
fn is_exact_tool_not_started_repair(
    index: usize,
    event: &Record,
    message: &Record,
    call_id: &str,
    error: Option<&Record>,
) -> Checked<bool> {
    let block = match message.get("content") {
        Some(Value::Array(content)) => content.first(),
        _ => return precondition(),
    };
    let repair_content = match block {
        Some(Value::Object(block)) => block.get("content"),
        _ => None,
    };
    let Some(error) = error else {
        return Ok(false);
    };
    if !str_eq(error.get("name"), "ToolNotStartedError")
        || !str_eq(error.get("code"), "TOOL_NOT_STARTED")
        || event.contains_key("sourceEventSeqs")
        || !str_eq(
            message.get("id"),
            &format!("interrupted-tool-result-{call_id}-{index}"),
        )
    {
        return Ok(false);
    }
    let is_error = match block {
        Some(Value::Object(block)) => block.get("isError") == Some(&Value::Bool(true)),
        _ => false,
    };
    if !is_error {
        return Ok(false);
    }
    let repair_content = match repair_content {
        Some(Value::Array(items)) if items.len() == 1 => &items[0],
        Some(Value::String(_) | Value::Object(_)) => return precondition(),
        _ => return Ok(false),
    };
    let text_type = match repair_content {
        Value::Object(item) => item.get("type"),
        Value::Null => None,
        _ => return precondition(),
    };
    Ok(str_eq(text_type, "text") && str_eq(repair_content.get("text"), TOOL_NOT_STARTED_TEXT))
}

/// `releasedV0Record`.
fn record(value: Option<&Value>, label: String) -> Checked<&Record> {
    match value {
        Some(Value::Object(fields)) => Ok(fields),
        _ => reject(format!("{label} must be a JSON object")),
    }
}

/// A member TypeScript casts to `string` and uses as a key or in a message.
fn string(value: Option<&Value>) -> Checked<&str> {
    match value {
        Some(Value::String(text)) => Ok(text),
        _ => precondition(),
    }
}

fn str_eq(value: Option<&Value>, expected: &str) -> bool {
    matches!(value, Some(Value::String(text)) if text == expected)
}

/// `value === n` for an integer `n` no larger than 2^53.
fn num_eq(value: Option<&Value>, n: u64) -> bool {
    matches!(value, Some(Value::Number(number)) if number.as_f64() == Some(n as f64))
}

/// `value === turn`, where `turn` is `null` or the given number.
fn turn_eq(value: Option<&Value>, turn: Option<u64>) -> bool {
    match (value, turn) {
        (Some(Value::Null), None) => true,
        (value, Some(turn)) => num_eq(value, turn),
        _ => false,
    }
}

/// JavaScript's `===` on two members, either possibly `undefined`.
fn strict_eq(left: Option<&Value>, right: Option<&Value>) -> Checked<bool> {
    let (left, right) = match (left, right) {
        (None, None) => return Ok(true),
        (Some(left), Some(right)) => (left, right),
        _ => return Ok(false),
    };
    Ok(match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(left), Value::Bool(right)) => left == right,
        (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
        (Value::String(left), Value::String(right)) => left == right,
        (Value::Array(_) | Value::Object(_), Value::Array(_) | Value::Object(_)) => {
            return precondition();
        }
        _ => false,
    })
}

/// A non-negative integral number no larger than 2^53 − 1, as its integer;
/// `-0` is 0, which is also the property key JavaScript reads for it.
fn count_index(value: &Value) -> Option<u64> {
    let Value::Number(number) = value else {
        return None;
    };
    if let Some(number) = number.as_u64() {
        return (number <= MAX_SAFE_INTEGER).then_some(number);
    }
    let number = number.as_f64()?;
    (number.fract() == 0.0 && number >= 0.0 && number <= MAX_SAFE_INTEGER as f64)
        .then_some(number as u64)
}

/// `JSON.stringify` of a string or a count; anything else is the
/// precondition limit.
fn stringify(value: Option<&Value>) -> Checked<String> {
    match value {
        Some(Value::String(text)) => Ok(quote(text)),
        Some(value) => count_index(value)
            .map(|count| count.to_string())
            .map_or_else(precondition, Ok),
        None => precondition(),
    }
}

/// `surface.indexOf(value)`.
fn index_of(surface: &[u64], value: Option<&Value>) -> Option<usize> {
    let value = value?.as_f64()?;
    surface.iter().position(|seq| *seq as f64 == value)
}

/// The outcome of `deepEqualJson` when part of it cannot be decided.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Equality {
    Equal,
    Unequal,
    Undecided,
}

/// `deepEqualJson` from `packages/util/values/src/index.ts`.
fn deep_equal_json(left: Option<&Value>, right: Option<&Value>) -> Checked<bool> {
    let outcome = match (left, right) {
        (None, None) => Equality::Equal,
        (Some(left), Some(right)) => deep_equal(left, right),
        _ => Equality::Unequal,
    };
    match outcome {
        Equality::Equal => Ok(true),
        Equality::Unequal => Ok(false),
        Equality::Undecided => Err(Stop::Limit(RelationshipLimit::PrototypeMember)),
    }
}

/// `deepEqualJson`'s member walk. Its result is the conjunction of member
/// results, which `every` reaches in any order because no member comparison
/// throws, so the pairs are compared from an explicit stack, safe at any
/// depth the parser produced.
fn deep_equal(left: &Value, right: &Value) -> Equality {
    let mut outcome = Equality::Equal;
    let mut pending = vec![(left, right)];
    while let Some((left, right)) = pending.pop() {
        let equal = match (left, right) {
            (Value::Array(left), Value::Array(right)) => {
                if left.len() != right.len() {
                    return Equality::Unequal;
                }
                pending.extend(left.iter().zip(right));
                true
            }
            (Value::Array(_), _) | (_, Value::Array(_)) => false,
            (Value::Object(left), Value::Object(right)) => {
                if left.len() != right.len() {
                    return Equality::Unequal;
                }
                for (key, value) in left {
                    match right.get(key) {
                        Some(other) => pending.push((value, other)),
                        // `key in right` finds the inherited member. Every
                        // inherited member but `__proto__` is a function,
                        // which never equals JSON; `Object.prototype` has no
                        // own enumerable keys, so only an empty object can
                        // equal it.
                        None if key == "__proto__"
                            && value.as_object().is_some_and(serde_json::Map::is_empty) =>
                        {
                            outcome = Equality::Undecided;
                        }
                        None => return Equality::Unequal,
                    }
                }
                true
            }
            (Value::Null, Value::Null) => true,
            (Value::Bool(left), Value::Bool(right)) => left == right,
            (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
            (Value::String(left), Value::String(right)) => left == right,
            _ => false,
        };
        if !equal {
            return Equality::Unequal;
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn deep_equality_lets_an_unequal_member_decide_past_a_prototype_member() {
        let decide = |left: Value, right: Value| deep_equal(&left, &right);
        assert!(
            decide(
                json!({"a": 1, "b": [1, {"c": null}]}),
                json!({"b": [1.0, {"c": null}], "a": 1})
            ) == Equality::Equal
        );
        assert!(decide(json!({"__proto__": {}}), json!({"x": 1})) == Equality::Undecided);
        assert!(decide(json!({"toString": 1}), json!({"x": 1})) == Equality::Unequal);
        assert!(decide(json!({"__proto__": {"a": 1}}), json!({"x": 1})) == Equality::Unequal);
        assert!(decide(json!({"__proto__": []}), json!({"x": 1})) == Equality::Unequal);
        assert!(
            decide(json!({"toString": 1, "a": 1}), json!({"x": 1, "a": 2})) == Equality::Unequal
        );
        assert!(decide(json!([{"valueOf": 1}, 2]), json!([{"y": 1}, 3])) == Equality::Unequal);
    }

    #[test]
    fn counts_accept_integral_doubles_and_refuse_the_rest() {
        let parse = |text: &str| serde_json::from_str::<Value>(text).expect("json");
        assert_eq!(count_index(&parse("1.0")), Some(1));
        assert_eq!(count_index(&parse("-0")), Some(0));
        assert_eq!(count_index(&parse("1.5")), None);
        assert_eq!(count_index(&parse("-1")), None);
        assert_eq!(count_index(&parse("9007199254740992")), None);
        assert_eq!(
            count_index(&parse("9007199254740991")),
            Some(MAX_SAFE_INTEGER)
        );
    }
}
