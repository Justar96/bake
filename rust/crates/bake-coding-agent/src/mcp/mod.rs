//! MCP servers a session owns, and their tools.
//!
//! Adapted from Pi v1.3.0's MCP integration (`src/extensions/mcp/index.ts`,
//! `runtime.ts`, `tools.ts`, `config.ts`, and `src/core/mcp-servers.ts`; MIT,
//! see the crate `NOTICE`) over `bake-mcp`'s client and stdio owner, with
//! Bake's retained contracts: public tool names from Bake's
//! `packages/mcp/mcp-client` ([`names`]), the D24 sandbox, and the session's
//! [`WorkGate`].
//!
//! `bake-rs` reads its servers from `$BAKE_HOME/mcp.json` once the file
//! passes [`home`]'s owner rules, and starts them for every session it
//! builds unless `--no-mcp` is given ([`crate::cli`]).
//!
//! [`AgentSession::start_mcp`](crate::agent_session::AgentSession::start_mcp)
//! starts each enabled server of an [`McpOptions`] as a task the session's
//! gate owns. The task, in order:
//!
//! 1. admits the server under D24 before anything runs: a `url` server is
//!    refused (Streamable HTTP has no egress admission yet), and a stdio
//!    server's command is wrapped by the session's sandbox provider for the
//!    standing sandbox mode, or refused when no backend can enforce it. Its
//!    environment is Bake's scrubbed environment with the entry's `env` set
//!    over it;
//! 2. spawns it (`bake-mcp`'s [`spawn_stdio`](bake_mcp::spawn_stdio), whose
//!    process and tree belong to `bake-process`'s owner), and from then on
//!    holds the process and the client together, from before the first
//!    suspending `initialize`;
//! 3. initializes, lists the tools when the server declares the `tools`
//!    capability, and publishes them as one generation;
//! 4. re-lists on `notifications/tools/list_changed`, publishing a new
//!    generation only when the whole listing is accepted, and otherwise
//!    keeping the earlier one with the error in its status;
//! 5. ends when the session shuts down, setup fails, or the connection
//!    closes: it withdraws the server's tools, shuts the client down, then
//!    shuts the process down, retrying an incomplete shutdown until the
//!    server and its tree are gone. The task's gate permit is released only
//!    then, so session shutdown returns after every server is gone.
//!
//! # Lifetime and permission boundaries
//!
//! Each server is admitted once, when [`AgentSession::start_mcp`](crate::agent_session::AgentSession::start_mcp)
//! starts it, under the standing sandbox mode in force then, and keeps that
//! confinement until it ends; nothing restarts or reconnects it, and the
//! configuration is read only by the caller, before the start. A later
//! [`StandingPermissions::set`] reaches the calls that resolve a policy per
//! call (`bash`, `write`, `edit`), not a running server, as in Bake's
//! TypeScript, where the sandbox knob is resolved per call by the shell,
//! code mode, and file tools (`packages/shell/tool-bash/src/index.ts`
//! `resolveSandboxPolicy`, `packages/core/tools/src/ptc.ts`) and MCP servers
//! are spawned outside it (`packages/mcp/mcp-client/src/transport.ts`).
//! Confining a server at its start is Bake's native D24 admission; there
//! the TypeScript ran servers unconfined. MCP calls have no approval step:
//! no terminal permission package gates `mcp__` tools in the TypeScript's
//! `tools/pre-execute` pipeline (`packages/core/tools/src/index.ts`), and
//! MCP tools take no `sandbox_permissions` to escalate. A native plugin
//! (`crate::plugins`) whose manifest names an MCP tool's public name may
//! veto its calls, as for any tool.
//!
//! Each prompt declares the tools published when it starts, Pi's tool
//! loadout: the agent announces added and removed tools in a system message
//! the session file records, so a resumed session's requests are rebuilt
//! from the file and declare nothing stale. A tool call keeps the client of
//! the generation it was declared with; Esc ends that call's request only
//! (`notifications/cancelled`), never the server. The first prompt waits up
//! to [`McpTimings::startup_wait`] for servers still connecting, as Pi's
//! `waitForDirectServers` does.
//!
//! A `codemode` script ([`crate::tools::codemode`]) calls MCP tools like
//! any other: each tool declares Pi's `CallToolResult` output schema
//! (`tools::mcp_result_schema`), so a script receives the result as the
//! server sent it, and its server is its namespace
//! ([`McpServers::tool_namespace`]).
//!
//! Not implemented: Streamable HTTP servers, OAuth, reconnecting a closed
//! server (its tools are withdrawn instead), resources and their tools,
//! prompts, server instructions and Pi's `mcp_servers` prompt section, the
//! `codemode`, `codemode-deferred`, and `deferred` exposures and tool search,
//! progress updates, the `/mcp` command, and the server log file.

pub mod config;
pub mod home;
pub mod names;
mod server;
pub(crate) mod tools;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bake_agent::AgentTool;
use bake_sandbox::SandboxProvider;
use tokio::sync::{Notify, watch};

pub use config::{
    MCP_CONFIG_FILE, McpConfig, McpExposure, McpServerEntry, McpServerSettings, McpStdioConfig,
    McpTransportConfig, load_mcp_config, parse_mcp_config,
};
pub use home::load_home_mcp_config;
pub use names::{MCP_TOOL_PREFIX, mcp_namespace, public_tool_name};
pub use tools::{MAX_DECLARATION_BYTES, MAX_TOOLS_PER_SERVER, MCP_OUTPUT_MAX_BYTES};

use crate::owned_work::WorkGate;
use crate::permissions::StandingPermissions;

/// How long the session waits for its servers and their processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpTimings {
    /// How long the first prompt waits for servers still connecting. Pi's
    /// `DEFAULT_STARTUP_WAIT_MS`: 10 s.
    pub startup_wait: Duration,
    /// How long a server may take to exit once its standard input closes.
    /// Default: 500 ms, as Pi.
    pub stdin_close_grace: Duration,
    /// How long after `SIGTERM` before `SIGKILL` (Unix). Default: 2 s.
    pub terminate_grace: Duration,
    /// How long each shutdown attempt waits for a killed server, and then
    /// for its tree, before it tries again. Default: 2 s.
    pub kill_wait: Duration,
}

impl Default for McpTimings {
    fn default() -> Self {
        Self {
            startup_wait: Duration::from_secs(10),
            stdin_close_grace: Duration::from_millis(500),
            terminate_grace: Duration::from_secs(2),
            kill_wait: Duration::from_secs(2),
        }
    }
}

/// The user's tool restrictions, applied to every MCP tool when a prompt
/// declares it, so a server's later listings are restricted too: Pi
/// v1.3.0's `_isAllowedTool` and `_isActivatable` without tool search
/// ([`crate::tools::index::dynamic_tool_restrictions`]). A tool left out is
/// neither declared nor callable; it stays published, so the server and its
/// generations are unchanged. The default restricts nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpToolFilter {
    allow: Option<Vec<String>>,
    exclude: Vec<String>,
}

impl McpToolFilter {
    /// Tools matching `allow` (names and `*` patterns), when given, and
    /// matching no `exclude` entry.
    pub fn new(allow: Option<Vec<String>>, exclude: Vec<String>) -> Self {
        Self { allow, exclude }
    }

    /// Whether the public tool name `name` may be declared.
    pub fn allows(&self, name: &str) -> bool {
        use crate::tools::index::tool_name_matcher;
        !tool_name_matcher(&self.exclude)(name)
            && self
                .allow
                .as_deref()
                .is_none_or(|allow| tool_name_matcher(allow)(name))
    }

    /// Whether no tool can be declared at all: an empty allowlist.
    pub fn allows_none(&self) -> bool {
        self.allow.as_ref().is_some_and(Vec::is_empty)
    }
}

/// What a session needs to run MCP servers.
#[derive(Clone)]
pub struct McpOptions {
    /// The servers, in configuration order. Disabled entries are listed and
    /// never started.
    pub servers: Vec<McpServerEntry>,
    /// The session's standing permission: its sandbox mode at each server's
    /// start decides how that server is confined (D24).
    pub standing: Arc<StandingPermissions>,
    /// The provider that confines each server under that mode; the one the
    /// session's `bash` uses.
    pub sandbox: Arc<dyn SandboxProvider>,
    /// Waits and shutdown bounds.
    pub timings: McpTimings,
    /// Which published tools each prompt may declare.
    pub tool_filter: McpToolFilter,
}

impl std::fmt::Debug for McpOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpOptions")
            .field("servers", &self.servers)
            .field("standing", &self.standing)
            .field("sandbox", &self.sandbox.name())
            .field("timings", &self.timings)
            .field("tool_filter", &self.tool_filter)
            .finish()
    }
}

/// Where a server is in its lifetime.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpServerState {
    /// Configured with `enabled: false`; never started.
    Disabled,
    /// Being admitted, spawned, initialized, or listed.
    Connecting,
    /// Serving its tools.
    Connected {
        /// Tools published from its current generation.
        tools: usize,
        /// Accepted listings so far, the first included.
        generation: u64,
        /// Why the last re-listing was not published, if it was not; the
        /// earlier generation stays.
        refresh_error: Option<String>,
    },
    /// Refused before anything ran: an unsupported transport, a sandbox
    /// mode no backend can enforce, or an environment value that cannot be
    /// resolved.
    Refused {
        /// Why.
        reason: String,
    },
    /// Started but not served: a failed spawn, initialization, or first
    /// listing, or a listing refused whole. Its process is gone.
    Failed {
        /// Why, with the end of its standard error.
        error: String,
    },
    /// Its connection closed after it was connected; its tools are
    /// withdrawn and its process is gone.
    Disconnected {
        /// Why, with the end of its standard error.
        error: String,
    },
    /// Its tools are withdrawn and its process is being stopped; `true` once
    /// a shutdown attempt found the server or its tree still running.
    Stopping {
        /// Whether an attempt was incomplete and is being retried.
        retrying: bool,
    },
    /// Stopped by session shutdown; its process is gone.
    Closed,
}

impl McpServerState {
    /// Whether the server may still publish tools for the first prompt.
    fn is_starting(&self) -> bool {
        matches!(self, Self::Connecting)
    }
}

/// One server's name and state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerStatus {
    /// The configured name.
    pub name: String,
    /// Its state.
    pub state: McpServerState,
}

struct Slot {
    entry: McpServerEntry,
    state: McpServerState,
    tools: Vec<Arc<AgentTool>>,
}

struct Shared {
    slots: Mutex<Vec<Slot>>,
    /// Bumped on every change of a state or a generation.
    changes: watch::Sender<u64>,
    /// Built-in tool names no MCP tool may take.
    reserved: Vec<String>,
    timings: McpTimings,
    /// Which published tools a prompt declares.
    filter: McpToolFilter,
    /// Set by the first prompt's wait.
    waited: AtomicBool,
    /// Ends that wait early: the session's abort.
    interrupt: Notify,
}

/// A session's MCP servers: their states and the tools they publish.
/// Cloning shares them.
#[derive(Clone)]
pub struct McpServers {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for McpServers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServers")
            .field("servers", &self.statuses())
            .finish()
    }
}

impl McpServers {
    pub(crate) fn new(
        servers: &[McpServerEntry],
        reserved: Vec<String>,
        timings: McpTimings,
        filter: McpToolFilter,
    ) -> Self {
        let slots = servers
            .iter()
            .map(|entry| Slot {
                entry: entry.clone(),
                state: if entry.settings.enabled {
                    McpServerState::Connecting
                } else {
                    McpServerState::Disabled
                },
                tools: Vec::new(),
            })
            .collect();
        Self {
            shared: Arc::new(Shared {
                slots: Mutex::new(slots),
                changes: watch::Sender::new(0),
                reserved,
                timings,
                filter,
                waited: AtomicBool::new(false),
                interrupt: Notify::new(),
            }),
        }
    }

    fn slots(&self) -> MutexGuard<'_, Vec<Slot>> {
        self.shared
            .slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn changed(&self) {
        self.shared.changes.send_modify(|count| *count += 1);
    }

    /// Every server's state, in configuration order.
    pub fn statuses(&self) -> Vec<McpServerStatus> {
        self.slots()
            .iter()
            .map(|slot| McpServerStatus {
                name: slot.entry.name.clone(),
                state: slot.state.clone(),
            })
            .collect()
    }

    /// The public names of the tools published now, in server order,
    /// whether or not the [`McpToolFilter`] lets a prompt declare them.
    pub fn tool_names(&self) -> Vec<String> {
        self.slots()
            .iter()
            .flat_map(|slot| slot.tools.iter().map(|tool| tool.name.clone()))
            .collect()
    }

    /// Changes whenever a state or a generation changes.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.shared.changes.subscribe()
    }

    /// The tools published now that the [`McpToolFilter`] allows, in
    /// server order: what the next prompt declares.
    pub(crate) fn tools(&self) -> Vec<Arc<AgentTool>> {
        self.slots()
            .iter()
            .flat_map(|slot| slot.tools.iter().cloned())
            .filter(|tool| self.shared.filter.allows(&tool.name))
            .collect()
    }

    /// The namespace of the published tool `name` for the `codemode` tool:
    /// its server's [`mcp_namespace`], with the server's name as alias and
    /// its `description`. Bake reads no server instructions.
    pub(crate) fn tool_namespace(
        &self,
        name: &str,
    ) -> Option<crate::tools::codemode::CodemodeNamespace> {
        let slots = self.slots();
        let slot = slots
            .iter()
            .find(|slot| slot.tools.iter().any(|tool| tool.name == name))?;
        let server = &slot.entry.name;
        Some(crate::tools::codemode::CodemodeNamespace {
            namespace: bake_codemode::ToolNamespace {
                name: mcp_namespace(server),
                description: slot
                    .entry
                    .settings
                    .description
                    .as_deref()
                    .map(str::trim)
                    .filter(|description| !description.is_empty())
                    .map(str::to_owned),
                instructions: None,
            },
            aliases: vec![server.clone()],
        })
    }

    /// The public names of the tools published now that the filter allows:
    /// those the next prompt declares.
    pub fn declared_tool_names(&self) -> Vec<String> {
        self.tools().iter().map(|tool| tool.name.clone()).collect()
    }

    pub(crate) fn entry(&self, index: usize) -> Option<McpServerEntry> {
        self.slots().get(index).map(|slot| slot.entry.clone())
    }

    pub(crate) fn timings(&self) -> McpTimings {
        self.shared.timings
    }

    pub(crate) fn set_state(&self, index: usize, state: McpServerState) {
        if let Some(slot) = self.slots().get_mut(index) {
            slot.state = state;
        }
        self.changed();
    }

    /// Replaces server `index`'s tools with `tools` as one generation, or
    /// refuses them all when a name is reserved or published by another
    /// server; the earlier generation then stays.
    pub(crate) fn publish(
        &self,
        index: usize,
        tools: Vec<Arc<AgentTool>>,
        state: impl FnOnce(usize) -> McpServerState,
    ) -> Result<(), String> {
        {
            let mut slots = self.slots();
            for tool in &tools {
                if self.shared.reserved.contains(&tool.name) {
                    return Err(format!("tool {} is a built-in tool's name", tool.name));
                }
                let taken = slots.iter().enumerate().find(|(other, slot)| {
                    *other != index && slot.tools.iter().any(|known| known.name == tool.name)
                });
                if let Some((_, slot)) = taken {
                    return Err(format!(
                        "tool {} is already published by server \"{}\"",
                        tool.name, slot.entry.name
                    ));
                }
            }
            let Some(slot) = slots.get_mut(index) else {
                return Err("no such server".into());
            };
            slot.state = state(tools.len());
            slot.tools = tools;
        }
        self.changed();
        Ok(())
    }

    /// Withdraws server `index`'s tools: the next prompt declares them
    /// removed.
    pub(crate) fn withdraw(&self, index: usize, state: McpServerState) {
        if let Some(slot) = self.slots().get_mut(index) {
            slot.tools.clear();
            slot.state = state;
        }
        self.changed();
    }

    /// The first call waits until no server is still connecting, for at
    /// most [`McpTimings::startup_wait`], or until `aborted` is true after
    /// [`Self::interrupt_startup_wait`]; later calls return at once.
    pub(crate) async fn wait_for_startup(&self, aborted: impl Fn() -> bool) {
        if self.shared.waited.swap(true, Ordering::SeqCst) {
            return;
        }
        // [`validate`] refused a wait the clock cannot represent; `None`
        // would wait without a deadline rather than panic.
        let deadline = tokio::time::Instant::now().checked_add(self.shared.timings.startup_wait);
        let mut changes = self.subscribe();
        loop {
            let interrupted = self.shared.interrupt.notified();
            tokio::pin!(interrupted);
            // Registered before the checks, so an abort between them and the
            // wait is not missed.
            interrupted.as_mut().enable();
            changes.borrow_and_update();
            if aborted() || !self.slots().iter().any(|slot| slot.state.is_starting()) {
                return;
            }
            tokio::select! {
                changed = changes.changed() => if changed.is_err() { return },
                () = async {
                    match deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                } => return,
                () = &mut interrupted => {}
            }
        }
    }

    /// Wakes the startup wait so it rechecks its abort.
    pub(crate) fn interrupt_startup_wait(&self) {
        self.shared.interrupt.notify_waiters();
    }
}

/// Checks `options` before anything starts: the startup wait, and each
/// enabled stdio server's client and process options, with `bake-mcp`'s
/// own checks (`StdioOptions::validate`), so every wait gives a deadline
/// the clock can represent. `McpOptions` and `McpServerEntry` are public,
/// so their values need not have come through the configuration parser.
pub(crate) fn validate(options: &McpOptions) -> Result<(), String> {
    let wait = options.timings.startup_wait;
    if std::time::Instant::now().checked_add(wait).is_none() {
        let error = bake_mcp::OptionsError::DurationTooLong {
            field: "timings.startup_wait",
            value: wait,
        };
        return Err(format!("invalid MCP options: {error}"));
    }
    for entry in &options.servers {
        if !entry.settings.enabled || !matches!(entry.transport, McpTransportConfig::Stdio(_)) {
            continue;
        }
        server::stdio_options(entry, options.timings)
            .validate()
            .map_err(|error| {
                format!("invalid MCP options for server \"{}\": {error}", entry.name)
            })?;
    }
    Ok(())
}

/// Starts every enabled server of `options` as a task `work` owns.
/// `cwd` is the session's working directory, against which a server's `cwd`
/// resolves. Returns the handle and, if the gate refused a server's task,
/// why; that server is then [`McpServerState::Closed`].
pub(crate) fn start(
    options: McpOptions,
    cwd: std::path::PathBuf,
    reserved: Vec<String>,
    work: &WorkGate,
) -> (McpServers, Option<String>) {
    let McpOptions {
        servers: entries,
        standing,
        sandbox,
        timings,
        tool_filter,
    } = options;
    let servers = McpServers::new(&entries, reserved, timings, tool_filter);
    let mut refused = None;
    for (index, entry) in entries.iter().enumerate() {
        if !entry.settings.enabled {
            continue;
        }
        let context = server::ServerContext {
            index,
            servers: servers.clone(),
            standing: Arc::clone(&standing),
            sandbox: Arc::clone(&sandbox),
            cwd: cwd.clone(),
            work: work.clone(),
        };
        // The gate owns the task; its handle is not needed.
        match work.spawn(move |scope| server::run(context, scope)) {
            Ok(task) => drop(task),
            Err(error) => {
                servers.set_state(index, McpServerState::Closed);
                if refused.is_none() {
                    refused = Some(error.to_string());
                }
            }
        }
    }
    (servers, refused)
}

#[cfg(test)]
mod tests {
    use bake_agent::AgentToolResult;
    use serde_json::json;

    use super::*;

    fn tool(name: &str) -> Arc<AgentTool> {
        Arc::new(AgentTool::new(
            name,
            name,
            name,
            json!({}),
            |_, _, _, _, _ctx| Box::pin(async { Ok(AgentToolResult::text("x")) }),
        ))
    }

    fn entries(names: &[&str]) -> Vec<McpServerEntry> {
        let text = json!({
            "mcpServers": names.iter().map(|name| (name.to_string(), json!({ "command": "x" }))).collect::<serde_json::Map<_, _>>()
        });
        parse_mcp_config(&text.to_string(), std::path::Path::new("mcp.json")).servers
    }

    fn connected(count: usize) -> McpServerState {
        McpServerState::Connected {
            tools: count,
            generation: 1,
            refresh_error: None,
        }
    }

    // Bake `mcp-client.spec.ts`: an update that conflicts with a registered
    // name is rejected whole and the earlier tools stay.
    #[test]
    fn a_generation_with_a_taken_name_is_refused_whole() {
        let servers = McpServers::new(
            &entries(&["a", "a__b"]),
            vec!["read".into()],
            McpTimings::default(),
            McpToolFilter::default(),
        );
        servers
            .publish(0, vec![tool("mcp__a__b__c"), tool("mcp__a__x")], connected)
            .unwrap();
        // `a__b`'s `c` has the name `a`'s `b__c` took.
        let clash = servers.publish(
            1,
            vec![tool("mcp__a__b__d"), tool(&public_tool_name("a__b", "c"))],
            connected,
        );
        assert!(
            clash
                .unwrap_err()
                .contains("already published by server \"a\"")
        );
        assert_eq!(servers.tool_names(), ["mcp__a__b__c", "mcp__a__x"]);
        assert_eq!(servers.statuses()[1].state, McpServerState::Connecting);
        let reserved = servers.publish(1, vec![tool("read")], connected);
        assert!(reserved.unwrap_err().contains("built-in"));
        // A server may replace its own generation.
        servers
            .publish(0, vec![tool("mcp__a__y")], connected)
            .unwrap();
        assert_eq!(servers.tool_names(), ["mcp__a__y"]);
        servers.withdraw(0, McpServerState::Closed);
        assert!(servers.tool_names().is_empty());
    }

    fn options(servers: Vec<McpServerEntry>, timings: McpTimings) -> McpOptions {
        McpOptions {
            servers,
            standing: Arc::new(StandingPermissions::new(
                bake_sandbox::SandboxMode::DangerFullAccess,
                crate::permissions::ApprovalPolicy::Never,
                "custom",
                std::env::temp_dir(),
                None,
            )),
            sandbox: Arc::new(bake_sandbox::provider::UnavailableProvider::new(None)),
            timings,
            tool_filter: McpToolFilter::default(),
        }
    }

    // Public options bypass the parser, so waits the clock cannot represent
    // are refused before anything starts, with `bake-mcp`'s own checks.
    #[test]
    fn waits_the_clock_cannot_represent_are_refused() {
        assert_eq!(
            validate(&options(entries(&["a"]), McpTimings::default())),
            Ok(())
        );
        let huge = Duration::MAX;
        type Set = fn(&mut McpTimings, Duration);
        let cases: [(&str, Set); 4] = [
            ("timings.startup_wait", |t, d| t.startup_wait = d),
            ("stdin_close_grace", |t, d| t.stdin_close_grace = d),
            ("terminate_grace", |t, d| t.terminate_grace = d),
            ("kill_wait", |t, d| t.kill_wait = d),
        ];
        for (field, set) in cases {
            let mut timings = McpTimings::default();
            set(&mut timings, huge);
            let error = validate(&options(entries(&["a"]), timings)).unwrap_err();
            assert!(error.contains(field), "{field}: {error}");
        }
        let mut servers = entries(&["a"]);
        servers[0].settings.timeout = huge;
        let error = validate(&options(servers, McpTimings::default())).unwrap_err();
        assert!(
            error.contains("server \"a\"") && error.contains("request_timeout"),
            "{error}"
        );
        // A server that never starts is not checked.
        let mut servers = entries(&["a"]);
        servers[0].settings.timeout = huge;
        servers[0].settings.enabled = false;
        assert_eq!(validate(&options(servers, McpTimings::default())), Ok(()));
        // Zero waits are representable: the deadline is now.
        let zero = McpTimings {
            startup_wait: Duration::ZERO,
            stdin_close_grace: Duration::ZERO,
            terminate_grace: Duration::ZERO,
            kill_wait: Duration::ZERO,
        };
        assert_eq!(validate(&options(entries(&["a"]), zero)), Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn the_startup_wait_runs_once_and_ends_at_its_bound() {
        let servers = McpServers::new(
            &entries(&["a"]),
            Vec::new(),
            McpTimings::default(),
            McpToolFilter::default(),
        );
        let started = tokio::time::Instant::now();
        servers.wait_for_startup(|| false).await;
        assert_eq!(started.elapsed(), McpTimings::default().startup_wait);
        let again = tokio::time::Instant::now();
        servers.wait_for_startup(|| false).await;
        assert_eq!(again.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn the_startup_wait_ends_when_no_server_is_connecting() {
        let mut entries = entries(&["a", "off"]);
        entries[1].settings.enabled = false;
        let servers = McpServers::new(
            &entries,
            Vec::new(),
            McpTimings::default(),
            McpToolFilter::default(),
        );
        assert_eq!(servers.statuses()[1].state, McpServerState::Disabled);
        let waiter = {
            let servers = servers.clone();
            tokio::spawn(async move { servers.wait_for_startup(|| false).await })
        };
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(!waiter.is_finished());
        servers.set_state(0, McpServerState::Failed { error: "x".into() });
        tokio::time::timeout(Duration::from_millis(1), waiter)
            .await
            .expect("woken by the change")
            .unwrap();
    }

    // Pi's `_isAllowedTool` and `_isActivatable` without tool search: the
    // exclusions always apply, an allowlist must match, and a filtered
    // tool stays published but is not declared, for every generation.
    #[test]
    fn the_filter_restricts_every_generation_it_declares() {
        let filter = McpToolFilter::new(
            Some(vec!["read".into(), "mcp__a__k*".into()]),
            vec!["mcp__a__kill".into()],
        );
        assert!(filter.allows("mcp__a__keep") && !filter.allows("mcp__a__kill"));
        assert!(!filter.allows("mcp__a__other") && !filter.allows_none());
        assert!(McpToolFilter::new(Some(Vec::new()), Vec::new()).allows_none());
        assert!(McpToolFilter::default().allows("mcp__a__anything"));
        let servers = McpServers::new(&entries(&["a"]), Vec::new(), McpTimings::default(), filter);
        servers
            .publish(
                0,
                vec![
                    tool("mcp__a__keep"),
                    tool("mcp__a__kill"),
                    tool("mcp__a__other"),
                ],
                connected,
            )
            .unwrap();
        assert_eq!(
            servers.tool_names(),
            ["mcp__a__keep", "mcp__a__kill", "mcp__a__other"]
        );
        assert_eq!(servers.declared_tool_names(), ["mcp__a__keep"]);
        // A later listing is restricted the same way.
        servers
            .publish(
                0,
                vec![tool("mcp__a__kite"), tool("mcp__a__new")],
                connected,
            )
            .unwrap();
        assert_eq!(servers.declared_tool_names(), ["mcp__a__kite"]);
    }
}
