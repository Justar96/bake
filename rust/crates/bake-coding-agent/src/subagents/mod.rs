//! Continuable subagents: children a session starts on any available
//! model, which keep their own Pi session files, run in the background
//! under the parent's ownership, and exchange logged messages with it.
//!
//! A [`SubagentHost`] is one parent session's set of children, built over
//! the parent's [`WorkGate`] and [`JobRegistry`]. Each child is an
//! [`AgentSession`] of its own, built by the same composition as its parent
//! ([`ChildBuilder`], `cli::build_session`) without the parent tools, so a
//! child cannot delegate (depth 1). Its file is in the [`children_dir`] of
//! the parent's sessions directory, which session discovery does not list,
//! its header's `parentSession` names the parent's file, and its first
//! entry is a [`CHILD_ENTRY`] naming its parent and label. The child id is
//! the child's session id.
//!
//! - **Ownership.** A resident child is driven by one task of the parent's
//!   gate, so the parent's shutdown cancels its run, shuts the child down,
//!   and awaits it. A child's run also holds the parent registry's live
//!   count ([`JobRegistry::hold_live`]), so print mode waits for it as for a
//!   job.
//! - **States.** `running` while a run is in progress (or the child is
//!   being started), `idle` when resident between runs, and `ready` when
//!   saved in its file and not resident. At [`MAX_ACTIVE`] resident
//!   children the least recently used idle one with a file is evicted to
//!   `ready`; with none, the start is refused with 0.3's limit text.
//! - **Intercom.** A message is a [`messages::MESSAGE_TYPE`] custom message
//!   posted to the recipient's registry ([`JobRegistry::post`]), which the
//!   session delivers as it delivers job notices: inside a run at its next
//!   step, else by a turn its owner opens within the wake budget. A message
//!   to an idle child starts a run; one to a `ready` child resumes it from
//!   its file first. A parent messages only its children and a child only
//!   its parent.
//! - **Settlement.** A run ends once: when the child's turn finishes with
//!   no message waiting and none of its own jobs live, or when it is
//!   stopped. Its notice ([`messages::NOTICE_TYPE`]) is posted to the
//!   parent once, or handed to a foreground call instead.
//! - **Restart.** The parent's file is the record: `subagent` results name
//!   each child and the run they started, `send_message` results the runs
//!   they started, and notices the runs that settled. A resumed parent
//!   lists its children as `ready` ([`recover`]); a run with no settlement
//!   gets one lost-run notice with the next prompt ([`lost_runs`]) and is
//!   never re-run.
//!
//! Ported from Bake's TypeScript `packages/subagent/subagent/src/{continuation,
//! continuation-activation,lifecycle,inbox,list-children,depth}.ts` and
//! `index.ts` (`maxActiveSubagents` 8, `maxDepth` 1). Differences: one
//! process-local host per parent session instead of a service with
//! activations and lineage records; children live in Pi session files;
//! eviction of idle children rather than refusal at the cap; a message is
//! logged by the sender's tool call and by the recipient's custom message
//! when delivered, with no inbox file in between; and a child may run on
//! any available model ([`models`]).

pub mod messages;
pub mod models;
mod tools;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use bake_agent::BoxFuture;
use bake_agent::RunOutcome;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::agent_session::{AgentSession, SessionSelection};
use crate::jobs::JobRegistry;
use crate::model_registry::ModelRuntime;
use crate::owned_work::{OwnedTask, TaskScope, WorkGate};
use crate::session::SessionManager;
use crate::session::messages::CustomMessage;
use crate::settings::Settings;
use messages::StopReason;
use models::ChildRoute;

pub use tools::{CHILD_TOOL_NAMES, PARENT_TOOL_NAMES, child_tools, parent_tools};

/// Resident children at once, 0.3's `maxActiveSubagents`.
pub const MAX_ACTIVE: usize = 8;

/// The longest message one agent may send another, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// The subdirectory of a sessions directory that holds subagent files.
pub const CHILDREN_DIR: &str = "subagents";

/// The `customType` of the entry that starts a child's file.
pub const CHILD_ENTRY: &str = "bake.subagent.child";

/// The `details` key of a `subagent` result that records the child: its
/// `agentId`, `label`, route, and whether it ran in the `foreground`.
pub const SUBAGENT_DETAIL: &str = "subagent";

/// Where a child session comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildSource {
    /// A new session with this id and label.
    New {
        /// The child's session id.
        id: String,
        /// Its display label.
        label: String,
    },
    /// The saved child at this path.
    Open {
        /// Its session file.
        path: PathBuf,
    },
}

/// What [`ChildBuilder`] builds a child from.
pub struct ChildSpec {
    /// Its session.
    pub source: ChildSource,
    /// The child's way back to its parent, for its `send_message`.
    pub link: ParentLink,
    /// The route of a new child; a saved one restores its own.
    pub route: Option<ChildRoute>,
}

/// Builds a child session as its parent was built, with
/// [`child_tools`] in place of [`parent_tools`]. A new child's store is
/// created in the parent's sessions directory with `parentSession` set,
/// starting with a [`CHILD_ENTRY`] ([`new_child_store`]).
pub type ChildBuilder =
    Arc<dyn Fn(ChildSpec) -> BoxFuture<'static, Result<AgentSession, String>> + Send + Sync>;

/// The directory, under the parent's sessions directory `place`, that
/// holds subagent files. Session discovery does not descend into it, so
/// `--continue`, `/resume`, and session ids find only top-level sessions.
pub fn children_dir(place: &std::path::Path) -> PathBuf {
    place.join(CHILDREN_DIR)
}

/// A new child's store: in `place`'s [`children_dir`] with
/// `parentSession` naming `parent_file`, persisted when the parent is, and
/// starting with its [`CHILD_ENTRY`]. Blocking.
pub fn new_child_store(
    cwd: &str,
    place: &std::path::Path,
    persisted: bool,
    parent_id: &str,
    parent_file: Option<&std::path::Path>,
    id: &str,
    label: &str,
) -> Result<SessionManager, String> {
    let options = crate::session::NewSessionOptions {
        id: Some(id.to_owned()),
        parent_session: parent_file.map(|file| file.to_string_lossy().into_owned()),
    };
    let mut store = if persisted {
        SessionManager::create(cwd, Some(&children_dir(place)), options)
    } else {
        SessionManager::in_memory(Some(cwd), options, Vec::new())
    }
    .map_err(|error| error.to_string())?;
    store
        .append_custom_entry(
            CHILD_ENTRY,
            Some(json!({ "version": 1, "parentId": parent_id, "label": label })),
        )
        .map_err(|error| error.to_string())?;
    Ok(store)
}

/// What a [`SubagentHost`] is built over.
pub struct HostOptions {
    /// The parent's session id.
    pub parent_id: String,
    /// The parent's session file, when it persists.
    pub parent_file: Option<PathBuf>,
    /// The parent's registry: notices to the parent are posted to it, and
    /// running children hold its live count.
    pub jobs: JobRegistry,
    /// The parent's gate, which owns every resident child.
    pub work: WorkGate,
    /// The parent's published model and thinking level, which a child
    /// inherits.
    pub selection: SessionSelection,
    /// The models a child may choose from.
    pub registry: ModelRuntime,
    /// The settings a new child's default level follows.
    pub settings: Settings,
    /// Builds each child.
    pub builder: ChildBuilder,
    /// Children the parent's file records ([`recover`]).
    pub recovered: Recovered,
}

/// A child's status as `list_agents` shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Being started, or resumed from its file.
    Starting,
    /// A run is in progress.
    Running,
    /// Resident, between runs.
    Idle,
    /// Saved in its file, not resident.
    Ready,
    /// Its file is missing or unreadable.
    Unavailable,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Self::Starting | Self::Running => "running",
            Self::Idle => "idle",
            Self::Ready => "ready",
            Self::Unavailable => "diagnostic: unavailable",
        }
    }
}

/// One row of [`SubagentHost::list`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildRow {
    /// The child's id.
    pub id: String,
    /// Its status.
    pub status: Status,
    /// Its label.
    pub label: String,
    /// `provider/model`.
    pub route: String,
}

/// How a child's run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settlement {
    /// Its run.
    pub run: u64,
    /// Why it ended.
    pub stop: StopReason,
    /// The text of its last response, if any.
    pub closing: Option<String>,
    /// The failure, for a run that failed.
    pub diagnostic: Option<String>,
}

/// A started child.
pub struct Started {
    /// Its id.
    pub id: String,
    /// The run its task started.
    pub run: u64,
    /// Where its file is.
    pub file: Option<PathBuf>,
    /// The route it runs on.
    pub provider: String,
    /// The route it runs on.
    pub model: String,
    /// The level it runs at.
    pub thinking_level: String,
    /// For a foreground start, its settlement.
    pub settled: Option<oneshot::Receiver<Settlement>>,
}

/// One parent session's children. Cloning shares the host.
#[derive(Clone)]
pub struct SubagentHost {
    inner: Arc<HostInner>,
}

/// A child's way back to its parent, for its `send_message`. It does not
/// keep the parent's children alive.
#[derive(Clone)]
pub struct ParentLink {
    host: Weak<HostInner>,
    parent_id: String,
    child_id: String,
}

impl std::fmt::Debug for ParentLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParentLink")
            .field("parent_id", &self.parent_id)
            .field("child_id", &self.child_id)
            .finish()
    }
}

struct HostInner {
    parent_id: String,
    jobs: JobRegistry,
    work: WorkGate,
    selection: SessionSelection,
    registry: ModelRuntime,
    settings: Settings,
    builder: ChildBuilder,
    state: Mutex<HostState>,
}

#[derive(Default)]
struct HostState {
    children: Vec<Child>,
    /// The last run id issued or recorded.
    last_run: u64,
    /// Orders uses, for eviction.
    tick: u64,
}

struct Child {
    id: String,
    label: String,
    file: Option<PathBuf>,
    provider: String,
    model: String,
    status: Status,
    /// The current or last run.
    run: u64,
    last_used: u64,
    resident: Option<Resident>,
    /// Messages for a child being started, posted once it is resident.
    waiting: Vec<CustomMessage>,
    /// The task of an evicted child, which shuts it down and releases its
    /// file; a resume waits for it.
    retiring: Option<OwnedTask<()>>,
}

struct Resident {
    session: Arc<AgentSession>,
    commands: mpsc::UnboundedSender<Command>,
    /// The child's task, kept so an eviction can be awaited.
    task: OwnedTask<()>,
    /// The current run's control, which an interrupt cancels.
    control: Option<bake_agent::RunControl>,
    /// Whether the current run was interrupted.
    interrupted: bool,
}

enum Command {
    Run {
        input: Input,
        run: u64,
        waiter: Option<oneshot::Sender<Settlement>>,
    },
    Evict,
}

enum Input {
    Task(String),
    Mail,
}

fn lock(state: &Mutex<HostState>) -> MutexGuard<'_, HostState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn unknown(id: &str) -> String {
    let shown: String = id.chars().take(64).collect();
    format!("unknown agent {shown}; use list_agents to find a subagent id.")
}

fn unavailable(id: &str) -> String {
    format!("subagent \"{id}\" is unavailable")
}

fn limit_reached() -> String {
    format!(
        "subagent limit reached: {MAX_ACTIVE} subagents are already active, the most allowed at once. A notice arrives when one finishes; retry then, or do this work yourself."
    )
}

/// Refuses an empty or oversized message.
fn check_message(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Err("invalid message: expected a non-empty string".to_owned());
    }
    if text.len() > MAX_MESSAGE_BYTES {
        return Err(format!(
            "message is too long: {} bytes, the most is {MAX_MESSAGE_BYTES}",
            text.len()
        ));
    }
    Ok(())
}

impl HostState {
    fn child(&mut self, id: &str) -> Option<&mut Child> {
        self.children.iter_mut().find(|child| child.id == id)
    }

    fn next_run(&mut self) -> u64 {
        self.last_run = self.last_run.saturating_add(1);
        self.last_run
    }

    fn touch(&mut self) -> u64 {
        self.tick = self.tick.saturating_add(1);
        self.tick
    }

    /// Makes room for one more resident child: evicts the least recently
    /// used idle child that has a file, or refuses.
    fn make_room(&mut self) -> Result<(), String> {
        let resident = self
            .children
            .iter()
            .filter(|child| {
                matches!(
                    child.status,
                    Status::Starting | Status::Running | Status::Idle
                )
            })
            .count();
        if resident < MAX_ACTIVE {
            return Ok(());
        }
        let evictable = self
            .children
            .iter_mut()
            .filter(|child| child.status == Status::Idle && child.file.is_some())
            .min_by_key(|child| child.last_used)
            .ok_or_else(limit_reached)?;
        evictable.status = Status::Ready;
        if let Some(resident) = evictable.resident.take() {
            let _ = resident.commands.send(Command::Evict);
            evictable.retiring = Some(resident.task);
        }
        Ok(())
    }
}

/// What a dropped start or resume undoes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Undo {
    Nothing,
    /// Remove the child being started.
    Remove,
    /// Return the child being resumed to `ready`.
    Ready,
}

/// Undoes a start or resume whose future is dropped while the child is
/// built, so no record stays `running` with no task behind it.
struct Pending<'a> {
    host: &'a SubagentHost,
    id: &'a str,
    undo: Undo,
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        let mut state = lock(&self.host.inner.state);
        match self.undo {
            Undo::Nothing => {}
            Undo::Remove => state.children.retain(|child| child.id != self.id),
            Undo::Ready => {
                if let Some(child) = state.child(self.id) {
                    child.status = Status::Ready;
                    child.waiting.clear();
                }
            }
        }
    }
}

impl SubagentHost {
    /// A host over `options`, holding the children its parent's file
    /// records as `ready` (or unavailable when their file is gone).
    pub fn new(options: HostOptions) -> Self {
        let HostOptions {
            parent_id,
            parent_file: _,
            jobs,
            work,
            selection,
            registry,
            settings,
            builder,
            recovered,
        } = options;
        let children = recovered
            .children
            .into_iter()
            .map(|child| Child {
                status: if child.file.as_ref().is_some_and(|file| file.is_file()) {
                    Status::Ready
                } else {
                    Status::Unavailable
                },
                id: child.id,
                label: child.label,
                file: child.file,
                provider: child.provider,
                model: child.model,
                run: 0,
                last_used: 0,
                resident: None,
                waiting: Vec::new(),
                retiring: None,
            })
            .collect();
        Self {
            inner: Arc::new(HostInner {
                parent_id,
                jobs,
                work,
                selection,
                registry,
                settings,
                builder,
                state: Mutex::new(HostState {
                    children,
                    last_run: recovered.last_run,
                    tick: 0,
                }),
            }),
        }
    }

    /// The parent's session id.
    pub fn parent_id(&self) -> &str {
        &self.inner.parent_id
    }

    /// The models a child may run on: those with credentials.
    pub fn available_models(&self) -> Vec<bake_ai::Model> {
        self.inner.registry.snapshot().available()
    }

    /// The settings a new child's default level follows.
    pub fn settings(&self) -> &Settings {
        &self.inner.settings
    }

    /// Resolves `request` against the parent's current route and the
    /// available models ([`models::resolve`]).
    pub fn resolve(&self, request: &models::RouteRequest) -> Result<ChildRoute, String> {
        let selected = self.inner.selection.get();
        let available = self.available_models();
        models::resolve(
            request,
            selected
                .model
                .as_ref()
                .map(|model| (model, selected.thinking_level)),
            &available,
        )
    }

    fn link(&self, child_id: &str) -> ParentLink {
        ParentLink {
            host: Arc::downgrade(&self.inner),
            parent_id: self.inner.parent_id.clone(),
            child_id: child_id.to_owned(),
        }
    }

    /// Starts a child on `route` with `task`, labelled `label`. Its task
    /// carries the return guidance naming the parent. A foreground start
    /// returns a receiver for the run's settlement instead of a notice.
    pub async fn start(
        &self,
        label: &str,
        task: &str,
        route: ChildRoute,
        foreground: bool,
    ) -> Result<Started, String> {
        let id = crate::session::id::uuid_v7();
        let run = {
            let mut state = lock(&self.inner.state);
            state.make_room()?;
            let run = state.next_run();
            let tick = state.touch();
            state.children.push(Child {
                id: id.clone(),
                label: label.to_owned(),
                file: None,
                provider: route.model.provider.clone(),
                model: route.model.id.clone(),
                status: Status::Starting,
                run,
                last_used: tick,
                resident: None,
                waiting: Vec::new(),
                retiring: None,
            });
            run
        };
        let spec = ChildSpec {
            source: ChildSource::New {
                id: id.clone(),
                label: label.to_owned(),
            },
            link: self.link(&id),
            route: Some(route),
        };
        // A call dropped while the child is built leaves no record behind.
        let mut pending = Pending {
            host: self,
            id: &id,
            undo: Undo::Remove,
        };
        let built = (self.inner.builder)(spec).await;
        pending.undo = Undo::Nothing;
        drop(pending);
        let session = match built {
            Ok(session) => Arc::new(session),
            Err(error) => {
                lock(&self.inner.state)
                    .children
                    .retain(|child| child.id != id);
                return Err(error);
            }
        };
        let (waiter, settled) = if foreground {
            let (sender, receiver) = oneshot::channel();
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        let input = Input::Task(messages::task_with_return_guidance(
            &self.inner.parent_id,
            task,
        ));
        let file = session.session_file();
        let (provider, model) = session
            .model()
            .map(|model| (model.provider, model.id))
            .unwrap_or_default();
        let thinking_level = session.thinking_level().as_str().to_owned();
        if let Err(error) = self.make_resident(&id, Arc::clone(&session), file.clone()) {
            session.shutdown().await;
            lock(&self.inner.state)
                .children
                .retain(|child| child.id != id);
            return Err(error);
        }
        self.dispatch(&id, input, run, waiter);
        Ok(Started {
            id,
            run,
            file,
            provider,
            model,
            thinking_level,
            settled,
        })
    }

    /// Spawns the child's task on the parent's gate and records it
    /// resident and running.
    fn make_resident(
        &self,
        id: &str,
        session: Arc<AgentSession>,
        file: Option<PathBuf>,
    ) -> Result<(), String> {
        let (commands, receiver) = mpsc::unbounded_channel();
        let host = Arc::downgrade(&self.inner);
        let owned = Arc::clone(&session);
        let child_id = id.to_owned();
        let parent_jobs = self.inner.jobs.clone();
        let task = self
            .inner
            .work
            .spawn(move |scope| drive(scope, owned, receiver, host, child_id, parent_jobs))
            .map_err(|_| "The session is shut down.".to_owned())?;
        let mut state = lock(&self.inner.state);
        let tick = state.touch();
        let child = state
            .child(id)
            .ok_or_else(|| "The subagent was removed while it started.".to_owned())?;
        child.file = file.or(child.file.take());
        child.last_used = tick;
        child.resident = Some(Resident {
            session,
            commands,
            task,
            control: None,
            interrupted: false,
        });
        Ok(())
    }

    /// Sends the child a run and posts the messages that waited for it.
    fn dispatch(
        &self,
        id: &str,
        input: Input,
        run: u64,
        waiter: Option<oneshot::Sender<Settlement>>,
    ) {
        let mut state = lock(&self.inner.state);
        let Some(child) = state.child(id) else {
            return;
        };
        child.status = Status::Running;
        child.run = run;
        let waiting = std::mem::take(&mut child.waiting);
        if let Some(resident) = &mut child.resident {
            resident.interrupted = false;
            for message in waiting {
                let _ = resident.session.jobs().post(message);
            }
            resident.session.jobs().reset_wakes();
            let _ = resident.commands.send(Command::Run { input, run, waiter });
        }
    }

    /// Sends `text` from the parent to child `id`: steered into its run,
    /// or starting one when it is idle, after resuming it from its file
    /// when it is `ready`. Returns the message id, and the run it started.
    pub async fn message_child(
        &self,
        id: &str,
        text: &str,
    ) -> Result<(String, Option<u64>), String> {
        check_message(text)?;
        let message_id = crate::session::id::uuid_v7();
        let message = messages::agent_message(&message_id, &self.inner.parent_id, id, text);
        // What the message needs once the lock is released.
        enum Then {
            Delivered,
            Run(u64),
            Resume(u64, PathBuf),
        }
        let then = {
            let mut state = lock(&self.inner.state);
            let tick = state.touch();
            let child = state.child(id).ok_or_else(|| unknown(id))?;
            child.last_used = tick;
            match child.status {
                Status::Unavailable => return Err(unavailable(id)),
                Status::Starting => {
                    child.waiting.push(message);
                    Then::Delivered
                }
                Status::Running | Status::Idle => {
                    let idle = child.status == Status::Idle;
                    let resident = child.resident.as_ref().ok_or_else(|| unavailable(id))?;
                    let jobs = resident.session.jobs();
                    jobs.reset_wakes();
                    jobs.post(message).map_err(|_| unavailable(id))?;
                    if idle {
                        // Running from here, so a second message steers
                        // into this run instead of starting another.
                        child.status = Status::Running;
                        Then::Run(state.next_run())
                    } else {
                        Then::Delivered
                    }
                }
                Status::Ready => {
                    let file = child.file.clone().ok_or_else(|| unavailable(id))?;
                    child.status = Status::Starting;
                    child.waiting.push(message);
                    if let Err(error) = state.make_room() {
                        if let Some(child) = state.child(id) {
                            child.status = Status::Ready;
                            child.waiting.clear();
                        }
                        return Err(error);
                    }
                    Then::Resume(state.next_run(), file)
                }
            }
        };
        let (run, file) = match then {
            Then::Delivered => return Ok((message_id, None)),
            Then::Run(run) => {
                self.dispatch(id, Input::Mail, run, None);
                return Ok((message_id, Some(run)));
            }
            Then::Resume(run, file) => (run, file),
        };
        // A call dropped while the child resumes leaves it `ready`.
        let mut pending = Pending {
            host: self,
            id,
            undo: Undo::Ready,
        };
        // An evicted child's file is released once its task ends.
        let retiring = lock(&self.inner.state)
            .child(id)
            .and_then(|child| child.retiring.take());
        if let Some(retiring) = retiring {
            let _ = retiring.await;
        }
        let spec = ChildSpec {
            source: ChildSource::Open { path: file.clone() },
            link: self.link(id),
            route: None,
        };
        let built = (self.inner.builder)(spec).await;
        pending.undo = Undo::Nothing;
        drop(pending);
        let session = match built {
            Ok(session) => Arc::new(session),
            Err(_) => {
                if let Some(child) = lock(&self.inner.state).child(id) {
                    child.status = Status::Ready;
                    child.waiting.clear();
                }
                return Err(unavailable(id));
            }
        };
        if let Err(error) = self.make_resident(id, Arc::clone(&session), Some(file)) {
            session.shutdown().await;
            return Err(error);
        }
        self.dispatch(id, Input::Mail, run, None);
        Ok((message_id, Some(run)))
    }

    /// Requests that child `id`'s current run stop; it stays available.
    pub fn interrupt(&self, id: &str) -> Result<(), String> {
        let mut state = lock(&self.inner.state);
        let child = state.child(id).ok_or_else(|| unknown(id))?;
        if child.status == Status::Running
            && let Some(resident) = &mut child.resident
        {
            resident.interrupted = true;
            if let Some(control) = &resident.control {
                control.cancel();
            }
        }
        Ok(())
    }

    /// The children, in start order.
    pub fn list(&self) -> Vec<ChildRow> {
        lock(&self.inner.state)
            .children
            .iter()
            .map(|child| ChildRow {
                id: child.id.clone(),
                status: child.status,
                label: child.label.clone(),
                route: format!("{}/{}", child.provider, child.model),
            })
            .collect()
    }

    /// Children that are starting or running.
    pub fn running(&self) -> usize {
        lock(&self.inner.state)
            .children
            .iter()
            .filter(|child| matches!(child.status, Status::Starting | Status::Running))
            .count()
    }
}

impl ParentLink {
    /// The parent's id.
    pub fn parent_id(&self) -> &str {
        &self.parent_id
    }

    /// The child's own id.
    pub fn child_id(&self) -> &str {
        &self.child_id
    }

    /// Sends `text` from the child to `agent_id`, which must be its
    /// parent. Returns the message id.
    pub fn send(&self, agent_id: &str, text: &str) -> Result<String, String> {
        if agent_id != self.parent_id {
            let shown: String = agent_id.chars().take(64).collect();
            return Err(format!(
                "agent \"{shown}\" is not your parent; a subagent can message only its parent, {}",
                self.parent_id
            ));
        }
        check_message(text)?;
        let lost = || "direct parent is not live; the message was not delivered".to_owned();
        let host = self.host.upgrade().ok_or_else(lost)?;
        let message_id = crate::session::id::uuid_v7();
        host.jobs
            .post(messages::agent_message(
                &message_id,
                &self.child_id,
                &self.parent_id,
                text,
            ))
            .map_err(|_| lost())?;
        Ok(message_id)
    }
}

/// What a run returned.
type RunResult = Result<RunOutcome, String>;

/// A resident child's task: runs what it is sent, one run at a time, until
/// it is evicted or the parent's gate closes; then shuts the child down.
async fn drive(
    scope: TaskScope,
    session: Arc<AgentSession>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    host: Weak<HostInner>,
    id: String,
    parent_jobs: JobRegistry,
) {
    loop {
        let command = tokio::select! {
            command = commands.recv() => command,
            () = scope.signal().cancelled() => None,
        };
        let Some(Command::Run { input, run, waiter }) = command else {
            break;
        };
        // Held until the notice is posted, so an owner waiting for live
        // work sees the notice before the count drops.
        let live = parent_jobs.hold_live();
        let Some(settlement) = activate(&scope, &session, input, run, &host, &id).await else {
            break;
        };
        match waiter {
            // A dropped foreground call wants no notice.
            Some(waiter) => drop(waiter.send(settlement)),
            None => {
                let notice = messages::settlement_notice(
                    &id,
                    settlement.run,
                    settlement.stop,
                    settlement.closing.as_deref(),
                );
                let _ = parent_jobs.post(notice);
            }
        }
        drop(live);
        if scope.signal().aborted() {
            break;
        }
    }
    session.shutdown().await;
    if let Some(host) = host.upgrade() {
        let mut state = lock(&host.state);
        if let Some(child) = state.child(&id)
            && child
                .resident
                .as_ref()
                .is_some_and(|resident| Arc::ptr_eq(&resident.session, &session))
        {
            child.resident = None;
            child.status = if child.file.is_some() {
                Status::Ready
            } else {
                Status::Unavailable
            };
        }
    }
}

/// What a run's end check decided.
enum Next {
    /// A message waits: deliver it in this run.
    Deliver,
    /// The child's own jobs are live: wait for them.
    Wait,
    /// The run is over; the child is idle.
    Settle,
}

/// One run: the task or the waiting messages, then every message that
/// arrives and every notice of the child's own jobs, until the child's
/// turn ends with nothing left or the run is stopped. `None` when the
/// parent's gate closed; the run was stopped and nothing is reported.
async fn activate(
    scope: &TaskScope,
    session: &AgentSession,
    input: Input,
    run: u64,
    host: &Weak<HostInner>,
    id: &str,
) -> Option<Settlement> {
    let control = bake_agent::RunControl::default();
    {
        let host = host.upgrade()?;
        let mut state = lock(&host.state);
        if let Some(resident) = state.child(id).and_then(|child| child.resident.as_mut()) {
            resident.control = Some(control.clone());
        }
    }
    let before = session.messages().len();
    let mut result: RunResult = match input {
        Input::Task(text) => {
            owned_run(
                scope,
                &control,
                session.prompt_with_control(&text, Vec::new(), control.clone()),
            )
            .await
        }
        Input::Mail => {
            owned_run(
                scope,
                &control,
                session.deliver_job_notices_with_control(control.clone()),
            )
            .await
        }
    };
    let mut events = session.jobs().subscribe();
    loop {
        if scope.signal().aborted() {
            return None;
        }
        let finished = matches!(result, Ok(RunOutcome::FinishedTurn));
        let next = {
            let host = host.upgrade()?;
            let mut state = lock(&host.state);
            let child = state.child(id)?;
            let interrupted = child
                .resident
                .as_ref()
                .is_some_and(|resident| resident.interrupted);
            let jobs = session.jobs();
            if finished && !interrupted && jobs.has_undelivered() {
                Next::Deliver
            } else if finished && !interrupted && jobs.live() > 0 {
                Next::Wait
            } else {
                if interrupted && finished {
                    result = Ok(RunOutcome::Cancelled);
                }
                child.status = Status::Idle;
                if let Some(resident) = &mut child.resident {
                    resident.control = None;
                    resident.interrupted = false;
                }
                Next::Settle
            }
        };
        match next {
            Next::Deliver => {
                result = owned_run(
                    scope,
                    &control,
                    session.deliver_job_notices_with_control(control.clone()),
                )
                .await;
            }
            Next::Wait => {
                tokio::select! {
                    _ = events.changed() => {}
                    () = scope.signal().cancelled() => return None,
                    _ = control.wait_stopped() => {}
                }
            }
            Next::Settle => break,
        }
    }
    Some(settle(session, before, run, result))
}

/// Runs `work` until it ends; when the parent's gate closes first, stops
/// it and waits for it to end.
async fn owned_run(
    scope: &TaskScope,
    control: &bake_agent::RunControl,
    work: impl std::future::Future<Output = RunResult>,
) -> RunResult {
    tokio::pin!(work);
    tokio::select! {
        result = &mut work => result,
        () = scope.signal().cancelled() => {
            control.cancel();
            work.await
        }
    }
}

/// The settlement of a run that returned `result`, from the last response
/// the run produced.
fn settle(session: &AgentSession, before: usize, run: u64, result: RunResult) -> Settlement {
    let messages = session.messages();
    let assistant = messages
        .get(before.min(messages.len())..)
        .unwrap_or_default()
        .iter()
        .rev()
        .find_map(|message| message.as_assistant().cloned());
    let closing = assistant
        .as_ref()
        .map(bake_ai::utils::text::assistant_text)
        .filter(|text| !text.trim().is_empty());
    let (stop, diagnostic) = match result {
        Ok(RunOutcome::FinishedTurn) => match assistant.as_ref().map(|message| message.stop_reason)
        {
            Some(bake_ai::StopReason::Length) => (StopReason::MaxTokens, None),
            Some(bake_ai::StopReason::Error) => (
                StopReason::Error,
                assistant
                    .as_ref()
                    .and_then(|message| message.error_message.clone()),
            ),
            Some(bake_ai::StopReason::Aborted) => (StopReason::Aborted, None),
            _ => (StopReason::Completed, None),
        },
        Ok(RunOutcome::Cancelled | RunOutcome::BudgetExhausted { .. }) => {
            (StopReason::Aborted, None)
        }
        Ok(RunOutcome::Failed { message }) | Err(message) => (StopReason::Error, Some(message)),
    };
    Settlement {
        run,
        stop,
        closing,
        diagnostic,
    }
}

/// A child the parent's file records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredChild {
    /// Its id.
    pub id: String,
    /// Its label.
    pub label: String,
    /// Its file.
    pub file: Option<PathBuf>,
    /// Its route.
    pub provider: String,
    /// Its route.
    pub model: String,
}

/// What the parent's file records about its children ([`recover`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recovered {
    /// Each child, in start order.
    pub children: Vec<RecoveredChild>,
    /// The highest run id recorded, so new runs never reuse one.
    pub last_run: u64,
}

/// A run the parent's file records as started.
struct RunRecord {
    agent: String,
    run: u64,
    on_branch: bool,
}

/// The `subagent` and `send_message` results and notices of `session`,
/// as children, started runs, and settled runs.
fn project(session: &SessionManager) -> (Recovered, Vec<RunRecord>, Vec<(String, u64)>) {
    let branch: std::collections::BTreeSet<&str> = session
        .branch_entries(None)
        .iter()
        .map(|entry| entry.id())
        .collect();
    let mut recovered = Recovered::default();
    let mut runs = Vec::new();
    let mut settled = Vec::new();
    for entry in session.entries() {
        let on_branch = branch.contains(entry.id());
        match entry.entry_type() {
            "message" => {
                let Some(message) = entry.get("message") else {
                    continue;
                };
                // A failed foreground start still records its child.
                if message.get("role").and_then(Value::as_str) != Some("toolResult") {
                    continue;
                }
                let Some(details) = message.get("details") else {
                    continue;
                };
                match message.get("toolName").and_then(Value::as_str) {
                    Some("subagent") => {
                        let Some(record) = details.get(SUBAGENT_DETAIL) else {
                            continue;
                        };
                        let text = |key: &str| record.get(key).and_then(Value::as_str);
                        let (Some(id), Some(label)) = (text("agentId"), text("label")) else {
                            continue;
                        };
                        let run = record.get("runId").and_then(Value::as_u64).unwrap_or(0);
                        recovered.last_run = recovered.last_run.max(run);
                        if !recovered.children.iter().any(|child| child.id == id) {
                            recovered.children.push(RecoveredChild {
                                id: id.to_owned(),
                                label: label.to_owned(),
                                file: text("file").map(PathBuf::from),
                                provider: text("provider").unwrap_or_default().to_owned(),
                                model: text("model").unwrap_or_default().to_owned(),
                            });
                        }
                        // A foreground run settled in this result.
                        if record.get("foreground").and_then(Value::as_bool) != Some(true)
                            && run > 0
                        {
                            runs.push(RunRecord {
                                agent: id.to_owned(),
                                run,
                                on_branch,
                            });
                        }
                    }
                    Some("send_message") => {
                        let (Some(agent), Some(run)) = (
                            details.get("agentId").and_then(Value::as_str),
                            details.get("runId").and_then(Value::as_u64),
                        ) else {
                            continue;
                        };
                        recovered.last_run = recovered.last_run.max(run);
                        runs.push(RunRecord {
                            agent: agent.to_owned(),
                            run,
                            on_branch,
                        });
                    }
                    _ => {}
                }
            }
            "custom_message"
                if entry.get("customType").and_then(Value::as_str)
                    == Some(messages::NOTICE_TYPE) =>
            {
                let Some(details) = entry.get("details") else {
                    continue;
                };
                if let (Some(agent), Some(run)) = (
                    details.get("agentId").and_then(Value::as_str),
                    details.get("runId").and_then(Value::as_u64),
                ) {
                    recovered.last_run = recovered.last_run.max(run);
                    if on_branch {
                        settled.push((agent.to_owned(), run));
                    }
                }
            }
            _ => {}
        }
    }
    (recovered, runs, settled)
}

/// The children `session`, a parent's store, records, for a resumed
/// parent's host.
pub fn recover(session: &SessionManager) -> Recovered {
    project(session).0
}

/// A lost-run notice for each run started on the current branch that has
/// no settlement there: the process ended while it ran. Each is reported
/// once, with the next prompt, and never re-run.
pub fn lost_runs(session: &SessionManager) -> Vec<CustomMessage> {
    let (_, runs, settled) = project(session);
    let mut seen = std::collections::BTreeSet::new();
    runs.into_iter()
        .filter(|record| record.on_branch)
        .filter(|record| {
            !settled
                .iter()
                .any(|(agent, run)| *agent == record.agent && *run == record.run)
        })
        .filter(|record| seen.insert(record.run))
        .map(|record| {
            messages::settlement_notice(&record.agent, record.run, StopReason::Interrupted, None)
        })
        .collect()
}

#[cfg(test)]
mod tests;
