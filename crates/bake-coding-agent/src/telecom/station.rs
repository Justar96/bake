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

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::sync::{Notify, oneshot};

use super::directory::{self, Membership, NATO, Team};
use super::endpoint;
use super::wire::{self, Activity, Body, Frame, Outcome, Presence, Sender, WIRE_VERSION};
use crate::agent_session::SessionEvent;
use crate::jobs::JobRegistry;
use crate::owned_work::WorkGate;
use crate::session::messages::CustomMessage;
use bake_ai::{TextContent, UserContent, UserContentBlock};

/// The `customType` of what telecom puts in a session.
pub const MESSAGE_TYPE: &str = "bake.telecom";

/// Turns peers' messages may start per minute; asks and a person's
/// messages always may.
pub const MAX_WAKES_PER_MINUTE: usize = 6;

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

/// An ask this station made and has no answer to.
struct Waiting {
    to: String,
    /// The tool call waiting for it; `None` once nobody waits, so a late
    /// answer arrives as a message.
    answer: Option<oneshot::Sender<String>>,
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
    /// Broadcasts held for the next brief.
    held: Vec<String>,
    /// The roster the last brief described; `None` before the first.
    briefed: Option<BTreeMap<String, Seen>>,
    wakes: VecDeque<Instant>,
}

struct Inner {
    membership: Mutex<Option<Membership>>,
    team: Team,
    state: Mutex<State>,
    jobs: JobRegistry,
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
                    membership: Mutex::new(Some(membership)),
                    team,
                    state: Mutex::new(State {
                        presence,
                        task_pinned: false,
                        waiting: HashMap::new(),
                        owed: Vec::new(),
                        held: Vec::new(),
                        briefed: None,
                        wakes: VecDeque::new(),
                    }),
                    jobs,
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

    /// The asks waiting on this member, oldest first.
    pub fn owed(&self) -> Vec<Owed> {
        lock(&self.inner.state).owed.clone()
    }

    /// Gives the callsign back; later deliveries fail. Idempotent.
    pub fn leave(&self) {
        let membership = lock(&self.inner.membership).take();
        drop(membership);
        let mut state = lock(&self.inner.state);
        for (_, waiting) in state.waiting.drain() {
            drop(waiting.answer);
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

    /// The brief to put before a prompt: on the first prompt with peers,
    /// every peer; later, only what changed, with held broadcasts and the
    /// asks this member owes. `None` when there is nothing to say. A
    /// prompt of three words or more becomes the task unless `status` set
    /// one.
    pub fn brief(&self, prompt: &str) -> Option<CustomMessage> {
        {
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
        let peers = self.peers();
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
        if !held.is_empty() {
            lines.push("[telecom] Broadcasts since your last prompt:".to_owned());
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
        let to = to.trim();
        let peers = self.peers();
        if let Some(peer) = peers.iter().find(|peer| peer.callsign == to) {
            return Ok(peer.clone());
        }
        let by_id: Vec<&Presence> = peers
            .iter()
            .filter(|peer| to.len() >= 4 && peer.session.starts_with(to))
            .collect();
        match by_id.as_slice() {
            [peer] => Ok((*peer).clone()),
            _ if to == self.callsign() => Err("that is you".to_owned()),
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

    async fn deliver(&self, peer: &Presence, body: Body) -> Result<(), String> {
        let frame = Frame::new(self.sender(), body);
        match endpoint::deliver(&self.inner.team.socket(&peer.callsign), &frame).await {
            Ok(Outcome::Accepted) => Ok(()),
            Ok(Outcome::Refused { reason }) => Err(reason),
            Err(error) => Err(error),
        }
    }

    /// Messages each of `to` (comma-separated callsigns, at most
    /// [`wire::GROUP_CAP`]). Returns who got it and who did not.
    pub async fn send(&self, to: &str, text: &str, human: bool) -> Result<String, String> {
        let text = checked_text(text)?;
        let names: Vec<&str> = to
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect();
        if names.is_empty() {
            return Err("to: name an agent".to_owned());
        }
        if names.len() > wire::GROUP_CAP {
            return Err(format!(
                "to: at most {} agents; use broadcast for the whole team",
                wire::GROUP_CAP
            ));
        }
        let peers = names
            .iter()
            .map(|name| self.resolve(name))
            .collect::<Result<Vec<_>, _>>()?;
        let mut reached = Vec::new();
        let mut failed = Vec::new();
        for peer in &peers {
            let group = peers
                .iter()
                .filter(|other| other.callsign != peer.callsign)
                .map(|other| other.callsign.clone())
                .collect();
            let body = Body::Message {
                text: text.clone(),
                group,
                human,
            };
            match self.deliver(peer, body).await {
                Ok(()) => reached.push(peer.callsign.clone()),
                Err(reason) => failed.push(format!("{}: {reason}", peer.callsign)),
            }
        }
        if reached.is_empty() {
            return Err(format!("not delivered ({})", failed.join("; ")));
        }
        let mut report = format!("delivered to {}", reached.join(", "));
        if !failed.is_empty() {
            report.push_str(&format!(" (failed: {})", failed.join("; ")));
        }
        Ok(report)
    }

    /// Messages every peer; it starts no turn. Returns who got it.
    pub async fn broadcast(&self, text: &str) -> Result<String, String> {
        let text = checked_text(text)?;
        let peers = self.peers();
        if peers.is_empty() {
            return Err("no other agent works in this repository".to_owned());
        }
        let mut reached = Vec::new();
        for peer in &peers {
            if self
                .deliver(peer, Body::Broadcast { text: text.clone() })
                .await
                .is_ok()
            {
                reached.push(peer.callsign.clone());
            }
        }
        Ok(format!(
            "broadcast to {}",
            if reached.is_empty() {
                "nobody".to_owned()
            } else {
                reached.join(", ")
            }
        ))
    }

    /// Asks `to` and, with `wait`, waits up to `timeout` for the answer.
    /// Without `wait`, or once the wait ends without one, the answer
    /// arrives later as a message.
    pub async fn ask(
        &self,
        to: &str,
        text: &str,
        timeout: Duration,
        wait: bool,
        signal: Option<bake_ai::AbortSignal>,
    ) -> Result<AskOutcome, String> {
        let text = checked_text(text)?;
        let peer = self.resolve(to)?;
        let ask_id = short_id();
        let (answer, answered) = oneshot::channel();
        lock(&self.inner.state).waiting.insert(
            ask_id.clone(),
            Waiting {
                to: peer.callsign.clone(),
                answer: wait.then_some(answer),
            },
        );
        let body = Body::Ask {
            ask_id: ask_id.clone(),
            text,
        };
        if let Err(reason) = self.deliver(&peer, body).await {
            lock(&self.inner.state).waiting.remove(&ask_id);
            return Err(format!("{} did not get the ask: {reason}", peer.callsign));
        }
        if !wait {
            return Ok(AskOutcome::Sent {
                ask_id,
                to: peer.callsign,
            });
        }
        let outcome = self
            .await_answer(
                &peer.callsign,
                answered,
                timeout.min(ASK_TIMEOUT_MAX),
                signal,
            )
            .await;
        if !matches!(outcome, AskOutcome::Answered { .. }) {
            // Nobody waits now: a late answer arrives as a message.
            if let Some(waiting) = lock(&self.inner.state).waiting.get_mut(&ask_id) {
                waiting.answer = None;
            }
            if matches!(
                outcome,
                AskOutcome::Gone { .. } | AskOutcome::Cancelled { .. }
            ) {
                lock(&self.inner.state).waiting.remove(&ask_id);
                let _ = self
                    .deliver(
                        &peer,
                        Body::Withdraw {
                            ask_id: ask_id.clone(),
                        },
                    )
                    .await;
            }
        }
        Ok(outcome.with_id(ask_id))
    }

    async fn await_answer(
        &self,
        to: &str,
        mut answered: oneshot::Receiver<String>,
        timeout: Duration,
        signal: Option<bake_ai::AbortSignal>,
    ) -> AskOutcome {
        let deadline = tokio::time::Instant::now() + timeout;
        let cancelled = async {
            match &signal {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(cancelled);
        loop {
            tokio::select! {
                answer = &mut answered => {
                    return match answer {
                        Ok(text) => AskOutcome::Answered { ask_id: String::new(), from: to.to_owned(), text },
                        Err(_) => AskOutcome::Gone { ask_id: String::new(), to: to.to_owned() },
                    };
                }
                () = &mut cancelled => return AskOutcome::Cancelled { ask_id: String::new() },
                () = tokio::time::sleep_until(deadline) => {
                    return AskOutcome::TimedOut { ask_id: String::new(), to: to.to_owned() };
                }
                () = tokio::time::sleep(PEER_CHECK) => {
                    if !self.peers().iter().any(|peer| peer.callsign == to) {
                        return AskOutcome::Gone { ask_id: String::new(), to: to.to_owned() };
                    }
                }
            }
        }
    }

    /// Answers an ask: `ask_id`, else the only one waiting, else the only
    /// one from `to`. With `to` and no ask of theirs open, the text goes
    /// as a message.
    pub async fn reply(
        &self,
        text: &str,
        ask_id: Option<&str>,
        to: Option<&str>,
    ) -> Result<String, String> {
        let text = checked_text(text)?;
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
        let peer = self
            .resolve(&ask.from)
            .map_err(|_| format!("{} has left; the answer cannot be delivered", ask.from))?;
        self.deliver(
            &peer,
            Body::Reply {
                ask_id: ask.ask_id.clone(),
                text,
            },
        )
        .await
        .map_err(|reason| format!("{} did not get the answer: {reason}", ask.from))?;
        lock(&self.inner.state)
            .owed
            .retain(|owed| owed.ask_id != ask.ask_id);
        Ok(format!("answered {} ({})", ask.from, ask.ask_id))
    }

    /// Takes an arriving frame.
    fn receive(&self, frame: Frame) -> Outcome {
        if lock(&self.inner.membership).is_none() {
            return refused("telecom is off in that session");
        }
        let from = frame.from.callsign.clone();
        let mut state = lock(&self.inner.state);
        let (text, wake, details) = match frame.body {
            Body::Message { text, group, human } => {
                let header = match (human, group.is_empty()) {
                    (true, _) => format!("[telecom] Message from the person at {from}'s terminal:"),
                    (false, true) => format!("[telecom] Message from {from}:"),
                    (false, false) => format!(
                        "[telecom] Message from {from} (also to {}):",
                        group.join(", ")
                    ),
                };
                let wake = human || allow_wake(&mut state.wakes);
                (
                    format!("{header}\n{text}"),
                    wake,
                    json!({ "kind": "message", "from": from, "human": human }),
                )
            }
            Body::Ask { ask_id, text } => {
                if state.owed.iter().any(|ask| ask.ask_id == ask_id) {
                    return Outcome::Accepted;
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
                let Some(waiting) = state.waiting.remove(&ask_id) else {
                    return refused("that ask is not open here");
                };
                if waiting.to != from {
                    state.waiting.insert(ask_id, waiting);
                    return refused("that ask was not put to you");
                }
                if let Some(answer) = waiting.answer {
                    match answer.send(text) {
                        Ok(()) => return Outcome::Accepted,
                        Err(text) => (
                            format!("[telecom] {from} answered your ask {ask_id}:\n{text}"),
                            true,
                            json!({ "kind": "reply", "from": from, "askId": ask_id }),
                        ),
                    }
                } else {
                    (
                        format!("[telecom] {from} answered your ask {ask_id}:\n{text}"),
                        true,
                        json!({ "kind": "reply", "from": from, "askId": ask_id }),
                    )
                }
            }
            Body::Broadcast { text } => {
                state
                    .held
                    .push(format!("- {from}: {}", wire::one_line(&text, 2000)));
                return Outcome::Accepted;
            }
            Body::Withdraw { ask_id } => {
                state
                    .owed
                    .retain(|ask| !(ask.ask_id == ask_id && ask.from == from));
                return Outcome::Accepted;
            }
        };
        drop(state);
        if wake {
            self.inner.jobs.reset_wakes();
        }
        match self.inner.jobs.post(message(text, true, details)) {
            Ok(()) => Outcome::Accepted,
            Err(_) => refused("the session is closing"),
        }
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
    },
    TimedOut {
        ask_id: String,
        to: String,
    },
    Gone {
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
            Self::Sent { to, .. } => Self::Sent { ask_id: id, to },
            Self::TimedOut { to, .. } => Self::TimedOut { ask_id: id, to },
            Self::Gone { to, .. } => Self::Gone { ask_id: id, to },
            Self::Cancelled { .. } => Self::Cancelled { ask_id: id },
        }
    }
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

/// Whether a peer's message may start a turn: at most
/// [`MAX_WAKES_PER_MINUTE`] in any minute.
fn allow_wake(wakes: &mut VecDeque<Instant>) -> bool {
    let now = Instant::now();
    while wakes
        .front()
        .is_some_and(|at| now.duration_since(*at) > Duration::from_secs(60))
    {
        wakes.pop_front();
    }
    if wakes.len() >= MAX_WAKES_PER_MINUTE {
        return false;
    }
    wakes.push_back(now);
    true
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
        if let Some(membership) = lock(&inner.membership).as_ref() {
            let _ = membership.publish(&record);
        }
    }
    // The session is ending: give the callsign back now rather than when
    // the last reference to the station goes.
    Station { inner }.leave();
}

#[cfg(test)]
mod tests;
