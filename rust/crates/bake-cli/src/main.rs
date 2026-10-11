//! `bake-rs`: Bake's native agent (Rust preview).
//!
//! `bake-rs -p "prompt"` runs one headless turn through
//! [`bake_coding_agent::cli`], a port of Pi's print mode; `bake-rs preview`
//! opens the sample terminal preview. Help and version need no Bake home.

use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use bake_coding_agent::cli::{self, Action, CliEnvironment, Interrupt};
use bake_coding_agent::print_mode::SharedWriter;
use bake_coding_agent::system_prompt::DocsPaths;
use bake_tui::PreviewExit;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "\
bake-rs: Bake's native agent (Rust preview)

Usage:
  bake-rs [options] [messages...]
  bake-rs preview      Open the fullscreen sample preview in an interactive terminal

Options:
  -p, --print               Send the prompts, print the final answer, and exit
      --mode <mode>         Output: text (the final answer) or json (every event)
      --provider <name>     Provider of --model
      --model <pattern>     Model id, pattern, or provider/id, optionally :<thinking>
      --thinking <level>    off, minimal, low, medium, high, xhigh, or max
  -c, --continue            Continue the most recent session in this directory
      --session <path|id>   Use a session file, or a session id or id prefix
      --no-session          Do not save the session
  -h, --help                Show this help
  -v, --version             Show the version

Messages are prompts, sent in order; piped standard input is prepended to the
first. The agent runs without tools. Interactive mode is not available yet:
bake-rs prints when -p or --mode is given or a standard stream is not a
terminal.

Models come from models.json and keys from auth.json in the Bake home
(BAKE_HOME, then DSH_HOME, then ~/.bake), which also holds settings.json, a
global AGENTS.md, and the saved sessions.

The preview shows sample content only and does not connect to a model.
Ctrl+C quits it.
";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Preview,
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
        ["preview"] => Ok(Command::Preview),
        ["preview", extra, ..] => Err(format!("unexpected argument '{extra}' for 'preview'")),
        _ => Ok(Command::Agent(args)),
    }
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match parse(&args) {
        Ok(Command::Help) => print(HELP),
        Ok(Command::Preview) => preview(),
        Ok(Command::Agent(args)) => agent(&args),
        Err(message) => {
            eprintln!("bake-rs: {message}\n\nRun 'bake-rs --help' for usage.");
            ExitCode::from(2)
        }
    }
}

fn exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// Pi's documentation paths, beside the executable.
fn docs_paths() -> DocsPaths {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
        .unwrap_or_default();
    DocsPaths::under(&dir)
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
            docs: docs_paths(),
            stdin_is_terminal,
            stdin: Box::new(io::stdin()),
            stdout,
            stderr,
            extra_providers: imported.providers,
            imported_default_model: imported.default_model,
            warnings: imported.warnings,
            interrupt: interrupt(),
        };
        cli::run(environment, *parsed, mode).await
    });
    // A blocked standard-input read cannot be cancelled; do not wait for it.
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    exit_code(code)
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

fn preview() -> ExitCode {
    // Checked before any terminal mode changes.
    if let Some(name) = non_terminal_stream(io::stdin().is_terminal(), io::stdout().is_terminal()) {
        eprintln!(
            "bake-rs: preview needs an interactive terminal, but standard {name} is not a terminal"
        );
        return ExitCode::FAILURE;
    }
    match bake_tui::run_preview() {
        Ok(PreviewExit::Quit) => ExitCode::SUCCESS,
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
        assert_eq!(parse_strs(&["preview"]), Ok(Command::Preview));
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
    fn unexpected_preview_arguments_name_the_offender() {
        assert_eq!(
            parse_strs(&["preview", "--fullscreen"]),
            Err("unexpected argument '--fullscreen' for 'preview'".into())
        );
    }
}
