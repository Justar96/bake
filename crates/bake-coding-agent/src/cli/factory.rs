//! [`SessionFactory`]: the next session an interactive or RPC run switches
//! to, built as the first one was.

use super::*;

/// Where a replacement session comes from: Pi's `newSession` and
/// `switchSession`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionTarget {
    /// A new session in the current one's session directory and working
    /// directory, persisted when it is, in memory when it is not
    /// (`--no-session`).
    New,
    /// The session file at this path, which must exist, read as a session,
    /// and hold the session `id`.
    Open { path: PathBuf, id: String },
}

/// Where [`SessionFactory::build_replacement`] takes its store from.
enum Staged {
    Target(SessionTarget),
    /// A store already built, such as a fork's.
    Store(Box<SessionManager>),
}

/// A replacement session, built and not yet in use, and what its setup
/// warned about. Its MCP servers are not started yet: as in Pi's
/// `AgentSessionRuntime`, which tears the current session down
/// (`session_shutdown` stops its servers) before it creates the next
/// (`session_start` connects them), the caller shuts the running session
/// down, awaited, and then calls [`Replacement::start_servers`], so the two
/// sessions' servers never run at once. A replacement that is not used is
/// shut down and has started none.
pub struct Replacement {
    pub session: AgentSession,
    pub warnings: Vec<String>,
    servers: Option<crate::mcp::McpOptions>,
}

impl Replacement {
    /// Starts the replacement's MCP servers, once, after the session it
    /// replaces has been shut down; later calls start nothing. Fails only
    /// when the replacement was shut down first.
    pub fn start_servers(&mut self) -> Result<(), String> {
        start_servers(&self.session, self.servers.take())
    }

    /// Whether [`Self::start_servers`] has servers to start.
    pub fn has_pending_servers(&self) -> bool {
        self.servers.is_some()
    }
}

/// Builds the sessions `/new`, `/resume`, and `/fork` replace the running
/// one with,
/// as Pi's `createRuntime` does for `AgentSessionRuntime`: the same Bake
/// home, tools, flags, sandbox provider, and approver as at startup, with
/// the running session's live settings and model runtime, so a sign-in, a
/// refreshed model list, a `--api-key`, or a changed setting carries over.
///
/// The command line's session choice (`--continue`, `--resume`,
/// `--session`, `--session-id`, `--fork`), `--name`, and its prompts and
/// files belong to the first session and are never applied again. A new
/// session starts with the running session's model and thinking level,
/// and with the startup permission flags; a resumed one restores the model,
/// thinking level, and permission its file records, as `--continue` does
/// without `--model`, `--thinking`, or permission flags.
///
/// The replacement is built from its own [`SessionManager`]; the running
/// session's is never touched, so a failure leaves it, its file, and its
/// writer lock as they were.
#[derive(Clone)]
pub struct SessionFactory {
    pub(super) args: Args,
    pub(super) home: PathBuf,
    pub(super) user_home: Option<PathBuf>,
    pub(super) docs_command: String,
    pub(super) sandbox_provider: Arc<dyn SandboxProvider>,
    pub(super) approver: Option<Arc<dyn Approver>>,
    pub(super) plugins: WasmSetup,
    /// The interactive terminal shows its sessions ([`Build::drawn`]).
    pub(super) drawn: bool,
    /// The process's telecom environment ([`CliEnvironment::telecom_env`]).
    pub(super) telecom_env: crate::telecom::TelecomEnv,
}

impl std::fmt::Debug for SessionFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionFactory")
            .field("home", &self.home)
            .finish_non_exhaustive()
    }
}

impl SessionFactory {
    /// A factory for sessions set up as `args` describe, with settings and
    /// credentials from `home`, bash confined by `sandbox_provider`, and
    /// approvals asked of `approver` (none fails them closed). The
    /// command line's own setup ([`start_interactive`]) also names the
    /// user's home.
    pub fn new(
        args: Args,
        home: PathBuf,
        docs_command: String,
        sandbox_provider: Arc<dyn SandboxProvider>,
        approver: Option<Arc<dyn Approver>>,
    ) -> Self {
        Self {
            args,
            home,
            user_home: None,
            docs_command,
            sandbox_provider,
            approver,
            plugins: WasmSetup::default(),
            drawn: false,
            telecom_env: crate::telecom::TelecomEnv::from_process(),
        }
    }

    /// Runs the home's enabled Wasm plugins under `plugins`; without this,
    /// the factory has no helper command, so each enabled plugin blocks the
    /// tools it lists.
    pub fn with_plugins(mut self, plugins: WasmSetup) -> Self {
        self.plugins = plugins;
        self
    }

    /// Builds the replacement for `current`, which keeps running
    /// meanwhile; the caller shuts `current` down before it uses the
    /// replacement, and shuts the replacement down if it does not.
    pub async fn replace(
        &self,
        current: &AgentSession,
        target: SessionTarget,
    ) -> Result<Replacement, String> {
        let resume = matches!(target, SessionTarget::Open { .. });
        self.build_replacement(current, Staged::Target(target), resume)
            .await
    }

    /// Builds the session `/fork` or `/clone` replaces `current` with, Pi's
    /// runtime `fork` at `entry_id` in `position`
    /// ([`AgentSession::fork_session_manager`]), and returns it with the
    /// chosen message's text, for the editor, when forking before one. A
    /// fork that holds history restores
    /// the model, thinking level, and permission its path records, as
    /// `/resume` does; an empty fork, from a first message, starts as
    /// `/new` does. `current` and its file are not touched; a persisted
    /// fork's file may already be written when a later step fails.
    pub async fn fork(
        &self,
        current: &AgentSession,
        entry_id: &str,
        position: ForkPosition,
    ) -> Result<(Replacement, Option<String>), String> {
        let forked = current.fork_session_manager(entry_id, position).await?;
        let resume = !forked.session.build_session_context().messages.is_empty();
        let replacement = self
            .build_replacement(current, Staged::Store(Box::new(forked.session)), resume)
            .await?;
        Ok((replacement, forked.selected_text))
    }

    async fn build_replacement(
        &self,
        current: &AgentSession,
        staged: Staged,
        resume: bool,
    ) -> Result<Replacement, String> {
        let settings = current.settings();
        let registry = current.model_runtime().clone();
        let (place, persisted) = current.session_place();
        let cwd = current.cwd();
        let (model, thinking_level) = if resume {
            (None, None)
        } else {
            (current.model(), Some(current.thinking_level()))
        };
        let factory = self.clone();
        let loaded = tokio::task::spawn_blocking(move || {
            let mut warnings = Vec::new();
            let mut session = match staged {
                Staged::Target(SessionTarget::New) if persisted => {
                    SessionManager::create(&cwd, Some(&place), NewSessionOptions::default())
                        .map_err(|error| error.to_string())
                }
                Staged::Target(SessionTarget::New) => {
                    SessionManager::in_memory(Some(&cwd), NewSessionOptions::default(), Vec::new())
                        .map_err(|error| error.to_string())
                }
                Staged::Target(SessionTarget::Open { path, id }) => {
                    open_listed(&path, &id, &place, &cwd)
                }
                Staged::Store(session) => Ok(*session),
            }?;
            let resources = load_resources(
                &mut session,
                &settings,
                &factory.args,
                &factory.home,
                factory.user_home.clone(),
                &factory.plugins,
                !resume,
                &mut warnings,
            )?;
            Ok::<_, String>((session, resources, settings, warnings))
        })
        .await
        .map_err(|error| format!("Session setup failed: {error}"))??;
        let (session, resources, settings, warnings) = loaded;
        let Built { session, servers } = build_session(
            &self.args,
            Build {
                session,
                apis: Arc::clone(current.apis()),
                resources,
                settings,
                registry,
                model,
                thinking_level,
                home: self.home.clone(),
                docs_command: self.docs_command.clone(),
                sandbox_provider: Arc::clone(&self.sandbox_provider),
                approver: self.approver.clone(),
                telecom_env: self.telecom_env.clone(),
                role: Role::Root(Some(Box::new(self.clone()))),
                drawn: self.drawn,
            },
        )
        .await?;
        Ok(Replacement {
            session,
            warnings,
            servers,
        })
    }
}

/// Opens the session file `/resume` chose: one that exists, reads as a
/// session, holds the session `id`, and started in `cwd`, the running
/// session's working directory, which the terminal, its file mentions, and
/// its status line also show; a session from another directory is refused
/// rather than run with tools in a workspace the screen does not name.
/// [`SessionManager::open`] would start a new session at a missing path,
/// so a file removed since it was listed is refused rather than recreated.
/// Blocking.
fn open_listed(path: &Path, id: &str, place: &Path, cwd: &str) -> Result<SessionManager, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("Cannot open {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(format!("{} is not a session file", path.display()));
    }
    let session =
        SessionManager::open(path, Some(place), None).map_err(|error| error.to_string())?;
    if session.session_id() != id {
        return Err(format!(
            "{} no longer holds session {id}; choose it again",
            path.display()
        ));
    }
    let wanted = crate::session::paths::resolve_path_string(cwd);
    if crate::session::paths::resolve_path_string(session.cwd()) != wanted {
        return Err(format!(
            "Session {id} started in {}, not in {wanted}; resume it from there",
            session.cwd()
        ));
    }
    if !Path::new(session.cwd()).is_dir() {
        return Err(format!(
            "The session's working directory {} does not exist",
            session.cwd()
        ));
    }
    Ok(session)
}
