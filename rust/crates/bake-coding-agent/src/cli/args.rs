//! Command-line arguments, ported from Pi
//! `packages/coding-agent/src/cli/args.ts` (v1.1.0) for the flags print
//! mode needs. Pi's other flags are not recognized: a long one becomes an
//! unknown flag (Pi's extension flags), which the caller reports as Pi does
//! when no extension registers it, and a short one is an error.
//!
//! `--skill <path>` and `--no-skills` (`-ns`) select skills
//! ([`crate::skills`]), `--no-mcp` (Pi v1.3.0) starts no MCP server
//! ([`crate::mcp::home`]), and `--export` writes a session file as HTML
//! ([`crate::cli::export`]).
//!
//! `--approve` (`-a`) and `--no-approve` (`-na`) parse to
//! [`Args::project_trust_override`] and are then refused: Pi's project
//! trust, which decides whether a project's own files are loaded, is not
//! ported ([`crate::settings`]), so there is no decision for them to
//! override. They are parsed rather than left unknown so the refusal says
//! that, and so `-a` is not read as an unknown short option. Bake's
//! `--approval` is a different thing: the session's approval policy (D24,
//! [`crate::permissions`]).
//!
//! `--verbose` is accepted and changes nothing. Pi's flag forces verbose
//! startup over its `quietStartup` setting; `bake-rs` has no such setting
//! and no quiet startup, so its startup already reports what Pi's verbose
//! startup reports: the session file, the model fallback, every setup
//! warning, and each MCP server's failure.
//!
//! Bake adds `--permission <preset>`, `--sandbox <mode>`, and
//! `--approval <policy>`, which select the session's permission (D24) the
//! way Pi's `--thinking` selects its thinking level, and `--inline`, which
//! keeps interactive mode in the terminal's scrollback.

use bake_ai::ModelThinkingLevel;

use bake_sandbox::SandboxMode;

use crate::model_registry::resolver::parse_thinking_level;
use crate::permissions::ApprovalPolicy;

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
    /// `--max-model-calls`: one shared print-mode request allowance.
    pub max_model_calls: Option<u64>,
    /// `--run-timeout-ms`: print-mode deadline starting before the first prompt.
    pub run_timeout_ms: Option<u64>,
    /// `--recoverable`: journal one print prompt for explicit recovery.
    pub recoverable: bool,
    /// `--resume-run`: continue only this recorded run in `--session`.
    pub resume_run: Option<String>,
    /// `--mode`.
    pub mode: Option<Mode>,
    /// `--continue`, `-c`.
    pub continue_session: bool,
    /// `--resume`, `-r`: pick the session to open. Interactive mode only.
    pub resume: bool,
    /// `--offline`: run no startup network operation. The environment can
    /// set it too ([`crate::cli::offline_mode`]).
    pub offline: bool,
    /// `--verbose`: force verbose startup. `bake-rs` has no quiet startup
    /// to override, so nothing changes; see the module documentation.
    pub verbose: bool,
    /// Pi's `projectTrustOverride`: `--approve` (`-a`) is `Some(true)` and
    /// `--no-approve` (`-na`) is `Some(false)`. `bake-rs` has no project
    /// trust, so [`crate::cli::plan`] refuses both; see the module
    /// documentation.
    pub project_trust_override: Option<bool>,
    /// `--provider`.
    pub provider: Option<String>,
    /// `--model`.
    pub model: Option<String>,
    /// `--api-key`.
    pub api_key: Option<String>,
    /// `--system-prompt`.
    pub system_prompt: Option<String>,
    /// `--append-system-prompt`, in order.
    pub append_system_prompt: Vec<String>,
    /// `--name`, `-n`.
    pub name: Option<String>,
    /// `--thinking`.
    pub thinking: Option<ModelThinkingLevel>,
    /// `--no-session`.
    pub no_session: bool,
    /// `--session`.
    pub session: Option<String>,
    /// `--session-id`.
    pub session_id: Option<String>,
    /// `--fork`.
    pub fork: Option<String>,
    /// `--session-dir`.
    pub session_dir: Option<String>,
    /// `--models`, comma-separated patterns.
    pub models: Option<Vec<String>>,
    /// `--tools`, `-t`: a checked list of names, patterns, or `+name` and
    /// `-name` changes.
    pub tools: Option<Vec<String>>,
    /// `--exclude-tools`, `-xt`.
    pub exclude_tools: Option<Vec<String>>,
    /// `--no-tools`, `-nt`.
    pub no_tools: bool,
    /// `--no-builtin-tools`, `-nbt`.
    pub no_builtin_tools: bool,
    /// `--skill`, each path in order.
    pub skills: Vec<String>,
    /// `--no-skills`, `-ns`.
    pub no_skills: bool,
    /// `--prompt-template`, each path in order.
    pub prompt_templates: Vec<String>,
    /// `--no-prompt-templates`, `-np`.
    pub no_prompt_templates: bool,
    /// `--no-context-files`, `-nc`.
    pub no_context_files: bool,
    /// `--no-mcp`: read no `mcp.json` and start no MCP server.
    pub no_mcp: bool,
    /// `--list-models [search]`: `Some(None)` when given without a search
    /// pattern, `Some(Some(pattern))` with one.
    pub list_models: Option<Option<String>>,
    /// `--export <session-file>`: the session file to write as HTML. The
    /// first positional message is its output path.
    pub export: Option<String>,
    /// Bake's `--permission`: the preset to select.
    pub permission: Option<String>,
    /// Bake's `--sandbox`: the sandbox mode to set.
    pub sandbox: Option<SandboxMode>,
    /// Bake's `--approval`: the approval policy to set.
    pub approval: Option<ApprovalPolicy>,
    /// Bake's `--inline`: interactive mode keeps its transcript in the
    /// terminal's scrollback instead of the alternate screen. Print mode
    /// ignores it.
    pub inline: bool,
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

/// Pi's comma-separated tool list: trimmed, empty entries dropped.
fn tool_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|entry| crate::tools::js::trim(entry).to_owned())
        .filter(|entry| !entry.is_empty())
        .collect()
}

const VALID_THINKING_LEVELS: &str = "off, minimal, low, medium, high, xhigh, max";

/// Pi's `normalizeSessionName`: trimmed, or `None` when blank.
pub fn normalize_session_name(value: &str) -> Option<String> {
    let trimmed = crate::tools::js::trim(value);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

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
            "--recoverable" => result.recoverable = true,
            "--resume-run" => {
                match next.filter(|value| !value.is_empty() && !value.starts_with('-')) {
                    Some(id) => {
                        result.resume_run = Some(id.to_owned());
                        i += 1;
                    }
                    None => result
                        .diagnostics
                        .push(Diagnostic::error("--resume-run requires a run id")),
                }
            }
            "--max-model-calls" | "--run-timeout-ms" => {
                match next
                    .filter(|value| {
                        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
                    })
                    .and_then(|value| value.parse::<u64>().ok())
                {
                    Some(value) => {
                        if arg == "--max-model-calls" {
                            result.max_model_calls = Some(value);
                        } else {
                            result.run_timeout_ms = Some(value);
                        }
                        i += 1;
                    }
                    None => result.diagnostics.push(Diagnostic::error(format!(
                        "{arg} requires a non-negative integer"
                    ))),
                }
            }
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
            "--resume" | "-r" => result.resume = true,
            "--provider" if next.is_some() => {
                result.provider = next.map(str::to_owned);
                i += 1;
            }
            "--model" if next.is_some() => {
                result.model = next.map(str::to_owned);
                i += 1;
            }
            "--api-key" if next.is_some() => {
                result.api_key = next.map(str::to_owned);
                i += 1;
            }
            "--system-prompt" if next.is_some() => {
                result.system_prompt = next.map(str::to_owned);
                i += 1;
            }
            "--append-system-prompt" if next.is_some() => {
                result.append_system_prompt.extend(next.map(str::to_owned));
                i += 1;
            }
            "--name" | "-n" => match next {
                Some(name) => {
                    result.name = Some(name.to_owned());
                    i += 1;
                }
                None => result
                    .diagnostics
                    .push(Diagnostic::error("--name requires a value")),
            },
            "--no-session" => result.no_session = true,
            "--inline" => result.inline = true,
            "--session" if next.is_some() => {
                result.session = next.map(str::to_owned);
                i += 1;
            }
            "--session-id" if next.is_some() => {
                result.session_id = next.map(str::to_owned);
                i += 1;
            }
            "--fork" if next.is_some() => {
                result.fork = next.map(str::to_owned);
                i += 1;
            }
            "--session-dir" if next.is_some() => {
                result.session_dir = next.map(str::to_owned);
                i += 1;
            }
            "--models" if next.is_some() => {
                result.models = Some(tool_list(next.unwrap_or_default()));
                i += 1;
            }
            "--no-tools" | "-nt" => result.no_tools = true,
            "--no-builtin-tools" | "-nbt" => result.no_builtin_tools = true,
            "--skill" if next.is_some() => {
                result.skills.extend(next.map(str::to_owned));
                i += 1;
            }
            "--no-skills" | "-ns" => result.no_skills = true,
            "--prompt-template" if next.is_some() => {
                result.prompt_templates.extend(next.map(str::to_owned));
                i += 1;
            }
            "--no-prompt-templates" | "-np" => result.no_prompt_templates = true,
            "--no-context-files" | "-nc" => result.no_context_files = true,
            "--no-mcp" => result.no_mcp = true,
            "--offline" => result.offline = true,
            "--verbose" => result.verbose = true,
            "--approve" | "-a" => result.project_trust_override = Some(true),
            "--no-approve" | "-na" => result.project_trust_override = Some(false),
            "--export" if next.is_some() => {
                result.export = next.map(str::to_owned);
                i += 1;
            }
            "--list-models" => {
                if let Some(search) = next
                    && !search.starts_with('-')
                    && !search.starts_with('@')
                {
                    result.list_models = Some(Some(search.to_owned()));
                    i += 1;
                } else {
                    result.list_models = Some(None);
                }
            }
            "--tools" | "-t" if next.is_some() => {
                let tools = tool_list(next.unwrap_or_default());
                i += 1;
                match crate::tools::index::get_tool_list_error(&tools) {
                    Some(error) => result
                        .diagnostics
                        .push(Diagnostic::error(format!("{arg}: {error}"))),
                    None => result.tools = Some(tools),
                }
            }
            "--exclude-tools" | "-xt" if next.is_some() => {
                result.exclude_tools = Some(tool_list(next.unwrap_or_default()));
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
            "--permission" if next.is_some() => {
                result.permission = next.map(str::to_owned);
                i += 1;
            }
            "--sandbox" if next.is_some() => {
                let mode = next.unwrap_or_default();
                i += 1;
                match mode.parse::<SandboxMode>() {
                    Ok(mode) => result.sandbox = Some(mode),
                    Err(_) => result.diagnostics.push(Diagnostic::error(format!(
                        "Invalid sandbox mode \"{mode}\". Valid values: read-only, workspace-write, danger-full-access"
                    ))),
                }
            }
            "--approval" if next.is_some() => {
                let policy = next.unwrap_or_default();
                i += 1;
                match policy.parse::<ApprovalPolicy>() {
                    Ok(policy) => result.approval = Some(policy),
                    Err(_) => result.diagnostics.push(Diagnostic::error(format!(
                        "Invalid approval policy \"{policy}\". Valid values: ask, never"
                    ))),
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
    fn run_limits_accept_zero_and_reject_invalid_or_overflowing_values() {
        let args = parse(&[
            "--max-model-calls",
            "0",
            "--run-timeout-ms",
            "100",
            "prompt",
        ]);
        assert_eq!(args.max_model_calls, Some(0));
        assert_eq!(args.run_timeout_ms, Some(100));
        assert_eq!(args.messages, ["prompt"]);
        assert!(args.diagnostics.is_empty());
        for flag in ["--max-model-calls", "--run-timeout-ms"] {
            for value in ["-1", "", "1.5", "18446744073709551616", "--print"] {
                assert!(
                    !parse(&[flag, value]).diagnostics.is_empty(),
                    "{flag} {value}"
                );
            }
            assert!(!parse(&[flag]).diagnostics.is_empty());
        }
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
        assert!(!args.resume);
        assert!(parse(&["-r"]).resume && parse(&["--resume"]).resume);
        assert_eq!(args.session.as_deref(), Some("abc"));
        assert!(args.diagnostics.is_empty());
        // Pi: a flag missing its value falls through to the unknown flags.
        assert_eq!(parse(&["--model"]).unknown_flags, [("model".into(), None)]);
    }

    #[test]
    fn tool_flags() {
        let args = parse(&[
            "--tools",
            " read, grep ,,",
            "-xt",
            "ls,mcp__*",
            "-nt",
            "-nbt",
        ]);
        assert_eq!(args.tools, Some(vec!["read".into(), "grep".into()]));
        assert_eq!(args.exclude_tools, Some(vec!["ls".into(), "mcp__*".into()]));
        assert!(args.no_tools && args.no_builtin_tools);
        assert!(args.diagnostics.is_empty());
    }

    // Pi v1.3.0 `args.ts`: `--no-mcp` takes no value.
    #[test]
    fn no_mcp_flag() {
        assert!(!parse(&["-p", "hi"]).no_mcp);
        let args = parse(&["--no-mcp", "-p", "hi"]);
        assert!(args.no_mcp);
        assert_eq!(args.messages, ["hi"]);
        assert!(args.diagnostics.is_empty() && args.unknown_flags.is_empty());
    }

    #[test]
    fn skill_flags() {
        let args = parse(&["--skill", "a/SKILL.md", "-ns", "--skill", "~/skills"]);
        assert_eq!(args.skills, ["a/SKILL.md", "~/skills"]);
        assert!(args.no_skills);
        assert!(args.diagnostics.is_empty());
        // Pi: `--skill` without a value falls through to the unknown flags.
        assert_eq!(parse(&["--skill"]).unknown_flags, [("skill".into(), None)]);
        assert_eq!(
            parse(&["-t", "+grep,-bash"]).tools,
            Some(vec!["+grep".into(), "-bash".into()])
        );
        assert_eq!(
            parse(&["-t", "read,+grep"]).diagnostics,
            [Diagnostic::error(
                "-t: tool names cannot be mixed with +name or -name entries"
            )]
        );
        assert_eq!(
            parse(&["--tools", "-gr*"]).diagnostics,
            [Diagnostic::error(
                "--tools: +name and -name entries take exact tool names, not patterns: -gr*"
            )]
        );
        // Pi: a missing value falls through, `-t` to an unknown option.
        assert_eq!(
            parse(&["-t"]).diagnostics,
            [Diagnostic::error("Unknown option: -t")]
        );
        assert_eq!(parse(&["--tools"]).unknown_flags, [("tools".into(), None)]);
        // Pi takes the value even when it starts with `-`.
        assert_eq!(
            parse(&["--tools", "-bash"]).tools,
            Some(vec!["-bash".into()])
        );
    }

    // Bake's permission flags.
    #[test]
    fn permission_flags() {
        let args = parse(&[
            "--permission",
            "danger-full-access",
            "--sandbox",
            "read-only",
            "--approval",
            "never",
        ]);
        assert_eq!(args.permission.as_deref(), Some("danger-full-access"));
        assert_eq!(args.sandbox, Some(SandboxMode::ReadOnly));
        assert_eq!(args.approval, Some(ApprovalPolicy::Never));
        assert!(args.diagnostics.is_empty());
        assert_eq!(
            parse(&["--sandbox", "root", "--approval", "always"]).diagnostics,
            [
                Diagnostic::error(
                    "Invalid sandbox mode \"root\". Valid values: read-only, workspace-write, danger-full-access"
                ),
                Diagnostic::error("Invalid approval policy \"always\". Valid values: ask, never"),
            ]
        );
        assert_eq!(
            parse(&["--permission"]).unknown_flags,
            [("permission".into(), None)]
        );
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
            "--custom-flag",
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
                ("custom-flag".into(), Some("k".into())),
                ("flag".into(), Some("v".into())),
                ("bare".into(), None),
            ]
        );
        assert_eq!(args.messages, ["-p"]);
        assert_eq!(args.file_args, ["f"]);
        assert_eq!(
            unknown_flags_message(&args.unknown_flags).as_deref(),
            Some("Unknown options: --custom-flag, --flag, --bare")
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

    // Pi `args.ts`: the project-trust flags take no value, and the last
    // one wins. `bake-rs` refuses them later ([`crate::cli::plan`]).
    #[test]
    fn project_trust_flags_parse_to_one_override() {
        assert_eq!(parse(&["-p", "hi"]).project_trust_override, None);
        for flag in ["--approve", "-a"] {
            assert_eq!(parse(&[flag]).project_trust_override, Some(true));
        }
        for flag in ["--no-approve", "-na"] {
            assert_eq!(parse(&[flag]).project_trust_override, Some(false));
        }
        let args = parse(&["-a", "-na", "-p", "hi"]);
        assert_eq!(args.project_trust_override, Some(false));
        assert_eq!(args.messages, ["hi"]);
        assert!(args.diagnostics.is_empty() && args.unknown_flags.is_empty());
    }

    // Pi `args.ts`: `--verbose` takes no value. `bake-rs` accepts it and
    // starts the same session; nothing else in `Args` changes.
    #[test]
    fn verbose_flag_is_accepted_and_changes_nothing_else() {
        assert!(!parse(&["-p", "hi"]).verbose);
        let args = parse(&["--verbose", "-p", "hi"]);
        assert!(args.verbose);
        assert_eq!(args.messages, ["hi"]);
        assert!(args.diagnostics.is_empty() && args.unknown_flags.is_empty());
        assert_eq!(
            Args {
                verbose: false,
                ..args.clone()
            },
            parse(&["-p", "hi"])
        );
    }

    // Pi `args.ts`: `--offline` takes no value.
    #[test]
    fn offline_flag() {
        assert!(!parse(&["-p", "hi"]).offline);
        let args = parse(&["--offline", "-p", "hi"]);
        assert!(args.offline);
        assert_eq!(args.messages, ["hi"]);
        assert!(args.diagnostics.is_empty() && args.unknown_flags.is_empty());
    }

    // Pi `args.ts`: `--export` takes the next argument, and the first
    // positional message is the output path.
    #[test]
    fn export_takes_a_session_file_and_an_optional_output() {
        let args = parse(&["--export", "a.jsonl"]);
        assert_eq!(args.export.as_deref(), Some("a.jsonl"));
        assert!(args.messages.is_empty());
        let args = parse(&["--export", "a.jsonl", "out.html"]);
        assert_eq!(args.export.as_deref(), Some("a.jsonl"));
        assert_eq!(args.messages, ["out.html"]);
        // Pi: a missing value falls through to the unknown flags.
        assert_eq!(
            parse(&["--export"]).unknown_flags,
            [("export".into(), None)]
        );
    }

    #[test]
    fn prompt_template_and_list_models_flags() {
        let args = parse(&[
            "--prompt-template",
            "/a/one.md",
            "--prompt-template",
            "/b/dir",
            "-np",
            "--list-models",
            "sonnet",
        ]);
        assert_eq!(args.prompt_templates, ["/a/one.md", "/b/dir"]);
        assert!(args.no_prompt_templates);
        assert_eq!(args.list_models, Some(Some("sonnet".into())));
        assert!(args.diagnostics.is_empty());

        let bare = parse(&["--no-prompt-templates", "--list-models", "-p", "hi"]);
        assert!(bare.no_prompt_templates);
        assert_eq!(bare.list_models, Some(None));
        assert!(bare.print);
        assert_eq!(bare.messages, ["hi"]);
    }
}
