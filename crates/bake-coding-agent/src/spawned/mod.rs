//! Spawned agents: headless `bake --mode rpc` processes a session starts
//! and owns, each a full Bake session of its own in the same repository.
//!
//! A [`SpawnHost`] belongs to one top-level session. Each agent it starts
//! is the leader of its own process tree ([`bake_process::OwnedProcess`]),
//! driven by one task of the session's [`WorkGate`], so the session's
//! shutdown closes every agent's standard input (RPC mode's orderly
//! shutdown), waits for it to exit, kills what is left after
//! [`STOP_GRACE`], and awaits the tree. The driver reads the agent's event
//! stream to keep a [`SpawnedAgent`] row: its state, the tool call it is
//! in, and the last line it wrote; a frontend draws those rows.
//!
//! - **Talking.** Agents join the parent's telecom team ([`crate::telecom`])
//!   as `<parent>-<n>`, so parent and child message and ask each other with
//!   the `telecom` tool. A person's message goes in over RPC instead
//!   ([`SpawnHost::message`]), steering a running turn.
//! - **Results.** Each time an agent's run settles, the parent gets a
//!   notice with the run's last answer ([`NOTICE_TYPE`]), delivered as job
//!   notices are, so a parent idle at the time is woken within the wake
//!   budget.
//! - **People.** An agent's `ask_user` questions and its approval requests
//!   reach the parent's person through the parent's
//!   [`QuestionBroker`], marked with the agent's name; without one (print
//!   mode) they are declined and the call refused.
//! - **Access.** An agent runs under the parent's sandbox mode and approval
//!   policy at the time it starts, so it never has wider access than its
//!   parent; its model is the parent's unless the call names another.
//! - **Depth.** A spawned agent cannot spawn agents of its own.

mod events;
mod tools;

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use bake_process::{OwnedProcess, ProcessSpec};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::jobs::JobRegistry;
use crate::owned_work::{TaskScope, WorkGate};
use crate::permissions::StandingPermissions;
use crate::questions::QuestionBroker;
use crate::session::messages::CustomMessage;
use crate::telecom::Station;
use bake_ai::{TextContent, UserContent, UserContentBlock};

pub use tools::{TOOL_NAMES, WITH_SUBAGENTS_GUIDELINE, tools};

/// The `customType` of a spawned agent's notice to its parent.
pub const NOTICE_TYPE: &str = "bake.spawned";

/// The most agents running at once.
pub const MAX_LIVE: usize = 6;

/// How long a stopped agent has to exit after its input closes.
pub const STOP_GRACE: Duration = Duration::from_secs(10);

/// The longest event line read from an agent; a longer one is skipped.
const LINE_CAP: usize = 8 * 1024 * 1024;

/// How much of an agent's standard error is kept, for a failure's reason.
const STDERR_TAIL: usize = 2048;

/// The longest answer a notice carries.
const NOTICE_TEXT_CAP: usize = 4000;

/// Where an agent is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnedState {
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
    /// Whether its process still runs.
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
}

enum Command {
    Prompt { text: String, steer: bool },
    Abort,
    Stop,
}

struct Entry {
    row: SpawnedAgent,
    commands: mpsc::UnboundedSender<Command>,
}

struct Inner {
    options: HostOptions,
    agents: Mutex<Vec<Entry>>,
    next: AtomicU64,
    /// Bumped on every change, so a frontend redraws only then.
    version: AtomicU64,
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
            }),
        }
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

    /// Starts an agent on `task`, on `model` (`provider/model`) or the
    /// parent's. Returns its row.
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
            if agents
                .iter()
                .filter(|entry| entry.row.state.is_live())
                .count()
                >= MAX_LIVE
            {
                return Err(format!(
                    "{MAX_LIVE} agents are running; stop one (stop_agent) before starting another"
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
                    state: SpawnedState::Starting,
                    tool: None,
                    last_line: None,
                    model: model.clone(),
                    since: bake_ai::now_ms(),
                    runs: 0,
                },
                commands: sender,
            });
            (id, name, receiver)
        };
        self.bump();
        let spec = self.spec(&name, model.as_deref());
        let weak = Arc::downgrade(&self.inner);
        let task = task.to_owned();
        let started = options.work.spawn(move |scope| async move {
            drive(weak, id, spec, task, receiver, scope).await;
        });
        if started.is_err() {
            self.update(id, |row| {
                row.state = SpawnedState::Exited(Some("the session is closing".to_owned()));
            });
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
        let agents = lock(&self.inner.agents);
        let entry = agents
            .iter()
            .find(|entry| entry.row.id == id)
            .ok_or("no such agent")?;
        if !entry.row.state.is_live() {
            return Err(format!("{} has exited", entry.row.name));
        }
        entry
            .commands
            .send(command)
            .map_err(|_| format!("{} has exited", entry.row.name))
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

    /// Ends agent `id`: its input closes and it shuts down.
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

/// Runs agent `id` until it exits or the session ends.
async fn drive(
    inner: Weak<Inner>,
    id: u64,
    spec: ProcessSpec,
    task: String,
    mut commands: mpsc::UnboundedReceiver<Command>,
    scope: TaskScope,
) {
    let Some(host) = inner.upgrade() else { return };
    let mut process = match OwnedProcess::spawn(&spec, None).await {
        Ok(process) => process,
        Err(error) => {
            update(&host, id, |row| {
                row.state = SpawnedState::Exited(Some(format!("could not start bake: {error}")));
            });
            return;
        }
    };
    let mut input = process.take_stdin();
    let Some(stdout) = process.take_stdout() else {
        update(&host, id, |row| {
            row.state = SpawnedState::Exited(Some("no output pipe".to_owned()));
        });
        process.kill();
        let _ = process.settle().await;
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
    write_record(
        &mut input,
        &json!({ "id": "bake-spawn-state", "type": "get_state" }),
    )
    .await;
    write_record(
        &mut input,
        &json!({ "id": "bake-spawn-task", "type": "prompt", "message": task }),
    )
    .await;

    let questions = host.options.questions.clone();
    let station = host.options.station.clone();
    let jobs = host.options.jobs.clone();
    drop(host);
    let mut follower = events::Follower::default();
    let (answers, mut answered) = mpsc::unbounded_channel::<Value>();
    let mut reader = BufReader::new(stdout);
    let mut stopping = false;
    loop {
        tokio::select! {
            () = scope.signal().cancelled(), if !stopping => {
                stopping = true;
                input = None;
            }
            line = next_line(&mut reader) => {
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
                        events::Effect::Settled { text, failed } => {
                            resolve_name(&host, id, station.as_ref());
                            let row = lock(&host.agents)
                                .iter()
                                .find(|entry| entry.row.id == id)
                                .map(|entry| entry.row.clone());
                            if let Some(row) = row.filter(|_| !stopping) {
                                let _ = jobs.post(settled_notice(&row, text, failed));
                            }
                        }
                        events::Effect::SessionId => resolve_name(&host, id, station.as_ref()),
                        events::Effect::Ask(request) => {
                            let name = lock(&host.agents)
                                .iter()
                                .find(|entry| entry.row.id == id)
                                .map(|entry| entry.row.name.clone())
                                .unwrap_or_default();
                            let questions = questions.clone();
                            let answers = answers.clone();
                            let signal = scope.signal().clone();
                            let weak = Arc::downgrade(&host);
                            side.spawn(async move {
                                if let Some(host) = weak.upgrade() {
                                    update(&host, id, |row| row.state = SpawnedState::Asking);
                                }
                                let response = request.forward(&name, questions.as_ref(), signal).await;
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
                    write_record(&mut input, &json!({
                        "type": "prompt", "message": text, "streamingBehavior": behavior,
                    })).await;
                }
                Command::Abort => {
                    write_record(&mut input, &json!({ "type": "abort" })).await;
                }
                Command::Stop => {
                    stopping = true;
                    input = None;
                }
            },
            Some(_) = side.join_next(), if !side.is_empty() => {}
        }
    }
    // The output ended: the agent is exiting, or was told to. Give it its
    // grace, then end its tree.
    drop(input);
    if tokio::time::timeout(STOP_GRACE, process.wait_exit())
        .await
        .is_err()
    {
        process.kill();
    }
    let settled = process.settle().await;
    // The tree is gone, so standard error reaches its end; let the reader
    // take what is left, which says why a failed agent stopped.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while side.join_next().await.is_some() {}
    })
    .await;
    side.abort_all();
    while side.join_next().await.is_some() {}
    let tail = String::from_utf8_lossy(&lock(&stderr_tail).iter().copied().collect::<Vec<_>>())
        .trim()
        .to_owned();
    let reason = match settled {
        _ if stopping => None,
        Err(error) => Some(error.to_string()),
        Ok(settled) if settled.status.success() => None,
        Ok(settled) => Some(failure(&tail, settled.status)),
    };
    if let Some(host) = inner.upgrade() {
        update(&host, id, |row| {
            row.state = SpawnedState::Exited(reason);
            row.tool = None;
            row.since = bake_ai::now_ms();
        });
    }
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

fn settled_notice(
    agent: &SpawnedAgent,
    text: Option<String>,
    failed: Option<String>,
) -> CustomMessage {
    let name = &agent.name;
    let starter = match agent.origin {
        Origin::Model => "you",
        Origin::Person => "the person (with /spawn)",
    };
    let who = format!("{name}, which {starter} started on \"{}\",", agent.task);
    let body = match (&failed, text) {
        (Some(reason), _) => format!("[spawn] {who} failed its run: {reason}"),
        (None, Some(text)) => {
            let mut text = text.trim().to_owned();
            if text.len() > NOTICE_TEXT_CAP {
                let mut cut = NOTICE_TEXT_CAP;
                while !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                text.truncate(cut);
                text.push_str("\n[...]");
            }
            format!("[spawn] {who} finished a run and is idle. Its last answer:\n{text}")
        }
        (None, None) => format!("[spawn] {who} finished a run and is idle, with no answer."),
    };
    notice(
        body,
        json!({ "agent": name, "status": if failed.is_some() { "failed" } else { "idle" } }),
    )
}

#[cfg(all(test, unix))]
mod tests;
