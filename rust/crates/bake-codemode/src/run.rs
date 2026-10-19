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

/// The script thread's stack: QuickJS's 512 KiB stack guard and Wasmtime's
/// Wasm stack limit both fit on it, so deep recursion is a catchable
/// `RangeError` and never overflows the host stack.
pub const SCRIPT_THREAD_STACK_BYTES: usize = 8 << 20;

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

/// A running script. It is the only handle to the script's thread: dropping
/// it aborts the script and joins the thread (on a blocking task when a Tokio
/// runtime is current, otherwise in place), so no script outlives its run.
pub struct Run {
    events: UnboundedReceiver<Event>,
    commands: Sender<Command>,
    book: Arc<Mutex<CallBook>>,
    control: Arc<RunControl>,
    deterministic: bool,
    thread: Option<JoinHandle<()>>,
    done: Option<Outcome>,
}

impl std::fmt::Debug for Run {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Run")
            .field("deterministic", &self.deterministic)
            .field("done", &self.done)
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
    let thread = std::thread::Builder::new()
        .name("bake-codemode".to_owned())
        .stack_size(SCRIPT_THREAD_STACK_BYTES)
        .spawn(move || {
            let _guard = guard;
            let outcome = run_script(input);
            lock(&thread_book).finished = true;
            let _ = event_sender.send(Event::Done(outcome));
        })
        .map_err(|error| StartError::Thread(error.to_string()))?;
    Ok(Run {
        events,
        commands,
        book,
        control,
        deterministic: request.deterministic.is_some(),
        thread: Some(thread),
        done: None,
    })
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
    /// [`Event::Done`], which every later call returns again.
    ///
    /// The run's deadline (`timeout_ms`) is enforced here as well as on the
    /// script's thread, so a long built-in such as a backtracking regular
    /// expression stops at it; with a deadline this must be awaited on a
    /// Tokio runtime with the time driver.
    pub async fn next(&mut self) -> Event {
        if let Some(done) = &self.done {
            return Event::Done(done.clone());
        }
        loop {
            let received = match self.control.deadline() {
                Some(deadline) if !self.control.stopped() => {
                    match tokio::time::timeout_at(deadline.into(), self.events.recv()).await {
                        Ok(received) => received,
                        Err(_) => {
                            self.stop(StopReason::TimedOut);
                            continue;
                        }
                    }
                }
                _ => self.events.recv().await,
            };
            return match received {
                Some(Event::Done(outcome)) => {
                    self.done = Some(outcome.clone());
                    Event::Done(outcome)
                }
                Some(event) => event,
                None => {
                    let outcome = Outcome::failed(Failure::Protocol {
                        message: "The script thread ended without an outcome".to_owned(),
                    });
                    self.done = Some(outcome.clone());
                    Event::Done(outcome)
                }
            };
        }
    }

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

    /// Whether the run has its outcome: [`Run::next`] returned
    /// [`Event::Done`], or the thread decided it and calls can no longer be
    /// answered.
    pub fn finished(&self) -> bool {
        self.done.is_some() || lock(&self.book).finished
    }

    /// Aborts the script with an error it cannot catch; [`Run::next`] then
    /// returns [`Failure::Aborted`] unless the script already ended.
    pub fn abort(&self) {
        self.stop(StopReason::Aborted);
    }

    fn stop(&self, reason: StopReason) {
        self.control.stop(reason);
        self.send(Command::Stop);
    }

    fn send(&self, command: Command) {
        // The thread may have ended; nothing then waits for the command.
        let _ = self.commands.send(command);
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.stop(StopReason::Aborted);
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
