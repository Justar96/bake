//! Spawned agents: headless `bake --mode rpc` processes a session starts
//! and owns, each a full Bake session of its own in the same repository.
//!
//! A [`SpawnHost`] belongs to one top-level session. Each agent it starts
//! is the leader of its own process tree ([`bake_process::OwnedProcess`]),
//! driven by one task of the session's [`WorkGate`], so the session's
//! shutdown closes every agent's standard input (RPC mode's orderly
//! shutdown) and kills the tree [`STOP_GRACE`] after the stop was asked
//! for, whether or not the agent has ended its output, then awaits the
//! tree: a wedged agent never holds the session open. The event stream is
//! read by a task of the driver's own that frames lines, so no other
//! event the driver waits on can cut one. The driver keeps a
//! [`SpawnedAgent`] row: its state, the tool call it is in, and the last
//! line it wrote; a frontend draws those rows.
//!
//! - **Slots.** At most [`MAX_LIVE`] agents run; one being stopped no
//!   longer counts. A spawn past that is queued, up to [`MAX_QUEUED`], and
//!   its driver starts it, oldest first, when a slot frees; stopping a
//!   queued agent cancels it.
//!
//! - **Talking.** Agents join the parent's telecom team ([`crate::telecom`])
//!   as `<parent>-<n>`, so parent and child message and ask each other with
//!   the `telecom` tool. A person's message goes in over RPC instead
//!   ([`SpawnHost::message`]), steering a running turn.
//! - **Results.** Each time an agent's run settles, the parent gets a
//!   notice with the run's last answer ([`NOTICE_TYPE`]), delivered as job
//!   notices are, so a parent idle at the time is woken within the wake
//!   budget. An agent that cannot start, refuses its task, or exits when
//!   the parent did not stop it is reported once the same way, and so is
//!   one that has waited [`events::ASK_NOTICE_AFTER`] for the person. A
//!   queued, starting, or working agent holds the parent's live count
//!   ([`JobRegistry::hold_live`]), so print mode waits for its notice.
//! - **People.** An agent's `ask_user` questions and its approval requests
//!   reach the parent's person through the parent's
//!   [`QuestionBroker`], marked with the agent's name; without one (print
//!   mode) they are declined and the call refused. A forwarded request is
//!   withdrawn when the agent's run settles, it is stopped, or it exits.
//! - **Access.** An agent runs under the parent's sandbox mode and approval
//!   policy at the time it starts, so it never has wider access than its
//!   parent; its model is the parent's unless the call names another.
//! - **Depth.** A spawned agent cannot spawn agents of its own.

mod events;
mod tools;

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use bake_process::{OwnedProcess, ProcessSpec};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Notify, mpsc};

use crate::jobs::{JobRegistry, LiveHold};
use crate::owned_work::{TaskScope, WorkGate};
use crate::permissions::StandingPermissions;
use crate::questions::QuestionBroker;
use crate::session::messages::CustomMessage;
use crate::session::{NewSessionOptions, SessionManager, load_entries_from_file};
use crate::telecom::Station;
use bake_ai::{TextContent, UserContent, UserContentBlock};

pub use tools::{TOOL_NAMES, WITH_SUBAGENTS_GUIDELINE, tools};

/// The `customType` of a spawned agent's notice to its parent.
pub const NOTICE_TYPE: &str = "bake.spawned";

/// The most agents running at once.
pub const MAX_LIVE: usize = 6;

/// The most agents waiting for a slot; a spawn past them is refused.
pub const MAX_QUEUED: usize = 32;

/// How long a stopped agent has to exit, from when the stop was asked for.
pub const STOP_GRACE: Duration = Duration::from_secs(10);

/// How many event lines the reader frames ahead of the driver.
const LINES_AHEAD: usize = 64;

/// The longest event line read from an agent; a longer one is skipped.
const LINE_CAP: usize = 8 * 1024 * 1024;

/// How much of an agent's standard error is kept, for a failure's reason.
const STDERR_TAIL: usize = 2048;

/// The longest answer a notice carries; a longer one ends with a marker
/// that says how much was cut and how to get it.
const NOTICE_TEXT_CAP: usize = 16 * 1024;

/// Where an agent is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnedState {
    /// Waiting for one of the [`MAX_LIVE`] slots; no process yet.
    Queued,
    /// Started, its first run not yet begun.
    Starting,
    /// In a run.
    Working,
    /// Between runs, waiting for a message.
    Idle,
    /// Waiting for the person to answer its question or approval.
    Asking,
    /// Its last run failed, with why.
    Failed(String),
    /// Its process ended: stopped, or exited by itself, with a reason when
    /// it exited unsuccessfully.
    Exited(Option<String>),
}

impl SpawnedState {
    /// Whether it is queued or its process still runs.
    pub fn is_live(&self) -> bool {
        !matches!(self, Self::Exited(_))
    }
}

/// Who started an agent, which its parent's model is told with each of
/// its notices, since a person's `/spawn` is not in the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The session's model, with `spawn_agent`.
    Model,
    /// The person, with `/spawn`.
    Person,
}

/// One spawned agent as a frontend shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnedAgent {
    /// Unique for the host's life.
    pub id: u64,
    /// Its callsign: the one it was asked to take until it reports the one
    /// it took.
    pub name: String,
    /// Its session id, once it reported it.
    pub session_id: Option<String>,
    /// The task it was started with, on one line.
    pub task: String,
    /// Who started it.
    pub origin: Origin,
    pub state: SpawnedState,
    /// The top-level tool call it is in.
    pub tool: Option<String>,
    /// The last line it wrote.
    pub last_line: Option<String>,
    /// `provider/model`, when known.
    pub model: Option<String>,
    /// When `state` last changed, Unix milliseconds.
    pub since: i64,
    /// Runs it finished.
    pub runs: u32,
}

/// What a spawn host is built with.
#[derive(Clone)]
pub struct HostOptions {
    /// The `bake` executable to start.
    pub program: PathBuf,
    pub cwd: PathBuf,
    /// The parent's station, which names the agents.
    pub station: Option<Station>,
    /// The parent's registry, which delivers the agents' notices.
    pub jobs: JobRegistry,
    pub work: WorkGate,
    /// Where the agents' questions and approvals go.
    pub questions: Option<QuestionBroker>,
    /// The parent's permission, which the agents inherit.
    pub standing: Arc<StandingPermissions>,
    /// The parent's current model as `provider/model`.
    pub model: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    /// How long a stopped agent has to exit; [`STOP_GRACE`].
    pub stop_grace: Duration,
}

enum Command {
    Prompt { text: String, steer: bool },
    Abort,
    Stop,
}

struct Entry {
    row: SpawnedAgent,
    commands: mpsc::UnboundedSender<Command>,
    /// Stopping or exiting: it takes no more commands and holds no slot.
    closing: bool,
}

impl Entry {
    /// Whether it holds one of the [`MAX_LIVE`] slots.
    fn holds_slot(&self) -> bool {
        !self.closing
            && !matches!(
                self.row.state,
                SpawnedState::Queued | SpawnedState::Exited(_)
            )
    }

    fn is_queued(&self) -> bool {
        !self.closing && self.row.state == SpawnedState::Queued
    }
}

struct Inner {
    options: HostOptions,
    agents: Mutex<Vec<Entry>>,
    next: AtomicU64,
    /// Bumped on every change, so a frontend redraws only then.
    version: AtomicU64,
    /// Notified when a slot may have freed, for the queued drivers.
    slots: Notify,
}

/// A session's spawned agents; cheap to clone.
#[derive(Clone)]
pub struct SpawnHost {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for SpawnHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpawnHost")
            .field("agents", &self.list().len())
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl SpawnHost {
    pub fn new(options: HostOptions) -> Self {
        Self {
            inner: Arc::new(Inner {
                options,
                agents: Mutex::new(Vec::new()),
                next: AtomicU64::new(1),
                version: AtomicU64::new(0),
                slots: Notify::new(),
            }),
        }
    }

    /// The session file of agent `id`, once it has reported its session
    /// and the file is among the Bake home's sessions for its directory.
    /// Reads session headers; a viewer keeps the path.
    pub fn session_file(&self, id: u64) -> Option<PathBuf> {
        let session = lock(&self.inner.agents)
            .iter()
            .find(|entry| entry.row.id == id)?
            .row
            .session_id
            .clone()?;
        let cwd = self.inner.options.cwd.to_string_lossy();
        SessionManager::find_by_id(&cwd, &session, None)
            .ok()
            .flatten()
    }

    /// The agents, in start order.
    pub fn list(&self) -> Vec<SpawnedAgent> {
        lock(&self.inner.agents)
            .iter()
            .map(|entry| entry.row.clone())
            .collect()
    }

    /// Changes with every change to [`Self::list`].
    pub fn version(&self) -> u64 {
        self.inner.version.load(Ordering::SeqCst)
    }

    /// How many agents hold a slot: starting, working, idle, or asking,
    /// and not being stopped.
    pub fn running(&self) -> usize {
        lock(&self.inner.agents)
            .iter()
            .filter(|entry| entry.holds_slot())
            .count()
    }

    /// Starts an agent on `task`, on `model` (`provider/model`) or the
    /// parent's, or queues it when [`MAX_LIVE`] run or others wait.
    /// Returns its row, [`SpawnedState::Queued`] when queued.
    pub fn spawn(
        &self,
        task: &str,
        model: Option<String>,
        origin: Origin,
    ) -> Result<SpawnedAgent, String> {
        let task = task.trim();
        if task.is_empty() {
            return Err("task: say what the agent should do".to_owned());
        }
        if task.len() > crate::telecom::wire::TEXT_CAP {
            return Err("task: too long; put the details in a file and point to it".to_owned());
        }
        let options = &self.inner.options;
        let model = model.or_else(|| (options.model)());
        let (id, name, receiver) = {
            let mut agents = lock(&self.inner.agents);
            let running = agents.iter().filter(|entry| entry.holds_slot()).count();
            let queued = agents.iter().filter(|entry| entry.is_queued()).count();
            // Behind the agents already waiting, so the queue stays in order.
            let queue = running >= MAX_LIVE || queued > 0;
            if queue && queued >= MAX_QUEUED {
                return Err(format!(
                    "{running} agents are running and {queued} are queued; stop one (stop_agent), or wait for their notices, before starting another"
                ));
            }
            let id = self.inner.next.fetch_add(1, Ordering::SeqCst);
            let name = self.plan_name(&agents);
            let (sender, receiver) = mpsc::unbounded_channel();
            agents.push(Entry {
                row: SpawnedAgent {
                    id,
                    name: name.clone(),
                    session_id: None,
                    task: crate::telecom::wire::one_line(task, 200),
                    origin,
                    state: if queue {
                        SpawnedState::Queued
                    } else {
                        SpawnedState::Starting
                    },
                    tool: None,
                    last_line: None,
                    model: model.clone(),
                    since: bake_ai::now_ms(),
                    runs: 0,
                },
                commands: sender,
                closing: false,
            });
            (id, name, receiver)
        };
        self.bump();
        let spec = self.spec(&name, model.as_deref());
        let weak = Arc::downgrade(&self.inner);
        let task = task.to_owned();
        // Taken here, so print mode waits for a queued agent too.
        let hold = options.jobs.hold_live();
        let started = options.work.spawn(move |scope| async move {
            drive(weak, id, spec, task, receiver, scope, hold).await;
        });
        if started.is_err() {
            self.update(id, |row| {
                row.state = SpawnedState::Exited(Some("the session is closing".to_owned()));
            });
            if let Some(entry) = lock(&self.inner.agents)
                .iter_mut()
                .find(|entry| entry.row.id == id)
            {
                entry.closing = true;
            }
            return Err("the session is closing".to_owned());
        }
        Ok(self.row(id).unwrap_or_else(|| unreachable_row(id)))
    }

    /// The first free `<parent>-<n>`, or `agent-<n>` without telecom.
    fn plan_name(&self, agents: &[Entry]) -> String {
        let station = self.inner.options.station.as_ref();
        let stem = station.map_or_else(|| "agent".to_owned(), Station::callsign);
        let mut taken: Vec<String> = agents.iter().map(|entry| entry.row.name.clone()).collect();
        if let Some(station) = station {
            taken.extend(station.peers().into_iter().map(|peer| peer.callsign));
        }
        (1..)
            .map(|n| format!("{stem}-{n}"))
            .find(|name| !taken.contains(name))
            .unwrap_or_else(|| "agent".to_owned())
    }

    fn spec(&self, name: &str, model: Option<&str>) -> ProcessSpec {
        let options = &self.inner.options;
        let mut args: Vec<OsString> = vec![
            "--mode".into(),
            "rpc".into(),
            "--sandbox".into(),
            options.standing.sandbox().as_str().into(),
            "--approval".into(),
            options.standing.approval().as_str().into(),
            "--name".into(),
            name.into(),
        ];
        if let Some(model) = model {
            args.push("--model".into());
            args.push(model.into());
        }
        let mut env: Vec<(OsString, OsString)> = std::env::vars_os()
            .filter(|(key, _)| {
                key != crate::telecom::CALLSIGN_ENV
                    && key != crate::telecom::PARENT_ENV
                    && key != crate::telecom::ENABLE_ENV
            })
            .collect();
        env.push((crate::telecom::CALLSIGN_ENV.into(), name.into()));
        // The agent joins telecom exactly when its parent did, whatever
        // its settings say, since its parent reaches it there.
        let joins = options.station.is_some();
        env.push((
            crate::telecom::ENABLE_ENV.into(),
            if joins { "on" } else { "off" }.into(),
        ));
        if let Some(station) = &options.station {
            env.push((crate::telecom::PARENT_ENV.into(), station.callsign().into()));
        }
        env.push((DEPTH_ENV.into(), "1".into()));
        ProcessSpec {
            program: options.program.clone().into_os_string(),
            args,
            cwd: Some(options.cwd.clone()),
            env: Some(env),
            stdin: true,
        }
    }

    fn row(&self, id: u64) -> Option<SpawnedAgent> {
        lock(&self.inner.agents)
            .iter()
            .find(|entry| entry.row.id == id)
            .map(|entry| entry.row.clone())
    }

    fn bump(&self) {
        self.inner.version.fetch_add(1, Ordering::SeqCst);
    }

    fn update(&self, id: u64, change: impl FnOnce(&mut SpawnedAgent)) {
        update(&self.inner, id, change);
    }

    /// The agent `who` names: its id, its name, or its session id prefix.
    pub fn find(&self, who: &str) -> Result<SpawnedAgent, String> {
        let who = who.trim();
        let agents = self.list();
        agents
            .iter()
            .find(|agent| agent.name == who || agent.id.to_string() == who)
            .or_else(|| {
                agents.iter().find(|agent| {
                    who.len() >= 4
                        && agent
                            .session_id
                            .as_deref()
                            .is_some_and(|id| id.starts_with(who))
                })
            })
            .cloned()
            .ok_or_else(|| {
                let names: Vec<&str> = agents.iter().map(|agent| agent.name.as_str()).collect();
                if names.is_empty() {
                    format!("no spawned agent \"{who}\"; none was started")
                } else {
                    format!("no spawned agent \"{who}\"; they are {}", names.join(", "))
                }
            })
    }

    fn command(&self, id: u64, command: Command) -> Result<(), String> {
        let stops = matches!(command, Command::Stop);
        {
            let mut agents = lock(&self.inner.agents);
            let entry = agents
                .iter_mut()
                .find(|entry| entry.row.id == id)
                .ok_or("no such agent")?;
            if !entry.row.state.is_live() {
                return Err(format!("{} has exited", entry.row.name));
            }
            if entry.closing {
                return Err(format!("{} is exiting", entry.row.name));
            }
            if matches!(command, Command::Abort) && entry.row.state == SpawnedState::Queued {
                return Err(format!(
                    "{} is queued and has no run to stop",
                    entry.row.name
                ));
            }
            entry
                .commands
                .send(command)
                .map_err(|_| format!("{} is exiting", entry.row.name))?;
            // Refused from now on, and no longer holding a slot, so a
            // queued agent can start while this one shuts down.
            if stops {
                entry.closing = true;
            }
        }
        if stops {
            self.inner.slots.notify_waiters();
        }
        Ok(())
    }

    /// Sends a person's message to agent `id`: it steers a running turn,
    /// or starts one.
    pub fn message(&self, id: u64, text: &str) -> Result<(), String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("write a message".to_owned());
        }
        self.command(
            id,
            Command::Prompt {
                text: text.to_owned(),
                steer: true,
            },
        )
    }

    /// Stops agent `id`'s run; the agent stays, idle.
    pub fn interrupt(&self, id: u64) -> Result<(), String> {
        self.command(id, Command::Abort)
    }

    /// Ends agent `id`: its input closes and it shuts down, or, queued, it
    /// never starts.
    pub fn stop(&self, id: u64) -> Result<(), String> {
        self.command(id, Command::Stop)
    }

    /// Forgets agents whose process ended.
    pub fn clear_exited(&self) {
        lock(&self.inner.agents).retain(|entry| entry.row.state.is_live());
        self.bump();
    }
}

/// Set in a spawned agent's environment; such a session spawns none.
pub const DEPTH_ENV: &str = "BAKE_SPAWN_DEPTH";

/// Whether this process is itself a spawned agent.
pub fn is_spawned() -> bool {
    std::env::var_os(DEPTH_ENV).is_some_and(|depth| !depth.is_empty() && depth != "0")
}

fn unreachable_row(id: u64) -> SpawnedAgent {
    SpawnedAgent {
        id,
        name: String::new(),
        session_id: None,
        task: String::new(),
        origin: Origin::Model,
        state: SpawnedState::Exited(None),
        tool: None,
        last_line: None,
        model: None,
        since: 0,
        runs: 0,
    }
}

fn update(inner: &Inner, id: u64, change: impl FnOnce(&mut SpawnedAgent)) {
    let changed = {
        let mut agents = lock(&inner.agents);
        match agents.iter_mut().find(|entry| entry.row.id == id) {
            Some(entry) => {
                let before = entry.row.clone();
                change(&mut entry.row);
                entry.row != before
            }
            None => false,
        }
    };
    if changed {
        inner.version.fetch_add(1, Ordering::SeqCst);
    }
}

fn notice(text: String, details: Value) -> CustomMessage {
    CustomMessage {
        custom_type: NOTICE_TYPE.to_owned(),
        content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))]),
        display: true,
        details: Some(details),
        timestamp: bake_ai::now_ms(),
    }
}

/// Writes one JSON record to the agent's input.
async fn write_record<W: tokio::io::AsyncWrite + Unpin>(
    input: &mut Option<W>,
    record: &Value,
) -> bool {
    let Some(writer) = input.as_mut() else {
        return false;
    };
    let mut line = serde_json::to_vec(record).unwrap_or_default();
    line.push(b'\n');
    let written = writer.write_all(&line).await.is_ok() && writer.flush().await.is_ok();
    if !written {
        *input = None;
    }
    written
}

/// Reads one line of at most [`LINE_CAP`] bytes. `None` at the end of the
/// stream; an over-long line is read to its end and returned empty.
async fn next_line<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> Option<Vec<u8>> {
    let mut line = Vec::new();
    let read = (&mut *reader)
        .take(LINE_CAP as u64)
        .read_until(b'\n', &mut line)
        .await
        .ok()?;
    if read == 0 {
        return None;
    }
    if line.last() != Some(&b'\n') && line.len() >= LINE_CAP {
        // Skip the rest of an over-long line.
        let mut rest = Vec::new();
        loop {
            rest.clear();
            let n = (&mut *reader)
                .take(LINE_CAP as u64)
                .read_until(b'\n', &mut rest)
                .await
                .ok()?;
            if n == 0 || rest.last() == Some(&b'\n') {
                break;
            }
        }
        return Some(Vec::new());
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    Some(line)
}

/// The live row of agent `id`, if any.
fn row_of(host: &Inner, id: u64) -> Option<SpawnedAgent> {
    lock(&host.agents)
        .iter()
        .find(|entry| entry.row.id == id)
        .map(|entry| entry.row.clone())
}

/// Marks agent `id` as taking no more commands and holding no slot.
fn close_entry(host: &Inner, id: u64) {
    if let Some(entry) = lock(&host.agents)
        .iter_mut()
        .find(|entry| entry.row.id == id)
    {
        entry.closing = true;
    }
    host.slots.notify_waiters();
}

/// Moves queued agent `id` to starting when it is the oldest queued agent
/// and a slot is free. True once it may start.
fn take_slot(host: &Inner, id: u64) -> bool {
    {
        let mut agents = lock(&host.agents);
        let running = agents.iter().filter(|entry| entry.holds_slot()).count();
        let first = agents
            .iter()
            .filter(|entry| entry.is_queued())
            .map(|entry| entry.row.id)
            .min();
        let Some(entry) = agents.iter_mut().find(|entry| entry.row.id == id) else {
            return false;
        };
        if entry.row.state != SpawnedState::Queued {
            return true;
        }
        if running >= MAX_LIVE || first != Some(id) {
            return false;
        }
        entry.row.state = SpawnedState::Starting;
        entry.row.since = bake_ai::now_ms();
    }
    host.version.fetch_add(1, Ordering::SeqCst);
    // Another slot may be free for the next in line.
    host.slots.notify_waiters();
    true
}

/// Waits for a slot for queued agent `id`. `None` when it was stopped or
/// the session ended first; else the person's messages that arrived
/// meanwhile, to send after its task.
async fn wait_for_slot(
    inner: &Weak<Inner>,
    id: u64,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    scope: &TaskScope,
) -> Option<Vec<String>> {
    let mut held = Vec::new();
    loop {
        let host = inner.upgrade()?;
        let freed = host.slots.notified();
        tokio::pin!(freed);
        // Registered before the check, so a slot freed after it wakes this.
        freed.as_mut().enable();
        if take_slot(&host, id) {
            return Some(held);
        }
        tokio::select! {
            () = &mut freed => {}
            () = scope.signal().cancelled() => return None,
            command = commands.recv() => match command {
                Some(Command::Prompt { text, .. }) => held.push(text),
                Some(Command::Abort) => {}
                Some(Command::Stop) | None => return None,
            },
        }
    }
}

/// A person's message sent to the agent, kept until the agent answers it.
struct Sent {
    key: String,
    text: String,
    /// Sent again as a follow-up after a refusal.
    retried: bool,
}

/// The most person's messages kept for a retry; older ones were answered.
const SENT_KEPT: usize = 64;

/// Runs agent `id` until it exits or the session ends. `hold` keeps the
/// parent's live count while the agent is queued, starting, or in a run.
async fn drive(
    inner: Weak<Inner>,
    id: u64,
    spec: ProcessSpec,
    task: String,
    mut commands: mpsc::UnboundedReceiver<Command>,
    scope: TaskScope,
    mut hold: Option<LiveHold>,
) {
    let Some(held) = wait_for_slot(&inner, id, &mut commands, &scope).await else {
        // Cancelled while queued: it never ran, so there is nothing to tell.
        if let Some(host) = inner.upgrade() {
            close_entry(&host, id);
            update(&host, id, |row| {
                row.state = SpawnedState::Exited(None);
                row.since = bake_ai::now_ms();
            });
        }
        return;
    };
    let Some(host) = inner.upgrade() else { return };
    let jobs = host.options.jobs.clone();
    let grace = host.options.stop_grace;
    let mut process = match OwnedProcess::spawn(&spec, None).await {
        Ok(process) => process,
        Err(error) => {
            let reason = format!("could not start bake: {error}");
            exited(&host, id, Some(reason), false, &jobs);
            return;
        }
    };
    let mut input = process.take_stdin();
    let Some(stdout) = process.take_stdout() else {
        process.kill();
        let _ = process.settle().await;
        exited(&host, id, Some("no output pipe".to_owned()), false, &jobs);
        return;
    };
    let stderr_tail = Arc::new(Mutex::new(VecDeque::<u8>::new()));
    let mut side = tokio::task::JoinSet::new();
    if let Some(mut stderr) = process.take_stderr() {
        let tail = Arc::clone(&stderr_tail);
        side.spawn(async move {
            let mut chunk = [0u8; 1024];
            while let Ok(n) = stderr.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                let mut tail = lock(&tail);
                tail.extend(&chunk[..n]);
                let excess = tail.len().saturating_sub(STDERR_TAIL);
                tail.drain(..excess);
            }
        });
    }
    // The stream is framed on a task of its own, which the driver awaits:
    // a line read halfway when another branch of the driver's select wins
    // is not lost.
    let (framed, mut lines) = mpsc::channel::<Vec<u8>>(LINES_AHEAD);
    let reader = tokio::spawn(async move {
        let mut stdout = BufReader::new(stdout);
        while let Some(line) = next_line(&mut stdout).await {
            if framed.send(line).await.is_err() {
                break;
            }
        }
    });
    write_record(
        &mut input,
        &json!({ "id": "bake-spawn-state", "type": "get_state" }),
    )
    .await;
    write_record(
        &mut input,
        &json!({ "id": TASK_ID, "type": "prompt", "message": task }),
    )
    .await;
    let mut sent: VecDeque<Sent> = VecDeque::new();
    let mut sent_count = 0u64;
    for text in held {
        send_message(&mut input, &mut sent, &mut sent_count, text, "followUp").await;
    }

    let questions = host.options.questions.clone();
    let station = host.options.station.clone();
    drop(host);
    let mut follower = events::Follower::default();
    let (answers, mut answered) = mpsc::unbounded_channel::<Value>();
    // Cancelled when a run settles, the agent stops, or it exits: its
    // forwarded requests leave the person's queue then.
    let mut asks = bake_ai::AbortController::new();
    let mut stopping = false;
    let mut deadline: Option<tokio::time::Instant> = None;
    let mut killed = false;
    let mut in_run = false;
    loop {
        let grace_over = async {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        let mut stop = false;
        tokio::select! {
            () = scope.signal().cancelled(), if !stopping => stop = true,
            () = grace_over => {
                process.kill();
                killed = true;
                break;
            }
            line = lines.recv() => {
                let Some(line) = line else { break };
                let Ok(event) = serde_json::from_slice::<Value>(&line) else { continue };
                let Some(host) = inner.upgrade() else { break };
                let effects = {
                    let mut agents = lock(&host.agents);
                    let Some(entry) = agents.iter_mut().find(|entry| entry.row.id == id) else { break };
                    follower.follow(&event, &mut entry.row)
                };
                host.version.fetch_add(1, Ordering::SeqCst);
                for effect in effects {
                    match effect {
                        events::Effect::Started => {
                            in_run = true;
                            if hold.is_none() {
                                hold = jobs.hold_live();
                            }
                        }
                        events::Effect::Settled { text, failed, interrupted } => {
                            in_run = false;
                            asks.abort();
                            asks = bake_ai::AbortController::new();
                            resolve_name(&host, id, station.as_ref());
                            if let Some(row) = row_of(&host, id).filter(|_| !stopping) {
                                let queued = lock(&host.agents)
                                    .iter()
                                    .filter(|entry| entry.is_queued())
                                    .count();
                                // Another agent's report, as a telecom
                                // message is: it refills the wake budget, so
                                // the parent always hears it.
                                jobs.reset_wakes();
                                let _ = jobs.post(settled_notice(&row, text, failed, interrupted, queued));
                            }
                            // Released after the notice is posted, so a
                            // waiting print mode finds it.
                            hold = None;
                        }
                        events::Effect::Refused { id: key, error } => {
                            if key.as_deref() == Some(TASK_ID) {
                                task_refused(&host, id, &error, &jobs, stopping);
                                if !in_run {
                                    hold = None;
                                }
                            } else if let Some(at) = sent.iter().position(|each| key.as_deref() == Some(each.key.as_str())) {
                                let message = sent.remove(at).unwrap_or_else(|| unreachable!());
                                if message.retried {
                                    let reason = crate::telecom::wire::one_line(&format!("your message was not delivered: {error}"), 300);
                                    update(&host, id, |row| {
                                        row.state = SpawnedState::Failed(reason);
                                        row.since = bake_ai::now_ms();
                                    });
                                } else {
                                    // Once more as a follow-up, which a busy
                                    // agent queues instead of refusing.
                                    let key = format!("{}-again", message.key);
                                    write_record(&mut input, &json!({ "id": key, "type": "follow_up", "message": message.text })).await;
                                    sent.push_back(Sent { key, text: message.text, retried: true });
                                }
                            }
                        }
                        events::Effect::SessionId => resolve_name(&host, id, station.as_ref()),
                        events::Effect::Ask(request) => {
                            let name = row_of(&host, id).map(|row| row.name).unwrap_or_default();
                            let questions = questions.clone();
                            let answers = answers.clone();
                            let signal = asks.signal();
                            let weak = Arc::downgrade(&host);
                            let jobs = jobs.clone();
                            side.spawn(async move {
                                if let Some(host) = weak.upgrade() {
                                    update(&host, id, |row| row.state = SpawnedState::Asking);
                                }
                                let waited = {
                                    let name = name.clone();
                                    move |what: String| {
                                        jobs.reset_wakes();
                                        let _ = jobs.post(waiting_notice(&name, &what));
                                    }
                                };
                                let response = request.forward(&name, questions.as_ref(), signal, waited).await;
                                if let Some(host) = weak.upgrade() {
                                    update(&host, id, |row| {
                                        if row.state == SpawnedState::Asking {
                                            row.state = SpawnedState::Working;
                                        }
                                    });
                                }
                                let _ = answers.send(response);
                            });
                        }
                    }
                }
            }
            Some(response) = answered.recv() => {
                write_record(&mut input, &response).await;
            }
            Some(command) = commands.recv() => match command {
                Command::Prompt { text, steer } => {
                    let behavior = if steer { "steer" } else { "followUp" };
                    send_message(&mut input, &mut sent, &mut sent_count, text, behavior).await;
                }
                Command::Abort => {
                    write_record(&mut input, &json!({ "type": "abort" })).await;
                }
                Command::Stop => stop = true,
            },
            Some(_) = side.join_next(), if !side.is_empty() => {}
        }
        if stop && !stopping {
            // The grace starts now, not when the output ends: a child
            // whose shutdown hangs is killed when it runs out.
            stopping = true;
            input = None;
            asks.abort();
            deadline = Some(tokio::time::Instant::now() + grace);
            if let Some(host) = inner.upgrade() {
                close_entry(&host, id);
            }
        }
    }
    // The driver reads no more commands.
    if let Some(host) = inner.upgrade() {
        close_entry(&host, id);
    }
    asks.abort();
    drop(input);
    if !killed {
        let left = deadline.map_or(grace, |deadline| {
            deadline.saturating_duration_since(tokio::time::Instant::now())
        });
        if tokio::time::timeout(left, process.wait_exit())
            .await
            .is_err()
        {
            process.kill();
        }
    }
    let settled = process.settle().await;
    // The tree is gone, so the pipes reach their end; let the readers take
    // what is left, which says why a failed agent stopped.
    drop(lines);
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        while side.join_next().await.is_some() {}
    })
    .await;
    side.abort_all();
    while side.join_next().await.is_some() {}
    reader.abort();
    let _ = reader.await;
    let tail = String::from_utf8_lossy(&lock(&stderr_tail).iter().copied().collect::<Vec<_>>())
        .trim()
        .to_owned();
    if let Some(host) = inner.upgrade() {
        if stopping {
            update(&host, id, |row| {
                row.state = SpawnedState::Exited(None);
                row.tool = None;
                row.since = bake_ai::now_ms();
            });
        } else {
            let reason = match settled {
                Err(error) => Some(error.to_string()),
                Ok(settled) if settled.status.success() => None,
                Ok(settled) => Some(failure(&tail, settled.status)),
            };
            exited(&host, id, reason, in_run, &jobs);
        }
    }
    drop(hold);
}

/// The request id of an agent's task prompt.
const TASK_ID: &str = "bake-spawn-task";

/// Sends a person's message, keeping it under its request id until the
/// agent answers, so a refusal can send it again.
async fn send_message<W: tokio::io::AsyncWrite + Unpin>(
    input: &mut Option<W>,
    sent: &mut VecDeque<Sent>,
    count: &mut u64,
    text: String,
    behavior: &str,
) {
    *count += 1;
    let key = format!("bake-spawn-msg-{count}");
    write_record(
        input,
        &json!({ "id": key, "type": "prompt", "message": text, "streamingBehavior": behavior }),
    )
    .await;
    sent.push_back(Sent {
        key,
        text,
        retried: false,
    });
    if sent.len() > SENT_KEPT {
        sent.pop_front();
    }
}

/// The agent refused its task, so the run its parent waits for never
/// comes: the row says why and, unless the parent is stopping it, the
/// parent is told.
fn task_refused(host: &Inner, id: u64, error: &str, jobs: &JobRegistry, stopping: bool) {
    if let Some(row) = row_of(host, id).filter(|_| !stopping) {
        jobs.reset_wakes();
        let _ = jobs.post(notice(
            format!("[spawn] {} could not start its task: {error}", who(&row)),
            json!({ "agent": row.name, "status": "failed" }),
        ));
    }
    update(host, id, |row| {
        row.state = SpawnedState::Failed(error.to_owned());
        row.since = bake_ai::now_ms();
    });
}

/// Marks agent `id` exited by itself, with `reason` when it failed, and
/// tells the parent once, since the parent did not stop it.
fn exited(host: &Inner, id: u64, reason: Option<String>, in_run: bool, jobs: &JobRegistry) {
    close_entry(host, id);
    if let Some(row) = row_of(host, id) {
        let body = match (&reason, in_run) {
            (Some(reason), _) => format!("[spawn] {} exited: {reason}", who(&row)),
            (None, true) => format!("[spawn] {} exited before its run finished.", who(&row)),
            (None, false) => format!("[spawn] {} exited.", who(&row)),
        };
        jobs.reset_wakes();
        let _ = jobs.post(notice(
            body,
            json!({ "agent": row.name, "status": "exited" }),
        ));
    }
    // After the notice, so whoever sees the row exited finds it posted.
    update(host, id, |row| {
        row.state = SpawnedState::Exited(reason);
        row.tool = None;
        row.since = bake_ai::now_ms();
    });
}

/// The notice for an agent that has waited long for the person.
fn waiting_notice(name: &str, what: &str) -> CustomMessage {
    notice(
        format!(
            "[spawn] {name} has waited {} s for the person to {what}; it continues when they answer, or stop it with stop_agent",
            events::ASK_NOTICE_AFTER.as_secs()
        ),
        json!({ "agent": name, "status": "waiting" }),
    )
}

/// Why an agent's process failed: its standard error's tail as one line,
/// which names the cause when bake refuses to start, or its exit status.
fn failure(tail: &str, status: std::process::ExitStatus) -> String {
    let text = tail
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        format!("bake {status}")
    } else {
        crate::telecom::wire::one_line(&text, 300)
    }
}

/// Takes the callsign the agent's session holds, once its session id is
/// known and its station has published.
fn resolve_name(host: &Inner, id: u64, station: Option<&Station>) {
    let Some(station) = station else { return };
    let session = lock(&host.agents)
        .iter()
        .find(|entry| entry.row.id == id)
        .and_then(|entry| entry.row.session_id.clone());
    let Some(session) = session else { return };
    if let Some(peer) = station
        .peers()
        .into_iter()
        .find(|peer| peer.session == session)
    {
        update(host, id, |row| row.name = peer.callsign);
    }
}

/// `<name>, which <starter> started on "<task>",`: how a notice names an
/// agent, since a person's `/spawn` is not in the conversation.
fn who(agent: &SpawnedAgent) -> String {
    let starter = match agent.origin {
        Origin::Model => "you",
        Origin::Person => "the person (with /spawn)",
    };
    format!(
        "{}, which {starter} started on \"{}\",",
        agent.name, agent.task
    )
}

fn settled_notice(
    agent: &SpawnedAgent,
    text: Option<String>,
    failed: Option<String>,
    interrupted: bool,
    queued: usize,
) -> CustomMessage {
    let name = &agent.name;
    let who = who(agent);
    let ended = if interrupted {
        "was interrupted and is idle"
    } else {
        "finished a run and is idle"
    };
    // While agents wait for a slot, an idle agent left running keeps one
    // of them waiting.
    let slot = match queued {
        0 => String::new(),
        1 => "; 1 agent is queued, and stopping this one (stop_agent) if its work is done lets it start".to_owned(),
        n => format!("; {n} agents are queued, and stopping this one (stop_agent) if its work is done lets one start"),
    };
    let body = match (&failed, text) {
        (Some(reason), _) => format!("[spawn] {who} failed its run: {reason}"),
        (None, Some(text)) => {
            let text = capped_answer(&text, name, NOTICE_TEXT_CAP);
            format!("[spawn] {who} {ended}{slot}. Its last answer:\n{text}")
        }
        (None, None) => format!("[spawn] {who} {ended}{slot}, with no answer."),
    };
    let status = match (&failed, interrupted) {
        (Some(_), _) => "failed",
        (None, true) => "interrupted",
        (None, false) => "idle",
    };
    notice(body, json!({ "agent": name, "status": status }))
}

/// The context messages a session file holds now: what a viewer of a
/// spawned agent's transcript shows. The file is read without a lock and
/// never written, as `/export` reads one, so the agent writing it is not
/// disturbed; a missing or empty file holds none.
pub fn read_transcript(path: &Path) -> Result<Vec<bake_agent::AgentMessage>, String> {
    let entries =
        load_entries_from_file(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let session = SessionManager::in_memory(None, NewSessionOptions::default(), entries)
        .map_err(|error| error.to_string())?;
    Ok(session
        .build_session_context()
        .messages
        .iter()
        .map(|message| message.to_agent())
        .collect())
}

/// `text` trimmed and cut to `cap` bytes at a character boundary, ending
/// with how many bytes were cut and how to get them from `name`.
fn capped_answer(text: &str, name: &str, cap: usize) -> String {
    let text = text.trim();
    if text.len() <= cap {
        return text.to_owned();
    }
    let mut cut = cap;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let more = text.len() - cut;
    format!(
        "{}\n[... {more} more bytes cut; ask {name} with telecom for the rest]",
        &text[..cut]
    )
}

#[cfg(test)]
mod answer_tests {
    use proptest::prelude::*;

    use super::{capped_answer, read_transcript};

    #[test]
    fn a_transcript_is_the_session_file_s_context() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pi-session/pi-v3.jsonl");
        let expected: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(fixture.with_extension("pi.json")).expect("fixture"),
        )
        .expect("json");
        let messages = read_transcript(&fixture).expect("read");
        assert_eq!(
            messages.len(),
            expected["context"]["messages"]
                .as_array()
                .expect("messages")
                .len()
        );
        let missing = fixture.with_file_name("no-such-session.jsonl");
        assert!(read_transcript(&missing).expect("read").is_empty());
    }

    proptest! {
        /// Any answer keeps a prefix of its trimmed text within the cap,
        /// and says exactly how many bytes the cut left out.
        #[test]
        fn any_answer_keeps_a_prefix_within_the_cap(text in "\\PC{0,64}", cap in 0usize..48) {
            let out = capped_answer(&text, "x", cap);
            let trimmed = text.trim();
            match out.split_once("\n[... ") {
                Some((kept, marker)) if trimmed.len() > cap => {
                    prop_assert!(kept.len() <= cap);
                    prop_assert!(trimmed.starts_with(kept));
                    let more = trimmed.len() - kept.len();
                    prop_assert_eq!(marker, format!("{more} more bytes cut; ask x with telecom for the rest]"));
                }
                _ => prop_assert_eq!(out, trimmed),
            }
        }
    }

    #[test]
    fn a_long_answer_says_how_much_was_cut_and_whom_to_ask() {
        assert_eq!(capped_answer("  short \n", "alpha-1", 16), "short");
        assert_eq!(
            capped_answer("0123456789abcdef!!", "alpha-1", 16),
            "0123456789abcdef\n[... 2 more bytes cut; ask alpha-1 with telecom for the rest]"
        );
        // Never inside a character: "é" is two bytes.
        assert_eq!(
            capped_answer("aé", "b", 2),
            "a\n[... 2 more bytes cut; ask b with telecom for the rest]"
        );
    }
}

#[cfg(all(test, unix))]
mod tests;
