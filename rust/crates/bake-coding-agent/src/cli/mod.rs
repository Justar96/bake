//! The `bake-rs` agent command line: arguments, session selection, model
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
//! Not ported (deferred): RPC mode, `--resume`,
//! theme and extension flags, extension and MCP tools, and the
//! missing-session-cwd prompt.

pub mod args;
pub mod file_processor;
pub mod list_models;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bake_ai::ApiRegistry;
use bake_sandbox::SandboxProvider;

use crate::agent_session::{AgentSession, AgentSessionOptions, format_no_models_available_message};
use crate::cli::file_processor::{ProcessFileOptions, process_file_arguments};
use crate::cli::list_models::list_models;
use crate::home::sessions_dir;
use crate::model_registry::ModelRegistry;
use crate::model_registry::resolver::{
    ScopedModel, resolve_cli_model, resolve_model_scope_from_models,
};
use crate::permissions::{
    Approver, NoApprover, PermissionInputs, ToolPermissions, prepare_session_permission,
    sandbox_file_tools, sandboxed_bash_tool,
};
use crate::print_mode::{PrintModeOptions, PrintOutput, SharedWriter, run_print_mode, write_out};
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
    NoTools, ToolSelectionOptions, ToolsOptions, create_all_tool_definitions, select_tools,
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
    /// How to run `bake-rs docs`, which the prompt and startup errors name
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
/// A `bake-rs` sign-in ([`crate::cliproxyapi::account`]) comes first:
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

/// What `bake-rs` should do with its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Print help.
    Help,
    /// Print the version.
    Version,
    /// Run print mode.
    Print(Box<Args>, Mode),
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
    let parsed = parse_args(args);
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
    if parsed.help {
        return Action::Help;
    }
    if let Some(message) = unknown_flags_message(&parsed.unknown_flags) {
        return Action::Exit(error(stderr, &message));
    }
    if let Some(message) = validate_fork_flags(&parsed) {
        return Action::Exit(error(stderr, &message));
    }
    if let Some(message) = validate_session_id_flags(&parsed) {
        return Action::Exit(error(stderr, &message));
    }
    if let Some(name) = &parsed.name
        && normalize_session_name(name).is_none()
    {
        return Action::Exit(error(stderr, "--name requires a non-empty value"));
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
        None => return Action::Interactive(Box::new(parsed)),
    };
    Action::Print(Box::new(parsed), mode)
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
}

/// Runs `bake-rs` print mode; returns the exit code. Must run inside a
/// multi-threaded Tokio runtime.
pub async fn run(environment: CliEnvironment, args: Args, mode: Mode) -> i32 {
    if let Some(search_pattern) = args.list_models.clone() {
        return run_list_models(environment, search_pattern.as_deref()).await;
    }
    let started = match start(environment, args).await {
        Ok(started) => started,
        Err(code) => return code,
    };
    let Started {
        session,
        initial_message,
        initial_images,
        messages,
        stdout,
        stderr,
        interrupt,
        home: _,
        factory: _,
    } = started;
    let options = PrintModeOptions {
        mode: match mode {
            Mode::Text => PrintOutput::Text,
            Mode::Json => PrintOutput::Json,
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
    start_with(environment, args, false).await
}

/// [`start`] for interactive mode, which, as Pi's `main` does, starts
/// without a model, or with one that has no key, so the person can sign in
/// from the session; a prompt is then refused with Pi's message. Invalid
/// flags and an explicit `--model` that matches nothing still fail.
pub async fn start_interactive(environment: CliEnvironment, args: Args) -> Result<Started, i32> {
    start_with(environment, args, true).await
}

/// What a session's setup reads besides its session file and model
/// selection: the context files, skills, prompt templates, permission, and
/// prompt overrides, all for the session's working directory. Pi's
/// resource loader and `buildSessionOptions`, without extensions.
struct Resources {
    context_files: Vec<crate::system_prompt::ContextFile>,
    skills: Vec<crate::skills::Skill>,
    prompt_templates: Vec<crate::prompt_templates::PromptTemplate>,
    permission: crate::permissions::SessionPermission,
    custom_prompt: Option<String>,
    append_system_prompt: Option<String>,
}

/// Settles `session`'s permission (D24), writing any change to its file
/// before anything can run, and loads the resources for its working
/// directory. `permission_flags` applies `--permission`, `--sandbox`, and
/// `--approval`; a resumed session leaves them out and keeps its own.
/// Blocking.
#[allow(clippy::too_many_arguments)]
fn load_resources(
    session: &mut SessionManager,
    settings: &Settings,
    args: &Args,
    home: &Path,
    user_home: Option<PathBuf>,
    imported_default_preset: Option<&str>,
    permission_flags: bool,
    warnings: &mut Vec<String>,
) -> Result<Resources, String> {
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
        skills,
        prompt_templates,
        permission,
        custom_prompt,
        append_system_prompt,
    })
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
}

/// Pi's `_buildRuntime` tools, `createAgentSession` tool selection, and
/// the system prompt, then the session itself.
async fn build_session(args: &Args, build: Build) -> Result<AgentSession, String> {
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
    } = build;
    let Resources {
        context_files,
        skills,
        prompt_templates,
        permission,
        custom_prompt,
        append_system_prompt,
    } = resources;
    let session_cwd = session.cwd().to_owned();
    let session_id = session.session_id().to_owned();
    let can_ask = approver.is_some();
    let tool_permissions = ToolPermissions::new(
        Arc::new(permission.standing(Path::new(&session_cwd), Some(session_id.clone()))),
        approver.unwrap_or_else(|| Arc::new(NoApprover)),
        sandbox_provider,
    );
    let session_file = session
        .session_file()
        .map(|file| file.to_string_lossy().into_owned());
    // The read tool asks for the session's model when it runs, and bash
    // reads the model and thinking level for its `BAKE_*` variables when
    // each call starts; both read what the session publishes now, so a
    // model change reaches the next call.
    let published = crate::agent_session::SessionSelection::default();
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
                context: Some(bash_context),
                ..Default::default()
            },
            &tool_permissions,
        )?),
    };
    let definitions = sandbox_file_tools(
        create_all_tool_definitions(Path::new(&session_cwd), &tools_options),
        Path::new(&session_cwd),
        &tool_permissions,
    );
    let available: Vec<String> = definitions
        .iter()
        .map(|definition| definition.name().to_owned())
        .collect();
    let selection = select_tools(
        &ToolSelectionOptions {
            tools: args.tools.clone(),
            exclude_tools: args.exclude_tools.clone(),
            no_tools: if args.no_tools {
                Some(NoTools::All)
            } else if args.no_builtin_tools {
                Some(NoTools::Builtin)
            } else {
                None
            },
        },
        settings.default_tools.as_deref(),
        &available,
    )?;
    let registered: Vec<_> = definitions
        .into_iter()
        .filter(|definition| {
            selection
                .registered
                .iter()
                .any(|name| name == definition.name())
        })
        .collect();

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
    })
    .await?;
    session.set_prompt_templates(prompt_templates);
    Ok(session)
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
/// warned about.
pub struct Replacement {
    pub session: AgentSession,
    pub warnings: Vec<String>,
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
        }
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

    /// Builds the session `/fork` replaces `current` with, Pi's runtime
    /// `fork` before the user message `entry_id`
    /// ([`AgentSession::fork_session_manager`]), and returns it with the
    /// message's text, for the editor. A fork that holds history restores
    /// the model, thinking level, and permission its path records, as
    /// `/resume` does; an empty fork, from a first message, starts as
    /// `/new` does. `current` and its file are not touched; a persisted
    /// fork's file may already be written when a later step fails.
    pub async fn fork(
        &self,
        current: &AgentSession,
        entry_id: &str,
    ) -> Result<(Replacement, String), String> {
        let forked = current.fork_session_manager(entry_id).await?;
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
                !resume,
                &mut warnings,
            )?;
            Ok::<_, String>((session, resources, settings, warnings))
        })
        .await
        .map_err(|error| format!("Session setup failed: {error}"))??;
        let (session, resources, settings, warnings) = loaded;
        let session = build_session(
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
            },
        )
        .await?;
        Ok(Replacement { session, warnings })
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
    allow_no_model: bool,
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
        warnings: startup_warnings,
        interrupt,
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
        // Pi points at `--list-models`, which bake-rs does not have.
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

    let session = match build_session(
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
        },
    )
    .await
    {
        Ok(session) => session,
        Err(message) => return Err(error(&stderr, &message)),
    };

    let piped = if stdin_is_terminal {
        Ok(None)
    } else {
        tokio::task::spawn_blocking(move || read_piped_stdin(stdin))
            .await
            .unwrap_or_else(|join| Err(join.to_string()))
    };
    let piped = match piped {
        Ok(piped) => piped,
        Err(message) => {
            session.shutdown().await;
            return Err(error(&stderr, &message));
        }
    };
    let initial_message = build_initial_message(piped, &processed_files.text, &mut args.messages);

    if session.model().is_none() && !allow_no_model {
        session.shutdown().await;
        write_out(
            &stderr,
            &format!("{}\n", format_no_models_available_message(&docs_command)),
        );
        return Err(1);
    }
    Ok(Started {
        session,
        initial_message,
        initial_images: processed_files.images,
        messages: args.messages,
        stdout,
        stderr,
        interrupt,
        home,
        factory,
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
}
