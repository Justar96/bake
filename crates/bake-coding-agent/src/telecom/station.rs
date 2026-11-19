//! One session's telecom station: its membership, the frames it receives,
//! the asks it waits on and owes, and the brief its model gets.
//!
//! The station is started with the session and owned by its
//! [`WorkGate`]: the socket server and the presence publisher are owned
//! tasks, so shutdown stops them and awaits them, and [`Station::leave`]
//! (or the last reference going) gives the callsign back. What arrives is
//! posted to the session's [`JobRegistry`] as a [`MESSAGE_TYPE`] message,
//! delivered as job notices are: inside a run at its next step, else by a
//! turn its owner opens within the wake budget. Broadcasts never start a
//! turn; they are held for the next prompt's brief.
//!
//! A waiting `ask` never holds a session that has something to read: it
//! ends at the answer, cancellation, its deadline, its peer's session
//! leaving, or this session's own undelivered mail, whichever comes first.
//! A cycle of waiting asks needs a member blocked while it holds an unread
//! ask, so none can form. An ask that ends unanswered stays open, and its
//! answer arrives as a message; an answer sent as a wait ends is taken, not
//! dropped. The directory reads of deliveries and waits, and the
//! publisher's writes, run on the blocking pool under the session's
//! [`WorkGate`]; no write is made under a lock another task takes.
//!
//! **One wake budget.** What a peer's model sends, a message or an ask,
//! may start a turn at most [`MAX_WAKES_PER_MINUTE`] times in any minute,
//! shared by every peer. Past that it is still accepted, and the sender is
//! told it is held: a run reads it at its next step as any notice, and an
//! idle session is woken for it by one timer, owned by the session's gate,
//! when the minute's window reopens. A person's message and the answer to
//! this session's own ask always wake it.
//!
//! **Nothing is accepted that cannot be read.** A frame is taken under the
//! membership lock, and the session leaves the team before its notices
//! close, so a frame is refused once its mail could no longer be read. An
//! ask this session made and no longer waits on holds the session's live
//! work ([`JobRegistry::hold_live`]) until it is answered, withdrawn, or
//! its peer leaves, which a watcher task reports as a message; print mode
//! therefore waits for the answer within its job wait.
//!
//! **The team update** (who joined or left, new tasks, broadcasts, owed
//! asks) reaches the model before a prompt, in a turn opened for notices,
//! and at a run's step boundary ([`JobRegistry::set_update_source`]); it
//! never starts a turn of its own.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::task::Poll;
use std::time::Duration;

use serde_json::json;
use tokio::sync::{Notify, oneshot};

use super::directory::{self, Membership, NATO, Team};
use super::endpoint;
use super::wire::{self, Activity, Body, Frame, Outcome, Presence, Sender, WIRE_VERSION};
use crate::agent_session::SessionEvent;
use crate::jobs::{JobRegistry, LiveHold};
use crate::owned_work::WorkGate;
use crate::session::messages::CustomMessage;
use bake_ai::{TextContent, UserContent, UserContentBlock};

/// The `customType` of what telecom puts in a session.
pub const MESSAGE_TYPE: &str = "bake.telecom";

/// Turns peers' models' messages and asks may start per minute, together;
/// a person's messages and answers to this session's asks always may.
pub const MAX_WAKES_PER_MINUTE: usize = 6;

/// The window [`MAX_WAKES_PER_MINUTE`] counts in.
const WAKE_WINDOW: Duration = Duration::from_secs(60);

/// The most broadcasts held for the next update; older ones are dropped.
pub const HELD_CAP: usize = 20;

/// How long an `ask` waits by default, and at most.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(300);
pub const ASK_TIMEOUT_MAX: Duration = Duration::from_secs(3600);

/// How often a waiting ask checks that its peer is still there.
const PEER_CHECK: Duration = Duration::from_secs(2);

/// What a station is started with.
#[derive(Debug, Clone)]
pub struct StationOptions {
    /// The telecom root ([`directory::root`]).
    pub root: PathBuf,
    /// Where the session works.
    pub cwd: PathBuf,
    pub session_id: String,
    /// A callsign to take when free (`BAKE_TELECOM_CALLSIGN`).
    pub callsign: Option<String>,
    /// The callsign of the member that spawned this session
    /// (`BAKE_TELECOM_PARENT`).
    pub parent: Option<String>,
    /// `provider/model`.
    pub model: Option<String>,
}

/// An ask this station owes an answer to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owed {
    pub ask_id: String,
    pub from: String,
    pub from_session: String,
    pub text: String,
    pub at: i64,
}

/// How many asks this station stopped owing unanswered it remembers, so
/// a later reply to one is told why ([`Station::reply`]).
const CLOSED_KEPT: usize = 32;

/// An ask this station made and has no answer to.
struct Waiting {
    /// The peer's callsign, as the model reads it.
    to: String,
    /// The session's live work while nobody waits on the answer, so print
    /// mode waits for it ([`JobRegistry::hold_live`]).
    hold: Option<LiveHold>,
    /// The peer's session: only it may answer, and the ask fails when it
    /// leaves, whoever takes the callsign next.
    session: String,
    /// The tool call waiting for it; `None` once nobody waits, so a late
    /// answer arrives as a message.
    answer: Option<oneshot::Sender<String>>,
}

/// An ask this station owed and stopped owing without answering it: its
/// asker left or withdrew it.
struct Closed {
    ask_id: String,
    from: String,
    /// What a reply to it is told.
    note: String,
}

/// What the last brief told the model about one peer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    task: Option<String>,
}

struct State {
    presence: Presence,
    /// Set by `status`; a prompt does not replace it.
    task_pinned: bool,
    waiting: HashMap<String, Waiting>,
    owed: Vec<Owed>,
    /// The latest asks closed unanswered, at most [`CLOSED_KEPT`].
    closed: VecDeque<Closed>,
    /// Broadcasts held for the next update, at most [`HELD_CAP`].
    held: Vec<String>,
    /// How many older broadcasts were dropped for the cap.
    dropped: usize,
    /// The roster the last brief described; `None` before the first.
    briefed: Option<BTreeMap<String, Seen>>,
    /// When peers' models last woke the session, in the last minute.
    wakes: VecDeque<tokio::time::Instant>,
    /// Whether the timer that releases held mail is pending.
    waking: bool,
    /// Whether the task that watches the peers of unwaited asks runs.
    watching: bool,
}

impl State {
    fn close_owed(&mut self, ask: &Owed, note: String) {
        self.owed.retain(|owed| owed.ask_id != ask.ask_id);
        if self.closed.len() == CLOSED_KEPT {
            self.closed.pop_front();
        }
        self.closed.push_back(Closed {
            ask_id: ask.ask_id.clone(),
            from: ask.from.clone(),
            note,
        });
    }
}

struct Inner {
    /// Shared so the publisher writes outside the lock; the last holder's
    /// drop gives the callsign back, after any write in progress.
    membership: Mutex<Option<Arc<Membership>>>,
    team: Team,
    state: Mutex<State>,
    jobs: JobRegistry,
    /// The session's gate: it owns the station's tasks and blocking reads.
    work: WorkGate,
    dirty: Notify,
}

/// A session's station; cheap to clone.
#[derive(Clone)]
pub struct Station {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Station {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Station")
            .field("callsign", &self.callsign())
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn short_id() -> String {
    crate::session::id::uuid_v4()[..8].to_owned()
}

/// The callsigns a member tries, in order: the one asked for, then a
/// spawned member's `<parent>-<n>`, else the NATO words and their
/// numbered forms.
fn candidates(wanted: Option<&str>, parent: Option<&str>) -> Vec<String> {
    let mut words: Vec<String> = wanted.into_iter().map(str::to_owned).collect();
    match parent {
        Some(parent) => {
            let stem: String = parent.chars().take(26).collect();
            words.extend((1..=99).map(|n| format!("{stem}-{n}")));
        }
        None => {
            words.extend(NATO.iter().map(|word| (*word).to_owned()));
            for n in 2..=9 {
                words.extend(NATO.iter().map(|word| format!("{word}{n}")));
            }
        }
    }
    words
}

impl Station {
    /// Joins the team of `options.cwd`, posting what arrives to `jobs`,
    /// with its tasks owned by `work`.
    pub fn start(
        options: StationOptions,
        jobs: JobRegistry,
        work: &WorkGate,
    ) -> Result<Self, String> {
        #[cfg(not(unix))]
        {
            let _ = (options, jobs, work);
            return Err(super::UNSUPPORTED.to_owned());
        }
        #[cfg(unix)]
        {
            let (common, checkout) = directory::locate(&options.cwd);
            let team = Team::open(&options.root, &directory::team_key(&common))
                .map_err(|error| format!("telecom: {error}"))?;
            let words = candidates(options.callsign.as_deref(), options.parent.as_deref());
            let membership = team
                .claim(words.iter().map(String::as_str))
                .map_err(|error| format!("telecom: {error}"))?
                .ok_or("telecom: every callsign is taken")?;
            membership.clear_socket();
            let listener = endpoint::Listener::bind(&team.socket(membership.callsign()))
                .map_err(|error| format!("telecom: cannot listen: {error}"))?;
            let now = bake_ai::now_ms();
            let presence = Presence {
                v: WIRE_VERSION,
                session: options.session_id,
                callsign: membership.callsign().to_owned(),
                pid: std::process::id(),
                checkout: checkout.display().to_string(),
                branch: directory::branch(&checkout),
                task: None,
                activity: Activity::Idle,
                since: now,
                tool: None,
                parent: options.parent.filter(|parent| wire::is_callsign(parent)),
                model: options.model,
                started_at: now,
            };
            membership
                .publish(&presence)
                .map_err(|error| format!("telecom: {error}"))?;
            let station = Self {
                inner: Arc::new(Inner {
                    membership: Mutex::new(Some(Arc::new(membership))),
                    team,
                    state: Mutex::new(State {
                        presence,
                        task_pinned: false,
                        waiting: HashMap::new(),
                        owed: Vec::new(),
                        closed: VecDeque::new(),
                        held: Vec::new(),
                        dropped: 0,
                        briefed: None,
                        wakes: VecDeque::new(),
                        waking: false,
                        watching: false,
                    }),
                    jobs,
                    work: work.clone(),
                    dirty: Notify::new(),
                }),
            };
            let weak = Arc::downgrade(&station.inner);
            let handler: endpoint::Handler = Arc::new(move |frame| match weak.upgrade() {
                Some(inner) => Self { inner }.receive(frame),
                None => Outcome::Refused {
                    reason: "the session has ended".to_owned(),
                },
            });
            // The gate owns and awaits both tasks; their outputs are unused.
            let _serve = work
                .spawn(move |scope| listener.serve(handler, scope))
                .map_err(|_| "telecom: the session is closing".to_owned())?;
            let weak = Arc::downgrade(&station.inner);
            let _publisher = work
                .spawn(move |scope| publisher(weak, scope))
                .map_err(|_| "telecom: the session is closing".to_owned())?;
            let weak = Arc::downgrade(&station.inner);
            station.inner.jobs.set_update_source(Arc::new(move || {
                weak.upgrade().and_then(|inner| Self { inner }.update())
            }));
            Ok(station)
        }
    }

    pub fn callsign(&self) -> String {
        lock(&self.inner.state).presence.callsign.clone()
    }

    /// This member's record as it publishes it.
    pub fn presence(&self) -> Presence {
        lock(&self.inner.state).presence.clone()
    }

    /// The live members other than this one.
    pub fn peers(&self) -> Vec<Presence> {
        if lock(&self.inner.membership).is_none() {
            return Vec::new();
        }
        let me = self.callsign();
        self.inner.team.members(Some(&me))
    }

    /// [`Self::peers`], read on the blocking pool under the session's gate;
    /// inline once the gate is closing.
    async fn live_peers(&self) -> Vec<Presence> {
        if lock(&self.inner.membership).is_none() {
            return Vec::new();
        }
        let team = self.inner.team.clone();
        let me = self.callsign();
        match self
            .inner
            .work
            .spawn_blocking(move || team.members(Some(&me)))
        {
            Ok(read) => read.await.unwrap_or_default(),
            Err(_) => self.peers(),
        }
    }

    /// Forgets the asks owed to members no longer in `peers`, remembering
    /// why ([`Self::reply`]).
    fn prune_owed(&self, peers: &[Presence]) {
        if lock(&self.inner.membership).is_none() {
            return;
        }
        let mut state = lock(&self.inner.state);
        let gone: Vec<Owed> = state
            .owed
            .iter()
            .filter(|ask| !peers.iter().any(|peer| peer.session == ask.from_session))
            .cloned()
            .collect();
        for ask in gone {
            let note = format!(
                "{} is gone; nothing more to do for ask {}",
                ask.from, ask.ask_id
            );
            state.close_owed(&ask, note);
        }
    }

    /// Why an ask matching `ask_id`, else from `from`, is no longer owed.
    fn closed_note(&self, ask_id: Option<&str>, from: Option<&str>) -> Option<String> {
        let state = lock(&self.inner.state);
        state
            .closed
            .iter()
            .rev()
            .find(|closed| match (ask_id, from) {
                (Some(id), _) => closed.ask_id == id,
                (None, Some(from)) => closed.from == from.trim(),
                (None, None) => false,
            })
            .map(|closed| closed.note.clone())
    }

    /// The asks waiting on this member, oldest first.
    pub fn owed(&self) -> Vec<Owed> {
        lock(&self.inner.state).owed.clone()
    }

    /// Gives the callsign back; later deliveries fail. Idempotent. A
    /// frame being taken finishes first ([`Self::receive`]), so whatever
    /// was accepted is in the session's notices when this returns. Open
    /// asks end, and their hold on the session's live work with them.
    pub fn leave(&self) {
        let membership = lock(&self.inner.membership).take();
        drop(membership);
        let mut state = lock(&self.inner.state);
        for (_, waiting) in state.waiting.drain() {
            drop(waiting);
        }
    }

    fn sender(&self) -> Sender {
        let state = lock(&self.inner.state);
        Sender {
            session: state.presence.session.clone(),
            callsign: state.presence.callsign.clone(),
        }
    }

    fn mark_dirty(&self) {
        self.inner.dirty.notify_one();
    }

    /// Follows the session's runs: busy from the agent's start to the
    /// prompt's settlement, with the top-level tool call it is in.
    pub fn observe(&self, event: &SessionEvent<'_>) {
        use bake_agent::AgentEvent;
        let mut state = lock(&self.inner.state);
        let presence = &mut state.presence;
        let changed = match event {
            SessionEvent::Agent(AgentEvent::AgentStart) => {
                let changed = presence.activity != Activity::Busy;
                if changed {
                    presence.activity = Activity::Busy;
                    presence.since = bake_ai::now_ms();
                }
                changed
            }
            SessionEvent::Agent(AgentEvent::ToolExecutionStart {
                tool_name,
                parent_tool_call_id: None,
                ..
            }) => {
                presence.tool = Some(wire::one_line(tool_name, 64));
                true
            }
            SessionEvent::Agent(AgentEvent::ToolExecutionEnd {
                parent_tool_call_id: None,
                ..
            }) => presence.tool.take().is_some(),
            SessionEvent::AgentSettled { .. } => {
                presence.activity = Activity::Idle;
                presence.since = bake_ai::now_ms();
                presence.tool = None;
                true
            }
            _ => false,
        };
        drop(state);
        if changed {
            self.mark_dirty();
        }
    }

    /// Sets the task peers see. Returns it as published.
    pub fn set_status(&self, task: &str) -> String {
        let task = wire::one_line(task, wire::TASK_CAP);
        {
            let mut state = lock(&self.inner.state);
            state.presence.task = (!task.is_empty()).then(|| task.clone());
            state.task_pinned = !task.is_empty();
        }
        self.mark_dirty();
        task
    }

    /// The brief to put before a typed prompt: [`Self::take_task`], then
    /// [`Self::update`].
    pub fn brief(&self, prompt: &str) -> Option<CustomMessage> {
        self.take_task(prompt);
        self.update()
    }

    /// The prompt part of [`Self::brief`]: a typed prompt of three words or
    /// more becomes the task unless `status` set one.
    pub fn take_task(&self, prompt: &str) {
        let mut state = lock(&self.inner.state);
        let line = prompt.lines().find(|line| !line.trim().is_empty());
        if let Some(line) = line.filter(|_| !state.task_pinned)
            && prompt.split_whitespace().count() >= 3
        {
            state.presence.task = Some(wire::one_line(line, 140));
            drop(state);
            self.mark_dirty();
        }
    }

    /// The team update of [`Self::brief`], for any request a run makes:
    /// the whole team the first time there is one, later only what
    /// changed, with held broadcasts and the asks this member owes. `None`
    /// when there is nothing to say.
    pub fn update(&self) -> Option<CustomMessage> {
        if lock(&self.inner.membership).is_none() {
            return None;
        }
        let peers = self.peers();
        self.prune_owed(&peers);
        let mut state = lock(&self.inner.state);
        let roster: BTreeMap<String, Seen> = peers
            .iter()
            .map(|peer| {
                (
                    peer.callsign.clone(),
                    Seen {
                        task: peer.task.clone(),
                    },
                )
            })
            .collect();
        let me = state.presence.callsign.clone();
        let mut lines = Vec::new();
        match &state.briefed {
            None if peers.is_empty() => {}
            None => {
                let count = peers.len();
                lines.push(format!(
                    "[telecom] You are {me}. {count} other agent{} in this repository:",
                    if count == 1 { " works" } else { "s work" }
                ));
                lines.extend(peers.iter().map(board_line));
                lines.push(
                    "Use the telecom tool to coordinate when your work overlaps theirs: send for requests and handoffs, ask when you need an answer to go on."
                        .to_owned(),
                );
            }
            Some(before) => {
                let mut changes = Vec::new();
                for peer in &peers {
                    match before.get(&peer.callsign) {
                        None => {
                            changes.push(format!("- {} joined -- {}", peer.callsign, about(peer)))
                        }
                        Some(seen) if seen.task != peer.task => {
                            if let Some(task) = &peer.task {
                                changes.push(format!("- {} is now on: {task}", peer.callsign));
                            }
                        }
                        Some(_) => {}
                    }
                }
                for gone in before
                    .keys()
                    .filter(|callsign| !roster.contains_key(*callsign))
                {
                    changes.push(format!("- {gone} left"));
                }
                if !changes.is_empty() {
                    lines.push(format!("[telecom] Team update (you are {me}):"));
                    lines.extend(changes);
                }
            }
        }
        let held = std::mem::take(&mut state.held);
        let dropped = std::mem::take(&mut state.dropped);
        if !held.is_empty() {
            lines.push("[telecom] New broadcasts:".to_owned());
            if dropped > 0 {
                lines.push(format!("({dropped} earlier broadcasts were dropped.)"));
            }
            lines.extend(held);
        }
        if !state.owed.is_empty() && !lines.is_empty() {
            let owed: Vec<String> = state
                .owed
                .iter()
                .map(|ask| format!("{} from {}", ask.ask_id, ask.from))
                .collect();
            lines.push(format!("Asks waiting on your reply: {}.", owed.join(", ")));
        }
        if state.briefed.is_some() || !peers.is_empty() {
            state.briefed = Some(roster);
        }
        if lines.is_empty() {
            return None;
        }
        Some(message(lines.join("\n"), false, json!({ "kind": "brief" })))
    }

    /// The board: this member, its peers, and the asks waiting on it.
    pub fn board(&self) -> String {
        let me = self.presence();
        let peers = self.peers();
        self.prune_owed(&peers);
        let mut lines = vec![format!(
            "You are {}.",
            board_line(&me).trim_start_matches("- ")
        )];
        if peers.is_empty() {
            lines.push("No other agent works in this repository.".to_owned());
        } else {
            lines.extend(peers.iter().map(board_line));
        }
        let owed = self.owed();
        if !owed.is_empty() {
            lines.push("Asks waiting on your reply:".to_owned());
            lines.extend(owed.iter().map(|ask| {
                format!(
                    "  {} from {}: {}",
                    ask.ask_id,
                    ask.from,
                    wire::one_line(&ask.text, 120)
                )
            }));
        }
        lines.join("\n")
    }

    /// The peer `to` names: a callsign, or a session id or its prefix of
    /// at least 4 characters.
    pub fn resolve(&self, to: &str) -> Result<Presence, String> {
        pick(&self.peers(), &self.callsign(), to)
    }

    /// Delivers `body` to `peer`: whether it holds it past its wake
    /// budget, or why it did not take it.
    async fn deliver(&self, peer: &Presence, body: Body) -> Result<bool, String> {
        match self.exchange(peer, body).await? {
            Outcome::Accepted { held } => Ok(held),
            Outcome::Refused { reason } => Err(reason),
        }
    }

    /// Delivers `body` to `peer`'s session: what it answered, or why it
    /// could not. Whoever holds the callsign now refuses it unless it is
    /// that session.
    async fn exchange(&self, peer: &Presence, body: Body) -> Result<Outcome, String> {
        let mut frame = Frame::new(self.sender(), body);
        frame.to = Some(peer.session.clone());
        endpoint::deliver(&self.inner.team.socket(&peer.callsign), &frame).await
    }

    /// Stops waiting on the ask `ask_id` if it is still open: its answer
    /// arrives as a message, and until then the ask holds the session's
    /// live work and the watcher reports its peer leaving.
    fn stop_waiting(&self, ask_id: &str) {
        let mut state = lock(&self.inner.state);
        let Some(waiting) = state.waiting.get_mut(ask_id) else {
            return;
        };
        waiting.answer = None;
        if waiting.hold.is_none() {
            waiting.hold = self.inner.jobs.hold_live();
        }
        self.arm_watcher(&mut state);
    }

    /// Starts the watcher of unwaited asks' peers, unless it runs.
    fn arm_watcher(&self, state: &mut State) {
        if state.watching {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        state.watching = self
            .inner
            .work
            .spawn(move |scope| watcher(weak, scope))
            .is_ok();
    }

    /// Starts the timer that releases held mail, unless it is pending. It
    /// holds the session's live work, so print mode waits for it.
    fn arm_waker(&self, state: &mut State) {
        if state.waking {
            return;
        }
        let hold = self.inner.jobs.hold_live();
        let weak = Arc::downgrade(&self.inner);
        state.waking = self
            .inner
            .work
            .spawn(move |scope| waker(weak, hold, scope))
            .is_ok();
    }

    /// Withdraws `ask_id` from `peer` on a task the session's gate owns,
    /// so the caller does not wait on a wedged peer.
    fn withdraw(&self, peer: &Presence, ask_id: String) {
        let station = self.clone();
        let peer = peer.clone();
        // Refused only while the session closes, when nobody reads it.
        let _withdrawal = self.inner.work.spawn(move |scope| async move {
            tokio::select! {
                () = scope.signal().cancelled() => {}
                _ = station.deliver(&peer, Body::Withdraw { ask_id }) => {}
            }
        });
    }

    /// Messages each of `to` (comma-separated callsigns or session ids),
    /// each named once: concurrently, in batches of [`wire::GROUP_CAP`],
    /// where each recipient sees the others of its batch as the group.
    /// Returns who got it and who did not.
    pub async fn send(&self, to: &str, text: &str, human: bool) -> Result<String, String> {
        let text = checked_text(text)?;
        let mut names: Vec<&str> = to
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect();
        let mut seen = HashSet::new();
        names.retain(|name| seen.insert(*name));
        if names.is_empty() {
            return Err("to: name an agent".to_owned());
        }
        let live = self.live_peers().await;
        let me = self.callsign();
        let mut peers = names
            .iter()
            .map(|name| pick(&live, &me, name))
            .collect::<Result<Vec<_>, _>>()?;
        // A callsign and a session id may name the same member.
        let mut sessions = HashSet::new();
        peers.retain(|peer| sessions.insert(peer.session.clone()));
        let mut reached = Vec::new();
        let mut held = Vec::new();
        let mut failed = Vec::new();
        for batch in peers.chunks(wire::GROUP_CAP) {
            let deliveries = batch.iter().map(|peer| {
                let group = batch
                    .iter()
                    .filter(|other| other.session != peer.session)
                    .map(|other| other.callsign.clone())
                    .collect();
                let body = Body::Message {
                    text: text.clone(),
                    group,
                    human,
                };
                async move { (peer, self.deliver(peer, body).await) }
            });
            for (peer, result) in join_all(deliveries.collect()).await {
                match result {
                    Ok(was_held) => {
                        reached.push(peer.callsign.clone());
                        if was_held {
                            held.push(peer.callsign.clone());
                        }
                    }
                    Err(reason) => failed.push(format!("{}: {reason}", peer.callsign)),
                }
            }
        }
        if reached.is_empty() {
            return Err(format!("not delivered ({})", failed.join("; ")));
        }
        let mut report = format!("delivered to {}", reached.join(", "));
        if !held.is_empty() {
            let verb = if held.len() == 1 { "reads" } else { "read" };
            report.push_str(&format!(
                " (held: {} {verb} it within a minute)",
                held.join(", ")
            ));
        }
        if !failed.is_empty() {
            report.push_str(&format!(" (failed: {})", failed.join("; ")));
        }
        Ok(report)
    }

    /// Messages every peer, concurrently; it starts no turn. Returns who
    /// got it and who did not.
    pub async fn broadcast(&self, text: &str) -> Result<String, String> {
        let text = checked_text(text)?;
        let peers = self.live_peers().await;
        if peers.is_empty() {
            return Err("no other agent works in this repository".to_owned());
        }
        let deliveries = peers.iter().map(|peer| {
            let body = Body::Broadcast { text: text.clone() };
            async move { (peer, self.deliver(peer, body).await) }
        });
        let mut reached = Vec::new();
        let mut failed = Vec::new();
        for (peer, result) in join_all(deliveries.collect()).await {
            match result {
                Ok(_) => reached.push(peer.callsign.clone()),
                Err(reason) => failed.push(format!("{}: {reason}", peer.callsign)),
            }
        }
        if reached.is_empty() {
            return Err(format!("not delivered ({})", failed.join("; ")));
        }
        let mut report = format!("broadcast to {}", reached.join(", "));
        if !failed.is_empty() {
            report.push_str(&format!(" (failed: {})", failed.join("; ")));
        }
        Ok(report)
    }

    /// Asks `to` and, with `wait`, waits up to `timeout` for the answer,
    /// ending early when the peer's session leaves, the call is cancelled,
    /// or mail waits for this session ([`AskOutcome::Interrupted`]).
    /// Without `wait`, or once the wait ends without an answer, the ask
    /// stays open and its answer arrives later as a message; cancellation
    /// or the peer leaving closes it.
    pub async fn ask(
        &self,
        to: &str,
        text: &str,
        timeout: Duration,
        wait: bool,
        signal: Option<bake_ai::AbortSignal>,
    ) -> Result<AskOutcome, String> {
        let text = checked_text(text)?;
        let peers = self.live_peers().await;
        let peer = pick(&peers, &self.callsign(), to)?;
        let ask_id = short_id();
        let (answer, mut answered) = oneshot::channel();
        lock(&self.inner.state).waiting.insert(
            ask_id.clone(),
            Waiting {
                to: peer.callsign.clone(),
                hold: if wait {
                    None
                } else {
                    self.inner.jobs.hold_live()
                },
                session: peer.session.clone(),
                answer: wait.then_some(answer),
            },
        );
        let body = Body::Ask {
            ask_id: ask_id.clone(),
            text,
        };
        let held = match self.deliver(&peer, body).await {
            Ok(held) => held,
            Err(reason) => {
                lock(&self.inner.state).waiting.remove(&ask_id);
                return Err(format!("{} did not get the ask: {reason}", peer.callsign));
            }
        };
        if !wait {
            self.stop_waiting(&ask_id);
            return Ok(AskOutcome::Sent {
                ask_id,
                to: peer.callsign,
                held,
            });
        }
        let outcome = self
            .await_answer(&peer, &mut answered, timeout.min(ASK_TIMEOUT_MAX), signal)
            .await;
        match outcome {
            AskOutcome::Answered { .. } | AskOutcome::Sent { .. } => {}
            AskOutcome::TimedOut { .. } | AskOutcome::Interrupted { .. } => {
                // Nobody waits now: a late answer arrives as a message.
                self.stop_waiting(&ask_id);
            }
            AskOutcome::Gone { .. } => {
                lock(&self.inner.state).waiting.remove(&ask_id);
            }
            AskOutcome::Cancelled { .. } => {
                lock(&self.inner.state).waiting.remove(&ask_id);
                self.withdraw(&peer, ask_id.clone());
            }
        }
        Ok(outcome.with_id(ask_id))
    }

    /// Waits for `answered` until the first of: cancellation (this
    /// session's own shutdown included), the answer, undelivered mail for
    /// this session, the deadline, or `peer`'s session leaving. Whatever
    /// ends it, an answer already sent is taken: `answered` is closed
    /// first, so a reply after that is posted as a message instead.
    async fn await_answer(
        &self,
        peer: &Presence,
        answered: &mut oneshot::Receiver<String>,
        timeout: Duration,
        signal: Option<bake_ai::AbortSignal>,
    ) -> AskOutcome {
        let to = peer.callsign.clone();
        let deadline = tokio::time::Instant::now() + timeout;
        let cancelled = async {
            match &signal {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(cancelled);
        // Subscribed before the first look at the mail, so nothing posted
        // after that look is missed.
        let mut events = self.inner.jobs.subscribe();
        let mut check =
            tokio::time::interval_at(tokio::time::Instant::now() + PEER_CHECK, PEER_CHECK);
        check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let ended = loop {
            if self.inner.jobs.has_undelivered() {
                break AskOutcome::Interrupted {
                    ask_id: String::new(),
                    to: to.clone(),
                };
            }
            tokio::select! {
                biased;
                () = &mut cancelled => break AskOutcome::Cancelled { ask_id: String::new() },
                answer = &mut *answered => {
                    return match answer {
                        Ok(text) => AskOutcome::Answered { ask_id: String::new(), from: to, text },
                        // Only `leave` drops a waiter unanswered: this
                        // session is closing.
                        Err(_) => AskOutcome::Cancelled { ask_id: String::new() },
                    };
                }
                open = events.changed() => {
                    if !open {
                        // The registry closed: this session is closing.
                        break AskOutcome::Cancelled { ask_id: String::new() };
                    }
                }
                () = tokio::time::sleep_until(deadline) => {
                    break AskOutcome::TimedOut { ask_id: String::new(), to: to.clone() };
                }
                _ = check.tick() => {
                    if !self.live_peers().await.iter().any(|live| live.session == peer.session) {
                        break AskOutcome::Gone { ask_id: String::new(), to: to.clone() };
                    }
                }
            }
        };
        answered.close();
        match answered.try_recv() {
            Ok(text) => AskOutcome::Answered {
                ask_id: String::new(),
                from: to,
                text,
            },
            Err(_) => ended,
        }
    }

    /// Answers an ask: `ask_id`, else the only one waiting, else the only
    /// one from `to`. With `to` and no ask of theirs open, the text goes
    /// as a message. An ask whose asker left, withdrew it, or refuses the
    /// answer is closed, and replying to it says so without failing.
    pub async fn reply(
        &self,
        text: &str,
        ask_id: Option<&str>,
        to: Option<&str>,
    ) -> Result<String, String> {
        let text = checked_text(text)?;
        let peers = self.live_peers().await;
        self.prune_owed(&peers);
        let chosen = {
            let state = lock(&self.inner.state);
            let open: Vec<&Owed> = state
                .owed
                .iter()
                .filter(|ask| ask_id.is_none_or(|id| ask.ask_id == id))
                .filter(|ask| to.is_none_or(|to| ask.from == to.trim()))
                .collect();
            match open.as_slice() {
                [ask] => Ok((*ask).clone()),
                _ => Err(open.len()),
            }
        };
        let ask = match chosen {
            Ok(ask) => ask,
            Err(0) => {
                let named_live =
                    to.is_some_and(|to| peers.iter().any(|peer| peer.callsign == to.trim()));
                if let Some(note) = self.closed_note(ask_id, to.filter(|_| !named_live)) {
                    return Ok(note);
                }
                if let (Some(to), None) = (to, ask_id) {
                    return self.send(to, &text, false).await;
                }
                return Err(match ask_id {
                    Some(id) => format!("no open ask {id}; it may have been withdrawn or answered"),
                    None => "no ask is waiting on you".to_owned(),
                });
            }
            Err(_) => {
                let owed: Vec<String> = self
                    .owed()
                    .iter()
                    .map(|ask| format!("{} from {}", ask.ask_id, ask.from))
                    .collect();
                return Err(format!(
                    "several asks are waiting ({}); name one with askId",
                    owed.join(", ")
                ));
            }
        };
        let gone = || {
            format!(
                "{} is gone; nothing more to do for ask {}",
                ask.from, ask.ask_id
            )
        };
        let Some(peer) = peers.iter().find(|peer| peer.session == ask.from_session) else {
            let note = gone();
            lock(&self.inner.state).close_owed(&ask, note.clone());
            return Ok(note);
        };
        let body = Body::Reply {
            ask_id: ask.ask_id.clone(),
            text,
        };
        match self.exchange(peer, body).await {
            Ok(Outcome::Accepted { .. }) => {
                lock(&self.inner.state)
                    .owed
                    .retain(|owed| owed.ask_id != ask.ask_id);
                Ok(format!("answered {} ({})", ask.from, ask.ask_id))
            }
            Ok(Outcome::Refused { reason }) => {
                let note = format!(
                    "{} no longer waits on ask {} ({reason}); nothing more to do",
                    ask.from, ask.ask_id
                );
                lock(&self.inner.state).close_owed(&ask, note.clone());
                Ok(note)
            }
            Err(reason) => Err(format!("{} did not get the answer: {reason}", ask.from)),
        }
    }

    /// Takes an arriving frame. The membership lock is held throughout, so
    /// [`Self::leave`] waits for a frame being taken and none is accepted
    /// after it.
    fn receive(&self, frame: Frame) -> Outcome {
        let membership = lock(&self.inner.membership);
        if membership.is_none() {
            return refused("telecom is off in that session");
        }
        let from = frame.from.callsign.clone();
        let mut state = lock(&self.inner.state);
        if frame
            .to
            .as_ref()
            .is_some_and(|to| *to != state.presence.session)
        {
            return refused("that member left");
        }
        // An unwaited ask's hold on the session's live work, kept until its
        // answer is posted.
        let mut _answered_hold = None;
        // Whether it spends the shared wake budget: what a peer's model
        // sends. A person's message and an answer always wake.
        let (text, budgeted, details) = match frame.body {
            Body::Message { text, group, human } => {
                let header = match (human, group.is_empty()) {
                    (true, _) => format!("[telecom] Message from the person at {from}'s terminal:"),
                    (false, true) => format!("[telecom] Message from {from}:"),
                    (false, false) => format!(
                        "[telecom] Message from {from} (also to {}):",
                        group.join(", ")
                    ),
                };
                (
                    format!("{header}\n{text}"),
                    !human,
                    json!({ "kind": "message", "from": from, "human": human }),
                )
            }
            Body::Ask { ask_id, text } => {
                if state.owed.iter().any(|ask| ask.ask_id == ask_id) {
                    return Outcome::Accepted { held: false };
                }
                state.owed.push(Owed {
                    ask_id: ask_id.clone(),
                    from: from.clone(),
                    from_session: frame.from.session.clone(),
                    text: text.clone(),
                    at: frame.sent_at,
                });
                (
                    format!(
                        "[telecom] {from} asks you (ask {ask_id}) and waits for your answer:\n{text}\nAnswer with telecom action \"reply\", askId \"{ask_id}\"."
                    ),
                    true,
                    json!({ "kind": "ask", "from": from, "askId": ask_id }),
                )
            }
            Body::Reply { ask_id, text } => {
                // An answer nobody waits on now, or to an ask this station
                // no longer holds (withdrawn, or ended with its wait), is
                // still the model's to read: it is posted as a message.
                let (text, earlier) = match state.waiting.remove(&ask_id) {
                    Some(waiting) if waiting.session != frame.from.session => {
                        state.waiting.insert(ask_id, waiting);
                        return refused("that ask was not put to you");
                    }
                    Some(waiting) => {
                        _answered_hold = waiting.hold;
                        match waiting.answer {
                            Some(answer) => match answer.send(text) {
                                Ok(()) => return Outcome::Accepted { held: false },
                                Err(text) => (text, ""),
                            },
                            None => (text, ""),
                        }
                    }
                    None => (text, "earlier "),
                };
                (
                    format!("[telecom] {from} answered your {earlier}ask {ask_id}:\n{text}"),
                    false,
                    json!({ "kind": "reply", "from": from, "askId": ask_id }),
                )
            }
            Body::Broadcast { text } => {
                if state.held.len() == HELD_CAP {
                    state.held.remove(0);
                    state.dropped += 1;
                }
                state
                    .held
                    .push(format!("- {from}: {}", wire::one_line(&text, 2000)));
                return Outcome::Accepted { held: false };
            }
            Body::Withdraw { ask_id } => {
                let withdrawn = state
                    .owed
                    .iter()
                    .find(|ask| ask.ask_id == ask_id && ask.from_session == frame.from.session)
                    .cloned();
                if let Some(ask) = withdrawn {
                    let note = format!("{from} withdrew ask {ask_id}; nothing more to do");
                    state.close_owed(&ask, note);
                }
                return Outcome::Accepted { held: false };
            }
        };
        let held = budgeted && (state.waking || !allow_wake(&mut state.wakes));
        let message = message(text, true, details);
        let posted = if held {
            self.inner.jobs.post_held(message)
        } else {
            self.inner.jobs.post_at(message, 0)
        };
        if posted.is_err() {
            return refused("the session is closing");
        }
        if held {
            self.arm_waker(&mut state);
        }
        Outcome::Accepted { held }
    }

    #[cfg(test)]
    pub(crate) fn team(&self) -> &Team {
        &self.inner.team
    }
}

/// How an ask ended for the tool that made it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskOutcome {
    Answered {
        ask_id: String,
        from: String,
        text: String,
    },
    Sent {
        ask_id: String,
        to: String,
        /// The peer holds it past its wake budget.
        held: bool,
    },
    TimedOut {
        ask_id: String,
        to: String,
    },
    Gone {
        ask_id: String,
        to: String,
    },
    /// The wait ended because mail arrived for this session; the ask
    /// stays open and its answer arrives as a message.
    Interrupted {
        ask_id: String,
        to: String,
    },
    Cancelled {
        ask_id: String,
    },
}

impl AskOutcome {
    fn with_id(self, id: String) -> Self {
        match self {
            Self::Answered { from, text, .. } => Self::Answered {
                ask_id: id,
                from,
                text,
            },
            Self::Sent { to, held, .. } => Self::Sent {
                ask_id: id,
                to,
                held,
            },
            Self::TimedOut { to, .. } => Self::TimedOut { ask_id: id, to },
            Self::Gone { to, .. } => Self::Gone { ask_id: id, to },
            Self::Interrupted { to, .. } => Self::Interrupted { ask_id: id, to },
            Self::Cancelled { .. } => Self::Cancelled { ask_id: id },
        }
    }
}

/// The peer of `peers` that `to` names: a callsign, or a session id or
/// its prefix of at least 4 characters. `me` is this member's callsign.
fn pick(peers: &[Presence], me: &str, to: &str) -> Result<Presence, String> {
    let to = to.trim();
    if let Some(peer) = peers.iter().find(|peer| peer.callsign == to) {
        return Ok(peer.clone());
    }
    let by_id: Vec<&Presence> = peers
        .iter()
        .filter(|peer| to.len() >= 4 && peer.session.starts_with(to))
        .collect();
    match by_id.as_slice() {
        [peer] => Ok((*peer).clone()),
        _ if to == me => Err("that is you".to_owned()),
        _ => {
            let names: Vec<&str> = peers.iter().map(|peer| peer.callsign.as_str()).collect();
            Err(if names.is_empty() {
                format!("no agent \"{to}\": no other agent works in this repository")
            } else {
                format!("no agent \"{to}\"; the team is {}", names.join(", "))
            })
        }
    }
}

/// Runs `futures` together on the caller's task; their outputs in order.
async fn join_all<F: Future>(futures: Vec<F>) -> Vec<F::Output> {
    let mut pending: Vec<_> = futures.into_iter().map(Box::pin).collect();
    let mut outputs: Vec<Option<F::Output>> = pending.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut done = true;
        for (future, output) in pending.iter_mut().zip(outputs.iter_mut()) {
            if output.is_none() {
                match future.as_mut().poll(cx) {
                    Poll::Ready(value) => *output = Some(value),
                    Poll::Pending => done = false,
                }
            }
        }
        if done { Poll::Ready(()) } else { Poll::Pending }
    })
    .await;
    outputs.into_iter().flatten().collect()
}

fn refused(reason: &str) -> Outcome {
    Outcome::Refused {
        reason: reason.to_owned(),
    }
}

fn checked_text(text: &str) -> Result<String, String> {
    let text = wire::clean_text(text);
    if text.trim().is_empty() {
        return Err("message: write something to send".to_owned());
    }
    if text.len() > wire::TEXT_CAP {
        return Err(format!(
            "message: {} bytes, more than the {} a message may carry",
            text.len(),
            wire::TEXT_CAP
        ));
    }
    Ok(text)
}

/// Whether a peer's model may start a turn now: at most
/// [`MAX_WAKES_PER_MINUTE`] in any [`WAKE_WINDOW`]. Spends one when it may.
fn allow_wake(wakes: &mut VecDeque<tokio::time::Instant>) -> bool {
    let now = tokio::time::Instant::now();
    if reopens_at(wakes, now).is_some() {
        return false;
    }
    wakes.push_back(now);
    true
}

/// When the wake budget has room again: `None` when it has now. Forgets
/// wakes older than the window.
fn reopens_at(
    wakes: &mut VecDeque<tokio::time::Instant>,
    now: tokio::time::Instant,
) -> Option<tokio::time::Instant> {
    while wakes
        .front()
        .is_some_and(|at| now.duration_since(*at) >= WAKE_WINDOW)
    {
        wakes.pop_front();
    }
    (wakes.len() >= MAX_WAKES_PER_MINUTE).then(|| wakes[0] + WAKE_WINDOW)
}

/// Releases the held mail once the wake budget has room, spending one
/// wake for all of it. Holds the session's live work until then.
async fn waker(inner: Weak<Inner>, _hold: Option<LiveHold>, scope: crate::owned_work::TaskScope) {
    loop {
        let reopens = {
            let Some(inner) = inner.upgrade() else { return };
            let mut state = lock(&inner.state);
            let now = tokio::time::Instant::now();
            match reopens_at(&mut state.wakes, now) {
                Some(at) => at,
                None => {
                    // Under the state lock: a frame taken after this is
                    // weighed against the budget as it now is.
                    if inner.jobs.release_held() {
                        state.wakes.push_back(now);
                    }
                    state.waking = false;
                    return;
                }
            }
        };
        tokio::select! {
            biased;
            () = scope.signal().cancelled() => return,
            () = tokio::time::sleep_until(reopens) => {}
        }
    }
}

/// Every [`PEER_CHECK`], ends the open asks nobody waits on whose peer's
/// session left, posting that it left; stops when there are none.
async fn watcher(inner: Weak<Inner>, scope: crate::owned_work::TaskScope) {
    loop {
        tokio::select! {
            biased;
            () = scope.signal().cancelled() => return,
            () = tokio::time::sleep(PEER_CHECK) => {}
        }
        let Some(inner) = inner.upgrade() else { return };
        let station = Station { inner };
        let unwaited = |waiting: &Waiting| waiting.answer.is_none();
        {
            let mut state = lock(&station.inner.state);
            if !state.waiting.values().any(unwaited) {
                state.watching = false;
                return;
            }
        }
        let peers = station.live_peers().await;
        if lock(&station.inner.membership).is_none() {
            return;
        }
        let gone: Vec<(String, Waiting)> = {
            let mut state = lock(&station.inner.state);
            let ids: Vec<String> = state
                .waiting
                .iter()
                .filter(|(_, waiting)| {
                    unwaited(waiting) && !peers.iter().any(|peer| peer.session == waiting.session)
                })
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| state.waiting.remove(&id).map(|waiting| (id, waiting)))
                .collect()
        };
        for (ask_id, waiting) in gone {
            let text = format!(
                "[telecom] {} left before answering ask {ask_id}.",
                waiting.to
            );
            let details = json!({ "kind": "gone", "to": waiting.to, "askId": ask_id });
            let _ = station.inner.jobs.post_at(message(text, true, details), 0);
            // Its hold ends after the notice is queued, so print mode
            // sees one or the other.
            drop(waiting);
        }
    }
}

fn message(text: String, display: bool, details: serde_json::Value) -> CustomMessage {
    CustomMessage {
        custom_type: MESSAGE_TYPE.to_owned(),
        content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))]),
        display,
        details: Some(details),
        timestamp: bake_ai::now_ms(),
    }
}

/// `busy, bash 3m`, `idle 10s`.
fn state_words(peer: &Presence) -> String {
    let age = format_age(bake_ai::now_ms().saturating_sub(peer.since));
    match (peer.activity, &peer.tool) {
        (Activity::Busy, Some(tool)) => format!("busy, {tool} {age}"),
        (Activity::Busy, None) => format!("busy {age}"),
        (Activity::Idle, _) => format!("idle {age}"),
    }
}

fn format_age(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        _ => format!("{}h", seconds / 3600),
    }
}

fn about(peer: &Presence) -> String {
    match &peer.task {
        Some(task) => format!("task: {task}"),
        None => "no task yet".to_owned(),
    }
}

/// One board line: `- bravo [busy, bash 3m] on develop (spawned by alpha) -- task: ...`.
fn board_line(peer: &Presence) -> String {
    let mut line = format!("- {} [{}]", peer.callsign, state_words(peer));
    if let Some(branch) = &peer.branch {
        line.push_str(&format!(" on {branch}"));
    }
    if let Some(parent) = &peer.parent {
        line.push_str(&format!(" (spawned by {parent})"));
    }
    if let Some(model) = &peer.model {
        line.push_str(&format!(", {model}"));
    }
    line.push_str(&format!(" -- {}", about(peer)));
    line
}

/// Republishes the presence record whenever it changes, until the session
/// ends.
async fn publisher(inner: Weak<Inner>, scope: crate::owned_work::TaskScope) {
    let Some(inner) = inner.upgrade() else { return };
    loop {
        tokio::select! {
            () = scope.signal().cancelled() => break,
            () = inner.dirty.notified() => {}
        }
        let record = lock(&inner.state).presence.clone();
        let membership = lock(&inner.membership).clone();
        if let Some(membership) = membership
            && let Ok(write) = scope
                .permit()
                .spawn_blocking(move || membership.publish(&record))
        {
            // Awaited, so records are written in order.
            let _ = write.await;
        }
    }
    // The session is ending: give the callsign back now rather than when
    // the last reference to the station goes.
    Station { inner }.leave();
}

#[cfg(test)]
mod tests;
