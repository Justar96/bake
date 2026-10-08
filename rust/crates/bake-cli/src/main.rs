//! `bake-rs`: entry point for Bake's native terminal preview.

mod inspect;
mod lookup;
mod stat;

use std::ffi::{OsStr, OsString};
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

use bake_tui::PreviewExit;
use inspect::{Encoding, InspectArgs, LookupArgs, MAX_BUDGET, Outcome, Target, parse_count};
use stat::StatArgs;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "\
bake-rs: Bake's native terminal preview (Rust preview)

Usage:
  bake-rs preview      Open the fullscreen preview in an interactive terminal
  bake-rs session inspect --max-bytes <N> --max-source-seqs <N> [--] <file>
                       Describe one Session log as JSON, read-only
  bake-rs session inspect --root <dir> --id <id> --max-bytes <N>
      --max-source-seqs <N> --max-entries <N> [--compression none|zstd]
                       Find one Session in a root and describe it, read-only
  bake-rs session stat --root <dir> --id <id> --max-header-bytes <N>
      --max-entries <N> [--compression none|zstd]
                       Report one Session's header as JSON, read-only
  bake-rs --help       Show this help
  bake-rs --version    Show the version

The preview shows sample content only. It does not connect to a model,
read credentials, write sessions, or start agents. Ctrl+C quits.
";

const INSPECT_HELP: &str = "\
bake-rs session inspect: describe one Session log (Rust preview)

Usage:
  bake-rs session inspect --max-bytes <N> --max-source-seqs <N> [--] <file>
  bake-rs session inspect --root <dir> --id <id> --max-bytes <N>
      --max-source-seqs <N> --max-entries <N> [--compression none|zstd]

Options:
  --max-bytes <N>        Largest file size, and largest decoded Zstd size
  --max-source-seqs <N>  Largest expanded sourceEventSeqs list of one event
  --root <dir>           Session root to search; only with --id
  --id <id>              Session id to find; only with --root
  --max-entries <N>      Most directory entries the lookup reads
  --compression <mode>   none or zstd, the root's encoding (default zstd)
  --                     End the options, as before a path starting with '-'
  -h, --help             Show this help

Budgets are required positive integers no greater than 9007199254740991.
<file> must be named session.v3.jsonl or session.v3.jsonl.zstd; the name
selects plain or Zstd decoding. Other format versions are refused without
opening the file.

With --root and --id, the command finds the Session as the JSONL backend
does: it refuses a root holding the flat legacy layout or the other
compression, a duplicate or missing id, and another format version, then
reads the newest generation, checks its stored identity, and restores it.
Windows roots and directory names outside the supported path subset are
native-limit refusals; see rust/README.md.

The command opens only what it reads, read-only, restores the log as the
Session read path does, and prints one JSON record: counts, the header,
recovery and closer metadata, but no message or tool content. It does not
truncate, repair, migrate, or resume the Session, and does not read the Bake
home, configuration, or credentials. A named file's stored identity is not
checked.

Exit status: 0 restored, 3 refused log (a JSON record on standard output),
1 unreadable file or directory or unsupported name, 2 usage error.
";

const STAT_HELP: &str = "\
bake-rs session stat: report one Session's header (Rust preview)

Usage:
  bake-rs session stat --root <dir> --id <id> --max-header-bytes <N>
      --max-entries <N> [--compression none|zstd]

Options:
  --root <dir>              Session root to search
  --id <id>                 Session id to find
  --max-header-bytes <N>    Most bytes read for the header, plus one byte
                            to detect the end of the file, and largest
                            decoded header record
  --max-entries <N>         Most directory entries the lookup reads
  --compression <mode>      none or zstd, the root's encoding (default zstd)
  -h, --help                Show this help

Budgets are required positive integers no greater than 9007199254740991.

The command finds the Session as 'session inspect --root --id' does, then
decodes only the first line or Zstd frame of the newest generation, in a
supported format, and reports its header translated to the current format.
It never decodes events, never falls back to an older generation, and
does not read the Bake home, configuration, or credentials. Nothing is
written, locked, or migrated. sizeBytes is read after the header, so the
record is an observation, not an atomic snapshot.

A missing Session, a missing file, or an incomplete or malformed header is
absent. A corrupt first Zstd frame, retired header fields, a header version
other than the file name's, a newer format, a stored identity other than
the selected file, and the root and lookup refusals of 'session inspect'
are refused.

Exit status: 0 found or absent, 3 refused (a JSON record on standard
output), 1 unreadable file or directory, 2 usage error.
";

const KNOWN: &[&str] = &["-h", "--help", "help", "-V", "--version", "preview"];

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Version,
    Preview,
    InspectHelp,
    Inspect(InspectArgs),
    StatHelp,
    Stat(StatArgs),
}

/// A usage error, and the help command it points to.
#[derive(Debug, PartialEq, Eq)]
struct Usage {
    message: String,
    help: &'static str,
}

fn parse(args: &[OsString]) -> Result<Command, Usage> {
    // `session` operands are parsed as OS strings: a path need not be UTF-8.
    if args.first().is_some_and(|first| first == "session") {
        let help = if args.get(1).is_some_and(|command| command == "stat") {
            "bake-rs session stat --help"
        } else {
            "bake-rs session inspect --help"
        };
        return parse_session(&args[1..]).map_err(|message| Usage { message, help });
    }
    parse_top(args).map_err(|message| Usage {
        message,
        help: "bake-rs --help",
    })
}

fn parse_top(args: &[OsString]) -> Result<Command, String> {
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

fn parse_session(args: &[OsString]) -> Result<Command, String> {
    let Some((command, rest)) = args.split_first() else {
        return Err("missing a command after 'session'".into());
    };
    let help = |flag: &OsStr, rest: &[OsString]| match rest.first() {
        None => Ok(Command::InspectHelp),
        Some(extra) => Err(format!(
            "unexpected argument {} after '{}'",
            quoted(extra),
            flag.display()
        )),
    };
    if is_help(command) {
        return help(command, rest);
    }
    if command == "stat" {
        return parse_stat(rest);
    }
    if command != "inspect" {
        return Err(format!("unknown command {} for 'session'", quoted(command)));
    }
    if let Some((flag, extra)) = rest.split_first().filter(|(flag, _)| is_help(flag)) {
        return help(flag, extra);
    }
    let mut max_bytes = None;
    let mut max_source_seqs = None;
    let mut max_entries = None;
    let mut root = None;
    let mut id = None;
    let mut compression = None;
    let mut path = None;
    let mut options = true;
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        let slot = match arg.to_str() {
            _ if !options => None,
            Some("--") => {
                options = false;
                continue;
            }
            Some(
                option @ ("--max-bytes" | "--max-source-seqs" | "--max-entries" | "--root" | "--id"
                | "--compression"),
            ) => Some(option),
            Some(flag @ ("-h" | "--help")) => {
                return Err(format!(
                    "option '{flag}' must be the only operand of 'session inspect'"
                ));
            }
            _ if arg.as_encoded_bytes().starts_with(b"-") => {
                return Err(format!(
                    "unknown option {} for 'session inspect'",
                    quoted(arg)
                ));
            }
            _ => None,
        };
        let Some(option) = slot else {
            if path.is_some() {
                return Err(format!(
                    "unexpected argument {} for 'session inspect'",
                    quoted(arg)
                ));
            }
            path = Some(arg.clone());
            continue;
        };
        let value = rest
            .next()
            .ok_or_else(|| format!("option '{option}' needs a value"))?;
        let given = match option {
            "--max-bytes" => set_count(&mut max_bytes, option, value)?,
            "--max-source-seqs" => set_count(&mut max_source_seqs, option, value)?,
            "--max-entries" => set_count(&mut max_entries, option, value)?,
            "--root" => root.replace(parse_root(value)?).is_some(),
            "--id" => id.replace(parse_id(value)?).is_some(),
            _ => compression.replace(parse_compression(value)?).is_some(),
        };
        if given {
            return Err(format!("option '{option}' was given more than once"));
        }
    }
    let max_bytes = max_bytes.ok_or("missing required option '--max-bytes'")?;
    let max_source_seqs = max_source_seqs.ok_or("missing required option '--max-source-seqs'")?;
    let target = match (root, id, path) {
        (Some(_), Some(_), Some(path)) => {
            return Err(format!(
                "unexpected argument {} for 'session inspect' with '--root' and '--id'",
                quoted(&path)
            ));
        }
        (Some(root), Some(id), None) => Target::Lookup(LookupArgs {
            root,
            id,
            encoding: compression.unwrap_or(Encoding::Zstd),
            max_entries: max_entries.ok_or("missing required option '--max-entries'")?,
        }),
        (Some(_), None, _) => return Err("option '--root' needs '--id'".into()),
        (None, Some(_), _) => return Err("option '--id' needs '--root'".into()),
        (None, None, path) => {
            for (option, given) in [
                ("--max-entries", max_entries.is_some()),
                ("--compression", compression.is_some()),
            ] {
                if given {
                    return Err(format!("option '{option}' needs '--root' and '--id'"));
                }
            }
            Target::File(path.ok_or("missing the Session log path")?)
        }
    };
    Ok(Command::Inspect(InspectArgs {
        max_bytes,
        max_source_seqs,
        target,
    }))
}

/// Parse the operands of `session stat`, which takes options only.
fn parse_stat(args: &[OsString]) -> Result<Command, String> {
    if let Some((flag, extra)) = args.split_first().filter(|(flag, _)| is_help(flag)) {
        return match extra.first() {
            None => Ok(Command::StatHelp),
            Some(extra) => Err(format!(
                "unexpected argument {} after '{}'",
                quoted(extra),
                flag.display()
            )),
        };
    }
    let mut max_header_bytes = None;
    let mut max_entries = None;
    let mut root = None;
    let mut id = None;
    let mut compression = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let option = match arg.to_str() {
            Some(
                option @ ("--max-header-bytes" | "--max-entries" | "--root" | "--id"
                | "--compression"),
            ) => option,
            Some(flag @ ("-h" | "--help")) => {
                return Err(format!(
                    "option '{flag}' must be the only operand of 'session stat'"
                ));
            }
            _ if arg.as_encoded_bytes().starts_with(b"-") => {
                return Err(format!("unknown option {} for 'session stat'", quoted(arg)));
            }
            _ => {
                return Err(format!(
                    "unexpected argument {} for 'session stat'",
                    quoted(arg)
                ));
            }
        };
        let value = rest
            .next()
            .ok_or_else(|| format!("option '{option}' needs a value"))?;
        let given = match option {
            "--max-header-bytes" => set_count(&mut max_header_bytes, option, value)?,
            "--max-entries" => set_count(&mut max_entries, option, value)?,
            "--root" => root.replace(parse_root(value)?).is_some(),
            "--id" => id.replace(parse_id(value)?).is_some(),
            _ => compression.replace(parse_compression(value)?).is_some(),
        };
        if given {
            return Err(format!("option '{option}' was given more than once"));
        }
    }
    Ok(Command::Stat(StatArgs {
        lookup: LookupArgs {
            root: root.ok_or("missing required option '--root'")?,
            id: id.ok_or("missing required option '--id'")?,
            encoding: compression.unwrap_or(Encoding::Zstd),
            max_entries: max_entries.ok_or("missing required option '--max-entries'")?,
        },
        max_header_bytes: max_header_bytes.ok_or("missing required option '--max-header-bytes'")?,
    }))
}

/// The TypeScript backend's root is a JavaScript string, so it must be UTF-8.
fn parse_root(value: &OsStr) -> Result<OsString, String> {
    if value.to_str().is_none() {
        return Err(format!(
            "option '--root' needs a UTF-8 path, not {}",
            quoted(value)
        ));
    }
    Ok(value.to_owned())
}

fn parse_id(value: &OsStr) -> Result<String, String> {
    value
        .to_str()
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "option '--id' needs a non-empty UTF-8 Session id, not {}",
                quoted(value)
            )
        })
}

fn parse_compression(value: &OsStr) -> Result<Encoding, String> {
    match value.to_str() {
        Some("none") => Ok(Encoding::None),
        Some("zstd") => Ok(Encoding::Zstd),
        _ => Err(format!(
            "option '--compression' needs none or zstd, not {}",
            quoted(value)
        )),
    }
}

/// Parse one budget into its slot; `Ok(true)` when the slot was already set.
fn set_count(slot: &mut Option<u64>, option: &str, value: &OsStr) -> Result<bool, String> {
    if slot.is_some() {
        return Ok(true);
    }
    let count = value.to_str().and_then(parse_count).ok_or_else(|| {
        format!(
            "option '{option}' needs a positive decimal integer no greater than \
             {MAX_BUDGET}, not {}",
            quoted(value)
        )
    })?;
    *slot = Some(count);
    Ok(false)
}

fn is_help(arg: &OsStr) -> bool {
    arg == "-h" || arg == "--help"
}

/// An argument for a one-line diagnostic: single-quoted when it is UTF-8,
/// with control characters, apostrophes, and backslashes escaped as in Rust
/// source, otherwise in Rust's escaped debug form.
fn quoted(arg: &OsStr) -> String {
    let Some(text) = arg.to_str() else {
        return format!("{arg:?}");
    };
    let mut quoted = String::from("'");
    for char in text.chars() {
        if char.is_control() || char == '\'' || char == '\\' {
            quoted.extend(char.escape_debug());
        } else {
            quoted.push(char);
        }
    }
    quoted.push('\'');
    quoted
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match parse(&args) {
        Ok(Command::Help) => print(HELP),
        Ok(Command::Version) => print(&format!("bake-rs {VERSION}\n")),
        Ok(Command::Preview) => preview(),
        Ok(Command::InspectHelp) => print(INSPECT_HELP),
        Ok(Command::Inspect(args)) => report(inspect::inspect(&args)),
        Ok(Command::StatHelp) => print(STAT_HELP),
        Ok(Command::Stat(args)) => report(match stat::run(&args) {
            Ok((record, status)) => Outcome::Record {
                json: record.to_string(),
                status,
            },
            Err(message) => Outcome::Failure(message),
        }),
        Err(Usage { message, help }) => {
            eprintln!("bake-rs: {message}\n\nRun '{help}' for usage.");
            ExitCode::from(2)
        }
    }
}

/// Print a record and exit with its status, or report a failure.
fn report(outcome: Outcome) -> ExitCode {
    match outcome {
        Outcome::Record { json, status } => {
            if write_stdout(&format!("{json}\n")) {
                ExitCode::from(status)
            } else {
                ExitCode::FAILURE
            }
        }
        Outcome::Failure(message) => {
            eprintln!("bake-rs: {message}");
            ExitCode::FAILURE
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
        parse(&args.iter().map(OsString::from).collect::<Vec<_>>()).map_err(|usage| usage.message)
    }

    fn inspect_args(max_bytes: u64, max_source_seqs: u64, path: &str) -> Command {
        Command::Inspect(InspectArgs {
            max_bytes,
            max_source_seqs,
            target: Target::File(path.into()),
        })
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
            parse_strs(&["--version", "x"]),
            Err("unexpected argument 'x' after '--version'".into())
        );
    }

    #[test]
    fn inspect_takes_both_budgets_in_either_order_and_one_path() {
        let path = "d/session.v3.jsonl";
        let want = Ok(inspect_args(7, 9, path));
        assert_eq!(
            parse_strs(&[
                "session",
                "inspect",
                "--max-bytes",
                "7",
                "--max-source-seqs",
                "9",
                path
            ]),
            want
        );
        assert_eq!(
            parse_strs(&[
                "session",
                "inspect",
                path,
                "--max-source-seqs",
                "9",
                "--max-bytes",
                "7"
            ]),
            want
        );
        assert_eq!(
            parse_strs(&[
                "session",
                "inspect",
                "--max-bytes",
                "7",
                "--max-source-seqs",
                "9",
                "--",
                "-p"
            ]),
            Ok(inspect_args(7, 9, "-p"))
        );
        assert_eq!(
            parse_strs(&[
                "session",
                "inspect",
                "--",
                "--max-bytes",
                "--max-bytes",
                "7",
                "--max-source-seqs",
                "9"
            ]),
            Err("unexpected argument '--max-bytes' for 'session inspect'".into())
        );
        for help in [
            &["session", "--help"][..],
            &["session", "inspect", "-h"],
            &["session", "inspect", "--help"],
        ] {
            assert_eq!(parse_strs(help), Ok(Command::InspectHelp));
        }
    }

    #[test]
    fn repeated_budgets_keep_the_existing_usage_error_order() {
        for option in ["--max-bytes", "--max-source-seqs", "--max-entries"] {
            assert_eq!(
                parse_strs(&["session", "inspect", option, "1", option, "bad"]),
                Err(format!("option '{option}' was given more than once"))
            );
            assert_eq!(
                parse_strs(&["session", "inspect", option, "1", option]),
                Err(format!("option '{option}' needs a value"))
            );
        }
    }

    #[test]
    fn the_lookup_form_takes_a_root_an_id_and_three_budgets() {
        let lookup = |encoding| {
            Ok(Command::Inspect(InspectArgs {
                max_bytes: 7,
                max_source_seqs: 9,
                target: Target::Lookup(LookupArgs {
                    root: "r".into(),
                    id: "a1".into(),
                    encoding,
                    max_entries: 5,
                }),
            }))
        };
        let base = [
            "session",
            "inspect",
            "--id",
            "a1",
            "--max-entries",
            "5",
            "--max-bytes",
            "7",
            "--root",
            "r",
            "--max-source-seqs",
            "9",
        ];
        assert_eq!(parse_strs(&base), lookup(Encoding::Zstd));
        for (mode, encoding) in [("none", Encoding::None), ("zstd", Encoding::Zstd)] {
            let args: Vec<&str> = base
                .iter()
                .copied()
                .chain(["--compression", mode])
                .collect();
            assert_eq!(parse_strs(&args), lookup(encoding));
        }
        let without = |option: &str| -> Vec<&str> {
            let at = base.iter().position(|arg| *arg == option).unwrap();
            let mut args = base.to_vec();
            args.drain(at..at + 2);
            args
        };
        for (args, error) in [
            (without("--root"), "option '--id' needs '--root'"),
            (without("--id"), "option '--root' needs '--id'"),
            (
                without("--max-entries"),
                "missing required option '--max-entries'",
            ),
            (
                without("--max-source-seqs"),
                "missing required option '--max-source-seqs'",
            ),
        ] {
            assert_eq!(parse_strs(&args), Err(error.into()), "{args:?}");
        }
        let with = |extra: &[&'static str]| -> Vec<&str> {
            base.iter().copied().chain(extra.iter().copied()).collect()
        };
        for (args, error) in [
            (
                with(&["d/session.v3.jsonl"]),
                "unexpected argument 'd/session.v3.jsonl' for 'session inspect' with '--root' and '--id'",
            ),
            (
                with(&["--compression", "zst"]),
                "option '--compression' needs none or zstd, not 'zst'",
            ),
            (
                with(&["--root", "s"]),
                "option '--root' was given more than once",
            ),
            (
                with(&["--id", "b"]),
                "option '--id' was given more than once",
            ),
            (
                without("--max-entries")
                    .into_iter()
                    .chain(["--max-entries", "0"])
                    .collect(),
                "option '--max-entries' needs a positive decimal integer no greater than 9007199254740991, not '0'",
            ),
        ] {
            assert_eq!(parse_strs(&args), Err(error.into()), "{args:?}");
        }
        let mut empty_id = base.to_vec();
        empty_id[3] = "";
        assert_eq!(
            parse_strs(&empty_id),
            Err("option '--id' needs a non-empty UTF-8 Session id, not ''".into())
        );
        for (option, value) in [("--max-entries", "5"), ("--compression", "none")] {
            assert_eq!(
                parse_strs(&[
                    "session",
                    "inspect",
                    "--max-bytes",
                    "7",
                    "--max-source-seqs",
                    "9",
                    option,
                    value,
                    "f"
                ]),
                Err(format!("option '{option}' needs '--root' and '--id'"))
            );
        }
    }

    #[test]
    fn stat_takes_a_root_an_id_and_two_budgets() {
        let stat = |encoding| {
            Ok(Command::Stat(StatArgs {
                lookup: LookupArgs {
                    root: "r".into(),
                    id: "a1".into(),
                    encoding,
                    max_entries: 5,
                },
                max_header_bytes: 7,
            }))
        };
        let base = [
            "session",
            "stat",
            "--max-entries",
            "5",
            "--id",
            "a1",
            "--max-header-bytes",
            "7",
            "--root",
            "r",
        ];
        assert_eq!(parse_strs(&base), stat(Encoding::Zstd));
        for (mode, encoding) in [("none", Encoding::None), ("zstd", Encoding::Zstd)] {
            let args: Vec<&str> = base
                .iter()
                .copied()
                .chain(["--compression", mode])
                .collect();
            assert_eq!(parse_strs(&args), stat(encoding));
        }
        for help in [
            &["session", "stat", "-h"][..],
            &["session", "stat", "--help"],
        ] {
            assert_eq!(parse_strs(help), Ok(Command::StatHelp));
        }
        let without = |option: &str| -> Vec<&str> {
            let at = base.iter().position(|arg| *arg == option).unwrap();
            let mut args = base.to_vec();
            args.drain(at..at + 2);
            args
        };
        let with = |extra: &[&'static str]| -> Vec<&str> {
            base.iter().copied().chain(extra.iter().copied()).collect()
        };
        for (args, error) in [
            (without("--root"), "missing required option '--root'"),
            (without("--id"), "missing required option '--id'"),
            (
                without("--max-entries"),
                "missing required option '--max-entries'",
            ),
            (
                without("--max-header-bytes"),
                "missing required option '--max-header-bytes'",
            ),
            (with(&["f"]), "unexpected argument 'f' for 'session stat'"),
            (
                with(&["--max-bytes", "1"]),
                "unknown option '--max-bytes' for 'session stat'",
            ),
            (
                with(&["--id", "b"]),
                "option '--id' was given more than once",
            ),
            (
                with(&["--help"]),
                "option '--help' must be the only operand of 'session stat'",
            ),
            (
                with(&["--compression"]),
                "option '--compression' needs a value",
            ),
            (
                with(&["--compression", "zst"]),
                "option '--compression' needs none or zstd, not 'zst'",
            ),
            (
                without("--max-header-bytes")
                    .into_iter()
                    .chain(["--max-header-bytes", "9007199254740992"])
                    .collect(),
                "option '--max-header-bytes' needs a positive decimal integer no greater than 9007199254740991, not '9007199254740992'",
            ),
            (
                vec!["session", "stat", "--help", "x"],
                "unexpected argument 'x' after '--help'",
            ),
        ] {
            assert_eq!(parse_strs(&args), Err(error.into()), "{args:?}");
        }
        let usage = parse(&[OsString::from("session"), OsString::from("stat")]).unwrap_err();
        assert_eq!(usage.help, "bake-rs session stat --help");
    }

    #[test]
    fn budgets_are_positive_safe_decimal_integers() {
        assert_eq!(parse_count("1"), Some(1));
        assert_eq!(parse_count("9007199254740991"), Some(MAX_BUDGET));
        for bad in [
            "",
            "0",
            "00",
            "01",
            "+1",
            "-1",
            " 1",
            "1 ",
            "1e3",
            "1.0",
            "0x1",
            "1_0",
            "9007199254740992",
            "99999999999999999999",
        ] {
            assert_eq!(parse_count(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn log_names_follow_the_typescript_generation_rule() {
        use inspect::{Encoding, LogName, parse_log_name};
        let canonical = |version, encoding| LogName::Canonical { version, encoding };
        assert_eq!(
            parse_log_name("session.jsonl"),
            canonical(0, Encoding::None)
        );
        assert_eq!(
            parse_log_name("session.v3.jsonl"),
            canonical(3, Encoding::None)
        );
        assert_eq!(
            parse_log_name("session.v3.jsonl.zstd"),
            canonical(3, Encoding::Zstd)
        );
        assert_eq!(
            parse_log_name("session.v12.jsonl.zstd"),
            canonical(12, Encoding::Zstd)
        );
        for name in [
            "session.v0.jsonl",
            "session.v03.jsonl",
            "Session.v3.jsonl",
            "session.v3.jsonl.zst",
            "session.v3.jsonl.tmp",
            "session.jsonl.zstd.zstd",
            "session.v.jsonl",
            "session.v9007199254740992.jsonl",
            "sessions.v3.jsonl",
            "",
        ] {
            assert_eq!(parse_log_name(name), LogName::NotCanonical, "{name:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn inspect_paths_and_values_stay_os_strings() {
        use std::os::unix::ffi::OsStringExt;
        let path = OsString::from_vec(b"\xff/session.v3.jsonl".to_vec());
        let args = |value: OsString, path: OsString| {
            [
                "session",
                "inspect",
                "--max-source-seqs",
                "1",
                "--max-bytes",
            ]
            .into_iter()
            .map(OsString::from)
            .chain([value, path])
            .collect::<Vec<_>>()
        };
        assert_eq!(
            parse(&args("2".into(), path.clone())),
            Ok(Command::Inspect(InspectArgs {
                max_bytes: 2,
                max_source_seqs: 1,
                target: Target::File(path.clone())
            }))
        );
        let root = [
            "session",
            "inspect",
            "--id",
            "a1",
            "--max-bytes",
            "1",
            "--max-source-seqs",
            "1",
            "--max-entries",
            "1",
            "--root",
        ]
        .into_iter()
        .map(OsString::from)
        .chain([OsString::from_vec(b"r\xff".to_vec())])
        .collect::<Vec<_>>();
        assert_eq!(
            parse(&root).map_err(|usage| usage.message),
            Err("option '--root' needs a UTF-8 path, not \"r\\xFF\"".into())
        );
        assert_eq!(
            parse(&args(OsString::from_vec(vec![0xff]), path)).map_err(|usage| usage.message),
            Err("option '--max-bytes' needs a positive decimal integer no greater than 9007199254740991, not \"\\xFF\"".into())
        );
    }
}
