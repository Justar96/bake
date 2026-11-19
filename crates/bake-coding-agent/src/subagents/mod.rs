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
//!   `ready`. With none, a background start and the resume a message to a
//!   `ready` child needs wait in a queue of at most [`MAX_QUEUED`], oldest
//!   first, and start as runs settle; a foreground start waits for a slot
//!   until its call is dropped. A start is refused only when the queue is
//!   full, or when no slot can ever free (every slot holds an idle child
//!   with no file).
//! - **Intercom.** A message is a [`messages::MESSAGE_TYPE`] custom message
//!   posted to the recipient's registry ([`JobRegistry::post`]), which the
//!   session delivers as it delivers job notices: inside a run at its next
//!   step, else by a turn its owner opens within the wake budget. A message
//!   to an idle child starts a run; one to a `ready` child resumes it from
//!   its file first. A parent messages only its children and a child only
//!   its parent.
//! - **Settlement.** A run ends once: when the child's turn finishes with
//!   no message waiting and none of its own jobs live, when its jobs are
//!   still live the session's bash foreground budget ([`JOB_WAIT`] when
//!   unset) past its turn, or when it is stopped. An interrupt that comes
//!   before the run has its control stops it as it begins. Messages the run
//!   did not read move to the child's next run, in memory only. A run left
//!   with live jobs is continued by a new run when one of them ends. Its
//!   notice ([`messages::NOTICE_TYPE`]), which says what it left behind
//!   ([`messages::Leftovers`]), is posted to the parent once, or handed to
//!   a foreground call instead. The notice and the child's messages carry
//!   the wake depth of the parent turn that started the run
//!   ([`JobRegistry::post_at`]), so they wake an idle parent as that turn's
//!   jobs would, and a run started by a wake turn carries its depth.
//! - **Cancellation.** A foreground call returns as soon as its abort
//!   signal fires, interrupting the child's run, whether the child is
//!   waiting for a slot, being built, or running; `send_message` returns
//!   as soon as its abort fires while a `ready` child resumes, leaving the
//!   message waiting for the child's next run.
//! - **Restart.** The parent's file is the record: `subagent` results name
//!   each child and the run they started, `send_message` results the runs
//!   they started, and notices the runs that settled. A resumed parent
//!   lists its children as `ready` ([`recover`]). A background run's
//!   settlement is recorded ([`SETTLED_TYPE`]) before its notice is posted,
//!   so a run whose notice the file lacks gets that notice with the next
//!   prompt, or, with no settlement recorded, one lost-run notice
//!   ([`lost_runs`]); it is never re-run.
//!
//! At most [`MAX_ACTIVE`] children are resident, at depth 1. There is one
//! process-local host per parent session; children live in Pi session
//! files; idle children are evicted rather than refused at the cap; a
//! message is
//! logged by the sender's tool call and by the recipient's custom message
//! when delivered, with no inbox file in between; and a child may run on
//! any available model ([`models`]).

pub mod messages;
pub mod models;
mod tools;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use bake_agent::BoxFuture;
use bake_agent::RunOutcome;
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc, oneshot};

use crate::agent_session::{AgentSession, SessionSelection};
use crate::jobs::JobRegistry;
use crate::model_registry::ModelRuntime;
use crate::owned_work::{OwnedTask, TaskScope, WorkGate};
use crate::session::SessionManager;
use crate::session::messages::CustomMessage;
use crate::settings::Settings;
use messages::{Leftovers, StopReason};
use models::ChildRoute;

pub use tools::{CHILD_TOOL_NAMES, PARENT_TOOL_NAMES, child_tools, parent_tools};

/// Resident children at once.
pub const MAX_ACTIVE: usize = 8;

/// Starts and resumes that may wait at once for a resident slot.
pub const MAX_QUEUED: usize = 32;

/// How long a run waits, past the child's turn, for the child's own
/// background jobs, when the settings name no bash foreground budget.
pub const JOB_WAIT: Duration = Duration::from_secs(300);

/// The longest message one agent may send another, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// The subdirectory of a sessions directory that holds subagent files.
pub const CHILDREN_DIR: &str = "subagents";

/// The `customType` of the entry that starts a child's file.
pub const CHILD_ENTRY: &str = "bake.subagent.child";

/// The `customType` of the custom entry, outside the model's context, that
/// records a background run's settlement before its notice is posted.
pub const SETTLED_TYPE: &str = "bake.subagent.settled";

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
    /// Its current or last run; 0 for a child its parent's file recorded
    /// that has not run since.
    pub run: u64,
    /// How its last run in this process ended; `None` before one has, so
    /// for a recovered child until it runs again.
    pub settled: Option<Settlement>,
    /// Its session file, unless it does not persist.
    pub file: Option<PathBuf>,
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
    /// Where its file is; `None` while it is queued.
    pub file: Option<PathBuf>,
    /// The route it runs on.
    pub provider: String,
    /// The route it runs on.
    pub model: String,
    /// The level it runs at; empty while it is queued with its route's
    /// default.
    pub thinking_level: String,
    /// Whether it waits for a resident slot ([`MAX_QUEUED`]); it starts
    /// when one frees.
    pub queued: bool,
    /// For a foreground start, its settlement and what the run left.
    pub settled: Option<oneshot::Receiver<(Settlement, Leftovers)>>,
}

/// What [`SubagentHost::interrupt`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interrupt {
    /// The current run, or the one being started, stops.
    Requested,
    /// No run is in progress or starting; the child's status word.
    NotRunning(&'static str),
}

/// How [`SubagentHost::message_child`] handed a message over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Posted into the running run, which reads it at its next step.
    Running,
    /// It started this run, which reads it first.
    Started(u64),
    /// Kept for the run the child waits to start: this one, when the
    /// message queued it, else the one its start already recorded.
    Queued(Option<u64>),
}

/// A message [`SubagentHost::message_child`] handed over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    /// The message's id.
    pub message_id: String,
    /// How it was handed over.
    pub delivery: Delivery,
}

impl Sent {
    /// The run the message started or queued, which the result records.
    pub fn run(&self) -> Option<u64> {
        match self.delivery {
            Delivery::Running => None,
            Delivery::Started(run) => Some(run),
            Delivery::Queued(run) => run,
        }
    }
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
    /// How long a run waits for the child's own jobs past its turn.
    job_wait: Duration,
    /// Notified when a resident slot may have freed, for foreground starts
    /// waiting for one.
    slots: Notify,
    state: Mutex<HostState>,
}

#[derive(Default)]
struct HostState {
    children: Vec<Child>,
    /// Starts and resumes waiting for a resident slot, oldest first.
    queue: VecDeque<Queued>,
    /// The last run id issued or recorded.
    last_run: u64,
    /// Orders uses, for eviction.
    tick: u64,
}

/// A start or resume waiting for a resident slot.
enum Queued {
    /// A new child's first run: its task, with the return guidance, and
    /// route.
    Start {
        id: String,
        task: String,
        route: Box<ChildRoute>,
    },
    /// A `ready` child's next run, for the messages waiting for it.
    Resume { id: String },
}

impl Queued {
    fn id(&self) -> &str {
        match self {
            Self::Start { id, .. } | Self::Resume { id } => id,
        }
    }
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
    /// The wake depth of the parent's turn that started the current or
    /// last run; its notice and the child's messages carry it
    /// ([`JobRegistry::post_at`]).
    depth: usize,
    /// How the last run in this process ended.
    settled: Option<Settlement>,
    last_used: u64,
    resident: Option<Resident>,
    /// Messages for the child's next run, posted when it is dispatched:
    /// those sent while it starts or waits, and those a run that settled
    /// did not read. They live in this process only.
    waiting: Vec<CustomMessage>,
    /// `Starting` and waiting in the host's queue for a resident slot.
    queued: bool,
    /// An interrupt that came while the child was starting; its run stops
    /// as soon as it begins.
    interrupt_pending: bool,
    /// The task of an evicted child, which shuts it down and releases its
    /// file; a resume waits for it.
    retiring: Option<OwnedTask<()>>,
}

impl Child {
    fn new(id: String, label: String, provider: String, model: String, status: Status) -> Self {
        Self {
            id,
            label,
            file: None,
            provider,
            model,
            status,
            run: 0,
            depth: 0,
            settled: None,
            last_used: 0,
            resident: None,
            waiting: Vec::new(),
            queued: false,
            interrupt_pending: false,
            retiring: None,
        }
    }
}

struct Resident {
    session: Arc<AgentSession>,
    commands: mpsc::UnboundedSender<Command>,
    /// The child's task, kept so an eviction can be awaited.
    task: OwnedTask<()>,
    /// The current run's control, which an interrupt cancels.
    control: Option<bake_agent::RunControl>,
    /// Whether the current run was interrupted, possibly before it
    /// installed its control.
    interrupted: bool,
}

enum Command {
    Run {
        input: Input,
        run: u64,
        depth: usize,
        waiter: Option<oneshot::Sender<(Settlement, Leftovers)>>,
    },
    Evict,
}

enum Input {
    Task(String),
    /// The parent's messages, and any notices of the child's jobs.
    Mail,
    /// Notices of the child's own jobs, which outlived its last run.
    Jobs,
}

/// Why [`HostState::make_room`] found no slot.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cap {
    /// A running or starting child holds a slot, which frees when it
    /// settles.
    Busy,
    /// Every slot holds an idle child with no file to set it aside to, so
    /// none will free.
    Stuck,
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

fn queue_full() -> String {
    format!(
        "subagent queue is full: {MAX_ACTIVE} subagents are busy and {MAX_QUEUED} more wait. Wait for a notice that one finished, or do this work yourself."
    )
}

fn stuck() -> String {
    format!(
        "subagent limit reached: {MAX_ACTIVE} subagents are kept in this unsaved session and none can be set aside. Continue one with send_message, or do this work yourself."
    )
}

fn refusal(cap: Cap) -> String {
    match cap {
        Cap::Busy => queue_full(),
        Cap::Stuck => stuck(),
    }
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

/// Removes the message `message_id` from `waiting`.
fn withdraw(waiting: &mut Vec<CustomMessage>, message_id: &str) {
    waiting.retain(|message| {
        message
            .details
            .as_ref()
            .and_then(|details| details.get("messageId"))
            .and_then(Value::as_str)
            != Some(message_id)
    });
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
    /// used idle child that has a file, or says why there is none.
    fn make_room(&mut self) -> Result<(), Cap> {
        let resident = self
            .children
            .iter()
            .filter(|child| {
                !child.queued
                    && matches!(
                        child.status,
                        Status::Starting | Status::Running | Status::Idle
                    )
            })
            .count();
        if resident < MAX_ACTIVE {
            return Ok(());
        }
        if let Some(evictable) = self
            .children
            .iter_mut()
            .filter(|child| child.status == Status::Idle && child.file.is_some())
            .min_by_key(|child| child.last_used)
        {
            evictable.status = Status::Ready;
            if let Some(resident) = evictable.resident.take() {
                let _ = resident.commands.send(Command::Evict);
                evictable.retiring = Some(resident.task);
            }
            return Ok(());
        }
        let busy = self.children.iter().any(|child| {
            !child.queued && matches!(child.status, Status::Starting | Status::Running)
        });
        Err(if busy { Cap::Busy } else { Cap::Stuck })
    }

    /// [`Self::make_room`] for a start that may queue: behind the queue,
    /// so what waits starts first.
    fn admit(&mut self, behind_queue: bool) -> Result<(), Cap> {
        if behind_queue && !self.queue.is_empty() {
            return Err(Cap::Busy);
        }
        self.make_room()
    }
}

/// Sends resident child `id` run `run`, started by a parent turn of wake
/// depth `depth`, after the messages that waited for it.
fn dispatch(
    state: &mut HostState,
    id: &str,
    input: Input,
    run: u64,
    depth: usize,
    waiter: Option<oneshot::Sender<(Settlement, Leftovers)>>,
) {
    let Some(child) = state.child(id) else {
        return;
    };
    let Some(resident) = &child.resident else {
        return;
    };
    child.status = Status::Running;
    child.run = run;
    child.depth = depth;
    let jobs = resident.session.jobs();
    for message in std::mem::take(&mut child.waiting) {
        let _ = jobs.post(message);
    }
    // A run for the child's own jobs keeps its wake depth, which bounds a
    // chain of jobs that each start the next.
    if !matches!(input, Input::Jobs) {
        jobs.reset_wakes();
    }
    let _ = resident.commands.send(Command::Run {
        input,
        run,
        depth,
        waiter,
    });
}

/// What a dropped start or resume undoes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Undo {
    Nothing,
    /// Remove the child being started.
    Remove,
    /// Return the child being resumed to `ready`, keeping its messages.
    Ready,
}

/// Undoes a start or resume whose future is dropped or fails while the
/// child is built, so no record stays `running` with no task behind it,
/// and lets what waits for a slot take the one it held.
struct Pending<'a> {
    host: &'a SubagentHost,
    id: &'a str,
    undo: Undo,
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        {
            let mut state = lock(&self.host.inner.state);
            match self.undo {
                Undo::Nothing => return,
                Undo::Remove => state.children.retain(|child| child.id != self.id),
                Undo::Ready => {
                    if let Some(child) = state.child(self.id) {
                        child.status = Status::Ready;
                        child.queued = false;
                        child.interrupt_pending = false;
                    }
                }
            }
        }
        self.host.pump();
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
            .map(|recovered| {
                let status = if recovered.file.as_ref().is_some_and(|file| file.is_file()) {
                    Status::Ready
                } else {
                    Status::Unavailable
                };
                let mut child = Child::new(
                    recovered.id,
                    recovered.label,
                    recovered.provider,
                    recovered.model,
                    status,
                );
                child.file = recovered.file;
                child
            })
            .collect();
        let job_wait = settings.bash_foreground_budget().unwrap_or(JOB_WAIT);
        Self {
            inner: Arc::new(HostInner {
                parent_id,
                jobs,
                work,
                selection,
                registry,
                settings,
                builder,
                job_wait,
                slots: Notify::new(),
                state: Mutex::new(HostState {
                    children,
                    queue: VecDeque::new(),
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
    ///
    /// With no resident slot to be had, a background start is queued
    /// ([`Started::queued`]) and a foreground one waits for a slot;
    /// dropping the call stops the wait. A start is refused when the queue
    /// is full ([`MAX_QUEUED`]) or no slot can ever free.
    pub async fn start(
        &self,
        label: &str,
        task: &str,
        route: ChildRoute,
        foreground: bool,
    ) -> Result<Started, String> {
        let id = crate::session::id::uuid_v7();
        let depth = self.inner.jobs.wake_depth();
        let task = messages::task_with_return_guidance(&self.inner.parent_id, task);
        let run = loop {
            let freed = self.inner.slots.notified();
            tokio::pin!(freed);
            freed.as_mut().enable();
            {
                let mut state = lock(&self.inner.state);
                match state.admit(!foreground) {
                    Err(Cap::Stuck) => return Err(stuck()),
                    Err(Cap::Busy) if foreground => {}
                    Err(Cap::Busy) if state.queue.len() >= MAX_QUEUED => return Err(queue_full()),
                    admitted => {
                        let queued = admitted.is_err();
                        let run = state.next_run();
                        let tick = state.touch();
                        let mut child = Child::new(
                            id.clone(),
                            label.to_owned(),
                            route.model.provider.clone(),
                            route.model.id.clone(),
                            Status::Starting,
                        );
                        child.run = run;
                        child.depth = depth;
                        child.last_used = tick;
                        child.queued = queued;
                        state.children.push(child);
                        if queued {
                            let started = Started {
                                id: id.clone(),
                                run,
                                file: None,
                                provider: route.model.provider.clone(),
                                model: route.model.id.clone(),
                                thinking_level: route
                                    .thinking_level
                                    .map(|level| level.as_str().to_owned())
                                    .unwrap_or_default(),
                                queued: true,
                                settled: None,
                            };
                            state.queue.push_back(Queued::Start {
                                id,
                                task,
                                route: Box::new(route),
                            });
                            return Ok(started);
                        }
                        break run;
                    }
                }
            }
            freed.await;
        };
        let (waiter, settled) = if foreground {
            let (sender, receiver) = oneshot::channel();
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        let launched = self
            .launch(&id, label, task, route, run, depth, waiter)
            .await?;
        Ok(Started {
            id,
            run,
            file: launched.file,
            provider: launched.provider,
            model: launched.model,
            thinking_level: launched.thinking_level,
            queued: false,
            settled,
        })
    }

    /// Builds new child `id`, recorded as starting with a slot, makes it
    /// resident, and dispatches its first run. A call dropped or failing
    /// while the child is built leaves no record behind.
    #[allow(clippy::too_many_arguments)]
    async fn launch(
        &self,
        id: &str,
        label: &str,
        task: String,
        route: ChildRoute,
        run: u64,
        depth: usize,
        waiter: Option<oneshot::Sender<(Settlement, Leftovers)>>,
    ) -> Result<Launched, String> {
        let spec = ChildSpec {
            source: ChildSource::New {
                id: id.to_owned(),
                label: label.to_owned(),
            },
            link: self.link(id),
            route: Some(route),
        };
        let mut pending = Pending {
            host: self,
            id,
            undo: Undo::Remove,
        };
        let session = Arc::new((self.inner.builder)(spec).await?);
        let file = session.session_file();
        let (provider, model) = session
            .model()
            .map(|model| (model.provider, model.id))
            .unwrap_or_default();
        let thinking_level = session.thinking_level().as_str().to_owned();
        if let Err(error) = self.make_resident(id, Arc::clone(&session), file.clone()) {
            session.shutdown().await;
            return Err(error);
        }
        pending.undo = Undo::Nothing;
        dispatch(
            &mut lock(&self.inner.state),
            id,
            Input::Task(task),
            run,
            depth,
            waiter,
        );
        Ok(Launched {
            file,
            provider,
            model,
            thinking_level,
        })
    }

    /// Spawns the child's task on the parent's gate and records it
    /// resident.
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
        let job_wait = self.inner.job_wait;
        let task = self
            .inner
            .work
            .spawn(move |scope| {
                drive(
                    scope,
                    owned,
                    receiver,
                    host,
                    child_id,
                    parent_jobs,
                    job_wait,
                )
            })
            .map_err(|_| "The session is shut down.".to_owned())?;
        let mut state = lock(&self.inner.state);
        let tick = state.touch();
        let child = state
            .child(id)
            .ok_or_else(|| "The subagent was removed while it started.".to_owned())?;
        child.file = file.or(child.file.take());
        child.last_used = tick;
        child.queued = false;
        child.resident = Some(Resident {
            session,
            commands,
            task,
            control: None,
            interrupted: std::mem::take(&mut child.interrupt_pending),
        });
        Ok(())
    }

    /// Sends `text` from the parent to child `id`: steered into its run,
    /// or starting one when it is idle, after resuming it from its file
    /// when it is `ready`. With no resident slot to be had for a `ready`
    /// child, the message waits with it in the queue. A message is never
    /// dropped once this returns `Ok`; on an error it was not delivered.
    pub async fn message_child(&self, id: &str, text: &str) -> Result<Sent, String> {
        check_message(text)?;
        let message_id = crate::session::id::uuid_v7();
        let message = messages::agent_message(&message_id, &self.inner.parent_id, id, text);
        let depth = self.inner.jobs.wake_depth();
        // What the message needs once the lock is released.
        enum Then {
            Done(Delivery),
            Resume(u64, PathBuf),
        }
        let then = {
            let mut state = lock(&self.inner.state);
            let tick = state.touch();
            let index = state
                .children
                .iter()
                .position(|child| child.id == id)
                .ok_or_else(|| unknown(id))?;
            let child = &mut state.children[index];
            child.last_used = tick;
            let status = child.status;
            match status {
                Status::Unavailable => return Err(unavailable(id)),
                Status::Starting => {
                    child.waiting.push(message);
                    Then::Done(Delivery::Queued(None))
                }
                Status::Running => {
                    let resident = child.resident.as_ref().ok_or_else(|| unavailable(id))?;
                    let jobs = resident.session.jobs();
                    jobs.reset_wakes();
                    jobs.post(message).map_err(|_| unavailable(id))?;
                    Then::Done(Delivery::Running)
                }
                Status::Idle => {
                    if child.resident.is_none() {
                        return Err(unavailable(id));
                    }
                    child.waiting.push(message);
                    let run = state.next_run();
                    dispatch(&mut state, id, Input::Mail, run, depth, None);
                    Then::Done(Delivery::Started(run))
                }
                Status::Ready => {
                    let file = child.file.clone().ok_or_else(|| unavailable(id))?;
                    child.waiting.push(message);
                    let admitted = state.admit(true);
                    let queue_full = state.queue.len() >= MAX_QUEUED;
                    let refused = match admitted {
                        Err(Cap::Busy) if !queue_full => None,
                        Err(cap) => Some(cap),
                        Ok(()) => None,
                    };
                    if let Some(cap) = refused {
                        withdraw(&mut state.children[index].waiting, &message_id);
                        return Err(refusal(cap));
                    }
                    let run = state.next_run();
                    let child = &mut state.children[index];
                    child.status = Status::Starting;
                    child.run = run;
                    child.depth = depth;
                    if admitted.is_ok() {
                        Then::Resume(run, file)
                    } else {
                        child.queued = true;
                        state.queue.push_back(Queued::Resume { id: id.to_owned() });
                        Then::Done(Delivery::Queued(Some(run)))
                    }
                }
            }
        };
        let (run, file) = match then {
            Then::Done(delivery) => {
                return Ok(Sent {
                    message_id,
                    delivery,
                });
            }
            Then::Resume(run, file) => (run, file),
        };
        if let Err(error) = self.resume(id, run, depth, file).await {
            if let Some(child) = lock(&self.inner.state).child(id) {
                withdraw(&mut child.waiting, &message_id);
            }
            return Err(error);
        }
        Ok(Sent {
            message_id,
            delivery: Delivery::Started(run),
        })
    }

    /// Resumes `ready` child `id`, recorded as starting with a slot, from
    /// `file`, and dispatches run `run` for the messages waiting for it.
    /// A call dropped or failing while the child is built leaves it
    /// `ready` with its messages.
    async fn resume(&self, id: &str, run: u64, depth: usize, file: PathBuf) -> Result<(), String> {
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
        let session = match (self.inner.builder)(spec).await {
            Ok(session) => Arc::new(session),
            Err(_) => return Err(unavailable(id)),
        };
        if let Err(error) = self.make_resident(id, Arc::clone(&session), Some(file)) {
            session.shutdown().await;
            return Err(error);
        }
        pending.undo = Undo::Nothing;
        dispatch(
            &mut lock(&self.inner.state),
            id,
            Input::Mail,
            run,
            depth,
            None,
        );
        Ok(())
    }

    /// Starts what the queue holds while resident slots can be had, and
    /// wakes foreground starts waiting for one. Queued work that no slot
    /// will ever take is reported to the parent instead.
    fn pump(&self) {
        if self.inner.work.is_closing() {
            return;
        }
        let mut ready = Vec::new();
        let mut failed = Vec::new();
        {
            let mut state = lock(&self.inner.state);
            while !state.queue.is_empty() {
                match state.make_room() {
                    Ok(()) => {
                        let Some(entry) = state.queue.pop_front() else {
                            break;
                        };
                        if let Some(child) = state.child(entry.id()) {
                            child.queued = false;
                        }
                        ready.push(entry);
                    }
                    Err(Cap::Busy) => break,
                    Err(Cap::Stuck) => {
                        let entries: Vec<Queued> = state.queue.drain(..).collect();
                        for entry in entries {
                            let id = entry.id().to_owned();
                            let Some(child) = state.child(&id) else {
                                continue;
                            };
                            let (run, depth) = (child.run, child.depth);
                            let (what, undelivered) = match entry {
                                Queued::Start { .. } => ("start", 0),
                                Queued::Resume { .. } => {
                                    child.status = Status::Ready;
                                    child.queued = false;
                                    ("resume", std::mem::take(&mut child.waiting).len())
                                }
                            };
                            if what == "start" {
                                state.children.retain(|child| child.id != id);
                            }
                            failed.push(((id, run, depth), what, undelivered));
                        }
                        break;
                    }
                }
            }
        }
        for entry in ready {
            let host = self.clone();
            let task = self.inner.work.spawn(move |scope| async move {
                tokio::select! {
                    () = host.run_queued(entry) => {}
                    () = scope.signal().cancelled() => {}
                }
            });
            drop(task);
        }
        for ((id, run, depth), what, undelivered) in failed {
            let notice = messages::start_failure_notice(&id, run, what, &stuck(), undelivered);
            let _ = self.inner.jobs.post_at(notice, depth);
        }
        self.inner.slots.notify_waiters();
    }

    /// Starts or resumes what waited in the queue, which now holds a slot.
    /// A failure is reported to the parent, since no call waits for it.
    async fn run_queued(&self, entry: Queued) {
        match entry {
            Queued::Start { id, task, route } => {
                let Some((label, run, depth)) = lock(&self.inner.state)
                    .child(&id)
                    .map(|child| (child.label.clone(), child.run, child.depth))
                else {
                    return;
                };
                if let Err(error) = self
                    .launch(&id, &label, task, *route, run, depth, None)
                    .await
                {
                    let notice = messages::start_failure_notice(&id, run, "start", &error, 0);
                    let _ = self.inner.jobs.post_at(notice, depth);
                }
            }
            Queued::Resume { id } => {
                let Some((run, depth, file)) = lock(&self.inner.state)
                    .child(&id)
                    .map(|child| (child.run, child.depth, child.file.clone()))
                else {
                    return;
                };
                let resumed = match file {
                    Some(file) => self.resume(&id, run, depth, file).await,
                    None => Err(unavailable(&id)),
                };
                if let Err(error) = resumed {
                    let undelivered = lock(&self.inner.state).child(&id).map_or(0, |child| {
                        child.status = Status::Ready;
                        std::mem::take(&mut child.waiting).len()
                    });
                    let notice =
                        messages::start_failure_notice(&id, run, "resume", &error, undelivered);
                    let _ = self.inner.jobs.post_at(notice, depth);
                }
            }
        }
    }

    /// Starts a run of idle child `id`, resident as `session`, for the
    /// notices of its own jobs.
    fn continue_for_jobs(&self, id: &str, session: &Arc<AgentSession>) {
        let mut state = lock(&self.inner.state);
        let Some(child) = state.child(id) else {
            return;
        };
        let resident = child
            .resident
            .as_ref()
            .is_some_and(|resident| Arc::ptr_eq(&resident.session, session));
        if child.status != Status::Idle || !resident {
            return;
        }
        let depth = child.depth;
        let run = state.next_run();
        dispatch(&mut state, id, Input::Jobs, run, depth, None);
    }

    /// Stops child `id`'s current run, or the run it is starting; it stays
    /// available. A child with no run says so.
    pub fn interrupt(&self, id: &str) -> Result<Interrupt, String> {
        let mut state = lock(&self.inner.state);
        let child = state.child(id).ok_or_else(|| unknown(id))?;
        match (child.status, &mut child.resident) {
            (Status::Running | Status::Starting, Some(resident)) => {
                resident.interrupted = true;
                if let Some(control) = &resident.control {
                    control.cancel();
                }
                Ok(Interrupt::Requested)
            }
            (Status::Starting, None) => {
                child.interrupt_pending = true;
                Ok(Interrupt::Requested)
            }
            (Status::Idle, _) => Ok(Interrupt::NotRunning("idle")),
            (Status::Ready, _) => Ok(Interrupt::NotRunning("ready")),
            _ => Ok(Interrupt::NotRunning("unavailable")),
        }
    }

    /// The children, in start order, each with whether it is queued.
    fn listing(&self) -> Vec<(ChildRow, bool)> {
        lock(&self.inner.state)
            .children
            .iter()
            .map(|child| {
                let row = ChildRow {
                    id: child.id.clone(),
                    status: child.status,
                    label: child.label.clone(),
                    route: format!("{}/{}", child.provider, child.model),
                    run: child.run,
                    settled: child.settled.clone(),
                    file: child.file.clone(),
                };
                (row, child.queued)
            })
            .collect()
    }

    /// The children, in start order. A queued child is `Starting`.
    pub fn list(&self) -> Vec<ChildRow> {
        self.listing().into_iter().map(|(row, _)| row).collect()
    }

    /// Children that are starting, queued, or running.
    pub fn running(&self) -> usize {
        lock(&self.inner.state)
            .children
            .iter()
            .filter(|child| matches!(child.status, Status::Starting | Status::Running))
            .count()
    }
}

/// What [`SubagentHost::launch`] reports of a built child.
struct Launched {
    file: Option<PathBuf>,
    provider: String,
    model: String,
    thinking_level: String,
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
    /// parent. Returns the message id. The message carries the wake depth
    /// of the parent turn that started the child's run, so it wakes an
    /// idle parent as that run's notice would.
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
        let message = messages::agent_message(&message_id, &self.child_id, &self.parent_id, text);
        let depth = lock(&host.state)
            .child(&self.child_id)
            .map(|child| child.depth);
        match depth {
            Some(depth) => host.jobs.post_at(message, depth),
            None => host.jobs.post(message),
        }
        .map_err(|_| lost())?;
        Ok(message_id)
    }
}

/// What a run returned.
type RunResult = Result<RunOutcome, String>;

/// A resident child's task: runs what it is sent, one run at a time, until
/// it is evicted or the parent's gate closes; then shuts the child down.
/// Between runs it watches the child's own jobs when a run settled while
/// they ran, and continues the child when one of them ends.
async fn drive(
    scope: TaskScope,
    session: Arc<AgentSession>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    host: Weak<HostInner>,
    id: String,
    parent_jobs: JobRegistry,
    job_wait: Duration,
) {
    let mut events = session.jobs().subscribe();
    let mut watching = false;
    loop {
        let command = tokio::select! {
            command = commands.recv() => command,
            () = scope.signal().cancelled() => None,
            open = events.changed(), if watching => {
                watching = open;
                if open && session.jobs().has_undelivered() {
                    watching = false;
                    if let Some(inner) = host.upgrade() {
                        SubagentHost { inner }.continue_for_jobs(&id, &session);
                    }
                }
                continue;
            }
        };
        let Some(Command::Run {
            input,
            run,
            depth,
            waiter,
        }) = command
        else {
            break;
        };
        // Held until the notice is posted, so an owner waiting for live
        // work sees the notice before the count drops.
        let live = parent_jobs.hold_live();
        let Some((settlement, leftovers)) =
            activate(&scope, &session, input, run, &host, &id, job_wait).await
        else {
            break;
        };
        watching = !leftovers.jobs.is_empty();
        match waiter {
            // A dropped foreground call wants no notice.
            Some(waiter) => drop(waiter.send((settlement, leftovers))),
            None => {
                let notice = messages::settlement_notice_with(
                    &id,
                    settlement.run,
                    settlement.stop,
                    settlement.closing.as_deref(),
                    &leftovers,
                );
                // Durable before it is posted, as a job's terminal fact is:
                // a parent that stops before delivering the notice reports
                // this outcome on resume, not an interrupted run. A failed
                // write stops the parent's session, so nothing is posted.
                let fact = settled_fact(&id, &settlement, &leftovers);
                if parent_jobs.record_fact(SETTLED_TYPE, fact).await.is_ok() {
                    // At the depth of the turn that started the run, so the
                    // notice wakes an idle parent however deep it is now.
                    let _ = parent_jobs.post_at(notice, depth);
                }
            }
        }
        drop(live);
        if scope.signal().aborted() {
            break;
        }
        // The idle child's slot can be taken now.
        if let Some(inner) = host.upgrade() {
            SubagentHost { inner }.pump();
        }
    }
    session.shutdown().await;
    if let Some(inner) = host.upgrade() {
        {
            let mut state = lock(&inner.state);
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
        SubagentHost { inner }.pump();
    }
}

/// The data of a [`SETTLED_TYPE`] entry for `settlement`, child `id`'s,
/// with the count of messages it left unread.
fn settled_fact(id: &str, settlement: &Settlement, leftovers: &Leftovers) -> Value {
    let mut fact = json!({
        "version": 1,
        "agentId": id,
        "runId": settlement.run,
        "stopReason": settlement.stop.as_str(),
        "closing": settlement.closing,
    });
    if leftovers.unread > 0 {
        fact["unread"] = json!(leftovers.unread);
    }
    fact
}

/// What a run's end check decided.
enum Next {
    /// A message waits: deliver it in this run.
    Deliver,
    /// The child's own jobs are live: wait for them.
    Wait,
    /// The run is over; the child is idle and records how it ended.
    Settle(Settlement, Leftovers),
}

/// One run: the task or the waiting messages, then every message that
/// arrives and every notice of the child's own jobs, until the child's
/// turn ends with nothing left or the run is stopped. Past the turn, the
/// child's jobs are waited for up to `job_wait`; then the run settles
/// naming those still running. Messages the run did not read move to the
/// child's next run. `None` when the parent's gate closed; the run was
/// stopped and nothing is reported.
async fn activate(
    scope: &TaskScope,
    session: &AgentSession,
    input: Input,
    run: u64,
    host: &Weak<HostInner>,
    id: &str,
    job_wait: Duration,
) -> Option<(Settlement, Leftovers)> {
    let control = bake_agent::RunControl::default();
    {
        let host = host.upgrade()?;
        let mut state = lock(&host.state);
        if let Some(resident) = state.child(id).and_then(|child| child.resident.as_mut()) {
            resident.control = Some(control.clone());
            // An interrupt that came before the run had its control.
            if resident.interrupted {
                control.cancel();
            }
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
        Input::Mail | Input::Jobs => {
            owned_run(
                scope,
                &control,
                session.deliver_job_notices_with_control(control.clone()),
            )
            .await
        }
    };
    let mut events = session.jobs().subscribe();
    // When the wait for the child's jobs past its turn ends.
    let mut deadline: Option<tokio::time::Instant> = None;
    let mut waited_out = false;
    loop {
        if scope.signal().aborted() {
            return None;
        }
        let finished = matches!(result, Ok(RunOutcome::FinishedTurn));
        // Read before the host's lock: nothing prompts the child while
        // this run owns it, so its messages stay as they are.
        let last = last_response(session, before);
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
            } else if finished && !interrupted && jobs.live() > 0 && !waited_out {
                Next::Wait
            } else {
                if interrupted && finished {
                    result = Ok(RunOutcome::Cancelled);
                }
                let settlement = settle(last, run, &result);
                // Under the lock that posts messages to a running child, so
                // none arrives between this and the child going idle.
                let mut unread = jobs.claim_mail();
                let leftovers = Leftovers {
                    unread: unread.len(),
                    jobs: if waited_out {
                        live_jobs(jobs)
                    } else {
                        Vec::new()
                    },
                    lost: 0,
                };
                unread.append(&mut child.waiting);
                child.waiting = unread;
                // Under the lock that marks the child idle, so no listing
                // shows it idle with an earlier run's ending.
                child.status = Status::Idle;
                child.settled = Some(settlement.clone());
                if let Some(resident) = &mut child.resident {
                    resident.control = None;
                    resident.interrupted = false;
                }
                Next::Settle(settlement, leftovers)
            }
        };
        match next {
            Next::Deliver => {
                deadline = None;
                result = owned_run(
                    scope,
                    &control,
                    session.deliver_job_notices_with_control(control.clone()),
                )
                .await;
            }
            Next::Wait => {
                if deadline.is_none() {
                    deadline = tokio::time::Instant::now().checked_add(job_wait);
                }
                let expired = async {
                    match deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    _ = events.changed() => {}
                    () = scope.signal().cancelled() => return None,
                    _ = control.wait_stopped() => {}
                    () = expired => waited_out = true,
                }
            }
            Next::Settle(settlement, leftovers) => return Some((settlement, leftovers)),
        }
    }
}

/// The ids of `jobs`' jobs that are still running.
fn live_jobs(jobs: &JobRegistry) -> Vec<String> {
    jobs.list()
        .iter()
        .filter(|job| !job.status.is_terminal())
        .map(|job| job.id.to_string())
        .collect()
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

/// The last response of a run that began with `before` messages.
fn last_response(session: &AgentSession, before: usize) -> Option<bake_ai::AssistantMessage> {
    let messages = session.messages();
    messages
        .get(before.min(messages.len())..)
        .unwrap_or_default()
        .iter()
        .rev()
        .find_map(|message| message.as_assistant().cloned())
}

/// The settlement of a run that returned `result`, whose last response
/// was `assistant`.
fn settle(
    assistant: Option<bake_ai::AssistantMessage>,
    run: u64,
    result: &RunResult,
) -> Settlement {
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
        Ok(RunOutcome::Failed { message }) | Err(message) => {
            (StopReason::Error, Some(message.clone()))
        }
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

/// A settlement [`SETTLED_TYPE`] records: its child, run, stop reason,
/// and closing text.
struct SettledRecord {
    agent: String,
    run: u64,
    stop: StopReason,
    closing: Option<String>,
    /// Messages the run left unread, which did not outlive the process.
    unread: usize,
}

/// What [`project`] reads from a parent's file.
struct Projection {
    recovered: Recovered,
    /// Runs started.
    runs: Vec<RunRecord>,
    /// Runs whose notice is on the current branch.
    settled: Vec<(String, u64)>,
    /// Settlements recorded on any branch: a run's end cannot be undone.
    recorded: Vec<SettledRecord>,
}

/// The `subagent` and `send_message` results, notices, and recorded
/// settlements of `session`.
fn project(session: &SessionManager) -> Projection {
    let branch: std::collections::BTreeSet<&str> = session
        .branch_entries(None)
        .iter()
        .map(|entry| entry.id())
        .collect();
    let mut recovered = Recovered::default();
    let mut runs = Vec::new();
    let mut settled = Vec::new();
    let mut recorded = Vec::new();
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
            "custom" if entry.get("customType").and_then(Value::as_str) == Some(SETTLED_TYPE) => {
                let Some(data) = entry.get("data") else {
                    continue;
                };
                if data.get("version").and_then(Value::as_u64) != Some(1) {
                    continue;
                }
                let (Some(agent), Some(run), Some(stop)) = (
                    data.get("agentId").and_then(Value::as_str),
                    data.get("runId").and_then(Value::as_u64),
                    data.get("stopReason")
                        .and_then(Value::as_str)
                        .and_then(StopReason::parse),
                ) else {
                    continue;
                };
                recovered.last_run = recovered.last_run.max(run);
                recorded.push(SettledRecord {
                    agent: agent.to_owned(),
                    run,
                    stop,
                    closing: data
                        .get("closing")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    unread: data
                        .get("unread")
                        .and_then(Value::as_u64)
                        .map_or(0, |unread| usize::try_from(unread).unwrap_or(usize::MAX)),
                });
            }
            _ => {}
        }
    }
    Projection {
        recovered,
        runs,
        settled,
        recorded,
    }
}

/// The children `session`, a parent's store, records, for a resumed
/// parent's host.
pub fn recover(session: &SessionManager) -> Recovered {
    project(session).recovered
}

/// A notice for each run started on the current branch whose notice is
/// not there: the settlement notice its recorded settlement
/// ([`SETTLED_TYPE`]) names, or, with none, a lost-run notice, since the
/// process ended while it ran. Each is reported once, with the next
/// prompt, and never re-run.
pub fn lost_runs(session: &SessionManager) -> Vec<CustomMessage> {
    let Projection {
        runs,
        settled,
        recorded,
        ..
    } = project(session);
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
            match recorded
                .iter()
                .find(|fact| fact.agent == record.agent && fact.run == record.run)
            {
                Some(fact) => messages::settlement_notice_with(
                    &fact.agent,
                    fact.run,
                    fact.stop,
                    fact.closing.as_deref(),
                    &Leftovers {
                        lost: fact.unread,
                        ..Leftovers::default()
                    },
                ),
                None => messages::settlement_notice(
                    &record.agent,
                    record.run,
                    StopReason::Interrupted,
                    None,
                ),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
