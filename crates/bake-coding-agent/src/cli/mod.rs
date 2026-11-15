//! The `bake` agent command line: arguments, session selection, model
//! selection, and print mode.
//!
//! Ported from Pi `packages/coding-agent/src/main.ts` (v1.1.0):
//! `readPipedStdin`, `resolveAppMode`, `createSessionManager`,
//! `resolveSessionPath`, `buildSessionOptions`, and the print-mode branch of
//! `main`, and from `src/cli/initial-message.ts`. Settings, `models.json`,
//! `auth.json`, and sessions come from the Bake home, the one
//! [`CliEnvironment::home`] names, never the process's own. The global
//! `AGENTS.md` and the user's skills come from the universal `~/.agents`
//! under [`CliEnvironment::user_home`], and a project's `AGENTS.md` files
//! and `.agents/skills` from the session's working directory and its
//! parents ([`crate::skills`]).
//! Diagnostics go to standard error as `Error: ` and `Warning: ` lines
//! without Pi's colors, except Pi's bare lines (`No session found
//! matching`, and Pi v1.5.2's `Stored session working directory does not
//! exist`, which print and RPC mode give for a session whose stored
//! working directory is gone), and every failure exits with code 1, as in
//! Pi.
//!
//! Bake composes its permission layer ([`crate::permissions`]) here:
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
//! Bake's own, for programs that run `bake`: Pi reads piped standard input
//! to its end whenever it is not a terminal, so a caller that leaves the
//! stream open and silent (a harness's inherited pipe, ssh, a CI runner)
//! waits forever. Here, when the prompt also comes from a message, an
//! `@file`, or `--resume-run`, a pipe that sends nothing within
//! [`STDIN_FIRST_BYTE_WAIT`] is left unread, with a warning; once a byte
//! arrives, or with no other prompt, or given `-`, it is read to its end
//! as Pi reads it. The same rule the senpi fork of Pi adopted
//! (`code-yeongyu/senpi` #3719), with a longer wait for slow producers.
//! `--output-format` is another way to choose print mode's output
//! ([`plan`]), and its result modes report a startup failure as a result
//! object too ([`run`]).
//!
//! Not ported (deferred): theme and extension flags, extension tools, and
//! interactive mode's missing-session-cwd prompt (continue, fork, or
//! cancel), so interactive mode opens such a session as before.

pub mod args;
pub mod auth_command;
mod build;
pub mod export;
mod factory;
pub mod file_processor;
pub mod list_models;
mod planning;
mod session_open;
mod start;
mod stdin;

#[cfg(test)]
use build::terminal_safe;
use build::{Build, Built, Role, build_session, load_resources, mcp_problems, start_servers};
pub use factory::{Replacement, SessionFactory, SessionTarget};
pub use planning::plan;
use planning::{build_initial_message, error, report, warn};
#[cfg(test)]
use session_open::{ResolvedSession, resolve_session_path};
use session_open::{SessionStartError, create_session_manager, is_session_path};
use start::{StartKind, start_with};
#[cfg(test)]
use stdin::read_piped_stdin;
use stdin::{Piped, read_stdin, silent_stdin_warning};

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
use crate::session::cwd::{format_missing_session_cwd_error, get_missing_session_cwd_issue};
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

/// How long print mode waits for piped standard input's first byte when
/// the prompt also comes from the command line; see the module docs.
pub const STDIN_FIRST_BYTE_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

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
    /// Providers to add below `models.json`
    /// ([`ModelRegistry::add_provider_config`]): id, config, and key.
    pub extra_providers: Vec<(String, serde_json::Value, Option<String>)>,
    /// Confines bash under each call's policy: the binary passes
    /// [`bake_sandbox::default_provider`], which fails every confined call
    /// closed on a host without a backend.
    pub sandbox_provider: Arc<dyn SandboxProvider>,
    /// Who answers approval requests: interactive mode passes its
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
    /// What the process's environment says about telecom and spawned
    /// agents: the binary passes [`crate::telecom::TelecomEnv::from_process`],
    /// a test its own.
    pub telecom_env: crate::telecom::TelecomEnv,
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

/// Why `--approve` and `--no-approve` are refused. Pi's project trust
/// decides whether a project's own files are loaded, and `bake` has no
/// such decision: a project's `AGENTS.md` files
/// ([`crate::system_prompt`]), its `.agents/skills` ([`crate::skills`]),
/// and its `.bake/prompts` ([`crate::prompt_templates`]) are always
/// loaded, and it reads no project settings ([`crate::settings`]) and no
/// project `mcp.json` ([`crate::mcp`]). Bake's `--approval` is a different
/// thing: the session's approval policy.
pub const PROJECT_TRUST_REFUSED: &str = "--approve and --no-approve need project trust, which bake does not have: a project's AGENTS.md, .agents/skills, and .bake/prompts are always loaded, and no project settings or mcp.json is read. For the session's approval policy, see --approval";

/// `BAKE_OFFLINE`.
pub const BAKE_OFFLINE_ENV: &str = "BAKE_OFFLINE";

/// Pi's `offlineMode`: `--offline`, or a truthy environment setting. Where
/// Pi reads `PI_OFFLINE`, Bake reads [`BAKE_OFFLINE_ENV`]; a blank value
/// counts as unset. The value's test is Pi's `isTruthyEnvFlag`: `1`,
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
    env(BAKE_OFFLINE_ENV)
        .map(|value| value.to_string_lossy().trim().to_ascii_lowercase())
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes"))
}

/// Whether `--resume` opens the session picker for this run, as Pi's
/// `createSessionManager` reaches its `parsed.resume` branch: `--no-session`,
/// `--fork`, and `--session` are taken first and leave `--resume` unused.
/// `--session-id` and `--fork` with `--resume` are refused before this
/// (`planning`'s `validate_session_id_flags` and `validate_fork_flags`).
pub fn resumes_at_startup(args: &Args) -> bool {
    args.resume && !args.no_session && args.fork.is_none() && args.session.is_none()
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
    /// The session's standing permission, as settled at startup, which the
    /// terminal's opening banner names.
    pub permission: crate::permissions::SessionPermission,
}

/// Runs `bake` print mode; returns the exit code. Must run inside a
/// multi-threaded Tokio runtime. With `--output-format json` or
/// `stream-json`, a startup failure is reported as a result object on
/// standard output too ([`crate::print_mode::startup_failure_json`]), with
/// the errors standard error got; a stop request writes none.
pub async fn run(mut environment: CliEnvironment, args: Args, mode: Mode) -> i32 {
    if let Some(search_pattern) = args.list_models.clone() {
        return run_list_models(environment, search_pattern.as_deref()).await;
    }
    let output = match mode {
        Mode::Json => PrintOutput::Json,
        Mode::Result => PrintOutput::Result,
        Mode::StreamJson => PrintOutput::StreamJson,
        // [`plan`] sends `--mode rpc` to [`run_rpc`], never here.
        Mode::Text | Mode::Rpc => PrintOutput::Text,
    };
    let failures = output.writes_result().then(|| {
        let (stderr, kept) = crate::print_mode::capture_errors(environment.stderr.clone());
        environment.stderr = stderr;
        (environment.stdout.clone(), kept)
    });
    let report = |code: i32| {
        // A stop request's status is a signal's, and reports nothing.
        if code == 1
            && let Some((stdout, kept)) = &failures
        {
            let errors = kept.lock().map(|kept| kept.clone()).unwrap_or_default();
            write_out(stdout, &crate::print_mode::startup_failure_json(&errors));
        }
        code
    };
    let max_model_calls = args.max_model_calls;
    let run_timeout_ms = args.run_timeout_ms;
    let recovery = match (&args.resume_run, args.recoverable) {
        (Some(run_id), _) => PrintRunRecovery::Resume(run_id.clone()),
        (_, true) => PrintRunRecovery::Start,
        _ => PrintRunRecovery::Disabled,
    };
    let started = match start(environment, args).await {
        Ok(started) => started,
        Err(code) => return report(code),
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
        permission: _,
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
                return report(error(
                    &stderr,
                    "--run-timeout-ms exceeds this platform's clock range",
                ));
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
        mode: output,
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
/// arrives. `approvals` is the queue the client answers; without one
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
        permission: _,
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

#[cfg(test)]
mod tests;
