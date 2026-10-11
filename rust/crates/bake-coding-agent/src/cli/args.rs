//! Command-line arguments, ported from Pi
//! `packages/coding-agent/src/cli/args.ts` (v1.1.0) for the flags print
//! mode needs. Pi's other flags are not recognized: a long one becomes an
//! unknown flag (Pi's extension flags), which the caller reports as Pi does
//! when no extension registers it, and a short one is an error.

use bake_ai::ModelThinkingLevel;

use crate::model_registry::resolver::parse_thinking_level;

/// Pi's `--mode` values that `bake-rs` runs; `rpc` is not ported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `text`
    Text,
    /// `json`
    Json,
}

/// Pi's diagnostic kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    /// Stops the program.
    Error,
    /// Reported and ignored.
    Warning,
}

/// A parse problem, Pi's `{ type, message }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Error or warning.
    pub kind: DiagnosticKind,
    /// The message.
    pub message: String,
}

impl Diagnostic {
    fn error(message: impl Into<String>) -> Self {
        Self {
            kind: DiagnosticKind::Error,
            message: message.into(),
        }
    }
}

/// Pi's `Args`, for the ported flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Args {
    /// `--help`, `-h`.
    pub help: bool,
    /// `--version`, `-v` (and `-V`, which `bake-rs` kept).
    pub version: bool,
    /// `--print`, `-p`.
    pub print: bool,
    /// `--mode`.
    pub mode: Option<Mode>,
    /// `--continue`, `-c`.
    pub continue_session: bool,
    /// `--provider`.
    pub provider: Option<String>,
    /// `--model`.
    pub model: Option<String>,
    /// `--thinking`.
    pub thinking: Option<ModelThinkingLevel>,
    /// `--no-session`.
    pub no_session: bool,
    /// `--session`.
    pub session: Option<String>,
    /// Positional prompts.
    pub messages: Vec<String>,
    /// `@file` arguments.
    pub file_args: Vec<String>,
    /// Unrecognized long flags, in first-seen order, each once with its
    /// last value, as Pi's `Map` holds them.
    pub unknown_flags: Vec<(String, Option<String>)>,
    /// Parse problems.
    pub diagnostics: Vec<Diagnostic>,
}

const VALID_THINKING_LEVELS: &str = "off, minimal, low, medium, high, xhigh, max";

/// Pi's `parseArgs`.
pub fn parse_args(args: &[String]) -> Args {
    let mut result = Args::default();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let next = args.get(i + 1).map(String::as_str);
        match arg {
            "--" => {
                for positional in &args[i + 1..] {
                    match positional.strip_prefix('@') {
                        Some(file) => result.file_args.push(file.to_owned()),
                        None => result.messages.push(positional.clone()),
                    }
                }
                break;
            }
            "--help" | "-h" => result.help = true,
            "--version" | "-v" | "-V" => result.version = true,
            "--mode" => match next {
                None => result
                    .diagnostics
                    .push(Diagnostic::error("--mode requires text or json")),
                Some(mode) if mode.starts_with('-') => result
                    .diagnostics
                    .push(Diagnostic::error("--mode requires text or json")),
                Some(mode) => {
                    i += 1;
                    match mode {
                        "text" => result.mode = Some(Mode::Text),
                        "json" => result.mode = Some(Mode::Json),
                        other => result.diagnostics.push(Diagnostic::error(format!(
                            "Invalid mode \"{other}\". Valid values: text, json"
                        ))),
                    }
                }
            },
            "--continue" | "-c" => result.continue_session = true,
            "--provider" if next.is_some() => {
                result.provider = next.map(str::to_owned);
                i += 1;
            }
            "--model" if next.is_some() => {
                result.model = next.map(str::to_owned);
                i += 1;
            }
            "--no-session" => result.no_session = true,
            "--session" if next.is_some() => {
                result.session = next.map(str::to_owned);
                i += 1;
            }
            "--thinking" if next.is_some() => {
                let level = next.unwrap_or_default();
                i += 1;
                match parse_thinking_level(level) {
                    Some(level) => result.thinking = Some(level),
                    None => result.diagnostics.push(Diagnostic {
                        kind: DiagnosticKind::Warning,
                        message: format!(
                            "Invalid thinking level \"{level}\". Valid values: {VALID_THINKING_LEVELS}"
                        ),
                    }),
                }
            }
            "--print" | "-p" => {
                result.print = true;
                if let Some(next) = next
                    && !next.starts_with('@')
                    && (!next.starts_with('-') || next.starts_with("---"))
                {
                    result.messages.push(next.to_owned());
                    i += 1;
                }
            }
            _ if arg.starts_with('@') => result.file_args.push(arg[1..].to_owned()),
            _ if arg.starts_with("--") => {
                let flag = &arg[2..];
                let (name, value) = match flag.split_once('=') {
                    Some((name, value)) => (name, Some(value.to_owned())),
                    None => match next {
                        Some(value) if !value.starts_with('-') && !value.starts_with('@') => {
                            i += 1;
                            (flag, Some(value.to_owned()))
                        }
                        _ => (flag, None),
                    },
                };
                // Pi's `Map.set`: a repeated flag keeps its place and takes
                // the later value.
                match result
                    .unknown_flags
                    .iter_mut()
                    .find(|(known, _)| known == name)
                {
                    Some(entry) => entry.1 = value,
                    None => result.unknown_flags.push((name.to_owned(), value)),
                }
            }
            _ if arg.starts_with('-') => result
                .diagnostics
                .push(Diagnostic::error(format!("Unknown option: {arg}"))),
            _ => result.messages.push(arg.to_owned()),
        }
        i += 1;
    }
    result
}

/// Pi's unknown-flag diagnostic from `agent-session-services.ts`.
pub fn unknown_flags_message(flags: &[(String, Option<String>)]) -> Option<String> {
    if flags.is_empty() {
        return None;
    }
    let names: Vec<String> = flags.iter().map(|(name, _)| format!("--{name}")).collect();
    Some(format!(
        "Unknown option{}: {}",
        if names.len() == 1 { "" } else { "s" },
        names.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    //! Cases from Pi `test/args.test.ts` (v1.1.0) for the ported flags.

    use super::*;

    fn parse(args: &[&str]) -> Args {
        parse_args(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn print_takes_the_next_argument_as_a_prompt() {
        let args = parse(&["-p", "hello", "world"]);
        assert!(args.print);
        assert_eq!(args.messages, ["hello", "world"]);
        assert_eq!(
            parse(&["--print", "--model", "x"]).messages,
            Vec::<String>::new()
        );
        assert_eq!(parse(&["-p", "---x"]).messages, ["---x"]);
        assert_eq!(parse(&["-p", "@f.txt"]).file_args, ["f.txt"]);
    }

    #[test]
    fn modes_and_their_errors() {
        assert_eq!(parse(&["--mode", "json"]).mode, Some(Mode::Json));
        assert_eq!(parse(&["--mode", "text"]).mode, Some(Mode::Text));
        assert_eq!(
            parse(&["--mode"]).diagnostics,
            [Diagnostic::error("--mode requires text or json")]
        );
        assert_eq!(
            parse(&["--mode", "rpc"]).diagnostics,
            [Diagnostic::error(
                "Invalid mode \"rpc\". Valid values: text, json"
            )]
        );
    }

    #[test]
    fn session_and_model_flags() {
        let args = parse(&[
            "--provider",
            "openai",
            "--model",
            "gpt-4o:high",
            "--thinking",
            "low",
            "-c",
            "--session",
            "abc",
            "--no-session",
        ]);
        assert_eq!(args.provider.as_deref(), Some("openai"));
        assert_eq!(args.model.as_deref(), Some("gpt-4o:high"));
        assert_eq!(args.thinking, Some(ModelThinkingLevel::Low));
        assert!(args.continue_session && args.no_session);
        assert_eq!(args.session.as_deref(), Some("abc"));
        assert!(args.diagnostics.is_empty());
        // Pi: a flag missing its value falls through to the unknown flags.
        assert_eq!(parse(&["--model"]).unknown_flags, [("model".into(), None)]);
    }

    #[test]
    fn invalid_thinking_levels_warn() {
        let args = parse(&["--thinking", "huge"]);
        assert_eq!(args.thinking, None);
        assert_eq!(
            args.diagnostics,
            [Diagnostic {
                kind: DiagnosticKind::Warning,
                message: "Invalid thinking level \"huge\". Valid values: off, minimal, low, medium, high, xhigh, max".into(),
            }]
        );
    }

    #[test]
    fn unknown_options() {
        let args = parse(&[
            "-x",
            "--api-key",
            "k",
            "--flag=v",
            "--bare",
            "--",
            "-p",
            "@f",
        ]);
        assert_eq!(args.diagnostics, [Diagnostic::error("Unknown option: -x")]);
        assert_eq!(
            args.unknown_flags,
            [
                ("api-key".into(), Some("k".into())),
                ("flag".into(), Some("v".into())),
                ("bare".into(), None),
            ]
        );
        assert_eq!(args.messages, ["-p"]);
        assert_eq!(args.file_args, ["f"]);
        assert_eq!(
            unknown_flags_message(&args.unknown_flags).as_deref(),
            Some("Unknown options: --api-key, --flag, --bare")
        );
        assert_eq!(
            unknown_flags_message(&[("x".into(), None)]).as_deref(),
            Some("Unknown option: --x")
        );
        // Pi keeps unknown flags in a `Map`: a repeat keeps its place and
        // takes the later value.
        let args = parse(&["--flag", "a", "--other", "--flag=b"]);
        assert_eq!(
            args.unknown_flags,
            [("flag".into(), Some("b".into())), ("other".into(), None),]
        );
        assert_eq!(
            unknown_flags_message(&args.unknown_flags).as_deref(),
            Some("Unknown options: --flag, --other")
        );
    }
}
