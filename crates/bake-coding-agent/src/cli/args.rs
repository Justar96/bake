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
//! startup over its `quietStartup` setting; `bake` has no such setting
//! and no quiet startup, so its startup already reports what Pi's verbose
//! startup reports: the session file, the model fallback, every setup
//! warning, and each MCP server's failure.
//!
//! Bake adds `--permission <preset>`, `--sandbox <mode>`, and
//! `--approval <policy>`, which select the session's permission (D24) the
//! way Pi's `--thinking` selects its thinking level, and `--inline`, which
//! keeps interactive mode in the terminal's scrollback.
//!
//! For callers that are programs, Bake also takes the headless
//! conventions other agent command lines share (Claude Code's and Gemini
//! CLI's `--output-format`): `--output-format text|json|stream-json`,
//! another way to choose print mode's output ([`Mode::Result`] and
//! [`Mode::StreamJson`]), and a bare `-`, which reads the prompt from
//! standard input however long it takes ([`Args::read_stdin`]).

use bake_ai::ModelThinkingLevel;

use bake_sandbox::SandboxMode;

use crate::model_registry::resolver::parse_thinking_level;
use crate::permissions::ApprovalPolicy;

/// Pi's `--mode` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `text`
    Text,
    /// `json`
    Json,
    /// `rpc`: Pi's JSONL protocol on standard input and output
    /// ([`crate::rpc`]).
    Rpc,
    /// Bake's `--output-format json`: one result object when the run ends
    /// ([`crate::print_mode::PrintOutput::Result`]).
    Result,
    /// Bake's `--output-format stream-json`: `json`'s events, then the
    /// result object.
    StreamJson,
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
    /// `--version`, `-v` (and `-V`, which `bake` kept).
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
    /// Bake's `--output-format`: `text` is [`Mode::Text`], `json`
    /// [`Mode::Result`], and `stream-json` [`Mode::StreamJson`]. It cannot
    /// be combined with `--mode` ([`crate::cli::plan`]).
    pub output_format: Option<Mode>,
    /// Bake's `-`: standard input is the prompt's start, read to its end
    /// however long it takes, even beside a message argument and even from
    /// a terminal. It implies print mode.
    pub read_stdin: bool,
    /// `--continue`, `-c`.
    pub continue_session: bool,
    /// `--resume`, `-r`: pick the session to open. Interactive mode only.
    pub resume: bool,
    /// `--offline`: run no startup network operation. The environment can
    /// set it too ([`crate::cli::offline_mode`]).
    pub offline: bool,
    /// `--verbose`: force verbose startup. `bake` has no quiet startup
    /// to override, so nothing changes; see the module documentation.
    pub verbose: bool,
    /// Pi's `projectTrustOverride`: `--approve` (`-a`) is `Some(true)` and
    /// `--no-approve` (`-na`) is `Some(false)`. `bake` has no project
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
                    .push(Diagnostic::error("--mode requires text, json, or rpc")),
                Some(mode) if mode.starts_with('-') => result
                    .diagnostics
                    .push(Diagnostic::error("--mode requires text, json, or rpc")),
                Some(mode) => {
                    i += 1;
                    match mode {
                        "text" => result.mode = Some(Mode::Text),
                        "json" => result.mode = Some(Mode::Json),
                        "rpc" => result.mode = Some(Mode::Rpc),
                        other => result.diagnostics.push(Diagnostic::error(format!(
                            "Invalid mode \"{other}\". Valid values: text, json, rpc"
                        ))),
                    }
                }
            },
            "--output-format" => match next.filter(|format| !format.starts_with('-')) {
                None => result.diagnostics.push(Diagnostic::error(
                    "--output-format requires text, json, or stream-json",
                )),
                Some(format) => {
                    i += 1;
                    match format {
                        "text" => result.output_format = Some(Mode::Text),
                        "json" => result.output_format = Some(Mode::Result),
                        "stream-json" => result.output_format = Some(Mode::StreamJson),
                        other => result.diagnostics.push(Diagnostic::error(format!(
                            "Invalid output format \"{other}\". Valid values: text, json, stream-json"
                        ))),
                    }
                }
            },
            "-" => result.read_stdin = true,
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
    //! Cases from Pi `test/args.test.ts` (v1.3.0,
    //! `71023aec1546d6d677a65669c5665c0927ce0dd0`) for every flag `bake`
    //! accepts, and for the flags it excludes, which it refuses as Pi
    //! refuses an option no extension registered. Bake's own flags have
    //! their own cases.
    //!
    //! Each of Pi's 85 cases, by `describe` block, and the test here that
    //! ports it (a `test.each` row counts once, as Pi's file declares it).
    //! "Excluded" cases are for the extension, theme, and `--tui-mode` flags
    //! scope 13 leaves out (D43); their port proves the flag is refused as
    //! an unknown option instead of parsed.
    //!
    //! | Pi case | Rust test |
    //! |---|---|
    //! | `--version flag`: parses --version flag | [`version_and_help_are_recorded_beside_everything_else`] |
    //! | `--version flag`: parses -v shorthand | [`version_and_help_are_recorded_beside_everything_else`] |
    //! | `--version flag`: --version takes precedence over other args | [`version_and_help_are_recorded_beside_everything_else`] |
    //! | `--help flag`: parses --help flag | [`version_and_help_are_recorded_beside_everything_else`] |
    //! | `--help flag`: parses -h shorthand | [`version_and_help_are_recorded_beside_everything_else`] |
    //! | `--print flag`: parses --print flag | [`switches_parse_in_their_long_and_short_spellings`] |
    //! | `--print flag`: parses -p shorthand | [`switches_parse_in_their_long_and_short_spellings`] |
    //! | `--print flag`: parses prompt after -p even when it starts with YAML frontmatter | [`print_takes_a_frontmatter_prompt_but_not_an_option`] |
    //! | `--print flag`: does not consume options after -p as prompts | [`print_takes_a_frontmatter_prompt_but_not_an_option`] |
    //! | `--continue flag`: parses --continue flag | [`switches_parse_in_their_long_and_short_spellings`] |
    //! | `--continue flag`: parses -c shorthand | [`switches_parse_in_their_long_and_short_spellings`] |
    //! | `--resume flag`: parses --resume flag | [`session_and_model_flags`] |
    //! | `--resume flag`: parses -r shorthand | [`session_and_model_flags`] |
    //! | `flags with values`: parses --provider | [`session_and_model_flags`] |
    //! | `flags with values`: parses --model | [`session_and_model_flags`] |
    //! | `flags with values`: parses --api-key | [`flags_with_values`] |
    //! | `flags with values`: parses --system-prompt | [`flags_with_values`] |
    //! | `flags with values`: parses --append-system-prompt | [`flags_with_values`] |
    //! | `flags with values`: parses multiple --append-system-prompt flags | [`flags_with_values`] |
    //! | `flags with values`: parses --session | [`flags_with_values`] |
    //! | `flags with values`: parses --session-id | [`flags_with_values`] |
    //! | `flags with values`: parses --fork | [`flags_with_values`] |
    //! | `flags with values`: parses --export | [`export_takes_a_session_file_and_an_optional_output`] |
    //! | `flags with values`: parses --thinking | [`messages_file_arguments_and_combinations`] |
    //! | `flags with values`: parses --models as comma-separated list | [`flags_with_values`] |
    //! | `flags with values`: ignores empty entries in --models | [`flags_with_values`] |
    //! | `--mode flag`: parses --mode %s (text, json, rpc) | [`modes_and_their_errors`] |
    //! | `--mode flag`: rejects invalid --mode value %j | [`mode_errors_take_no_other_argument`] |
    //! | `--mode flag`: reports a missing --mode value | [`modes_and_their_errors`] |
    //! | `--mode flag`: does not consume another option as a --mode value | [`mode_errors_take_no_other_argument`] |
    //! | `--mode flag`: reports an invalid --mode value after a valid one | [`mode_errors_take_no_other_argument`] |
    //! | `--name flag`: parses --name flag with value | [`name_keeps_an_empty_value_and_reports_a_missing_one`] |
    //! | `--name flag`: parses -n shorthand | [`name_keeps_an_empty_value_and_reports_a_missing_one`] |
    //! | `--name flag`: preserves empty values for main validation | [`name_keeps_an_empty_value_and_reports_a_missing_one`] |
    //! | `--name flag`: normalizes display names and rejects whitespace-only values | [`name_keeps_an_empty_value_and_reports_a_missing_one`] |
    //! | `--name flag`: reports missing value | [`name_keeps_an_empty_value_and_reports_a_missing_one`] |
    //! | `--name flag`: works alongside other flags | [`name_keeps_an_empty_value_and_reports_a_missing_one`] |
    //! | `--no-session flag`: parses --no-session flag | [`session_and_model_flags`] |
    //! | `--no-session flag`: preserves custom session IDs for non-persisting commands | [`a_session_id_survives_the_non_persisting_commands`] |
    //! | `--extension flag`: parses single --extension | Excluded: [`pis_extension_theme_and_tui_mode_flags_are_not_recognized`] |
    //! | `--extension flag`: parses -e shorthand | Excluded: [`pis_extension_theme_and_tui_mode_flags_are_not_recognized`] |
    //! | `--extension flag`: parses multiple --extension flags | Excluded: [`pis_extension_theme_and_tui_mode_flags_are_not_recognized`] |
    //! | `--no-extensions flag`: parses --no-extensions flag | Excluded: [`pis_extension_theme_and_tui_mode_flags_are_not_recognized`] |
    //! | `--no-extensions flag`: parses --no-extensions with explicit -e flags | Excluded: [`pis_extension_theme_and_tui_mode_flags_are_not_recognized`] |
    //! | `--no-mcp flag`: parses --no-mcp flag | [`no_mcp_flag`] |
    //! | `--skill flag`: parses single --skill | [`skill_flags`] |
    //! | `--skill flag`: parses multiple --skill flags | [`skill_flags`] |
    //! | `--prompt-template flag`: parses single --prompt-template | [`prompt_template_and_list_models_flags`] |
    //! | `--prompt-template flag`: parses multiple --prompt-template flags | [`prompt_template_and_list_models_flags`] |
    //! | `--theme flag`: parses single --theme | Excluded: [`pis_extension_theme_and_tui_mode_flags_are_not_recognized`] |
    //! | `--theme flag`: parses multiple --theme flags | Excluded: [`pis_extension_theme_and_tui_mode_flags_are_not_recognized`] |
    //! | `--use-theme flag`: parses --use-theme | Excluded: [`pis_extension_theme_and_tui_mode_flags_are_not_recognized`] |
    //! | `--use-theme flag`: reports when the theme name value is missing | Excluded: [`pis_extension_theme_and_tui_mode_flags_are_not_recognized`] |
    //! | `--no-skills flag`: parses --no-skills flag | [`switches_parse_in_their_long_and_short_spellings`] |
    //! | `--no-prompt-templates flag`: parses --no-prompt-templates flag | [`prompt_template_and_list_models_flags`] |
    //! | `--no-themes flag`: parses --no-themes flag | Excluded: [`pis_extension_theme_and_tui_mode_flags_are_not_recognized`] |
    //! | `--no-context-files flag`: parses --no-context-files flag | [`context_and_tool_switches_combine`] |
    //! | `--no-context-files flag`: parses -nc shorthand | [`context_and_tool_switches_combine`] |
    //! | `project approval flags`: parses --approve | [`project_trust_flags_parse_to_one_override`] |
    //! | `project approval flags`: parses -a shorthand | [`project_trust_flags_parse_to_one_override`] |
    //! | `project approval flags`: parses --no-approve | [`project_trust_flags_parse_to_one_override`] |
    //! | `project approval flags`: parses -na shorthand | [`project_trust_flags_parse_to_one_override`] |
    //! | `--verbose flag`: parses --verbose flag | [`verbose_flag_is_accepted_and_changes_nothing_else`] |
    //! | `--offline flag`: parses --offline flag | [`offline_flag`] |
    //! | `--tui-mode flag`: parses %s mode (regular, fullscreen) | Excluded: [`pis_extension_theme_and_tui_mode_flags_are_not_recognized`] |
    //! | `--tui-mode flag`: rejects invalid modes | Excluded: [`pis_extension_theme_and_tui_mode_flags_are_not_recognized`] |
    //! | `--tui-mode flag`: requires a mode | Excluded: [`pis_extension_theme_and_tui_mode_flags_are_not_recognized`] |
    //! | `--tui-mode flag`: does not recognize the old --ui-mode flag | [`pis_extension_theme_and_tui_mode_flags_are_not_recognized`] |
    //! | `tool flags`: parses --no-tools flag | [`switches_parse_in_their_long_and_short_spellings`] |
    //! | `tool flags`: parses -nt shorthand | [`switches_parse_in_their_long_and_short_spellings`] |
    //! | `tool flags`: parses --no-builtin-tools flag | [`switches_parse_in_their_long_and_short_spellings`] |
    //! | `tool flags`: parses -nbt shorthand | [`switches_parse_in_their_long_and_short_spellings`] |
    //! | `tool flags`: parses --tools flag | [`switches_parse_in_their_long_and_short_spellings`] |
    //! | `tool flags`: parses -t shorthand | [`switches_parse_in_their_long_and_short_spellings`] |
    //! | `tool flags`: parses +name and -name tool modifiers | [`context_and_tool_switches_combine`] |
    //! | `tool flags`: rejects tool names mixed with modifiers | [`a_rejected_tool_list_is_dropped`] |
    //! | `tool flags`: rejects patterns in tool modifiers | [`a_rejected_tool_list_is_dropped`] |
    //! | `tool flags`: parses --exclude-tools flag | [`switches_parse_in_their_long_and_short_spellings`] |
    //! | `tool flags`: parses -xt shorthand | [`switches_parse_in_their_long_and_short_spellings`] |
    //! | `tool flags`: parses --no-tools with explicit --tools flags | [`context_and_tool_switches_combine`] |
    //! | `tool flags`: parses --no-builtin-tools with explicit --tools flags | [`context_and_tool_switches_combine`] |
    //! | `messages and file args`: parses plain text messages | [`messages_file_arguments_and_combinations`] |
    //! | `messages and file args`: parses @file arguments | [`messages_file_arguments_and_combinations`] |
    //! | `messages and file args`: parses mixed messages and file args | [`messages_file_arguments_and_combinations`] |
    //! | `messages and file args`: captures unknown long flags with string values | [`messages_file_arguments_and_combinations`] |
    //! | `messages and file args`: captures unknown boolean long flags | [`messages_file_arguments_and_combinations`] |
    //! | `messages and file args`: captures unknown long flags with equals syntax | [`messages_file_arguments_and_combinations`] |
    //! | `complex combinations`: parses multiple flags together | [`messages_file_arguments_and_combinations`] |

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
        for (value, mode) in [
            ("json", Mode::Json),
            ("text", Mode::Text),
            ("rpc", Mode::Rpc),
        ] {
            let args = parse(&["--mode", value]);
            assert_eq!(args.mode, Some(mode), "{value}");
            assert!(args.diagnostics.is_empty(), "{value}");
        }
        let missing = parse(&["--mode"]);
        assert_eq!(missing.mode, None);
        assert!(missing.unknown_flags.is_empty());
        assert_eq!(
            missing.diagnostics,
            [Diagnostic::error("--mode requires text, json, or rpc")]
        );
        assert_eq!(
            parse(&["--mode", "acp"]).diagnostics,
            [Diagnostic::error(
                "Invalid mode \"acp\". Valid values: text, json, rpc"
            )]
        );
        // Pi's `--mode` takes only its own values.
        assert!(!parse(&["--mode", "stream-json"]).diagnostics.is_empty());
    }

    // Bake's `--output-format`, Claude Code's and Gemini CLI's names for
    // the same choice: `json` is one result object and `stream-json` the
    // event stream ending with it.
    #[test]
    fn output_formats_and_their_errors() {
        for (value, mode) in [
            ("text", Mode::Text),
            ("json", Mode::Result),
            ("stream-json", Mode::StreamJson),
        ] {
            let args = parse(&["--output-format", value, "hi"]);
            assert_eq!(args.output_format, Some(mode), "{value}");
            assert_eq!(args.mode, None, "{value}");
            assert_eq!(args.messages, ["hi"], "{value}");
            assert!(args.diagnostics.is_empty(), "{value}");
        }
        for missing in [&["--output-format"][..], &["--output-format", "-p"]] {
            assert_eq!(
                parse(missing).diagnostics,
                [Diagnostic::error(
                    "--output-format requires text, json, or stream-json"
                )],
                "{missing:?}"
            );
        }
        assert_eq!(
            parse(&["--output-format", "rpc"]).diagnostics,
            [Diagnostic::error(
                "Invalid output format \"rpc\". Valid values: text, json, stream-json"
            )]
        );
    }

    // A bare `-` reads standard input; after `--` it is a message.
    #[test]
    fn a_bare_dash_reads_standard_input() {
        let args = parse(&["-p", "-", "review this"]);
        assert!(args.print && args.read_stdin);
        assert_eq!(args.messages, ["review this"]);
        assert!(args.diagnostics.is_empty());
        assert!(parse(&["-"]).read_stdin);
        let after = parse(&["--", "-"]);
        assert!(!after.read_stdin);
        assert_eq!(after.messages, ["-"]);
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
    // one wins. `bake` refuses them later ([`crate::cli::plan`]).
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

    // Pi `args.ts`: `--verbose` takes no value. `bake` accepts it and
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

    // Pi `args.test.ts` "--version flag" and "--help flag": both are
    // recorded, and a message beside them is still a message.
    #[test]
    fn version_and_help_are_recorded_beside_everything_else() {
        for flag in ["--version", "-v", "-V"] {
            assert!(parse(&[flag]).version, "{flag}");
        }
        for flag in ["--help", "-h"] {
            assert!(parse(&[flag]).help, "{flag}");
        }
        let args = parse(&["--version", "--help", "some message"]);
        assert!(args.version && args.help);
        assert_eq!(args.messages, ["some message"]);
    }

    // Pi `args.test.ts` "--print flag": a prompt that opens with YAML
    // frontmatter is a prompt, and an option after `-p` is not.
    #[test]
    fn print_takes_a_frontmatter_prompt_but_not_an_option() {
        let prompt = "---\ntitle: hello\n---\nSay hi.";
        let args = parse(&["-p", prompt]);
        assert!(args.print);
        assert_eq!(args.messages, [prompt]);
        assert!(args.unknown_flags.is_empty());
        let args = parse(&["-p", "--provider", "openai", "Say hi."]);
        assert!(args.print);
        assert_eq!(args.provider.as_deref(), Some("openai"));
        assert_eq!(args.messages, ["Say hi."]);
    }

    // Pi `args.test.ts` "flags with values".
    #[test]
    fn flags_with_values() {
        assert_eq!(
            parse(&["--api-key", "sk-test-key"]).api_key.as_deref(),
            Some("sk-test-key")
        );
        assert_eq!(
            parse(&["--system-prompt", "You are a helpful assistant"])
                .system_prompt
                .as_deref(),
            Some("You are a helpful assistant")
        );
        assert_eq!(
            parse(&["--append-system-prompt", "Additional context"]).append_system_prompt,
            ["Additional context"]
        );
        assert_eq!(
            parse(&[
                "--append-system-prompt",
                "Context A",
                "--append-system-prompt",
                "Context B"
            ])
            .append_system_prompt,
            ["Context A", "Context B"]
        );
        assert_eq!(
            parse(&["--session", "/path/to/session.jsonl"])
                .session
                .as_deref(),
            Some("/path/to/session.jsonl")
        );
        assert_eq!(
            parse(&["--session-id", "orchestrated-session"])
                .session_id
                .as_deref(),
            Some("orchestrated-session")
        );
        let forked = parse(&["--fork", "1234abcd"]);
        assert_eq!(forked.fork.as_deref(), Some("1234abcd"));
        assert!(forked.messages.is_empty());
        assert_eq!(
            parse(&["--session-dir", "/tmp/sessions"])
                .session_dir
                .as_deref(),
            Some("/tmp/sessions")
        );
        assert_eq!(
            parse(&["--models", "gpt-4o,claude-sonnet,gemini-pro"]).models,
            Some(vec![
                "gpt-4o".into(),
                "claude-sonnet".into(),
                "gemini-pro".into()
            ])
        );
        // Pi issue #10334: empty entries are dropped.
        assert_eq!(
            parse(&["--models", "gpt-4o, ,claude-sonnet,"]).models,
            Some(vec!["gpt-4o".into(), "claude-sonnet".into()])
        );
    }

    // Pi `args.test.ts` "--mode flag" (issue #9045), for the two modes
    // `bake` runs: an invalid or missing value is an error and takes no
    // other argument with it.
    #[test]
    fn mode_errors_take_no_other_argument() {
        for mode in ["yaml", ""] {
            let args = parse(&["--mode", mode, "--version"]);
            assert_eq!(args.mode, None);
            assert!(args.version);
            assert!(args.messages.is_empty() && args.unknown_flags.is_empty());
            assert_eq!(
                args.diagnostics,
                [Diagnostic::error(format!(
                    "Invalid mode \"{mode}\". Valid values: text, json, rpc"
                ))]
            );
        }
        let args = parse(&["--mode", "--version"]);
        assert_eq!(args.mode, None);
        assert!(args.version);
        assert!(args.unknown_flags.is_empty());
        assert_eq!(
            args.diagnostics,
            [Diagnostic::error("--mode requires text, json, or rpc")]
        );
        // The later value decides, and its error is reported.
        assert_eq!(
            parse(&["--mode", "json", "--mode", "yaml"]).diagnostics,
            [Diagnostic::error(
                "Invalid mode \"yaml\". Valid values: text, json, rpc"
            )]
        );
    }

    // Pi `args.test.ts` "--name flag": an empty value is kept for the
    // caller to refuse, and a missing one is an error.
    #[test]
    fn name_keeps_an_empty_value_and_reports_a_missing_one() {
        assert_eq!(
            parse(&["--name", "my-session"]).name.as_deref(),
            Some("my-session")
        );
        assert_eq!(
            parse(&["-n", "quick-session"]).name.as_deref(),
            Some("quick-session")
        );
        assert_eq!(parse(&["--name", ""]).name.as_deref(), Some(""));
        assert_eq!(
            normalize_session_name("  named session  ").as_deref(),
            Some("named session")
        );
        assert_eq!(normalize_session_name("   "), None);
        assert_eq!(
            parse(&["--name"]).diagnostics,
            [Diagnostic::error("--name requires a value")]
        );
        let args = parse(&[
            "--name",
            "named-run",
            "--print",
            "--model",
            "gpt-4o",
            "hello",
        ]);
        assert_eq!(args.name.as_deref(), Some("named-run"));
        assert!(args.print);
        assert_eq!(args.model.as_deref(), Some("gpt-4o"));
        assert_eq!(args.messages, ["hello"]);
    }

    // Pi `args.test.ts` "--no-session flag": a session id is kept for the
    // commands that persist nothing.
    #[test]
    fn a_session_id_survives_the_non_persisting_commands() {
        for other in ["--help", "--list-models", "--no-session"] {
            let args = parse(&["--session-id", "ephemeral-id", other]);
            assert_eq!(args.session_id.as_deref(), Some("ephemeral-id"), "{other}");
        }
        assert!(parse(&["--session-id", "x", "--help"]).help);
        assert_eq!(
            parse(&["--session-id", "x", "--list-models"]).list_models,
            Some(None)
        );
        assert!(parse(&["--session-id", "x", "--no-session"]).no_session);
    }

    // Pi `args.test.ts` "--no-context-files flag" and the tool flags Pi
    // combines with explicit lists.
    #[test]
    fn context_and_tool_switches_combine() {
        for flag in ["--no-context-files", "-nc"] {
            assert!(parse(&[flag]).no_context_files, "{flag}");
        }
        for (flags, no_tools) in [
            (vec!["--no-tools", "--tools", "read,bash"], true),
            (vec!["--no-builtin-tools", "--tools", "read,bash"], false),
        ] {
            let args = parse(&flags);
            assert_eq!(
                args.tools,
                Some(vec!["read".into(), "bash".into()]),
                "{flags:?}"
            );
            assert_eq!(
                (args.no_tools, args.no_builtin_tools),
                (no_tools, !no_tools)
            );
            assert!(args.diagnostics.is_empty(), "{flags:?}");
        }
        let modifiers = parse(&["-t", "+codemode,-write"]);
        assert_eq!(
            modifiers.tools,
            Some(vec!["+codemode".into(), "-write".into()])
        );
        assert!(modifiers.diagnostics.is_empty());
        assert_eq!(
            parse(&["-t", "+mcp__radius__*"]).diagnostics,
            [Diagnostic::error(
                "-t: +name and -name entries take exact tool names, not patterns: +mcp__radius__*"
            )]
        );
    }

    // Pi `args.test.ts` "messages and file args" and "complex
    // combinations".
    #[test]
    fn messages_file_arguments_and_combinations() {
        assert_eq!(parse(&["hello", "world"]).messages, ["hello", "world"]);
        assert_eq!(
            parse(&["@README.md", "@src/main.ts"]).file_args,
            ["README.md", "src/main.ts"]
        );
        let mixed = parse(&["@file.txt", "explain this", "@image.png"]);
        assert_eq!(mixed.file_args, ["file.txt", "image.png"]);
        assert_eq!(mixed.messages, ["explain this"]);
        let unknown = parse(&["--unknown-flag", "message"]);
        assert!(unknown.messages.is_empty());
        assert_eq!(
            unknown.unknown_flags,
            [("unknown-flag".into(), Some("message".into()))]
        );
        assert_eq!(
            parse(&["--unknown-flag"]).unknown_flags,
            [("unknown-flag".into(), None)]
        );
        assert_eq!(
            parse(&["--unknown-flag=value"]).unknown_flags,
            [("unknown-flag".into(), Some("value".into()))]
        );
        let args = parse(&[
            "--provider",
            "anthropic",
            "--model",
            "claude-sonnet",
            "--print",
            "--thinking",
            "high",
            "@prompt.md",
            "Do the task",
        ]);
        assert_eq!(args.provider.as_deref(), Some("anthropic"));
        assert_eq!(args.model.as_deref(), Some("claude-sonnet"));
        assert!(args.print);
        assert_eq!(args.thinking, Some(ModelThinkingLevel::High));
        assert_eq!(args.file_args, ["prompt.md"]);
        assert_eq!(args.messages, ["Do the task"]);
    }

    // Pi's extension and theme flags, and `--tui-mode`, are excluded from
    // `bake`: a long one becomes an unknown flag and a short one an
    // unknown option, both of which the caller refuses
    // ([`crate::cli::plan`]). `--ui-mode`, which Pi also does not take, is
    // no different.
    #[test]
    fn pis_extension_theme_and_tui_mode_flags_are_not_recognized() {
        for (flags, name, value) in [
            (
                vec!["--extension", "./my-extension.ts"],
                "extension",
                Some("./my-extension.ts"),
            ),
            (vec!["--no-extensions"], "no-extensions", None),
            (
                vec!["--theme", "./theme.json"],
                "theme",
                Some("./theme.json"),
            ),
            (vec!["--use-theme", "light"], "use-theme", Some("light")),
            (vec!["--no-themes"], "no-themes", None),
            (vec!["--tui-mode", "regular"], "tui-mode", Some("regular")),
            (
                vec!["--ui-mode", "fullscreen"],
                "ui-mode",
                Some("fullscreen"),
            ),
        ] {
            let args = parse(&flags);
            assert_eq!(
                args.unknown_flags,
                [(name.to_owned(), value.map(str::to_owned))],
                "{flags:?}"
            );
            assert!(args.diagnostics.is_empty(), "{flags:?}");
            assert_eq!(
                unknown_flags_message(&args.unknown_flags).as_deref(),
                Some(format!("Unknown option: --{name}").as_str())
            );
        }
        for flag in ["-e", "-ne"] {
            assert_eq!(
                parse(&[flag, "./ext.ts"]).diagnostics,
                [Diagnostic::error(format!("Unknown option: {flag}"))],
                "{flag}"
            );
        }
        // Repeats, and the values Pi rejects itself, are refused the same
        // way; a following option is still parsed as one.
        let repeated = parse(&["--theme", "./dark.json", "--theme", "./light.json"]);
        assert_eq!(
            repeated.unknown_flags,
            [("theme".into(), Some("./light.json".into()))]
        );
        let extensions = parse(&["--no-extensions", "-e", "foo.ts", "-e", "bar.ts"]);
        assert_eq!(extensions.unknown_flags, [("no-extensions".into(), None)]);
        assert!(
            extensions
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.message == "Unknown option: -e")
                && !extensions.diagnostics.is_empty()
        );
        let theme = parse(&["--use-theme", "--print"]);
        assert!(theme.print);
        assert_eq!(theme.unknown_flags, [("use-theme".into(), None)]);
        assert!(theme.diagnostics.is_empty());
        for flags in [vec!["--tui-mode"], vec!["--tui-mode", "other"]] {
            let args = parse(&flags);
            assert_eq!(
                args.unknown_flags,
                [(
                    "tui-mode".to_owned(),
                    flags.get(1).map(|value| (*value).to_owned())
                )],
                "{flags:?}"
            );
            assert!(args.diagnostics.is_empty(), "{flags:?}");
        }
    }

    // Pi `args.test.ts` "--print flag", "--continue flag", "--no-skills
    // flag", and "tool flags": each switch alone, in each spelling, sets
    // only its own field.
    #[test]
    fn switches_parse_in_their_long_and_short_spellings() {
        type Field = fn(&Args) -> bool;
        let switches: [(&[&str], Field); 6] = [
            (&["--print", "-p"], |args| args.print),
            (&["--continue", "-c"], |args| args.continue_session),
            (&["--no-skills", "-ns"], |args| args.no_skills),
            (&["--no-tools", "-nt"], |args| args.no_tools),
            (&["--no-builtin-tools", "-nbt"], |args| {
                args.no_builtin_tools
            }),
            (&["--no-context-files", "-nc"], |args| args.no_context_files),
        ];
        for (spellings, field) in switches {
            for flag in spellings {
                let args = parse(&[flag]);
                assert!(field(&args), "{flag}");
                assert!(args.messages.is_empty(), "{flag}");
                assert!(
                    args.diagnostics.is_empty() && args.unknown_flags.is_empty(),
                    "{flag}"
                );
                for (others, other) in switches {
                    if !others.contains(flag) {
                        assert!(!other(&args), "{flag} set {others:?}");
                    }
                }
            }
        }
        for flag in ["--tools", "-t"] {
            assert_eq!(
                parse(&[flag, "read,bash"]).tools,
                Some(vec!["read".into(), "bash".into()]),
                "{flag}"
            );
        }
        for flag in ["--exclude-tools", "-xt"] {
            let args = parse(&[flag, "read,bash"]);
            assert_eq!(
                args.exclude_tools,
                Some(vec!["read".into(), "bash".into()]),
                "{flag}"
            );
            assert_eq!(args.tools, None, "{flag}");
        }
    }

    // Pi `args.test.ts` "rejects tool names mixed with modifiers" and
    // "rejects patterns in tool modifiers": the list is dropped, not kept
    // in part, and the error names the flag as typed.
    #[test]
    fn a_rejected_tool_list_is_dropped() {
        let mixed = parse(&["--tools", "read,+codemode"]);
        assert_eq!(mixed.tools, None);
        assert_eq!(
            mixed.diagnostics,
            [Diagnostic::error(
                "--tools: tool names cannot be mixed with +name or -name entries"
            )]
        );
        let pattern = parse(&["-t", "+mcp__radius__*"]);
        assert_eq!(pattern.tools, None);
        assert_eq!(
            pattern.diagnostics,
            [Diagnostic::error(
                "-t: +name and -name entries take exact tool names, not patterns: +mcp__radius__*"
            )]
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
