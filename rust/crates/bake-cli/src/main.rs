//! `bake-rs`: entry point for Bake's native terminal preview.

use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

use bake_tui::PreviewExit;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "\
bake-rs: Bake's native terminal preview (Rust preview)

Usage:
  bake-rs preview      Open the fullscreen preview in an interactive terminal
  bake-rs --help       Show this help
  bake-rs --version    Show the version

The preview shows sample content only. It does not connect to a model,
read credentials, write sessions, or start agents. Ctrl+C quits.
";

const KNOWN: &[&str] = &["-h", "--help", "help", "-V", "--version", "preview"];

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Version,
    Preview,
}

fn parse(args: &[OsString]) -> Result<Command, String> {
    let args = args
        .iter()
        .map(|a| {
            a.to_str()
                .ok_or_else(|| format!("argument {a:?} is not valid UTF-8"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    match args.as_slice() {
        [] | ["-h" | "--help" | "help"] | ["preview", "-h" | "--help"] => Ok(Command::Help),
        ["-V" | "--version"] => Ok(Command::Version),
        ["preview"] => Ok(Command::Preview),
        ["preview", extra, ..] => Err(format!("unexpected argument '{extra}' for 'preview'")),
        [first, ..] if !KNOWN.contains(first) => Err(if first.starts_with('-') {
            format!("unknown option '{first}'")
        } else {
            format!("unknown command '{first}'")
        }),
        [first, extra, ..] => Err(format!("unexpected argument '{extra}' after '{first}'")),
        [first] => Err(format!("unexpected argument '{first}'")),
    }
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match parse(&args) {
        Ok(Command::Help) => print(HELP),
        Ok(Command::Version) => print(&format!("bake-rs {VERSION}\n")),
        Ok(Command::Preview) => preview(),
        Err(message) => {
            eprintln!("bake-rs: {message}\n\nRun 'bake-rs --help' for usage.");
            ExitCode::from(2)
        }
    }
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
    fn preview_names_the_first_stream_that_is_not_a_terminal() {
        assert_eq!(non_terminal_stream(true, true), None);
        assert_eq!(non_terminal_stream(true, false), Some("output"));
        assert_eq!(non_terminal_stream(false, true), Some("input"));
        assert_eq!(non_terminal_stream(false, false), Some("input"));
    }

    #[test]
    fn unexpected_arguments_name_the_offender() {
        assert_eq!(
            parse_strs(&["preview", "--fullscreen"]),
            Err("unexpected argument '--fullscreen' for 'preview'".into())
        );
        assert_eq!(
            parse_strs(&["--bogus"]),
            Err("unknown option '--bogus'".into())
        );
        assert_eq!(parse_strs(&["run"]), Err("unknown command 'run'".into()));
        assert_eq!(
            parse_strs(&["session", "inspect"]),
            Err("unknown command 'session'".into())
        );
        assert_eq!(
            parse_strs(&["--version", "x"]),
            Err("unexpected argument 'x' after '--version'".into())
        );
    }
}
