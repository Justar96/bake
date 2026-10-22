//! The `bake` agent command line: arguments, session selection, model
//! selection, and print mode.
//!
//! Ported from Pi `packages/coding-agent/src/main.ts` (v1.1.0):
//! `readPipedStdin`, `resolveAppMode`, `createSessionManager`,
//! `resolveSessionPath`, `buildSessionOptions`, and the print-mode branch of
//! `main`, and from `src/cli/initial-message.ts`. Settings, `models.json`,
//! `auth.json`, and sessions come from the Bake home (D25), the one
//! [`CliEnvironment::home`] names, never the process's own. The global
//! `AGENTS.md` and the user's skills come from the universal `~/.agents`
//! under [`CliEnvironment::user_home`], and a project's `AGENTS.md` files
//! and `.agents/skills` from the session's working directory and its
//! parents ([`crate::skills`]).
//! Diagnostics go to standard error as `Error: ` and `Warning: ` lines
//! without Pi's colors, except Pi's bare `No session found matching`
//! line, and every failure exits with code 1, as in Pi.
//!
//! Bake composes its permission layer (D24, [`crate::permissions`]) here:
//! the session's permission is settled and stored in its session file
//! before the agent starts, `bash`, `write`, and `edit` are wrapped by the
//! permission engine over [`CliEnvironment::sandbox_provider`], approvals
//! go to [`CliEnvironment::approver`] (interactive mode's terminal; print
//! mode has none, so they fail closed), and the permission is stated in the
//! system prompt's `permissions` section.
//!
//! [`start`] is the setup print mode and interactive mode share: the
//! binary runs print mode through [`run`] and hands an interactive
//! session to the terminal frontend (`bake-tui`), with a
//! [`SessionFactory`] that builds the sessions `/new`, `/resume`, and
//! `/fork` switch to from the same arguments and the running session's settings and
//! models, without touching the running session (adapted from Pi v1.2.0
//! `core/agent-session-runtime.ts`).
//!
//! Every session it builds, the first and each replacement, reads
//! `$BAKE_HOME/mcp.json` when its resources load, through
//! [`load_home_mcp_config`](crate::mcp::load_home_mcp_config)'s owner rules,
//! and starts the file's servers once the session exists
//! ([`AgentSession::start_mcp`]), under the session's standing permission
//! and sandbox provider, unless `--no-mcp` is given (Pi v1.3.0's flag). A
//! problem with the file or an entry is a startup warning; each server's own
//! failure is reported by print mode once the first prompt's startup wait
//! ends ([`run`]). The session owns its servers from their start, so every
//! later setup failure, and every stop request ([`CliEnvironment::interrupt`]:
//! the binary's `SIGINT`, `SIGTERM`, and `SIGHUP`) while setup waits for
//! piped input or the servers' startup, or while a prompt runs, shuts it
//! down, and with it the servers, before print mode returns. `--tools`,
//! `--exclude-tools`, and `--no-tools` restrict MCP tools as they restrict
//! built-in ones, whenever a server publishes them
//! ([`crate::mcp::McpToolFilter`]); with `--no-tools` alone no server
//! starts. A [`Replacement`] starts its servers only once the session it
//! replaces is shut down ([`Replacement::start_servers`]).
//!
//! Every session it builds also reads the hooks files `settings.json`'s
//! `hooks` member names, under the same owner rules
//! ([`crate::hooks::load_hook_files`]); a problem with one is a startup
//! warning. The session runs them ([`AgentSession::set_hooks`]) under its
//! standing permission, sandbox provider, and approver; a subagent runs
//! only the tool hooks.
//!
//! `--resume` opens the session picker the terminal already has
//! ([`Started::resume`]), where Pi opens its own before the session is
//! made; print mode, which has no terminal for it, refuses the flag.
//!
//! RPC mode ([`crate::rpc`]) takes the same setup through [`start_rpc`],
//! which reads no piped standard input: there the stream carries the
//! protocol. [`run_rpc`] runs its loop.
//!
//! Not ported (deferred): theme and extension flags, extension tools, and
//! the missing-session-cwd prompt.

pub mod args;
pub mod auth_command;
pub mod export;
pub mod file_processor;
pub mod list_models;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bake_ai::ApiRegistry;
use bake_sandbox::SandboxProvider;

use crate::agent_session::{
    AgentSession, AgentSessionOptions, ForkPosition, format_no_models_available_message,
};
use crate::cli::file_processor::{ProcessFileOptions, process_file_arguments};
use crate::cli::list_models::list_models;
use crate::home::sessions_dir;
use crate::model_registry::ModelRegistry;
use crate::model_registry::resolver::{
    ScopedModel, resolve_cli_model, resolve_model_scope_from_models,
};
use crate::permissions::{
    Approver, NoApprover, PermissionInputs, ToolPermissions, owned_sandbox_file_tools,
    prepare_session_permission, sandboxed_bash_tool,
};
use crate::plugins::installed::PluginHome;
use crate::plugins::wasm::WasmSetup;
use crate::print_mode::{
    PrintModeOptions, PrintOutput, PrintRunRecovery, SharedWriter, run_print_mode, write_out,
};
use crate::prompt_templates::{LoadPromptTemplatesOptions, load_prompt_templates};
use crate::session::file::default_session_dir_path;
use crate::session::id::assert_valid_session_id;
use crate::session::paths::{lexical_normalize, normalize_path};
use crate::session::time::parse_date_ms;
use crate::session::{NewSessionOptions, SessionManager};
use crate::settings::{Settings, SettingsStore};
use crate::skills::{LoadSkillsOptions, load_skills};
use crate::system_prompt::{
    Environment, SystemPromptOptions, load_project_context_files, resolve_prompt_input,
    user_agents_dir,
};
use crate::tools::index::{
    NoTools, ToolSelectionOptions, ToolsOptions, create_bake_tool_definitions, select_tools,
};
use crate::tools::read::ReadToolOptions;

pub use args::{
    Args, Diagnostic, DiagnosticKind, Mode, normalize_session_name, parse_args,
    unknown_flags_message,
};

/// The most piped standard input read for a prompt.
pub const MAX_STDIN_BYTES: u64 = 64 * 1024 * 1024;

/// Everything the command line reads from its process, so tests can supply
/// their own.
pub struct CliEnvironment {
    /// The working directory.
    pub cwd: PathBuf,
    /// The Bake home; `None` when it cannot be resolved.
    pub home: Option<PathBuf>,
    /// The user's home directory, which holds `.agents`; `None` when it
    /// cannot be resolved.
    pub user_home: Option<PathBuf>,
    /// How to run `bake docs`, which the prompt and startup errors name
    /// ([`AgentSessionOptions::docs_command`]).
    pub docs_command: String,
    /// Whether standard input is a terminal.
    pub stdin_is_terminal: bool,
    /// Standard input, read when it is not a terminal.
    pub stdin: Box<dyn Read + Send>,
    /// Standard output.
    pub stdout: SharedWriter,
    /// Standard error.
    pub stderr: SharedWriter,
    /// Providers to add below `models.json`, the CLIPROXY seam
    /// ([`ModelRegistry::add_provider_config`]): id, config, and key.
    pub extra_providers: Vec<(String, serde_json::Value, Option<String>)>,
    /// The default model the 0.3 home saved (D25), used only when
    /// `settings.json` names neither a default provider nor a default model.
    pub imported_default_model: Option<crate::cliproxyapi::DefaultModel>,
    /// The default permission preset the 0.3 home saved (D25), used only
    /// when `settings.json` names none.
    pub imported_default_preset: Option<String>,
    /// Confines bash under each call's policy (D24): the binary passes
    /// [`bake_sandbox::default_provider`], which fails every confined call
    /// closed on a host without a backend.
    pub sandbox_provider: Arc<dyn SandboxProvider>,
    /// Who answers approval requests (D24): interactive mode passes its
    /// terminal's [`crate::permissions::ApprovalBroker`]; `None`, as in
    /// print mode, means nobody can, so every escalation fails closed and
    /// the system prompt states the `never` policy.
    pub approver: Option<Arc<dyn Approver>>,
    /// How sessions run the Wasm plugins the Bake home enables
    /// ([`crate::plugins::installed`]): the binary passes itself as the
    /// helper and one compile queue for the process. Without a helper
    /// command, each enabled plugin blocks the tools it lists.
    pub plugins: WasmSetup,
    /// Startup warnings found before [`run`], printed with its own.
    pub warnings: Vec<String>,
    /// Resolves with an exit code when the process should stop, such as on
    /// `SIGTERM`; print mode then shuts the session down and returns it.
    pub interrupt: Interrupt,
}

/// What the 0.3 home's CLIProxyAPI route contributes to a run (D25).
#[derive(Debug, Default)]
pub struct ImportedStartup {
    /// For [`CliEnvironment::extra_providers`].
    pub providers: Vec<(String, serde_json::Value, Option<String>)>,
    /// For [`CliEnvironment::imported_default_model`].
    pub default_model: Option<crate::cliproxyapi::DefaultModel>,
    /// For [`CliEnvironment::imported_default_preset`].
    pub default_preset: Option<String>,
    /// For [`CliEnvironment::warnings`]; never holds the key.
    pub warnings: Vec<String>,
}

/// Reads the CLIProxyAPI route, key, and default model from the Bake home
/// at `home`, read-only, through
/// [`import_cliproxyapi`](crate::cliproxyapi::import_cliproxyapi), and the
/// default permission preset through
/// [`import_default_preset`](crate::permissions::import::import_default_preset).
/// A home without them contributes nothing; an unreadable one contributes
/// a warning, so a `models.json` route still runs.
///
/// A `bake` sign-in ([`crate::cliproxyapi::account`]) comes first:
/// while `settings.json` has a `cliproxyapi` member, the 0.3 route and key
/// are not imported. The account itself is applied to the registry later,
/// from its own read ([`apply_account`]).
pub fn import_startup(
    home: &std::path::Path,
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> ImportedStartup {
    use crate::cliproxyapi::account::{AccountView, read_account};
    use crate::cliproxyapi::{CLIPROXYAPI_ID, import_cliproxyapi};
    let mut startup = ImportedStartup::default();
    let owned = !matches!(read_account(home), AccountView::Absent);
    match import_cliproxyapi(home, env) {
        Ok(imported) => {
            startup.default_model = imported.default_model;
            if let (false, Some(route)) = (owned, imported.route) {
                let key = route.key.map(|key| key.key.expose().to_owned());
                let mut config = route.provider.config;
                // The config names the key's variable (`$CLIPROXYAPI_API_KEY`),
                // which outranks a literal key; the importer has already
                // resolved it from the environment or the credentials file.
                if key.is_some()
                    && let Some(config) = config.as_object_mut()
                {
                    config.remove("apiKey");
                }
                startup
                    .providers
                    .push((CLIPROXYAPI_ID.to_owned(), config, key));
            }
        }
        Err(error) => startup
            .warnings
            .push(format!("Could not import the CLIProxyAPI route: {error}")),
    }
    match crate::permissions::import::import_default_preset(home) {
        Ok(preset) => startup.default_preset = preset,
        Err(error) => startup.warnings.push(format!(
            "Could not import the default permission preset: {error}"
        )),
    }
    startup
}

/// Applies the home's CLIProxyAPI account to `registry`, from one
/// coherent read ([`crate::cliproxyapi::account::read_account`]); returns
/// a warning when the account is off or its route is refused. Blocks.
pub fn apply_account(home: &std::path::Path, registry: &mut ModelRegistry) -> Option<String> {
    crate::cliproxyapi::account::CliProxyAccount::new(home)
        .apply_to(registry)
        .unwrap_or_else(Some)
}

/// A stop request: resolves with the exit code to stop with.
pub type Interrupt = std::pin::Pin<Box<dyn std::future::Future<Output = i32> + Send>>;

/// An [`Interrupt`] that never fires.
pub fn no_interrupt() -> Interrupt {
    Box::pin(std::future::pending())
}

/// What `bake` should do with its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Print help.
    Help,
    /// Print the version.
    Version,
    /// Write a session file as HTML and exit (`--export`): the session
    /// file, then the output path the first message named, if any.
    Export(String, Option<String>),
    /// Run print mode.
    Print(Box<Args>, Mode),
    /// Run RPC mode (`--mode rpc`): Pi's JSONL protocol on standard input
    /// and output, whether or not the streams are terminals
    /// ([`crate::rpc`]).
    Rpc(Box<Args>),
    /// Run interactive mode: both standard streams are terminals and
    /// neither `--print` nor `--mode` was given.
    Interactive(Box<Args>),
    /// Stop with this exit code; the diagnostics are written.
    Exit(i32),
}

/// Writes Pi's diagnostic lines.
fn report(stderr: &SharedWriter, diagnostics: &[Diagnostic]) {
    for diagnostic in diagnostics {
        let prefix = match diagnostic.kind {
            DiagnosticKind::Error => "Error",
            DiagnosticKind::Warning => "Warning",
        };
        write_out(stderr, &format!("{prefix}: {}\n", diagnostic.message));
    }
}

fn error(stderr: &SharedWriter, message: &str) -> i32 {
    write_out(stderr, &format!("Error: {message}\n"));
    1
}

fn warn(stderr: &SharedWriter, message: &str) {
    write_out(stderr, &format!("Warning: {message}\n"));
}

/// Pi's argument checks and `resolveAppMode`, before anything is loaded:
/// print mode when asked for or when a standard stream is not a terminal,
/// and interactive mode otherwise.
pub fn plan(
    args: &[String],
    stdin_is_terminal: bool,
    stdout_is_terminal: bool,
    stderr: &SharedWriter,
) -> Action {
    let mut parsed = parse_args(args);
    report(stderr, &parsed.diagnostics);
    if parsed
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.kind == DiagnosticKind::Error)
    {
        return Action::Exit(1);
    }
    if parsed.version {
        return Action::Version;
    }
    // Pi's `main` exports before it reads any other flag, `--help`
    // included, so `--export` with anything else still exports.
    if let Some(input) = parsed.export.take() {
        return Action::Export(input, parsed.messages.first().cloned());
    }
    if parsed.help {
        return Action::Help;
    }
    if let Some(message) = unknown_flags_message(&parsed.unknown_flags) {
        return Action::Exit(error(stderr, &message));
    }
    if parsed.project_trust_override.is_some() {
        return Action::Exit(error(stderr, PROJECT_TRUST_REFUSED));
    }
    if let Some(message) = validate_fork_flags(&parsed) {
        return Action::Exit(error(stderr, &message));
    }
    if let Some(message) = validate_session_id_flags(&parsed) {
        return Action::Exit(error(stderr, &message));
    }
    if let Some(message) = validate_run_recovery_flags(&parsed) {
        return Action::Exit(error(stderr, &message));
    }
    if let Some(name) = &parsed.name
        && normalize_session_name(name).is_none()
    {
        return Action::Exit(error(stderr, "--name requires a non-empty value"));
    }
    if parsed.mode == Some(Mode::Rpc) {
        if let Some(message) = validate_rpc_flags(&parsed) {
            return Action::Exit(error(stderr, &message));
        }
        return Action::Rpc(Box::new(parsed));
    }
    let mode = match parsed.mode {
        Some(mode) => mode,
        None if parsed.print
            || parsed.list_models.is_some()
            || !stdin_is_terminal
            || !stdout_is_terminal =>
        {
            Mode::Text
        }
        None => {
            if parsed.recoverable || parsed.resume_run.is_some() {
                return Action::Exit(error(
                    stderr,
                    "--recoverable and --resume-run require print mode (--print or --mode)",
                ));
            }
            if parsed.max_model_calls.is_some() || parsed.run_timeout_ms.is_some() {
                return Action::Exit(error(
                    stderr,
                    "--max-model-calls and --run-timeout-ms require print mode (--print or --mode)",
                ));
            }
            return Action::Interactive(Box::new(parsed));
        }
    };
    // Pi's `--resume` opens a terminal session picker from
    // `createSessionManager`, whatever the mode; print mode here has no
    // terminal to open it in, so it is refused instead of ignored.
    if resumes_at_startup(&parsed) {
        return Action::Exit(error(
            stderr,
            "--resume opens the session picker, which print mode has no terminal for; use --session or --continue",
        ));
    }
    Action::Print(Box::new(parsed), mode)
}

/// The flags RPC mode cannot serve: the print-mode run limits, which it
/// takes no prompt to spend, and Pi's refusal of `@file` arguments, since
/// standard input carries the protocol rather than a prompt.
fn validate_rpc_flags(args: &Args) -> Option<String> {
    if resumes_at_startup(args) {
        return Some(
            "--resume opens the session picker, which RPC mode has no terminal for; use --session or --continue"
                .to_owned(),
        );
    }
    if !args.file_args.is_empty() {
        return Some("@file arguments are not supported in RPC mode".into());
    }
    if args.recoverable || args.resume_run.is_some() {
        return Some(
            "--recoverable and --resume-run require print mode (--print or --mode text or json)"
                .into(),
        );
    }
    if args.max_model_calls.is_some() || args.run_timeout_ms.is_some() {
        return Some(
            "--max-model-calls and --run-timeout-ms require print mode (--print or --mode text or json)"
                .into(),
        );
    }
    if args.list_models.is_some() {
        return Some("--list-models cannot be combined with --mode rpc".into());
    }
    None
}

fn validate_run_recovery_flags(args: &Args) -> Option<String> {
    if !args.recoverable && args.resume_run.is_none() {
        return None;
    }
    if args.no_session {
        return Some("Run recovery requires a persisted session; remove --no-session.".into());
    }
    if args.recoverable && args.resume_run.is_some() {
        return Some("--recoverable and --resume-run cannot be combined.".into());
    }
    if args.resume_run.is_some() {
        if !args.session.as_deref().is_some_and(is_session_path) {
            return Some(
                "--resume-run requires --session with an explicit session file path.".into(),
            );
        }
        if args.continue_session || args.fork.is_some() || args.session_id.is_some() {
            return Some(
                "--resume-run cannot be combined with --continue, --fork, or --session-id.".into(),
            );
        }
        if args.max_model_calls.is_some() || args.run_timeout_ms.is_some() {
            return Some(
                "--resume-run restores the recorded budget; new run limits cannot replace it."
                    .into(),
            );
        }
        if !args.messages.is_empty() || !args.file_args.is_empty() {
            return Some("--resume-run takes no new prompt or file arguments.".into());
        }
    } else if args.messages.len() > 1 {
        return Some("--recoverable accepts one prompt per invocation.".into());
    }
    None
}

/// Pi's `readPipedStdin`: the trimmed input, or `None` when empty.
fn read_piped_stdin(stdin: Box<dyn Read + Send>) -> Result<Option<String>, String> {
    let mut bytes = Vec::new();
    stdin
        .take(MAX_STDIN_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Could not read standard input: {error}"))?;
    if bytes.len() as u64 > MAX_STDIN_BYTES {
        return Err(format!(
            "Standard input is larger than {MAX_STDIN_BYTES} bytes"
        ));
    }
    let text = String::from_utf8_lossy(&bytes);
    let trimmed = text.trim();
    Ok((!trimmed.is_empty()).then(|| trimmed.to_owned()))
}

/// Pi's `validateForkFlags`.
fn validate_fork_flags(parsed: &Args) -> Option<String> {
    parsed.fork.as_ref()?;
    let mut conflicts = Vec::new();
    if parsed.session.is_some() {
        conflicts.push("--session");
    }
    if parsed.continue_session {
        conflicts.push("--continue");
    }
    if parsed.resume {
        conflicts.push("--resume");
    }
    if parsed.no_session {
        conflicts.push("--no-session");
    }
    (!conflicts.is_empty())
        .then(|| format!("--fork cannot be combined with {}", conflicts.join(", ")))
}

/// Pi's `validateSessionIdFlags`.
fn validate_session_id_flags(parsed: &Args) -> Option<String> {
    let session_id = parsed.session_id.as_ref()?;
    let mut conflicts = Vec::new();
    if parsed.session.is_some() {
        conflicts.push("--session");
    }
    if parsed.continue_session {
        conflicts.push("--continue");
    }
    if parsed.resume {
        conflicts.push("--resume");
    }
    if !conflicts.is_empty() {
        return Some(format!(
            "--session-id cannot be combined with {}",
            conflicts.join(", ")
        ));
    }
    assert_valid_session_id(session_id)
        .err()
        .map(|error| error.to_string())
}

/// Pi's `buildInitialMessage`: stdin, `@file` text, then the first CLI
/// message; later CLI messages stay separate prompts.
fn build_initial_message(
    stdin: Option<String>,
    file_text: &str,
    messages: &mut Vec<String>,
) -> Option<String> {
    let mut parts: Vec<String> = stdin.into_iter().collect();
    if !file_text.is_empty() {
        parts.push(file_text.to_owned());
    }
    if !messages.is_empty() {
        parts.push(messages.remove(0));
    }
    (!parts.is_empty()).then(|| parts.concat())
}

/// Pi's `resolveSessionPath` result.
enum ResolvedSession {
    Path(PathBuf),
    Global { path: PathBuf, cwd: String },
    NotFound,
}

fn is_session_path(arg: &str) -> bool {
    arg.contains('/') || arg.contains('\\') || arg.ends_with(".jsonl")
}

/// Pi's `resolveSessionPath`: a path, else a local id or id prefix, else
/// one in another project. Without `session_dir`, sessions are looked up
/// under `home`.
fn resolve_session_path(
    arg: &str,
    cwd: &str,
    session_dir: Option<&Path>,
    home: &Path,
) -> ResolvedSession {
    if is_session_path(arg) {
        // Pi's `resolvePath(sessionArg, cwd)`: `~` and Windows shell paths
        // expanded, then absolute against `cwd` with `.` and `..` removed.
        let normalized = normalize_path(arg);
        let joined = if normalized.is_absolute() {
            normalized
        } else {
            normalize_path(cwd).join(normalized)
        };
        return ResolvedSession::Path(lexical_normalize(&joined));
    }
    let local_dir =
        session_dir.map_or_else(|| default_session_dir_path(cwd, home), Path::to_path_buf);
    if let Ok(Some(exact)) = SessionManager::find_by_id(cwd, arg, Some(&local_dir)) {
        return ResolvedSession::Path(exact);
    }
    let local = SessionManager::list(cwd, Some(&local_dir), None, None).unwrap_or_default();
    if let Some(found) = local
        .iter()
        .find(|info| info.id == arg)
        .or_else(|| local.iter().find(|info| info.id.starts_with(arg)))
    {
        return ResolvedSession::Path(found.path.clone());
    }
    let all = match session_dir {
        Some(dir) => SessionManager::list_all(Some(dir), None, None),
        None => SessionManager::list_all_in(&sessions_dir(home), None, None),
    }
    .unwrap_or_default();
    match all
        .iter()
        .find(|info| info.id == arg)
        .or_else(|| all.iter().find(|info| info.id.starts_with(arg)))
    {
        Some(found) => ResolvedSession::Global {
            path: found.path.clone(),
            cwd: found.cwd.clone(),
        },
        None => ResolvedSession::NotFound,
    }
}

/// Why the session store could not start: Pi prints most failures as
/// errors, and a session id it cannot find as a bare line.
#[derive(Debug, PartialEq, Eq)]
enum SessionStartError {
    Error(String),
    Plain(String),
}

/// Why `--approve` and `--no-approve` are refused. Pi's project trust
/// decides whether a project's own files are loaded, and `bake` has no
/// such decision: a project's `AGENTS.md` files
/// ([`crate::system_prompt`]), its `.agents/skills` ([`crate::skills`]),
/// and its `.bake/prompts` ([`crate::prompt_templates`]) are always
/// loaded, and it reads no project settings ([`crate::settings`]) and no
/// project `mcp.json` ([`crate::mcp`]). Bake's `--approval` is a different
/// thing: the session's approval policy (D24).
pub const PROJECT_TRUST_REFUSED: &str = "--approve and --no-approve need project trust, which bake does not have: a project's AGENTS.md, .agents/skills, and .bake/prompts are always loaded, and no project settings or mcp.json is read. For the session's approval policy, see --approval";

/// `BAKE_OFFLINE`, read before [`DSH_OFFLINE_ENV`].
pub const BAKE_OFFLINE_ENV: &str = "BAKE_OFFLINE";
/// `DSH_OFFLINE`, read when `BAKE_OFFLINE` is unset or blank.
pub const DSH_OFFLINE_ENV: &str = "DSH_OFFLINE";

/// Pi's `offlineMode`: `--offline`, or a truthy environment setting. Where
/// Pi reads `PI_OFFLINE`, Bake reads [`BAKE_OFFLINE_ENV`] and then
/// [`DSH_OFFLINE_ENV`], as it reads its home ([`crate::home`]); a blank
/// value counts as unset. The value's test is Pi's `isTruthyEnvFlag`: `1`,
/// `true`, or `yes`, ignoring case.
///
/// `bake` runs one startup network operation, the interactive
/// terminal's once-per-launch CLIProxyAPI catalog refresh
/// (`bake_tui`'s `startup_refresh`), and this stops it. Print mode starts
/// no network operation of its own: its models, keys, settings, sessions,
/// skills, context files, and `mcp.json` are files, an `mcp.json` server
/// reached over HTTP is refused before it is started, and the web tools
/// and model requests run only for a prompt. Pi's version check, which
/// `PI_OFFLINE` also stops, has no equivalent here.
pub fn offline_mode(args: &Args, env: impl Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    if args.offline {
        return true;
    }
    [BAKE_OFFLINE_ENV, DSH_OFFLINE_ENV]
        .into_iter()
        .filter_map(env)
        .map(|value| value.to_string_lossy().trim().to_ascii_lowercase())
        .find(|value| !value.is_empty())
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes"))
}

/// Whether `--resume` opens the session picker for this run, as Pi's
/// `createSessionManager` reaches its `parsed.resume` branch: `--no-session`,
/// `--fork`, and `--session` are taken first and leave `--resume` unused.
/// `--session-id` and `--fork` with `--resume` are refused before this
/// ([`validate_session_id_flags`], [`validate_fork_flags`]).
pub fn resumes_at_startup(args: &Args) -> bool {
    args.resume && !args.no_session && args.fork.is_none() && args.session.is_none()
}

/// Pi's `createSessionManager` for the ported flags. Without
/// `session_dir`, sessions live under `home`, never the process's own Bake
/// home. Blocking.
fn create_session_manager(
    args: &Args,
    cwd: &str,
    session_dir: Option<&Path>,
    home: &Path,
    warnings: &mut Vec<String>,
) -> Result<SessionManager, SessionStartError> {
    let failed = |error: crate::session::SessionError| SessionStartError::Error(error.to_string());
    if args.no_session {
        return SessionManager::in_memory(
            Some(cwd),
            NewSessionOptions {
                id: args.session_id.clone(),
                parent_session: None,
            },
            Vec::new(),
        )
        .map_err(failed);
    }
    let dir = match session_dir {
        Some(dir) => dir.to_path_buf(),
        None => default_session_dir_path(cwd, home),
    };
    if let Some(fork) = &args.fork {
        if let Some(session_id) = &args.session_id
            && SessionManager::find_by_id(cwd, session_id, Some(&dir))
                .ok()
                .flatten()
                .is_some()
        {
            return Err(SessionStartError::Plain(format!(
                "Session already exists with id '{session_id}'"
            )));
        }
        return match resolve_session_path(fork, cwd, session_dir, home) {
            ResolvedSession::Path(path) | ResolvedSession::Global { path, .. } => {
                SessionManager::fork_from(
                    &path,
                    cwd,
                    Some(&dir),
                    NewSessionOptions {
                        id: args.session_id.clone(),
                        parent_session: None,
                    },
                )
                .map_err(failed)
            }
            ResolvedSession::NotFound => Err(SessionStartError::Plain(format!(
                "No session found matching '{fork}'"
            ))),
        };
    }
    if let Some(arg) = &args.session {
        return match resolve_session_path(arg, cwd, session_dir, home) {
            ResolvedSession::Path(path) => {
                if args.resume_run.is_some() && !path.is_file() {
                    return Err(SessionStartError::Error(
                        "--resume-run requires an existing session file.".into(),
                    ));
                }
                SessionManager::open(&path, Some(&dir), None).map_err(failed)
            }
            // Neither mode prompts whether to fork yet; point at `--fork`.
            ResolvedSession::Global { cwd, .. } => Err(SessionStartError::Error(format!(
                "Session found in different project: {cwd}. Use --fork {arg} to fork it into this directory."
            ))),
            ResolvedSession::NotFound => Err(SessionStartError::Plain(format!(
                "No session found matching '{arg}'"
            ))),
        };
    }
    // Pi's `createSessionManager` takes `--resume` before `--continue`:
    // the session picker decides what opens, and nothing else does. The
    // terminal opens the picker over this new session, which writes no
    // file until it holds a message ([`Started::resume`]).
    if resumes_at_startup(args) {
        return SessionManager::create(cwd, Some(&dir), NewSessionOptions::default())
            .map_err(failed);
    }
    if args.continue_session {
        return SessionManager::continue_recent(cwd, Some(&dir)).map_err(failed);
    }
    if let Some(session_id) = &args.session_id {
        if let Some(existing) =
            SessionManager::find_by_id(cwd, session_id, Some(&dir)).map_err(failed)?
        {
            return SessionManager::open(&existing, Some(&dir), None).map_err(failed);
        }
        warnings.push(format!(
            "No project session found with id '{session_id}'; creating a new session with that id."
        ));
    }
    SessionManager::create(
        cwd,
        Some(&dir),
        NewSessionOptions {
            id: args.session_id.clone(),
            parent_session: None,
        },
    )
    .map_err(failed)
}

/// A session set up from the command line, ready for print or interactive
/// mode: Pi's `main` up to the mode branch.
pub struct Started {
    /// The session, with its model, tools, permission, and prompt.
    pub session: AgentSession,
    /// The first prompt: piped standard input, `@file` text, and the first
    /// message, as Pi's `buildInitialMessage` joins them.
    pub initial_message: Option<String>,
    /// Images from `@file` arguments, for the first prompt.
    pub initial_images: Vec<bake_ai::ImageContent>,
    /// The later messages, each its own prompt.
    pub messages: Vec<String>,
    /// Standard input, unread: RPC mode's protocol stream. `None` in print
    /// and interactive mode, which read piped input as the first prompt.
    pub stdin: Option<Box<dyn Read + Send>>,
    /// Standard output.
    pub stdout: SharedWriter,
    /// Standard error.
    pub stderr: SharedWriter,
    /// The stop request from [`CliEnvironment::interrupt`].
    pub interrupt: Interrupt,
    /// The Bake home the session's settings and credentials come from.
    pub home: PathBuf,
    /// Builds the sessions `/new` and `/resume` replace this one with.
    pub factory: SessionFactory,
    /// `--resume`: the terminal opens the session picker over this new
    /// session, before its first prompt ([`resumes_at_startup`]). Print
    /// mode never sees it: [`plan`] refuses `--resume` there.
    pub resume: bool,
}

/// Runs `bake` print mode; returns the exit code. Must run inside a
/// multi-threaded Tokio runtime.
pub async fn run(environment: CliEnvironment, args: Args, mode: Mode) -> i32 {
    if let Some(search_pattern) = args.list_models.clone() {
        return run_list_models(environment, search_pattern.as_deref()).await;
    }
    let max_model_calls = args.max_model_calls;
    let run_timeout_ms = args.run_timeout_ms;
    let recovery = match (&args.resume_run, args.recoverable) {
        (Some(run_id), _) => PrintRunRecovery::Resume(run_id.clone()),
        (_, true) => PrintRunRecovery::Start,
        _ => PrintRunRecovery::Disabled,
    };
    let started = match start(environment, args).await {
        Ok(started) => started,
        Err(code) => return code,
    };
    let Started {
        session,
        initial_message,
        initial_images,
        messages,
        stdin: _,
        stdout,
        stderr,
        mut interrupt,
        home: _,
        factory: _,
        resume: _,
    } = started;
    // The first prompt's startup wait, taken here so print mode can report
    // what is not serving before the answer; the prompt then waits no more.
    if let Some(servers) = session.mcp_servers() {
        tokio::select! {
            () = servers.wait_for_startup(|| false) => {}
            code = &mut interrupt => {
                session.shutdown().await;
                return code;
            }
        }
        for problem in mcp_problems(servers) {
            warn(&stderr, &problem);
        }
    }
    let deadline = match run_timeout_ms {
        Some(milliseconds) => match tokio::time::Instant::now()
            .checked_add(std::time::Duration::from_millis(milliseconds))
        {
            Some(deadline) => Some(deadline),
            None => {
                session.shutdown().await;
                return error(
                    &stderr,
                    "--run-timeout-ms exceeds this platform's clock range",
                );
            }
        },
        None => None,
    };
    let options = PrintModeOptions {
        job_wait: crate::print_mode::DEFAULT_JOB_WAIT,
        control: bake_agent::RunControl::new(bake_agent::RunLimits {
            deadline,
            max_model_calls,
        }),
        recovery,
        mode: match mode {
            Mode::Json => PrintOutput::Json,
            // [`plan`] sends `--mode rpc` to [`run_rpc`], never here.
            Mode::Text | Mode::Rpc => PrintOutput::Text,
        },
        initial_message,
        initial_images,
        messages,
    };
    // Pi's print-mode signal handlers: dispose of the runtime, then exit.
    tokio::select! {
        code = run_print_mode(&session, options, stdout, stderr) => code,
        code = interrupt => {
            session.shutdown().await;
            code
        }
    }
}

/// `--list-models`: loads the registry and lists its models.
async fn run_list_models(environment: CliEnvironment, search_pattern: Option<&str>) -> i32 {
    let CliEnvironment {
        home,
        docs_command,
        stdout,
        stderr,
        extra_providers,
        warnings: startup_warnings,
        ..
    } = environment;
    let Some(home) = home else {
        return error(
            &stderr,
            "Cannot find the Bake home: set BAKE_HOME or make the user's home directory resolvable",
        );
    };
    let loaded = tokio::task::spawn_blocking(move || {
        let settings = Settings::load(&home);
        let mut registry = ModelRegistry::load(&home);
        let mut warnings = startup_warnings;
        warnings.extend(settings.warnings);
        for (id, config, key) in extra_providers {
            if let Err(message) = registry.add_provider_config(&id, config, key) {
                warnings.push(message);
            }
        }
        warnings.extend(apply_account(&home, &mut registry));
        if let Some(message) = registry.error() {
            warnings.push(format!("errors loading models.json:\n{message}"));
        }
        (registry, warnings)
    })
    .await;
    let (registry, warnings) = match loaded {
        Ok(loaded) => loaded,
        Err(join) => return error(&stderr, &format!("Startup failed: {join}")),
    };
    for warning in &warnings {
        warn(&stderr, warning);
    }
    list_models(&registry, search_pattern, &docs_command, &stdout);
    0
}

/// Pi's `--export` branch: the page is written, its path is printed, and
/// the program exits 0; a failure is one `Error: ` line and status 1. It
/// reads no settings, credentials, or network, and needs no Bake home.
/// Blocking.
pub fn run_export(
    input: &str,
    output: Option<&str>,
    stdout: &SharedWriter,
    stderr: &SharedWriter,
) -> i32 {
    match export::export_from_file(input, output) {
        Ok(path) => {
            write_out(stdout, &format!("Exported to: {}\n", path.display()));
            0
        }
        Err(message) => error(stderr, &message),
    }
}

/// Runs `bake` RPC mode (`--mode rpc`); returns the exit status. The
/// session is set up as print mode sets it up ([`start_rpc`]), its MCP
/// servers connect in the background, as in Pi, and then
/// [`crate::rpc::run_rpc_mode`] serves the protocol on the environment's
/// standard input and output until that input ends or a stop request
/// arrives. `approvals` is the queue the client answers (D24); without one
/// every escalation fails closed. Must run inside a multi-threaded Tokio
/// runtime.
pub async fn run_rpc(
    environment: CliEnvironment,
    args: Args,
    approvals: Option<crate::permissions::ApprovalBroker>,
) -> i32 {
    let started = match start_rpc(environment, args).await {
        Ok(started) => started,
        Err(code) => return code,
    };
    let Started {
        session,
        initial_message: _,
        initial_images: _,
        // Pi's RPC mode ignores the message arguments too: prompts arrive
        // as `prompt` commands.
        messages: _,
        stdin,
        stdout,
        stderr,
        interrupt,
        home: _,
        factory,
        // `plan` refuses `--resume` in RPC mode.
        resume: _,
    } = started;
    let Some(stdin) = stdin else {
        session.shutdown().await;
        return error(&stderr, "RPC mode needs standard input");
    };
    crate::rpc::run_rpc_mode(
        session,
        crate::rpc::RpcModeOptions {
            stdin,
            stdout,
            stderr,
            approvals,
            factory: Some(factory),
            interrupt,
        },
    )
    .await
}

/// Sets the session up from the command line as Pi's `main` does before
/// it picks a mode: settings, models, the session file and its permission,
/// context files, skills, prompt templates, tools, the system prompt, and
/// the first prompt. Print and interactive mode share it, so both run the
/// same session. Reports a failure on standard error and returns its exit
/// code; nothing has touched the terminal by then. Must run inside a
/// multi-threaded Tokio runtime.
///
/// Without a model, print mode stops here with Pi's no-models message.
pub async fn start(environment: CliEnvironment, args: Args) -> Result<Started, i32> {
    start_with(environment, args, StartKind::Print).await
}

/// [`start`] for interactive mode, which, as Pi's `main` does, starts
/// without a model, or with one that has no key, so the person can sign in
/// from the session; a prompt is then refused with Pi's message. Invalid
/// flags and an explicit `--model` that matches nothing still fail.
pub async fn start_interactive(environment: CliEnvironment, args: Args) -> Result<Started, i32> {
    start_with(environment, args, StartKind::Interactive).await
}

/// [`start`] for RPC mode, which, as Pi's `main` does, reads no piped
/// standard input: that stream carries the protocol, not a prompt. As in
/// print mode, a session without a usable model fails the startup.
pub async fn start_rpc(environment: CliEnvironment, args: Args) -> Result<Started, i32> {
    start_with(environment, args, StartKind::Rpc).await
}

/// Which mode a session is being set up for: what it does without a model,
/// and whether piped standard input is a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartKind {
    /// Print mode: piped input is the first prompt, and no model fails.
    Print,
    /// Interactive mode: no model still opens the session.
    Interactive,
    /// RPC mode: standard input is the protocol, and no model fails.
    Rpc,
}

/// What a session's setup reads besides its session file and model
/// selection: the context files, skills, prompt templates, permission, and
/// prompt overrides, all for the session's working directory. Pi's
/// resource loader and `buildSessionOptions`, without extensions.
struct Resources {
    context_files: Vec<crate::system_prompt::ContextFile>,
    /// The `mcp.json` servers to start; empty under `--no-mcp`.
    mcp_servers: Vec<crate::mcp::McpServerEntry>,
    skills: Vec<crate::skills::Skill>,
    prompt_templates: Vec<crate::prompt_templates::PromptTemplate>,
    permission: crate::permissions::SessionPermission,
    custom_prompt: Option<String>,
    append_system_prompt: Option<String>,
    plugins: crate::plugins::PluginRegistry,
    /// The hooks files `settings.json` names ([`crate::hooks`]).
    hooks: Vec<crate::hooks::HookFile>,
}

/// Settles `session`'s permission (D24), writing any change to its file
/// before anything can run, and loads the resources for its working
/// directory. `permission_flags` applies `--permission`, `--sandbox`, and
/// `--approval`; a resumed session leaves them out and keeps its own.
/// The Wasm plugins the home enables are loaded under `plugins`; an
/// invalid or untrusted `plugins.json`, or a plugins directory inside the
/// session's working directory or a git repository, fails the setup
/// before any tool can run.
/// Blocking.
#[allow(clippy::too_many_arguments)]
fn load_resources(
    session: &mut SessionManager,
    settings: &Settings,
    args: &Args,
    home: &Path,
    user_home: Option<PathBuf>,
    imported_default_preset: Option<&str>,
    plugins: &WasmSetup,
    permission_flags: bool,
    warnings: &mut Vec<String>,
) -> Result<Resources, String> {
    let loaded = PluginHome::new(home)
        .load(plugins, Some(Path::new(session.cwd())))
        .map_err(|error| error.to_string())?;
    warnings.extend(loaded.warnings);
    let permission = prepare_session_permission(
        session,
        &PermissionInputs {
            settings: settings.permission.as_ref(),
            imported_default_preset,
            preset: args.permission.as_deref().filter(|_| permission_flags),
            sandbox: args.sandbox.filter(|_| permission_flags),
            approval: args.approval.filter(|_| permission_flags),
        },
    )?;
    warnings.extend(permission.warnings.iter().cloned());
    let mcp_servers = if args.no_mcp {
        Vec::new()
    } else {
        load_mcp_servers(
            home,
            Path::new(session.cwd()),
            user_home.as_deref(),
            warnings,
        )
    };
    let (hooks, hook_warnings) = crate::hooks::load_hook_files(
        &settings.hooks,
        home,
        Path::new(session.cwd()),
        user_home.as_deref(),
    );
    warnings.extend(hook_warnings.iter().map(|warning| terminal_safe(warning)));
    let context_files = if args.no_context_files {
        Vec::new()
    } else {
        let agents_dir = user_home.as_deref().map(user_agents_dir);
        let (files, context_warnings) =
            load_project_context_files(session.cwd(), agents_dir.as_deref());
        warnings.extend(context_warnings);
        files
    };
    // Pi's print mode does not report skill or prompt-template diagnostics.
    let skills = load_skills(&LoadSkillsOptions {
        cwd: session.cwd().to_owned(),
        user_home,
        skill_paths: args.skills.clone(),
        include_defaults: !args.no_skills,
    })
    .skills;
    let prompt_templates = load_prompt_templates(&LoadPromptTemplatesOptions {
        cwd: session.cwd().to_owned(),
        agent_dir: home.to_path_buf(),
        prompt_paths: args.prompt_templates.clone(),
        include_defaults: !args.no_prompt_templates,
    })
    .templates;
    let custom_prompt =
        resolve_prompt_input(args.system_prompt.as_deref(), session.cwd(), warnings);
    let append_parts: Vec<String> = args
        .append_system_prompt
        .iter()
        .filter_map(|input| resolve_prompt_input(Some(input), session.cwd(), warnings))
        .collect();
    let append_system_prompt = (!append_parts.is_empty()).then(|| append_parts.join("\n\n"));
    Ok(Resources {
        context_files,
        mcp_servers,
        skills,
        prompt_templates,
        permission,
        custom_prompt,
        append_system_prompt,
        plugins: loaded.registry,
        hooks,
    })
}

/// Reads the Bake home's `mcp.json` for a session in `cwd`
/// ([`crate::mcp::load_home_mcp_config`]), adding a warning for the file's
/// refusal or each refused entry. Blocking.
fn load_mcp_servers(
    home: &Path,
    cwd: &Path,
    user_home: Option<&Path>,
    warnings: &mut Vec<String>,
) -> Vec<crate::mcp::McpServerEntry> {
    let config = crate::mcp::load_home_mcp_config(home, cwd, user_home);
    warnings.extend(
        config
            .errors
            .iter()
            .map(|error| terminal_safe(&format!("MCP: {error}"))),
    );
    config.servers
}

/// `text` as one line, with every control character, line breaks
/// included, and every bidirectional format character shown as a `\u{…}`
/// escape, so text a configuration file or a server supplies can neither
/// reach the terminal as an escape sequence nor reorder or forge a line.
pub(crate) fn terminal_safe(text: &str) -> String {
    let mut safe = String::with_capacity(text.len());
    for character in text.chars() {
        // Control characters, and the format characters that reorder
        // or hide text (bidirectional embeddings, overrides, isolates).
        let hidden = matches!(character, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
        if character.is_control() || hidden {
            safe.push_str(&format!("\\u{{{:x}}}", u32::from(character)));
        } else {
            safe.push(character);
        }
    }
    safe
}

/// The first line of each MCP server that is not serving once the startup
/// wait ends, or Pi's note that servers are still connecting.
fn mcp_problems(servers: &crate::mcp::McpServers) -> Vec<String> {
    use crate::mcp::McpServerState;
    let mut problems = Vec::new();
    let mut connecting = false;
    for status in servers.statuses() {
        let error = match status.state {
            McpServerState::Refused { reason } => reason,
            McpServerState::Failed { error } | McpServerState::Disconnected { error } => error,
            McpServerState::Connecting => {
                connecting = true;
                continue;
            }
            _ => continue,
        };
        let line = error.lines().next().unwrap_or_default();
        problems.push(terminal_safe(line));
    }
    if connecting {
        problems.push(
            "MCP servers are still connecting; their tools become available once connected."
                .to_owned(),
        );
    }
    problems
}

/// What [`build_session`] makes a session from.
struct Build {
    session: SessionManager,
    apis: Arc<ApiRegistry>,
    resources: Resources,
    settings: Settings,
    registry: crate::model_registry::ModelRuntime,
    model: Option<bake_ai::Model>,
    thinking_level: Option<bake_ai::ModelThinkingLevel>,
    home: PathBuf,
    docs_command: String,
    sandbox_provider: Arc<dyn SandboxProvider>,
    approver: Option<Arc<dyn Approver>>,
    role: Role,
}

/// Whether a session can start subagents, or is one.
enum Role {
    /// A top-level session; with a factory it starts subagents built as
    /// it was ([`crate::subagents`]).
    Root(Option<Box<SessionFactory>>),
    /// A subagent, which messages its parent over this link and starts no
    /// subagents of its own.
    Child(crate::subagents::ParentLink),
}

/// What a parent's subagents are built with: its setup, model runtime,
/// settings, place, and permission, which every child inherits.
struct ChildSetup {
    factory: SessionFactory,
    apis: Arc<ApiRegistry>,
    registry: crate::model_registry::ModelRuntime,
    settings: Settings,
    cwd: String,
    place: PathBuf,
    persisted: bool,
    parent_id: String,
    parent_file: Option<PathBuf>,
    permission: crate::permissions::SessionPermission,
}

/// The builder of a parent's subagents ([`crate::subagents::ChildBuilder`]).
fn child_builder(setup: ChildSetup) -> crate::subagents::ChildBuilder {
    let setup = Arc::new(setup);
    Arc::new(move |spec| build_child(Arc::clone(&setup), spec))
}

/// Builds one subagent as its parent was built: the parent's arguments
/// with its sandbox mode and approval policy, its settings, model runtime,
/// home, plugins, MCP servers, sandbox provider, and approver, and the
/// child's tools in place of the parent's.
fn build_child(
    setup: Arc<ChildSetup>,
    spec: crate::subagents::ChildSpec,
) -> bake_agent::BoxFuture<'static, Result<AgentSession, String>> {
    Box::pin(async move {
        let crate::subagents::ChildSpec {
            source,
            link,
            route,
        } = spec;
        // The parent's preset, or its knobs when they match none.
        let mut args = setup.factory.args.clone();
        let custom = setup
            .permission
            .table
            .spec(&setup.permission.preset)
            .is_none();
        args.permission = (!custom).then(|| setup.permission.preset.clone());
        args.sandbox = custom.then_some(setup.permission.sandbox);
        args.approval = custom.then_some(setup.permission.approval);
        let loaded = {
            let setup = Arc::clone(&setup);
            let args = args.clone();
            tokio::task::spawn_blocking(move || {
                let mut session = match source {
                    crate::subagents::ChildSource::New { id, label } => {
                        crate::subagents::new_child_store(
                            &setup.cwd,
                            &setup.place,
                            setup.persisted,
                            &setup.parent_id,
                            setup.parent_file.as_deref(),
                            &id,
                            &label,
                        )?
                    }
                    crate::subagents::ChildSource::Open { path } => SessionManager::open(
                        &path,
                        Some(&crate::subagents::children_dir(&setup.place)),
                        None,
                    )
                    .map_err(|error| error.to_string())?,
                };
                let mut warnings = Vec::new();
                let factory = &setup.factory;
                let resources = load_resources(
                    &mut session,
                    &setup.settings,
                    &args,
                    &factory.home,
                    factory.user_home.clone(),
                    factory.imported_default_preset.as_deref(),
                    &factory.plugins,
                    true,
                    &mut warnings,
                )?;
                Ok::<_, String>((session, resources))
            })
            .await
            .map_err(|error| format!("Subagent setup failed: {error}"))??
        };
        let (session, resources) = loaded;
        let factory = &setup.factory;
        let Built { session, servers } = build_session(
            &args,
            Build {
                session,
                apis: Arc::clone(&setup.apis),
                resources,
                // Reminders belong to the user's session, not to a subagent.
                settings: Settings {
                    schedules: false,
                    ..setup.settings.clone()
                },
                registry: setup.registry.clone(),
                model: route.as_ref().map(|route| route.model.clone()),
                thinking_level: route.and_then(|route| route.thinking_level),
                home: factory.home.clone(),
                docs_command: factory.docs_command.clone(),
                sandbox_provider: Arc::clone(&factory.sandbox_provider),
                approver: factory.approver.clone(),
                role: Role::Child(link),
            },
        )
        .await?;
        if let Err(error) = start_servers(&session, servers) {
            session.shutdown().await;
            return Err(error);
        }
        Ok(session)
    })
}

/// Pi's `_buildRuntime` tools, `createAgentSession` tool selection, and
/// the system prompt, then the session itself.
async fn build_session(args: &Args, build: Build) -> Result<Built, String> {
    let Build {
        session,
        apis,
        resources,
        settings,
        registry,
        model,
        thinking_level,
        home,
        docs_command,
        sandbox_provider,
        approver,
        role,
    } = build;
    let Resources {
        context_files,
        mcp_servers,
        skills,
        prompt_templates,
        permission,
        custom_prompt,
        append_system_prompt,
        plugins,
        hooks,
    } = resources;
    let session_cwd = session.cwd().to_owned();
    let session_id = session.session_id().to_owned();
    // A subagent's prompts come from its parent's tool calls, so it runs
    // only the tool hooks.
    let hook_scope = match role {
        Role::Child(_) => crate::hooks::HookScope::ToolsOnly,
        Role::Root(_) => crate::hooks::HookScope::Session,
    };
    let can_ask = approver.is_some();
    let approver: Arc<dyn Approver> = approver.unwrap_or_else(|| Arc::new(NoApprover));
    // One standing permission for the tools, the MCP servers' admission,
    // and the hooks' confinement.
    let standing = Arc::new(permission.standing(Path::new(&session_cwd), Some(session_id.clone())));
    let tool_permissions = ToolPermissions::new(
        Arc::clone(&standing),
        Arc::clone(&approver),
        Arc::clone(&sandbox_provider),
    );
    // As the prompt states `never` without an answerer, the tools offer no
    // escalation that would fail.
    let tool_permissions = if can_ask {
        tool_permissions
    } else {
        tool_permissions.without_escalation()
    };
    let session_file = session
        .session_file()
        .map(|file| file.to_string_lossy().into_owned());
    // The read tool asks for the session's model when it runs, and bash
    // reads the model and thinking level for its `BAKE_*` variables when
    // each call starts; both read what the session publishes now, so a
    // model change reaches the next call.
    let published = crate::agent_session::SessionSelection::default();
    // The session's owner of its work, given to the tools so that shutdown
    // waits for their blocking work and every tree they start.
    let work = crate::owned_work::WorkGate::default();
    // The session's background jobs, shared by `bash`, the job tools, and
    // the session that delivers their notices.
    let jobs = crate::jobs::JobRegistry::new(work.clone(), crate::jobs::JobLimits::default());
    let bash_context: crate::tools::bash::BashToolContextFn = {
        let selection = published.clone();
        let session_cwd = PathBuf::from(&session_cwd);
        Arc::new(move || {
            let selected = selection.get();
            crate::tools::bash::BashToolContext {
                cwd: Some(session_cwd.clone()),
                session_id: session_id.clone(),
                session_file: session_file.clone(),
                thinking_level: Some(selected.thinking_level.as_str().to_owned()),
                model: selected.model.map(|model| crate::tools::bash::BashModel {
                    provider: model.provider,
                    id: model.id,
                }),
            }
        })
    };
    let tools_options = ToolsOptions {
        read: ReadToolOptions {
            auto_resize_images: settings.image_auto_resize(),
            resize_options: None,
            model: Some({
                let selection = published.clone();
                Arc::new(move || selection.get().model)
            }),
        },
        bash: Some(sandboxed_bash_tool(
            Path::new(&session_cwd),
            crate::tools::bash::BashToolOptions {
                context: Some(bash_context.clone()),
                owner: Some(work.clone()),
                ..Default::default()
            },
            &tool_permissions,
        )?),
        owner: Some(work.clone()),
        jobs: Some(jobs.clone()),
        web: Some(crate::tools::web_tools::WebTools {
            fetcher: crate::web::fetch::HttpFetcher::new(
                crate::web::fetch::HttpFetchLimits::default(),
                Some(work.clone()),
            ),
            search: crate::web::search::CliProxySearch::new(
                registry.clone(),
                Some(published.clone()),
                settings.web_search_model.clone(),
                Some(work.clone()),
            ),
        }),
    };
    let mut definitions = owned_sandbox_file_tools(
        create_bake_tool_definitions(Path::new(&session_cwd), &tools_options),
        Path::new(&session_cwd),
        &tool_permissions,
        Some(work.clone()),
    );
    // With `subagents` on, a top-level session starts subagents built as it
    // is; a subagent messages its parent and starts none (depth 1).
    let mut subagent_host = None;
    let subagent_tools: &[&str] = match role {
        Role::Root(Some(factory)) if settings.subagents => {
            let parent_file = session
                .is_persisted()
                .then(|| session.session_file().map(Path::to_path_buf))
                .flatten();
            let host = crate::subagents::SubagentHost::new(crate::subagents::HostOptions {
                parent_id: session.session_id().to_owned(),
                parent_file: parent_file.clone(),
                jobs: jobs.clone(),
                work: work.clone(),
                selection: published.clone(),
                registry: registry.clone(),
                settings: settings.clone(),
                builder: child_builder(ChildSetup {
                    factory: *factory,
                    apis: Arc::clone(&apis),
                    registry: registry.clone(),
                    settings: settings.clone(),
                    cwd: session_cwd.clone(),
                    place: session.session_dir().to_path_buf(),
                    persisted: session.is_persisted(),
                    parent_id: session.session_id().to_owned(),
                    parent_file,
                    permission: permission.clone(),
                }),
                recovered: crate::subagents::recover(&session),
            });
            let model_choice = registry.snapshot().available().len() > 1;
            definitions.extend(crate::subagents::parent_tools(&host, model_choice));
            subagent_host = Some(host);
            // The job tools say a subagent's id is not a job id.
            let job_tools =
                crate::tools::job_tools::create_job_tool_definitions_with_subagents(&jobs);
            for definition in &mut definitions {
                if let Some(replacement) = job_tools
                    .iter()
                    .find(|tool| tool.name() == definition.name())
                {
                    *definition = replacement.clone();
                }
            }
            &crate::subagents::PARENT_TOOL_NAMES
        }
        Role::Root(_) => &[],
        Role::Child(link) => {
            definitions.extend(crate::subagents::child_tools(&link));
            &crate::subagents::CHILD_TOOL_NAMES
        }
    };
    let mut available: Vec<String> = definitions
        .iter()
        .map(|definition| definition.name().to_owned())
        .collect();
    if settings.task_documents {
        available.push(crate::agent_session::task_documents::NAME.to_owned());
    }
    if settings.schedules {
        available.extend(crate::agent_session::schedules::NAMES.map(str::to_owned));
    }
    // The session's own `codemode` tool, a Bake default where Pi leaves it
    // inactive: `--tools` without it, `--no-tools`, `--exclude-tools
    // codemode`, or a `defaultTools` setting that omits it turns it off.
    available.push(crate::tools::codemode::CODEMODE_TOOL_NAME.to_owned());
    let mut defaults = settings.default_tools.clone().unwrap_or_else(|| {
        crate::tools::index::BAKE_DEFAULT_TOOL_NAMES
            .map(str::to_owned)
            .to_vec()
    });
    if settings.task_documents && settings.default_tools.is_none() {
        defaults.push(crate::agent_session::task_documents::NAME.to_owned());
    }
    if settings.default_tools.is_none() {
        defaults.extend(subagent_tools.iter().map(|name| (*name).to_owned()));
        if settings.schedules {
            defaults.extend(crate::agent_session::schedules::NAMES.map(str::to_owned));
        }
    }
    let selection_options = ToolSelectionOptions {
        tools: args.tools.clone(),
        exclude_tools: args.exclude_tools.clone(),
        no_tools: if args.no_tools {
            Some(NoTools::All)
        } else if args.no_builtin_tools {
            Some(NoTools::Builtin)
        } else {
            None
        },
    };
    let selection = select_tools(&selection_options, Some(&defaults), &available)?;
    // The job tools control `bash`'s background jobs, their only producer:
    // without `bash` they are not active.
    let mut selection = selection;
    if !selection.active.iter().any(|name| name == "bash") {
        selection
            .active
            .retain(|name| !crate::tools::job_tools::JOB_TOOL_NAMES.contains(&name.as_str()));
    }
    let mut registered: Vec<_> = definitions
        .into_iter()
        .filter(|definition| {
            selection
                .registered
                .iter()
                .any(|name| name == definition.name())
        })
        .collect();
    // `web_search` names `web_fetch` only when the model can call it.
    if !selection.active.iter().any(|name| name == "web_fetch") {
        for definition in &mut registered {
            if definition.name() == "web_search"
                && let Some(web) = &tools_options.web
            {
                *definition =
                    crate::tools::web_tools::web_search_tool_definition(web.search.clone(), false);
            }
        }
    }
    // `bash` takes `run_in_background` only when the model can read the
    // job it starts.
    if selection.active.iter().any(|name| name == "job_output") {
        let background = sandboxed_bash_tool(
            Path::new(&session_cwd),
            crate::tools::bash::BashToolOptions {
                context: Some(bash_context),
                owner: Some(work.clone()),
                jobs: Some(jobs.clone()),
                ..Default::default()
            },
            &tool_permissions,
        )?;
        for definition in &mut registered {
            if definition.name() == "bash" {
                *definition = background.clone();
            }
        }
    }

    // The session header's start, so a resumed session states the same
    // date and patches nothing.
    let started_ms = session
        .header()
        .and_then(|header| header.timestamp())
        .and_then(parse_date_ms)
        .unwrap_or_else(bake_ai::now_ms);
    let mut system_prompt = SystemPromptOptions::new(session_cwd.clone());
    system_prompt.custom_prompt = custom_prompt;
    if let Some(append) = append_system_prompt {
        system_prompt.append_system_prompt = append;
    }
    system_prompt.context_files = context_files;
    system_prompt.skills = skills;
    // Without an approver, escalations fail closed (D24), and the prompt
    // states the `never` policy whatever the session's.
    system_prompt.permissions = permission.prompt_text(Path::new(&session_cwd), can_ask);
    system_prompt.environment = Environment::detect(Path::new(&session_cwd), started_ms);
    let mut session = AgentSession::create(AgentSessionOptions {
        session,
        registry,
        apis,
        settings,
        settings_store: Some(SettingsStore::new(&home)),
        selection: Some(published),
        model,
        thinking_level,
        tools: registered
            .iter()
            .map(|definition| Arc::new(definition.tool.clone()))
            .collect(),
        active_tool_names: selection.active,
        tool_prompts: registered
            .iter()
            .map(|definition| definition.prompt())
            .collect(),
        system_prompt,
        docs_command,
        plugins,
        work: Some(work),
        jobs: Some(jobs),
    })
    .await?;
    // Before any prompt: each approval request and its answer is written to
    // this session's file before the call can run.
    if let Err(message) = session.record_approvals(tool_permissions.engine.recorder_slot()) {
        session.shutdown().await;
        return Err(message);
    }
    session.set_prompt_templates(prompt_templates);
    if let Some(host) = subagent_host {
        session.set_subagents(host);
    }
    if let Err(message) = session.set_hooks(crate::hooks::HooksOptions {
        files: hooks,
        scope: hook_scope,
        standing: Arc::clone(&standing),
        sandbox: Arc::clone(&sandbox_provider),
        approver,
        default_timeout: crate::hooks::DEFAULT_HOOK_TIMEOUT,
    }) {
        session.shutdown().await;
        return Err(message);
    }
    // The same restrictions the built-in tools had, for MCP tools whenever
    // they are published. With nothing declarable (`--no-tools` alone) no
    // server is started: none of its tools could be declared or called.
    let (allow, exclude) = crate::tools::index::dynamic_tool_restrictions(&selection_options);
    let tool_filter = crate::mcp::McpToolFilter::new(allow, exclude);
    let servers =
        (!mcp_servers.is_empty() && !tool_filter.allows_none()).then(|| crate::mcp::McpOptions {
            servers: mcp_servers,
            standing,
            sandbox: sandbox_provider,
            timings: crate::mcp::McpTimings::default(),
            tool_filter,
            http: crate::mcp::McpHttpOptions::default(),
            resolver: None,
        });
    // Checked now, so starting them later can fail only if the session has
    // been shut down.
    if let Some(options) = &servers
        && let Err(message) = crate::mcp::validate(options)
    {
        session.shutdown().await;
        return Err(message);
    }
    Ok(Built { session, servers })
}

/// A session [`build_session`] built, and the MCP servers it is to start.
struct Built {
    session: AgentSession,
    servers: Option<crate::mcp::McpOptions>,
}

/// Starts `servers` for `session` ([`AgentSession::start_mcp`]). From then
/// on the session owns them: whoever holds it shuts it down, which stops
/// them and waits until they are gone.
fn start_servers(
    session: &AgentSession,
    servers: Option<crate::mcp::McpOptions>,
) -> Result<(), String> {
    match servers {
        Some(options) => session.start_mcp(options).map(drop),
        None => Ok(()),
    }
}

/// Where a replacement session comes from: Pi's `newSession` and
/// `switchSession`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionTarget {
    /// A new session in the current one's session directory and working
    /// directory, persisted when it is, in memory when it is not
    /// (`--no-session`).
    New,
    /// The session file at this path, which must exist, read as a session,
    /// and hold the session `id`.
    Open { path: PathBuf, id: String },
}

/// Where [`SessionFactory::build_replacement`] takes its store from.
enum Staged {
    Target(SessionTarget),
    /// A store already built, such as a fork's.
    Store(Box<SessionManager>),
}

/// A replacement session, built and not yet in use, and what its setup
/// warned about. Its MCP servers are not started yet: as in Pi's
/// `AgentSessionRuntime`, which tears the current session down
/// (`session_shutdown` stops its servers) before it creates the next
/// (`session_start` connects them), the caller shuts the running session
/// down, awaited, and then calls [`Replacement::start_servers`], so the two
/// sessions' servers never run at once. A replacement that is not used is
/// shut down and has started none.
pub struct Replacement {
    pub session: AgentSession,
    pub warnings: Vec<String>,
    servers: Option<crate::mcp::McpOptions>,
}

impl Replacement {
    /// Starts the replacement's MCP servers, once, after the session it
    /// replaces has been shut down; later calls start nothing. Fails only
    /// when the replacement was shut down first.
    pub fn start_servers(&mut self) -> Result<(), String> {
        start_servers(&self.session, self.servers.take())
    }

    /// Whether [`Self::start_servers`] has servers to start.
    pub fn has_pending_servers(&self) -> bool {
        self.servers.is_some()
    }
}

/// Builds the sessions `/new`, `/resume`, and `/fork` replace the running
/// one with,
/// as Pi's `createRuntime` does for `AgentSessionRuntime`: the same Bake
/// home, tools, flags, sandbox provider, and approver as at startup, with
/// the running session's live settings and model runtime, so a sign-in, a
/// refreshed model list, a `--api-key`, or a changed setting carries over.
///
/// The command line's session choice (`--continue`, `--resume`,
/// `--session`, `--session-id`, `--fork`), `--name`, and its prompts and
/// files belong to the first session and are never applied again. A new
/// session starts with the running session's model and thinking level,
/// and with the startup permission flags; a resumed one restores the model,
/// thinking level, and permission its file records, as `--continue` does
/// without `--model`, `--thinking`, or permission flags.
///
/// The replacement is built from its own [`SessionManager`]; the running
/// session's is never touched, so a failure leaves it, its file, and its
/// writer lock as they were.
#[derive(Clone)]
pub struct SessionFactory {
    args: Args,
    home: PathBuf,
    user_home: Option<PathBuf>,
    docs_command: String,
    sandbox_provider: Arc<dyn SandboxProvider>,
    approver: Option<Arc<dyn Approver>>,
    imported_default_preset: Option<String>,
    plugins: WasmSetup,
}

impl std::fmt::Debug for SessionFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionFactory")
            .field("home", &self.home)
            .finish_non_exhaustive()
    }
}

impl SessionFactory {
    /// A factory for sessions set up as `args` describe, with settings and
    /// credentials from `home`, bash confined by `sandbox_provider`, and
    /// approvals asked of `approver` (none fails them closed). The
    /// command line's own setup ([`start_interactive`]) also names the
    /// user's home and the 0.3 home's imported default preset.
    pub fn new(
        args: Args,
        home: PathBuf,
        docs_command: String,
        sandbox_provider: Arc<dyn SandboxProvider>,
        approver: Option<Arc<dyn Approver>>,
    ) -> Self {
        Self {
            args,
            home,
            user_home: None,
            docs_command,
            sandbox_provider,
            approver,
            imported_default_preset: None,
            plugins: WasmSetup::default(),
        }
    }

    /// Runs the home's enabled Wasm plugins under `plugins`; without this,
    /// the factory has no helper command, so each enabled plugin blocks the
    /// tools it lists.
    pub fn with_plugins(mut self, plugins: WasmSetup) -> Self {
        self.plugins = plugins;
        self
    }

    /// Builds the replacement for `current`, which keeps running
    /// meanwhile; the caller shuts `current` down before it uses the
    /// replacement, and shuts the replacement down if it does not.
    pub async fn replace(
        &self,
        current: &AgentSession,
        target: SessionTarget,
    ) -> Result<Replacement, String> {
        let resume = matches!(target, SessionTarget::Open { .. });
        self.build_replacement(current, Staged::Target(target), resume)
            .await
    }

    /// Builds the session `/fork` or `/clone` replaces `current` with, Pi's
    /// runtime `fork` at `entry_id` in `position`
    /// ([`AgentSession::fork_session_manager`]), and returns it with the
    /// chosen message's text, for the editor, when forking before one. A
    /// fork that holds history restores
    /// the model, thinking level, and permission its path records, as
    /// `/resume` does; an empty fork, from a first message, starts as
    /// `/new` does. `current` and its file are not touched; a persisted
    /// fork's file may already be written when a later step fails.
    pub async fn fork(
        &self,
        current: &AgentSession,
        entry_id: &str,
        position: ForkPosition,
    ) -> Result<(Replacement, Option<String>), String> {
        let forked = current.fork_session_manager(entry_id, position).await?;
        let resume = !forked.session.build_session_context().messages.is_empty();
        let replacement = self
            .build_replacement(current, Staged::Store(Box::new(forked.session)), resume)
            .await?;
        Ok((replacement, forked.selected_text))
    }

    async fn build_replacement(
        &self,
        current: &AgentSession,
        staged: Staged,
        resume: bool,
    ) -> Result<Replacement, String> {
        let settings = current.settings();
        let registry = current.model_runtime().clone();
        let (place, persisted) = current.session_place();
        let cwd = current.cwd();
        let (model, thinking_level) = if resume {
            (None, None)
        } else {
            (current.model(), Some(current.thinking_level()))
        };
        let factory = self.clone();
        let loaded = tokio::task::spawn_blocking(move || {
            let mut warnings = Vec::new();
            let mut session = match staged {
                Staged::Target(SessionTarget::New) if persisted => {
                    SessionManager::create(&cwd, Some(&place), NewSessionOptions::default())
                        .map_err(|error| error.to_string())
                }
                Staged::Target(SessionTarget::New) => {
                    SessionManager::in_memory(Some(&cwd), NewSessionOptions::default(), Vec::new())
                        .map_err(|error| error.to_string())
                }
                Staged::Target(SessionTarget::Open { path, id }) => {
                    open_listed(&path, &id, &place, &cwd)
                }
                Staged::Store(session) => Ok(*session),
            }?;
            let resources = load_resources(
                &mut session,
                &settings,
                &factory.args,
                &factory.home,
                factory.user_home.clone(),
                factory.imported_default_preset.as_deref(),
                &factory.plugins,
                !resume,
                &mut warnings,
            )?;
            Ok::<_, String>((session, resources, settings, warnings))
        })
        .await
        .map_err(|error| format!("Session setup failed: {error}"))??;
        let (session, resources, settings, warnings) = loaded;
        let Built { session, servers } = build_session(
            &self.args,
            Build {
                session,
                apis: Arc::clone(current.apis()),
                resources,
                settings,
                registry,
                model,
                thinking_level,
                home: self.home.clone(),
                docs_command: self.docs_command.clone(),
                sandbox_provider: Arc::clone(&self.sandbox_provider),
                approver: self.approver.clone(),
                role: Role::Root(Some(Box::new(self.clone()))),
            },
        )
        .await?;
        Ok(Replacement {
            session,
            warnings,
            servers,
        })
    }
}

/// Opens the session file `/resume` chose: one that exists, reads as a
/// session, holds the session `id`, and started in `cwd`, the running
/// session's working directory, which the terminal, its file mentions, and
/// its status line also show; a session from another directory is refused
/// rather than run with tools in a workspace the screen does not name.
/// [`SessionManager::open`] would start a new session at a missing path,
/// so a file removed since it was listed is refused rather than recreated.
/// Blocking.
fn open_listed(path: &Path, id: &str, place: &Path, cwd: &str) -> Result<SessionManager, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("Cannot open {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(format!("{} is not a session file", path.display()));
    }
    let session =
        SessionManager::open(path, Some(place), None).map_err(|error| error.to_string())?;
    if session.session_id() != id {
        return Err(format!(
            "{} no longer holds session {id}; choose it again",
            path.display()
        ));
    }
    let wanted = crate::session::paths::resolve_path_string(cwd);
    if crate::session::paths::resolve_path_string(session.cwd()) != wanted {
        return Err(format!(
            "Session {id} started in {}, not in {wanted}; resume it from there",
            session.cwd()
        ));
    }
    if !Path::new(session.cwd()).is_dir() {
        return Err(format!(
            "The session's working directory {} does not exist",
            session.cwd()
        ));
    }
    Ok(session)
}

async fn start_with(
    environment: CliEnvironment,
    args: Args,
    kind: StartKind,
) -> Result<Started, i32> {
    let CliEnvironment {
        cwd,
        home,
        user_home,
        docs_command,
        stdin_is_terminal,
        stdin,
        stdout,
        stderr,
        extra_providers,
        imported_default_model,
        imported_default_preset,
        sandbox_provider,
        approver,
        plugins,
        warnings: startup_warnings,
        mut interrupt,
    } = environment;
    let mut args = args;
    let Some(home) = home else {
        return Err(error(
            &stderr,
            "Cannot find the Bake home: set BAKE_HOME or make the user's home directory resolvable",
        ));
    };
    let cwd = cwd.to_string_lossy().into_owned();
    let factory = SessionFactory {
        args: args.clone(),
        home: home.clone(),
        user_home: user_home.clone(),
        docs_command: docs_command.clone(),
        sandbox_provider: Arc::clone(&sandbox_provider),
        approver: approver.clone(),
        imported_default_preset: imported_default_preset.clone(),
        plugins: plugins.clone(),
    };

    let loaded = {
        let home = home.clone();
        let cwd = cwd.clone();
        let args = args.clone();
        tokio::task::spawn_blocking(move || {
            let mut settings = Settings::load(&home);
            if settings.default_provider.is_none()
                && settings.default_model.is_none()
                && let Some(imported) = imported_default_model
            {
                settings.default_provider = Some(imported.provider);
                settings.default_model = Some(imported.model);
                settings.default_thinking_level =
                    settings.default_thinking_level.or(imported.thinking_level);
            }
            let mut registry = ModelRegistry::load(&home);
            let mut warnings = startup_warnings;
            warnings.extend(settings.warnings.iter().cloned());
            for (id, config, key) in extra_providers {
                if let Err(message) = registry.add_provider_config(&id, config, key) {
                    warnings.push(message);
                }
            }
            warnings.extend(apply_account(&home, &mut registry));
            if let Some(message) = registry.error() {
                warnings.push(message);
            }
            let session_dir = args
                .session_dir
                .as_deref()
                .map(normalize_path)
                .or_else(|| settings.session_dir.clone());
            let session =
                create_session_manager(&args, &cwd, session_dir.as_deref(), &home, &mut warnings);
            let session = session.and_then(|mut session| {
                if let Some(name) = args.name.as_deref().and_then(normalize_session_name) {
                    session
                        .append_session_info(&name)
                        .map_err(|error| SessionStartError::Error(error.to_string()))?;
                }
                let resources = load_resources(
                    &mut session,
                    &settings,
                    &args,
                    &home,
                    user_home,
                    imported_default_preset.as_deref(),
                    &plugins,
                    true,
                    &mut warnings,
                )
                .map_err(SessionStartError::Error)?;
                let processed_files = if args.file_args.is_empty() {
                    Ok(file_processor::ProcessedFiles::default())
                } else {
                    // AgentSession resizes prompt images against the selected model's limits.
                    process_file_arguments(
                        &args.file_args,
                        Path::new(&cwd),
                        ProcessFileOptions {
                            auto_resize_images: false,
                        },
                    )
                    .map_err(SessionStartError::Error)
                }?;
                Ok((session, resources, processed_files))
            });
            (settings, registry, warnings, session)
        })
        .await
    };
    let (settings, mut registry, warnings, session) = match loaded {
        Ok(loaded) => loaded,
        Err(join) => return Err(error(&stderr, &format!("Startup failed: {join}"))),
    };
    let (session, resources, processed_files) = match session {
        Ok(session) => session,
        Err(SessionStartError::Error(message)) => return Err(error(&stderr, &message)),
        Err(SessionStartError::Plain(message)) => {
            write_out(&stderr, &format!("{message}\n"));
            return Err(1);
        }
    };
    let has_existing_session = !session.build_session_context().messages.is_empty();

    // Pi's `resolveModelScope` and `buildSessionOptions`.
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let scoped_models: Vec<ScopedModel> = match args
        .models
        .as_deref()
        .or(settings.enabled_models.as_deref())
    {
        Some(patterns) if !patterns.is_empty() => {
            let candidates = if args.api_key.is_some() {
                registry.models()
            } else {
                registry.available()
            };
            let scope = resolve_model_scope_from_models(patterns, &candidates);
            for diagnostic in scope.diagnostics {
                diagnostics.push(Diagnostic {
                    kind: DiagnosticKind::Warning,
                    message: diagnostic.message,
                });
            }
            scope.scoped_models
        }
        _ => Vec::new(),
    };
    let mut model = None;
    let mut thinking_level = args.thinking;
    if let Some(provider) = &args.provider
        && args.model.is_none()
    {
        diagnostics.push(Diagnostic {
            kind: DiagnosticKind::Error,
            message: format!(
                "--provider requires --model (for example: --provider {provider} --model <pattern>)"
            ),
        });
    }
    if let Some(pattern) = &args.model {
        let resolved = resolve_cli_model(
            args.provider.as_deref(),
            Some(pattern),
            args.thinking,
            &registry,
        );
        if let Some(warning) = resolved.warning {
            diagnostics.push(Diagnostic {
                kind: DiagnosticKind::Warning,
                message: warning,
            });
        }
        // Pi points at `--list-models`, which bake does not have.
        if let Some(message) = resolved.error {
            diagnostics.push(Diagnostic {
                kind: DiagnosticKind::Error,
                message: format!("{message} See:\n  {docs_command} models"),
            });
        }
        if let Some(found) = resolved.model {
            model = Some(found);
            if args.thinking.is_none() && resolved.thinking_level.is_some() {
                thinking_level = resolved.thinking_level;
            }
        }
    }
    if model.is_none() && !scoped_models.is_empty() && !has_existing_session {
        let saved_in_scope = settings
            .default_provider
            .as_deref()
            .zip(settings.default_model.as_deref())
            .and_then(|(provider, id)| registry.model(provider, id))
            .and_then(|saved| {
                scoped_models.iter().find(|scoped| {
                    scoped.model.provider == saved.provider && scoped.model.id == saved.id
                })
            });
        let chosen = saved_in_scope.unwrap_or(&scoped_models[0]);
        model = Some(chosen.model.clone());
        if args.thinking.is_none() && chosen.thinking_level.is_some() {
            thinking_level = chosen.thinking_level;
        }
    }
    if let Some(api_key) = &args.api_key {
        match &model {
            Some(selected) => registry.set_runtime_api_key(&selected.provider, api_key),
            None => diagnostics.push(Diagnostic {
                kind: DiagnosticKind::Error,
                message: "--api-key requires a model to be specified via --model, --provider/--model, or --models".into(),
            }),
        }
    }
    for warning in &warnings {
        warn(&stderr, warning);
    }
    report(&stderr, &diagnostics);
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.kind == DiagnosticKind::Error)
    {
        return Err(1);
    }

    let built = match build_session(
        &args,
        Build {
            session,
            apis: Arc::new(ApiRegistry::with_builtins()),
            resources,
            settings,
            registry: registry.into(),
            model,
            thinking_level,
            home: home.clone(),
            docs_command: docs_command.clone(),
            sandbox_provider,
            approver,
            role: Role::Root(Some(Box::new(factory.clone()))),
        },
    )
    .await
    {
        Ok(built) => built,
        Err(message) => return Err(error(&stderr, &message)),
    };
    let session = built.session;
    // At once, before piped input is read: as in Pi, the servers connect
    // while the rest of startup runs.
    if let Err(message) = start_servers(&session, built.servers) {
        session.shutdown().await;
        return Err(error(&stderr, &message));
    }

    // The session, and any MCP server it started, is shut down whether the
    // read fails or a stop request (print mode's SIGTERM or SIGHUP) comes
    // while a writer holds the pipe open. The read itself cannot be
    // cancelled; the binary does not wait for it at exit.
    // In RPC mode the stream carries the protocol, as in Pi; nothing
    // reads it here.
    let mut protocol_stdin = None;
    let piped = if stdin_is_terminal || kind == StartKind::Rpc {
        if kind == StartKind::Rpc {
            protocol_stdin = Some(stdin);
        }
        Ok(None)
    } else {
        let read = tokio::task::spawn_blocking(move || read_piped_stdin(stdin));
        tokio::select! {
            read = read => read.unwrap_or_else(|join| Err(join.to_string())),
            code = &mut interrupt => {
                session.shutdown().await;
                return Err(code);
            }
        }
    };
    let piped = match piped {
        Ok(piped) => piped,
        Err(message) => {
            session.shutdown().await;
            return Err(error(&stderr, &message));
        }
    };
    let initial_message = build_initial_message(piped, &processed_files.text, &mut args.messages);

    if session.model().is_none() && kind != StartKind::Interactive {
        session.shutdown().await;
        write_out(
            &stderr,
            &format!("{}\n", format_no_models_available_message(&docs_command)),
        );
        return Err(1);
    }
    let resume = resumes_at_startup(&args);
    Ok(Started {
        session,
        initial_message,
        initial_images: processed_files.images,
        messages: args.messages,
        stdin: protocol_stdin,
        stdout,
        stderr,
        interrupt,
        home,
        factory,
        resume,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn sink() -> (SharedWriter, Arc<Mutex<Vec<u8>>>) {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        (buffer.clone() as SharedWriter, buffer)
    }

    fn text(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
        String::from_utf8_lossy(&buffer.lock().map(|b| b.clone()).unwrap_or_default()).into_owned()
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    // Pi `resolveAppMode`: print when asked or when a stream is not a terminal.
    #[test]
    fn app_mode_follows_pi() {
        let (stderr, errors) = sink();
        assert!(matches!(
            plan(&strings(&["hi"]), true, false, &stderr),
            Action::Print(_, Mode::Text)
        ));
        assert!(matches!(
            plan(&strings(&["--mode", "json", "hi"]), true, true, &stderr),
            Action::Print(_, Mode::Json)
        ));
        assert!(matches!(
            plan(&strings(&["-p", "hi"]), true, true, &stderr),
            Action::Print(_, Mode::Text)
        ));
        assert!(matches!(
            plan(&strings(&["hi", "--inline"]), true, true, &stderr),
            Action::Interactive(args) if args.inline && args.messages == ["hi"]
        ));
        assert!(matches!(
            plan(&strings(&[]), true, true, &stderr),
            Action::Interactive(_)
        ));
        assert_eq!(text(&errors), "");
    }

    // The flags `bake` excludes (Pi's extensions, themes, and
    // `--tui-mode`) are refused before anything is loaded, with Pi's
    // unknown-option message and its exit status.
    #[test]
    fn the_excluded_pi_flags_are_refused_as_unknown_options() {
        for (flags, message) in [
            (
                vec!["--extension", "./ext.ts", "-p", "hi"],
                "Error: Unknown option: --extension\n",
            ),
            (
                vec!["--no-extensions"],
                "Error: Unknown option: --no-extensions\n",
            ),
            (
                vec!["--theme", "./dark.json"],
                "Error: Unknown option: --theme\n",
            ),
            (
                vec!["--use-theme", "light"],
                "Error: Unknown option: --use-theme\n",
            ),
            (vec!["--no-themes"], "Error: Unknown option: --no-themes\n"),
            (
                vec!["--tui-mode", "regular"],
                "Error: Unknown option: --tui-mode\n",
            ),
            (vec!["-e", "./ext.ts"], "Error: Unknown option: -e\n"),
            (vec!["-ne"], "Error: Unknown option: -ne\n"),
        ] {
            let (stderr, errors) = sink();
            assert_eq!(
                plan(&strings(&flags), true, true, &stderr),
                Action::Exit(1),
                "{flags:?}"
            );
            assert_eq!(text(&errors), message, "{flags:?}");
        }
    }

    // Bake: the project-trust flags are parsed and then refused, with the
    // reason instead of `Unknown option`, and nothing is loaded first.
    #[test]
    fn the_project_trust_flags_are_refused() {
        for flags in [
            vec!["-a"],
            vec!["--approve", "-p", "hi"],
            vec!["-na"],
            vec!["--no-approve", "task"],
        ] {
            let (stderr, errors) = sink();
            assert_eq!(
                plan(&strings(&flags), true, true, &stderr),
                Action::Exit(1),
                "{flags:?}"
            );
            assert_eq!(text(&errors), format!("Error: {PROJECT_TRUST_REFUSED}\n"));
        }
    }

    // Pi's `offlineMode`, with Bake's variable names: the flag, then
    // `BAKE_OFFLINE`, then `DSH_OFFLINE`, each read as Pi's
    // `isTruthyEnvFlag` reads `PI_OFFLINE`.
    #[test]
    fn offline_comes_from_the_flag_or_the_environment() {
        let none = |_: &str| None;
        assert!(!offline_mode(&parse_args(&[]), none));
        assert!(offline_mode(&parse_args(&strings(&["--offline"])), none));
        let set = |name: &'static str, value: &'static str| {
            move |asked: &str| (asked == name).then(|| std::ffi::OsString::from(value.to_owned()))
        };
        for value in ["1", "true", "TRUE", "yes", " Yes "] {
            assert!(
                offline_mode(&parse_args(&[]), set(BAKE_OFFLINE_ENV, value)),
                "{value:?}"
            );
            assert!(
                offline_mode(&parse_args(&[]), set(DSH_OFFLINE_ENV, value)),
                "{value:?}"
            );
        }
        for value in ["", "   ", "0", "no", "false", "on"] {
            assert!(
                !offline_mode(&parse_args(&[]), set(BAKE_OFFLINE_ENV, value)),
                "{value:?}"
            );
        }
        // A blank `BAKE_OFFLINE` falls through to `DSH_OFFLINE`, as the
        // home does.
        let blank_then_set = |asked: &str| match asked {
            BAKE_OFFLINE_ENV => Some(std::ffi::OsString::from("  ")),
            DSH_OFFLINE_ENV => Some(std::ffi::OsString::from("1")),
            _ => None,
        };
        assert!(offline_mode(&parse_args(&[]), blank_then_set));
        // A set `BAKE_OFFLINE` decides, whatever `DSH_OFFLINE` says.
        let both = |asked: &str| match asked {
            BAKE_OFFLINE_ENV => Some(std::ffi::OsString::from("0")),
            DSH_OFFLINE_ENV => Some(std::ffi::OsString::from("1")),
            _ => None,
        };
        assert!(!offline_mode(&parse_args(&[]), both));
    }

    // Pi's `createSessionManager` reaches `--resume` only after
    // `--no-session`, `--fork`, and `--session`, and `validateForkFlags`
    // and `validateSessionIdFlags` refuse it beside `--fork` and
    // `--session-id`. Print mode has no terminal for the picker.
    #[test]
    fn resume_opens_the_picker_only_in_an_interactive_session() {
        let (stderr, errors) = sink();
        assert!(matches!(
            plan(&strings(&["-r"]), true, true, &stderr),
            Action::Interactive(args) if args.resume
        ));
        assert!(matches!(
            plan(&strings(&["--resume", "--continue"]), true, true, &stderr),
            Action::Interactive(_)
        ));
        // Pi takes the other session choice and leaves `--resume` unused,
        // so print mode still runs.
        for flags in [
            vec!["-p", "-r", "--no-session"],
            vec!["-p", "-r", "--session", "abc"],
        ] {
            assert!(
                matches!(
                    plan(&strings(&flags), true, true, &stderr),
                    Action::Print(..)
                ),
                "{flags:?}"
            );
        }
        assert_eq!(text(&errors), "");
        for flags in [
            vec!["-p", "-r"],
            vec!["--mode", "json", "--resume"],
            vec!["-r", "--fork", "a"],
            vec!["-r", "--session-id", "s1"],
        ] {
            let (stderr, errors) = sink();
            assert_eq!(
                plan(&strings(&flags), true, true, &stderr),
                Action::Exit(1),
                "{flags:?}"
            );
            assert!(text(&errors).starts_with("Error: "), "{flags:?}");
        }
    }

    #[test]
    fn recovery_flags_require_one_persisted_selected_run() {
        for flags in [
            vec!["-p", "--recoverable", "--no-session", "task"],
            vec!["-p", "--recoverable", "first", "second"],
            vec!["-p", "--resume-run", "run-1"],
            vec!["-p", "--resume-run", "run-1", "--session", "prefix"],
            vec![
                "-p",
                "--resume-run",
                "run-1",
                "--session",
                "session.jsonl",
                "extra",
            ],
            vec![
                "-p",
                "--resume-run",
                "run-1",
                "--session",
                "session.jsonl",
                "--max-model-calls",
                "9",
            ],
            vec![
                "-p",
                "--resume-run",
                "run-1",
                "--session",
                "session.jsonl",
                "--recoverable",
            ],
            vec!["--recoverable", "task"],
        ] {
            let (stderr, errors) = sink();
            assert_eq!(
                plan(&strings(&flags), true, true, &stderr),
                Action::Exit(1),
                "{flags:?}"
            );
            assert!(!text(&errors).is_empty());
        }
        for flags in [
            vec!["-p", "--recoverable", "--max-model-calls", "9", "task"],
            vec!["-p", "--resume-run", "run-1", "--session", "session.jsonl"],
        ] {
            let (stderr, errors) = sink();
            assert!(
                matches!(
                    plan(&strings(&flags), true, true, &stderr),
                    Action::Print(_, _)
                ),
                "{}",
                text(&errors)
            );
        }
    }

    #[test]
    fn argument_errors_exit_before_loading() {
        let (stderr, errors) = sink();
        assert_eq!(
            plan(&strings(&["-x"]), false, false, &stderr),
            Action::Exit(1)
        );
        assert_eq!(
            plan(
                &strings(&["--unknown-flag", "k", "-p"]),
                false,
                false,
                &stderr
            ),
            Action::Exit(1)
        );
        assert_eq!(
            plan(
                &strings(&["--fork", "a", "--continue", "-p"]),
                false,
                false,
                &stderr
            ),
            Action::Exit(1)
        );
        assert_eq!(
            plan(
                &strings(&["--session-id", "bad/id", "-p"]),
                false,
                false,
                &stderr
            ),
            Action::Exit(1)
        );
        assert_eq!(
            plan(&strings(&["--name", "   ", "-p"]), false, false, &stderr),
            Action::Exit(1)
        );
        assert_eq!(
            text(&errors),
            "Error: Unknown option: -x\nError: Unknown option: --unknown-flag\nError: --fork cannot be combined with --continue\nError: Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character\nError: --name requires a non-empty value\n"
        );
        assert_eq!(
            plan(&strings(&["-v", "-h"]), true, true, &stderr),
            Action::Version
        );
        assert_eq!(
            plan(&strings(&["-h", "-p"]), true, true, &stderr),
            Action::Help
        );
    }

    // Pi `test/initial-message.test.ts`: stdin comes first, then `@file` text,
    // then the first message; later messages stay separate prompts.
    #[test]
    fn initial_message_joins_stdin_file_text_and_the_first_message() {
        let mut messages = strings(&["first", "second"]);
        assert_eq!(
            build_initial_message(Some("piped".into()), "", &mut messages).as_deref(),
            Some("pipedfirst")
        );
        assert_eq!(messages, ["second"]);
        let mut messages = strings(&["review"]);
        assert_eq!(
            build_initial_message(
                Some("stdin\n".into()),
                "<file name=\"a\">\nhi\n</file>\n",
                &mut messages,
            )
            .as_deref(),
            Some("stdin\n<file name=\"a\">\nhi\n</file>\nreview")
        );
        assert!(messages.is_empty());
        assert_eq!(build_initial_message(None, "", &mut Vec::new()), None);
        assert_eq!(
            read_piped_stdin(Box::new(&b"  hello \n"[..])),
            Ok(Some("hello".into()))
        );
        assert_eq!(read_piped_stdin(Box::new(&b" \n"[..])), Ok(None));
    }

    fn session_args(configure: impl FnOnce(&mut Args)) -> Args {
        let mut args = parse_args(&[]);
        configure(&mut args);
        args
    }

    /// A session file Pi would list: a header and one user message.
    fn write_session(dir: &Path, id: &str, cwd: &str) -> PathBuf {
        let path = dir.join(format!("2026-01-01T00-00-00-000Z_{id}.jsonl"));
        let header = serde_json::json!({
            "type": "session", "version": 3, "id": id,
            "timestamp": "2026-01-01T00:00:00.000Z", "cwd": cwd,
        });
        let message = serde_json::json!({
            "type": "message", "id": "a1", "parentId": null,
            "timestamp": "2026-01-01T00:00:01.000Z",
            "message": { "role": "user", "content": "hi", "timestamp": 1 },
        });
        crate::test_support::write(&path, &format!("{header}\n{message}\n"));
        path
    }

    // The session store follows the home the environment names, not the
    // process's own Bake home.
    #[test]
    fn sessions_live_under_the_given_home() {
        let root = crate::test_support::TempDir::new("cli-home");
        let home = root.join("home");
        let cwd = root.join("project");
        std::fs::create_dir_all(&cwd).expect("project");
        let cwd = cwd.to_string_lossy().into_owned();
        let dir = default_session_dir_path(&cwd, &home);

        let mut warnings = Vec::new();
        let created =
            create_session_manager(&session_args(|_| {}), &cwd, None, &home, &mut warnings)
                .expect("create");
        assert_eq!(created.session_dir(), dir);
        drop(created);

        let file = write_session(&dir, "0193aaaa-0000-7000-8000-000000000000", &cwd);
        let continued = create_session_manager(
            &session_args(|args| args.continue_session = true),
            &cwd,
            None,
            &home,
            &mut warnings,
        )
        .expect("continue");
        assert_eq!(continued.session_file(), Some(file.as_path()));
        // The continued manager holds the file's writer lock until dropped.
        drop(continued);

        let by_prefix = create_session_manager(
            &session_args(|args| args.session = Some("0193aaaa".into())),
            &cwd,
            None,
            &home,
            &mut warnings,
        )
        .expect("by id");
        assert_eq!(by_prefix.session_file(), Some(file.as_path()));
        // A second writer of the held file is refused, by id and by
        // `--continue`, until the holder is dropped.
        let locked = Some(SessionStartError::Error(format!(
            "Session file is in use by another writer: {}",
            file.display()
        )));
        for args in [
            session_args(|args| args.session = Some("0193aaaa".into())),
            session_args(|args| args.continue_session = true),
        ] {
            assert_eq!(
                create_session_manager(&args, &cwd, None, &home, &mut warnings).err(),
                locked
            );
        }
        drop(by_prefix);

        // A session of another project, found under the same home.
        let other = root.join("other");
        let other = other.to_string_lossy().into_owned();
        write_session(
            &default_session_dir_path(&other, &home),
            "0193bbbb-0000-7000-8000-000000000000",
            &other,
        );
        assert_eq!(
            create_session_manager(
                &session_args(|args| args.session = Some("0193bbbb".into())),
                &cwd,
                None,
                &home,
                &mut warnings,
            )
            .err(),
            Some(SessionStartError::Error(format!(
                "Session found in different project: {other}. Use --fork 0193bbbb to fork it into this directory."
            )))
        );
        let forked = create_session_manager(
            &session_args(|args| {
                args.fork = Some("0193bbbb".into());
                args.session_id = Some("forked-id".into());
            }),
            &cwd,
            None,
            &home,
            &mut warnings,
        )
        .expect("forked");
        assert_eq!(forked.session_id(), "forked-id");
        drop(forked);
        assert_eq!(
            create_session_manager(
                &session_args(|args| {
                    args.fork = Some("0193bbbb".into());
                    args.session_id = Some("forked-id".into());
                }),
                &cwd,
                None,
                &home,
                &mut warnings,
            )
            .err(),
            Some(SessionStartError::Plain(
                "Session already exists with id 'forked-id'".into()
            ))
        );
        // Pi prints a missing id without the `Error: ` prefix.
        assert_eq!(
            create_session_manager(
                &session_args(|args| args.session = Some("ffff".into())),
                &cwd,
                None,
                &home,
                &mut warnings,
            )
            .err(),
            Some(SessionStartError::Plain(
                "No session found matching 'ffff'".into()
            ))
        );
    }

    // Pi `resolvePath(sessionArg, cwd)`: relative to `cwd`, `.` and `..`
    // removed, `~/` expanded.
    #[test]
    fn session_paths_resolve_against_cwd() {
        let home = Path::new("/unused-home");
        let cwd = if cfg!(windows) {
            "C:\\work\\p"
        } else {
            "/work/p"
        };
        let expected = Path::new(cwd).join("s").join("x.jsonl");
        assert!(matches!(
            resolve_session_path("./t/../s/x.jsonl", cwd, None, home),
            ResolvedSession::Path(path) if path == expected
        ));
        if let Some(user) = std::env::home_dir() {
            assert!(matches!(
                resolve_session_path("~/x.jsonl", cwd, None, home),
                ResolvedSession::Path(path) if path == user.join("x.jsonl")
            ));
        }
    }

    #[test]
    fn session_arguments_resolve_as_pi_resolves_them() {
        assert!(is_session_path("a/b"));
        assert!(is_session_path("a\\b"));
        assert!(is_session_path("x.jsonl"));
        assert!(!is_session_path("0193abc"));
    }

    #[test]
    fn text_from_a_file_or_server_is_one_terminal_safe_line() {
        assert_eq!(terminal_safe("plain \"name\" ok"), "plain \"name\" ok");
        assert_eq!(
            terminal_safe("a\u{1b}[31mb\nWarning: c\r\u{7}\u{9b}"),
            "a\\u{1b}[31mb\\u{a}Warning: c\\u{d}\\u{7}\\u{9b}"
        );
        assert_eq!(terminal_safe("x\u{202e}y\u{2066}"), "x\\u{202e}y\\u{2066}");
    }

    /// A directory tree for one setup: `home` (the Bake home), `work` (the
    /// working directory), and `user` (the user's home), private on Unix.
    fn setup_dirs(name: &str) -> crate::test_support::TempDir {
        let dir = crate::test_support::TempDir::new(name);
        for sub in ["home", "work", "user"] {
            std::fs::create_dir_all(dir.join(sub)).expect("setup directory");
        }
        #[cfg(unix)]
        for sub in ["", "home", "work", "user"] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path().join(sub), std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        dir
    }

    /// Writes `text` at `path`, writable by its owner only, whatever the
    /// umask.
    fn write_owned(path: &Path, text: &str) {
        std::fs::write(path, text).expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        }
    }

    fn setup_environment(dir: &Path) -> (CliEnvironment, Arc<Mutex<Vec<u8>>>) {
        let (stderr, buffer) = sink();
        let environment = CliEnvironment {
            cwd: dir.join("work"),
            home: Some(dir.join("home")),
            user_home: Some(dir.join("user")),
            docs_command: "bake docs".into(),
            stdin_is_terminal: true,
            stdin: Box::new(std::io::empty()),
            stdout: sink().0,
            stderr,
            extra_providers: Vec::new(),
            imported_default_model: None,
            imported_default_preset: None,
            sandbox_provider: Arc::new(bake_sandbox::provider::UnavailableProvider::new(None)),
            approver: None,
            warnings: Vec::new(),
            interrupt: no_interrupt(),
            plugins: WasmSetup::default(),
        };
        (environment, buffer)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn document_opt_in_obeys_cli_and_configured_tool_restrictions() {
        use bake_ai::providers::faux::{
            FauxProvider, FauxProviderOptions, FauxResponseStep, faux_assistant_message,
        };
        for (extra, defaults, expected) in [
            (vec![], None, true),
            (vec!["--no-tools"], None, false),
            (vec!["--no-builtin-tools"], None, false),
            (vec!["--tools", "read"], None, false),
            (vec!["--exclude-tools", "task_document"], None, false),
            (vec![], Some(serde_json::json!(["read"])), false),
            (
                vec!["--tools", "+task_document"],
                Some(serde_json::json!([])),
                true,
            ),
        ] {
            let dir = setup_dirs("cli-document-selection");
            let mut settings = serde_json::json!({"taskDocuments":true});
            if let Some(defaults) = defaults {
                settings["defaultTools"] = defaults;
            }
            write_owned(&dir.join("home/settings.json"), &settings.to_string());
            let faux = Arc::new(FauxProvider::new(FauxProviderOptions::default()));
            faux.set_responses(vec![FauxResponseStep::from_fn(move |context, _, _, _| {
                assert_eq!(
                    bake_ai::transcript::get_current_tools(context.messages())
                        .iter()
                        .any(|tool| tool.name == "task_document"),
                    expected
                );
                Ok(faux_assistant_message("done"))
            })]);
            let (mut environment, errors) = setup_environment(dir.path());
            environment.extra_providers.push((
                "faux".into(),
                serde_json::json!({
                    "baseUrl":"http://127.0.0.1:9/unused", "api":faux.model().api,
                    "models":[{"id":"faux-1"}]
                }),
                Some("test-key".into()),
            ));
            let mut args = vec![
                "--model",
                "faux/faux-1",
                "--permission",
                "danger-full-access",
                "--no-session",
                "--no-mcp",
            ];
            args.extend(extra);
            let Ok(started) = start_interactive(environment, parse_args(&strings(&args))).await
            else {
                panic!("setup failed: {}", text(&errors));
            };
            started.session.apis().register(faux.clone());
            started
                .session
                .prompt("Continue", Vec::new())
                .await
                .unwrap();
            assert_eq!(faux.call_count(), 1);
            started.session.shutdown().await;
        }
    }

    // Interactive setup and every replacement session share print mode's
    // consumer: each reads the home's `mcp.json` and starts its servers,
    // and `--no-mcp` starts none. The server's program does not exist, so
    // nothing runs; under `danger-full-access` its admission needs no
    // backend.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn interactive_and_replacement_sessions_start_the_homes_servers() {
        use crate::mcp::McpServerState;
        let dir = setup_dirs("cli-mcp-consumer");
        let missing = dir.join("no-such-program");
        let config = serde_json::json!({ "mcpServers": {
            "absent": { "command": missing.to_string_lossy() },
            "off": { "command": "x", "enabled": false },
            // Refused by the egress policy without any lookup or
            // connection: a public literal over plain http.
            "remote": { "url": "http://93.184.216.34/mcp" },
            "bad\u{1b}name": { "command": "x" },
        } });
        write_owned(&dir.join("home").join("mcp.json"), &config.to_string());
        let args = |extra: &[&str]| {
            let mut args = vec!["--sandbox", "danger-full-access", "--no-session"];
            args.extend_from_slice(extra);
            parse_args(&strings(&args))
        };

        let (environment, stderr) = setup_environment(dir.path());
        let Ok(started) = start_interactive(environment, args(&[])).await else {
            panic!("setup failed: {}", text(&stderr));
        };
        let warnings = text(&stderr);
        assert!(warnings.contains("Warning: MCP: "), "{warnings}");
        assert!(
            warnings.contains("invalid server name \"bad\\u{1b}name\""),
            "{warnings}"
        );
        assert!(!warnings.contains('\u{1b}'), "{warnings}");
        let wait = |servers: crate::mcp::McpServers| async move {
            servers.wait_for_startup(|| false).await;
            servers.statuses()
        };
        let servers = started
            .session
            .mcp_servers()
            .expect("servers started")
            .clone();
        let statuses = wait(servers).await;
        let names: Vec<_> = statuses.iter().map(|status| status.name.as_str()).collect();
        assert_eq!(names, ["absent", "off", "remote"]);
        assert!(
            matches!(&statuses[0].state, McpServerState::Failed { error } if error.contains("failed to start")),
            "{statuses:?}"
        );
        assert_eq!(statuses[1].state, McpServerState::Disabled);
        assert!(
            matches!(&statuses[2].state, McpServerState::Refused { reason } if reason.contains("clear text")),
            "{statuses:?}"
        );

        // A replacement starts no server while the session it replaces
        // runs; the caller starts them once that session is shut down.
        let mut replacement = started
            .factory
            .replace(&started.session, SessionTarget::New)
            .await
            .expect("replacement");
        assert!(replacement.session.mcp_servers().is_none());
        assert!(replacement.has_pending_servers());
        started.session.shutdown().await;
        let old = started
            .session
            .mcp_servers()
            .expect("old servers")
            .statuses();
        assert!(
            old.iter().all(|status| matches!(
                status.state,
                McpServerState::Failed { .. }
                    | McpServerState::Disabled
                    | McpServerState::Refused { .. }
                    | McpServerState::Closed
            )),
            "{old:?}"
        );
        replacement.start_servers().expect("start");
        assert!(!replacement.has_pending_servers());
        let servers = replacement
            .session
            .mcp_servers()
            .expect("replacement servers")
            .clone();
        assert_eq!(wait(servers).await.len(), 3);
        // Once only.
        replacement.start_servers().expect("nothing left to start");
        replacement.session.shutdown().await;

        // A replacement shut down unused never starts its servers.
        let (environment, stderr) = setup_environment(dir.path());
        let Ok(started) = start_interactive(environment, args(&[])).await else {
            panic!("setup failed: {}", text(&stderr));
        };
        let mut unused = started
            .factory
            .replace(&started.session, SessionTarget::New)
            .await
            .expect("replacement");
        unused.session.shutdown().await;
        assert!(unused.start_servers().is_err());
        assert!(unused.session.mcp_servers().is_none());
        started.session.shutdown().await;

        let (environment, stderr) = setup_environment(dir.path());
        let Ok(started) = start_interactive(environment, args(&["--no-mcp"])).await else {
            panic!("setup failed: {}", text(&stderr));
        };
        assert!(started.session.mcp_servers().is_none());
        assert_eq!(text(&stderr), "");
        let replacement = started
            .factory
            .replace(&started.session, SessionTarget::New)
            .await
            .expect("replacement");
        assert!(replacement.session.mcp_servers().is_none());
        replacement.session.shutdown().await;
        started.session.shutdown().await;

        // `--no-tools` alone leaves nothing an MCP tool could be declared
        // as, so no server starts; with `--tools` naming one, they do.
        for (extra, starts) in [
            (&["-nt"][..], false),
            (&["-nt", "-t", "mcp__absent__x"][..], true),
        ] {
            let (environment, stderr) = setup_environment(dir.path());
            let Ok(started) = start_interactive(environment, args(extra)).await else {
                panic!("setup failed: {}", text(&stderr));
            };
            assert_eq!(started.session.mcp_servers().is_some(), starts, "{extra:?}");
            started.session.shutdown().await;
        }
    }

    // Every session setup builds runs the hooks files `settings.json`
    // names, and warns about a file it cannot use. A steered prompt runs
    // `UserPromptSubmit` without a model, so the hook's block shows.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sessions_run_the_hooks_files_settings_names() {
        let dir = setup_dirs("cli-hooks");
        let hooks = serde_json::json!({ "hooks": { "UserPromptSubmit": [{ "hooks": [
            { "type": "command", "command": "echo 'not today' >&2; exit 2" },
        ] }] } });
        write_owned(&dir.join("user").join("claude.json"), &hooks.to_string());
        write_owned(
            &dir.join("home").join("settings.json"),
            &serde_json::json!({ "hooks": { "claudeCode": "~/claude.json", "codex": "missing.json" } })
                .to_string(),
        );
        let args = parse_args(&strings(&[
            "--sandbox",
            "danger-full-access",
            "--no-session",
        ]));
        let (environment, stderr) = setup_environment(dir.path());
        let Ok(started) = start_interactive(environment, args).await else {
            panic!("setup failed: {}", text(&stderr));
        };
        let warnings = text(&stderr);
        assert!(
            warnings.contains(&format!(
                "Warning: Codex hooks: {}: not found. None of its hooks run",
                dir.join("home").join("missing.json").display()
            )),
            "{warnings}"
        );
        let blocked = Err("Prompt blocked by UserPromptSubmit hook: not today".to_owned());
        assert_eq!(started.session.steer("hello", Vec::new()).await, blocked);
        let replacement = started
            .factory
            .replace(&started.session, SessionTarget::New)
            .await
            .expect("replacement");
        assert_eq!(
            replacement.session.steer("hello", Vec::new()).await,
            blocked
        );
        replacement.session.shutdown().await;
        started.session.shutdown().await;
    }

    // A file the session's own tools could change is refused whole, before
    // any server starts.
    // Where who may change `mcp.json` cannot be shown yet, the shared
    // consumer refuses it unread and starts nothing.
    #[cfg(not(unix))]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unchecked_platform_starts_no_server() {
        let dir = setup_dirs("cli-mcp-unchecked");
        write_owned(
            &dir.join("home").join("mcp.json"),
            r#"{"mcpServers":{"a":{"command":"x"}}}"#,
        );
        let (environment, stderr) = setup_environment(dir.path());
        let args = parse_args(&strings(&[
            "--sandbox",
            "danger-full-access",
            "--no-session",
        ]));
        let Ok(started) = start_interactive(environment, args).await else {
            panic!("setup failed: {}", text(&stderr));
        };
        assert!(started.session.mcp_servers().is_none());
        assert!(
            text(&stderr).contains(crate::mcp::home::UNCHECKED_PLATFORM),
            "{}",
            text(&stderr)
        );
        started.session.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_home_in_the_working_directory_starts_no_server() {
        let dir = setup_dirs("cli-mcp-workspace-home");
        let (mut environment, stderr) = setup_environment(dir.path());
        let home = dir.join("work").join(".bake");
        std::fs::create_dir_all(&home).expect("home");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        }
        write_owned(
            &home.join("mcp.json"),
            r#"{"mcpServers":{"a":{"command":"x"}}}"#,
        );
        environment.home = Some(home);
        let args = parse_args(&strings(&[
            "--sandbox",
            "danger-full-access",
            "--no-session",
        ]));
        let Ok(started) = start_interactive(environment, args).await else {
            panic!("setup failed: {}", text(&stderr));
        };
        assert!(started.session.mcp_servers().is_none());
        let warnings = text(&stderr);
        assert!(
            warnings.contains("inside the session's working directory")
                && warnings.contains("No MCP server was started from it"),
            "{warnings}"
        );
        started.session.shutdown().await;
    }
}
