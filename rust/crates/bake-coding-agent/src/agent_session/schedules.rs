//! The opt-in schedule tools and the timer that delivers due reminders
//! ([`crate::schedules`]).
//!
//! Ported from Bake's TypeScript `packages/schedule/schedule/src/{tools,runtime}.ts`:
//! the three tools' names, parameters, descriptions, validation order,
//! results, and error values; and the runtime's decision, one due one-shot
//! or one batch of due fixed-rate reminders at a time, with a durable
//! dispatch before delivery, so a reminder is delivered at most once.
//! Differences, for Pi's loop:
//!
//! - A reminder reaches the model as a session notice
//!   ([`crate::jobs::JobRegistry::post`]): at the running turn's next step,
//!   or in a turn it opens while idle, where 0.3 waited for idle. A
//!   reminder is the user's own request arriving, so it restarts the
//!   notices' wake allowance.
//! - The timer sleeps at most [`RECHECK`] between looks at the wall clock,
//!   so a clock moved forward is seen within it, and a clock moved back
//!   never fires a reminder early.
//! - A persistence failure stops the session's writes, as every write
//!   failure does; 0.3's `persistence_uncertain` value reports it.

#[cfg(test)]
use std::sync::Mutex;
use std::sync::{Arc, Weak};
use std::time::Duration;

use bake_agent::{AgentTool, AgentToolResult, BoxFuture, ToolExecutionMode, ToolReplay};
use bake_ai::{AbortSignal, TextContent, UserContentBlock};
use serde_json::{Map, Value, json};
use tokio::sync::Notify;

use super::recovery::Core;
use super::{WriteRefused, write_session};
use crate::jobs::JobRegistry;
use crate::owned_work::TaskScope;
use crate::schedules::{
    self, CHANGE_TYPE, Change, Due, Folded, InputError, MIN_EVERY_INTERVAL_SECONDS,
};
use crate::session::{GroupEntry, SessionManager};

/// The tool names, reserved while `schedules` is on.
pub(crate) const NAMES: [&str; 3] = ["schedule_create", "schedule_list", "schedule_delete"];

/// The longest the timer goes without reading the wall clock.
pub(crate) const RECHECK: Duration = Duration::from_secs(60);

const CREATE_DESCRIPTION: &str = "Create one reminder in the current session. Supply a non-empty prompt and exactly one selector: a positive safe-integer after_seconds delay, at as a strict offset date-time or local date/time object, or safe-integer every_seconds of at least 300. Fixed-rate reminders stay creation-aligned, skip missed occurrences, and batch one latest occurrence per overdue rule. Delivery is session-local: the reminder runs on time only while this session is live and otherwise becomes overdue until the session is resumed.";
const LIST_DESCRIPTION: &str = "List every active reminder in the current session in creation order, including its exact id, UTC target, scheduled or overdue state, and session-local delivery mode.";
const DELETE_DESCRIPTION: &str = "Delete one active reminder in the current session by the exact id returned by schedule_create or schedule_list. Unknown or already-finished ids return deleted false.";

/// A replaced wall clock, for tests.
#[cfg(test)]
type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// What the tools and the timer share: the timer's wake-up, and the clock.
#[derive(Default)]
pub(crate) struct Schedules {
    changed: Notify,
    #[cfg(test)]
    clock: Mutex<Option<Clock>>,
}

impl Schedules {
    /// The wall clock in epoch milliseconds.
    fn now(&self) -> i64 {
        #[cfg(test)]
        if let Some(clock) = self
            .clock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            return clock();
        }
        bake_ai::now_ms()
    }

    /// Wakes the timer to look again: after a change, or a moved clock.
    pub(crate) fn kick(&self) {
        self.changed.notify_one();
    }

    /// Replaces the wall clock, for tests.
    #[cfg(test)]
    pub(crate) fn set_clock(&self, clock: Clock) {
        *self
            .clock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(clock);
        self.kick();
    }
}

fn corrupt_log() -> Value {
    json!({ "code": "corrupt_schedule_log", "message": "The session schedule log is corrupt." })
}

fn persistence_uncertain(operation: &str, id: Option<&str>) -> Value {
    let mut value = json!({
        "code": "persistence_uncertain",
        "message": "Schedule persistence is uncertain; retry with schedule_list before relying on this result.",
        "operation": operation,
    });
    if let Some(id) = id {
        value["id"] = Value::String(id.to_owned());
    }
    value
}

/// A JSON number that is a JavaScript safe integer.
fn safe_integer(value: &Value) -> Option<i64> {
    if let Some(integer) = value.as_i64() {
        return (integer.unsigned_abs() <= 9_007_199_254_740_991).then_some(integer);
    }
    let float = value.as_f64()?;
    (float.fract() == 0.0 && float.abs() <= 9_007_199_254_740_991.0).then_some(float as i64)
}

/// The rule a create names, once its arguments pass 0.3's
/// `validateCreateArgs`.
enum Selector {
    After(i64),
    At(Value),
    Every(i64),
}

fn validate_create(arguments: &Map<String, Value>) -> Result<(String, Selector), Value> {
    let error = |code: &str, message: &str| json!({ "code": code, "message": message });
    let selectors = ["after_seconds", "at", "every_seconds"]
        .iter()
        .filter(|key| arguments.contains_key(**key))
        .count();
    if arguments.keys().any(|key| {
        !matches!(
            key.as_str(),
            "prompt" | "after_seconds" | "at" | "every_seconds"
        )
    }) || selectors != 1
    {
        return Err(error(
            "invalid_selector",
            "schedule_create accepts exactly one of after_seconds, at, or every_seconds.",
        ));
    }
    let prompt = arguments
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if prompt.trim().is_empty() {
        return Err(error(
            "invalid_prompt",
            "prompt must be non-empty after trimming.",
        ));
    }
    let selector = if let Some(value) = arguments.get("after_seconds") {
        match safe_integer(value).filter(|seconds| *seconds > 0) {
            Some(seconds) => Selector::After(seconds),
            None => {
                return Err(error(
                    "invalid_rule",
                    "after_seconds must be a positive safe integer.",
                ));
            }
        }
    } else if let Some(value) = arguments.get("every_seconds") {
        match safe_integer(value) {
            Some(seconds) if seconds < MIN_EVERY_INTERVAL_SECONDS => {
                return Err(error(
                    "frequency_too_high",
                    &format!("every_seconds must be at least {MIN_EVERY_INTERVAL_SECONDS}."),
                ));
            }
            Some(seconds) => Selector::Every(seconds),
            None => {
                return Err(error(
                    "invalid_rule",
                    "every_seconds must be a safe integer.",
                ));
            }
        }
    } else {
        Selector::At(arguments.get("at").cloned().unwrap_or(Value::Null))
    };
    Ok((prompt.to_owned(), selector))
}

/// The fold for a tool, a corrupt log as 0.3's value.
fn folded(session: &SessionManager) -> Result<Folded, Value> {
    schedules::fold(session).map_err(|_| corrupt_log())
}

/// Appends `change` durably; `Err` once the file could not take it.
fn append(
    session: &mut SessionManager,
    change: &Change,
) -> Result<(), crate::session::SessionError> {
    session
        .append_durable_group(vec![GroupEntry::Custom {
            custom_type: CHANGE_TYPE.into(),
            data: Some(change.to_json()),
        }])
        .map(|_| ())
}

/// One tool's work on the session, through its write boundary.
async fn run(
    core: &Arc<Core>,
    schedules: &Arc<Schedules>,
    operation: &'static str,
    signal: Option<AbortSignal>,
    work: impl FnOnce(
        &mut SessionManager,
        &Schedules,
        &AbortSignalCheck,
    ) -> Result<Value, (Value, Option<String>)>
    + Send
    + 'static,
) -> Result<AgentToolResult, String> {
    let shared = Arc::clone(schedules);
    let check = AbortSignalCheck(signal);
    let task = core
        .owned_context_commit(move |core| {
            let mut failed_id: Option<String> = None;
            let written = write_session(&core.session, &core.gap, |session| {
                match work(session, &shared, &check) {
                    Ok(value) => Ok(Ok(value)),
                    // A failed append: the session records the gap.
                    Err((value, Some(error))) => {
                        failed_id = value.get("id").and_then(Value::as_str).map(str::to_owned);
                        Err(error)
                    }
                    Err((value, None)) => Ok(Err(value)),
                }
            });
            match written {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(value)) => Err(value),
                Err(WriteRefused::AfterGap(_) | WriteRefused::Failed(_)) => {
                    Err(persistence_uncertain(operation, failed_id.as_deref()))
                }
            }
        })
        .map_err(|error| error.to_string())?;
    let value = match task.await {
        Ok(Ok(value)) => {
            schedules.kick();
            value
        }
        Ok(Err(value)) => value,
        Err(error) => return Err(format!("Schedule I/O failed: {error}")),
    };
    if value.get("code").and_then(Value::as_str) == Some("aborted") {
        return Err("Operation aborted".to_owned());
    }
    Ok(AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent::new(value.to_string()))],
        details: Some(value),
        ..AgentToolResult::default()
    })
}

/// Whether the call was cancelled before it wrote.
struct AbortSignalCheck(Option<AbortSignal>);

impl AbortSignalCheck {
    fn aborted(&self) -> bool {
        self.0.as_ref().is_some_and(AbortSignal::aborted)
    }
}

fn aborted() -> (Value, Option<String>) {
    (json!({ "code": "aborted" }), None)
}

fn tool(
    name: &'static str,
    label: &'static str,
    description: &'static str,
    parameters: Value,
    replay: ToolReplay,
    execute: impl Fn(Value, Option<AbortSignal>) -> BoxFuture<'static, Result<AgentToolResult, String>>
    + Send
    + Sync
    + 'static,
) -> Arc<AgentTool> {
    let mut tool = AgentTool::new(
        name,
        label,
        description,
        parameters,
        move |_id, arguments, signal, _update, _ctx| execute(arguments, signal),
    );
    // One change at a time, in call order.
    tool.execution_mode = Some(ToolExecutionMode::Sequential);
    tool.replay = Some(replay);
    Arc::new(tool)
}

/// The three tools over `core`'s session.
pub(super) fn tools(core: &Arc<Core>, schedules: &Arc<Schedules>) -> Vec<Arc<AgentTool>> {
    let create = {
        let (core, schedules) = (Arc::clone(core), Arc::clone(schedules));
        tool(
            "schedule_create",
            "Create reminder",
            CREATE_DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "Reminder content to present when the target becomes due." },
                    "after_seconds": { "type": "number", "description": "Positive safe-integer delay in seconds." },
                    "every_seconds": { "type": "number", "description": "Fixed-rate safe-integer interval in seconds, at least 300." },
                    "at": {
                        "oneOf": [
                            { "type": "string" },
                            {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "date": { "type": "string" },
                                    "time": { "type": "string" },
                                    "time_zone": { "type": "string" }
                                },
                                "required": ["date", "time", "time_zone"]
                            }
                        ],
                        "description": "Absolute target as strict offset RFC 3339 or local date/time with an explicit IANA zone."
                    }
                },
                "required": ["prompt"]
            }),
            // A rerun would create a second reminder.
            ToolReplay::Never,
            move |arguments, signal| {
                let (core, schedules) = (Arc::clone(&core), Arc::clone(&schedules));
                Box::pin(async move {
                    let arguments = arguments.as_object().cloned().unwrap_or_default();
                    let (prompt, selector) = match validate_create(&arguments) {
                        Ok(valid) => valid,
                        Err(value) => return Ok(result(value)),
                    };
                    run(
                        &core,
                        &schedules,
                        "create",
                        signal,
                        move |session, schedules, check| {
                            let folded = folded(session).map_err(|value| (value, None))?;
                            let id = schedules::allocate_id(&folded);
                            let now = schedules.now();
                            let record = match selector {
                                Selector::After(seconds) => {
                                    schedules::create_after(id.clone(), &prompt, seconds, now)
                                }
                                Selector::At(at) => {
                                    schedules::create_at(id.clone(), &prompt, &at, now)
                                }
                                Selector::Every(seconds) => {
                                    schedules::create_every(id.clone(), &prompt, seconds, now)
                                }
                            }
                            .map_err(|error: InputError| (error.to_json(), None))?;
                            if check.aborted() {
                                return Err(aborted());
                            }
                            let change = Change::Create {
                                session_id: session.session_id().to_owned(),
                                record: record.clone(),
                            };
                            append(session, &change)
                                .map_err(|error| (json!({ "id": id }), Some(error.to_string())))?;
                            Ok(schedules::view(&record, schedules.now()))
                        },
                    )
                    .await
                })
            },
        )
    };
    let list = {
        let (core, schedules) = (Arc::clone(core), Arc::clone(schedules));
        tool(
            "schedule_list",
            "List reminders",
            LIST_DESCRIPTION,
            json!({ "type": "object", "properties": {} }),
            ToolReplay::Safe,
            move |_arguments, signal| {
                let (core, schedules) = (Arc::clone(&core), Arc::clone(&schedules));
                Box::pin(async move {
                    run(
                        &core,
                        &schedules,
                        "list",
                        signal,
                        |session, schedules, _check| {
                            let folded = folded(session).map_err(|value| (value, None))?;
                            let now = schedules.now();
                            Ok(Value::Array(
                                folded
                                    .active
                                    .iter()
                                    .map(|record| schedules::view(record, now))
                                    .collect(),
                            ))
                        },
                    )
                    .await
                })
            },
        )
    };
    let delete = {
        let (core, schedules) = (Arc::clone(core), Arc::clone(schedules));
        tool(
            "schedule_delete",
            "Delete reminder",
            DELETE_DESCRIPTION,
            json!({
                "type": "object",
                "properties": { "id": { "type": "string", "description": "Exact session-local schedule id." } },
                "required": ["id"]
            }),
            ToolReplay::Never,
            move |arguments, signal| {
                let (core, schedules) = (Arc::clone(&core), Arc::clone(&schedules));
                Box::pin(async move {
                    let id = arguments
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    if id.is_empty() || id.trim() != id {
                        return Ok(result(json!({
                            "code": "invalid_rule",
                            "message": "schedule_delete id must be non-empty without surrounding whitespace.",
                        })));
                    }
                    run(&core, &schedules, "delete", signal, move |session, _schedules, check| {
                        let folded = folded(session).map_err(|value| (value, None))?;
                        if !folded.active.iter().any(|record| record.id == id) {
                            return Ok(json!({ "id": id, "deleted": false, "code": "schedule_not_found" }));
                        }
                        if check.aborted() {
                            return Err(aborted());
                        }
                        append(session, &Change::Delete { id: id.clone() })
                            .map_err(|error| (json!({ "id": id }), Some(error.to_string())))?;
                        Ok(json!({ "id": id, "deleted": true }))
                    })
                    .await
                })
            },
        )
    };
    vec![create, list, delete]
}

fn result(value: Value) -> AgentToolResult {
    AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent::new(value.to_string()))],
        details: Some(value),
        ..AgentToolResult::default()
    }
}

/// Starts the session's timer on its work gate; shutdown cancels and
/// awaits it.
pub(super) fn start(
    core: &Arc<Core>,
    jobs: JobRegistry,
    schedules: Arc<Schedules>,
) -> Result<(), String> {
    let weak = Arc::downgrade(core);
    core.work
        .spawn(move |scope| timer(weak, jobs, schedules, scope))
        .map(drop)
        .map_err(|error| error.to_string())
}

/// What the timer does next.
enum Step {
    Wait(Duration),
    Stop,
    Again,
}

async fn timer(core: Weak<Core>, jobs: JobRegistry, schedules: Arc<Schedules>, scope: TaskScope) {
    loop {
        // Enabled before the look, so a change made during it still wakes.
        let changed = schedules.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let step = match core.upgrade() {
            Some(core) => look(&core, &jobs, &schedules).await,
            None => Step::Stop,
        };
        let wait = match step {
            Step::Stop => return,
            Step::Again => continue,
            Step::Wait(wait) => wait,
        };
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = &mut changed => {}
            () = scope.signal().cancelled() => return,
        }
    }
}

/// One look: deliver what is due, or say how long to wait.
async fn look(core: &Arc<Core>, jobs: &JobRegistry, schedules: &Arc<Schedules>) -> Step {
    let clock = Arc::clone(schedules);
    let decided = core
        .with_session(move |session| {
            let folded = schedules::fold(session)?;
            let now = clock.now();
            schedules::due(&folded, now).map(|due| (due, now))
        })
        .await;
    // A corrupt log or a session that takes no more writes: nothing more
    // is delivered from it.
    let Ok(Ok((due, now))) = decided else {
        return Step::Stop;
    };
    let Some((message, changes)) = schedules::delivery(&due) else {
        let Due::Wait(target) = due else {
            return Step::Stop;
        };
        let until = target.map_or(RECHECK, |target| {
            Duration::from_millis(u64::try_from(target - now).unwrap_or(0)).min(RECHECK)
        });
        return Step::Wait(until);
    };
    // The dispatch is durable before the reminder is posted: a crash
    // between them loses the reminder rather than repeating it.
    let Ok(task) = core.owned_context_commit(move |core| {
        write_session(&core.session, &core.gap, |session| {
            session.append_durable_group(
                changes
                    .iter()
                    .map(|change| GroupEntry::Custom {
                        custom_type: CHANGE_TYPE.into(),
                        data: Some(change.to_json()),
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .is_ok()
    }) else {
        return Step::Stop;
    };
    if !matches!(task.await, Ok(true)) {
        return Step::Stop;
    }
    jobs.reset_wakes();
    if jobs.post(message).is_err() {
        return Step::Stop;
    }
    Step::Again
}

#[cfg(test)]
mod tests;
