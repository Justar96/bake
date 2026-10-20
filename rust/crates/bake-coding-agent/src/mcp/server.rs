//! One server's lifetime, from admission to an awaited stop, as one task
//! the session's gate owns (see the [module documentation](super)).
//!
//! Adapted from Pi v1.3.0 `src/extensions/mcp/runtime.ts`
//! (`McpServerConnection`: `connectOnce`, `refreshTools`,
//! `handleClientClose`, `close`) and `createDefaultTransport`. Pi closes a
//! connection when its session's abort signal fires and lets the transport
//! end the process; here the same task that started the server stops it,
//! and its permit holds the session's shutdown until the server's process
//! and tree are gone.
//!
//! This module owns a stdio server and the part both transports share:
//! [`serve`], which initializes, lists, publishes, and watches one
//! connection. [`super::http`] owns a Streamable HTTP server's admitted
//! egress, connections, and reconnection over the same [`serve`].

use std::ffi::OsString;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use bake_ai::AbortSignal;
use bake_mcp::{
    ClientOptions, Connecting, Implementation, McpClient, SpawnError, StartError, StdioCommand,
    StdioOptions, StdioProcess, scrubbed_parent_environment, spawn_stdio,
};
use bake_sandbox::SandboxProvider;
use bake_sandbox::provider::confine;

use super::config::{McpServerEntry, McpStdioConfig, McpTransportConfig};
use super::tools::{Caller, ListedTool, bind_tools, plan_tools};
use super::{McpServerState, McpServers};
use crate::config_value::{
    is_command_config_value, missing_config_value_env_var_names, resolve_config_value_uncached,
};
use crate::owned_work::{TaskScope, WorkGate};
use crate::permissions::StandingPermissions;

/// The end of the server's standard error a failure quotes, Pi's
/// `STDERR_TAIL_CHARS`.
const STDERR_TAIL_CHARS: usize = 2_000;

/// The pause between incomplete shutdown attempts.
const RETRY_PAUSE: std::time::Duration = std::time::Duration::from_millis(50);

/// What one server's task needs.
pub(crate) struct ServerContext {
    pub(crate) index: usize,
    pub(crate) servers: McpServers,
    pub(crate) standing: Arc<StandingPermissions>,
    pub(crate) sandbox: Arc<dyn SandboxProvider>,
    pub(crate) cwd: PathBuf,
    pub(crate) work: WorkGate,
    /// What a Streamable HTTP server connects with ([`super::http`]).
    pub(crate) http: super::McpHttpOptions,
    /// The resolver its egress admission pins addresses from.
    pub(crate) resolver: super::egress::HostResolver,
}

/// How serving ended.
pub(super) enum End {
    /// The session is shutting down.
    Cancelled,
    /// Setup failed; nothing was published.
    Failed(String),
    /// The connection closed after it was connected.
    Disconnected(String),
}

/// The connection's owner at each stage, so whichever stage is interrupted
/// is the one shut down.
pub(super) enum Connection {
    Connecting(Box<Connecting>),
    Client(Arc<McpClient>),
}

/// `future`, unless `signal` aborts first; `None` drops the future.
pub(super) async fn unless_aborted<T>(
    signal: &AbortSignal,
    future: impl Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        biased;
        () = signal.cancelled() => None,
        value = future => Some(value),
    }
}

/// Runs one server until the session's shutdown or its own end, then stops
/// it and waits until its process and tree are gone.
pub(crate) async fn run(context: ServerContext, scope: TaskScope) {
    let signal = scope.signal().clone();
    let Some(entry) = context.servers.entry(context.index) else {
        return;
    };
    match &entry.transport {
        McpTransportConfig::Http(config) => {
            super::http::run(&context, &entry, config, &signal).await;
        }
        McpTransportConfig::Stdio(_) => run_stdio(context, &entry, &signal).await,
    }
}

/// Runs one stdio server: admission, spawn, serving, and an awaited stop.
async fn run_stdio(context: ServerContext, entry: &McpServerEntry, signal: &AbortSignal) {
    let servers = &context.servers;
    let index = context.index;
    let command = match unless_aborted(signal, admit(&context, entry)).await {
        None => return servers.set_state(index, McpServerState::Closed),
        Some(Err(refusal)) => return servers.set_state(index, refusal),
        Some(Ok(command)) => command,
    };
    let options = stdio_options(entry, servers.timings());
    // The spawn may suspend only before the process exists, so dropping it
    // leaves nothing running.
    let (process, connecting) = match unless_aborted(signal, spawn_stdio(command, options)).await {
        None => return servers.set_state(index, McpServerState::Closed),
        Some(Ok(started)) => started,
        Some(Err(SpawnError::Started { process, error })) => {
            let error = match *error {
                StartError::Client(error) => {
                    format!(
                        "MCP server \"{}\" started, but its client did not: {error}",
                        entry.name
                    )
                }
                StartError::MissingPipe(stream) => format!(
                    "MCP server \"{}\" started without a piped {stream}",
                    entry.name
                ),
                _ => format!(
                    "MCP server \"{}\" started, but its client did not",
                    entry.name
                ),
            };
            stop_process(&context, &process).await;
            return servers.set_state(
                index,
                McpServerState::Failed {
                    error: with_stderr(error, &process),
                },
            );
        }
        Some(Err(error)) => {
            return servers.set_state(
                index,
                McpServerState::Failed {
                    error: format!("MCP server \"{}\" failed to start: {error}", entry.name),
                },
            );
        }
    };
    // Owned together from here, before the first suspending `initialize`.
    let mut connection = Connection::Connecting(Box::new(connecting));
    let mut generation = 0_u64;
    let end = serve(&context, entry, signal, &mut connection, &mut generation).await;
    // No later prompt declares the tools of a server being stopped.
    servers.withdraw(index, McpServerState::Stopping { retrying: false });
    stop_client(&connection).await;
    stop_process(&context, &process).await;
    let state = match end {
        End::Cancelled => McpServerState::Closed,
        End::Failed(error) => McpServerState::Failed {
            error: with_stderr(error, &process),
        },
        End::Disconnected(error) => McpServerState::Disconnected {
            error: with_stderr(error, &process),
        },
    };
    servers.set_state(index, state);
}

/// The client options every transport's server runs with: Bake as the
/// client, and the entry's `timeout` as each request's deadline, which is
/// also what bounds a stalled `initialize`.
pub(super) fn client_options(entry: &McpServerEntry) -> ClientOptions {
    let mut client = ClientOptions::new(Implementation::new("bake", env!("CARGO_PKG_VERSION")));
    client.request_timeout = Some(entry.settings.timeout);
    client
}

/// The client and process options a stdio server runs with; checked by
/// [`super::validate`] before any server starts, and again by
/// `spawn_stdio`.
pub(crate) fn stdio_options(entry: &McpServerEntry, timings: super::McpTimings) -> StdioOptions {
    let mut options = StdioOptions::new(client_options(entry));
    options.stdin_close_grace = timings.stdin_close_grace;
    options.terminate_grace = timings.terminate_grace;
    options.kill_wait = timings.kill_wait;
    options
}

/// `error` with the end of the server's standard error, as Pi reports a
/// failed connection.
fn with_stderr(error: String, process: &StdioProcess) -> String {
    let tail = process.stderr().tail_text();
    let tail = tail.trim();
    if tail.is_empty() {
        return error;
    }
    let start = tail
        .char_indices()
        .rev()
        .nth(STDERR_TAIL_CHARS.saturating_sub(1))
        .map_or(0, |(index, _)| index);
    format!("{error}\n{}", &tail[start..])
}

/// Initializes, lists, publishes, and serves until the session's shutdown,
/// a failure, or the connection's end. `generation` counts the listings
/// this server has published, so a reconnected HTTP server's generations go
/// on from where the last connection left them.
pub(super) async fn serve(
    context: &ServerContext,
    entry: &McpServerEntry,
    signal: &AbortSignal,
    connection: &mut Connection,
    generation: &mut u64,
) -> End {
    let servers = &context.servers;
    let index = context.index;
    let name = &entry.name;
    let Connection::Connecting(connecting) = connection else {
        return End::Failed("the connection was already initialized".into());
    };
    let (client, mut events) = match unless_aborted(signal, connecting.initialize()).await {
        None => return End::Cancelled,
        Some(Err(error)) => {
            return End::Failed(format!("MCP server \"{name}\" failed to connect: {error}"));
        }
        Some(Ok(connected)) => connected,
    };
    let client = Arc::new(client);
    *connection = Connection::Client(Arc::clone(&client));
    // Taken before the first listing, so a change during it is not lost.
    let mut changes = client.tool_list_changes();
    let caller = Arc::new(Caller {
        server: name.clone(),
        client: Arc::clone(&client),
        work: context.work.clone(),
    });
    // Servers without the tools capability need not answer `tools/list`.
    let lists = client.server().capabilities.tools().is_some();
    match list(&caller, entry, signal, lists).await {
        None => return End::Cancelled,
        Some(Err(error)) => {
            return End::Failed(format!(
                "MCP server \"{name}\" failed to list its tools: {error}"
            ));
        }
        Some(Ok(tools)) => {
            let first = *generation + 1;
            if let Err(error) = servers.publish(index, tools, |count| McpServerState::Connected {
                tools: count,
                generation: first,
                refresh_error: None,
            }) {
                return End::Failed(format!(
                    "MCP server \"{name}\" offered tools that were refused: {error}"
                ));
            }
            *generation = first;
        }
    }
    let mut events_open = true;
    loop {
        tokio::select! {
            biased;
            () = signal.cancelled() => return End::Cancelled,
            reason = client.closed() => return End::Disconnected(
                format!("MCP server \"{name}\" disconnected: {reason}"),
            ),
            change = changes.changed() => {
                if change.is_none() {
                    // Closed: the next pass reports it.
                    continue;
                }
                match list(&caller, entry, signal, true).await {
                    None => return End::Cancelled,
                    Some(listed) => {
                        let published = listed.and_then(|tools| {
                            let next = *generation + 1;
                            servers
                                .publish(index, tools, |count| McpServerState::Connected {
                                    tools: count,
                                    generation: next,
                                    refresh_error: None,
                                })
                                .map(|()| *generation = next)
                        });
                        if let Err(error) = published {
                            keep_generation(servers, index, format!("failed to refresh tools: {error}"));
                        }
                    }
                }
            }
            event = events.recv(), if events_open => {
                // Log notifications and diagnostics are not recorded yet;
                // reading them keeps the client's queue moving.
                events_open = event.is_some();
            }
        }
    }
}

/// Records why a re-listing was not published; the earlier generation
/// stays.
fn keep_generation(servers: &McpServers, index: usize, error: String) {
    let state = servers
        .statuses()
        .get(index)
        .map(|status| status.state.clone());
    if let Some(McpServerState::Connected {
        tools, generation, ..
    }) = state
    {
        servers.set_state(
            index,
            McpServerState::Connected {
                tools,
                generation,
                refresh_error: Some(error),
            },
        );
    }
}

/// Lists and plans the server's tools; `None` when the session's shutdown
/// interrupted it.
async fn list(
    caller: &Arc<Caller>,
    entry: &McpServerEntry,
    signal: &AbortSignal,
    lists: bool,
) -> Option<Result<Vec<Arc<bake_agent::AgentTool>>, String>> {
    let tools = if lists {
        match unless_aborted(signal, caller.client.list_tools()).await? {
            Ok(tools) => tools,
            Err(error) => return Some(Err(error.to_string())),
        }
    } else {
        Vec::new()
    };
    let listed: Vec<ListedTool<'_>> = tools.iter().map(ListedTool::from).collect();
    Some(
        plan_tools(&entry.name, &entry.settings, &listed)
            .map(|planned| bind_tools(caller, planned)),
    )
}

/// Shuts the connection down at whichever stage it reached, and returns
/// once its driver task has ended and dropped both transport halves.
pub(super) async fn stop_client(connection: &Connection) {
    match connection {
        Connection::Connecting(connecting) => {
            connecting.shutdown().await;
        }
        Connection::Client(client) => {
            client.shutdown().await;
        }
    }
}

/// Shuts the process down, until the server and its tree are gone. An
/// incomplete shutdown keeps everything owned and is retried: each attempt
/// waits within [`McpTimings::kill_wait`](super::McpTimings).
async fn stop_process(context: &ServerContext, process: &StdioProcess) {
    let mut retrying = false;
    while process.shutdown().await.is_err() {
        if !retrying {
            retrying = true;
            context
                .servers
                .set_state(context.index, McpServerState::Stopping { retrying });
        }
        // Each attempt already waited `kill_wait`; this only keeps a zero
        // bound from spinning.
        tokio::time::sleep(RETRY_PAUSE).await;
    }
}

/// D24 admission: the command the server runs, or why it may not run. A
/// stdio server runs under the session's standing sandbox mode, wrapped by
/// the provider, and is refused when the mode confines and no backend can
/// enforce it.
async fn admit(
    context: &ServerContext,
    entry: &McpServerEntry,
) -> Result<StdioCommand, McpServerState> {
    let name = &entry.name;
    let config = match &entry.transport {
        McpTransportConfig::Stdio(config) => config,
        // `run` sends an HTTP server to `super::http` instead.
        McpTransportConfig::Http(_) => {
            return Err(McpServerState::Refused {
                reason: format!("MCP server \"{name}\" is not a stdio server"),
            });
        }
    };
    let environment =
        environment(name, config).map_err(|reason| McpServerState::Refused { reason })?;
    let program = expand_home(&config.command);
    let args: Vec<OsString> = config.args.iter().map(|arg| expand_home(arg)).collect();
    let cwd = context
        .cwd
        .join(expand_home(config.cwd.as_deref().unwrap_or(".")));
    // Read once: the server keeps this confinement for its whole life (see
    // the module documentation's lifetime boundaries).
    let policy = context.standing.policy();
    let argv: Vec<OsString> = std::iter::once(program.clone())
        .chain(args.iter().cloned())
        .collect();
    let confined = confine(context.sandbox.as_ref(), &argv, &policy)
        .await
        .map_err(|unavailable| McpServerState::Refused {
            reason: format!("MCP server \"{name}\" was not started: {unavailable}"),
        })?;
    let command = match confined {
        // `danger-full-access`: the server's own argv.
        None => StdioCommand::new(program).args(args).envs(environment),
        // The runner's variables go on top of the server's.
        Some(confined) => StdioCommand::new(confined.program)
            .args(confined.args)
            .envs(environment)
            .envs(confined.env),
    };
    Ok(command.current_dir(cwd))
}

/// Bake's scrubbed environment with the entry's values over it; a value
/// whose variables are unset is refused, as Pi's
/// `resolveConfigValueOrThrow` throws.
fn environment(name: &str, config: &McpStdioConfig) -> Result<Vec<(OsString, OsString)>, String> {
    let mut environment = scrubbed_parent_environment();
    for (key, value) in &config.env {
        if is_command_config_value(value) {
            return Err(format!(
                "MCP server \"{name}\" env \"{key}\" runs a command, which Bake does not run"
            ));
        }
        let missing = missing_config_value_env_var_names(value, None);
        if !missing.is_empty() {
            return Err(format!(
                "MCP server \"{name}\" env \"{key}\" references unset variables: {}",
                missing.join(", ")
            ));
        }
        let resolved = resolve_config_value_uncached(value, None).unwrap_or_default();
        environment.push((key.into(), resolved.into()));
    }
    Ok(environment)
}

/// Pi's `expandHome`: `~` and `~/…` (also `~\…` on Windows) name the home
/// directory.
fn expand_home(value: &str) -> OsString {
    let home = std::env::home_dir();
    let rest = value
        .strip_prefix("~/")
        .or_else(|| cfg!(windows).then(|| value.strip_prefix("~\\")).flatten());
    match (home, value, rest) {
        (Some(home), "~", _) => home.into_os_string(),
        (Some(home), _, Some(rest)) => home.join(rest).into_os_string(),
        _ => value.into(),
    }
}
