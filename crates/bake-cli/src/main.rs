//! `bake`: Bake, a terminal coding agent.
//!
//! `bake` in a terminal opens an interactive session in the terminal
//! frontend ([`bake_tui::run_live`]); `bake -p "prompt"` runs one
//! headless turn through [`bake_coding_agent::cli`], a port of Pi's print
//! mode. Both set the session up through [`cli::start`]. `bake preview`
//! opens the sample terminal preview, and `bake docs` prints the user
//! documentation ([`docs`]). `bake --self-check` reports that the
//! executable loads and carries its documentation ([`self_check`]);
//! `bake plugin check` starts one Wasm plugin package in its helper
//! ([`plugin_check`]). These commands need no Bake home; `bake plugin
//! install`, `enable`, `disable`, and `list` manage the Wasm plugins the
//! Bake home enables for sessions ([`plugin_manage`]). `bake update`
//! installs a published release over the native install this executable
//! runs from, or rolls it back ([`update`]).

mod docs;
mod plugin_check;
mod plugin_manage;
mod print_output;
mod self_check;
mod update;

use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use bake_ai::ImageContent;
use bake_coding_agent::cli::{self, Action, CliEnvironment, Interrupt};
use bake_coding_agent::permissions::{ApprovalBroker, Approver};
use bake_coding_agent::print_mode::SharedWriter;
use bake_sandbox::SandboxProvider;
use bake_tui::{Banner, LiveSession, PreviewExit, ScreenMode};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "\
bake: Bake, a terminal coding agent

Usage:
  bake [options] [messages...]
                              Start an interactive session in this terminal,
                              or print one answer with -p
  bake preview [--inline]     Open the sample preview in an interactive terminal
  bake docs [topic]           List the documentation topics, or print one
  bake --self-check           Check that this executable runs on this host,
                              and print a JSON record of its version and
                              platform
  bake plugin check <dir> [<tool> <args-json>]
                              Start a Wasm plugin package in its helper, and
                              optionally ask it about one tool call
  bake plugin install <dir>
                              Check a Wasm plugin package and copy it into the
                              Bake home's plugin store, named by its hash
  bake plugin enable <sha256>
                              Enable an installed package for new sessions
  bake plugin disable <id>    Disable a plugin, clearing a recorded failure
  bake plugin list            List the enabled plugins and their state
  bake update                 Install the published release over this native
                              install and make it current
  bake update --check         Report a published release, installing nothing,
                              and exit 10 when one is available
  bake update --force <version>
                              Install the published version even when it is
                              not newer than the installed one
  bake update --rollback      Make the release the last update replaced
                              current again
  bake auth check --provider <name> [--model <id>] [--json]
                              [--credentials] [--no-refresh]
                              Report whether a provider is ready to send
  bake auth print-api-key --provider <name> [--model <id>]
                              Print a provider's API key on standard output
  bake auth print-bearer-token --provider <name> [--model <id>]
                              [--min-expiry <duration>]
                              Print an OAuth bearer token; bake has no
                              OAuth provider, so this always fails
  bake auth                   Show the auth commands' help

Options:
  -p, --print               Send the prompts, print the final answer, and exit
      --output-format <format>
                            Print-mode output: text (the final answer), json
                            (one result object), or stream-json (every event,
                            then the result object)
  -                         Read the start of the prompt from standard input,
                            waiting for it however long it takes
      --inline              Keep an interactive session in the terminal's
                            scrollback instead of the full screen
      --mode <mode>         Output: text (the final answer), json (every event),
                            or rpc (Pi's JSONL protocol on stdin and stdout)
      --max-model-calls <n> Print-mode request limit shared by all prompts,
                            retries, and summaries; 0 starts no model request
      --run-timeout-ms <n>  Print-mode deadline, starting before the first prompt
      --recoverable        Journal one print prompt for selected-run recovery
      --resume-run <id>    Resume a recorded run in an existing --session <path>
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
  -r, --resume              Pick the session to open, before the first prompt
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
      --no-mcp              Start no MCP server from the Bake home's mcp.json
      --list-models [search]
                            List available models (with optional fuzzy search)
      --export <file> [out.html]
                            Write a session file as one HTML page and exit
      --permission <preset> Permission preset: workspace-write or
                            danger-full-access, or one from settings.json;
                            new sessions default to the saved preset, else
                            workspace-write
      --sandbox <mode>      read-only, workspace-write, or danger-full-access
      --approval <policy>   ask or never
      --offline             Run no startup network operation (BAKE_OFFLINE,
                            then DSH_OFFLINE, set to 1, true, or yes)
      --verbose             Accepted and without effect: bake has no quiet
                            startup to force, and always reports its startup
  -a, --approve             Refused, with the reason: Pi's project trust is
  -na, --no-approve         not ported, so a project's own files are always
                            loaded. --approval sets the approval policy
  -h, --help                Show this help
  -v, --version             Show the version

Messages are prompts, sent in order; piped standard input and @file arguments
are prepended to the first. Beside a message or @file, piped input that sends
nothing within 1 s is left unread, with a warning: pass - to wait for it. The agent starts with the read, bash, edit, and
write tools, the background-job tools job_output, job_list, and job_kill, and
the web tools web_search and web_fetch; grep, find, and ls are available
through --tools, and grep and find need rg and fd on PATH. A session keeps its
permission: write and edit change files only where its sandbox mode allows,
and bash runs only under a sandbox that enforces it: bubblewrap or Landlock
on Linux and Seatbelt on macOS. Without one (Windows, for now), confined bash
calls are refused. Escalations need approval: under --approval ask an
interactive session asks, Y then Enter allowing that one call; print mode
cannot ask, so it refuses.

bake is interactive when both standard streams are terminals and neither
-p nor --mode is given; otherwise it prints. Interactive messages run in
order as the session's first prompts. Enter sends a prompt, or steers the
running turn; Esc stops the turn and puts unsent prompts back in the draft;
Ctrl+C twice quits. Slash commands such as /model are not available yet;
/skill:name and prompt templates work.

Models come from models.json and keys from auth.json in the Bake home
(BAKE_HOME, then DSH_HOME, then ~/.bake), which also holds settings.json, the
saved sessions, and mcp.json, the stdio MCP servers each session starts under
its sandbox mode; bake refuses an mcp.json that other users, the working
directory, or a git repository could change. Instructions come from ~/.agents/AGENTS.md and the
AGENTS.md files from the filesystem root down to the working directory.
Skills come from ~/.agents/skills and every .agents/skills from the working
directory up to its git repository root.

The preview shows sample content only and does not connect to a model.
Ctrl+C twice quits it.

The auth commands read the Bake home's models.json, auth.json, and
settings.json, reach no network, and write nothing. A key reaches standard
output only from 'auth print-api-key' and 'auth check --credentials', whose
purpose is to print it.

From a script or another agent, run one prompt and read one JSON object:

  bake -p --output-format json 'Fix the failing test' < /dev/null

It holds the answer (result), how the run ended (subtype, isError), the
session to continue with --session <sessionId>, and the run's usage; the exit
status is 0 only for a complete answer. See 'bake docs headless'.

With --mode rpc, bake serves Pi's JSONL protocol instead of printing: it
takes commands on standard input and writes responses, the json mode's events,
and approval questions on standard output, until that input ends. Message and
@file arguments are not prompts there; see 'bake docs rpc'.

'bake update' installs the release published for this platform beside the
running one and makes it current only after the new binary passes its own
--self-check; the release it replaces stays installed, and 'bake update
--rollback' returns to it. '--check' installs nothing and exits 10 when an
update is available. Every run takes the install lock, and needs the install
the Bake installer made (BAKE_NATIVE_ROOT names another one).

Run 'bake docs' for the documentation: the Bake home, settings, models,
permissions, sessions, plugins, context files, skills, MCP servers, the RPC
protocol, updates, and the command line.
";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Preview(ScreenMode),
    /// `bake docs`, with a topic or without one.
    Docs(Option<String>),
    /// `bake --self-check`, alone.
    SelfCheck,
    /// `bake plugin check`: a package directory and an optional call.
    PluginCheck(PathBuf, Option<plugin_check::Call>),
    /// `bake plugin install | enable | disable | list`.
    PluginManage(plugin_manage::Manage),
    /// `bake update [--check] [--force <version>] [--rollback]`.
    Update(update::Request),
    /// `bake auth <command>`: the arguments from `auth` on.
    Auth(Vec<String>),
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
        ["help"] | ["preview", "-h" | "--help"] => Ok(Command::Help),
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
        ["--self-check"] => Ok(Command::SelfCheck),
        ["--self-check", extra, ..] => {
            Err(format!("unexpected argument '{extra}' for '--self-check'"))
        }
        ["plugin", "check", dir] => Ok(Command::PluginCheck(PathBuf::from(dir), None)),
        ["plugin", "check", dir, tool, args] => Ok(Command::PluginCheck(
            PathBuf::from(dir),
            Some(plugin_check::parse_call(tool, args)?),
        )),
        ["plugin", "check", _, _] => {
            Err("'plugin check' takes a tool and its arguments together".to_owned())
        }
        ["plugin", "check", _, _, _, extra, ..] => {
            Err(format!("unexpected argument '{extra}' for 'plugin check'"))
        }
        ["plugin", "check"] => Err("'plugin check' needs a package directory".to_owned()),
        ["plugin", "install", dir] => Ok(Command::PluginManage(plugin_manage::Manage::Install(
            PathBuf::from(dir),
        ))),
        ["plugin", "enable", hash] => Ok(Command::PluginManage(plugin_manage::Manage::Enable(
            (*hash).to_owned(),
        ))),
        ["plugin", "disable", id] => Ok(Command::PluginManage(plugin_manage::Manage::Disable(
            (*id).to_owned(),
        ))),
        ["plugin", "list"] => Ok(Command::PluginManage(plugin_manage::Manage::List)),
        [
            "plugin",
            command @ ("install" | "enable" | "disable"),
            rest @ ..,
        ] => Err(format!(
            "'plugin {command}' takes one argument, not {}",
            rest.len()
        )),
        ["plugin", "list", extra, ..] => {
            Err(format!("unexpected argument '{extra}' for 'plugin list'"))
        }
        // A word after `update` is a prompt, as it is after `plugin`;
        // an option belongs to the update command, typo and all.
        ["update", rest @ ..] if rest.first().is_none_or(|first| first.starts_with('-')) => {
            update::parse(rest).map(Command::Update)
        }
        ["auth", ..] => Ok(Command::Auth(args)),
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
    // The Windows sandbox runner re-executes this binary the same way.
    if args
        .first()
        .is_some_and(|arg| arg == bake_sandbox::backends::windows_acl::HELPER_ARG)
    {
        bake_sandbox::backends::windows_acl::helper_main(args.into_iter().skip(1));
    }
    // The Wasm plugin helper sets its own memory limit first; it never
    // returns.
    if args
        .first()
        .is_some_and(|arg| arg == bake_coding_agent::plugins::wasm::HELPER_ARG)
    {
        bake_coding_agent::plugins::wasm::helper_main(args.into_iter().skip(1));
    }
    // So does the unqualified AppContainer runner, which no default chain
    // selects.
    if args
        .first()
        .is_some_and(|arg| arg == bake_sandbox::backends::windows_appcontainer::HELPER_ARG)
    {
        bake_sandbox::backends::windows_appcontainer::helper_main(args.into_iter().skip(1));
    }
    match parse(&args) {
        Ok(Command::Help) => print(HELP),
        Ok(Command::Preview(mode)) => preview(mode),
        Ok(Command::Docs(topic)) => show_docs(topic.as_deref()),
        Ok(Command::SelfCheck) => run_self_check(),
        Ok(Command::PluginCheck(dir, call)) => plugin_check::run(&dir, call.as_ref()),
        Ok(Command::PluginManage(command)) => plugin_manage::run(&command),
        Ok(Command::Update(request)) => update::run(&request),
        Ok(Command::Auth(args)) => run_auth(&args),
        // Without arguments, a terminal opens a session; a pipe gets help.
        Ok(Command::Agent(args))
            if args.is_empty() && !(io::stdin().is_terminal() && io::stdout().is_terminal()) =>
        {
            print(HELP)
        }
        Ok(Command::Agent(args)) => agent(&args),
        Err(message) => {
            // A closed standard error is not a panic.
            let _ = io::stderr()
                .lock()
                .write_all(format!("bake: {message}\n\nRun 'bake --help' for usage.\n").as_bytes());
            ExitCode::from(2)
        }
    }
}

/// `bake auth <command>`: Pi's `runAuthCommand` over the Bake home,
/// reading its files and reaching no network. The Bake 0.3 route is
/// imported as a run imports it (D25), so a provider it contributes is
/// checked too.
fn run_auth(args: &[String]) -> ExitCode {
    let stdout: SharedWriter = Arc::new(Mutex::new(io::stdout()));
    let stderr: SharedWriter = Arc::new(Mutex::new(io::stderr()));
    let home = bake_coding_agent::home::bake_home();
    let imported = match &home {
        Some(home) => cli::import_startup(home, &|name| std::env::var_os(name)),
        None => cli::ImportedStartup::default(),
    };
    let code = cli::auth_command::run_auth_command(
        args,
        home.as_deref(),
        imported.providers,
        &stdout,
        &stderr,
    );
    // `parse` sends only `auth` commands here.
    exit_code(code.unwrap_or(1))
}

fn exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// How to run `bake docs` from this executable, for the prompt's
/// `docs` section and the messages that point at the documentation.
fn docs_command() -> String {
    match std::env::current_exe() {
        Ok(exe) => docs::docs_command(&exe),
        Err(_) => "bake docs".to_owned(),
    }
}

/// `bake docs [topic]`: the topic's Markdown, or the topic list. An
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
                        "bake: unknown docs topic '{name}'\n\n{}",
                        docs::topic_list()
                    )
                    .as_bytes(),
                );
                ExitCode::from(2)
            }
        },
    }
}

/// `bake --self-check`: the record on standard output and status 0, or
/// the reason on standard error and status 1. A closed standard output is
/// status 1, not a panic, so a reader never mistakes a cut record for a pass.
fn run_self_check() -> ExitCode {
    match self_check::record(VERSION, self_check::host_platform(), docs::TOPICS) {
        Ok(line) => print(&line),
        Err(reason) => {
            let _ = io::stderr()
                .lock()
                .write_all(format!("bake: self-check failed: {reason}\n").as_bytes());
            ExitCode::FAILURE
        }
    }
}

/// The stop requests of print mode: `SIGTERM` and `SIGHUP` with Pi's exit
/// codes, 143 and 129, and `SIGINT` (Ctrl+C) with 130, the status a shell
/// gives a process it ends. Each shuts the session down, and with it every
/// MCP server and process tree it owns, before the process exits; a second
/// signal during that shutdown is ignored, since the shutdown is already
/// bounded by its owners.
///
/// The handlers are installed when this is called, inside the runtime and
/// before the session or any MCP server starts, so a signal that comes
/// during setup is held until setup next waits (for piped input, or for
/// the servers' startup), or until the prompt runs, and then shuts the
/// session down, rather than ending the process with its servers left
/// running. A signal that cannot be registered is left at its default.
#[cfg(unix)]
fn interrupt() -> Interrupt {
    use tokio::signal::unix::{Signal, SignalKind, signal};
    /// Resolves when `stream` delivers, never when it was not registered.
    async fn delivered(stream: Option<&mut Signal>) {
        let delivered = match stream {
            Some(stream) => stream.recv().await.is_some(),
            None => false,
        };
        if !delivered {
            std::future::pending::<()>().await;
        }
    }
    let mut term = signal(SignalKind::terminate()).ok();
    let mut hangup = signal(SignalKind::hangup()).ok();
    let mut int = signal(SignalKind::interrupt()).ok();
    Box::pin(async move {
        tokio::select! {
            () = delivered(term.as_mut()) => 143,
            () = delivered(hangup.as_mut()) => 129,
            () = delivered(int.as_mut()) => 130,
        }
    })
}

/// Ctrl+C and Ctrl+Break stop print mode with 130 and shut the session
/// down, as `SIGINT` does on Unix; Windows has no `SIGTERM` or `SIGHUP`.
#[cfg(windows)]
fn interrupt() -> Interrupt {
    use tokio::signal::windows::{ctrl_break, ctrl_c};
    let (c, brk) = (ctrl_c().ok(), ctrl_break().ok());
    Box::pin(async move {
        match (c, brk) {
            (Some(mut c), Some(mut brk)) => {
                tokio::select! {
                    _ = c.recv() => 130,
                    _ = brk.recv() => 130,
                }
            }
            (Some(mut c), None) => {
                c.recv().await;
                130
            }
            (None, Some(mut brk)) => {
                brk.recv().await;
                130
            }
            (None, None) => std::future::pending().await,
        }
    })
}

#[cfg(not(any(unix, windows)))]
fn interrupt() -> Interrupt {
    cli::no_interrupt()
}

/// What [`agent`] runs once the command line is planned.
enum Run {
    /// Print mode, in `text` or `json`.
    Print(Box<cli::Args>, cli::Mode),
    /// RPC mode: Pi's JSONL protocol on standard input and output.
    Rpc(Box<cli::Args>),
    /// An interactive session in this terminal.
    Interactive(Box<cli::Args>),
}

fn agent(args: &[String]) -> ExitCode {
    let stderr: SharedWriter = Arc::new(Mutex::new(io::stderr()));
    let stdin_is_terminal = io::stdin().is_terminal();
    let action = cli::plan(args, stdin_is_terminal, io::stdout().is_terminal(), &stderr);
    let run = match action {
        Action::Help => return print(HELP),
        Action::Version => return print(&format!("bake {VERSION}\n")),
        Action::Export(input, output) => {
            let stdout: SharedWriter = Arc::new(Mutex::new(io::stdout()));
            return exit_code(cli::run_export(&input, output.as_deref(), &stdout, &stderr));
        }
        Action::Exit(code) => return exit_code(code),
        Action::Print(parsed, mode) => Run::Print(parsed, mode),
        Action::Rpc(parsed) => Run::Rpc(parsed),
        Action::Interactive(parsed) => Run::Interactive(parsed),
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
    let code = match run {
        Run::Print(parsed, mode) => runtime.block_on(async move {
            let output = match print_output::Output::start() {
                Ok(output) => output,
                Err(error) => {
                    eprintln!("Error: could not start print output: {error}");
                    return 1;
                }
            };
            let (signals, interrupt) = match print_output::Signals::start(&output, interrupt) {
                Ok(signals) => signals,
                Err(error) => {
                    eprintln!("Error: could not listen for stop requests: {error}");
                    return 1;
                }
            };
            let mut environment = environment(stdin_is_terminal, output.stderr(), interrupt, None);
            environment.stdout = output.stdout();
            let mut code = cli::run(environment, *parsed, mode).await;
            if output.finish().await.is_err() {
                code = 1;
            }
            // Output cancellation may let a prompt settle before the forwarded
            // interrupt wins cli::run's select; the observed signal owns status.
            signals.finish().unwrap_or(code)
        }),
        Run::Rpc(parsed) => runtime.block_on(async move {
            let output = match print_output::Output::start() {
                Ok(output) => output,
                Err(error) => {
                    eprintln!("Error: could not start RPC output: {error}");
                    return 1;
                }
            };
            let (signals, interrupt) = match print_output::Signals::start(&output, interrupt) {
                Ok(signals) => signals,
                Err(error) => {
                    eprintln!("Error: could not listen for stop requests: {error}");
                    return 1;
                }
            };
            // The client answers the session's approval requests (D24)
            // over the protocol, as the terminal answers them interactively.
            let approvals = ApprovalBroker::new();
            let mut environment = environment(
                stdin_is_terminal,
                output.stderr(),
                interrupt,
                Some(approvals.clone()),
            );
            environment.stdout = output.stdout();
            let mut code = cli::run_rpc(environment, *parsed, Some(approvals)).await;
            if output.finish().await.is_err() {
                code = 1;
            }
            // As in print mode, the observed signal owns the status.
            signals.finish().unwrap_or(code)
        }),
        Run::Interactive(parsed) => interactive(&runtime, *parsed, stderr),
    };
    // A blocked standard-input read cannot be cancelled; do not wait for it.
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    exit_code(code)
}

/// The process as the command line sees it. `approvals` is interactive
/// mode's broker; print mode has none, so its escalations fail closed.
fn environment(
    stdin_is_terminal: bool,
    stderr: SharedWriter,
    interrupt: Interrupt,
    approvals: Option<ApprovalBroker>,
) -> CliEnvironment {
    let home = bake_coding_agent::home::bake_home();
    let imported = match &home {
        Some(home) => cli::import_startup(home, &|name| std::env::var_os(name)),
        None => cli::ImportedStartup::default(),
    };
    CliEnvironment {
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        home,
        user_home: std::env::home_dir(),
        docs_command: docs_command(),
        stdin_is_terminal,
        stdin: Box::new(io::stdin()),
        stdout: Arc::new(Mutex::new(io::stdout())),
        stderr,
        extra_providers: imported.providers,
        imported_default_model: imported.default_model,
        imported_default_preset: imported.default_preset,
        sandbox_provider: sandbox_provider(),
        approver: approvals.map(|broker| Arc::new(broker) as Arc<dyn Approver>),
        // This executable is the plugin helper; one compile queue serves
        // every session of the process (`/new` and `/resume` included).
        plugins: bake_coding_agent::plugins::wasm::WasmSetup {
            command: bake_coding_agent::plugins::wasm::HelperCommand::current_exe().ok(),
            ..Default::default()
        },
        warnings: imported.warnings,
        interrupt,
    }
}

/// Interactive mode: the session print mode would run, set up the same way
/// and before the terminal changes mode, so a setup failure is reported as
/// print mode reports it. Unlike print mode, it starts without a model or
/// key, so `/login` and `/model` can set one up. The terminal frontend handles SIGINT, SIGTERM,
/// and SIGHUP itself, shutting the session down before it restores the
/// terminal.
fn interactive(runtime: &tokio::runtime::Runtime, args: cli::Args, stderr: SharedWriter) -> i32 {
    // `--offline`, or BAKE_OFFLINE then DSH_OFFLINE: the terminal starts
    // the session without the CLIProxyAPI catalog refresh.
    let offline = cli::offline_mode(&args, |name| std::env::var_os(name));
    let screen = if args.inline {
        ScreenMode::Inline
    } else {
        ScreenMode::Fullscreen
    };
    // The terminal answers the session's approval requests (D24); until it
    // is connected, and once it is gone, nobody can, so they fail closed.
    let approvals = ApprovalBroker::new();
    let started = runtime.block_on(async {
        let environment = environment(true, stderr, cli::no_interrupt(), Some(approvals.clone()));
        cli::start_interactive(environment, args).await
    });
    let started = match started {
        Ok(started) => started,
        Err(code) => return code,
    };
    let session = started.session;
    let user_home = std::env::home_dir();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| {
            i64::try_from(time.as_millis()).unwrap_or(i64::MAX)
        });
    let banner = Banner {
        version: VERSION.to_owned(),
        directory: tilde(std::path::Path::new(&session.cwd()), user_home.as_deref()),
        session: bake_tui::live::session_summary(
            &session.messages(),
            now_ms,
            session.session_file().is_none(),
        ),
        access: bake_tui::live::access_pills(&started.permission, offline),
        tip: None,
    };
    let mut notes: Vec<String> = session
        .model_fallback_message()
        .map(str::to_owned)
        .into_iter()
        .collect();
    // Without a model or key the terminal still starts, so /login and
    // /model can set one up; a prompt is refused with its draft kept until
    // then. Print mode stays strict.
    if session.check_model_auth().is_err() {
        notes.push(UNCONFIGURED_NOTE.to_owned());
    }
    let mut prompts: Vec<(String, Vec<ImageContent>)> = started
        .initial_message
        .filter(|text| !text.is_empty())
        .map(|text| (text, started.initial_images))
        .into_iter()
        .collect();
    prompts.extend(started.messages.into_iter().map(|text| (text, Vec::new())));
    let live = LiveSession {
        session,
        runtime: runtime.handle().clone(),
        prompts,
        banner: Some(banner),
        notes,
        // `--offline` reaches no network at startup, this check included.
        update_check: (!offline)
            .then(|| update::startup_check(&started.home))
            .flatten(),
        approvals: Some(approvals),
        home: Some(started.home),
        factory: Some(started.factory),
        resume: started.resume,
        offline,
    };
    match bake_tui::run_live(screen, live) {
        Ok(exit) => exit_status(exit),
        Err(err) => {
            eprintln!("bake: interactive mode failed: {err}");
            1
        }
    }
}

/// `path` for the banner, with the user's home shortened to `~`.
fn tilde(path: &std::path::Path, home: Option<&std::path::Path>) -> String {
    let home = home.filter(|home| home.is_absolute());
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        None => path.display().to_string(),
    }
}

/// The note an interactive session without a usable model opens with.
const UNCONFIGURED_NOTE: &str = "No model is ready yet. Run /login to sign in to CLIProxyAPI, or /model to choose a model with a key.";

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
            "bake: preview needs an interactive terminal, but standard {name} is not a terminal"
        );
        return ExitCode::FAILURE;
    }
    match bake_tui::run_preview_mode(mode) {
        Ok(exit) => exit_code(exit_status(exit)),
        Err(err) => {
            eprintln!("bake: preview failed: {err}");
            ExitCode::FAILURE
        }
    }
}

/// The exit status for how the terminal frontend ended.
fn exit_status(exit: PreviewExit) -> i32 {
    match exit {
        PreviewExit::Quit => 0,
        // The terminal is gone, so there is nowhere to report anything; 129
        // is the SIGHUP status the TypeScript launcher reports too.
        PreviewExit::Hangup => 129,
        PreviewExit::Signal(signal) => 128 + signal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Command, String> {
        parse(&args.iter().map(OsString::from).collect::<Vec<_>>())
    }

    #[test]
    fn plugin_check_takes_a_directory_and_an_optional_call() {
        assert_eq!(
            parse_strs(&["plugin", "check", "pkg"]),
            Ok(Command::PluginCheck(PathBuf::from("pkg"), None))
        );
        assert_eq!(
            parse_strs(&["plugin", "check", "pkg", "write", r#"{"path":".env"}"#]),
            Ok(Command::PluginCheck(
                PathBuf::from("pkg"),
                Some(plugin_check::parse_call("write", r#"{"path":".env"}"#).unwrap())
            ))
        );
        for args in [
            &["plugin", "check"][..],
            &["plugin", "check", "pkg", "write"],
            &["plugin", "check", "pkg", "write", "[]"],
            &["plugin", "check", "pkg", "write", "{}", "extra"],
        ] {
            assert!(parse_strs(args).is_err(), "{args:?}");
        }
        assert_eq!(
            parse_strs(&["plugin", "install", "pkg"]),
            Ok(Command::PluginManage(plugin_manage::Manage::Install(
                PathBuf::from("pkg")
            )))
        );
        assert_eq!(
            parse_strs(&["plugin", "disable", "guard"]),
            Ok(Command::PluginManage(plugin_manage::Manage::Disable(
                "guard".to_owned()
            )))
        );
        assert_eq!(
            parse_strs(&["plugin", "list"]),
            Ok(Command::PluginManage(plugin_manage::Manage::List))
        );
        for args in [
            &["plugin", "install"][..],
            &["plugin", "enable", "a", "b"],
            &["plugin", "list", "x"],
        ] {
            assert!(parse_strs(args).is_err(), "{args:?}");
        }
        // Anything else that starts with "plugin" is still a prompt.
        assert_eq!(
            parse_strs(&["plugin", "ideas"]),
            Ok(Command::Agent(vec![
                "plugin".to_owned(),
                "ideas".to_owned()
            ]))
        );
    }

    #[test]
    fn no_arguments_go_to_the_agent_and_never_open_the_preview() {
        assert_eq!(parse_strs(&[]), Ok(Command::Agent(Vec::new())));
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
    fn the_self_check_takes_no_other_argument() {
        assert_eq!(parse_strs(&["--self-check"]), Ok(Command::SelfCheck));
        assert_eq!(
            parse_strs(&["--self-check", "--json"]),
            Err("unexpected argument '--json' for '--self-check'".into())
        );
        // Anywhere but first, it is an agent option, which Pi's parser refuses.
        assert_eq!(
            parse_strs(&["-p", "--self-check"]),
            Ok(Command::Agent(vec!["-p".into(), "--self-check".into()]))
        );
    }

    // Unix paths: a Windows home has a drive.
    #[cfg(unix)]
    #[test]
    fn the_banner_shortens_the_users_home_to_a_tilde() {
        use std::path::Path;
        let home = Some(Path::new("/home/ada"));
        assert_eq!(tilde(Path::new("/home/ada"), home), "~");
        assert_eq!(tilde(Path::new("/home/ada/src/app"), home), "~/src/app");
        // A sibling that only shares the prefix's text is not under it.
        assert_eq!(tilde(Path::new("/home/adam/x"), home), "/home/adam/x");
        assert_eq!(tilde(Path::new("/srv/x"), home), "/srv/x");
        assert_eq!(tilde(Path::new("/srv/x"), None), "/srv/x");
        assert_eq!(tilde(Path::new("/srv/x"), Some(Path::new(""))), "/srv/x");
    }

    #[test]
    fn update_options_are_a_command_and_a_word_after_update_is_a_prompt() {
        assert_eq!(
            parse_strs(&["update"]),
            Ok(Command::Update(update::Request::Install(None)))
        );
        assert_eq!(
            parse_strs(&["update", "--check"]),
            Ok(Command::Update(update::Request::Check))
        );
        assert_eq!(
            parse_strs(&["update", "--rollback"]),
            Ok(Command::Update(update::Request::Rollback))
        );
        assert_eq!(
            parse_strs(&["update", "--force", "0.4.1"]),
            Ok(Command::Update(update::Request::Install(Some(
                "0.4.1".to_owned()
            ))))
        );
        // A mistyped option is a mistake, not a prompt.
        assert!(parse_strs(&["update", "--checks"]).is_err());
        assert_eq!(
            parse_strs(&["update", "the", "docs"]),
            Ok(Command::Agent(vec![
                "update".to_owned(),
                "the".to_owned(),
                "docs".to_owned()
            ]))
        );
    }

    #[test]
    fn unexpected_preview_arguments_name_the_offender() {
        assert_eq!(
            parse_strs(&["preview", "--fullscreen"]),
            Err("unexpected argument '--fullscreen' for 'preview'".into())
        );
    }
}
