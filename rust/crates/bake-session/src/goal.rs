//! Development-only fold of a restored Session's durable goal, as
//! `goalProjectionDefinition` in `packages/goal/goal/src/index.ts` folds the
//! same events with `applyGoalProjection` and the strict replay rules of
//! `packages/goal/goal/src/fold.ts`.
//!
//! Only `goal/change` events and `user/message` events whose source `kind` is
//! `goal` are read. A `goal/change` is decoded strictly and checked against
//! the current goal: `create` needs a fresh id and no current goal other than
//! a complete one, every other operation advances the current goal by exactly
//! one revision and keeps its counters and timestamps, and a goal-sourced
//! message must start the active goal's next admitted round. The first
//! violation is kept as `failure`, the exact TypeScript string
//! `goal replay failed at session event N: <message>`, with the state before
//! that event, and every later event is ignored.
//!
//! Restoration proved each `user/message`'s data and source objects, and
//! every number in them spelled as `JSON.stringify` writes it, but checked no
//! `goal/change` payload. Where TypeScript's outcome rests on JavaScript
//! number reading or `String` formatting this port does not reproduce,
//! [`goal_projection`] refuses with a [`GoalLimit`] and claims no TypeScript
//! outcome.

use serde_json::{Map, Value};

use crate::{MAX_SAFE_INTEGER, RestoredLog};

/// `GoalPhase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalPhase {
    Active,
    Paused,
    Blocked,
    Complete,
}

impl GoalPhase {
    /// The phase as the log spells it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
            Self::Complete => "complete",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "active" => Self::Active,
            "paused" => Self::Paused,
            "blocked" => Self::Blocked,
            "complete" => Self::Complete,
            _ => return None,
        })
    }
}

/// `GoalBlockReason`: a lower-kebab-case code and a trimmed message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalBlockReason {
    pub code: String,
    pub message: String,
}

/// `GoalSnapshot`, as decoded from a `goal/change`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalSnapshot {
    pub id: String,
    /// A positive safe integer.
    pub revision: u64,
    pub objective: String,
    pub phase: GoalPhase,
    /// A positive safe integer.
    pub max_goal_rounds: u64,
    /// Present exactly when `phase` is [`GoalPhase::Blocked`].
    pub blocked_reason: Option<GoalBlockReason>,
}

/// `GoalProjection`: the current goal and its counters. `rounds_started`
/// can exceed `max_goal_rounds` after an `edit` lowers the limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalProjection {
    pub goal: GoalSnapshot,
    pub rounds_started: u64,
    pub created_at: u64,
    pub updated_at: u64,
}

/// `GoalProjectionState`: the current goal, every goal id ever created in
/// creation order, and the first replay failure.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoalProjectionState {
    pub current: Option<GoalProjection>,
    pub seen_goal_ids: Vec<String>,
    pub failure: Option<String>,
}

/// Event `seq` needs JavaScript behavior this port does not reproduce;
/// nothing is claimed about TypeScript's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoalRefusal {
    pub seq: u64,
    pub limit: GoalLimit,
}

/// Input whose goal fold depends on JavaScript number reading or formatting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalLimit {
    /// A `goal/change` count `Number.isSafeInteger` checks is spelled with a
    /// fraction or an exponent, is -0, or lies beyond `u64`, so JavaScript
    /// may read it as a safe integer this port does not.
    Number,
    /// An unsupported `version` that JavaScript formats with `String`: a
    /// number other than a safe integer written without a fraction or
    /// exponent, an object, or an array.
    VersionDiagnostic,
}

/// Fold `goalProjectionDefinition` from `init()` over the restored stored
/// events and then the closers. The appended end seed is not a goal event.
pub fn goal_projection(restored: &RestoredLog) -> Result<GoalProjectionState, GoalRefusal> {
    let mut state = GoalProjectionState::default();
    for event in restored.stored().events() {
        let envelope = event.envelope();
        apply(&mut state, envelope.seq, envelope.event_type, envelope.data)?;
    }
    for closer in restored.closers() {
        let event_type = closer["type"].as_str().expect("closer type");
        let seq = closer["seq"].as_u64().expect("closer seq");
        apply(&mut state, seq, event_type, &closer["data"])?;
    }
    Ok(state)
}

/// Why one owned event was not applied.
enum Thrown {
    /// The strict fold throws an `Error` with this message.
    Error(String),
    Limit(GoalLimit),
}

impl From<GoalLimit> for Thrown {
    fn from(limit: GoalLimit) -> Self {
        Self::Limit(limit)
    }
}

fn error<T>(message: impl Into<String>) -> Result<T, Thrown> {
    Err(Thrown::Error(message.into()))
}

/// `applyGoalProjection`: the next state is built from a copy, so a failure
/// keeps the state before the event.
fn apply(
    state: &mut GoalProjectionState,
    seq: u64,
    event_type: &str,
    data: &Value,
) -> Result<(), GoalRefusal> {
    if state.failure.is_some() {
        return Ok(());
    }
    let result = match event_type {
        "goal/change" => apply_change(state, seq, data),
        // Restoration proved a `user/message`'s data and source objects.
        "user/message" if data["source"]["kind"] == "goal" => {
            apply_round(state, seq, &data["source"])
        }
        _ => return Ok(()),
    };
    match result {
        Ok(next) => *state = next,
        Err(Thrown::Error(message)) => {
            state.failure = Some(format!(
                "goal replay failed at session event {seq}: {message}"
            ));
        }
        Err(Thrown::Limit(limit)) => return Err(GoalRefusal { seq, limit }),
    }
    Ok(())
}

/// A decoded `goal/change`.
enum Change {
    Clear {
        id: String,
        revision: u64,
        cleared_at: u64,
    },
    Snapshot {
        operation: Operation,
        goal: GoalSnapshot,
        rounds_started: u64,
        created_at: u64,
        updated_at: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    Create,
    Edit,
    Pause,
    Resume,
    Complete,
    Block,
}

impl Operation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Edit => "edit",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Complete => "complete",
            Self::Block => "block",
        }
    }

    fn parse(value: Option<&Value>) -> Option<Self> {
        Some(match value?.as_str()? {
            "create" => Self::Create,
            "edit" => Self::Edit,
            "pause" => Self::Pause,
            "resume" => Self::Resume,
            "complete" => Self::Complete,
            "block" => Self::Block,
            _ => return None,
        })
    }
}

/// `applyGoalEvent` for a `goal/change`, then `applyGoalChange`.
fn apply_change(
    state: &GoalProjectionState,
    seq: u64,
    data: &Value,
) -> Result<GoalProjectionState, Thrown> {
    let Some(change) = decode_change(data)? else {
        return error(format!(
            "goal change at session event {seq} has an invalid kind"
        ));
    };
    let mut next = state.clone();
    match change {
        Change::Clear {
            id,
            revision,
            cleared_at,
        } => {
            let Some(current) = &state.current else {
                return error("goal clear requires a current goal");
            };
            require_next_revision(&current.goal, &id, revision, "clear")?;
            if cleared_at < current.updated_at {
                return error("goal clear timestamp cannot precede the current goal update");
            }
            next.current = None;
        }
        Change::Snapshot {
            operation,
            goal,
            rounds_started,
            created_at,
            updated_at,
        } => {
            if operation == Operation::Create {
                let blocked_by_current = state
                    .current
                    .as_ref()
                    .is_some_and(|current| current.goal.phase != GoalPhase::Complete);
                if goal.revision != 1
                    || goal.phase != GoalPhase::Active
                    || rounds_started != 0
                    || blocked_by_current
                    || state.seen_goal_ids.contains(&goal.id)
                {
                    return error(
                        "goal create requires a fresh active revision-one goal with zero rounds",
                    );
                }
                next.seen_goal_ids.push(goal.id.clone());
            } else {
                let Some(current) = &state.current else {
                    return error(format!(
                        "goal {} requires a current goal",
                        operation.as_str()
                    ));
                };
                validate_transition(
                    current,
                    operation,
                    &goal,
                    rounds_started,
                    created_at,
                    updated_at,
                )?;
            }
            next.current = Some(GoalProjection {
                goal,
                rounds_started,
                created_at,
                updated_at,
            });
        }
    }
    Ok(next)
}

/// `validateSnapshotTransition`, after the current goal is known to exist.
fn validate_transition(
    current: &GoalProjection,
    operation: Operation,
    goal: &GoalSnapshot,
    rounds_started: u64,
    created_at: u64,
    updated_at: u64,
) -> Result<(), Thrown> {
    let name = operation.as_str();
    require_next_revision(&current.goal, &goal.id, goal.revision, name)?;
    if created_at != current.created_at
        || updated_at < current.updated_at
        || rounds_started != current.rounds_started
    {
        return error(format!(
            "goal {name} does not preserve the current counters and timestamps"
        ));
    }
    let from = current.goal.phase;
    let to = goal.phase;
    if operation == Operation::Edit {
        if to != from || goal.blocked_reason != current.goal.blocked_reason {
            return error("goal edit cannot change phase or blocked reason");
        }
        return Ok(());
    }
    if goal.objective != current.goal.objective
        || goal.max_goal_rounds != current.goal.max_goal_rounds
    {
        return error(format!(
            "goal {name} cannot change objective or maxGoalRounds"
        ));
    }
    let valid = match operation {
        Operation::Pause => from == GoalPhase::Active && to == GoalPhase::Paused,
        Operation::Resume => {
            if from == GoalPhase::Complete
                || to != GoalPhase::Active
                || current.rounds_started >= goal.max_goal_rounds
            {
                return error(
                    "goal resume has an invalid phase transition or exhausted round budget",
                );
            }
            true
        }
        Operation::Complete => from != GoalPhase::Complete && to == GoalPhase::Complete,
        Operation::Block => from == GoalPhase::Active && to == GoalPhase::Blocked,
        Operation::Create | Operation::Edit => unreachable!("handled by the caller"),
    };
    if !valid {
        return error(format!("goal {name} has an invalid phase transition"));
    }
    Ok(())
}

/// `requireNextRevision`.
fn require_next_revision(
    current: &GoalSnapshot,
    id: &str,
    revision: u64,
    operation: &str,
) -> Result<(), Thrown> {
    if id != current.id || revision != current.revision + 1 {
        return error(format!(
            "goal {operation} must advance the current goal by one revision"
        ));
    }
    Ok(())
}

/// `applyGoalEvent` for a goal-sourced `user/message`: `goalSource`, then
/// round admission.
fn apply_round(
    state: &GoalProjectionState,
    seq: u64,
    source: &Value,
) -> Result<GoalProjectionState, Thrown> {
    let invalid = || error("goal message source is invalid");
    let goal_id = match source.get("goalId") {
        Some(Value::String(id)) if !id.is_empty() => id,
        _ => return invalid(),
    };
    let Some(revision) = message_safe_integer(source.get("revision")).filter(|value| *value >= 1)
    else {
        return invalid();
    };
    let Some(round) = message_safe_integer(source.get("round")).filter(|value| *value >= 1) else {
        return invalid();
    };
    let admitted = state.current.as_ref().filter(|current| {
        current.goal.phase == GoalPhase::Active
            && *goal_id == current.goal.id
            && revision == current.goal.revision as i64
            && round == current.rounds_started as i64 + 1
            && round <= current.goal.max_goal_rounds as i64
    });
    if admitted.is_none() {
        return error(format!(
            "goal round at session event {seq} is not the next admitted round of the active goal"
        ));
    }
    let mut next = state.clone();
    next.current.as_mut().expect("admitted goal").rounds_started = round as u64;
    Ok(next)
}

/// `decodeGoalChange`; `None` for a value that is not a goal change.
fn decode_change(value: &Value) -> Result<Option<Change>, Thrown> {
    let Some(fields) = value.as_object() else {
        return Ok(None);
    };
    if fields.get("kind").and_then(Value::as_str) != Some("goal/change") {
        return Ok(None);
    }
    match fields.get("version") {
        Some(Value::Number(version)) if version.as_f64() == Some(1.0) => {}
        version => {
            let shown = version_label(version).ok_or(GoalLimit::VersionDiagnostic)?;
            return error(format!("unsupported goal change version {shown}"));
        }
    }
    if fields.get("operation").and_then(Value::as_str) == Some("clear") {
        let allowed = "cleared,clearedAt,kind,operation,version";
        if sorted_keys(fields) != allowed {
            return error(format!(
                "goal clear change must have exactly {allowed} fields"
            ));
        }
        let (id, revision) = decode_ref(&value["cleared"])?;
        let cleared_at = non_negative_integer(fields.get("clearedAt"), "clearedAt")?;
        return Ok(Some(Change::Clear {
            id,
            revision,
            cleared_at,
        }));
    }
    let Some(operation) = Operation::parse(fields.get("operation")) else {
        return error("goal change operation is invalid");
    };
    let allowed = "createdAt,goal,kind,operation,roundsStarted,updatedAt,version";
    if sorted_keys(fields) != allowed {
        return error(format!(
            "goal snapshot change must have exactly {allowed} fields"
        ));
    }
    let created_at = non_negative_integer(fields.get("createdAt"), "createdAt")?;
    let updated_at = non_negative_integer(fields.get("updatedAt"), "updatedAt")?;
    if updated_at < created_at {
        return error("goal change updatedAt cannot precede createdAt");
    }
    let goal = decode_snapshot(&value["goal"])?;
    let rounds_started = non_negative_integer(fields.get("roundsStarted"), "roundsStarted")?;
    Ok(Some(Change::Snapshot {
        operation,
        goal,
        rounds_started,
        created_at,
        updated_at,
    }))
}

/// `decodeSnapshot`.
fn decode_snapshot(value: &Value) -> Result<GoalSnapshot, Thrown> {
    let Some(fields) = value.as_object() else {
        return error("goal change goal must be a record");
    };
    let id = match fields.get("id") {
        Some(Value::String(id)) if !id.is_empty() => id.clone(),
        _ => return error("goal change goal.id must be a non-empty string"),
    };
    let Some(objective) = fields
        .get("objective")
        .and_then(Value::as_str)
        .filter(|objective| normalized(objective))
    else {
        return error("goal change goal.objective must be non-empty and normalized");
    };
    let Some(phase) = fields
        .get("phase")
        .and_then(Value::as_str)
        .and_then(GoalPhase::parse)
    else {
        return error("goal change goal.phase is invalid");
    };
    let expected = if phase == GoalPhase::Blocked {
        "blockedReason,id,maxGoalRounds,objective,phase,revision"
    } else {
        "id,maxGoalRounds,objective,phase,revision"
    };
    if sorted_keys(fields) != expected {
        return error(format!(
            "goal change goal for phase {} must have exactly {expected} fields",
            phase.as_str()
        ));
    }
    let revision = positive_integer(fields.get("revision"), "goal.revision")?;
    let max_goal_rounds = positive_integer(fields.get("maxGoalRounds"), "goal.maxGoalRounds")?;
    let blocked_reason = if phase == GoalPhase::Blocked {
        Some(decode_block_reason(&value["blockedReason"])?)
    } else {
        None
    };
    Ok(GoalSnapshot {
        id,
        revision,
        objective: objective.to_owned(),
        phase,
        max_goal_rounds,
        blocked_reason,
    })
}

/// `decodeBlockReason`.
fn decode_block_reason(value: &Value) -> Result<GoalBlockReason, Thrown> {
    let Some(fields) = value
        .as_object()
        .filter(|fields| sorted_keys(fields) == "code,message")
    else {
        return error("goal change goal.blockedReason must have exactly code and message fields");
    };
    let Some(code) = fields
        .get("code")
        .and_then(Value::as_str)
        .filter(|code| lower_kebab(code))
    else {
        return error("goal change goal.blockedReason.code must be lower-kebab-case");
    };
    let Some(message) = fields
        .get("message")
        .and_then(Value::as_str)
        .filter(|message| normalized(message))
    else {
        return error("goal change goal.blockedReason.message must be non-empty and normalized");
    };
    Ok(GoalBlockReason {
        code: code.to_owned(),
        message: message.to_owned(),
    })
}

/// `decodeRef` for a clear tombstone.
fn decode_ref(value: &Value) -> Result<(String, u64), Thrown> {
    let Some(fields) = value
        .as_object()
        .filter(|fields| sorted_keys(fields) == "id,revision")
    else {
        return error("goal clear tombstone must have exactly id and revision fields");
    };
    let id = match fields.get("id") {
        Some(Value::String(id)) if !id.is_empty() => id.clone(),
        _ => return error("goal clear tombstone id must be a non-empty string"),
    };
    let revision = positive_integer(fields.get("revision"), "cleared.revision")?;
    Ok((id, revision))
}

fn positive_integer(value: Option<&Value>, field: &str) -> Result<u64, Thrown> {
    match safe_integer(value)? {
        Some(value) if value >= 1 => Ok(value as u64),
        _ => error(format!(
            "goal change {field} must be a positive safe integer"
        )),
    }
}

fn non_negative_integer(value: Option<&Value>, field: &str) -> Result<u64, Thrown> {
    match safe_integer(value)? {
        Some(value) if value >= 0 => Ok(value as u64),
        _ => error(format!(
            "goal change {field} must be a non-negative safe integer"
        )),
    }
}

/// `typeof value === 'number' && Number.isSafeInteger(value)`, as the value;
/// `None` when that fails. A number serde_json holds as a float, including
/// -0, is [`GoalLimit::Number`].
fn safe_integer(value: Option<&Value>) -> Result<Option<i64>, GoalLimit> {
    let Some(Value::Number(number)) = value else {
        return Ok(None);
    };
    if let Some(number) = number.as_u64() {
        return Ok((number <= MAX_SAFE_INTEGER).then_some(number as i64));
    }
    if let Some(number) = number.as_i64() {
        return Ok((number.unsigned_abs() <= MAX_SAFE_INTEGER).then_some(number));
    }
    Err(GoalLimit::Number)
}

/// `Number.isSafeInteger` of a restored `user/message` number, as the value.
/// Restoration admitted only numbers spelled as `JSON.stringify` writes them,
/// and it writes every safe integer without a fraction or an exponent, so a
/// number serde_json holds as a float is not one.
fn message_safe_integer(value: Option<&Value>) -> Option<i64> {
    safe_integer(value).ok().flatten()
}

/// `String(value['version'])` where this port reproduces it.
fn version_label(value: Option<&Value>) -> Option<String> {
    match value {
        None => Some("undefined".to_owned()),
        Some(Value::Null) => Some("null".to_owned()),
        Some(Value::Bool(flag)) => Some(flag.to_string()),
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Number(_)) => safe_integer(value).ok().flatten().map(|n| n.to_string()),
        Some(Value::Array(_) | Value::Object(_)) => None,
    }
}

/// `Object.keys(value).sort().join(',')`: keys sorted by UTF-16 code units
/// and joined, so a key containing a comma can complete an expected list.
fn sorted_keys(fields: &Map<String, Value>) -> String {
    let mut keys: Vec<&str> = fields.keys().map(String::as_str).collect();
    keys.sort_by(|left, right| crate::js_string::cmp_code_units(left, right));
    keys.join(",")
}

/// `text.trim().length > 0 && text === text.trim()`.
fn normalized(text: &str) -> bool {
    let trimmed = text.trim_matches(is_js_trimmed);
    !trimmed.is_empty() && trimmed.len() == text.len()
}

/// ECMAScript WhiteSpace and LineTerminator, which `String.prototype.trim`
/// removes. Unlike `char::is_whitespace`, it includes U+FEFF and excludes
/// U+0085.
const fn is_js_trimmed(character: char) -> bool {
    matches!(
        character,
        '\u{9}'..='\u{d}'
            | ' '
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// `/^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$/`, whose `$` matches only at the end.
fn lower_kebab(code: &str) -> bool {
    let segment = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    };
    code.as_bytes().first().is_some_and(u8::is_ascii_lowercase) && code.split('-').all(segment)
}
