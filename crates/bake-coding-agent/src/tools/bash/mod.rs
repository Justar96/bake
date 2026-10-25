//! Pi's bash tool (v1.1.0; see the crate's `NOTICE`).
//!
//! | Module | Source |
//! |---|---|
//! | this module | Pi `src/core/tools/bash.ts` (`createShellToolDefinition`, `createBashTool`), `BASH_UPDATE_THROTTLE_MS` from `src/core/tools/renderers/bash.ts` |
//! | [`local`] | Pi `src/core/tools/bash.ts` (`createLocalShellOperations`), `src/utils/child-process.ts` (`waitForChildProcess`) |
//! | [`shell`] | Pi `src/utils/shell.ts` (`getShellConfig`, `getShellEnv`) |
//! | `bake-process` (a crate of its own, shared with MCP stdio servers) | Pi `src/utils/shell.ts` (`killProcessTree`, detached-child tracking) |
//!
//! The model-visible text (name, description, schemas, results, exit-code,
//! timeout, abort, and truncation notices) is Pi's byte for byte;
//! `tests/fixtures/pi-bash/` records it from Pi's own code. Three
//! deviations name Bake where Pi names itself (D33): the session's details
//! reach commands as `BAKE_*` variables where Pi exports `PI_*`, spill
//! files are named `bake-bash-…` where Pi's are `pi-bash-…`, and the tool
//! contributes no prompt guideline, since Pi's names the `PI_*` variables.
//!
//! [`BashOperations`] is the one execution seam: the tool hands it the
//! final command, directory, and environment, and a wrapper can refuse or
//! rewrite the spawn there, and [`LocalShellOperations::exec_prepared`]
//! lets it spawn a wrapped argv (a sandbox runner) with Pi's own process
//! handling. Sandboxing and approvals (D24) live in
//! [`crate::permissions`], which wraps this tool. Pi's `powershell` tool is not ported: Pi's default tools are
//! `read`, `bash`, `edit`, and `write` on every platform, Windows included.
//!
//! Pi passes each call an extension context for its working directory and
//! session details; [`BashToolOptions::context`] stands in for it. Output
//! streams through `tools::output_accumulator`, which spills past Pi's
//! limits to a private file that outlives the call for later reads, as
//! Pi's temp file does.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bake_agent::{AgentTool, AgentToolResult, AgentToolUpdateCallback, BoxFuture};
use bake_ai::{AbortSignal, ConstrainedSampling, StrictPreference, TextContent, UserContentBlock};
use serde_json::{Map, Value, json};
use tokio::sync::Notify;
use tokio::time::Instant;

use crate::agent_session::ToolPrompt;
use crate::jobs::{JobContext, JobOutcome, JobRegistry, JobSpec, OutputMode};
use crate::session::json::js_number_to_string;
use crate::tools::output_accumulator::{
    OutputAccumulator, OutputAccumulatorOptions, OutputSnapshot,
};
use crate::tools::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, format_size};

pub mod local;
pub mod shell;

#[cfg(test)]
mod tests;

pub use bake_process::kill_tracked_detached_children;
pub use local::{
    LocalShellOperations, PreparedExecError, PreparedSpawn, ShellInvocation,
    create_local_bash_operations,
};
pub use shell::{CommandTransport, ShellConfig, get_shell_config, get_shell_env};

/// Pi's `bashToolSystemPromptContribution.snippet`.
pub const BASH_TOOL_PROMPT_SNIPPET: &str = "Execute bash commands (ls, grep, find, etc.)";
/// Pi's `BASH_UPDATE_THROTTLE_MS`: the least time between streamed updates.
pub const BASH_UPDATE_THROTTLE: Duration = Duration::from_millis(100);
/// Pi's `STRUCTURED_OUTPUT_MAX_BYTES`: the cap on `structured_content.output`.
pub const STRUCTURED_OUTPUT_MAX_BYTES: usize = 1024 * 1024;

/// The spill file's name prefix; Pi's is `pi-bash`.
const TEMP_FILE_PREFIX: &str = "bake-bash";
/// The variables a command receives the session's details in, Pi's
/// `PI_*` names under Bake's prefix; inherited values are removed first.
pub const SESSION_VARIABLES: [&str; 5] = [
    "BAKE_SESSION_ID",
    "BAKE_SESSION_FILE",
    "BAKE_PROVIDER",
    "BAKE_MODEL",
    "BAKE_REASONING_LEVEL",
];

/// Receives output bytes as the command writes them, Pi's `onData`.
pub type BashDataCallback = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// What a [`BashOperations::exec`] call receives besides the command and
/// directory, Pi's `exec` options.
#[derive(Clone)]
pub struct BashExecOptions {
    /// Called with each chunk of combined stdout and stderr. Calls after
    /// `exec` settles are ignored.
    pub on_data: BashDataCallback,
    /// Aborts the command.
    pub signal: Option<AbortSignal>,
    /// Timeout in seconds, as the model gave it.
    pub timeout: Option<f64>,
    /// The complete environment; `None` means [`get_shell_env`].
    pub env: Option<Vec<(OsString, OsString)>>,
}

impl std::fmt::Debug for BashExecOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BashExecOptions")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// Why a command did not finish, the errors Pi's `exec` throws.
#[derive(Debug, Clone, PartialEq)]
pub enum BashExecError {
    /// Pi's `aborted`.
    Aborted,
    /// Pi's `timeout:<seconds>`, with the seconds the call gave.
    TimedOut(f64),
    /// Any other failure; the message reaches the model unchanged.
    Failed(String),
}

/// Pi's `BashOperations`: runs a command and streams its output. Resolves
/// to the exit code; report a signal termination as 128 plus the signal
/// number. `None` is Pi's `null`, a failed command.
pub trait BashOperations: Send + Sync {
    /// Runs `command` in `cwd`.
    fn exec<'a>(
        &'a self,
        command: String,
        cwd: PathBuf,
        options: BashExecOptions,
    ) -> BoxFuture<'a, Result<Option<i64>, BashExecError>>;
}

/// The command, directory, and environment a call will run with, Pi's
/// `BashSpawnContext`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashSpawnContext {
    /// The command, after the prefix.
    pub command: String,
    /// The working directory.
    pub cwd: PathBuf,
    /// The complete environment.
    pub env: Vec<(OsString, OsString)>,
}

/// Adjusts a call before it runs, Pi's `BashSpawnHook`.
pub type BashSpawnHook = Arc<dyn Fn(BashSpawnContext) -> BashSpawnContext + Send + Sync>;

/// The model a session uses, for `BAKE_PROVIDER` and `BAKE_MODEL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashModel {
    /// The provider name.
    pub provider: String,
    /// The model id.
    pub id: String,
}

/// What Pi reads from a call's extension context.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BashToolContext {
    /// The session's working directory, which wins over the tool's when
    /// not empty (Pi's `ctx.cwd || cwd`).
    pub cwd: Option<PathBuf>,
    /// `BAKE_SESSION_ID`.
    pub session_id: String,
    /// `BAKE_SESSION_FILE`, for a persisted session.
    pub session_file: Option<String>,
    /// `BAKE_PROVIDER` and `BAKE_MODEL`.
    pub model: Option<BashModel>,
    /// `BAKE_REASONING_LEVEL`.
    pub thinking_level: Option<String>,
}

/// Supplies a call's context when the call starts.
pub type BashToolContextFn = Arc<dyn Fn() -> BashToolContext + Send + Sync>;

/// Pi's `BashToolOptions`, plus what Pi takes from its globals.
#[derive(Clone)]
pub struct BashToolOptions {
    /// Custom execution; defaults to [`create_local_bash_operations`].
    pub operations: Option<Arc<dyn BashOperations>>,
    /// Prepended to every command on its own line, Pi's `commandPrefix`.
    pub command_prefix: Option<String>,
    /// An explicit shell, Pi's `shellPath` setting.
    pub shell_path: Option<PathBuf>,
    /// Exposes session details as the [`SESSION_VARIABLES`]. Default: true.
    pub expose_session_environment: bool,
    /// Adjusts the command, directory, or environment before each call.
    pub spawn_hook: Option<BashSpawnHook>,
    /// The per-call context Pi's extension context carries.
    pub context: Option<BashToolContextFn>,
    /// Put first on `PATH`, Pi's `getBinDir()`; Bake passes its home's
    /// `bin`. `None` leaves `PATH` as it is.
    pub bin_dir: Option<PathBuf>,
    /// Where spill files go. Defaults to the OS temporary directory.
    pub spill_directory: Option<PathBuf>,
    /// The owner of the shells the default local operations spawn
    /// ([`LocalShellOperations::with_owner`]): the session's
    /// [`crate::owned_work::WorkGate`], so its shutdown waits for every
    /// command's tree. Ignored with custom `operations`.
    pub owner: Option<crate::owned_work::WorkGate>,
    /// The session's background jobs. With them the tool takes
    /// `run_in_background`, which starts the command as a `bash` job
    /// through the same `operations`, so a sandbox confines it alike, and
    /// returns its id after the session records its start. `None` keeps Pi's tool.
    pub jobs: Option<JobRegistry>,
}

impl Default for BashToolOptions {
    fn default() -> Self {
        Self {
            operations: None,
            command_prefix: None,
            shell_path: None,
            expose_session_environment: true,
            spawn_hook: None,
            context: None,
            bin_dir: None,
            spill_directory: None,
            owner: None,
            jobs: None,
        }
    }
}

impl std::fmt::Debug for BashToolOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BashToolOptions")
            .field("command_prefix", &self.command_prefix)
            .field("shell_path", &self.shell_path)
            .field(
                "expose_session_environment",
                &self.expose_session_environment,
            )
            .field("bin_dir", &self.bin_dir)
            .field("spill_directory", &self.spill_directory)
            .finish_non_exhaustive()
    }
}

/// The key of a background start's `details`: `{ "id", "label" }`.
pub const BACKGROUND_JOB_DETAIL: &str = "backgroundJob";

/// Bake's TypeScript `bashDescription` sentence for background runs, after
/// Pi's description when the tool takes `run_in_background`.
pub const BASH_BACKGROUND_DESCRIPTION: &str =
    " Use `run_in_background` for long builds and tests, then `job_output` and `job_kill`.";

/// The `run_in_background` parameter, from `tool-bash`.
fn background_parameter() -> Value {
    json!({ "type": "boolean", "description": "No timeout." })
}

/// Pi's description, with [`BASH_BACKGROUND_DESCRIPTION`] when `background`.
pub fn bash_tool_description_for(background: bool) -> String {
    let mut description = bash_tool_description();
    if background {
        description.push_str(BASH_BACKGROUND_DESCRIPTION);
    }
    description
}

/// Pi's parameters, with `run_in_background` when `background`.
pub fn bash_tool_parameters_for(background: bool) -> Value {
    let mut parameters = bash_tool_parameters();
    if background
        && let Some(properties) = parameters
            .get_mut("properties")
            .and_then(Value::as_object_mut)
    {
        properties.insert("run_in_background".to_owned(), background_parameter());
    }
    parameters
}

/// Pi's tool description.
pub fn bash_tool_description() -> String {
    format!(
        "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
        DEFAULT_MAX_BYTES / 1024
    )
}

/// Pi's `bashSchema`, in TypeBox's key order.
pub fn bash_tool_parameters() -> Value {
    json!({
        "type": "object",
        "required": ["command"],
        "properties": {
            "command": { "type": "string", "description": "Shell command to execute" },
            "timeout": {
                "type": "number",
                "description": "Timeout in seconds (optional, no default timeout)"
            }
        }
    })
}

/// Pi's `bashOutputSchema`, the shape of `structured_content`.
pub fn bash_tool_output_schema() -> Value {
    json!({
        "type": "object",
        "required": ["output", "truncated", "exit_code", "wall_time_seconds"],
        "properties": {
            "output": {
                "type": "string",
                "description": "Combined stdout and stderr, possibly truncated"
            },
            "truncated": { "type": "boolean" },
            "full_output_path": { "type": "string", "description": "Full output, when truncated" },
            "exit_code": { "type": "number" },
            "wall_time_seconds": { "type": "number" }
        }
    })
}

/// The tool's prompt text, Pi's `promptSnippet`; Pi's one guideline names
/// the `PI_*` variables, so Bake's tool has none.
pub fn bash_tool_prompt() -> ToolPrompt {
    ToolPrompt {
        name: "bash".to_owned(),
        snippet: Some(BASH_TOOL_PROMPT_SNIPPET.to_owned()),
        guidelines: Vec::new(),
    }
}

/// Pi's `createBashTool(cwd, options)`.
pub fn create_bash_tool(cwd: impl Into<PathBuf>, options: BashToolOptions) -> AgentTool {
    let operations: Arc<dyn BashOperations> = options.operations.clone().unwrap_or_else(|| {
        Arc::new(
            create_local_bash_operations(options.shell_path.clone())
                .with_owner(options.owner.clone()),
        )
    });
    let background = options.jobs.is_some();
    let shared = Arc::new(Shared {
        cwd: cwd.into(),
        operations,
        options,
    });
    let mut tool = AgentTool::new(
        "bash",
        "bash",
        bash_tool_description_for(background),
        bash_tool_parameters_for(background),
        move |_id, params, signal, on_update, _ctx| {
            let shared = Arc::clone(&shared);
            Box::pin(async move { shared.execute(params, signal, on_update).await })
        },
    );
    tool.output_schema = Some(bash_tool_output_schema());
    tool.replay = Some(bake_agent::ToolReplay::Never);
    tool.constrained_sampling = Some(ConstrainedSampling::JsonSchema {
        strict: StrictPreference::Prefer,
    });
    tool
}

struct Shared {
    cwd: PathBuf,
    operations: Arc<dyn BashOperations>,
    options: BashToolOptions,
}

/// Streaming state the data callback and the update timer share.
struct Stream {
    output: OutputAccumulator,
    accepting: bool,
    dirty: bool,
    last_update_at: Option<Instant>,
    timer: Option<Instant>,
}

fn lock(stream: &Mutex<Stream>) -> MutexGuard<'_, Stream> {
    stream.lock().unwrap_or_else(PoisonError::into_inner)
}

fn text_result(text: String) -> Vec<UserContentBlock> {
    vec![UserContentBlock::Text(TextContent::new(text))]
}

fn path_text(snapshot: &OutputSnapshot) -> String {
    snapshot
        .full_output_path
        .as_ref()
        .map_or_else(|| "undefined".to_owned(), |path| path.display().to_string())
}

fn details(snapshot: &OutputSnapshot) -> Value {
    let mut details = Map::new();
    if snapshot.truncation.truncated {
        details.insert(
            "truncation".to_owned(),
            serde_json::to_value(&snapshot.truncation).unwrap_or(Value::Null),
        );
    }
    if let Some(path) = &snapshot.full_output_path {
        details.insert(
            "fullOutputPath".to_owned(),
            Value::String(path.display().to_string()),
        );
    }
    Value::Object(details)
}

impl Stream {
    /// Pi's `emitOutputUpdate`, minus the call.
    fn take_update(&mut self) -> Option<AgentToolResult> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        self.last_update_at = Some(Instant::now());
        let snapshot = self.output.snapshot().ok()?;
        Some(AgentToolResult {
            content: text_result(snapshot.content.clone()),
            details: Some(details(&snapshot)),
            ..AgentToolResult::default()
        })
    }

    /// Pi's `scheduleOutputUpdate`: an update now if the last is old enough,
    /// otherwise a timer for when it will be.
    fn schedule(&mut self, notify: &Notify) -> Option<AgentToolResult> {
        self.dirty = true;
        let now = Instant::now();
        let due = self
            .last_update_at
            .map_or(now, |last| last + BASH_UPDATE_THROTTLE);
        if due <= now {
            self.timer = None;
            return self.take_update();
        }
        if self.timer.is_none() {
            self.timer = Some(due);
            notify.notify_one();
        }
        None
    }
}

/// Pi's `appendStatus`.
fn append_status(text: &str, status: &str) -> String {
    if text.is_empty() {
        status.to_owned()
    } else {
        format!("{text}\n\n{status}")
    }
}

/// Pi's `formatOutput`.
fn format_output(
    snapshot: &OutputSnapshot,
    last_line_bytes: usize,
    empty_text: &str,
) -> (String, Option<Value>) {
    let truncation = &snapshot.truncation;
    let mut text = if snapshot.content.is_empty() {
        empty_text.to_owned()
    } else {
        snapshot.content.clone()
    };
    if !truncation.truncated {
        return (text, None);
    }
    let path = path_text(snapshot);
    let end_line = truncation.total_lines;
    let start_line = (truncation.total_lines + 1).saturating_sub(truncation.output_lines);
    if truncation.last_line_partial {
        text.push_str(&format!(
            "\n\n[Showing last {} of line {end_line} (line is {}). Full output: {path}]",
            format_size(truncation.output_bytes),
            format_size(last_line_bytes)
        ));
    } else if truncation.truncated_by == Some(TruncatedBy::Lines) {
        text.push_str(&format!(
            "\n\n[Showing lines {start_line}-{end_line} of {}. Full output: {path}]",
            truncation.total_lines
        ));
    } else {
        text.push_str(&format!(
            "\n\n[Showing lines {start_line}-{end_line} of {} ({} limit). Full output: {path}]",
            truncation.total_lines,
            format_size(DEFAULT_MAX_BYTES)
        ));
    }
    (text, Some(details(snapshot)))
}

/// Pi's `Math.round(ms / 100) / 10` as a JSON number, integral when whole.
fn wall_time_seconds(elapsed: Duration) -> Value {
    let tenths = (elapsed.as_secs_f64() * 10.0).round();
    if tenths % 10.0 == 0.0 && tenths < 9.0e15 {
        // Whole seconds below 2^53 convert exactly.
        Value::from((tenths / 10.0) as u64)
    } else {
        serde_json::Number::from_f64(tenths / 10.0).map_or(Value::Null, Value::Number)
    }
}

/// The parsed arguments; the agent validated them against the schema, so
/// a mismatch only comes from a direct call.
fn parse_params(params: &Value) -> Result<(String, Option<f64>), String> {
    let command = params
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| "Invalid arguments: command must be a string".to_owned())?
        .to_owned();
    let timeout = match params.get("timeout") {
        None | Some(Value::Null) => None,
        // A non-number is not finite, which Pi's local operations refuse.
        Some(value) => Some(value.as_f64().unwrap_or(f64::NAN)),
    };
    Ok((command, timeout))
}

impl Shared {
    /// Pi's `resolveSpawnContext`.
    fn spawn_context(&self, command: String) -> BashSpawnContext {
        let mut env: Vec<(OsString, OsString)> = get_shell_env(self.options.bin_dir.as_deref())
            .into_iter()
            .filter(|(key, _)| !SESSION_VARIABLES.iter().any(|name| key == name))
            .collect();
        let context = self.options.context.as_ref().map(|context| context());
        if self.options.expose_session_environment
            && let Some(context) = &context
        {
            env.push(("BAKE_SESSION_ID".into(), context.session_id.clone().into()));
            if let Some(file) = context
                .session_file
                .as_ref()
                .filter(|file| !file.is_empty())
            {
                env.push(("BAKE_SESSION_FILE".into(), file.into()));
            }
            if let Some(model) = &context.model {
                env.push(("BAKE_PROVIDER".into(), model.provider.clone().into()));
                env.push(("BAKE_MODEL".into(), model.id.clone().into()));
            }
            if let Some(level) = context
                .thinking_level
                .as_ref()
                .filter(|level| !level.is_empty())
            {
                env.push(("BAKE_REASONING_LEVEL".into(), level.into()));
            }
        }
        let cwd = context
            .and_then(|context| context.cwd)
            .filter(|cwd| !cwd.as_os_str().is_empty())
            .unwrap_or_else(|| self.cwd.clone());
        let base = BashSpawnContext { command, cwd, env };
        match &self.options.spawn_hook {
            Some(hook) => hook(base),
            None => base,
        }
    }

    async fn execute(
        &self,
        params: Value,
        signal: Option<AbortSignal>,
        on_update: AgentToolUpdateCallback,
    ) -> Result<AgentToolResult, String> {
        let (command, timeout) = parse_params(&params)?;
        let label = command.clone();
        let command = match self
            .options
            .command_prefix
            .as_ref()
            .filter(|prefix| !prefix.is_empty())
        {
            Some(prefix) => format!("{prefix}\n{command}"),
            None => command,
        };
        let spawn = self.spawn_context(command);
        if params.get("run_in_background").and_then(Value::as_bool) == Some(true) {
            return self.start_background(label, spawn, signal.as_ref()).await;
        }
        let output = OutputAccumulator::new(OutputAccumulatorOptions {
            temp_directory: self
                .options
                .spill_directory
                .clone()
                .unwrap_or_else(std::env::temp_dir),
            temp_file_prefix: TEMP_FILE_PREFIX.to_owned(),
            ..OutputAccumulatorOptions::default()
        })
        .map_err(|error| error.to_string())?;
        let stream = Arc::new(Mutex::new(Stream {
            output,
            accepting: true,
            dirty: false,
            last_update_at: None,
            timer: None,
        }));
        let notify = Arc::new(Notify::new());

        on_update.update(AgentToolResult::default());

        let on_data: BashDataCallback = {
            let stream = Arc::clone(&stream);
            let notify = Arc::clone(&notify);
            let on_update = on_update.clone();
            Arc::new(move |data: &[u8]| {
                let update = {
                    let mut stream = lock(&stream);
                    if !stream.accepting {
                        return;
                    }
                    // A failed write poisons the accumulator; `finish`
                    // reports it once the command settles.
                    if stream.output.append(data).is_err() {
                        return;
                    }
                    stream.schedule(&notify)
                };
                if let Some(update) = update {
                    on_update.update(update);
                }
            })
        };

        let started_at = Instant::now();
        let exec = self.operations.exec(
            spawn.command,
            spawn.cwd,
            BashExecOptions {
                on_data,
                signal,
                timeout,
                env: Some(spawn.env),
            },
        );
        tokio::pin!(exec);
        let outcome = loop {
            let deadline = lock(&stream).timer;
            tokio::select! {
                biased;
                outcome = &mut exec => break outcome,
                () = sleep_until(deadline) => {
                    let update = {
                        let mut stream = lock(&stream);
                        stream.timer = None;
                        stream.take_update()
                    };
                    if let Some(update) = update {
                        on_update.update(update);
                    }
                }
                () = notify.notified() => {}
            }
        };

        // Pi's `finishOutput`.
        let (snapshot, last_line_bytes) = {
            let finished = {
                let mut stream = lock(&stream);
                stream.accepting = false;
                stream.timer = None;
                stream.output.finish().map(|()| stream.take_update())
            };
            let update = finished.map_err(|error| error.to_string())?;
            if let Some(update) = update {
                on_update.update(update);
            }
            let stream = lock(&stream);
            let snapshot = stream
                .output
                .snapshot()
                .map_err(|error| error.to_string())?;
            (snapshot, stream.output.last_line_bytes())
        };

        let exit_code = match outcome {
            Ok(exit_code) => exit_code,
            Err(error) => {
                let (text, _) = format_output(&snapshot, last_line_bytes, "");
                return Err(match error {
                    BashExecError::Aborted => append_status(&text, "Command aborted"),
                    BashExecError::TimedOut(seconds) => append_status(
                        &text,
                        &format!(
                            "Command timed out after {} seconds",
                            js_number_to_string(seconds)
                        ),
                    ),
                    BashExecError::Failed(message) => message,
                });
            }
        };
        let (output_text, details) = format_output(&snapshot, last_line_bytes, "(no output)");
        let Some(exit_code) = exit_code else {
            return Err(append_status(
                &output_text,
                "Command terminated without an exit code",
            ));
        };
        let wall_time = wall_time_seconds(started_at.elapsed());
        let full_output = lock(&stream)
            .output
            .read_full_output(STRUCTURED_OUTPUT_MAX_BYTES)
            .map_err(|error| error.to_string())?;
        let mut structured = Map::new();
        structured.insert("output".to_owned(), Value::String(full_output.content));
        structured.insert("truncated".to_owned(), Value::Bool(full_output.truncated));
        if full_output.truncated
            && let Some(path) = &snapshot.full_output_path
        {
            structured.insert(
                "full_output_path".to_owned(),
                Value::String(path.display().to_string()),
            );
        }
        structured.insert("exit_code".to_owned(), Value::from(exit_code));
        structured.insert("wall_time_seconds".to_owned(), wall_time);
        if exit_code != 0 {
            return Ok(AgentToolResult {
                content: text_result(append_status(
                    &output_text,
                    &format!("Command exited with code {exit_code}"),
                )),
                details,
                structured_content: Some(Value::Object(structured)),
                is_error: true,
                ..AgentToolResult::default()
            });
        }
        Ok(AgentToolResult {
            content: text_result(output_text),
            details,
            structured_content: Some(Value::Object(structured)),
            ..AgentToolResult::default()
        })
    }
}

impl Shared {
    /// `tool-bash`'s background branch: the command becomes a `bash` job
    /// that streams its output into the job, with no timeout, and the call
    /// returns its id. A stop of the job aborts the command, which kills
    /// its tree; the job ends once `exec` has settled it.
    async fn start_background(
        &self,
        label: String,
        spawn: BashSpawnContext,
        signal: Option<&AbortSignal>,
    ) -> Result<AgentToolResult, String> {
        let Some(jobs) = &self.options.jobs else {
            return Err("run_in_background is not available in this session".to_owned());
        };
        // The caller owns cancellation until the registry admits the job.
        if signal.is_some_and(AbortSignal::aborted) {
            return Err("Command aborted".to_owned());
        }
        let operations = Arc::clone(&self.operations);
        let spec = JobSpec {
            kind: "bash",
            label,
            output: OutputMode::Stream,
        };
        let id = jobs
            .start(spec, move |context| run_job(context, operations, spawn))
            .map_err(|error| error.to_string())?;
        jobs.wait_admitted(id).await?;
        let label = jobs.get(id).map(|job| job.label).unwrap_or_default();
        // The result links the tool call to the job whose start is
        // already durable in an attached session
        // ([`crate::agent_session::job_notices`]).
        Ok(AgentToolResult {
            content: text_result(format!("started background job {id}")),
            details: Some(
                json!({ BACKGROUND_JOB_DETAIL: { "id": id.to_string(), "label": label } }),
            ),
            ..AgentToolResult::default()
        })
    }
}

/// Runs a background command as its job, `processOutcome`'s mapping: an
/// exit, nonzero included, is `completed` with its code; a stop is
/// `killed`; a failure to run is `failed` with its message.
async fn run_job(
    context: JobContext,
    operations: Arc<dyn BashOperations>,
    spawn: BashSpawnContext,
) -> JobOutcome {
    let killed = || JobOutcome::killed("killed before exit");
    if context.signal().aborted() {
        return killed();
    }
    let output = context.output().cloned();
    let on_data: BashDataCallback = Arc::new(move |data: &[u8]| {
        if let Some(output) = &output {
            output.append(data);
        }
    });
    let exec = operations.exec(
        spawn.command,
        spawn.cwd,
        BashExecOptions {
            on_data,
            signal: Some(context.signal().clone()),
            timeout: None,
            env: Some(spawn.env),
        },
    );
    match exec.await {
        Ok(Some(code)) => JobOutcome::completed(format!("exit code: {code}")),
        Ok(None) | Err(BashExecError::Aborted | BashExecError::TimedOut(_)) => killed(),
        Err(BashExecError::Failed(message)) => JobOutcome::failed(message),
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}
