//! MCP servers a real `AgentSession` owns, over real stdio server processes
//! and `bake-ai`'s faux model: D24 admission before spawn, tools declared,
//! called, and logged, Esc ending one call, shutdown during `initialize`,
//! failed discovery, tool-list refreshes with in-flight calls keeping their
//! generation, a crashed server, a resumed session that declares no stale
//! tool, the resource tools of a server that offers resources, the prompt
//! section a server's instructions reach, and the replay class of every
//! tool `bake-rs` composes, which no request carries. Each case names the Pi v1.3.0 or Bake TypeScript case it follows,
//! where one exists.
//!
//! The binary is its own fixture server (`fixture.rs`), so it has its own
//! harness (`harness = false`): each case runs on a runtime of its own under
//! a 60 s bound, in a temporary directory it owns. Whatever happens in the
//! case, passing, failing, or timing out, its sessions are shut down and
//! awaited within a bound, then any process it observed that is still
//! running is killed and awaited within a bound, descendants first, a
//! session whose shutdown timed out is awaited once more, and the directory
//! is removed; a cleanup that had to kill or timed out fails the case.
//! Processes are observed by stable identities (`probe.rs`: a Linux pidfd, a
//! Windows process handle, or a macOS pid with its start time), and a case
//! that expects a server or descendant fails if it cannot observe it.

#[path = "mcp_session/fixture.rs"]
mod fixture;
#[path = "mcp_session/probe.rs"]
mod probe;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bake_agent::{
    AgentContext, AgentLoopConfig, AgentMessage, AgentTool, StreamFn, ToolReplay, run_agent_loop,
};
use bake_ai::providers::faux::{
    FauxProvider, FauxProviderOptions, FauxResponseStep, faux_assistant_blocks,
    faux_assistant_message, faux_model, faux_tool_call,
};
use bake_ai::transcript::{get_current_system_message, get_current_tools};
use bake_ai::types::{
    Message, SystemContent, SystemMessage, TranscriptContext, UserContent, UserMessage,
};
use bake_ai::utils::text::get_system_message_text;
use bake_ai::{ApiRegistry, AssistantMessage, StopReason, UserContentBlock};
use bake_coding_agent::agent_session::{AgentSession, AgentSessionOptions};
use bake_coding_agent::auth_storage::AuthStorage;
use bake_coding_agent::cli::{CliEnvironment, no_interrupt, parse_args, start_interactive};
use bake_coding_agent::mcp::{
    McpHttpOptions, McpOptions, McpServerEntry, McpServerState, McpServers, McpTimings,
    McpToolFilter, RESOURCE_TOOL_NAMES, load_mcp_config,
};
use bake_coding_agent::model_registry::ModelRegistry;
use bake_coding_agent::model_registry::config::ModelConfig;
use bake_coding_agent::owned_work::WorkGate;
use bake_coding_agent::permissions::{ApprovalPolicy, StandingPermissions};
use bake_coding_agent::plugins::wasm::WasmSetup;
use bake_coding_agent::session::{NewSessionOptions, SessionManager};
use bake_coding_agent::settings::Settings;
use bake_coding_agent::system_prompt::SystemPromptOptions;
use bake_sandbox::provider::{ConfineFuture, ConfinedCommand, Enforcement, UnavailableProvider};
use bake_sandbox::{SandboxMode, SandboxPolicy, SandboxProvider};
use serde_json::{Value, json};

/// Each case's bound.
const CASE_BOUND: Duration = Duration::from_secs(60);
/// Bounds each wait for something the case already caused.
const BOUND: Duration = Duration::from_secs(20);
/// A variable the test process holds that the scrub must keep from servers.
const SECRET: &str = "BAKE_MCP_SESSION_TEST_SECRET_TOKEN";

type CaseFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
/// A case body.
type CaseBody = fn(Arc<Case>) -> CaseFuture;

fn main() {
    if fixture::run_if_fixture() {
        return;
    }
    // SAFETY: set before any thread or runtime starts.
    unsafe { std::env::set_var(SECRET, "leaked") };
    let filter: Option<String> = std::env::args().skip(1).find(|arg| !arg.starts_with('-'));
    let cases: Vec<(&str, CaseBody)> = vec![
        ("tools_are_admitted_declared_called_and_logged", |case| {
            Box::pin(tools_are_admitted_declared_called_and_logged(case))
        }),
        ("denied_admission_starts_nothing", |case| {
            Box::pin(denied_admission_starts_nothing(case))
        }),
        ("esc_ends_one_call_not_the_server", |case| {
            Box::pin(esc_ends_one_call_not_the_server(case))
        }),
        (
            "shutdown_during_initialize_stops_the_server_and_its_tree",
            |case| {
                Box::pin(shutdown_during_initialize_stops_the_server_and_its_tree(
                    case,
                ))
            },
        ),
        (
            "failed_discovery_publishes_nothing_and_stops_the_server",
            |case| {
                Box::pin(failed_discovery_publishes_nothing_and_stops_the_server(
                    case,
                ))
            },
        ),
        (
            "a_refresh_publishes_a_whole_generation_and_calls_keep_theirs",
            |case| Box::pin(a_refresh_publishes_a_whole_generation_and_calls_keep_theirs(case)),
        ),
        ("a_crashed_server_withdraws_its_tools", |case| {
            Box::pin(a_crashed_server_withdraws_its_tools(case))
        }),
        ("a_resumed_session_declares_no_stale_tool", |case| {
            Box::pin(a_resumed_session_declares_no_stale_tool(case))
        }),
        (
            "a_session_starts_its_servers_once_and_never_after_shutdown",
            |case| {
                Box::pin(a_session_starts_its_servers_once_and_never_after_shutdown(
                    case,
                ))
            },
        ),
        (
            "the_first_prompt_waits_for_startup_only_up_to_its_bound",
            |case| {
                Box::pin(the_first_prompt_waits_for_startup_only_up_to_its_bound(
                    case,
                ))
            },
        ),
        ("a_real_sandbox_confines_the_server", |case| {
            Box::pin(a_real_sandbox_confines_the_server(case))
        }),
        ("a_running_server_keeps_its_admission", |case| {
            Box::pin(a_running_server_keeps_its_admission(case))
        }),
        ("invalid_options_start_nothing", |case| {
            Box::pin(invalid_options_start_nothing(case))
        }),
        ("the_probe_observes_real_processes", |case| {
            Box::pin(the_probe_observes_real_processes(case))
        }),
        ("resource_tools_are_declared_called_and_logged", |case| {
            Box::pin(resource_tools_are_declared_called_and_logged(case))
        }),
        (
            "the_prompt_changes_only_for_a_server_that_reports_something",
            |case| {
                Box::pin(the_prompt_changes_only_for_a_server_that_reports_something(
                    case,
                ))
            },
        ),
        ("a_resource_call_runs_through_the_veto_pipeline", |case| {
            Box::pin(a_resource_call_runs_through_the_veto_pipeline(case))
        }),
        ("a_resource_list_change_republishes_nothing", |case| {
            Box::pin(a_resource_list_change_republishes_nothing(case))
        }),
        (
            "every_composed_tool_has_a_replay_class_no_request_carries",
            |case| {
                Box::pin(every_composed_tool_has_a_replay_class_no_request_carries(
                    case,
                ))
            },
        ),
    ];
    let mut failed = Vec::new();
    let mut ran = 0;
    for (name, body) in cases {
        if filter
            .as_deref()
            .is_some_and(|filter| !name.contains(filter))
        {
            continue;
        }
        ran += 1;
        match run_case(name, body) {
            Ok(()) => println!("test {name} ... ok"),
            Err(error) => {
                println!("test {name} ... FAILED\n{error}");
                failed.push(name);
            }
        }
    }
    println!(
        "\ntest result: {}. {} passed; {} failed",
        if failed.is_empty() { "ok" } else { "FAILED" },
        ran - failed.len(),
        failed.len()
    );
    if !failed.is_empty() {
        std::process::exit(1);
    }
}

/// Runs `body` on a runtime of its own, then cleans up whatever happened.
fn run_case(name: &str, body: CaseBody) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let case = Arc::new(Case::new(name));
        let task = tokio::spawn(body(Arc::clone(&case)));
        let abort = task.abort_handle();
        let outcome = match tokio::time::timeout(CASE_BOUND, task).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(panic_message(error)),
            Err(_) => {
                abort.abort();
                Err(format!("timed out after {CASE_BOUND:?}"))
            }
        };
        let cleanup = case.clean_up().await;
        match (outcome, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Err(cleanup)) => Err(format!("cleanup: {cleanup}")),
            (Err(error), Err(cleanup)) => Err(format!("{error}\ncleanup: {cleanup}")),
        }
    })
}

fn panic_message(error: tokio::task::JoinError) -> String {
    match error.try_into_panic() {
        Ok(panic) => panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_else(|| "panicked".into()),
        Err(error) => error.to_string(),
    }
}

/// What one case owns: its directory, its sessions, and the processes it
/// observed.
struct Case {
    dir: PathBuf,
    sessions: Mutex<Vec<Arc<AgentSession>>>,
    /// Observed processes, each marked whether it is a descendant.
    processes: Mutex<Vec<(bool, probe::Process)>>,
}

impl Case {
    fn new(name: &str) -> Self {
        let mut random = [0; 8];
        getrandom::getrandom(&mut random).expect("random");
        let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let dir = std::env::temp_dir().join(format!("bake-mcp-session-{name}-{id}"));
        std::fs::create_dir_all(dir.join("work")).expect("case directory");
        Self {
            dir: bake_sandbox::canonical_path(&dir),
            sessions: Mutex::default(),
            processes: Mutex::default(),
        }
    }

    fn workspace(&self) -> PathBuf {
        self.dir.join("work")
    }

    /// A server's own directory, where its events go.
    fn server_dir(&self, name: &str) -> PathBuf {
        let dir = self.dir.join(format!("server-{name}"));
        std::fs::create_dir_all(&dir).expect("server directory");
        dir
    }

    fn events(&self, server: &str) -> Vec<String> {
        std::fs::read_to_string(self.dir.join(format!("server-{server}")).join("events"))
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// Waits until `server` has logged an event starting with `prefix`.
    async fn event(&self, server: &str, prefix: &str) -> String {
        let deadline = tokio::time::Instant::now() + BOUND;
        loop {
            if let Some(found) = self
                .events(server)
                .into_iter()
                .find(|event| event.starts_with(prefix))
            {
                return found;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "server {server} never logged {prefix:?}: {:?}",
                self.events(server)
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Observes the server a `pid <n>` event names and, if given, the
    /// descendant a `grandchild <n>` event names, while the server still
    /// runs. A process that cannot be observed fails the case here, on every
    /// platform, so no later check passes for lack of a process.
    fn observe_tree(&self, server: &str, grandchild: Option<&str>) -> Vec<u32> {
        let pid = |event: &str| -> u32 {
            event
                .rsplit(' ')
                .next()
                .and_then(|pid| pid.parse().ok())
                .unwrap_or_else(|| panic!("no pid in {event:?}"))
        };
        let server = probe::Process::observe(pid(server)).unwrap_or_else(|error| panic!("{error}"));
        let descendant = grandchild.map(|event| {
            probe::Process::observe_descendant(&server, pid(event))
                .unwrap_or_else(|error| panic!("{error}"))
        });
        let mut processes = self
            .processes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut pids = vec![server.pid];
        processes.push((false, server));
        if let Some(descendant) = descendant {
            pids.push(descendant.pid);
            processes.push((true, descendant));
        }
        pids
    }

    /// Whether every observed process with one of `pids` has exited, checked
    /// at once, with no wait.
    fn all_exited(&self, pids: &[u32]) -> bool {
        self.observed(pids).iter().all(|exited| *exited)
    }

    /// Whether none of them has exited yet.
    fn none_exited(&self, pids: &[u32]) -> bool {
        self.observed(pids).iter().all(|exited| !*exited)
    }

    fn observed(&self, pids: &[u32]) -> Vec<bool> {
        let processes = self
            .processes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let found: Vec<bool> = processes
            .iter()
            .filter(|(_, process)| pids.contains(&process.pid))
            .map(|(_, process)| process.exited())
            .collect();
        assert_eq!(found.len(), pids.len(), "unobserved pids among {pids:?}");
        found
    }

    /// Shuts every session down and awaits it, bounded; kills, descendants
    /// first, and awaits, bounded, every observed process still running;
    /// then gives a session whose shutdown timed out one more bounded wait,
    /// since its owners can settle once their processes are gone. Any of
    /// that fails the case.
    async fn clean_up(&self) -> Result<(), String> {
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
            .collect();
        let mut problems = Vec::new();
        let mut stuck = Vec::new();
        for session in sessions {
            if tokio::time::timeout(BOUND, session.shutdown())
                .await
                .is_err()
            {
                problems.push("a session did not shut down within its bound".to_owned());
                stuck.push(session);
            }
        }
        let mut processes: Vec<(bool, probe::Process)> = self
            .processes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
            .collect();
        processes.sort_by_key(|(descendant, _)| !*descendant);
        let killed = tokio::task::spawn_blocking(move || {
            processes
                .iter()
                .filter_map(|(_, process)| match process.clean_up() {
                    Ok(false) => None,
                    Ok(true) => Some(format!("process {} was still running", process.pid)),
                    Err(error) => Some(error),
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_else(|error| vec![format!("process cleanup panicked: {error}")]);
        problems.extend(killed);
        for session in stuck {
            if tokio::time::timeout(BOUND, session.shutdown())
                .await
                .is_err()
            {
                problems.push(
                    "a session still did not shut down after its processes were killed".to_owned(),
                );
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        }
    }

    fn keep(&self, session: &Arc<AgentSession>) {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::clone(session));
    }
}

/// A faux model whose responses are scripted, recording the tools each
/// request declares.
struct Model {
    faux: Arc<FauxProvider>,
    registry: Arc<ModelRegistry>,
    apis: Arc<ApiRegistry>,
    requests: Arc<Mutex<Vec<Vec<String>>>>,
    /// Each request's system prompt, as the transcript rebuilds it.
    prompts: Arc<Mutex<Vec<String>>>,
}

impl Model {
    fn new() -> Self {
        let faux = Arc::new(FauxProvider::new(FauxProviderOptions::default()));
        let apis = Arc::new(ApiRegistry::new());
        apis.register(faux.clone());
        let mut registry = ModelRegistry::new(
            ModelConfig::from_value(json!({}), "models.json"),
            AuthStorage::empty(),
        );
        registry
            .add_provider_config(
                "faux",
                json!({
                    "baseUrl": "http://127.0.0.1:9/unused",
                    "api": faux.model().api,
                    "models": [{ "id": "faux-1" }],
                }),
                Some("test-key".into()),
            )
            .expect("faux provider");
        Self {
            faux,
            registry: Arc::new(registry),
            apis,
            requests: Arc::default(),
            prompts: Arc::default(),
        }
    }

    /// Queues `messages` as responses, recording each request's tools.
    fn respond(&self, messages: Vec<AssistantMessage>) {
        let steps = messages
            .into_iter()
            .map(|message| {
                let requests = Arc::clone(&self.requests);
                let prompts = Arc::clone(&self.prompts);
                FauxResponseStep::from_fn(move |context, _, _, _| {
                    let declared = get_current_tools(context.messages())
                        .into_iter()
                        .map(|tool| tool.name)
                        .collect();
                    requests
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(declared);
                    let prompt = get_current_system_message(context.messages())
                        .map(|system| get_system_message_text(&system))
                        .unwrap_or_default();
                    prompts
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(prompt);
                    Ok(message.clone())
                })
            })
            .collect();
        self.faux.append_responses(steps);
    }

    fn requests(&self) -> Vec<Vec<String>> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn calls(calls: &[(&str, Value, &str)]) -> AssistantMessage {
    faux_assistant_blocks(
        calls
            .iter()
            .map(|(name, arguments, id)| faux_tool_call(name, arguments.clone(), Some(id)))
            .collect(),
        StopReason::ToolUse,
    )
}

/// A session over `model` with no built-in tools, persisted in `dir` when
/// it is given.
async fn session(case: &Case, model: &Model, store: Option<SessionManager>) -> Arc<AgentSession> {
    build_session(
        case,
        model,
        store,
        bake_coding_agent::plugins::PluginRegistry::default(),
    )
    .await
}

/// A session like [`session`] whose `plugins` veto its calls.
async fn session_with_plugins(
    case: &Case,
    model: &Model,
    plugins: bake_coding_agent::plugins::PluginRegistry,
) -> Arc<AgentSession> {
    build_session(case, model, None, plugins).await
}

async fn build_session(
    case: &Case,
    model: &Model,
    store: Option<SessionManager>,
    plugins: bake_coding_agent::plugins::PluginRegistry,
) -> Arc<AgentSession> {
    let cwd = case.workspace().to_string_lossy().into_owned();
    let store = store.unwrap_or_else(|| {
        SessionManager::in_memory(Some(&cwd), NewSessionOptions::default(), Vec::new())
            .expect("session")
    });
    let session = Arc::new(
        AgentSession::create(AgentSessionOptions {
            session: store,
            registry: Arc::clone(&model.registry).into(),
            settings_store: None,
            selection: None,
            apis: Arc::clone(&model.apis),
            settings: Settings::default(),
            model: model.registry.model("faux", "faux-1"),
            thinking_level: None,
            tools: Vec::new(),
            active_tool_names: Vec::new(),
            tool_prompts: Vec::new(),
            system_prompt: SystemPromptOptions::new(&cwd),
            docs_command: "bake-rs docs".into(),
            plugins,
            work: Some(WorkGate::default()),
            jobs: None,
        })
        .await
        .expect("session"),
    );
    case.keep(&session);
    session
}

/// A sandbox provider that wraps every command, recording the policy, and
/// marks it with a runner variable the server can read.
#[derive(Default)]
struct Wrapping {
    calls: Mutex<Vec<(Vec<OsString>, SandboxMode)>>,
}

impl SandboxProvider for Wrapping {
    fn name(&self) -> &str {
        "wrapping"
    }

    fn confine<'a>(&'a self, argv: &'a [OsString], policy: &'a SandboxPolicy) -> ConfineFuture<'a> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((argv.to_vec(), policy.mode));
        let mut parts = argv.iter().cloned();
        let program = parts.next().unwrap_or_default();
        let command = ConfinedCommand::new(program, parts.collect(), Enforcement::Full)
            .with_env("BAKE_TEST_CONFINED", policy.mode.as_str());
        Box::pin(async move { Ok(command) })
    }
}

/// The JSON of a server entry that runs this binary as the fixture server.
fn server_json(case: &Case, name: &str, mode: &str) -> Value {
    let exe = std::env::current_exe().expect("test binary");
    json!({
        "command": exe.to_string_lossy(),
        "env": {
            fixture::ROLE: "server",
            "FIXTURE_DIR": case.server_dir(name).to_string_lossy(),
            "FIXTURE_MODE": mode,
        },
        "timeout": 20,
    })
}

/// Entries read through the configuration loader from a file in the case's
/// directory, as a user's `mcp.json` would be.
fn entries(case: &Case, servers: Value) -> Vec<McpServerEntry> {
    let path = case.dir.join("mcp.json");
    std::fs::write(&path, json!({ "mcpServers": servers }).to_string()).expect("mcp.json");
    let config = load_mcp_config(&path);
    assert!(config.errors.is_empty(), "{:?}", config.errors);
    config.servers
}

fn options(
    case: &Case,
    servers: Vec<McpServerEntry>,
    mode: SandboxMode,
    sandbox: Arc<dyn SandboxProvider>,
) -> McpOptions {
    McpOptions {
        servers,
        standing: Arc::new(StandingPermissions::new(
            mode,
            ApprovalPolicy::Never,
            "custom",
            case.workspace(),
            None,
        )),
        sandbox,
        timings: McpTimings {
            startup_wait: BOUND,
            stdin_close_grace: Duration::from_millis(200),
            terminate_grace: Duration::from_millis(500),
            kill_wait: Duration::from_secs(2),
        },
        tool_filter: McpToolFilter::default(),
        http: McpHttpOptions::default(),
        resolver: None,
    }
}

/// Waits until `check` holds for the servers' states.
async fn wait_for(servers: &McpServers, what: &str, check: impl Fn(&[McpServerState]) -> bool) {
    let mut changes = servers.subscribe();
    let deadline = tokio::time::Instant::now() + BOUND;
    loop {
        changes.borrow_and_update();
        let states: Vec<McpServerState> = servers
            .statuses()
            .into_iter()
            .map(|status| status.state)
            .collect();
        if check(&states) {
            return;
        }
        let waited = tokio::time::timeout_at(deadline, changes.changed()).await;
        assert!(matches!(waited, Ok(Ok(()))), "never saw {what}: {states:?}");
    }
}

fn connected(state: &McpServerState, generation_at_least: u64) -> bool {
    matches!(state, McpServerState::Connected { generation, .. } if *generation >= generation_at_least)
}

/// The text of the result of call `id`.
fn result_text(session: &AgentSession, id: &str) -> (String, bool) {
    session
        .messages()
        .iter()
        .filter_map(|message| message.as_tool_result())
        .find(|result| result.tool_call_id == id)
        .map(|result| {
            let text = result
                .content
                .iter()
                .filter_map(|block| match block {
                    UserContentBlock::Text(text) => Some(text.text.clone()),
                    UserContentBlock::Image(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            (text, result.is_error)
        })
        .unwrap_or_else(|| panic!("no result for {id}"))
}

/// The tool names each system line of a session file adds and removes.
fn logged_tool_changes(file: &Path) -> Vec<(Vec<String>, Vec<String>)> {
    let names = |value: &Value| -> Vec<String> {
        value
            .as_array()
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    std::fs::read_to_string(file)
        .expect("session file")
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| entry["type"] == "message" && entry["message"]["role"] == "system")
        .map(|entry| {
            (
                names(&entry["message"]["toolsAdded"]),
                names(&entry["message"]["toolsRemoved"]),
            )
        })
        .filter(|(added, removed)| !added.is_empty() || !removed.is_empty())
        .collect()
}

const FIXTURE_TOOLS: [&str; 9] = [
    "mcp__fixture__echo",
    "mcp__fixture__env",
    "mcp__fixture__slow",
    "mcp__fixture__change",
    "mcp__fixture__break_list",
    "mcp__fixture__spawn",
    "mcp__fixture__fail",
    "mcp__fixture__exit",
    "mcp__fixture__write",
];

// Pi `test/suite/agent-session-mcp.test.ts`, "declares directly exposed MCP
// tools to the model" and "holds the first prompt for servers with direct
// tools"; Bake `mcp-client.e2e.ts` for names, errors, and the scrubbed
// environment (`transport.ts`).
async fn tools_are_admitted_declared_called_and_logged(case: Arc<Case>) {
    let model = Model::new();
    let sessions_dir = case.dir.join("sessions");
    let cwd = case.workspace().to_string_lossy().into_owned();
    let store = SessionManager::create(&cwd, Some(&sessions_dir), NewSessionOptions::default())
        .expect("store");
    let session = session(&case, &model, Some(store)).await;
    let wrapping = Arc::new(Wrapping::default());
    let mut server = server_json(&case, "fixture", "");
    // A value may name a variable the scrub drops: the user chose to pass it.
    server["env"]["TEMPLATED"] = json!(format!("v-${{{SECRET}}}"));
    // The runner's variables go over the entry's: `env` cannot forge them
    // (`env-runner` below still reads the runner's value).
    server["env"]["BAKE_TEST_CONFINED"] = json!("forged");
    let servers = entries(&case, json!({ "fixture": server }));
    let servers = session
        .start_mcp(options(
            &case,
            servers,
            SandboxMode::WorkspaceWrite,
            wrapping.clone(),
        ))
        .expect("start");
    model.respond(vec![
        calls(&[
            ("mcp__fixture__echo", json!({ "text": "hi" }), "echo-1"),
            (
                "mcp__fixture__env",
                json!({ "name": "BAKE_TEST_CONFINED" }),
                "env-runner",
            ),
            ("mcp__fixture__env", json!({ "name": SECRET }), "env-secret"),
            (
                "mcp__fixture__env",
                json!({ "name": "TEMPLATED" }),
                "env-templated",
            ),
            ("mcp__fixture__fail", json!({}), "fail-1"),
            ("mcp__fixture__spawn", json!({}), "spawn-1"),
        ]),
        faux_assistant_message("done"),
    ]);
    // The first prompt waits for the server, so its first request declares
    // the server's tools.
    session.prompt("go", Vec::new()).await.expect("prompt");
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], FIXTURE_TOOLS);
    assert_eq!(result_text(&session, "echo-1"), ("echo: hi".into(), false));
    // Admitted under the standing mode before the spawn, and the runner's
    // variable reached the server.
    assert_eq!(
        result_text(&session, "env-runner"),
        ("workspace-write".into(), false)
    );
    let admitted = wrapping
        .calls
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(admitted.len(), 1);
    assert_eq!(admitted[0].1, SandboxMode::WorkspaceWrite);
    assert_eq!(
        admitted[0].0[0],
        std::env::current_exe().unwrap().into_os_string()
    );
    assert_eq!(
        result_text(&session, "env-secret"),
        ("<unset>".into(), false)
    );
    assert_eq!(
        result_text(&session, "env-templated"),
        ("v-leaked".into(), false)
    );
    assert_eq!(result_text(&session, "fail-1"), ("boom".into(), true));
    // The server got its own tool names.
    let events = case.events("fixture");
    assert!(
        events.iter().any(|event| event.starts_with("call echo ")),
        "{events:?}"
    );
    // The declaration is in the session file: a resumed session rebuilds
    // the same request.
    let file = session.session_file().expect("persisted");
    assert_eq!(
        logged_tool_changes(&file),
        [(FIXTURE_TOOLS.map(str::to_owned).to_vec(), Vec::new())]
    );
    let server = case.event("fixture", "pid ").await;
    let grandchild = case.event("fixture", "grandchild ").await;
    let pids = case.observe_tree(&server, Some(&grandchild));
    assert!(case.none_exited(&pids), "the server runs until shutdown");
    session.shutdown().await;
    // Shutdown returned only once the server and its descendant were gone.
    assert!(case.all_exited(&pids), "a server process outlived shutdown");
    assert_eq!(servers.statuses()[0].state, McpServerState::Closed);
    assert!(servers.tool_names().is_empty());
}

// D24: nothing runs before admission, and a mode no backend enforces, a
// `url` the egress policy refuses, or an unresolvable value is refused.
async fn denied_admission_starts_nothing(case: Arc<Case>) {
    let model = Model::new();
    let session = session(&case, &model, None).await;
    let mut denied = server_json(&case, "denied", "");
    denied["env"]["MISSING"] = json!("${BAKE_MCP_SESSION_TEST_UNSET}");
    let servers = entries(
        &case,
        json!({
            "confined": server_json(&case, "confined", ""),
            "remote": { "url": "http://93.184.216.34/mcp" },
            "unset": denied,
            "off": { "command": "x", "enabled": false },
        }),
    );
    let servers = session
        .start_mcp(options(
            &case,
            servers,
            SandboxMode::ReadOnly,
            Arc::new(UnavailableProvider::new(Some(
                "no backend in this test".into(),
            ))),
        ))
        .expect("start");
    wait_for(&servers, "every refusal", |states| {
        states
            .iter()
            .all(|state| !matches!(state, McpServerState::Connecting))
    })
    .await;
    let statuses = servers.statuses();
    let reason = |index: usize| match &statuses[index].state {
        McpServerState::Refused { reason } => reason.clone(),
        other => panic!("{} was not refused: {other:?}", statuses[index].name),
    };
    assert!(
        reason(0)
            .contains("sandbox mode \"read-only\" is requested but no sandbox backend is usable")
    );
    assert!(reason(0).contains("no backend in this test"));
    assert!(reason(1).contains("would send its headers in clear text"));
    assert!(reason(2).contains("references unset variables: BAKE_MCP_SESSION_TEST_UNSET"));
    assert_eq!(statuses[3].state, McpServerState::Disabled);
    // No process was started.
    assert!(case.events("confined").is_empty());
    assert!(case.events("unset").is_empty());
    model.respond(vec![faux_assistant_message("ready")]);
    session.prompt("go", Vec::new()).await.expect("prompt");
    assert_eq!(model.requests(), [Vec::<String>::new()]);
}

// Pi `test/client.test.ts` cancellation through the session: Esc during a
// call cancels its request (`notifications/cancelled`) and leaves the
// server serving the next prompt.
async fn esc_ends_one_call_not_the_server(case: Arc<Case>) {
    let model = Model::new();
    let session = session(&case, &model, None).await;
    let servers = entries(
        &case,
        json!({ "fixture": server_json(&case, "fixture", "") }),
    );
    let servers = session
        .start_mcp(options(
            &case,
            servers,
            SandboxMode::DangerFullAccess,
            Arc::new(Wrapping::default()),
        ))
        .expect("start");
    // Nothing more is queued: the aborted run sends no further request.
    model.respond(vec![calls(&[("mcp__fixture__slow", json!({}), "slow-1")])]);
    let prompt = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("wait", Vec::new()).await })
    };
    let call = case.event("fixture", "call slow ").await;
    session.abort().await;
    let _ = tokio::time::timeout(BOUND, prompt)
        .await
        .expect("the prompt ends");
    let id = call.rsplit(' ').next().unwrap_or_default().to_owned();
    case.event("fixture", &format!("cancelled {id}")).await;
    let (text, is_error) = result_text(&session, "slow-1");
    assert!(is_error, "{text}");
    assert!(connected(&servers.statuses()[0].state, 1));
    let server = case.event("fixture", "pid ").await;
    // The same server answers the next prompt.
    model.respond(vec![
        calls(&[("mcp__fixture__echo", json!({ "text": "again" }), "echo-2")]),
        faux_assistant_message("done"),
    ]);
    session.prompt("again", Vec::new()).await.expect("prompt");
    assert_eq!(
        result_text(&session, "echo-2"),
        ("echo: again".into(), false)
    );
    let pids: Vec<String> = case
        .events("fixture")
        .into_iter()
        .filter(|event| event.starts_with("pid "))
        .collect();
    assert_eq!(pids, [server]);
}

// Pi `test/mcp-extension.test.ts`, "closes a connection that is still
// initializing before close returns" (#10249), with a server that ignores
// the end of its input and `SIGTERM` and holds a descendant (Pi's
// `stubborn-server.mjs`).
async fn shutdown_during_initialize_stops_the_server_and_its_tree(case: Arc<Case>) {
    let model = Model::new();
    let session = session(&case, &model, None).await;
    let servers = entries(
        &case,
        json!({ "fixture": server_json(&case, "fixture", "stall stubborn grandchild") }),
    );
    let servers = session
        .start_mcp(options(
            &case,
            servers,
            SandboxMode::DangerFullAccess,
            Arc::new(Wrapping::default()),
        ))
        .expect("start");
    let server = case.event("fixture", "pid ").await;
    let grandchild = case.event("fixture", "grandchild ").await;
    let pids = case.observe_tree(&server, Some(&grandchild));
    case.event("fixture", "initialize").await;
    assert_eq!(servers.statuses()[0].state, McpServerState::Connecting);
    tokio::time::timeout(BOUND, session.shutdown())
        .await
        .expect("shutdown ends");
    assert!(case.all_exited(&pids), "a server process outlived shutdown");
    assert_eq!(servers.statuses()[0].state, McpServerState::Closed);
}

// Bake `mcp-client.spec.ts`: a discovery failure or an invalid listing
// publishes nothing; Pi `runtime.ts` `connectOnce` closes the connection.
async fn failed_discovery_publishes_nothing_and_stops_the_server(case: Arc<Case>) {
    let model = Model::new();
    let session = session(&case, &model, None).await;
    let servers = entries(
        &case,
        json!({
            "listing": server_json(&case, "listing", "fail_list grandchild"),
            "schema": server_json(&case, "schema", "bad_schema"),
            "quiet": server_json(&case, "quiet", "no_tools"),
        }),
    );
    let servers = session
        .start_mcp(options(
            &case,
            servers,
            SandboxMode::DangerFullAccess,
            Arc::new(Wrapping::default()),
        ))
        .expect("start");
    let server = case.event("listing", "pid ").await;
    let grandchild = case.event("listing", "grandchild ").await;
    let pids = case.observe_tree(&server, Some(&grandchild));
    // The failing listing is answered only now, so both were observed
    // running.
    std::fs::write(case.dir.join("server-listing").join("release-list"), "").expect("release");
    wait_for(&servers, "both failures and the quiet server", |states| {
        matches!(states[0], McpServerState::Failed { .. })
            && matches!(states[1], McpServerState::Failed { .. })
            && connected(&states[2], 1)
    })
    .await;
    // The owner stopped the failed server before it reported the failure.
    assert!(
        case.all_exited(&pids),
        "a failed server's process outlived its failure"
    );
    let statuses = servers.statuses();
    let McpServerState::Failed { error } = &statuses[0].state else {
        unreachable!()
    };
    assert!(
        error.contains("failed to list its tools") && error.contains("list failed"),
        "{error}"
    );
    let McpServerState::Failed { error } = &statuses[1].state else {
        unreachable!()
    };
    assert!(error.contains("type \"string\", not \"object\""), "{error}");
    // Pi: "connects to servers without the tools capability without
    // listing tools".
    assert!(
        !case
            .events("quiet")
            .iter()
            .any(|event| event.starts_with("list"))
    );
    assert!(servers.tool_names().is_empty());
    model.respond(vec![faux_assistant_message("ready")]);
    session.prompt("go", Vec::new()).await.expect("prompt");
    assert_eq!(model.requests(), [Vec::<String>::new()]);
}

// Pi `agent-session-mcp.test.ts`, "withdraws and restores direct MCP tools
// the server changes"; Bake `mcp-client.spec.ts`: a re-listing that fails
// keeps the previous generation. A call in flight keeps the client of the
// generation it was declared with, and its run keeps its tools.
async fn a_refresh_publishes_a_whole_generation_and_calls_keep_theirs(case: Arc<Case>) {
    let model = Model::new();
    let sessions_dir = case.dir.join("sessions");
    let cwd = case.workspace().to_string_lossy().into_owned();
    let store = SessionManager::create(&cwd, Some(&sessions_dir), NewSessionOptions::default())
        .expect("store");
    let session = session(&case, &model, Some(store)).await;
    let servers = entries(
        &case,
        json!({ "fixture": server_json(&case, "fixture", "") }),
    );
    let servers = session
        .start_mcp(options(
            &case,
            servers,
            SandboxMode::DangerFullAccess,
            Arc::new(Wrapping::default()),
        ))
        .expect("start");
    let release = case.dir.join("release");
    model.respond(vec![
        calls(&[
            (
                "mcp__fixture__slow",
                json!({ "release": release.to_string_lossy() }),
                "slow-1",
            ),
            ("mcp__fixture__change", json!({}), "change-1"),
        ]),
        faux_assistant_message("first done"),
    ]);
    let prompt = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("change", Vec::new()).await })
    };
    case.event("fixture", "call slow ").await;
    // The server's second listing becomes generation 2 while `slow` waits.
    wait_for(&servers, "generation 2", |states| connected(&states[0], 2)).await;
    let mut generation_two = FIXTURE_TOOLS.map(str::to_owned).to_vec();
    generation_two.retain(|name| name != "mcp__fixture__echo");
    generation_two.push("mcp__fixture__extra".into());
    assert_eq!(servers.tool_names(), generation_two);
    std::fs::write(&release, "").expect("release");
    tokio::time::timeout(BOUND, prompt)
        .await
        .expect("the prompt ends")
        .expect("joined")
        .expect("prompt");
    assert_eq!(result_text(&session, "slow-1"), ("slow done".into(), false));
    // The run's second request still declared generation 1.
    let requests = model.requests();
    assert_eq!(requests[1], FIXTURE_TOOLS);

    // The next prompt declares generation 2, and the file logs the change.
    model.respond(vec![
        calls(&[
            ("mcp__fixture__extra", json!({}), "extra-1"),
            ("mcp__fixture__break_list", json!({}), "break-1"),
        ]),
        faux_assistant_message("second done"),
    ]);
    session.prompt("again", Vec::new()).await.expect("prompt");
    assert_eq!(model.requests()[2], generation_two);
    assert_eq!(result_text(&session, "extra-1"), ("extra".into(), false));
    let changes = logged_tool_changes(&session.session_file().expect("persisted"));
    assert_eq!(changes.len(), 2);
    assert_eq!(
        changes[1],
        (
            vec!["mcp__fixture__extra".to_owned()],
            vec!["mcp__fixture__echo".to_owned()]
        )
    );

    // A listing with a repeated name is refused whole; generation 2 stays.
    wait_for(&servers, "the refused refresh", |states| {
        matches!(
            &states[0],
            McpServerState::Connected {
                generation: 2,
                refresh_error: Some(_),
                ..
            }
        )
    })
    .await;
    let statuses = servers.statuses();
    let McpServerState::Connected {
        refresh_error: Some(error),
        ..
    } = &statuses[0].state
    else {
        unreachable!()
    };
    assert!(error.contains("more than once"), "{error}");
    assert_eq!(servers.tool_names(), generation_two);
}

// Pi `mcp-extension.test.ts`, "marks a dropped connection": Bake withdraws
// the tools of a server whose connection closed (no reconnect yet) and
// awaits its process.
async fn a_crashed_server_withdraws_its_tools(case: Arc<Case>) {
    let model = Model::new();
    let session = session(&case, &model, None).await;
    let servers = entries(
        &case,
        json!({ "fixture": server_json(&case, "fixture", "") }),
    );
    let servers = session
        .start_mcp(options(
            &case,
            servers,
            SandboxMode::DangerFullAccess,
            Arc::new(Wrapping::default()),
        ))
        .expect("start");
    model.respond(vec![
        calls(&[("mcp__fixture__exit", json!({}), "exit-1")]),
        faux_assistant_message("done"),
    ]);
    session.prompt("exit", Vec::new()).await.expect("prompt");
    assert_eq!(result_text(&session, "exit-1"), ("bye".into(), false));
    wait_for(&servers, "the disconnect", |states| {
        matches!(states[0], McpServerState::Disconnected { .. })
    })
    .await;
    assert!(servers.tool_names().is_empty());
    model.respond(vec![
        calls(&[("mcp__fixture__echo", json!({ "text": "x" }), "echo-gone")]),
        faux_assistant_message("done"),
    ]);
    session.prompt("again", Vec::new()).await.expect("prompt");
    assert!(model.requests()[2].is_empty(), "{:?}", model.requests());
    let (text, is_error) = result_text(&session, "echo-gone");
    assert!(is_error && text.contains("not found"), "{text}");
}

// Pi `agent-session-mcp.test.ts`, "does not declare unnamed MCP tools
// restored from the transcript": a session reopened without its servers
// declares their tools removed before its next request.
async fn a_resumed_session_declares_no_stale_tool(case: Arc<Case>) {
    let model = Model::new();
    let sessions_dir = case.dir.join("sessions");
    let cwd = case.workspace().to_string_lossy().into_owned();
    let store = SessionManager::create(&cwd, Some(&sessions_dir), NewSessionOptions::default())
        .expect("store");
    let first = session(&case, &model, Some(store)).await;
    let servers = entries(
        &case,
        json!({ "fixture": server_json(&case, "fixture", "") }),
    );
    first
        .start_mcp(options(
            &case,
            servers,
            SandboxMode::DangerFullAccess,
            Arc::new(Wrapping::default()),
        ))
        .expect("start");
    model.respond(vec![faux_assistant_message("one")]);
    first.prompt("one", Vec::new()).await.expect("prompt");
    let file = first.session_file().expect("persisted");
    first.shutdown().await;

    let reopened = SessionManager::open(&file, Some(&sessions_dir), None).expect("reopen");
    let second = session(&case, &model, Some(reopened)).await;
    model.respond(vec![faux_assistant_message("two")]);
    second.prompt("two", Vec::new()).await.expect("prompt");
    let requests = model.requests();
    assert_eq!(requests[0], FIXTURE_TOOLS);
    assert!(requests[1].is_empty(), "{:?}", requests[1]);
    let changes = logged_tool_changes(&file);
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[1].1, FIXTURE_TOOLS.map(str::to_owned).to_vec());
}

async fn a_session_starts_its_servers_once_and_never_after_shutdown(case: Arc<Case>) {
    let model = Model::new();
    let session = session(&case, &model, None).await;
    let first = session.start_mcp(options(
        &case,
        Vec::new(),
        SandboxMode::DangerFullAccess,
        Arc::new(Wrapping::default()),
    ));
    assert!(first.is_ok());
    let again = session.start_mcp(options(
        &case,
        Vec::new(),
        SandboxMode::DangerFullAccess,
        Arc::new(Wrapping::default()),
    ));
    assert!(again.unwrap_err().contains("already started"));
    let closed = self::session(&case, &model, None).await;
    closed.shutdown().await;
    let servers = entries(
        &case,
        json!({ "fixture": server_json(&case, "fixture", "") }),
    );
    let refused = closed.start_mcp(options(
        &case,
        servers,
        SandboxMode::DangerFullAccess,
        Arc::new(Wrapping::default()),
    ));
    assert!(refused.unwrap_err().contains("shut down"));
    assert!(
        case.events("fixture").is_empty(),
        "a closed session started a server"
    );
}

// Pi `agent-session-mcp.test.ts`, "holds the first prompt for servers with
// direct tools only up to startupWaitMs", and an Esc that ends the wait.
async fn the_first_prompt_waits_for_startup_only_up_to_its_bound(case: Arc<Case>) {
    let model = Model::new();
    let session = session(&case, &model, None).await;
    let servers = entries(
        &case,
        json!({ "fixture": server_json(&case, "fixture", "stall") }),
    );
    let mut options = options(
        &case,
        servers,
        SandboxMode::DangerFullAccess,
        Arc::new(Wrapping::default()),
    );
    options.timings.startup_wait = Duration::from_millis(300);
    let servers = session.start_mcp(options).expect("start");
    case.event("fixture", "initialize").await;
    model.respond(vec![faux_assistant_message("ready")]);
    session.prompt("go", Vec::new()).await.expect("prompt");
    assert_eq!(model.requests(), [Vec::<String>::new()]);
    assert_eq!(servers.statuses()[0].state, McpServerState::Connecting);

    // A second session whose wait is ended by Esc instead.
    let waiting = self::session(&case, &model, None).await;
    let entries = entries(
        &case,
        json!({ "other": server_json(&case, "other", "stall") }),
    );
    waiting
        .start_mcp(self::options(
            &case,
            entries,
            SandboxMode::DangerFullAccess,
            Arc::new(Wrapping::default()),
        ))
        .expect("start");
    case.event("other", "initialize").await;
    let prompt = {
        let waiting = Arc::clone(&waiting);
        tokio::spawn(async move { waiting.prompt("go", Vec::new()).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    waiting.abort().await;
    let ended = tokio::time::timeout(Duration::from_secs(5), prompt).await;
    assert!(ended.is_ok(), "Esc did not end the startup wait");
    assert_eq!(
        model.requests().len(),
        1,
        "the aborted prompt sent a request"
    );
}

// D24 with a real backend: under `workspace-write` a server writes its
// workspace and is denied a write elsewhere. Skipped where no backend is
// usable, as `bake-sandbox`'s enforcement tests skip.
async fn a_real_sandbox_confines_the_server(case: Arc<Case>) {
    let provider = bake_sandbox::default_provider();
    let probe = SandboxPolicy::new(SandboxMode::WorkspaceWrite, case.workspace());
    if provider
        .confine(&[OsString::from("true")], &probe)
        .await
        .is_err()
    {
        println!("note: no sandbox backend is usable here; skipping the real confinement case");
        return;
    }
    let outside = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "bake-mcp-denied-{}",
        case.dir.file_name().unwrap_or_default().to_string_lossy()
    ));
    let model = Model::new();
    let session = session(&case, &model, None).await;
    // The server's events go into its workspace, which it may write.
    let mut server = server_json(&case, "fixture", "");
    let events = case.workspace().join("server");
    std::fs::create_dir_all(&events).expect("events dir");
    server["env"]["FIXTURE_DIR"] = json!(events.to_string_lossy());
    let servers = entries(&case, json!({ "fixture": server }));
    session
        .start_mcp(options(
            &case,
            servers,
            SandboxMode::WorkspaceWrite,
            provider,
        ))
        .expect("start");
    let inside = case.workspace().join("inside.txt");
    model.respond(vec![
        calls(&[
            (
                "mcp__fixture__write",
                json!({ "path": inside.to_string_lossy() }),
                "inside",
            ),
            (
                "mcp__fixture__write",
                json!({ "path": outside.to_string_lossy() }),
                "outside",
            ),
        ]),
        faux_assistant_message("done"),
    ]);
    session.prompt("write", Vec::new()).await.expect("prompt");
    let denied = outside.exists();
    let _ = std::fs::remove_file(&outside);
    assert_eq!(result_text(&session, "inside"), ("wrote".into(), false));
    let (text, _) = result_text(&session, "outside");
    assert!(text.starts_with("error: "), "{text}");
    assert!(!denied, "the confined server wrote outside its workspace");
    assert!(inside.exists());
}

// The lifetime boundary of the retained contract (`crate::mcp`'s module
// documentation). Bake's TypeScript resolves the sandbox knob per call in
// the shell, code mode, and file tools (`packages/shell/tool-bash/src/index.ts`
// `resolveSandboxPolicy`, `packages/core/tools/src/ptc.ts`) and spawns MCP
// servers outside it (`packages/mcp/mcp-client/src/transport.ts`), so a
// permission change never reached a running server. Here a server is
// confined once, at its start, under the mode then in force: after the
// session's standing permission is tightened it is the same process, with
// the same tools declared, still under its start-time confinement, and it
// is not admitted again.
async fn a_running_server_keeps_its_admission(case: Arc<Case>) {
    let model = Model::new();
    let session = session(&case, &model, None).await;
    let wrapping = Arc::new(Wrapping::default());
    let servers = entries(
        &case,
        json!({ "fixture": server_json(&case, "fixture", "") }),
    );
    let options = options(
        &case,
        servers,
        SandboxMode::WorkspaceWrite,
        wrapping.clone(),
    );
    let standing = Arc::clone(&options.standing);
    let servers = session.start_mcp(options).expect("start");
    let env = |id: &'static str| {
        calls(&[(
            "mcp__fixture__env",
            json!({ "name": "BAKE_TEST_CONFINED" }),
            id,
        )])
    };
    model.respond(vec![env("before"), faux_assistant_message("one")]);
    session.prompt("one", Vec::new()).await.expect("prompt");
    assert_eq!(
        result_text(&session, "before"),
        ("workspace-write".into(), false)
    );
    standing.set(SandboxMode::ReadOnly, ApprovalPolicy::Never, "custom");
    model.respond(vec![env("after"), faux_assistant_message("two")]);
    session.prompt("two", Vec::new()).await.expect("prompt");
    let requests = model.requests();
    assert_eq!(requests[0], FIXTURE_TOOLS);
    assert_eq!(requests[2], FIXTURE_TOOLS);
    assert_eq!(
        result_text(&session, "after"),
        ("workspace-write".into(), false)
    );
    let admitted: Vec<SandboxMode> = wrapping
        .calls
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|(_, mode)| *mode)
        .collect();
    assert_eq!(admitted, [SandboxMode::WorkspaceWrite]);
    let started: Vec<String> = case
        .events("fixture")
        .into_iter()
        .filter(|event| event.starts_with("pid "))
        .collect();
    assert_eq!(started.len(), 1, "{started:?}");
    assert!(connected(&servers.statuses()[0].state, 1));
    // A server started after the change is admitted under the new mode.
    let later = self::session(&case, &model, None).await;
    let entries = entries(&case, json!({ "later": server_json(&case, "later", "") }));
    let mut options = self::options(&case, entries, SandboxMode::ReadOnly, wrapping.clone());
    options.standing = Arc::clone(&standing);
    later.start_mcp(options).expect("start");
    model.respond(vec![
        calls(&[(
            "mcp__later__env",
            json!({ "name": "BAKE_TEST_CONFINED" }),
            "later",
        )]),
        faux_assistant_message("three"),
    ]);
    later.prompt("three", Vec::new()).await.expect("prompt");
    assert_eq!(result_text(&later, "later"), ("read-only".into(), false));
}

// Public options that bypass the parser: a wait the clock cannot represent
// is refused by `start_mcp` before any server is admitted or spawned, and
// the refusal leaves the session free to start corrected options.
async fn invalid_options_start_nothing(case: Arc<Case>) {
    let model = Model::new();
    let session = session(&case, &model, None).await;
    let wrapping = Arc::new(Wrapping::default());
    let entries = entries(
        &case,
        json!({ "fixture": server_json(&case, "fixture", "") }),
    );
    let mut bad = options(
        &case,
        entries.clone(),
        SandboxMode::DangerFullAccess,
        wrapping.clone(),
    );
    bad.timings.startup_wait = Duration::MAX;
    let error = session.start_mcp(bad).unwrap_err();
    assert!(error.contains("timings.startup_wait"), "{error}");
    let mut bad = options(
        &case,
        entries.clone(),
        SandboxMode::DangerFullAccess,
        wrapping.clone(),
    );
    bad.servers[0].settings.timeout = Duration::MAX;
    let error = session.start_mcp(bad).unwrap_err();
    assert!(
        error.contains("server \"fixture\"") && error.contains("request_timeout"),
        "{error}"
    );
    assert!(session.mcp_servers().is_none());
    assert!(
        wrapping
            .calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    );
    assert!(
        case.events("fixture").is_empty(),
        "a refused start spawned a server"
    );
    let good = options(
        &case,
        entries,
        SandboxMode::DangerFullAccess,
        wrapping.clone(),
    );
    let servers = session.start_mcp(good).expect("start");
    wait_for(&servers, "the server", |states| connected(&states[0], 1)).await;
}

// The observer every process check rests on, on the platform it runs on: a
// running child is observed running, a kill through its identity is seen as
// an exit, and a pid no process can have is not observed, so no check can
// pass for lack of an identity.
async fn the_probe_observes_real_processes(_case: Arc<Case>) {
    let exe = std::env::current_exe().expect("test binary");
    let mut child = std::process::Command::new(exe)
        .env(fixture::ROLE, "sleeper")
        .spawn()
        .expect("sleeper");
    // Held unreaped by `child` until the wait below.
    let observed = probe::Process::observe(child.id());
    let checked = tokio::task::spawn_blocking(move || {
        let observed = observed?;
        if observed.exited() {
            return Err("a running child was observed as exited".to_owned());
        }
        match observed.clean_up()? {
            true if observed.exited() => Ok(()),
            _ => Err("the kill was not observed as an exit".to_owned()),
        }
    })
    .await
    .expect("joined");
    // Whatever happened, the child is reaped before the case ends.
    let _ = child.kill();
    let _ = child.wait();
    checked.unwrap_or_else(|error| panic!("{error}"));
    assert!(probe::Process::observe(u32::MAX).is_err());
}

/// The replay class of every tool `bake-rs` composes with every opt-in on
/// (scope 08, D37): reads rerun, effects do not, and no MCP server's own
/// read-only claim is trusted.
const REPLAY_CLASSES: [(&str, ToolReplay); 22] = [
    ("read", ToolReplay::Safe),
    ("bash", ToolReplay::Never),
    ("edit", ToolReplay::Never),
    ("write", ToolReplay::Never),
    ("grep", ToolReplay::Safe),
    ("find", ToolReplay::Safe),
    ("ls", ToolReplay::Safe),
    ("job_output", ToolReplay::Never),
    ("job_list", ToolReplay::Safe),
    ("job_kill", ToolReplay::Never),
    ("subagent", ToolReplay::Never),
    ("list_subagent_models", ToolReplay::Safe),
    ("send_message", ToolReplay::Never),
    ("interrupt_agent", ToolReplay::Never),
    ("list_agents", ToolReplay::Safe),
    ("task_document", ToolReplay::Never),
    ("schedule_create", ToolReplay::Never),
    ("schedule_list", ToolReplay::Safe),
    ("schedule_delete", ToolReplay::Never),
    ("web_search", ToolReplay::Never),
    ("web_fetch", ToolReplay::Never),
    // Its scripts' nested calls may write, run commands, or leave the
    // machine.
    ("codemode", ToolReplay::Never),
];

/// Writes `text` at `path`, writable by its owner only, as the home's
/// files must be.
fn write_owned(path: &Path, text: &str) {
    std::fs::write(path, text).expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    }
}

/// The first request a run over `tools` sends for a fixed prompt: the
/// transcript every provider builds its body from, then the body of each
/// wire protocol.
async fn first_request(tools: Vec<Arc<AgentTool>>) -> Vec<String> {
    let captured: Arc<Mutex<Option<TranscriptContext>>> = Arc::default();
    let sink = Arc::clone(&captured);
    let stream_fn: StreamFn = Arc::new(move |_, context, _| {
        *sink.lock().unwrap_or_else(PoisonError::into_inner) = Some(context.clone());
        Err("captured".into())
    });
    let model = faux_model("faux-1");
    let convert = bake_agent::hook::convert_to_llm(|messages| {
        let converted: Vec<Message> = messages
            .iter()
            .filter_map(|message| message.as_llm().cloned())
            .collect();
        Box::pin(async move { converted })
    });
    // Fixed timestamps; the tools land on the prompt's own system message.
    let prompts: Vec<AgentMessage> = vec![
        Message::System(SystemMessage {
            content: SystemContent::Text("Be brief.".into()),
            timestamp: 1,
            ..SystemMessage::default()
        })
        .into(),
        Message::User(UserMessage {
            content: UserContent::Text("go".into()),
            timestamp: 2,
        })
        .into(),
    ];
    let _ = run_agent_loop(
        prompts,
        AgentContext {
            messages: Vec::new(),
            tools,
        },
        AgentLoopConfig::new(model.clone(), convert),
        Arc::new(|_| Box::pin(async {})),
        None,
        Some(stream_fn),
    )
    .await;
    let context = captured
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .expect("a request");
    use bake_ai::api::{anthropic_messages, openai_completions, openai_responses};
    vec![
        serde_json::to_string(&context).expect("transcript"),
        openai_completions::build_params(
            &model,
            &context,
            &openai_completions::OpenAICompletionsOptions::default(),
            &openai_completions::get_compat(&model),
            bake_ai::types::CacheRetention::Short,
        )
        .expect("completions body")
        .to_string(),
        openai_responses::build_params(
            &model,
            &context,
            &openai_responses::OpenAIResponsesOptions::default(),
            &openai_responses::get_compat(&model),
        )
        .expect("responses body")
        .to_string(),
        anthropic_messages::build_params(
            &model,
            &context,
            false,
            &anthropic_messages::AnthropicOptions::default(),
        )
        .expect("messages body")
        .to_string(),
    ]
}

// Scope 08: every tool `bake-rs` composes, built through the CLI with the
// task-document, schedule, and subagent opt-ins and an MCP server, has a
// replay class, the one recovery reads (`run_journal`). Plugins add no
// tools, only vetoes (`plugins` module). Neither the class nor `read_only`
// reaches the provider: the session's request declares exactly each tool's
// declaration, and flipping or clearing both on every tool leaves the
// transcript and every wire body byte-identical.
async fn every_composed_tool_has_a_replay_class_no_request_carries(case: Arc<Case>) {
    let home = case.dir.join("home");
    let user = case.dir.join("user");
    for dir in [&home, &user] {
        std::fs::create_dir_all(dir).expect("directory");
    }
    #[cfg(unix)]
    for dir in [
        case.dir.clone(),
        home.clone(),
        user.clone(),
        case.workspace(),
    ] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    write_owned(
        &home.join("settings.json"),
        &json!({ "taskDocuments": true, "schedules": true, "subagents": true }).to_string(),
    );
    // Windows refuses an existing `mcp.json` unread until its ACLs are
    // checked (`mcp::home`), so the MCP half runs on Unix only.
    let with_mcp = cfg!(unix);
    if with_mcp {
        write_owned(
            &home.join("mcp.json"),
            &json!({ "mcpServers": { "fixture": server_json(&case, "fixture", "") } }).to_string(),
        );
    }
    let faux = Arc::new(FauxProvider::new(FauxProviderOptions::default()));
    let requests: Arc<Mutex<Vec<TranscriptContext>>> = Arc::default();
    let seen = Arc::clone(&requests);
    faux.set_responses(vec![FauxResponseStep::from_fn(move |context, _, _, _| {
        seen.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(context.clone());
        Ok(faux_assistant_message("done"))
    })]);
    let stderr: Arc<Mutex<Vec<u8>>> = Arc::default();
    let environment = CliEnvironment {
        cwd: case.workspace(),
        home: Some(home),
        user_home: Some(user),
        docs_command: "bake-rs docs".into(),
        stdin_is_terminal: true,
        stdin: Box::new(std::io::empty()),
        stdout: Arc::new(Mutex::new(Vec::<u8>::new())),
        stderr: stderr.clone(),
        // Two models, so `list_subagent_models` is composed too.
        extra_providers: vec![(
            "faux".into(),
            json!({
                "baseUrl": "http://127.0.0.1:9/unused",
                "api": faux.model().api,
                "models": [{ "id": "faux-1" }, { "id": "faux-2" }],
            }),
            Some("test-key".into()),
        )],
        imported_default_model: None,
        imported_default_preset: None,
        sandbox_provider: Arc::new(UnavailableProvider::new(None)),
        approver: None,
        plugins: WasmSetup::default(),
        warnings: Vec::new(),
        interrupt: no_interrupt(),
    };
    let args = [
        "--model",
        "faux/faux-1",
        "--permission",
        "danger-full-access",
        "--no-session",
        "--tools",
        "*",
    ]
    .map(str::to_owned);
    let Ok(started) = start_interactive(environment, parse_args(&args)).await else {
        panic!(
            "setup failed: {}",
            String::from_utf8_lossy(&stderr.lock().unwrap_or_else(PoisonError::into_inner))
        );
    };
    let session = Arc::new(started.session);
    case.keep(&session);
    session.apis().register(faux.clone());
    if with_mcp {
        let servers = session.mcp_servers().expect("the home's server");
        wait_for(servers, "the server", |states| connected(&states[0], 1)).await;
    }
    session.prompt("go", Vec::new()).await.expect("prompt");

    let tools = session.agent().tools();
    let unclassified: Vec<&str> = tools
        .iter()
        .filter(|tool| tool.replay.is_none())
        .map(|tool| tool.name.as_str())
        .collect();
    assert!(unclassified.is_empty(), "no replay class: {unclassified:?}");
    let classes: BTreeMap<String, Option<ToolReplay>> = tools
        .iter()
        .map(|tool| (tool.name.clone(), tool.replay))
        .collect();
    let expected: BTreeMap<String, Option<ToolReplay>> = REPLAY_CLASSES
        .iter()
        .map(|(name, replay)| ((*name).to_owned(), Some(*replay)))
        .chain(
            FIXTURE_TOOLS
                .iter()
                .filter(|_| with_mcp)
                .map(|name| ((*name).to_owned(), Some(ToolReplay::Never))),
        )
        .collect();
    assert_eq!(classes, expected);
    assert_eq!(classes.len(), tools.len(), "a tool name repeats");

    // The session's own request declares each tool as its declaration.
    let requests = requests
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(requests.len(), 1);
    let declared: BTreeMap<String, Value> = get_current_tools(requests[0].messages())
        .into_iter()
        .map(|tool| (tool.name.clone(), json!(tool)))
        .collect();
    let declarations: BTreeMap<String, Value> = tools
        .iter()
        .map(|tool| (tool.name.clone(), json!(tool.declaration())))
        .collect();
    assert_eq!(declared, declarations);

    // Flipped and cleared classes and read-only flags send the same bytes.
    let altered = |change: fn(&mut AgentTool)| -> Vec<Arc<AgentTool>> {
        tools
            .iter()
            .map(|tool| {
                let mut tool = (**tool).clone();
                change(&mut tool);
                Arc::new(tool)
            })
            .collect()
    };
    let original = first_request(tools.clone()).await;
    assert!(
        original
            .iter()
            .all(|body| (!with_mcp || body.contains("mcp__fixture__echo"))
                && body.contains("schedule_list")),
        "every body declares the tools"
    );
    let flipped = altered(|tool| {
        tool.replay = match tool.replay {
            Some(ToolReplay::Safe) => Some(ToolReplay::Never),
            _ => Some(ToolReplay::Safe),
        };
        tool.read_only = !tool.read_only;
    });
    assert_eq!(first_request(flipped).await, original);
    let cleared = altered(|tool| {
        tool.replay = None;
        tool.read_only = false;
    });
    assert_eq!(first_request(cleared).await, original);
}

/// The content blocks of the result of call `id`, images named by their
/// media type and data.
fn result_blocks(session: &AgentSession, id: &str) -> Vec<String> {
    session
        .messages()
        .iter()
        .filter_map(|message| message.as_tool_result())
        .find(|result| result.tool_call_id == id)
        .map(|result| {
            result
                .content
                .iter()
                .map(|block| match block {
                    UserContentBlock::Text(text) => text.text.clone(),
                    UserContentBlock::Image(image) => {
                        format!("image {} {}", image.mime_type, image.data)
                    }
                })
                .collect()
        })
        .unwrap_or_else(|| panic!("no result for {id}"))
}

/// The JSON payload of a resource listing's result, as the model reads it.
fn result_payload(session: &AgentSession, id: &str) -> Value {
    let text = result_blocks(session, id).join("\n");
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{id} is not JSON: {error}: {text}"))
}

/// The tools a server with the `resources` fixture mode publishes: its own,
/// then the three shared resource tools.
fn resource_fixture_tools() -> Vec<String> {
    let mut names: Vec<String> = FIXTURE_TOOLS.map(str::to_owned).to_vec();
    names.push("mcp__fixture__change_resources".into());
    names.extend(RESOURCE_TOOL_NAMES.map(str::to_owned));
    names
}

// Pi `test/suite/agent-session-mcp.test.ts`, "lists and reads resources
// with Codex's resource tools": a server that declares the `resources`
// capability publishes the three resource tools with its own generation,
// the model lists, lists templates, and reads through them, an oversized
// read is bounded, an image is projected, and the session file logs the
// generation so a resumed session rebuilds the same declarations.
async fn resource_tools_are_declared_called_and_logged(case: Arc<Case>) {
    let model = Model::new();
    let sessions_dir = case.dir.join("sessions");
    let cwd = case.workspace().to_string_lossy().into_owned();
    let store = SessionManager::create(&cwd, Some(&sessions_dir), NewSessionOptions::default())
        .expect("store");
    let session = session(&case, &model, Some(store)).await;
    let servers = entries(
        &case,
        json!({ "fixture": server_json(&case, "fixture", "resources") }),
    );
    let servers = session
        .start_mcp(options(
            &case,
            servers,
            SandboxMode::DangerFullAccess,
            Arc::new(Wrapping::default()),
        ))
        .expect("start");
    model.respond(vec![
        calls(&[
            ("list_mcp_resources", json!({}), "list-1"),
            (
                "list_mcp_resource_templates",
                json!({ "server": "fixture" }),
                "templates-1",
            ),
            (
                "read_mcp_resource",
                json!({ "server": "fixture", "uri": "docs://intro" }),
                "read-1",
            ),
            (
                "read_mcp_resource",
                json!({ "server": "fixture", "uri": "docs://logo" }),
                "read-image",
            ),
            (
                "read_mcp_resource",
                json!({ "server": "fixture", "uri": "docs://huge" }),
                "read-huge",
            ),
            (
                "read_mcp_resource",
                json!({ "server": "nope", "uri": "docs://intro" }),
                "read-nope",
            ),
            (
                "read_mcp_resource",
                json!({ "server": "fixture", "uri": "docs://missing" }),
                "read-missing",
            ),
        ]),
        faux_assistant_message("done"),
    ]);
    session.prompt("read", Vec::new()).await.expect("prompt");

    // The first request declared the server's tools and the resource tools.
    assert_eq!(model.requests()[0], resource_fixture_tools());
    // The `ui://` resource is left out, and `_meta` with it.
    // The model reads the payload as compact JSON, as Codex sends it.
    assert_eq!(
        result_blocks(&session, "list-1"),
        [serde_json::to_string(&json!({
            "resources": [
                { "server": "fixture", "uri": "docs://intro", "name": "intro", "mimeType": "text/markdown" },
                { "server": "fixture", "uri": "docs://logo", "name": "logo", "mimeType": "image/png" },
            ],
        }))
        .unwrap()]
    );
    // Icons are left out of a template listing.
    assert_eq!(
        result_payload(&session, "templates-1"),
        json!({
            "server": "fixture",
            "resourceTemplates": [
                { "server": "fixture", "uriTemplate": "docs://pages/{slug}", "name": "page" },
            ],
        })
    );
    assert_eq!(
        result_text(&session, "read-1"),
        ("# docs://intro".into(), false)
    );
    assert_eq!(
        result_blocks(&session, "read-image"),
        [format!("image image/png {}", fixture::TINY_PNG)]
    );
    let huge = result_blocks(&session, "read-huge").join("\n");
    assert!(huge.starts_with("Warning: truncated output"), "{huge}");
    assert!(
        huge.len() < bake_coding_agent::mcp::MCP_OUTPUT_MAX_BYTES + 1_024,
        "{}",
        huge.len()
    );
    let saved = huge
        .lines()
        .last()
        .and_then(|line| line.strip_prefix("[Full output: "))
        .and_then(|rest| rest.strip_suffix(" (read it with offset/limit)]"))
        .unwrap_or_else(|| panic!("{huge}"));
    assert_eq!(
        std::fs::read_to_string(saved)
            .expect("the saved output")
            .len(),
        fixture::HUGE_RESOURCE_BYTES
    );
    std::fs::remove_dir_all(Path::new(saved).parent().expect("a private directory"))
        .expect("remove the spill directory");
    // Pi names the servers that have resources when one does not.
    assert_eq!(
        result_text(&session, "read-nope"),
        (
            "MCP server \"nope\" has no resources. Servers with resources: fixture".into(),
            true
        )
    );
    let (missing, is_error) = result_text(&session, "read-missing");
    assert!(missing.contains("Resource not found"), "{missing}");
    assert!(is_error);

    // The declaration is in the session file, so a resumed session rebuilds
    // the same request; the server is gone, so it declares them removed.
    let file = session.session_file().expect("persisted");
    assert_eq!(
        logged_tool_changes(&file),
        [(resource_fixture_tools(), Vec::new())]
    );
    session.shutdown().await;
    assert!(servers.tool_names().is_empty());

    let reopened = SessionManager::open(&file, Some(&sessions_dir), None).expect("reopen");
    let resumed = build_session(
        &case,
        &model,
        Some(reopened),
        bake_coding_agent::plugins::PluginRegistry::default(),
    )
    .await;
    let resumed_servers = resumed
        .start_mcp(options(
            &case,
            entries(
                &case,
                json!({ "fixture": server_json(&case, "again", "resources") }),
            ),
            SandboxMode::DangerFullAccess,
            Arc::new(Wrapping::default()),
        ))
        .expect("start");
    wait_for(&resumed_servers, "the second server", |states| {
        connected(&states[0], 1)
    })
    .await;
    model.respond(vec![faux_assistant_message("again")]);
    resumed.prompt("again", Vec::new()).await.expect("prompt");
    assert_eq!(model.requests()[2], resource_fixture_tools());
    // Nothing was added or removed: the resumed session rebuilt the set.
    assert_eq!(
        logged_tool_changes(&resumed.session_file().expect("persisted")).len(),
        1
    );
}

// Bake's model-visible rule: a session with no MCP server, and one whose
// server reports neither resources nor instructions, send the same system
// prompt; only a server that reports instructions adds the `mcp_servers`
// section, bounded to its first line and attributed to the server.
async fn the_prompt_changes_only_for_a_server_that_reports_something(case: Arc<Case>) {
    let plain_model = Model::new();
    let plain = session(&case, &plain_model, None).await;
    plain_model.respond(vec![faux_assistant_message("one")]);
    plain.prompt("go", Vec::new()).await.expect("prompt");
    let without_mcp = plain_model.prompts()[0].clone();
    assert!(!without_mcp.contains("mcp_servers"), "{without_mcp}");

    let quiet_model = Model::new();
    let quiet = session(&case, &quiet_model, None).await;
    let quiet_servers = quiet
        .start_mcp(options(
            &case,
            entries(&case, json!({ "fixture": server_json(&case, "quiet", "") })),
            SandboxMode::DangerFullAccess,
            Arc::new(Wrapping::default()),
        ))
        .expect("start");
    wait_for(&quiet_servers, "the quiet server", |states| {
        connected(&states[0], 1)
    })
    .await;
    quiet_model.respond(vec![faux_assistant_message("two")]);
    quiet.prompt("go", Vec::new()).await.expect("prompt");
    assert_eq!(quiet_model.requests()[0], FIXTURE_TOOLS);
    // Byte-identical: a server without resources or instructions adds no
    // tool and no section.
    assert_eq!(quiet_model.prompts()[0], without_mcp);

    let talkative_model = Model::new();
    let talkative = session(&case, &talkative_model, None).await;
    let talkative_servers = talkative
        .start_mcp(options(
            &case,
            entries(
                &case,
                json!({ "fixture": server_json(&case, "talkative", "instructions") }),
            ),
            SandboxMode::DangerFullAccess,
            Arc::new(Wrapping::default()),
        ))
        .expect("start");
    wait_for(&talkative_servers, "the talkative server", |states| {
        connected(&states[0], 1)
    })
    .await;
    talkative_model.respond(vec![faux_assistant_message("three")]);
    talkative.prompt("go", Vec::new()).await.expect("prompt");
    // Instructions alone add no tool: the server declared no resources.
    assert_eq!(talkative_model.requests()[0], FIXTURE_TOOLS);
    let prompt = talkative_model.prompts()[0].clone();
    let section = prompt
        .split("<mcp_servers>\n")
        .nth(1)
        .and_then(|rest| rest.split("\n</mcp_servers>").next())
        .unwrap_or_else(|| panic!("no mcp_servers section in {prompt}"));
    let lines: Vec<&str> = section.lines().collect();
    assert!(
        lines[0].contains("servers' own text, not the user's"),
        "{section}"
    );
    assert_eq!(
        lines[1],
        format!(
            "- mcp__fixture: {}",
            fixture::FIXTURE_INSTRUCTIONS_FIRST_LINE
        )
    );
    assert_eq!(lines.len(), 2, "{section}");
    // The rest of the server's instructions never reach the model.
    assert!(!prompt.contains("guidance guidance"), "{prompt}");
    // Nothing else in the prompt changed.
    assert_eq!(
        prompt.replace(&format!("\n\n<mcp_servers>\n{section}\n</mcp_servers>"), ""),
        without_mcp
    );
}

// Scope 12: a resource call goes through the same pipeline as any other
// tool, so a plugin veto blocks it before the server is asked, with no
// approval in between.
async fn a_resource_call_runs_through_the_veto_pipeline(case: Arc<Case>) {
    struct Guard;

    impl bake_coding_agent::plugins::Plugin for Guard {
        fn manifest(&self) -> bake_coding_agent::plugins::PluginManifest {
            bake_coding_agent::plugins::PluginManifest::new(
                "resource-guard",
                "1",
                [bake_coding_agent::mcp::READ_MCP_RESOURCE_TOOL],
            )
            .expect("manifest")
        }

        fn on_tool_call<'a>(
            &'a self,
            _call: bake_coding_agent::plugins::ToolCallView<'a>,
        ) -> bake_agent::BoxFuture<'a, Result<bake_coding_agent::plugins::ToolCallVeto, String>>
        {
            Box::pin(async {
                Ok(bake_coding_agent::plugins::ToolCallVeto::Block {
                    reason: "resource reads are not allowed here".into(),
                    terminate: false,
                })
            })
        }
    }

    let model = Model::new();
    let mut plugins = bake_coding_agent::plugins::PluginRegistry::new();
    plugins.register(Arc::new(Guard), 0).expect("register");
    let session = session_with_plugins(&case, &model, plugins).await;
    let servers = session
        .start_mcp(options(
            &case,
            entries(
                &case,
                json!({ "fixture": server_json(&case, "fixture", "resources") }),
            ),
            SandboxMode::DangerFullAccess,
            Arc::new(Wrapping::default()),
        ))
        .expect("start");
    wait_for(&servers, "the server", |states| connected(&states[0], 1)).await;
    model.respond(vec![
        calls(&[
            (
                "read_mcp_resource",
                json!({ "server": "fixture", "uri": "docs://intro" }),
                "read-1",
            ),
            ("list_mcp_resources", json!({}), "list-1"),
        ]),
        faux_assistant_message("done"),
    ]);
    session.prompt("read", Vec::new()).await.expect("prompt");
    assert_eq!(
        result_text(&session, "read-1"),
        ("resource reads are not allowed here".into(), true)
    );
    // The listing the manifest does not name still runs.
    assert!(!result_text(&session, "list-1").1);
    let events = case.events("fixture");
    assert!(
        !events.iter().any(|event| event.starts_with("read ")),
        "the blocked call reached the server: {events:?}"
    );
    assert!(
        events.iter().any(|event| event == "resources/list"),
        "{events:?}"
    );
}

// `notifications/resources/list_changed` republishes nothing, as in Pi: the
// generation stays, and the next listing shows the server's new set.
async fn a_resource_list_change_republishes_nothing(case: Arc<Case>) {
    let model = Model::new();
    let session = session(&case, &model, None).await;
    let servers = session
        .start_mcp(options(
            &case,
            entries(
                &case,
                json!({ "fixture": server_json(&case, "fixture", "resources") }),
            ),
            SandboxMode::DangerFullAccess,
            Arc::new(Wrapping::default()),
        ))
        .expect("start");
    model.respond(vec![
        calls(&[("mcp__fixture__change_resources", json!({}), "change-1")]),
        calls(&[("list_mcp_resources", json!({}), "list-1")]),
        faux_assistant_message("done"),
    ]);
    session.prompt("change", Vec::new()).await.expect("prompt");
    assert_eq!(result_text(&session, "change-1"), ("added".into(), false));
    // The published generation is the first one still: no tool changed.
    assert!(connected(&servers.statuses()[0].state, 1));
    assert_eq!(servers.tool_names(), resource_fixture_tools());
    let listed = result_payload(&session, "list-1");
    let uris: Vec<&str> = listed["resources"]
        .as_array()
        .expect("resources")
        .iter()
        .filter_map(|resource| resource["uri"].as_str())
        .collect();
    assert_eq!(uris, ["docs://intro", "docs://logo", "docs://extra"]);
}
