//! `bake-rs`: Bake's native agent (Rust preview).
//!
//! `bake-rs -p "prompt"` runs one headless turn through
//! [`bake_coding_agent::cli`], a port of Pi's print mode; `bake-rs preview`
//! opens the sample terminal preview, and `bake-rs docs` prints the user
//! documentation ([`docs`]). Help, version, and docs need no Bake home.

mod docs;

use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use bake_coding_agent::cli::{self, Action, CliEnvironment, Interrupt};
use bake_coding_agent::print_mode::SharedWriter;
use bake_sandbox::SandboxProvider;
use bake_tui::{PreviewExit, ScreenMode};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "\
bake-rs: Bake's native agent (Rust preview)

Usage:
  bake-rs [options] [messages...]
  bake-rs preview [--inline]  Open the sample preview in an interactive terminal
  bake-rs docs [topic]        List the documentation topics, or print one

Options:
  -p, --print               Send the prompts, print the final answer, and exit
      --mode <mode>         Output: text (the final answer) or json (every event)
      --provider <name>     Provider of --model
      --model <pattern>     Model id, pattern, or provider/id, optionally :<thinking>
      --models <patterns>   Scope available models (comma-separated patterns or globs)
      --api-key <key>       Runtime API key for the selected model's provider
      --thinking <level>    off, minimal, low, medium, high, xhigh, or max
      --system-prompt <text|path>
                            Replace the default system prompt preamble and rules
      --append-system-prompt <text|path>
                            Append to the system prompt (repeatable)
  -c, --continue            Continue the most recent session in this directory
      --session <path|id>   Use a session file, or a session id or id prefix
      --session-id <id>     Use a specific session id, opening or creating it
      --fork <path|id>      Fork a session into a new session in this directory
      --session-dir <dir>   Directory for session storage and lookup
  -n, --name <name>         Set the session display name
      --no-session          Do not save the session
  -t, --tools <list>        Allow these tools: names, patterns with *, or +name
                            and -name changes to the defaults, comma-separated
  -xt, --exclude-tools <list>
                            Disable these tools: names or patterns with *
  -nt, --no-tools           Start with no tools
  -nbt, --no-builtin-tools  Start without the built-in tools
      --skill <path>        Load a skill file or directory (repeatable)
  -ns, --no-skills          Do not discover skills in .agents/skills
      --prompt-template <path>
                            Load a prompt template file or directory (repeatable)
  -np, --no-prompt-templates
                            Do not discover prompt templates
  -nc, --no-context-files   Do not load AGENTS.md context files
      --list-models [search]
                            List available models (with optional fuzzy search)
      --permission <preset> Permission preset: workspace-write or
                            danger-full-access, or one from settings.json;
                            new sessions default to the saved preset, else
                            workspace-write
      --sandbox <mode>      read-only, workspace-write, or danger-full-access
      --approval <policy>   ask or never
  -h, --help                Show this help
  -v, --version             Show the version

Messages are prompts, sent in order; piped standard input and @file arguments
are prepended to the first. The agent starts with the read, bash, edit, and
write tools; grep, find, and ls are available through --tools, and grep and
find need rg and fd on PATH. A session keeps its permission: write and edit
change files only where its sandbox mode allows, and bash runs only under a
sandbox that enforces it: bubblewrap or Landlock on Linux and Seatbelt on
macOS. Without one (Windows, for now), confined bash calls are refused.
Escalations need approval, which print mode cannot give.
Interactive mode is not available yet: bake-rs prints when -p or --mode is
given or a standard stream is not a terminal.

Models come from models.json and keys from auth.json in the Bake home
(BAKE_HOME, then DSH_HOME, then ~/.bake), which also holds settings.json and
the saved sessions. Instructions come from ~/.agents/AGENTS.md and the
AGENTS.md files from the filesystem root down to the working directory.
Skills come from ~/.agents/skills and every .agents/skills from the working
directory up to its git repository root.

The preview shows sample content only and does not connect to a model.
Ctrl+C quits it.

Run 'bake-rs docs' for the documentation: the Bake home, settings, models,
permissions, sessions, context files, skills, and the command line.
";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Preview(ScreenMode),
    /// `bake-rs docs`, with a topic or without one.
    Docs(Option<String>),
    Agent(Vec<String>),
}

fn parse(args: &[OsString]) -> Result<Command, String> {
    let args = args
        .iter()
        .map(|a| {
            a.to_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("argument {a:?} is not valid UTF-8"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let strs: Vec<&str> = args.iter().map(String::as_str).collect();
    match strs.as_slice() {
        [] | ["help"] | ["preview", "-h" | "--help"] => Ok(Command::Help),
        ["preview"] => Ok(Command::Preview(ScreenMode::Fullscreen)),
        ["preview", "--inline"] => Ok(Command::Preview(ScreenMode::Inline)),
        ["preview", "--inline", "-h" | "--help"] => Ok(Command::Help),
        ["preview", "--inline", extra, ..] => {
            Err(format!("unexpected argument '{extra}' for 'preview'"))
        }
        ["preview", extra, ..] => Err(format!("unexpected argument '{extra}' for 'preview'")),
        ["docs"] => Ok(Command::Docs(None)),
        ["docs", topic] => Ok(Command::Docs(Some((*topic).to_owned()))),
        ["docs", _, extra, ..] => Err(format!("unexpected argument '{extra}' for 'docs'")),
        _ => Ok(Command::Agent(args)),
    }
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    // The sandbox's Landlock helper re-executes this binary; it must run
    // before anything starts a thread, and it never returns.
    if args
        .first()
        .is_some_and(|arg| arg == bake_sandbox::backends::landlock::HELPER_ARG)
    {
        bake_sandbox::backends::landlock::helper_main(args.into_iter().skip(1));
    }
    match parse(&args) {
        Ok(Command::Help) => print(HELP),
        Ok(Command::Preview(mode)) => preview(mode),
        Ok(Command::Docs(topic)) => show_docs(topic.as_deref()),
        Ok(Command::Agent(args)) => agent(&args),
        Err(message) => {
            // A closed standard error is not a panic.
            let _ = io::stderr().lock().write_all(
                format!("bake-rs: {message}\n\nRun 'bake-rs --help' for usage.\n").as_bytes(),
            );
            ExitCode::from(2)
        }
    }
}

fn exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// How to run `bake-rs docs` from this executable, for the prompt's
/// `docs` section and the messages that point at the documentation.
fn docs_command() -> String {
    match std::env::current_exe() {
        Ok(exe) => docs::docs_command(&exe),
        Err(_) => "bake-rs docs".to_owned(),
    }
}

/// `bake-rs docs [topic]`: the topic's Markdown, or the topic list. An
/// unknown topic fails with status 2 and the list on standard error.
fn show_docs(topic: Option<&str>) -> ExitCode {
    match topic {
        None | Some("-h" | "--help") => print(&docs::topic_list()),
        Some(name) => match docs::topic(name) {
            Some(topic) => print(topic.text),
            None => {
                // A closed standard error is not a panic either.
                let _ = io::stderr().lock().write_all(
                    format!(
                        "bake-rs: unknown docs topic '{name}'\n\n{}",
                        docs::topic_list()
                    )
                    .as_bytes(),
                );
                ExitCode::from(2)
            }
        },
    }
}

/// `SIGTERM` and `SIGHUP` stop print mode with Pi's exit codes, 143 and 129.
#[cfg(unix)]
fn interrupt() -> Interrupt {
    use tokio::signal::unix::{SignalKind, signal};
    Box::pin(async {
        let (Ok(mut term), Ok(mut hangup)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::hangup()),
        ) else {
            return std::future::pending().await;
        };
        tokio::select! {
            _ = term.recv() => 143,
            _ = hangup.recv() => 129,
        }
    })
}

/// Pi registers only `SIGTERM` on Windows, which has no such signal.
#[cfg(not(unix))]
fn interrupt() -> Interrupt {
    cli::no_interrupt()
}

fn agent(args: &[String]) -> ExitCode {
    let stdout: SharedWriter = Arc::new(Mutex::new(io::stdout()));
    let stderr: SharedWriter = Arc::new(Mutex::new(io::stderr()));
    let stdin_is_terminal = io::stdin().is_terminal();
    let action = cli::plan(args, stdin_is_terminal, io::stdout().is_terminal(), &stderr);
    let (parsed, mode) = match action {
        Action::Help => return print(HELP),
        Action::Version => return print(&format!("bake-rs {VERSION}\n")),
        Action::Exit(code) => return exit_code(code),
        Action::Print(parsed, mode) => (parsed, mode),
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Error: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let home = bake_coding_agent::home::bake_home();
    let imported = match &home {
        Some(home) => cli::import_startup(home, &|name| std::env::var_os(name)),
        None => cli::ImportedStartup::default(),
    };
    let code = runtime.block_on(async move {
        // Signal streams register with the runtime, so they start inside it.
        let environment = CliEnvironment {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            home,
            user_home: std::env::home_dir(),
            docs_command: docs_command(),
            stdin_is_terminal,
            stdin: Box::new(io::stdin()),
            stdout,
            stderr,
            extra_providers: imported.providers,
            imported_default_model: imported.default_model,
            imported_default_preset: imported.default_preset,
            sandbox_provider: sandbox_provider(),
            warnings: imported.warnings,
            interrupt: interrupt(),
        };
        cli::run(environment, *parsed, mode).await
    });
    // A blocked standard-input read cannot be cancelled; do not wait for it.
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    exit_code(code)
}

/// The process's one sandbox provider: bubblewrap or Landlock on Linux,
/// Seatbelt on macOS, and a fail-closed refusal elsewhere. It is built on
/// first use and shared by every consumer, so a backend probes and caches
/// its runner once.
fn sandbox_provider() -> Arc<dyn SandboxProvider> {
    bake_sandbox::default_provider()
}

/// Writes to stdout without panicking when the reader has gone away.
fn print(text: &str) -> ExitCode {
    if write_stdout(text) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Whether the whole text reached stdout; a closed reader is not a panic.
fn write_stdout(text: &str) -> bool {
    let mut stdout = io::stdout().lock();
    stdout.write_all(text.as_bytes()).is_ok() && stdout.flush().is_ok()
}

/// Names the first standard stream that is not a terminal, input before output.
fn non_terminal_stream(stdin_tty: bool, stdout_tty: bool) -> Option<&'static str> {
    if !stdin_tty {
        Some("input")
    } else if !stdout_tty {
        Some("output")
    } else {
        None
    }
}

fn preview(mode: ScreenMode) -> ExitCode {
    // Checked before any terminal mode changes.
    if let Some(name) = non_terminal_stream(io::stdin().is_terminal(), io::stdout().is_terminal()) {
        eprintln!(
            "bake-rs: preview needs an interactive terminal, but standard {name} is not a terminal"
        );
        return ExitCode::FAILURE;
    }
    match bake_tui::run_preview_mode(mode) {
        Ok(PreviewExit::Quit) => ExitCode::SUCCESS,
        // The terminal is gone, so there is nowhere to report anything; 129
        // is the SIGHUP status the TypeScript launcher reports too.
        Ok(PreviewExit::Hangup) => ExitCode::from(129),
        Ok(PreviewExit::Signal(signal)) => {
            ExitCode::from(u8::try_from(128 + signal).unwrap_or(u8::MAX))
        }
        Err(err) => {
            eprintln!("bake-rs: preview failed: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Command, String> {
        parse(&args.iter().map(OsString::from).collect::<Vec<_>>())
    }

    #[test]
    fn no_arguments_print_help_instead_of_opening_the_preview() {
        assert_eq!(parse_strs(&[]), Ok(Command::Help));
        assert_eq!(parse_strs(&["preview", "--help"]), Ok(Command::Help));
        assert_eq!(
            parse_strs(&["preview"]),
            Ok(Command::Preview(ScreenMode::Fullscreen))
        );
    }

    #[test]
    fn other_arguments_go_to_the_agent_command_line() {
        assert_eq!(
            parse_strs(&["-p", "hi"]),
            Ok(Command::Agent(vec!["-p".into(), "hi".into()]))
        );
        assert_eq!(
            parse_strs(&["--version"]),
            Ok(Command::Agent(vec!["--version".into()]))
        );
    }

    #[test]
    fn preview_names_the_first_stream_that_is_not_a_terminal() {
        assert_eq!(non_terminal_stream(true, true), None);
        assert_eq!(non_terminal_stream(true, false), Some("output"));
        assert_eq!(non_terminal_stream(false, true), Some("input"));
        assert_eq!(non_terminal_stream(false, false), Some("input"));
    }

    #[test]
    fn the_sandbox_provider_is_built_once() {
        assert!(Arc::ptr_eq(&sandbox_provider(), &sandbox_provider()));
    }

    #[test]
    fn unexpected_preview_arguments_name_the_offender() {
        assert_eq!(
            parse_strs(&["preview", "--fullscreen"]),
            Err("unexpected argument '--fullscreen' for 'preview'".into())
        );
    }
}
