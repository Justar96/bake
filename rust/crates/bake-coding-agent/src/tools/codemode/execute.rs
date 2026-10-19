//! Runs one codemode script, Pi v1.3.0's
//! `src/extensions/codemode/execute.ts` (MIT; see the crate's `NOTICE`)
//! without the `models` globals.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use bake_agent::{
    AgentTool, AgentToolCallOutcome, AgentToolResult, AgentToolUpdateCallback, BoxFuture,
    NestedToolCallOptions, NestedToolExecutor, ToolContext,
};
use bake_ai::{AbortController, AbortSignal, ImageContent, TextContent, UserContentBlock};
use bake_codemode::{
    Bm25Ranker, CallError, CallResult, CodemodeEngine, Completion, DEFAULT_TOOL_SEARCH_LIMIT,
    Event, ExecuteRequest, ExitHook, Failure, HostCall, OutputItem, RunHandle, Source,
    ToolDeclaration, create_tool_search_document, discovery_globals, render_tool_sample,
    to_codemode_identifier,
};
use serde_json::{Value, json};
use tokio::task::JoinSet;

use super::{
    CODEMODE_TOOL_NAME, CodemodeHost, CodemodeNamespace, CodemodeNestedCall,
    CodemodeNestedCallStatus, CodemodeStoreEntryData, codemode_details, to_codemode_declaration,
};
use crate::owned_work::WorkGate;
use crate::tools::truncate::format_size;

/// The default token budget of a script's output.
pub const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 10_000;
/// Characters per token when estimating.
const CHARS_PER_TOKEN: usize = 4;
/// Characters of a nested call's arguments in its record.
const ARGS_PREVIEW_CHARS: usize = 200;
/// Characters of a nested call's error in its record.
const ERROR_PREVIEW_CHARS: usize = 500;
/// Scripts that may run at once in one process, Pi's
/// `MAX_CONCURRENT_CODEMODE_WORKERS`: a script holds its slot until its
/// thread has exited.
pub const MAX_CONCURRENT_CODEMODE_SCRIPTS: usize = 2;
/// A script's tool calls that run at once; the rest wait in the order the
/// script made them, so a `Promise.all` over a thousand items does not start
/// a thousand processes. Pi has no limit.
pub const MAX_CONCURRENT_NESTED_CALLS: usize = 16;
/// What an aborted script reports: the message of the reason Pi's aborted
/// signal carries by default.
pub const CODEMODE_ABORT_MESSAGE: &str = "This operation was aborted";
/// The prefix of the files output is saved to.
const OUTPUT_PREFIX: &str = "bake-codemode";

/// Scripts alive in the process, Pi's `activeCodemodeWorkers`.
#[cfg(not(test))]
static ACTIVE_SCRIPTS: AtomicUsize = AtomicUsize::new(0);

/// A count of live scripts that admits at most
/// [`MAX_CONCURRENT_CODEMODE_SCRIPTS`].
#[derive(Clone, Copy)]
pub(super) struct Admission(&'static AtomicUsize);

impl Admission {
    /// The process's count.
    #[cfg(not(test))]
    pub(super) fn process() -> Self {
        Self(&ACTIVE_SCRIPTS)
    }

    /// Unit tests run in parallel on their own threads, so each test
    /// thread counts its own scripts; the admission cases use their own
    /// counts.
    #[cfg(test)]
    pub(super) fn process() -> Self {
        thread_local! {
            static COUNT: &'static AtomicUsize = Box::leak(Box::new(AtomicUsize::new(0)));
        }
        Self(COUNT.with(|count| *count))
    }

    #[cfg(test)]
    pub(super) fn new(count: &'static AtomicUsize) -> Self {
        Self(count)
    }

    /// A slot, released by the returned hook once the script's thread has
    /// exited; `None` at capacity.
    fn acquire(self) -> Option<ExitHook> {
        self.0
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |active| {
                (active < MAX_CONCURRENT_CODEMODE_SCRIPTS).then_some(active + 1)
            })
            .ok()?;
        let count = self.0;
        Some(ExitHook::new(move || {
            count.fetch_sub(1, Ordering::SeqCst);
        }))
    }
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() > max_chars {
        let kept: String = text.chars().take(max_chars - 3).collect();
        format!("{kept}...")
    } else {
        text.to_owned()
    }
}

/// Pi's `previewArgs`.
fn preview_args(args: Option<&Value>) -> String {
    args.map_or_else(String::new, |args| {
        truncate_text(&args.to_string(), ARGS_PREVIEW_CHARS)
    })
}

/// The text blocks of a result, joined by newlines.
fn text_of(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Pi's `toScriptValue`: a tool that declares an output schema resolves to
/// its structured content, also for error results that carry one; any other
/// tool resolves to its text. Other failures reject with the tool's error
/// text.
fn to_script_value(tool: &AgentTool, outcome: &AgentToolCallOutcome) -> CallResult {
    if tool.output_schema.is_some()
        && let Some(structured) = &outcome.result.structured_content
    {
        return Ok(Some(structured.clone()));
    }
    let text = text_of(&outcome.result);
    if outcome.is_error {
        return Err(CallError::new(if text.is_empty() {
            format!("Tool \"{}\" failed", tool.name)
        } else {
            text
        }));
    }
    Ok(Some(Value::String(text)))
}

/// A script's output item after layout: text or an image.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Item {
    Text(String),
    Image(ImageContent),
}

/// Pi's `valueText`: strings as they are, other values as compact JSON.
fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Pi's `formatOutput`: with more than one text item (`text()` or the
/// returned value), each starts with a `==> text N/M <==` line; `console.*`
/// lines follow all other output in one `<console_output>` block.
pub(super) fn format_output(output: &[OutputItem]) -> Vec<Item> {
    let total = output
        .iter()
        .filter(|item| matches!(item, OutputItem::Text { console: false, .. }))
        .count();
    let mut items = Vec::new();
    let mut console_lines: Vec<&str> = Vec::new();
    let mut index = 0;
    for item in output {
        match item {
            OutputItem::Image { data, mime_type } => items.push(Item::Image(ImageContent {
                data: data.clone(),
                mime_type: mime_type.clone(),
            })),
            OutputItem::Text {
                text,
                console: true,
            } => console_lines.push(text),
            OutputItem::Text {
                text,
                console: false,
            } => {
                index += 1;
                items.push(Item::Text(if total > 1 {
                    format!("==> text {index}/{total} <==\n{text}")
                } else {
                    text.clone()
                }));
            }
        }
    }
    if !console_lines.is_empty() {
        items.push(Item::Text(format!(
            "<console_output>\n{}\n</console_output>",
            console_lines.join("\n")
        )));
    }
    items
}

/// Pi's `joinAdjacentText`: adjacent text items become one, each part
/// starting on its own line.
pub(super) fn join_adjacent_text(items: Vec<Item>) -> Vec<Item> {
    let mut joined = Vec::new();
    let mut text = String::new();
    let mut parts = 0;
    let mut last_part_ends_line = true;
    let flush = |joined: &mut Vec<Item>, text: &mut String, parts: &mut usize| {
        if *parts > 0 {
            joined.push(Item::Text(std::mem::take(text)));
        }
        *parts = 0;
    };
    for item in items {
        match item {
            Item::Image(_) => {
                flush(&mut joined, &mut text, &mut parts);
                last_part_ends_line = true;
                joined.push(item);
            }
            Item::Text(part) => {
                if !last_part_ends_line {
                    text.push('\n');
                    last_part_ends_line = true;
                }
                if !part.is_empty() {
                    last_part_ends_line = part.ends_with('\n');
                }
                text.push_str(&part);
                parts += 1;
            }
        }
    }
    flush(&mut joined, &mut text, &mut parts);
    joined
}

fn format_call_summary(calls: &[CodemodeNestedCall]) -> String {
    if calls.is_empty() {
        return "No tool calls were made.".to_owned();
    }
    format!(
        "Tool calls made before the failure (they are not undone): {}",
        calls
            .iter()
            .map(|call| format!("{} ({})", call.name, call.status.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Pi's `formatError`.
pub(super) fn format_error(failure: &Failure, calls: &[CodemodeNestedCall]) -> String {
    let head = match failure {
        Failure::Exception(error) | Failure::NeverSettles(error) => {
            error.stack.clone().unwrap_or_else(|| {
                format!(
                    "{}: {}",
                    error.name.as_deref().unwrap_or("Error"),
                    error.message
                )
            })
        }
        Failure::Timeout { message } => format!("Script timed out: {message}"),
        Failure::Aborted { message } => format!("Script aborted: {message}"),
        other => format!("Script sandbox failed: {}", other.message()),
    };
    format!("{head}\n\n{}", format_call_summary(calls))
}

/// Writes `bytes` to a new private file, on `work`'s blocking pool when
/// given.
async fn write_output_file(
    work: Option<&WorkGate>,
    extension: &'static str,
    bytes: Vec<u8>,
) -> Result<PathBuf, String> {
    let write = move || {
        let (path, mut file) = crate::tools::output_files::create_with_extension(
            &std::env::temp_dir(),
            OUTPUT_PREFIX,
            extension,
        )?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok::<PathBuf, std::io::Error>(path)
    };
    let written = match work {
        Some(work) => work
            .spawn_blocking(write)
            .map_err(|refused| refused.to_string())?
            .await
            .map_err(|error| error.to_string())?,
        None => tokio::task::spawn_blocking(write)
            .await
            .map_err(|error| error.to_string())?,
    };
    written.map_err(|error| error.to_string())
}

/// The file extensions of the image types `image()` accepts.
fn image_extension(mime_type: &str) -> Option<&'static str> {
    match mime_type {
        "image/png" => Some(".png"),
        "image/jpeg" => Some(".jpg"),
        "image/gif" => Some(".gif"),
        "image/webp" => Some(".webp"),
        _ => None,
    }
}

/// Pi's `saveImages`: each image is saved to a temp file and a text item
/// with its path goes before it; an image shown more than once is saved
/// once. A failed write becomes part of the label.
pub(super) async fn save_images(items: Vec<Item>, work: Option<&WorkGate>) -> Vec<Item> {
    let mut labels: HashMap<String, String> = HashMap::new();
    let mut saved = Vec::with_capacity(items.len());
    for item in items {
        let Item::Image(image) = &item else {
            saved.push(item);
            continue;
        };
        let label = match labels.get(&image.data) {
            Some(label) => label.clone(),
            None => {
                let bytes = crate::mcp::tools::decode_base64(&image.data);
                let kind = format!("{}, {}", image.mime_type, format_size(bytes.len()));
                let label = match image_extension(&image.mime_type) {
                    None => format!(
                        "[Image ({kind}) could not be saved: No file extension for image type {}]",
                        image.mime_type
                    ),
                    Some(extension) => match write_output_file(work, extension, bytes).await {
                        Ok(path) => format!("[Image saved to {} ({kind})]", path.display()),
                        Err(error) => format!("[Image ({kind}) could not be saved: {error}]"),
                    },
                };
                labels.insert(image.data.clone(), label.clone());
                label
            }
        };
        saved.push(Item::Text(label));
        saved.push(item);
    }
    saved
}

/// Pi's `truncateOutput`: when the combined text passes the budget, the
/// text items become one item that keeps the start and the end, images
/// follow it, and the full text is written to a temp file.
pub(super) async fn truncate_output(
    items: Vec<Item>,
    max_tokens: u64,
    work: Option<&WorkGate>,
) -> (Vec<Item>, Option<String>) {
    let texts: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            Item::Text(text) => Some(text.as_str()),
            Item::Image(_) => None,
        })
        .collect();
    let combined = texts.join("\n");
    let length = combined.chars().count();
    let budget = usize::try_from(max_tokens)
        .unwrap_or(usize::MAX)
        .saturating_mul(CHARS_PER_TOKEN);
    if texts.is_empty() || length <= budget {
        return (items, None);
    }
    let head_chars = budget / 2;
    let tail_chars = budget - head_chars;
    let removed = length - head_chars - tail_chars;
    let head: String = combined.chars().take(head_chars).collect();
    let tail: String = combined.chars().skip(length - tail_chars).collect();
    let mut text = format!(
        "Warning: truncated output (original token count: {})\nTotal output lines: {}\n\n{head}…{} tokens truncated…{tail}",
        length.div_ceil(CHARS_PER_TOKEN),
        combined.split('\n').count(),
        removed.div_ceil(CHARS_PER_TOKEN),
    );
    let spilled = write_output_file(work, ".txt", combined.into_bytes()).await;
    let full_output_path = match spilled {
        Ok(path) => {
            let path = path.display().to_string();
            text.push_str(&format!(
                "\n\n[Full output: {path} (read with offset/limit)]"
            ));
            Some(path)
        }
        Err(error) => {
            text.push_str(&format!("\n\n[Could not save the full output: {error}]"));
            None
        }
    };
    let mut truncated = vec![Item::Text(text)];
    truncated.extend(
        items
            .into_iter()
            .filter(|item| matches!(item, Item::Image(_))),
    );
    (truncated, full_output_path)
}

/// Pi's `isNamespaceName`: the namespace's name or one of its aliases, as
/// written or as the same script identifier.
fn is_namespace_name(namespace: &CodemodeNamespace, query: &str) -> bool {
    let query_id = to_codemode_identifier(query);
    std::iter::once(&namespace.namespace.name)
        .chain(&namespace.aliases)
        .any(|name| name == query || to_codemode_identifier(name) == query_id)
}

/// The tools a script may call and what it sees of them.
struct Catalog {
    tools: Vec<Arc<AgentTool>>,
    samples: HashMap<String, String>,
    host: Option<Arc<dyn CodemodeHost>>,
}

impl Catalog {
    fn namespace(&self, name: &str) -> Option<CodemodeNamespace> {
        self.host.as_ref()?.tool_namespace(name)
    }

    fn entry(&self, name: &str) -> Value {
        json!({
            "name": to_codemode_identifier(name),
            "description": self.samples.get(name).cloned().unwrap_or_default(),
        })
    }

    /// Pi's discovery globals: `searchTools()`, `describeTool()`, and
    /// `describeNamespace()`, each given its arguments as an array.
    fn answer_global(&self, name: &str, args: Option<&Value>) -> CallResult {
        let args: &[Value] = args.and_then(Value::as_array).map_or(&[], Vec::as_slice);
        let arg = |index: usize| args.get(index).filter(|value| !value.is_null());
        match name {
            "searchTools" => {
                let Some(Value::String(query)) = arg(0) else {
                    return Err(CallError::new("searchTools() expects a query string"));
                };
                let options = arg(1);
                let limit = match options.and_then(|options| options.get("limit")) {
                    None | Some(Value::Null) => DEFAULT_TOOL_SEARCH_LIMIT,
                    Some(limit) => match limit.as_f64() {
                        Some(limit) if limit.fract() == 0.0 && limit > 0.0 => {
                            if limit >= usize::MAX as f64 {
                                usize::MAX
                            } else {
                                limit as usize
                            }
                        }
                        _ => {
                            return Err(CallError::new(
                                "searchTools() limit must be a positive integer",
                            ));
                        }
                    },
                };
                let namespace = match options.and_then(|options| options.get("namespace")) {
                    None | Some(Value::Null) => None,
                    Some(Value::String(namespace)) => Some(namespace.as_str()),
                    Some(_) => {
                        return Err(CallError::new("searchTools() namespace must be a string"));
                    }
                };
                let documents: Vec<_> = self
                    .tools
                    .iter()
                    .filter_map(|tool| {
                        let tool_namespace = self.namespace(&tool.name);
                        if let Some(wanted) = namespace.filter(|wanted| !wanted.is_empty())
                            && !tool_namespace
                                .as_ref()
                                .is_some_and(|known| is_namespace_name(known, wanted))
                        {
                            return None;
                        }
                        Some(create_tool_search_document(
                            &tool.name,
                            &tool.description,
                            Some(&tool.parameters),
                            tool_namespace.as_ref().map(|known| &known.namespace),
                        ))
                    })
                    .collect();
                let matches = Bm25Ranker::default().rank(query, &documents, limit);
                Ok(Some(Value::Array(
                    matches
                        .iter()
                        .map(|found| self.entry(&found.name))
                        .collect(),
                )))
            }
            "describeTool" => {
                let Some(Value::String(name)) = arg(0) else {
                    return Err(CallError::new("describeTool() expects a tool name"));
                };
                Ok(self
                    .tools
                    .iter()
                    .find(|tool| &tool.name == name || &to_codemode_identifier(&tool.name) == name)
                    .and_then(|tool| self.samples.get(&tool.name))
                    .map(|sample| Value::String(sample.clone())))
            }
            "describeNamespace" => {
                let Some(Value::String(name)) = arg(0) else {
                    return Err(CallError::new(
                        "describeNamespace() expects a namespace name",
                    ));
                };
                let mut found: Option<CodemodeNamespace> = None;
                let mut names = Vec::new();
                for tool in &self.tools {
                    let Some(namespace) = self.namespace(&tool.name) else {
                        continue;
                    };
                    if !is_namespace_name(&namespace, name) {
                        continue;
                    }
                    found.get_or_insert(namespace);
                    names.push(Value::String(to_codemode_identifier(&tool.name)));
                }
                Ok(found.map(|namespace| {
                    let mut described = json!({ "name": namespace.namespace.name });
                    if let Some(description) = namespace
                        .namespace
                        .description
                        .filter(|description| !description.is_empty())
                    {
                        described["description"] = json!(description);
                    }
                    if let Some(instructions) = namespace
                        .namespace
                        .instructions
                        .filter(|instructions| !instructions.is_empty())
                    {
                        described["instructions"] = json!(instructions);
                    }
                    described["tools"] = Value::Array(names);
                    described
                }))
            }
            other => Err(CallError::new(format!("Unknown global \"{other}\""))),
        }
    }
}

/// The records of a script's nested calls, published as partial results.
#[derive(Clone)]
struct Records {
    /// The codemode call's id, the prefix of a record's id until the
    /// nested call has its own.
    caller_id: String,
    calls: Arc<Mutex<Vec<CodemodeNestedCall>>>,
    on_update: AgentToolUpdateCallback,
}

impl Records {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<CodemodeNestedCall>> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn snapshot(&self) -> Vec<CodemodeNestedCall> {
        self.lock().clone()
    }

    /// Pi's `publish`: the records so far as a partial result.
    fn publish(&self) {
        let details = codemode_details(&self.snapshot(), None);
        self.on_update.update(AgentToolResult {
            details: Some(details),
            ..AgentToolResult::default()
        });
    }
}

/// A tool call the script made that has not started.
struct QueuedCall {
    id: u32,
    tool: Arc<AgentTool>,
    args: Option<Value>,
}

/// A nested call that has started: what answering the script and
/// recording it need.
struct StartedCall {
    id: u32,
    index: usize,
    tool: Arc<AgentTool>,
    started: Instant,
}

/// Records a call as it starts, in the order calls start, and publishes
/// the records; returns its place.
fn record_start(records: &Records, call: &QueuedCall) -> usize {
    let index = {
        let mut calls = records.lock();
        calls.push(CodemodeNestedCall {
            id: format!("{}/?", records.caller_id),
            name: call.tool.name.clone(),
            args: preview_args(call.args.as_ref()),
            status: CodemodeNestedCallStatus::Running,
            duration_ms: None,
            error: None,
        });
        calls.len() - 1
    };
    records.publish();
    index
}

/// Waits for one started nested call, recorded at `index`, and answers the
/// script. A failed call is `cancelled` when its own `signal` or the
/// script's aborted: on Esc the call also stops on the script's signal,
/// and may settle before the script aborts its controller.
async fn settle_nested_call(
    outcome: BoxFuture<'static, AgentToolCallOutcome>,
    call: StartedCall,
    signal: AbortSignal,
    script_signal: Option<AbortSignal>,
    handle: RunHandle,
    records: Records,
) {
    let StartedCall {
        id,
        index,
        tool,
        started,
    } = call;
    let outcome = outcome.await;
    {
        let mut calls = records.lock();
        if let Some(record) = calls.get_mut(index) {
            record.id = outcome.tool_call.id.clone();
            record.duration_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
            if outcome.is_error {
                record.status = if signal.aborted()
                    || script_signal.as_ref().is_some_and(AbortSignal::aborted)
                {
                    CodemodeNestedCallStatus::Cancelled
                } else {
                    CodemodeNestedCallStatus::Error
                };
                let text = text_of(&outcome.result);
                record.error = Some(truncate_text(
                    &if text.is_empty() {
                        format!("Tool \"{}\" failed", tool.name)
                    } else {
                        text
                    },
                    ERROR_PREVIEW_CHARS,
                ));
            } else {
                record.status = CodemodeNestedCallStatus::Ok;
            }
        }
    }
    records.publish();
    // The run may have ended meanwhile; its answers are then ignored.
    let _ = handle.answer(id, to_script_value(&tool, &outcome));
}

/// The engine, compiled on the first script off the async workers.
async fn engine(work: Option<&WorkGate>) -> Result<&'static CodemodeEngine, String> {
    static COMPILED: OnceLock<&'static CodemodeEngine> = OnceLock::new();
    if let Some(engine) = COMPILED.get() {
        return Ok(engine);
    }
    let compile = || CodemodeEngine::get().map_err(|error| error.to_string());
    let compiled = match work {
        Some(work) => work
            .spawn_blocking(compile)
            .map_err(|refused| refused.to_string())?
            .await
            .map_err(|error| error.to_string())?,
        None => tokio::task::spawn_blocking(compile)
            .await
            .map_err(|error| error.to_string())?,
    }?;
    Ok(*COMPILED.get_or_init(|| compiled))
}

/// What ran: the outcome, the output, and the nested calls.
struct Ran {
    result: Result<Completion, Failure>,
    output: Vec<OutputItem>,
}

/// Starts the script and serves its calls until it ends.
async fn run_script(
    source: Source,
    catalog: &Catalog,
    signal: Option<&AbortSignal>,
    nested: &NestedToolExecutor,
    records: &Records,
    admission: Admission,
) -> Ran {
    let failed = |failure| Ran {
        result: Err(failure),
        output: Vec::new(),
    };
    let aborted = || Failure::Aborted {
        message: CODEMODE_ABORT_MESSAGE.to_owned(),
    };
    let work = catalog.host.as_ref().and_then(|host| host.work());
    let engine = match engine(work.as_ref()).await {
        Ok(engine) => engine,
        Err(message) => return failed(Failure::Protocol { message }),
    };
    let store = match &catalog.host {
        Some(host) => match host.load_store().await {
            Ok(store) => store,
            Err(message) => return failed(Failure::Protocol { message }),
        },
        None => Default::default(),
    };
    if signal.is_some_and(AbortSignal::aborted) {
        return failed(aborted());
    }
    let Some(on_exit) = admission.acquire() else {
        return failed(Failure::Protocol {
            message: format!(
                "Codemode worker limit reached ({MAX_CONCURRENT_CODEMODE_SCRIPTS} workers are still running or shutting down). Retry after they exit."
            ),
        });
    };
    let mut request = ExecuteRequest::new("");
    request.source = source;
    request.tools = catalog
        .tools
        .iter()
        .map(|tool| ToolDeclaration {
            name: tool.name.clone(),
            js_name: None,
            description: catalog.samples.get(&tool.name).cloned(),
        })
        .collect();
    request.globals = discovery_globals();
    request.store = store;
    request.on_exit = Some(on_exit);
    let mut run = match engine.start(request) {
        Ok(run) => run,
        Err(error) => {
            return failed(Failure::Protocol {
                message: error.to_string(),
            });
        }
    };

    let handle = run.handle();
    let mut output = Vec::new();
    let mut queue: VecDeque<QueuedCall> = VecDeque::new();
    let mut running: JoinSet<()> = JoinSet::new();
    let mut controllers: Vec<AbortController> = Vec::new();
    let mut stopping = false;
    let cancelled = async {
        match signal {
            Some(signal) => signal.cancelled().await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(cancelled);
    let outcome = loop {
        // Start waiting calls while there is room, in the order made.
        while !stopping && running.len() < MAX_CONCURRENT_NESTED_CALLS {
            let Some(call) = queue.pop_front() else {
                break;
            };
            let controller = AbortController::new();
            let index = record_start(records, &call);
            let QueuedCall { id, tool, args } = call;
            let started = Instant::now();
            // Started here, not in the task, so nested ids follow the order
            // the script made its calls.
            let outcome = nested.start(
                &tool.name,
                args.unwrap_or(Value::Null),
                NestedToolCallOptions {
                    signal: Some(controller.signal()),
                    on_update: None,
                },
            );
            running.spawn(settle_nested_call(
                outcome,
                StartedCall {
                    id,
                    index,
                    tool,
                    started,
                },
                controller.signal(),
                signal.cloned(),
                handle.clone(),
                records.clone(),
            ));
            controllers.push(controller);
        }
        tokio::select! {
            event = run.next() => match event {
                Event::Call(HostCall::Tool { id, name, args }) => {
                    match catalog.tools.iter().find(|tool| tool.name == name) {
                        Some(tool) => queue.push_back(QueuedCall { id, tool: Arc::clone(tool), args }),
                        None => {
                            let _ = handle.answer(id, Err(CallError::new(format!("Tool {name} not found"))));
                        }
                    }
                }
                Event::Call(HostCall::Global { id, name, args }) => {
                    let _ = handle.answer(id, catalog.answer_global(&name, args.as_ref()));
                }
                Event::Output(item) => output.push(item),
                Event::Done(outcome) => break outcome,
                Event::Started { .. } | Event::Idle { .. } => {}
            },
            () = &mut cancelled, if !stopping => {
                stopping = true;
                run.abort_with(CODEMODE_ABORT_MESSAGE);
                // Pi's `finish()`: every running call is cancelled with the
                // script, not after it.
                for controller in &controllers {
                    controller.abort();
                }
            }
            Some(_) = running.join_next(), if !running.is_empty() => {}
        }
    };
    // Calls still running when the script ends are cancelled and awaited;
    // waiting ones never start.
    queue.clear();
    for controller in &controllers {
        controller.abort();
    }
    while running.join_next().await.is_some() {}
    drop(run);
    Ran {
        result: outcome.result.clone(),
        output,
    }
}

/// Pi's `executeCodemode`: runs one script. Without a session (a detached
/// context or no host) scripts cannot call tools, the store starts empty,
/// and writes are dropped.
pub(super) async fn execute_codemode(
    tool_call_id: String,
    params: Value,
    signal: Option<AbortSignal>,
    on_update: AgentToolUpdateCallback,
    ctx: ToolContext,
    host: Option<Arc<dyn CodemodeHost>>,
) -> Result<AgentToolResult, String> {
    execute_with(
        tool_call_id,
        params,
        signal,
        on_update,
        ctx,
        host,
        Admission::process(),
    )
    .await
}

/// [`execute_codemode`] with the given admission count.
pub(super) async fn execute_with(
    tool_call_id: String,
    params: Value,
    signal: Option<AbortSignal>,
    on_update: AgentToolUpdateCallback,
    ctx: ToolContext,
    host: Option<Arc<dyn CodemodeHost>>,
    admission: Admission,
) -> Result<AgentToolResult, String> {
    let started = Instant::now();
    let code = params.get("code").and_then(Value::as_str).unwrap_or("");
    let source = Source::parse(code).map_err(|error| error.to_string())?;
    let max_output_tokens = source
        .options
        .max_output_tokens
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
    let records = Records {
        caller_id: tool_call_id,
        calls: Arc::default(),
        on_update,
    };
    // Show the script as running before its first nested call.
    records.publish();

    let nested = ctx.nested().clone();
    let tools: Vec<Arc<AgentTool>> = nested
        .tools()
        .await
        .into_iter()
        .filter(|tool| tool.name != CODEMODE_TOOL_NAME)
        .collect();
    let samples = tools
        .iter()
        .map(|tool| {
            let guidelines = host
                .as_ref()
                .map(|host| host.tool_guidelines(&tool.name))
                .unwrap_or_default();
            (
                tool.name.clone(),
                render_tool_sample(&to_codemode_declaration(tool, &guidelines), None),
            )
        })
        .collect();
    let catalog = Catalog {
        tools,
        samples,
        host,
    };

    let ran = run_script(
        source,
        &catalog,
        signal.as_ref(),
        &nested,
        &records,
        admission,
    )
    .await;
    let mut calls = records.snapshot();
    for call in &mut calls {
        if call.status == CodemodeNestedCallStatus::Running {
            call.status = CodemodeNestedCallStatus::Cancelled;
        }
    }

    let mut script_output = ran.output;
    if let Ok(completion) = &ran.result {
        let writes = &completion.store_writes;
        if (!writes.set.is_empty() || !writes.delete.is_empty())
            && let Some(host) = &catalog.host
        {
            host.append_store(CodemodeStoreEntryData {
                set: writes.set.clone(),
                delete: writes.delete.clone(),
            })
            .await?;
        }
        // Pi's extension: a returned value is appended like `text()`.
        if let Some(value) = &completion.value {
            script_output.push(OutputItem::Text {
                text: value_text(value),
                console: false,
            });
        }
    }
    let mut items = format_output(&script_output);
    if let Err(failure) = &ran.result {
        items.push(Item::Text(format!(
            "Script error:\n{}",
            format_error(failure, &calls)
        )));
    }
    let work = catalog.host.as_ref().and_then(|host| host.work());
    let (truncated, full_output_path) =
        truncate_output(join_adjacent_text(items), max_output_tokens, work.as_ref()).await;
    // After truncation, which joins the text items and moves images after
    // them, so each path stays next to its image and is never cut.
    let output = join_adjacent_text(save_images(truncated, work.as_ref()).await);
    let header = format!(
        "{}\nWall time {:.1} seconds\nOutput:\n",
        if ran.result.is_ok() {
            "Script completed"
        } else {
            "Script failed"
        },
        started.elapsed().as_secs_f64()
    );
    let mut content = vec![UserContentBlock::Text(TextContent::new(header))];
    content.extend(output.into_iter().map(|item| match item {
        Item::Text(text) => UserContentBlock::Text(TextContent::new(text)),
        Item::Image(image) => UserContentBlock::Image(image),
    }));
    Ok(AgentToolResult {
        content,
        details: Some(codemode_details(&calls, full_output_path.as_deref())),
        is_error: ran.result.is_err(),
        ..AgentToolResult::default()
    })
}
