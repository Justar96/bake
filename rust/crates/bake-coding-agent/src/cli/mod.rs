//! The `bake-rs` agent command line: arguments, session selection, model
//! selection, and print mode.
//!
//! Ported from Pi `packages/coding-agent/src/main.ts` (v1.1.0):
//! `readPipedStdin`, `resolveAppMode`, `createSessionManager`,
//! `resolveSessionPath`, `buildSessionOptions`, and the print-mode branch of
//! `main`, and from `src/cli/initial-message.ts`. Settings, `models.json`,
//! `auth.json`, sessions, and the global `AGENTS.md` come from the Bake home
//! (D25), the one [`CliEnvironment::home`] names, never the process's own.
//! Diagnostics go to standard error as `Error: ` and `Warning: ` lines
//! without Pi's colors, except Pi's bare `No session found matching`
//! line, and every failure exits with code 1, as in Pi.
//!
//! Not ported (deferred): interactive and RPC modes, `@file` arguments,
//! `--resume`, `--fork`, `--session-id`, `--session-dir`, `--api-key`,
//! `--models`, tool and resource flags, forking a session found in another
//! project, and the missing-session-cwd prompt.

pub mod args;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bake_ai::ApiRegistry;

use crate::agent_session::{AgentSession, AgentSessionOptions, format_no_models_available_message};
use crate::home::sessions_dir;
use crate::model_registry::ModelRegistry;
use crate::model_registry::resolver::resolve_cli_model;
use crate::print_mode::{PrintModeOptions, PrintOutput, SharedWriter, run_print_mode, write_out};
use crate::session::file::default_session_dir_path;
use crate::session::paths::{lexical_normalize, normalize_path};
use crate::session::{NewSessionOptions, SessionManager};
use crate::settings::Settings;
use crate::system_prompt::{DocsPaths, SystemPromptOptions, load_project_context_files};

pub use args::{Args, Diagnostic, DiagnosticKind, Mode, parse_args, unknown_flags_message};

/// The most piped standard input read for a prompt.
pub const MAX_STDIN_BYTES: u64 = 64 * 1024 * 1024;

/// Everything the command line reads from its process, so tests can supply
/// their own.
pub struct CliEnvironment {
    /// The working directory.
    pub cwd: PathBuf,
    /// The Bake home; `None` when it cannot be resolved.
    pub home: Option<PathBuf>,
    /// Where Pi's prompt says its docs are.
    pub docs: DocsPaths,
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
    /// For [`CliEnvironment::warnings`]; never holds the key.
    pub warnings: Vec<String>,
}

/// Reads the CLIProxyAPI route, key, and default model from the Bake home
/// at `home`, read-only, through
/// [`import_cliproxyapi`](crate::cliproxyapi::import_cliproxyapi). A home
/// without a route contributes nothing; an unreadable one contributes a
/// warning, so a `models.json` route still runs.
pub fn import_startup(
    home: &std::path::Path,
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> ImportedStartup {
    use crate::cliproxyapi::{CLIPROXYAPI_ID, import_cliproxyapi};
    let mut startup = ImportedStartup::default();
    match import_cliproxyapi(home, env) {
        Ok(imported) => {
            startup.default_model = imported.default_model;
            if let Some(route) = imported.route {
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
    startup
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

/// Pi's argument checks and `resolveAppMode`, before anything is loaded.
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
    if !parsed.file_args.is_empty() {
        return Action::Exit(error(
            stderr,
            "@file arguments are not supported by bake-rs yet",
        ));
    }
    let mode = match parsed.mode {
        Some(mode) => mode,
        None if parsed.print || !stdin_is_terminal || !stdout_is_terminal => Mode::Text,
        None => {
            return Action::Exit(error(
                stderr,
                "Interactive mode is not available in bake-rs yet. Use -p or --mode json.",
            ));
        }
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

/// Pi's `buildInitialMessage` without files: stdin, then the first
/// message; the rest stay prompts of their own.
fn build_initial_message(stdin: Option<String>, messages: &mut Vec<String>) -> Option<String> {
    let mut parts: Vec<String> = stdin.into_iter().collect();
    if !messages.is_empty() {
        parts.push(messages.remove(0));
    }
    (!parts.is_empty()).then(|| parts.concat())
}

/// Pi's `resolveSessionPath` result.
enum ResolvedSession {
    Path(PathBuf),
    Global { cwd: String },
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
) -> Result<SessionManager, SessionStartError> {
    let failed = |error: crate::session::SessionError| SessionStartError::Error(error.to_string());
    if args.no_session {
        return SessionManager::in_memory(Some(cwd), NewSessionOptions::default(), Vec::new())
            .map_err(failed);
    }
    if let Some(arg) = &args.session {
        return match resolve_session_path(arg, cwd, session_dir, home) {
            ResolvedSession::Path(path) => {
                SessionManager::open(&path, session_dir, None).map_err(failed)
            }
            // Pi asks whether to fork the session; forking is not ported.
            ResolvedSession::Global { cwd } => Err(SessionStartError::Error(format!(
                "Session found in different project: {cwd}. Forking it is not supported by bake-rs yet."
            ))),
            ResolvedSession::NotFound => Err(SessionStartError::Plain(format!(
                "No session found matching '{arg}'"
            ))),
        };
    }
    let dir = match session_dir {
        Some(dir) => dir.to_path_buf(),
        None => default_session_dir_path(cwd, home),
    };
    if args.continue_session {
        return SessionManager::continue_recent(cwd, Some(&dir)).map_err(failed);
    }
    SessionManager::create(cwd, Some(&dir), NewSessionOptions::default()).map_err(failed)
}

/// Runs `bake-rs` print mode; returns the exit code. Must run inside a
/// multi-threaded Tokio runtime.
pub async fn run(environment: CliEnvironment, args: Args, mode: Mode) -> i32 {
    let CliEnvironment {
        cwd,
        home,
        docs,
        stdin_is_terminal,
        stdin,
        stdout,
        stderr,
        extra_providers,
        imported_default_model,
        warnings: startup_warnings,
        interrupt,
    } = environment;
    let mut args = args;
    let Some(home) = home else {
        return error(
            &stderr,
            "Cannot find the Bake home: set BAKE_HOME or make the user's home directory resolvable",
        );
    };
    let cwd = cwd.to_string_lossy().into_owned();

    // Settings, models, and the session store are blocking file work.
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
            if let Some(message) = registry.error() {
                warnings.push(message);
            }
            let session =
                create_session_manager(&args, &cwd, settings.session_dir.as_deref(), &home);
            let session = session.map(|session| {
                let (files, context_warnings) = load_project_context_files(session.cwd(), &home);
                warnings.extend(context_warnings);
                (session, files)
            });
            (settings, registry, warnings, session)
        })
        .await
    };
    let (settings, registry, warnings, session) = match loaded {
        Ok(loaded) => loaded,
        Err(join) => return error(&stderr, &format!("Startup failed: {join}")),
    };
    let (session, context_files) = match session {
        Ok(session) => session,
        Err(SessionStartError::Error(message)) => return error(&stderr, &message),
        Err(SessionStartError::Plain(message)) => {
            write_out(&stderr, &format!("{message}\n"));
            return 1;
        }
    };
    let session_cwd = session.cwd().to_owned();

    // Pi's `buildSessionOptions`.
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
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
        if let Some(message) = resolved.error {
            diagnostics.push(Diagnostic {
                kind: DiagnosticKind::Error,
                message,
            });
        }
        if let Some(found) = resolved.model {
            model = Some(found);
            if args.thinking.is_none() && resolved.thinking_level.is_some() {
                thinking_level = resolved.thinking_level;
            }
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
        return 1;
    }

    let mut system_prompt = SystemPromptOptions::new(session_cwd, docs.clone());
    system_prompt.context_files = context_files;
    let session = match AgentSession::create(AgentSessionOptions {
        session,
        registry: Arc::new(registry),
        apis: Arc::new(ApiRegistry::with_builtins()),
        settings,
        model,
        thinking_level,
        tools: Vec::new(),
        active_tool_names: Vec::new(),
        tool_prompts: Vec::new(),
        system_prompt,
    })
    .await
    {
        Ok(session) => session,
        Err(message) => return error(&stderr, &message),
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
            return error(&stderr, &message);
        }
    };
    let initial_message = build_initial_message(piped, &mut args.messages);

    if session.model().is_none() {
        session.shutdown().await;
        write_out(
            &stderr,
            &format!("{}\n", format_no_models_available_message(&docs.docs)),
        );
        return 1;
    }
    let options = PrintModeOptions {
        mode: match mode {
            Mode::Text => PrintOutput::Text,
            Mode::Json => PrintOutput::Json,
        },
        initial_message,
        initial_images: Vec::new(),
        messages: args.messages,
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
        assert_eq!(
            plan(&strings(&["hi"]), true, true, &stderr),
            Action::Exit(1)
        );
        assert!(text(&errors).contains("Interactive mode is not available"));
    }

    #[test]
    fn argument_errors_exit_before_loading() {
        let (stderr, errors) = sink();
        assert_eq!(
            plan(&strings(&["-x"]), false, false, &stderr),
            Action::Exit(1)
        );
        assert_eq!(
            plan(&strings(&["--api-key", "k", "-p"]), false, false, &stderr),
            Action::Exit(1)
        );
        assert_eq!(
            plan(&strings(&["-p", "@f"]), false, false, &stderr),
            Action::Exit(1)
        );
        assert_eq!(
            text(&errors),
            "Error: Unknown option: -x\nError: Unknown option: --api-key\nError: @file arguments are not supported by bake-rs yet\n"
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

    // Pi `test/initial-message.test.ts`: stdin comes first, then the first
    // message; later messages stay separate prompts.
    #[test]
    fn initial_message_joins_stdin_and_the_first_message() {
        let mut messages = strings(&["first", "second"]);
        assert_eq!(
            build_initial_message(Some("piped".into()), &mut messages).as_deref(),
            Some("pipedfirst")
        );
        assert_eq!(messages, ["second"]);
        assert_eq!(build_initial_message(None, &mut Vec::new()), None);
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

        let created =
            create_session_manager(&session_args(|_| {}), &cwd, None, &home).expect("create");
        assert_eq!(created.session_dir(), dir);

        let file = write_session(&dir, "0193aaaa-0000-7000-8000-000000000000", &cwd);
        let continued = create_session_manager(
            &session_args(|args| args.continue_session = true),
            &cwd,
            None,
            &home,
        )
        .expect("continue");
        assert_eq!(continued.session_file(), Some(file.as_path()));

        let by_prefix = create_session_manager(
            &session_args(|args| args.session = Some("0193aaaa".into())),
            &cwd,
            None,
            &home,
        )
        .expect("by id");
        assert_eq!(by_prefix.session_file(), Some(file.as_path()));

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
            )
            .err(),
            Some(SessionStartError::Error(format!(
                "Session found in different project: {other}. Forking it is not supported by bake-rs yet."
            )))
        );
        // Pi prints a missing id without the `Error: ` prefix.
        assert_eq!(
            create_session_manager(
                &session_args(|args| args.session = Some("ffff".into())),
                &cwd,
                None,
                &home,
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
