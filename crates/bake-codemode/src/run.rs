//! One script run: its thread, its events, and the answers to its calls.
//! The host-side checks follow Pi v1.3.0's `runtime/execution.ts` (MIT, see
//! `NOTICE`): global names, unique tool names, and, for deterministic runs,
//! unique script names and the answer rules of `CodemodeRun.deliver()`.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde_json::{Map, Value};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::engine::CodemodeEngine;
use crate::host::{CallBook, RunControl, StopReason, lock, store_json};
use crate::identifier::{is_identifier, to_codemode_identifier};
use crate::types::{
    AnswerError, CallResult, Event, ExecuteRequest, ExitHook, Failure, GlobalDeclaration, Outcome,
    StartError,
};
use crate::vm::{Command, Delivery, Script, ScriptInput, run_script};

/// The script thread's stack: Wasmtime's 32 MiB Wasm stack limit and the
/// host's own frames fit on it, so QuickJS's 512 KiB stack guard makes deep
/// recursion a catchable `RangeError` and nothing overflows the host stack.
/// It is reserved address space; only the pages a script uses are committed.
pub const SCRIPT_THREAD_STACK_BYTES: usize = 40 << 20;

/// Names a global may not take: the prelude's own globals.
const RESERVED_GLOBALS: [&str; 9] = [
    "tools",
    "ALL_TOOLS",
    "console",
    "text",
    "image",
    "exit",
    "globalThis",
    "store",
    "load",
];

/// A running script. It owns the script's thread: dropping it aborts the
/// script and joins the thread (on a blocking task when a Tokio runtime is
/// current, otherwise in place), so no script outlives its run.
/// [`Run::handle`] gives tasks that answer calls or abort the run a handle of
/// their own.
pub struct Run {
    events: UnboundedReceiver<Event>,
    handle: RunHandle,
    thread: Option<JoinHandle<()>>,
    done: Option<Arc<Outcome>>,
}

impl std::fmt::Debug for Run {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Run")
            .field("deterministic", &self.handle.deterministic)
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

/// A cloneable handle that answers a [`Run`]'s calls and aborts it, for tasks
/// that execute calls while another owns the `Run` and awaits its events. It
/// does not keep the script alive: once the `Run` is dropped, answers are
/// ignored.
#[derive(Clone)]
pub struct RunHandle {
    commands: Sender<Command>,
    book: Arc<Mutex<CallBook>>,
    control: Arc<RunControl>,
    deterministic: bool,
}

impl std::fmt::Debug for RunHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunHandle")
            .field("deterministic", &self.deterministic)
            .finish_non_exhaustive()
    }
}

/// Pi's `checkGlobals`: identifiers or `namespace.member`, not shadowing a
/// built-in global or each other.
fn check_globals(globals: &[GlobalDeclaration]) -> Result<(), StartError> {
    let mut names: Vec<&str> = Vec::new();
    let mut namespaces: Vec<&str> = Vec::new();
    for global in globals {
        let parts: Vec<&str> = global.name.split('.').collect();
        if parts.len() > 2
            || !parts.iter().all(|part| is_identifier(part))
            || RESERVED_GLOBALS.contains(&parts[0])
        {
            return Err(StartError::InvalidRequest(format!(
                "Invalid global name \"{}\"",
                global.name
            )));
        }
        if names.contains(&global.name.as_str()) {
            return Err(StartError::InvalidRequest(format!(
                "Global \"{}\" is already registered",
                global.name
            )));
        }
        if parts.len() == 2 {
            namespaces.push(parts[0]);
        }
        names.push(&global.name);
    }
    if let Some(name) = namespaces
        .iter()
        .find(|namespace| names.contains(namespace))
    {
        return Err(StartError::InvalidRequest(format!(
            "Global \"{name}\" conflicts with the namespace \"{name}\""
        )));
    }
    Ok(())
}

/// Calls the exit hook when the script thread ends, or when a run that took
/// the hook does not start.
struct ExitGuard(Option<ExitHook>);

impl Drop for ExitGuard {
    fn drop(&mut self) {
        if let Some(hook) = self.0.take() {
            hook.call();
        }
    }
}

pub(crate) fn start(
    engine: &CodemodeEngine,
    mut request: ExecuteRequest,
) -> Result<Run, StartError> {
    let guard = ExitGuard(request.on_exit.take());
    check_request(&request)?;
    let timeout = request.timeout();
    let tools: Vec<Value> = request
        .tools
        .iter()
        .map(|tool| {
            let mut entry = Map::new();
            entry.insert("name".to_owned(), Value::String(tool.name.clone()));
            entry.insert(
                "jsName".to_owned(),
                Value::String(
                    tool.js_name
                        .clone()
                        .unwrap_or_else(|| to_codemode_identifier(&tool.name)),
                ),
            );
            entry.insert(
                "description".to_owned(),
                Value::String(tool.description.clone().unwrap_or_default()),
            );
            Value::Object(entry)
        })
        .collect();
    let globals: Vec<Value> = request
        .globals
        .iter()
        .map(|global| {
            let mut entry = Map::new();
            entry.insert("name".to_owned(), Value::String(global.name.clone()));
            entry.insert("spread".to_owned(), Value::Bool(global.spread));
            Value::Object(entry)
        })
        .collect();
    let control = Arc::new(RunControl::new(engine.wasmtime_engine().clone(), timeout));
    let book = Arc::new(Mutex::new(CallBook::default()));
    let (event_sender, events) = unbounded_channel();
    let (commands, command_receiver) = std::sync::mpsc::channel();
    let input = ScriptInput {
        pre: engine.instance_pre().clone(),
        script: Script {
            code: std::mem::take(&mut request.source.code),
            tools_json: Value::Array(tools).to_string(),
            globals_json: Value::Array(globals).to_string(),
            store_json: store_json(&request.store),
        },
        limits: request.limits,
        deterministic: request.deterministic,
        control: Arc::clone(&control),
        events: event_sender.clone(),
        book: Arc::clone(&book),
        commands: command_receiver,
    };
    let thread_book = Arc::clone(&book);
    let thread_control = Arc::clone(&control);
    let thread = std::thread::Builder::new()
        .name("bake-codemode".to_owned())
        .stack_size(SCRIPT_THREAD_STACK_BYTES)
        .spawn(move || {
            let _guard = guard;
            let outcome = run_with_deadline(input, &thread_control);
            lock(&thread_book).finished = true;
            let _ = event_sender.send(Event::Done(Arc::new(outcome)));
        })
        .map_err(|error| StartError::Thread(error.to_string()))?;
    Ok(Run {
        events,
        handle: RunHandle {
            commands,
            book,
            control,
            deterministic: request.deterministic.is_some(),
        },
        thread: Some(thread),
        done: None,
    })
}

/// The deadline watchdog's stack; it only waits on a condition variable.
const WATCHDOG_STACK_BYTES: usize = 64 << 10;

/// Runs the script with a watchdog thread that stops it at its deadline, so
/// the deadline holds while nothing awaits [`Run::next`]. The watchdog is
/// joined before the script thread exits.
fn run_with_deadline(input: ScriptInput, control: &Arc<RunControl>) -> Outcome {
    let watchdog = match control.deadline() {
        None => None,
        Some(_) => {
            let watched = Arc::clone(control);
            let spawned = std::thread::Builder::new()
                .name("bake-codemode-deadline".to_owned())
                .stack_size(WATCHDOG_STACK_BYTES)
                .spawn(move || watched.watch_deadline());
            match spawned {
                Ok(watchdog) => Some(watchdog),
                Err(error) => {
                    return Outcome::failed(Failure::Protocol {
                        message: format!("The script's deadline thread could not start: {error}"),
                    });
                }
            }
        }
    };
    let outcome = run_script(input);
    control.finish();
    if let Some(watchdog) = watchdog {
        let _ = watchdog.join();
    }
    outcome
}

fn check_request(request: &ExecuteRequest) -> Result<(), StartError> {
    let invalid = |message: &str| Err(StartError::InvalidRequest(message.to_owned()));
    for (index, tool) in request.tools.iter().enumerate() {
        if request.tools[..index]
            .iter()
            .any(|other| other.name == tool.name)
        {
            return Err(StartError::InvalidRequest(format!(
                "Tool \"{}\" is already registered",
                tool.name
            )));
        }
    }
    check_globals(&request.globals)?;
    if let Some(mode) = request.deterministic {
        if !mode.started_at_ms.is_finite() || mode.started_at_ms < 0.0 {
            return invalid(
                "startedAt must be a non-negative number of milliseconds since the epoch",
            );
        }
        let names: Vec<String> = request
            .tools
            .iter()
            .map(|tool| {
                tool.js_name
                    .clone()
                    .unwrap_or_else(|| to_codemode_identifier(&tool.name))
            })
            .collect();
        for (index, name) in names.iter().enumerate() {
            if names[..index].contains(name) {
                return invalid("Tool script names must be unique");
            }
        }
    }
    Ok(())
}

impl Run {
    /// The next event: calls and output as the script makes them, then one
    /// [`Event::Done`], which every later call returns again (the same
    /// [`Arc`], not a copy).
    ///
    /// The run's deadline (`timeout_ms`) is enforced by the run's own
    /// threads, whether or not this is awaited; it needs no Tokio timer.
    pub async fn next(&mut self) -> Event {
        if let Some(done) = &self.done {
            return Event::Done(Arc::clone(done));
        }
        let outcome = match self.events.recv().await {
            Some(Event::Done(outcome)) => outcome,
            Some(event) => return event,
            None => Arc::new(Outcome::failed(Failure::Protocol {
                message: "The script thread ended without an outcome".to_owned(),
            })),
        };
        self.done = Some(Arc::clone(&outcome));
        Event::Done(outcome)
    }

    /// A handle that answers calls and aborts the run without owning it.
    pub fn handle(&self) -> RunHandle {
        self.handle.clone()
    }

    /// Answers call `id`; see [`RunHandle::answer`].
    ///
    /// # Errors
    ///
    /// As [`RunHandle::answer`].
    pub fn answer(&self, id: u32, result: CallResult) -> Result<(), AnswerError> {
        self.handle.answer(id, result)
    }

    /// Answers call `id` as result number `seq`; see
    /// [`RunHandle::answer_in_order`].
    ///
    /// # Errors
    ///
    /// As [`RunHandle::answer_in_order`].
    pub fn answer_in_order(
        &self,
        seq: u32,
        now_ms: f64,
        id: u32,
        result: CallResult,
    ) -> Result<(), AnswerError> {
        self.handle.answer_in_order(seq, now_ms, id, result)
    }

    /// Whether the run has its outcome: [`Run::next`] returned
    /// [`Event::Done`], or the thread decided it and calls can no longer be
    /// answered.
    pub fn finished(&self) -> bool {
        self.done.is_some() || lock(&self.handle.book).finished
    }

    /// Aborts the script with an error it cannot catch; see
    /// [`RunHandle::abort`].
    pub fn abort(&self) {
        self.handle.abort();
    }

    /// Aborts the script, reporting `message`; see [`RunHandle::abort_with`].
    pub fn abort_with(&self, message: impl Into<String>) {
        self.handle.abort_with(message);
    }
}

impl RunHandle {
    /// Answers call `id`. Answers for a run that has its outcome are ignored.
    ///
    /// # Errors
    ///
    /// [`AnswerError::NotWaiting`] when the call is not waiting, and
    /// [`AnswerError::WrongMode`] for a deterministic run.
    pub fn answer(&self, id: u32, result: CallResult) -> Result<(), AnswerError> {
        let mut book = lock(&self.book);
        if book.finished {
            return Ok(());
        }
        if self.deterministic {
            return Err(AnswerError::WrongMode);
        }
        if !book.pending.remove(&id) {
            return Err(AnswerError::NotWaiting(id));
        }
        self.send(Command::Answer(Delivery {
            id,
            result,
            order: None,
        }));
        Ok(())
    }

    /// Deterministic mode: answers call `id` as result number `seq`. Results
    /// are numbered 1, 2, ... and applied strictly in that order, whatever
    /// order they arrive in; while the VM processes this one its clock reads
    /// `now_ms`, or the previous reading if that is later. Answers for a run
    /// that has its outcome are ignored.
    ///
    /// # Errors
    ///
    /// Pi's `deliver()` refusals: a clock value that is not finite, a result
    /// number used before or 0, a call that is not waiting, and
    /// [`AnswerError::WrongMode`] for a run that is not deterministic.
    pub fn answer_in_order(
        &self,
        seq: u32,
        now_ms: f64,
        id: u32,
        result: CallResult,
    ) -> Result<(), AnswerError> {
        let mut book = lock(&self.book);
        if book.finished {
            return Ok(());
        }
        if !self.deterministic {
            return Err(AnswerError::WrongMode);
        }
        if !now_ms.is_finite() {
            return Err(AnswerError::ClockNotFinite(seq));
        }
        if seq == 0 || book.delivered.contains(&seq) {
            return Err(AnswerError::SequenceUsed(seq));
        }
        if !book.pending.contains(&id) {
            return Err(AnswerError::NotWaiting(id));
        }
        book.delivered.insert(seq);
        book.pending.remove(&id);
        self.send(Command::Answer(Delivery {
            id,
            result,
            order: Some((seq, now_ms)),
        }));
        Ok(())
    }

    /// Aborts the script with an error it cannot catch; [`Run::next`] then
    /// returns [`Failure::Aborted`] with the message `Execution aborted`
    /// unless the script already ended.
    pub fn abort(&self) {
        self.stop(StopReason::Aborted, None);
    }

    /// [`RunHandle::abort`], reporting `message` instead, as Pi reports an
    /// abort signal's reason. Only the first stop's message counts.
    pub fn abort_with(&self, message: impl Into<String>) {
        self.stop(StopReason::Aborted, Some(message.into()));
    }

    fn stop(&self, reason: StopReason, message: Option<String>) {
        self.control.stop_with(reason, message);
        self.send(Command::Stop);
    }

    fn send(&self, command: Command) {
        // The thread may have ended; nothing then waits for the command.
        let _ = self.commands.send(command);
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.handle.abort();
        let Some(thread) = self.thread.take() else {
            return;
        };
        if thread.is_finished() {
            let _ = thread.join();
            return;
        }
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn_blocking(move || {
                    let _ = thread.join();
                });
            }
            Err(_) => {
                let _ = thread.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::{OutputItem, ToolDeclaration};

    #[tokio::test]
    async fn timeout_settlement_preserves_output_and_the_thread_exits() {
        let bound = Duration::from_secs(5);
        let exits = Arc::new(AtomicUsize::new(0));
        let mut request = ExecuteRequest::new("text(\"partial\"); await tools.hang()");
        request.tools = vec![ToolDeclaration::named("hang")];
        request.source.options.timeout_ms = Some(30_000);
        let observed_exits = Arc::clone(&exits);
        request.on_exit = Some(ExitHook::new(move || {
            observed_exits.fetch_add(1, Ordering::SeqCst);
        }));
        let engine = CodemodeEngine::get().expect("the test engine compiles");
        let mut run = engine.start(request).expect("the fixture starts");
        let output = tokio::time::timeout(bound, run.next()).await;
        assert_eq!(
            output.ok(),
            Some(Event::Output(OutputItem::Text {
                text: "partial".to_owned(),
                console: false,
            }))
        );
        let call = tokio::time::timeout(bound, run.next()).await;
        assert!(matches!(call, Ok(Event::Call(ref call)) if call.name() == "hang"));

        // Deliver the stop after the pending call is observable. Integration
        // tests cover real deadlines; settlement must not depend on the VM
        // starting before a short wall-clock deadline under suite load.
        run.handle.stop(StopReason::TimedOut, None);
        let event = tokio::time::timeout(bound, run.next()).await;
        let Ok(Event::Done(outcome)) = event else {
            panic!("{event:?}");
        };
        assert_eq!(
            outcome.result,
            Err(Failure::Timeout {
                message: "Execution timed out after 30000 ms".to_owned(),
            })
        );
        assert_eq!(
            tokio::time::timeout(bound, run.next()).await.ok(),
            Some(Event::Done(outcome))
        );

        let exited = tokio::time::timeout(bound, async {
            while !run.thread.as_ref().is_some_and(JoinHandle::is_finished) {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(exited.is_ok(), "the script thread did not exit");
        run.thread
            .take()
            .expect("the run owns its thread")
            .join()
            .expect("the script thread exits without panicking");
        assert_eq!(exits.load(Ordering::SeqCst), 1);
    }
}
