//! A host for tests: Pi's `CodemodeSandbox.execute()` over a [`Run`], which
//! executes the script's calls with in-process tools and collects its output
//! and call records, as Pi's `runtime/execution.ts` does.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bake_codemode::{
    AnswerError, CallError, CodemodeEngine, Completion, Event, ExecuteRequest, Failure,
    GlobalDeclaration, HostCall, Limits, Outcome, OutputItem, Run, ScriptError, StoreValues,
    ToolDeclaration,
};
use serde_json::Value;
use tokio::task::JoinSet;

/// Every wait in these tests is bounded.
pub const BOUND: Duration = Duration::from_secs(60);

pub fn engine() -> &'static CodemodeEngine {
    match CodemodeEngine::get() {
        Ok(engine) => engine,
        Err(error) => panic!("{error}"),
    }
}

pub type ToolFuture = Pin<Box<dyn Future<Output = Result<Option<Value>, String>> + Send>>;
/// A tool's body: its arguments and a flag set when the run cancels the call.
pub type Execute = Arc<dyn Fn(Option<Value>, Arc<AtomicBool>) -> ToolFuture + Send + Sync>;

#[derive(Clone)]
pub struct Tool {
    pub name: String,
    pub description: Option<String>,
    pub execute: Execute,
}

/// A tool that answers at once.
pub fn tool(
    name: &str,
    body: impl Fn(Option<Value>) -> Result<Option<Value>, String> + Send + Sync + 'static,
) -> Tool {
    let body = Arc::new(body);
    Tool {
        name: name.to_owned(),
        description: None,
        execute: Arc::new(move |args, _| {
            let body = Arc::clone(&body);
            Box::pin(async move { body(args) })
        }),
    }
}

/// A tool whose body is a future.
pub fn async_tool<F>(
    name: &str,
    body: impl Fn(Option<Value>, Arc<AtomicBool>) -> F + Send + Sync + 'static,
) -> Tool
where
    F: Future<Output = Result<Option<Value>, String>> + Send + 'static,
{
    Tool {
        name: name.to_owned(),
        description: None,
        execute: Arc::new(move |args, cancelled| Box::pin(body(args, cancelled))),
    }
}

pub fn echo() -> Tool {
    tool("echo", Ok)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallStatus {
    Ok,
    Error,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct CallRecord {
    pub name: String,
    pub status: CallStatus,
}

#[derive(Default)]
pub struct Setup {
    pub tools: Vec<Tool>,
    pub globals: Vec<(GlobalDeclaration, Execute)>,
    pub timeout_ms: Option<u64>,
    pub store: StoreValues,
    pub limits: Option<Limits>,
    /// Holds every call's task alive after it has answered, so
    /// [`Event::Done`] always arrives before the loop could join it: the
    /// seam that proves a record settles with its answer and not with its
    /// task.
    pub hold_answered_calls: bool,
}

impl Setup {
    pub fn tools(tools: Vec<Tool>) -> Self {
        Self {
            tools,
            ..Self::default()
        }
    }
}

/// A poisoned record list is still the records, so a panicking call's test
/// reports its own assertion instead of a lock error.
fn lock<T>(records: &Mutex<T>) -> MutexGuard<'_, T> {
    records.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug)]
pub struct Execution {
    pub outcome: Outcome,
    pub output: Vec<OutputItem>,
    pub calls: Vec<CallRecord>,
}

impl Execution {
    pub fn completion(&self) -> &Completion {
        match &self.outcome.result {
            Ok(completion) => completion,
            Err(failure) => panic!("the script failed: {failure:?}, output {:?}", self.output),
        }
    }

    pub fn value(&self) -> Option<&Value> {
        self.completion().value.as_ref()
    }

    pub fn failure(&self) -> &Failure {
        match &self.outcome.result {
            Ok(completion) => panic!("the script completed: {completion:?}"),
            Err(failure) => failure,
        }
    }

    /// The script error of a script that threw.
    pub fn exception(&self) -> &ScriptError {
        match self.failure() {
            Failure::Exception(error) => error,
            other => panic!("not a script error: {other:?}"),
        }
    }

    pub fn texts(&self) -> Vec<&str> {
        self.output
            .iter()
            .filter_map(|item| match item {
                OutputItem::Text { text, .. } => Some(text.as_str()),
                OutputItem::Image { .. } => None,
            })
            .collect()
    }
}

pub fn text(text: &str) -> OutputItem {
    OutputItem::Text {
        text: text.to_owned(),
        console: false,
    }
}

pub fn console(text: &str) -> OutputItem {
    OutputItem::Text {
        text: text.to_owned(),
        console: true,
    }
}

pub fn image(data: &str, mime_type: &str) -> OutputItem {
    OutputItem::Image {
        data: data.to_owned(),
        mime_type: mime_type.to_owned(),
    }
}

pub fn start(code: &str, setup: &Setup) -> Run {
    let mut request = ExecuteRequest::new(code);
    request.source.options.timeout_ms = setup.timeout_ms;
    request.tools = setup
        .tools
        .iter()
        .map(|tool| ToolDeclaration {
            name: tool.name.clone(),
            js_name: None,
            description: tool.description.clone(),
        })
        .collect();
    request.globals = setup
        .globals
        .iter()
        .map(|(global, _)| global.clone())
        .collect();
    request.store = setup.store.clone();
    if let Some(limits) = setup.limits {
        request.limits = limits;
    }
    match engine().start(request) {
        Ok(run) => run,
        Err(error) => panic!("the run did not start: {error}"),
    }
}

/// Runs `code` to its end, executing its calls with `setup`'s tools and
/// globals, within [`BOUND`].
pub async fn execute(code: &str, setup: Setup) -> Execution {
    match tokio::time::timeout(BOUND, execute_unbounded(code, &setup)).await {
        Ok(execution) => execution,
        Err(_) => panic!("the script did not end within {BOUND:?}: {code}"),
    }
}

async fn execute_unbounded(code: &str, setup: &Setup) -> Execution {
    let mut run = start(code, setup);
    // Each call's task answers through its own handle, as a host whose
    // nested calls run apart from the loop that owns the run would.
    let mut tasks: JoinSet<(u32, Result<(), AnswerError>)> = JoinSet::new();
    let mut output = Vec::new();
    let calls: Arc<Mutex<Vec<CallRecord>>> = Arc::default();
    let mut flags: HashMap<u32, Arc<AtomicBool>> = HashMap::new();
    loop {
        tokio::select! {
            event = run.next() => match event {
                Event::Call(call) => {
                    let (execute, id, args, record) = match &call {
                        HostCall::Tool { id, name, args } => {
                            let mut records = lock(&calls);
                            records.push(CallRecord { name: name.clone(), status: CallStatus::Cancelled });
                            let index = records.len() - 1;
                            drop(records);
                            (setup.tools.iter().find(|tool| &tool.name == name).map(|tool| Arc::clone(&tool.execute)), *id, args.clone(), Some(index))
                        }
                        HostCall::Global { id, name, args } => (
                            setup.globals.iter().find(|(global, _)| &global.name == name).map(|(_, execute)| Arc::clone(execute)),
                            *id,
                            args.clone(),
                            None,
                        ),
                    };
                    let cancelled = Arc::new(AtomicBool::new(false));
                    flags.insert(id, Arc::clone(&cancelled));
                    let name = call.name().to_owned();
                    let handle = run.handle();
                    let records = Arc::clone(&calls);
                    let hold = setup.hold_answered_calls;
                    tasks.spawn(async move {
                        let result = match execute {
                            Some(execute) => execute(args, cancelled).await,
                            None => Err(format!("Unknown tool \"{name}\"")),
                        };
                        // The record settles before the answer, as the
                        // codemode tool's `settle_nested_call` does: the
                        // script may end as soon as it has the result, and
                        // the `Event::Done` that follows reports the records
                        // as they stand.
                        if let Some(index) = record
                            && let Some(call) = lock(&records).get_mut(index)
                        {
                            call.status = if result.is_ok() { CallStatus::Ok } else { CallStatus::Error };
                        }
                        let answer = handle.answer(id, result.map_err(CallError::new));
                        if hold {
                            std::future::pending::<()>().await;
                        }
                        (id, answer)
                    });
                }
                Event::Output(item) => output.push(item),
                Event::Done(outcome) => {
                    for flag in flags.values() {
                        flag.store(true, Ordering::SeqCst);
                    }
                    tasks.abort_all();
                    let outcome = Arc::unwrap_or_clone(outcome);
                    let calls = lock(&calls).clone();
                    return Execution { outcome, output, calls };
                }
                Event::Started { .. } | Event::Idle { .. } => {}
            },
            Some(joined) = tasks.join_next(), if !tasks.is_empty() => {
                let Ok((id, answer)) = joined else { continue };
                flags.remove(&id);
                assert!(answer.is_ok(), "{answer:?}");
            }
        }
    }
}

/// `execute` with no tools.
pub async fn run(code: &str) -> Execution {
    execute(code, Setup::default()).await
}

pub fn json(text: &str) -> Value {
    match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => panic!("{text}: {error}"),
    }
}
