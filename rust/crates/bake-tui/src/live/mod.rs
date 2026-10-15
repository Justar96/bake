//! The live runtime port: drives a real [`AgentSession`] from the
//! composer and reports what it does as [`RuntimeUpdate`]s.
//!
//! Adapted from Pi v1.2.0 `packages/coding-agent/src/modes/interactive/
//! interactive-mode.ts`: `onSubmit` sends a prompt while idle and steers
//! while streaming; `onEscape` gives the queued prompts back to the editor
//! and aborts (`restoreQueuedMessagesToEditor({ abort: true })`); the
//! initial message and the later command-line messages run in order, each
//! after the previous one settles. Pi's built-in slash commands are not
//! ported, so each is refused with the draft kept, as is any other
//! `/word` that names no prompt template; `/skill:name` and prompt
//! templates reach the session, which expands them.
//!
//! One driver task owns every runtime decision. The loop's requests reach
//! it over a channel; it starts each prompt as its own task, so a cancel or
//! a steer is handled while the prompt runs. Session events are projected
//! inside the session's listener ([`projection::Projector`]) and sent to the
//! loop in dispatch order; the driver sends the turn's start and end around
//! them. Each composer prompt is answered once: [`RuntimeUpdate::Admitted`]
//! when the session takes it, as its user message or as a queued steer, or
//! [`RuntimeUpdate::Refused`] when it ends before that, so its draft goes
//! back. Steers a cancel takes off the session's queue come back as
//! [`RuntimeUpdate::Returned`], so no submission is lost.
//!
//! Approvals (D24) are answered through the [`ApprovalBroker`] the command
//! line built the session's tools with ([`LiveSession::approvals`]): the
//! port connects the broker to the view, so its queue arrives as
//! [`RuntimeUpdate::Approvals`] in order with the other updates, and
//! passes the view's answers to it directly, not through the driver, so an
//! answer never waits behind a request the driver is handling. The broker
//! settles an answer only for the displayed request it names. Stopping the
//! port, or a loop that no longer takes updates, aborts the run, which
//! withdraws a waiting request, and closes the broker, so any later request
//! is `unavailable`.
//!
//! The port owns the session from [`LivePort::start`], but runs nothing
//! until [`LivePort::begin`], so no prompt starts before the terminal is
//! set up. Stopping or dropping it closes the request channel, as does a
//! loop that no longer takes updates; the driver then shuts the session
//! down, awaiting its run, its writes, and its model requests, before
//! [`LivePort::stop`] returns. No update is sent after that.

pub mod projection;

use std::collections::VecDeque;
use std::io;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, PoisonError};

use bake_ai::{ImageContent, StopReason};
use bake_coding_agent::agent_session::AgentSession;
use bake_coding_agent::permissions::{self, ApprovalBroker, ApprovalPrompt};
use bake_tui_view::paste::Image;
use bake_tui_view::runtime::{Approval, ApprovalAnswer, RuntimeUpdate, SessionStatus};
use bake_tui_view::state::Outcome;
use bake_tui_view::status::ContextUsage;
use bake_tui_view::transcript::Row;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::port::Request;
use projection::Projector;

/// Pi v1.2.0's built-in slash commands (`core/slash-commands.ts`), none of
/// which `bake-rs` runs yet. A prompt that names one is refused instead of
/// reaching the model.
const PI_COMMANDS: &[&str] = &[
    "settings",
    "model",
    "tree",
    "thinking",
    "scoped-models",
    "export",
    "import",
    "share",
    "bug",
    "copy",
    "name",
    "session",
    "changelog",
    "hotkeys",
    "fork",
    "clone",
    "trust",
    "login",
    "logout",
    "new",
    "compact",
    "resume",
    "reload",
    "quit",
];

/// A session the command line set up for interactive mode.
pub struct LiveSession {
    /// The session.
    pub session: AgentSession,
    /// The Tokio runtime the session runs on. The terminal loop runs on
    /// its own thread; the driver task runs here.
    pub runtime: tokio::runtime::Handle,
    /// Prompts to send first, in order, as the command line gave them.
    pub prompts: Vec<(String, Vec<ImageContent>)>,
    /// Notes for the top of the transcript, such as the session file.
    pub notes: Vec<String>,
    /// The broker the session's tools ask for approval, which the view
    /// answers; `None` when the session was built without one, so its
    /// escalations fail closed.
    pub approvals: Option<ApprovalBroker>,
}

impl LiveSession {
    /// The rows the transcript opens with: the notes, then what the session
    /// already holds.
    pub fn initial_rows(&self) -> Vec<Row> {
        let mut rows: Vec<Row> = self.notes.iter().cloned().map(Row::Info).collect();
        rows.extend(projection::replay(&self.session.messages()));
        rows
    }
}

type Send = Arc<dyn Fn(RuntimeUpdate) -> bool + std::marker::Send + Sync>;

/// The live port. Requests go to the driver task; updates come out through
/// the `send` given to [`LivePort::start`].
pub struct LivePort {
    approvals: Option<ApprovalBroker>,
    requests: Option<mpsc::UnboundedSender<Request>>,
    begin: Option<oneshot::Sender<()>>,
    done: Option<std_mpsc::Receiver<Result<(), String>>>,
}

impl LivePort {
    /// Takes the session and subscribes to it. From here the port owns the
    /// session: stopping or dropping the port shuts it down. Nothing runs,
    /// not even the command line's prompts, until [`LivePort::begin`].
    pub fn start(
        live: LiveSession,
        send: impl Fn(RuntimeUpdate) -> bool + std::marker::Send + Sync + 'static,
    ) -> Self {
        let LiveSession {
            session,
            runtime,
            prompts,
            notes: _,
            approvals,
        } = live;
        let session = Arc::new(session);
        // A receiver that is gone stops the driver, as a closed request
        // channel does.
        let gone = Arc::new(Gone::default());
        let send: Send = {
            let gone = Arc::clone(&gone);
            Arc::new(move |update| {
                let sent = send(update);
                if !sent {
                    gone.set();
                }
                sent
            })
        };
        if let Some(broker) = &approvals {
            let send = Arc::clone(&send);
            broker.connect(Box::new(move |queue| {
                send(RuntimeUpdate::Approvals(
                    queue.into_iter().map(approval).collect(),
                ))
            }));
        }
        let projector = Arc::new(Mutex::new(Projector::default()));
        let subscription = {
            let projector = Arc::clone(&projector);
            let send = Arc::clone(&send);
            session.subscribe(move |event| {
                let updates = projector
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .apply(event);
                for update in updates {
                    send(update);
                }
            })
        };
        let (requests, inbox) = mpsc::unbounded_channel();
        let (begin, begun) = oneshot::channel();
        let (finished, done) = std_mpsc::channel();
        let driver = Driver {
            session: Arc::clone(&session),
            projector,
            send,
            gone,
            waiting: prompts
                .into_iter()
                .map(|(text, images)| Queued {
                    id: None,
                    text,
                    images,
                })
                .collect(),
            aborts: Vec::new(),
            turns: 0,
            approvals: approvals.clone(),
        };
        let closing = approvals.clone();
        runtime.spawn(async move {
            let result = if begun.await.is_err() {
                // Dropped before it began: the session only shuts down.
                shut_down(&session, closing.as_ref()).await;
                Ok(())
            } else {
                // The driver runs as its own task, so its panic reaches
                // here; the session still shuts down, and the panic is
                // reported once it has.
                match tokio::spawn(driver.run(inbox)).await {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        shut_down(&session, closing.as_ref()).await;
                        Err(format!("the agent session driver failed: {error}"))
                    }
                }
            };
            subscription.unsubscribe();
            let _ = finished.send(result);
        });
        Self {
            approvals,
            requests: Some(requests),
            begin: Some(begin),
            done: Some(done),
        }
    }

    /// Starts the driver: the command line's prompts run, and requests are
    /// taken. The terminal calls it once it is set up.
    pub fn begin(&mut self) {
        if let Some(begin) = self.begin.take() {
            let _ = begin.send(());
        }
    }

    /// Passes a request on; one sent after the driver ended is dropped. An
    /// approval answer goes straight to the broker.
    pub fn request(&self, request: Request) {
        if let Request::Answer { generation, answer } = request {
            if let Some(broker) = &self.approvals {
                broker.answer(generation, broker_answer(answer));
            }
            return;
        }
        if let Some(requests) = &self.requests {
            let _ = requests.send(request);
        }
    }

    /// Shuts the session down and waits until it has; reports a driver
    /// that failed.
    pub fn stop(mut self) -> io::Result<()> {
        self.join()
    }

    /// Whether the driver ended by itself within `patience`, with the port
    /// still open.
    #[cfg(test)]
    fn ended_within(&mut self, patience: std::time::Duration) -> bool {
        let Some(done) = self.done.take() else {
            return true;
        };
        match done.recv_timeout(patience) {
            Ok(result) => {
                let (finished, done) = std_mpsc::channel();
                let _ = finished.send(result);
                self.done = Some(done);
                true
            }
            Err(_) => {
                self.done = Some(done);
                false
            }
        }
    }

    fn join(&mut self) -> io::Result<()> {
        self.requests = None;
        self.begin = None;
        match self.done.take().map(|done| done.recv()) {
            Some(Ok(Err(message))) => Err(io::Error::other(message)),
            Some(Err(_)) => Err(io::Error::other("the agent session driver vanished")),
            _ => Ok(()),
        }
    }
}

impl Drop for LivePort {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

/// Set once the loop no longer takes updates.
#[derive(Default)]
struct Gone {
    flag: AtomicBool,
    notify: Notify,
}

impl Gone {
    fn set(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }

    fn is_set(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// A prompt waiting for the next turn, with the composer's id when the
/// composer sent it.
struct Queued {
    id: Option<u64>,
    text: String,
    images: Vec<ImageContent>,
}

/// The prompt that is running.
struct Running {
    task: JoinHandle<Result<(), String>>,
    id: Option<u64>,
    text: String,
    cancelled: bool,
}

/// What the driver does next.
enum Next {
    Settled(Result<Result<(), String>, tokio::task::JoinError>),
    Request(Option<Request>),
}

struct Driver {
    session: Arc<AgentSession>,
    projector: Arc<Mutex<Projector>>,
    send: Send,
    gone: Arc<Gone>,
    /// Prompts for the next turns, oldest first: the command line's, those
    /// submitted with images while a turn ran, and those submitted after a
    /// cancel, before its turn ended.
    waiting: VecDeque<Queued>,
    /// Aborts in flight. Each is awaited before the next prompt starts, so
    /// an abort never reaches a later run.
    aborts: Vec<JoinHandle<()>>,
    turns: u64,
    approvals: Option<ApprovalBroker>,
}

impl Driver {
    async fn run(mut self, mut inbox: mpsc::UnboundedReceiver<Request>) {
        let mut running: Option<Running> = None;
        self.status();
        if let Some(next) = self.waiting.pop_front() {
            running = Some(self.begin(next));
        }
        self.publish_pending();
        while !self.gone.is_set() {
            let next = {
                let settle = async {
                    match running.as_mut() {
                        Some(run) => (&mut run.task).await,
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    joined = settle => Next::Settled(joined),
                    request = inbox.recv() => Next::Request(request),
                    () = self.gone.notify.notified() => Next::Request(None),
                }
            };
            match next {
                Next::Settled(joined) => {
                    if let Some(run) = running.take() {
                        self.settled(run, joined).await;
                    }
                    if let Some(next) = self.waiting.pop_front() {
                        running = Some(self.begin(next));
                        self.publish_pending();
                    }
                }
                Next::Request(None) => break,
                Next::Request(Some(request)) => self.request(request, &mut running).await,
            }
        }
        // Pi's `dispose`, awaited: the run, its writes, and its requests end
        // before the terminal is given back.
        shut_down(&self.session, self.approvals.as_ref()).await;
        if let Some(run) = running {
            let _ = run.task.await;
        }
        for abort in self.aborts.drain(..) {
            let _ = abort.await;
        }
    }

    fn send(&self, update: RuntimeUpdate) {
        (self.send)(update);
    }

    fn refuse(&self, id: Option<u64>, text: String, reason: String) {
        self.send(match id {
            Some(id) => RuntimeUpdate::Refused { id, reason },
            // A command-line prompt has no draft; its text comes back.
            None => RuntimeUpdate::Returned(vec![text]),
        });
    }

    fn begin(&mut self, prompt: Queued) -> Running {
        self.projector
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .begin(prompt.id);
        self.turns += 1;
        self.send(RuntimeUpdate::TurnStarted {
            seed: format!("{}-{}", self.session.session_id(), self.turns),
        });
        let session = Arc::clone(&self.session);
        let Queued { id, text, images } = prompt;
        let sent = text.clone();
        let task = tokio::spawn(async move { session.prompt(&sent, images).await });
        Running {
            task,
            id,
            text,
            cancelled: false,
        }
    }

    async fn request(&mut self, request: Request, running: &mut Option<Running>) {
        match request {
            Request::Submit(prompt) => {
                let id = Some(prompt.id);
                let text = prompt.submission.text().to_owned();
                let templates: Vec<String> = self
                    .session
                    .prompt_templates()
                    .iter()
                    .map(|template| template.name.clone())
                    .collect();
                if let Some(reason) = refused_command(&text, &templates) {
                    return self.refuse(id, text, reason);
                }
                let images: Vec<ImageContent> = prompt.images.iter().map(image_content).collect();
                match running {
                    // Pi's `onSubmit` while streaming: a steer. One with
                    // images waits for the next turn instead, so they stay
                    // with their prompt, and so does one sent after a
                    // cancel, which must not join the run it stops.
                    Some(run) if images.is_empty() && !run.cancelled => {
                        match self.session.steer(&text, Vec::new()).await {
                            Ok(()) => self.send(RuntimeUpdate::Admitted(prompt.id)),
                            Err(reason) => self.refuse(id, text, reason),
                        }
                    }
                    Some(_) => {
                        self.waiting.push_back(Queued { id, text, images });
                        self.publish_pending();
                    }
                    None => *running = Some(self.begin(Queued { id, text, images })),
                }
            }
            // Pi's `onEscape` while streaming.
            Request::Cancel => {
                let Some(run) = running.as_mut().filter(|run| !run.cancelled) else {
                    return;
                };
                run.cancelled = true;
                let queued = self.session.clear_queue();
                let returned: Vec<String> = queued
                    .steering
                    .into_iter()
                    .chain(queued.follow_up)
                    .collect();
                if !returned.is_empty() {
                    self.send(RuntimeUpdate::Returned(returned));
                }
                for waiting in std::mem::take(&mut self.waiting) {
                    self.refuse(waiting.id, waiting.text, INTERRUPTED_PROMPT.into());
                }
                self.publish_pending();
                let session = Arc::clone(&self.session);
                self.aborts
                    .push(tokio::spawn(async move { session.abort().await }));
            }
            // Waiting prompts join the running turn at its next step; one
            // with images keeps waiting for its own turn.
            // The port answers approvals itself.
            Request::Answer { .. } => {}
            Request::SendPending => {
                if running.as_ref().is_none_or(|run| run.cancelled) {
                    return;
                }
                let mut kept = VecDeque::new();
                while let Some(queued) = self.waiting.pop_front() {
                    if !queued.images.is_empty() {
                        kept.push_back(queued);
                        continue;
                    }
                    match self.session.steer(&queued.text, Vec::new()).await {
                        Ok(()) => {
                            if let Some(id) = queued.id {
                                self.send(RuntimeUpdate::Admitted(id));
                            }
                        }
                        Err(reason) => self.refuse(queued.id, queued.text, reason),
                    }
                }
                self.waiting = kept;
                self.publish_pending();
            }
        }
    }

    async fn settled(
        &mut self,
        run: Running,
        joined: Result<Result<(), String>, tokio::task::JoinError>,
    ) {
        // The run's abort ends before anything else starts.
        for abort in self.aborts.drain(..) {
            let _ = abort.await;
        }
        let (admitted, last_stop, mut updates) = {
            let mut projector = self
                .projector
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            (
                projector.admitted(),
                projector.last_stop(),
                projector.finish(),
            )
        };
        let failure = match joined {
            Ok(Ok(())) => None,
            Ok(Err(message)) => Some(message),
            Err(error) => Some(format!("The agent task failed: {error}")),
        };
        let outcome = if run.cancelled || last_stop == Some(StopReason::Aborted) {
            Outcome::Interrupted
        } else if failure.is_some() || last_stop == Some(StopReason::Error) {
            Outcome::Failed
        } else {
            Outcome::Completed
        };
        for update in updates.drain(..) {
            self.send(update);
        }
        if admitted {
            if let Some(message) = failure {
                self.send(RuntimeUpdate::Commit(vec![Row::Error(message)]));
            }
        } else {
            // The agent never took the prompt: refused, such as without an
            // API key, or cancelled first. Its draft goes back.
            let reason = failure.unwrap_or_else(|| INTERRUPTED_PROMPT.into());
            self.refuse(run.id, run.text, reason);
        }
        self.send(RuntimeUpdate::TurnEnded(outcome));
        self.status();
        if run.cancelled {
            return;
        }
        // A steer that arrived as the run was ending stays queued with no
        // run to take it; it runs next, ahead of the other waiting prompts.
        if !self.session.queued_messages().is_empty() {
            let queued = self.session.clear_queue();
            for text in queued.steering.into_iter().chain(queued.follow_up).rev() {
                self.waiting.push_front(Queued {
                    id: None,
                    text,
                    images: Vec::new(),
                });
            }
            self.publish_pending();
        }
    }

    /// Sends the pending panel: the session's queues and the driver's own.
    fn publish_pending(&self) {
        let queued = self.session.queued_messages();
        let waiting: Vec<String> = self.waiting.iter().map(|q| q.text.clone()).collect();
        let update = self
            .projector
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_waiting(waiting, &queued.steering, &queued.follow_up);
        self.send(update);
    }

    /// Sends the status line's model, thinking level, and context use.
    fn status(&self) {
        let model = self.session.model();
        let context = model.as_ref().and_then(|model| {
            let used = self.session.messages().iter().rev().find_map(|message| {
                let assistant = message.as_assistant()?;
                if matches!(
                    assistant.stop_reason,
                    StopReason::Aborted | StopReason::Error
                ) {
                    return None;
                }
                let tokens = bake_ai::utils::estimate::calculate_context_tokens(&assistant.usage);
                (tokens > 0).then_some(tokens)
            })?;
            (model.context_window > 0).then_some(ContextUsage {
                used,
                window: model.context_window,
            })
        });
        self.send(RuntimeUpdate::Status(SessionStatus {
            thinking: model
                .as_ref()
                .filter(|model| model.reasoning)
                .map(|_| self.session.thinking_level().as_str().to_owned()),
            model: model.map(|model| format!("{}/{}", model.provider, model.id)),
            context,
        }));
    }
}

/// Why a prompt a cancel stopped before the agent took it came back.
const INTERRUPTED_PROMPT: &str = "Stopped before it was sent; your prompt is back in the draft.";

/// Why a slash prompt is refused: it names one of Pi's built-in commands,
/// which `bake-rs` does not run yet, or another command-like word that is
/// neither `/skill:name` nor one of `templates`. Text whose first word is
/// not command-like, such as a path, goes to the model as Pi sends it.
fn refused_command(text: &str, templates: &[String]) -> Option<String> {
    let word = text
        .trim_start()
        .strip_prefix('/')?
        .split_whitespace()
        .next()?;
    if word.starts_with("skill:") || templates.iter().any(|name| name == word) {
        return None;
    }
    let command_like = word
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if PI_COMMANDS.contains(&word) {
        Some(format!(
            "/{word} is not available in bake-rs yet; your draft is kept."
        ))
    } else if command_like {
        Some(format!(
            "/{word} is not a command or prompt template; your draft is kept."
        ))
    } else {
        None
    }
}

/// Shuts `session` down and closes `approvals`. The shutdown's first step,
/// taken before it first waits, aborts the run, so a request it holds is
/// withdrawn with the run and its call never goes on to another model
/// request; the broker closes right after, so any request nothing aborted
/// is cancelled, and nobody is asked again.
async fn shut_down(session: &AgentSession, approvals: Option<&ApprovalBroker>) {
    tokio::join!(session.shutdown(), async {
        if let Some(broker) = approvals {
            broker.close();
        }
    });
}

/// A queued approval request as the view shows it.
fn approval(prompt: ApprovalPrompt) -> Approval {
    Approval {
        generation: prompt.generation,
        tool: prompt.tool_name,
        call_id: prompt.call_id,
        reason: prompt.reason,
        arguments: arguments_text(&prompt.arguments),
    }
}

/// The escalation arguments, which the reason already states.
const ESCALATION_ARGUMENTS: [&str; 2] = ["sandbox_permissions", "justification"];

/// A call's arguments for review, one argument per line as `name: value`,
/// apart from the escalation pair. A string shows as written, a string of
/// several lines on the lines under its name, each indented; any other
/// value as compact JSON. Control characters other than tab show as
/// `\u{…}` escapes, so nothing a call would run is hidden and nothing
/// reaches the terminal as an escape sequence.
fn arguments_text(arguments: &serde_json::Value) -> String {
    let Some(object) = arguments.as_object() else {
        return visible(&arguments.to_string());
    };
    let mut lines = Vec::new();
    for (name, value) in object {
        if ESCALATION_ARGUMENTS.contains(&name.as_str()) {
            continue;
        }
        let name = visible(name);
        match value.as_str() {
            Some(text) if text.contains('\n') => {
                lines.push(format!("{name}:"));
                lines.extend(text.split('\n').map(|line| format!("  {}", visible(line))));
            }
            Some(text) => lines.push(format!("{name}: {}", visible(text))),
            None => lines.push(format!("{name}: {}", visible(&value.to_string()))),
        }
    }
    lines.join("\n")
}

/// `text` with every control character but tab, and every invisible
/// format character that reorders or hides text (bidirectional controls,
/// zero-width characters, the byte-order mark), written as a `\u{…}`
/// escape.
fn visible(text: &str) -> String {
    let hidden = |c: char| matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}');
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if (c.is_control() && c != '\t') || hidden(c) {
            out.push_str(&format!("\\u{{{:x}}}", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    out
}

/// The view's answer as the broker takes it.
fn broker_answer(answer: ApprovalAnswer) -> permissions::ApprovalAnswer {
    match answer {
        ApprovalAnswer::AllowOnce => permissions::ApprovalAnswer::AllowOnce,
        ApprovalAnswer::Reject => permissions::ApprovalAnswer::Reject,
        ApprovalAnswer::Cancel => permissions::ApprovalAnswer::Cancel,
    }
}

/// A staged image as the session takes it.
fn image_content(image: &Image) -> ImageContent {
    use base64::Engine as _;
    ImageContent {
        data: base64::engine::general_purpose::STANDARD.encode(image.data.bytes()),
        mime_type: image.media_type.to_owned(),
    }
}

#[cfg(test)]
mod tests;
