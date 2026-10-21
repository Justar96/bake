//! The live runtime port: drives a real [`AgentSession`] from the
//! composer and reports what it does as [`RuntimeUpdate`]s.
//!
//! Adapted from Pi v1.2.0 `packages/coding-agent/src/modes/interactive/
//! interactive-mode.ts`: `onSubmit` sends a prompt while idle and steers
//! while streaming; `onEscape` gives the queued prompts back to the editor
//! and aborts (`restoreQueuedMessagesToEditor({ abort: true })`); the
//! initial message and the later command-line messages run in order, each
//! after the previous one settles. Pi's built-in slash commands that
//! [`commands`] does not run are refused with the draft kept, as is any other
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
//! Background shells and subagents are read from the session's job
//! registry and subagent host four times a second, since neither
//! announces every change, and sent only when they changed. The status
//! line's model, thinking level, and context use are sent at startup and
//! after every prompt and command; while a prompt runs they are read again
//! whenever the session reports something that can change them
//! ([`projection::moves_status`]) and sent if they changed, so the context
//! reading follows a long run step by step instead of waiting for its end.
//!
//! MCP registry changes are observed even while the session is idle.
//! Startup, refresh, and connection failures publish diagnostic rows with
//! the server's name; server error bodies and stderr stay out of the view.
//! Replacing the session replaces this subscription along with its tools.
//!
//! The port owns the session from [`LivePort::start`], but runs nothing
//! until [`LivePort::begin`], so no prompt starts before the terminal is
//! set up. Stopping or dropping it closes the request channel, as does a
//! loop that no longer takes updates; the driver then shuts the session
//! down, awaiting its run, its writes, and its model requests, before
//! [`LivePort::stop`] returns. No update is sent after that.

mod agents;
mod approvals;
mod command_runs;
pub mod commands;
mod driver;
mod jobs;
mod mcp;
pub mod projection;
mod publish;
mod requests;
pub mod tree;

use std::collections::VecDeque;
use std::io;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, PoisonError};

use bake_agent::{RunControl, RunOutcome};
use bake_ai::{AbortController, ImageContent};
use bake_coding_agent::agent_session::AgentSession;
use bake_coding_agent::permissions::ApprovalBroker;
use bake_tui_view::copy;
use bake_tui_view::dialog::DialogAnswer;
use bake_tui_view::paste::Image;
use bake_tui_view::runtime::{RuntimeUpdate, SessionStatus};
use bake_tui_view::state::Outcome;
use bake_tui_view::status::ContextUsage;
use bake_tui_view::transcript::Row;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::port::Request;
use approvals::{approval, broker_answer};
use bake_coding_agent::cli::SessionFactory;
use driver::{INTERRUPTED_PROMPT, refused_command, work_outcome};
use mcp::McpNotices;
use projection::Projector;

/// Pi v1.2.0's built-in slash commands (`core/slash-commands.ts`) that
/// `bake` does not run yet; [`commands::COMMANDS`] are the ones it
/// runs. A prompt that names one is refused instead of reaching the model.
const PI_COMMANDS: &[&str] = &[
    "scoped-models",
    "export",
    "import",
    "copy",
    "hotkeys",
    "trust",
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
    /// The Bake home, whose CLIProxyAPI account `/login`, `/logout`, and
    /// `/refresh-models` change; `None` when it cannot be resolved.
    pub home: Option<std::path::PathBuf>,
    /// Builds the sessions `/new` and `/resume` replace this one with;
    /// `None` refuses both.
    pub factory: Option<SessionFactory>,
    /// `--resume`: open `/resume` before the first prompt, which waits for
    /// it ([`bake_coding_agent::cli::Started::resume`]).
    pub resume: bool,
    /// `--offline` ([`bake_coding_agent::cli::offline_mode`]): the
    /// once-per-launch CLIProxyAPI catalog refresh does not run, so the
    /// session starts without reaching the network.
    pub offline: bool,
    /// Holds `/login` at its commit, for tests of that boundary.
    #[cfg(test)]
    pub commit_gate: Option<commands::CommitGate>,
    /// The GitHub CLI `/share` and `/bug` run instead of `gh`, for tests.
    #[cfg(test)]
    pub gh: Option<std::ffi::OsString>,
}

impl LiveSession {
    /// The rows the transcript opens with: the notes, then what the session
    /// already holds.
    pub fn initial_rows(&self) -> Vec<Row> {
        opening_rows(&self.notes, &self.session)
    }
}

/// A session's opening rows: `notes`, then what its log holds, replayed.
fn opening_rows(notes: &[String], session: &AgentSession) -> Vec<Row> {
    let mut rows: Vec<Row> = notes.iter().cloned().map(Row::Info).collect();
    rows.extend(projection::replay(&session.messages()));
    rows
}

/// The note a replacement session opens with: what the command did, where
/// the session is saved, why its model fell back, its setup's warnings,
/// and that no model is ready when none is.
fn replacement_notes(note: String, session: &AgentSession, warnings: Vec<String>) -> Vec<String> {
    let mut notes = vec![note];
    notes.push(match session.session_file() {
        Some(file) => format!("Session saved to {}", file.display()),
        None => "Session not saved (--no-session)".to_owned(),
    });
    notes.extend(session.model_fallback_message().map(str::to_owned));
    notes.extend(
        warnings
            .into_iter()
            .map(|warning| format!("Warning: {warning}")),
    );
    if session.check_model_auth().is_err() {
        notes.push(
            "No model is ready yet. Run /login to sign in to CLIProxyAPI, or /model to choose a model with a key."
                .to_owned(),
        );
    }
    notes
}

type Send = Arc<dyn Fn(RuntimeUpdate) -> bool + std::marker::Send + Sync>;

/// The live port. Requests go to the driver task; updates come out through
/// the `send` given to [`LivePort::start`].
pub struct LivePort {
    #[cfg(test)]
    test_session: std::sync::Weak<AgentSession>,
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
            home,
            factory,
            resume,
            offline,
            #[cfg(test)]
            commit_gate,
            #[cfg(test)]
            gh,
        } = live;
        let session = Arc::new(session);
        #[cfg(test)]
        let test_session = Arc::downgrade(&session);
        // The session in use, which `/new` and `/resume` replace; what a
        // panicking driver's handler shuts down.
        let current = Arc::new(Mutex::new(Arc::clone(&session)));
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
        let projector = Arc::new(Mutex::new(Projector::with_history(&session.messages())));
        let status_due = Arc::new(Notify::new());
        let subscription = subscribe(&session, &projector, &send, &status_due);
        let (requests, inbox) = mpsc::unbounded_channel();
        let (begin, begun) = oneshot::channel();
        let (finished, done) = std_mpsc::channel();
        let driver = Driver {
            session: Arc::clone(&session),
            mcp: session.mcp_servers().map(McpNotices::new),
            projector,
            send,
            gone,
            waiting: VecDeque::new(),
            held: prompts
                .into_iter()
                .map(|(text, images)| Queued {
                    id: None,
                    text,
                    images,
                })
                .collect(),
            resume,
            offline,
            aborts: Vec::new(),
            turns: 0,
            approvals: approvals.clone(),
            home,
            factory,
            current: Arc::clone(&current),
            subscription: Some(subscription),
            #[cfg(test)]
            commit_gate,
            #[cfg(test)]
            gh,
            command: None,
            dialog_ids: Arc::default(),
            startup: None,
            orphans: Arc::default(),
            shells: Vec::new(),
            agents: Vec::new(),
            status_due,
            status: None,
        };
        let closing = approvals.clone();
        let orphans = Arc::clone(&driver.orphans);
        runtime.spawn(async move {
            let result = if begun.await.is_err() {
                // Dropped before it began: the session only shuts down.
                drop(driver);
                shut_down(&session, closing.as_ref()).await;
                Ok(())
            } else {
                drop(session);
                // The driver runs as its own task, so its panic reaches
                // here; the session in use then still shuts down, the
                // tasks the driver owned are awaited as its own shutdown
                // awaits them (shutting down a replacement one built), and
                // the panic is reported once they have ended.
                match tokio::spawn(driver.run(inbox)).await {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        let session =
                            Arc::clone(&current.lock().unwrap_or_else(PoisonError::into_inner));
                        shut_down(&session, closing.as_ref()).await;
                        let owned = std::mem::take(
                            &mut *orphans.lock().unwrap_or_else(PoisonError::into_inner),
                        );
                        for task in owned {
                            task.await;
                        }
                        Err(format!("the agent session driver failed: {error}"))
                    }
                }
            };
            let _ = finished.send(result);
        });
        Self {
            #[cfg(test)]
            test_session,
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

/// Subscribes the loop to `session`'s events, projected by `projector`,
/// and wakes `status_due` when one can change the status line; the driver
/// reads it then, outside the session's dispatch.
fn subscribe(
    session: &AgentSession,
    projector: &Arc<Mutex<Projector>>,
    send: &Send,
    status_due: &Arc<Notify>,
) -> bake_coding_agent::agent_session::SessionSubscription {
    let projector = Arc::clone(projector);
    let send = Arc::clone(send);
    let status_due = Arc::clone(status_due);
    session.subscribe(move |event| {
        let updates = projector
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .apply(event);
        for update in updates {
            send(update);
        }
        if projection::moves_status(event) {
            status_due.notify_one();
        }
    })
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
    task: JoinHandle<Result<RunOutcome, String>>,
    id: Option<u64>,
    text: String,
    cancelled: bool,
    /// A turn background job notices opened, with no prompt of its own.
    notice: bool,
}

/// A command that is running ([`commands`]), and the prompt that named it,
/// whose draft comes back unless it succeeds.
struct CommandRun {
    task: JoinHandle<commands::Outcome>,
    /// The dialog this command shows ([`commands::Context::shown`]).
    shown: Arc<std::sync::atomic::AtomicU64>,
    /// The cancel of the work it runs as the session's
    /// ([`commands::Context::work`]).
    work: Arc<std::sync::Mutex<Option<AbortController>>>,
    answers: mpsc::UnboundedSender<(u64, DialogAnswer)>,
    abort: AbortController,
    id: Option<u64>,
    text: String,
    /// The command's name.
    name: &'static str,
    /// Whether it replaces the session ([`commands::TRANSITIONS`]).
    transition: bool,
}

/// What the driver does next.
enum Next {
    Settled(Result<Result<RunOutcome, String>, tokio::task::JoinError>),
    /// A background job may have settled while the session was idle.
    Jobs,
    Commanded(Result<commands::Outcome, tokio::task::JoinError>),
    McpChanged,
    Request(Option<Request>),
    /// Time to look at the background shells and subagents again.
    Background,
    /// The session reported something the status line shows.
    Status,
}

struct Driver {
    session: Arc<AgentSession>,
    mcp: Option<McpNotices>,
    projector: Arc<Mutex<Projector>>,
    send: Send,
    gone: Arc<Gone>,
    /// Prompts for the next turns, oldest first: the command line's, those
    /// submitted with images while a turn ran, and those submitted after a
    /// cancel, before its turn ended.
    waiting: VecDeque<Queued>,
    /// The command line's prompts while the startup `/resume` runs, which
    /// must not count as waiting: a session is replaced only while nothing
    /// waits ([`Driver::busy`]). They become `waiting` once it ends.
    held: VecDeque<Queued>,
    /// `--resume`: open `/resume` before the first prompt.
    resume: bool,
    /// `--offline`: do not refresh the CLIProxyAPI catalog at startup.
    offline: bool,
    /// Aborts in flight. Each is awaited before the next prompt starts, so
    /// an abort never reaches a later run.
    aborts: Vec<JoinHandle<()>>,
    turns: u64,
    approvals: Option<ApprovalBroker>,
    home: Option<std::path::PathBuf>,
    factory: Option<SessionFactory>,
    /// The session in use, shared with the port's handler.
    current: Arc<Mutex<Arc<AgentSession>>>,
    /// The loop's subscription to `session`.
    subscription: Option<bake_coding_agent::agent_session::SessionSubscription>,
    /// The command that is running; one at a time.
    command: Option<CommandRun>,
    /// The id of the last dialog opened, shared by every command.
    dialog_ids: Arc<std::sync::atomic::AtomicU64>,
    /// The once-per-launch CLIProxyAPI refresh, and its abort.
    startup: Option<(JoinHandle<()>, AbortController)>,
    /// Where a panicking driver leaves `command` and `startup`.
    orphans: Orphans,
    #[cfg(test)]
    commit_gate: Option<commands::CommitGate>,
    #[cfg(test)]
    gh: Option<std::ffi::OsString>,
    /// The background shells last sent to the view.
    shells: Vec<bake_tui_view::jobs::BackgroundJob>,
    /// The subagents last sent to the view.
    agents: Vec<bake_tui_view::agents::Agent>,
    /// Woken by the session's listener when the status line may have
    /// changed ([`projection::moves_status`]).
    status_due: Arc<Notify>,
    /// The status line last sent to the view.
    status: Option<SessionStatus>,
}

/// How often the driver looks for changes in the background shells and
/// subagents: their states and output, which the job registry and the
/// subagent host do not announce.
const BACKGROUND_EVERY: std::time::Duration = std::time::Duration::from_millis(250);

/// The tasks a driver owned when it was dropped without its shutdown.
type Orphans = Arc<Mutex<Vec<std::pin::Pin<Box<dyn Future<Output = ()> + std::marker::Send>>>>>;

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
