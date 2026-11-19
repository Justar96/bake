//! Runs one codemode script, Pi v1.3.0's
//! `src/extensions/codemode/execute.ts` (MIT; see the crate's `NOTICE`)
//! without the `models` globals, with v1.4.0's queue for a free script
//! slot.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use bake_agent::{
    AgentTool, AgentToolCallOutcome, AgentToolResult, AgentToolUpdateCallback, BoxFuture,
    NestedToolCallOptions, NestedToolExecutor, ToolContext,
};
use bake_ai::{AbortController, AbortSignal, ImageContent, TextContent, UserContentBlock};
use bake_codemode::{
    Bm25Ranker, CallError, CallResult, CodemodeEngine, Completion, DEFAULT_TOOL_SEARCH_LIMIT,
    Event, ExecuteRequest, ExitHook, Failure, GlobalDeclaration, HostCall, OutputItem, RunHandle,
    Source, ToolDeclaration, create_tool_search_document, discovery_globals, render_tool_sample,
    to_codemode_identifier,
};
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use super::{
    CODEMODE_TOOL_NAME, CODEMODE_WAITING_DETAIL, CodemodeCallCounts, CodemodeHost,
    CodemodeNamespace, CodemodeNestedCall, CodemodeNestedCallStatus, CodemodeStoreEntryData,
    codemode_details, to_codemode_declaration,
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
/// Scripts that may run at once in one process by default, Pi's
/// `DEFAULT_MAX_CODEMODE_WORKERS`: a script holds its slot until its thread
/// has exited.
pub const MAX_CONCURRENT_CODEMODE_SCRIPTS: usize = 2;
/// Where Pi reads `PI_CODEMODE_MAX_WORKERS`, Bake reads this.
pub const BAKE_CODEMODE_MAX_WORKERS_ENV: &str = "BAKE_CODEMODE_MAX_WORKERS";
/// How long a script without `timeout_ms` waits for a free slot before
/// failing; one with it waits up to its deadline. Pi's
/// `CODEMODE_WORKER_WAIT_MS` is 60 seconds; scripts of subagents have
/// their own slots ([`Admission::children`]), so no script waits on a
/// slot its own child needs, and a longer wait fails fewer scripts.
const CODEMODE_WORKER_WAIT: Duration = Duration::from_secs(600);
/// The longest one `sleep(ms)` call waits.
pub const MAX_SLEEP_MS: u64 = 300_000;
/// Call records partial results and the final details list, the latest
/// ones; the rest are counted by status.
pub const MAX_LISTED_CALLS: usize = 256;
/// Partial results go out at most this often, plus the final one, so a
/// script of thousands of calls does not republish its list per call.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(100);
/// Characters of a failed call's error in the failure summary.
const SUMMARY_ERROR_CHARS: usize = 200;
/// Calls a note names before it counts the rest.
const NAMED_CALLS: usize = 10;
/// A script's tool calls that run at once; the rest wait in the order the
/// script made them, so a `Promise.all` over a thousand items does not start
/// a thousand processes. Pi has no limit.
pub const MAX_CONCURRENT_NESTED_CALLS: usize = 16;
/// What an aborted script reports: the message of the reason Pi's aborted
/// signal carries by default.
pub const CODEMODE_ABORT_MESSAGE: &str = "This operation was aborted";
/// The host-answered global that waits: `await sleep(ms)`.
pub const SLEEP_GLOBAL: &str = "sleep";
/// The prefix of the files output is saved to.
const OUTPUT_PREFIX: &str = "bake-codemode";

/// Pi's `maxCodemodeWorkers`: the slot limit an environment setting gives
/// when it is a positive integer, as JavaScript's `Number` reads it, else
/// [`MAX_CONCURRENT_CODEMODE_SCRIPTS`]. A blank setting counts as unset.
/// Pi reads it on every call; Bake reads it once, when the process's first
/// script asks for a slot.
pub fn max_codemode_workers(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> usize {
    env(BAKE_CODEMODE_MAX_WORKERS_ENV)
        .map(|value| value.to_string_lossy().trim().to_owned())
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| *value >= 1.0 && value.fract() == 0.0)
        .map_or(MAX_CONCURRENT_CODEMODE_SCRIPTS, |value| {
            if value >= Semaphore::MAX_PERMITS as f64 {
                Semaphore::MAX_PERMITS
            } else {
                value as usize
            }
        })
}

/// Script slots: at most a limit of scripts alive, the rest waiting in
/// arrival order (`tokio`'s semaphore is fair), Pi's
/// `activeCodemodeWorkers` and `codemodeWorkerWaiters`. Top-level sessions
/// and in-process subagents draw from separate sets of the same size: a
/// parent's script that waits for its child must not hold the slot the
/// child's script needs, and a child cannot delegate, so no wait crosses
/// back.
#[derive(Clone)]
pub(super) struct Admission {
    slots: Arc<Semaphore>,
    limit: usize,
}

/// Why [`Admission::reserve`] gave no slot.
enum Refusal {
    Aborted,
    TimedOut,
}

impl Admission {
    /// The process's slots for scripts of top-level sessions.
    #[cfg(not(test))]
    pub(super) fn process() -> Self {
        static SLOTS: std::sync::LazyLock<Admission> = std::sync::LazyLock::new(|| {
            Admission::new(max_codemode_workers(|name| std::env::var_os(name)))
        });
        SLOTS.clone()
    }

    /// The process's slots for scripts of subagents.
    #[cfg(not(test))]
    pub(super) fn children() -> Self {
        static SLOTS: std::sync::LazyLock<Admission> = std::sync::LazyLock::new(|| {
            Admission::new(max_codemode_workers(|name| std::env::var_os(name)))
        });
        SLOTS.clone()
    }

    /// Unit tests run in parallel on their own threads, so each test
    /// thread has its own slots; the admission cases use their own.
    #[cfg(test)]
    pub(super) fn process() -> Self {
        thread_local! {
            static SLOTS: Admission = Admission::new(MAX_CONCURRENT_CODEMODE_SCRIPTS);
        }
        SLOTS.with(Admission::clone)
    }

    #[cfg(test)]
    pub(super) fn children() -> Self {
        thread_local! {
            static SLOTS: Admission = Admission::new(MAX_CONCURRENT_CODEMODE_SCRIPTS);
        }
        SLOTS.with(Admission::clone)
    }

    /// `limit` slots of their own.
    pub(super) fn new(limit: usize) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(limit)),
            limit,
        }
    }

    /// The slots, for tests that hold some.
    #[cfg(test)]
    pub(super) fn slots(&self) -> &Arc<Semaphore> {
        &self.slots
    }

    fn limit(&self) -> usize {
        self.limit
    }

    /// Pi's `reserveCodemodeWorker`: a slot, waiting in arrival order for
    /// at most `wait` while all are taken, after calling `on_wait` once.
    /// The returned hook releases it once the script's thread has exited;
    /// dropped unused, it releases it at once.
    async fn reserve(
        &self,
        signal: Option<&AbortSignal>,
        wait: Duration,
        on_wait: impl FnOnce(),
    ) -> Result<ExitHook, Refusal> {
        if signal.is_some_and(AbortSignal::aborted) {
            return Err(Refusal::Aborted);
        }
        if let Ok(permit) = Arc::clone(&self.slots).try_acquire_owned() {
            return Ok(ExitHook::new(move || drop(permit)));
        }
        on_wait();
        let cancelled = async {
            match signal {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending().await,
            }
        };
        // An aborted script never starts, even when a slot came free in the
        // same poll; dropping the acquire returns a slot it was given.
        tokio::select! {
            biased;
            () = cancelled => Err(Refusal::Aborted),
            permit = Arc::clone(&self.slots).acquire_owned() => match permit {
                Ok(permit) => Ok(ExitHook::new(move || drop(permit))),
                // The semaphore is never closed.
                Err(_) => Err(Refusal::TimedOut),
            },
            () = tokio::time::sleep(wait) => Err(Refusal::TimedOut),
        }
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
///
/// Bake's addition: the rejection's `Error` also carries `tool`, the tool's
/// name, and `args`, its arguments' preview, so a script that awaits several
/// calls together can tell which one failed; the message stays the tool's
/// text, which scripts compare.
fn to_script_value(tool: &AgentTool, outcome: &AgentToolCallOutcome, args: &str) -> CallResult {
    if tool.output_schema.is_some()
        && let Some(structured) = &outcome.result.structured_content
    {
        return Ok(Some(structured.clone()));
    }
    let text = text_of(&outcome.result);
    if outcome.is_error {
        let mut properties = serde_json::Map::new();
        properties.insert("tool".to_owned(), Value::String(tool.name.clone()));
        properties.insert("args".to_owned(), Value::String(args.to_owned()));
        return Err(CallError {
            message: if text.is_empty() {
                format!("Tool \"{}\" failed", tool.name)
            } else {
                text
            },
            properties: Some(properties),
        });
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

/// One call in a summary: a failed call with its arguments and the start of
/// its error, any other with its status.
fn summarize_call(call: &CodemodeNestedCall) -> String {
    match (&call.status, &call.error) {
        (CodemodeNestedCallStatus::Error, Some(error)) => {
            let mut start: String = error.chars().take(SUMMARY_ERROR_CHARS).collect();
            if start.len() < error.len() {
                start.push('…');
            }
            format!("{} {} (error: {start})", call.name, call.args)
        }
        _ => format!("{} ({})", call.name, call.status.as_str()),
    }
}

/// Pi's call summary. Past [`MAX_LISTED_CALLS`] it counts the calls by
/// status and lists the failed ones still listed.
fn format_call_summary(view: &CallView) -> String {
    let counts = view.counts();
    if counts.total() == 0 {
        return "No tool calls were made.".to_owned();
    }
    let head = "Tool calls made before the failure (they are not undone):";
    if view.omitted.total() == 0 {
        let listed: Vec<String> = view.listed.iter().map(summarize_call).collect();
        return format!("{head} {}", listed.join(", "));
    }
    let failed: Vec<String> = view
        .listed
        .iter()
        .filter(|call| call.status == CodemodeNestedCallStatus::Error)
        .map(summarize_call)
        .collect();
    let mut summary = format!("{head} {}", counts.describe());
    if !failed.is_empty() {
        summary.push_str(&format!(
            ". Failed among the last {}: {}",
            view.listed.len(),
            failed.join(", ")
        ));
    }
    summary
}

/// The names in `names`, at most [`NAMED_CALLS`] of them, then how many
/// more there are.
fn name_calls(names: &[String], total: usize) -> String {
    let shown = names.len().min(NAMED_CALLS);
    let mut text = names[..shown].join(", ");
    if total > shown {
        text.push_str(&format!(", and {} more", total - shown));
    }
    text
}

/// The next step for a script that used a global the sandbox does not
/// have, one of those `bake-codemode`'s globals test proves absent.
fn missing_global_hint(failure: &Failure, web_fetch: bool) -> Option<&'static str> {
    let Failure::Exception(error) = failure else {
        return None;
    };
    if error.name.as_deref() != Some("ReferenceError") {
        return None;
    }
    let name = error.message.strip_suffix(" is not defined")?;
    Some(match name {
        "setTimeout" | "setInterval" => {
            "There are no timers: await tool calls, or `await sleep(ms)`."
        }
        "fetch" if web_fetch => "There is no fetch: use `tools.web_fetch({ url })`.",
        "fetch" => "There is no network access from scripts.",
        "require" | "process" => {
            "There are no Node APIs: use tools.read, tools.bash and the other tools."
        }
        "structuredClone" => "Use JSON.parse(JSON.stringify(value)).",
        "TextEncoder" | "TextDecoder" | "URL" | "URLSearchParams" | "crypto" | "Intl" => {
            "Not available in the sandbox."
        }
        _ => return None,
    })
}

/// What a failure's text says besides the failure: the calls, the calls
/// still running when it ended, and whether a web fetch tool exists.
#[derive(Debug, Default)]
pub(super) struct Ending<'a> {
    /// The calls, with their final statuses.
    pub(super) calls: CallView,
    /// The names of the calls still running when the script ended.
    pub(super) running: &'a [String],
    /// Scripts can call `web_fetch`.
    pub(super) web_fetch: bool,
}

/// Pi's `formatError`. Bake adds a hint for a missing global and, for a
/// timeout, the calls still running.
pub(super) fn format_error(failure: &Failure, ending: &Ending<'_>) -> String {
    let mut head = match failure {
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
        Failure::Busy { message } => message.clone(),
        Failure::Aborted { message } => format!("Script aborted: {message}"),
        other => format!("Script sandbox failed: {}", other.message()),
    };
    if let Some(hint) = missing_global_hint(failure, ending.web_fetch) {
        head.push('\n');
        head.push_str(hint);
    }
    if matches!(failure, Failure::Timeout { .. }) && !ending.running.is_empty() {
        head.push_str(&format!(
            "\nStill running when it timed out: {}.",
            name_calls(ending.running, ending.running.len())
        ));
    }
    format!("{head}\n\n{}", format_call_summary(&ending.calls))
}

/// The note a successful script's result ends with when calls it made were
/// still running, or never started, when it ended.
fn cancelled_note(view: &CallView) -> Option<String> {
    let count = view.counts().cancelled;
    if count == 0 {
        return None;
    }
    let names: Vec<String> = view
        .listed
        .iter()
        .filter(|call| call.status == CodemodeNestedCallStatus::Cancelled)
        .map(|call| call.name.clone())
        .collect();
    let (calls, were) = if count == 1 {
        ("tool call was", "was")
    } else {
        ("tool calls were", "were")
    };
    Some(format!(
        "[{count} {calls} still running when the script ended and {were} cancelled: {}. Await calls (Promise.all) before the script ends.]",
        name_calls(&names, count)
    ))
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

/// Calls by status: the calls a [`CallView`] does not list, or all of them.
pub(super) type StatusCounts = CodemodeCallCounts;

/// What a result shows of a script's calls: the latest
/// [`MAX_LISTED_CALLS`] records, in the order the script made them, and
/// the others counted by status.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct CallView {
    pub(super) listed: Vec<CodemodeNestedCall>,
    pub(super) omitted: StatusCounts,
}

impl CallView {
    /// Every call by status, listed or not.
    pub(super) fn counts(&self) -> StatusCounts {
        let mut counts = self.omitted;
        for call in &self.listed {
            counts.add(call.status);
        }
        counts
    }

    fn details(&self, full_output_path: Option<&str>) -> Value {
        codemode_details(&self.listed, &self.omitted, full_output_path)
    }
}

/// The records behind a [`CallView`], bounded: calls still waiting or
/// running, each settled call until [`MAX_LISTED_CALLS`] later ones have
/// settled, and counts of the settled calls let go.
#[derive(Default)]
struct Book {
    made: usize,
    live: BTreeMap<usize, CodemodeNestedCall>,
    settled: VecDeque<(usize, CodemodeNestedCall)>,
    let_go: StatusCounts,
    /// On Tokio's clock, which tests can pause.
    published: Option<tokio::time::Instant>,
    dirty: bool,
}

impl Book {
    fn view(&self) -> CallView {
        let mut all: Vec<(usize, &CodemodeNestedCall)> = self
            .live
            .iter()
            .map(|(index, call)| (*index, call))
            .chain(self.settled.iter().map(|(index, call)| (*index, call)))
            .collect();
        all.sort_by_key(|(index, _)| *index);
        let skip = all.len().saturating_sub(MAX_LISTED_CALLS);
        let mut omitted = self.let_go;
        for (_, call) in &all[..skip] {
            omitted.add(call.status);
        }
        CallView {
            listed: all[skip..]
                .iter()
                .map(|(_, call)| (*call).clone())
                .collect(),
            omitted,
        }
    }

    fn keep_settled(&mut self, index: usize, call: CodemodeNestedCall) {
        self.settled.push_back((index, call));
        while self.settled.len() > MAX_LISTED_CALLS {
            if let Some((_, call)) = self.settled.pop_front() {
                self.let_go.add(call.status);
            }
        }
    }
}

/// The records of a script's nested calls, published as partial results at
/// most every [`PUBLISH_INTERVAL`].
#[derive(Clone)]
pub(super) struct Records {
    /// The codemode call's id, the prefix of a record's id until the
    /// nested call has its own.
    caller_id: String,
    book: Arc<Mutex<Book>>,
    on_update: AgentToolUpdateCallback,
}

impl Records {
    pub(super) fn new(caller_id: String, on_update: AgentToolUpdateCallback) -> Self {
        Self {
            caller_id,
            book: Arc::default(),
            on_update,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Pi's `publish`: the records so far as a partial result, now.
    pub(super) fn publish(&self) {
        let view = {
            let mut book = self.lock();
            book.published = Some(tokio::time::Instant::now());
            book.dirty = false;
            book.view()
        };
        self.on_update.update(AgentToolResult {
            details: Some(view.details(None)),
            ..AgentToolResult::default()
        });
    }

    /// Publishes the change now when the last publication is old enough,
    /// else leaves it for [`Records::flush_at`].
    fn changed(&self) {
        let due = {
            let mut book = self.lock();
            book.dirty = true;
            book.published
                .is_none_or(|published| published.elapsed() >= PUBLISH_INTERVAL)
        };
        if due {
            self.publish();
        }
    }

    /// When an unpublished change is due.
    pub(super) fn flush_at(&self) -> Option<tokio::time::Instant> {
        let book = self.lock();
        if !book.dirty {
            return None;
        }
        Some(
            book.published
                .map_or_else(tokio::time::Instant::now, |published| {
                    published + PUBLISH_INTERVAL
                }),
        )
    }

    /// Publishes an unpublished change.
    pub(super) fn flush(&self) {
        if self.lock().dirty {
            self.publish();
        }
    }

    /// Records a call when the script makes it, as Pi does, so a call that
    /// never leaves the queue is still listed; returns its place.
    pub(super) fn record(&self, tool: &AgentTool, args: &str) -> usize {
        let index = {
            let mut book = self.lock();
            let index = book.made;
            book.made += 1;
            book.live.insert(
                index,
                CodemodeNestedCall {
                    id: format!("{}/?", self.caller_id),
                    name: tool.name.clone(),
                    args: args.to_owned(),
                    status: CodemodeNestedCallStatus::Running,
                    duration_ms: None,
                    error: None,
                },
            );
            index
        };
        self.changed();
        index
    }

    /// Settles the call at `index` with `settle`.
    pub(super) fn settle(&self, index: usize, settle: impl FnOnce(&mut CodemodeNestedCall)) {
        {
            let mut book = self.lock();
            if let Some(mut call) = book.live.remove(&index) {
                settle(&mut call);
                book.keep_settled(index, call);
            }
        }
        self.changed();
    }

    /// The names of the calls not settled, in the order made.
    fn unsettled(&self) -> Vec<String> {
        self.lock()
            .live
            .values()
            .map(|call| call.name.clone())
            .collect()
    }

    /// The final view: every call not settled is cancelled.
    pub(super) fn finish(&self) -> CallView {
        let mut book = self.lock();
        let live = std::mem::take(&mut book.live);
        for (index, mut call) in live {
            call.status = CodemodeNestedCallStatus::Cancelled;
            book.keep_settled(index, call);
        }
        book.view()
    }
}

/// A tool call the script made that has not started, recorded at `index`.
struct QueuedCall {
    id: u32,
    index: usize,
    tool: Arc<AgentTool>,
    args: Option<Value>,
    preview: String,
}

/// A nested call that has started: what answering the script and
/// recording it need.
struct StartedCall {
    id: u32,
    index: usize,
    tool: Arc<AgentTool>,
    preview: String,
    started: Instant,
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
        preview,
        started,
    } = call;
    let outcome = outcome.await;
    records.settle(index, |record| {
        record.id = outcome.tool_call.id.clone();
        record.duration_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
        if outcome.is_error {
            record.status =
                if signal.aborted() || script_signal.as_ref().is_some_and(AbortSignal::aborted) {
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
    });
    // The run may have ended meanwhile; its answers are then ignored.
    let _ = handle.answer(id, to_script_value(&tool, &outcome, &preview));
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
    /// The names of the calls not settled when the script ended.
    running: Vec<String>,
}

/// Why a script never started: no slot came free, or it was aborted while
/// waiting for one.
struct NotStarted(String);

/// Pi's wait for a slot: it counts toward the script's `timeout_ms`, so the
/// script then has what remains, at least a millisecond. Without
/// `timeout_ms` it waits up to [`CODEMODE_WORKER_WAIT`]. While it waits, a
/// partial result says so.
async fn reserve_slot(
    admission: &Admission,
    signal: Option<&AbortSignal>,
    deadline_ms: Option<u64>,
    on_update: &AgentToolUpdateCallback,
) -> Result<(ExitHook, Option<u64>), NotStarted> {
    let wait = deadline_ms.map_or(CODEMODE_WORKER_WAIT, Duration::from_millis);
    let waiting = Instant::now();
    let reserved = admission
        .reserve(signal, wait, || {
            on_update.update(AgentToolResult {
                content: vec![UserContentBlock::Text(TextContent::new(format!(
                    "Waiting for a free codemode worker: all {} are running other scripts.",
                    admission.limit()
                )))],
                details: Some({
                    let mut details = CallView::default().details(None);
                    details[CODEMODE_WAITING_DETAIL] = Value::Bool(true);
                    details
                }),
                ..AgentToolResult::default()
            });
        })
        .await;
    let waited_ms = waiting.elapsed().as_secs_f64() * 1000.0;
    match reserved {
        Ok(on_exit) => Ok((
            on_exit,
            deadline_ms.map(|deadline| ((deadline as f64 - waited_ms).round() as u64).max(1)),
        )),
        Err(Refusal::Aborted) => Err(NotStarted(format!(
            "Script aborted: {CODEMODE_ABORT_MESSAGE}"
        ))),
        Err(Refusal::TimedOut) => Err(NotStarted(match deadline_ms {
            Some(deadline) => format!(
                "Script timed out: Execution timed out after {deadline} ms while waiting for a free codemode worker. Make these tool calls directly instead of through codemode, or retry later."
            ),
            None => format!(
                "No codemode worker became free within {} minutes: all {} are running other scripts in this process. Make these tool calls directly instead of through codemode.",
                CODEMODE_WORKER_WAIT.as_secs() / 60,
                admission.limit()
            ),
        })),
    }
}

/// `sleep(ms)`'s wait: a non-negative number of milliseconds, at most
/// [`MAX_SLEEP_MS`].
fn sleep_duration(args: Option<&Value>) -> Result<Duration, CallError> {
    match args.and_then(Value::as_f64) {
        Some(ms) if ms >= 0.0 => Ok(Duration::from_millis(ms.min(MAX_SLEEP_MS as f64) as u64)),
        _ => Err(CallError::new(
            "sleep(ms) expects a non-negative number of milliseconds",
        )),
    }
}

/// Starts the script once a slot is free and serves its calls until it
/// ends.
async fn run_script(
    mut source: Source,
    catalog: &Catalog,
    signal: Option<&AbortSignal>,
    nested: &NestedToolExecutor,
    records: &Records,
    admission: Admission,
) -> Result<Ran, NotStarted> {
    let failed = |failure| {
        Ok(Ran {
            result: Err(failure),
            output: Vec::new(),
            running: Vec::new(),
        })
    };
    // Held from here; dropped on an early return, it frees the slot.
    let (on_exit, timeout_ms) = reserve_slot(
        &admission,
        signal,
        source.options.timeout_ms,
        &records.on_update,
    )
    .await?;
    source.options.timeout_ms = timeout_ms;
    let work = catalog.host.as_ref().and_then(|host| host.work());
    let engine = match engine(work.as_ref()).await {
        Ok(engine) => engine,
        Err(message) => return failed(Failure::Protocol { message }),
    };
    // Read once the script has its slot, so it sees what scripts that ran
    // while it waited wrote.
    let store = match &catalog.host {
        Some(host) => match host.load_store().await {
            Ok(store) => store,
            Err(message) => return failed(Failure::Protocol { message }),
        },
        None => Default::default(),
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
    request.globals.push(GlobalDeclaration::new(SLEEP_GLOBAL));
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
    // `sleep()` calls; aborted with the script, they answer nothing late.
    let mut sleeps: JoinSet<()> = JoinSet::new();
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
        // Start waiting calls while there is room, in the order made. A
        // running call may stop on the script's aborted signal and free its
        // room before the abort is handled below, so the signal is read too.
        while !stopping
            && !signal.is_some_and(AbortSignal::aborted)
            && running.len() < MAX_CONCURRENT_NESTED_CALLS
        {
            let Some(call) = queue.pop_front() else {
                break;
            };
            let controller = AbortController::new();
            let QueuedCall {
                id,
                index,
                tool,
                args,
                preview,
            } = call;
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
                    preview,
                    started,
                },
                controller.signal(),
                signal.cloned(),
                handle.clone(),
                records.clone(),
            ));
            controllers.push(controller);
        }
        let flush_at = records.flush_at();
        tokio::select! {
            event = run.next() => match event {
                Event::Call(HostCall::Tool { id, name, args }) => {
                    match catalog.tools.iter().find(|tool| tool.name == name) {
                        Some(tool) => {
                            let preview = preview_args(args.as_ref());
                            let index = records.record(tool, &preview);
                            queue.push_back(QueuedCall { id, index, tool: Arc::clone(tool), args, preview });
                        }
                        None => {
                            let available: Vec<&str> = catalog.tools.iter().map(|tool| tool.name.as_str()).collect();
                            let message = bake_agent::unknown_tool_message(&name, &available);
                            let _ = handle.answer(id, Err(CallError::new(message)));
                        }
                    }
                }
                Event::Call(HostCall::Global { id, name, args }) if name == SLEEP_GLOBAL => {
                    match sleep_duration(args.as_ref()) {
                        Ok(duration) => {
                            let handle = handle.clone();
                            sleeps.spawn(async move {
                                tokio::time::sleep(duration).await;
                                let _ = handle.answer(id, Ok(None));
                            });
                        }
                        Err(error) => {
                            let _ = handle.answer(id, Err(error));
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
            Some(_) = sleeps.join_next(), if !sleeps.is_empty() => {}
            () = async {
                match flush_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => records.flush(),
        }
    };
    let unsettled = records.unsettled();
    // Calls still running when the script ends are cancelled and awaited;
    // waiting ones never start, and their records read cancelled.
    queue.clear();
    sleeps.abort_all();
    for controller in &controllers {
        controller.abort();
    }
    while running.join_next().await.is_some() {}
    while sleeps.join_next().await.is_some() {}
    drop(run);
    Ok(Ran {
        result: outcome.result.clone(),
        output,
        running: unsettled,
    })
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
    let admission = if host.as_ref().is_some_and(|host| host.is_subagent()) {
        Admission::children()
    } else {
        Admission::process()
    };
    execute_with(
        tool_call_id,
        params,
        signal,
        on_update,
        ctx,
        host,
        admission,
    )
    .await
}

/// [`execute_codemode`] with the given slots.
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
    let records = Records::new(tool_call_id, on_update);
    // Show the script as running before its first nested call.
    records.publish();

    let nested = ctx.nested().clone();
    // Scripts see each tool's own description, not the one the request
    // declares.
    let tools: Vec<Arc<AgentTool>> = nested
        .tools()
        .await
        .into_iter()
        .filter(|tool| tool.name != CODEMODE_TOOL_NAME)
        .map(|tool| {
            match host
                .as_ref()
                .and_then(|host| host.tool_description(&tool.name))
            {
                Some(description) => {
                    let mut own = (*tool).clone();
                    own.description = description;
                    Arc::new(own)
                }
                None => tool,
            }
        })
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

    let ran = match run_script(
        source,
        &catalog,
        signal.as_ref(),
        &nested,
        &records,
        admission,
    )
    .await
    {
        Ok(ran) => ran,
        // Pi's result for a script that never got a worker: no output and
        // no calls to summarize.
        Err(NotStarted(error)) => {
            return Ok(AgentToolResult {
                content: vec![
                    UserContentBlock::Text(TextContent::new(format!(
                        "Script failed\nWall time {:.1} seconds\nOutput:\n",
                        started.elapsed().as_secs_f64()
                    ))),
                    UserContentBlock::Text(TextContent::new(format!(
                        "Script error:\n{error}\n\nThe script did not start."
                    ))),
                ],
                details: Some(CallView::default().details(None)),
                is_error: true,
                ..AgentToolResult::default()
            });
        }
    };
    // The last partial result shows every call as it settled.
    records.flush();
    let calls = records.finish();

    let mut script_output = ran.output;
    let mut store_failure = None;
    if let Ok(completion) = &ran.result {
        let writes = &completion.store_writes;
        if (!writes.set.is_empty() || !writes.delete.is_empty())
            && let Some(host) = &catalog.host
        {
            // The script's calls already ran, so a failed write is reported
            // with its output instead of replacing it.
            store_failure = host
                .append_store(CodemodeStoreEntryData {
                    set: writes.set.clone(),
                    delete: writes.delete.clone(),
                })
                .await
                .err();
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
    if let Some(error) = store_failure {
        items.push(Item::Text(format!(
            "[The codemode store could not be written: {error}]"
        )));
    }
    if let Err(failure) = &ran.result {
        let ending = Ending {
            calls: calls.clone(),
            running: &ran.running,
            web_fetch: catalog.tools.iter().any(|tool| tool.name == "web_fetch"),
        };
        items.push(Item::Text(format!(
            "Script error:\n{}",
            format_error(failure, &ending)
        )));
    }
    let work = catalog.host.as_ref().and_then(|host| host.work());
    let (truncated, full_output_path) =
        truncate_output(join_adjacent_text(items), max_output_tokens, work.as_ref()).await;
    // After truncation, which joins the text items and moves images after
    // them, so each path stays next to its image and is never cut.
    let mut output = save_images(truncated, work.as_ref()).await;
    // After truncation, so it is never cut.
    if ran.result.is_ok()
        && let Some(note) = cancelled_note(&calls)
    {
        output.push(Item::Text(note));
    }
    let output = join_adjacent_text(output);
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
        details: Some(calls.details(full_output_path.as_deref())),
        is_error: ran.result.is_err(),
        ..AgentToolResult::default()
    })
}
