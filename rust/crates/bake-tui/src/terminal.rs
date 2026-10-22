//! The terminal owner and the preview's event loop.
//!
//! [`TerminalSession`] is the only code that changes terminal modes. Each mode
//! is recorded before it is requested, so a partial setup failure, an error,
//! a panic, or a handled SIGINT/SIGTERM/SIGHUP restores exactly what was
//! changed.
//!
//! A terminal that goes away ends the preview as a [`PreviewExit::Hangup`]:
//! SIGHUP, a hangup the watcher thread sees on standard input or output, or a
//! write that fails with EIO or EPIPE. Restoring a terminal that is gone
//! fails, so those failures are dropped, as the TypeScript launcher's
//! `tolerateLostTerminal` drops them.
//!
//! The loop owns the view's state and the terminal. An input thread, the
//! runtime port's thread, and, on Unix, a signal thread and a hangup watcher
//! only send it messages over one channel; the loop waits for the next message or the
//! view's next timed change, applies every waiting message, and draws once.
//!
//! An approval request is armed across both threads. Once a frame has
//! drawn the displayed request whole, the loop publishes the token of that
//! showing ([`State::approval_drawn`]);
//! the input thread then reads every event already waiting, in Crossterm's
//! buffer or the terminal's, sends those ahead, and only then sends
//! [`Msg::ApprovalArmed`]. The channel keeps that order, so a key typed
//! before the prompt appeared, whether still queued for the loop or not
//! yet read, reaches the view before the arming and cannot answer it; no
//! delay stands in for that order.

use std::io::{self, Stdout};
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Once};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bake_tui_view::activity::Tones;
use bake_tui_view::frame;
use bake_tui_view::paste::Image;
use bake_tui_view::render::render;
use bake_tui_view::runtime::RuntimeUpdate;
use bake_tui_view::state::{Effect, ImageSource, Msg, State, update};
use bake_tui_view::status;
use bake_tui_view::wheel::{self, WheelSteps};
use crossterm::cursor::{RestorePosition, Show};
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;
use crossterm::style::Print;
use crossterm::terminal::{
    BeginSynchronizedUpdate, Clear, ClearType, DisableLineWrap, EnableLineWrap,
    EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
    enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;

use crate::clipboard::{self, Clipboard, Platform};
use crate::files::{Ask, Finder};
use crate::git;
use crate::inline::{self, Inline};
use crate::input;
use crate::live::{LivePort, LiveSession};
use crate::port::{FixturePort, Request};

/// How often the loop looks for a finished copy while one is pending.
const COPY_POLL: Duration = Duration::from_millis(20);

/// Clipboard workers, joined when the loop ends so none outlives it. A tool
/// that hangs is killed after its own timeout, so the join is bounded.
#[derive(Default)]
struct Workers(Vec<JoinHandle<()>>);

impl Drop for Workers {
    fn drop(&mut self) {
        for worker in self.0.drain(..) {
            let _ = worker.join();
        }
    }
}

/// What a worker finished: a copy, with whether a tool took the text, or
/// an image read for the draft.
enum Done {
    Copy(String, bool),
    Image(ImageSource, Option<Image>),
}

/// Clipboard work runs here, off the loop: copies go to the terminal's own
/// clipboard at once over SSH or where no tool applies, and otherwise to
/// the tools on a worker thread; images are read on one too. Each worker
/// reports on `done`.
struct Jobs {
    clipboard: Clipboard,
    home: Option<String>,
    done: Sender<Done>,
    results: Receiver<Done>,
    pending: usize,
    workers: Workers,
}

impl Jobs {
    fn new(clipboard: Clipboard, home: Option<String>) -> Self {
        let (done, results) = mpsc::channel();
        Self {
            clipboard,
            home,
            done,
            results,
            pending: 0,
            workers: Workers::default(),
        }
    }

    fn spawn(&mut self, work: impl FnOnce() -> Done + Send + 'static) {
        let done = self.done.clone();
        self.workers.0.retain(|worker| !worker.is_finished());
        self.workers.0.push(thread::spawn(move || {
            let _ = done.send(work());
        }));
        self.pending += 1;
    }

    fn copy(
        &mut self,
        screen: &mut impl Screen,
        state: &mut State,
        text: String,
    ) -> io::Result<()> {
        let tools = self.clipboard.tools();
        if self.clipboard.terminal_first() || tools.is_empty() {
            screen.write(&clipboard::osc52(&text))?;
            update(state, Msg::Copied(true));
            return Ok(());
        }
        self.spawn(move || {
            let ok = tools.into_iter().any(|tool| clipboard::feed(tool, &text));
            Done::Copy(text, ok)
        });
        Ok(())
    }

    fn read_image(&mut self, source: ImageSource) {
        let clipboard = self.clipboard;
        let home = self.home.clone();
        self.spawn(move || {
            let image = match &source {
                ImageSource::Clipboard => clipboard.read_image(),
                ImageSource::File { path, .. } => clipboard::read_image_file(path, home.as_deref()),
            };
            Done::Image(source, image)
        });
    }

    /// Applies every finished job. When no tool took a copy, the
    /// terminal's own clipboard is the last resort; a terminal that ignores
    /// OSC 52 gives no sign, so it counts as copied.
    fn settle(&mut self, screen: &mut impl Screen, state: &mut State) -> io::Result<()> {
        while let Ok(done) = self.results.try_recv() {
            self.pending -= 1;
            match done {
                Done::Copy(text, ok) => {
                    if !ok {
                        screen.write(&clipboard::osc52(&text))?;
                    }
                    update(state, Msg::Copied(true));
                }
                Done::Image(source, image) => {
                    update(state, Msg::ImageRead { source, image });
                }
            }
        }
        Ok(())
    }
}

const OWNED: u8 = 1;
const RAW: u8 = 1 << 1;
const ALTERNATE: u8 = 1 << 2;
const PASTE: u8 = 1 << 3;
const NO_WRAP: u8 = 1 << 4;
/// Set while a frame is between synchronized-update markers.
const SYNC: u8 = 1 << 5;
const MOUSE: u8 = 1 << 6;

/// Report presses, releases, and the wheel (1000), and motion while a button
/// is held (1002), in SGR encoding (1006), which has no coordinate limit; as
/// the TypeScript TUI requests. Crossterm's own `EnableMouseCapture` also
/// asks for every motion (1003), which floods the loop with reports. Native
/// selection stays on Shift-drag, or Option-drag on macOS.
const MOUSE_ON: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";
const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l";

/// Modes the live session changed. Process-wide so the panic hook can restore
/// them before the panic message is printed.
static MODES: AtomicU8 = AtomicU8::new(0);
/// A positive count means the terminal's save slot names a safe held-region
/// boundary. Publishing transcript rows clears the claim until blank rows
/// have a saved boundary again, so restoration cannot erase settled output.
pub(crate) static HELD_ROWS: AtomicU16 = AtomicU16::new(0);
const INLINE: u8 = 1 << 7;

/// Which terminal screen the sample preview owns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScreenMode {
    #[default]
    Fullscreen,
    /// Uses terminal scrollback for settled transcript rows.
    Inline,
}

/// How long the input thread waits for input before it checks whether to
/// stop. It bounds how long shutdown waits to join the thread; input itself
/// is read as soon as it arrives.
const READER_WAKE: Duration = Duration::from_millis(50);
/// How long shutdown waits for the input thread after a hangup. Crossterm's
/// reader retries a failed or empty read without returning, so on a terminal
/// that is gone the thread may never stop; past this it ends with the
/// process.
const HANGUP_JOIN: Duration = Duration::from_millis(100);
/// Messages applied before a frame is drawn, so a long burst still draws.
const MAX_BATCH: usize = 256;
/// The shortest gap between frames that only runtime updates prompted. A
/// batch with input is drawn at once.
const RUNTIME_FRAME: Duration = Duration::from_millis(16);

/// How the preview ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewExit {
    Quit,
    /// A handled termination signal, by number. SIGHUP is a
    /// [`PreviewExit::Hangup`] instead.
    Signal(i32),
    /// The terminal went away: SIGHUP, a hangup on standard input or output,
    /// or a write that failed with EIO or EPIPE. Nothing can be shown on it,
    /// so the caller should exit quietly with the SIGHUP status, 129.
    Hangup,
}

/// What the loop receives from the threads that feed it.
enum Source {
    Msg(Msg),
    // Only Unix watches for a hangup; elsewhere, only tests send one.
    #[cfg_attr(not(unix), allow(dead_code))]
    Hangup,
    // Only Unix forwards signals; elsewhere, only tests send one.
    #[cfg_attr(not(unix), allow(dead_code))]
    Signal(i32),
    Failed(io::Error),
}

/// Where the loop draws. [`TerminalSession`] is the real screen; tests use a
/// recording one.
trait Screen {
    /// Draws one frame. When `stale`, every cell is repainted.
    fn draw(&mut self, state: &mut State, stale: bool) -> io::Result<()>;
    /// Writes an escape sequence that is not part of a frame, such as OSC 52.
    fn write(&mut self, sequence: &str) -> io::Result<()>;
}

/// Owns raw mode, bracketed paste, autowrap, cursor visibility, synchronized
/// output, and either the alternate screen or the inline cursor save slot.
/// [`TerminalSession::close`] restores
/// them and reports failures; dropping it restores them silently if `close`
/// did not run.
pub struct TerminalSession {
    /// Dropped only through [`release`], never by Ratatui's own drop alone.
    terminal: ManuallyDrop<Terminal<CrosstermBackend<Stdout>>>,
    inline: Option<Inline>,
}

impl TerminalSession {
    pub fn acquire() -> io::Result<Self> {
        Self::acquire_mode(ScreenMode::Fullscreen)
    }

    /// Acquires the terminal for an explicitly selected preview screen.
    pub fn acquire_mode(mode: ScreenMode) -> io::Result<Self> {
        if MODES
            .compare_exchange(0, OWNED, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(io::Error::other("the terminal is already in use"));
        }
        install_panic_hook();
        let setup = || {
            enable_raw_mode()?;
            MODES.fetch_or(RAW, Ordering::SeqCst);
            // Input as VT sequences, for bracketed paste (`windows_input`);
            // a console that refuses it keeps Crossterm's key records.
            #[cfg(windows)]
            crate::windows_input::enable();
            let mut out = io::stdout();
            if mode == ScreenMode::Fullscreen {
                MODES.fetch_or(ALTERNATE, Ordering::SeqCst);
                // Cleared directly: `Terminal::clear` queries the cursor position,
                // which waits on a reply that some terminals never send.
                execute!(out, EnterAlternateScreen, Clear(ClearType::All))?;
            } else {
                MODES.fetch_or(INLINE, Ordering::SeqCst);
            }
            MODES.fetch_or(PASTE, Ordering::SeqCst);
            execute!(out, EnableBracketedPaste)?;
            MODES.fetch_or(MOUSE, Ordering::SeqCst);
            execute!(out, Print(MOUSE_ON))?;
            // A row the terminal draws wider than measured is clipped at the
            // right edge instead of wrapping onto the row below.
            MODES.fetch_or(NO_WRAP, Ordering::SeqCst);
            execute!(out, DisableLineWrap)?;
            match mode {
                ScreenMode::Fullscreen => Terminal::new(CrosstermBackend::new(out)),
                ScreenMode::Inline => inline::fixed(Rect::default()),
            }
        };
        match setup() {
            Ok(terminal) => Ok(Self {
                terminal: ManuallyDrop::new(terminal),
                inline: (mode == ScreenMode::Inline).then(Inline::default),
            }),
            Err(err) => {
                let _ = restore();
                Err(err)
            }
        }
    }

    /// Restores the terminal and returns the first restoration error. Every
    /// step is attempted even if an earlier one fails.
    pub fn close(self) -> io::Result<()> {
        // `Drop` then finds nothing left to restore.
        restore()
    }
}

impl Screen for TerminalSession {
    /// Draws one frame as a single synchronized update, so a supporting
    /// terminal never shows it half drawn.
    ///
    /// When `stale`, the screen is cleared and the last frame forgotten first,
    /// so every cell is repainted. A shrink and grow between draws can end at
    /// the size of the last frame while the terminal has reflowed what it
    /// showed; Ratatui's own size check would then skip the clear and leave
    /// stale rows. The clear is inside the update, so a resize does not flash
    /// an empty screen.
    fn draw(&mut self, state: &mut State, stale: bool) -> io::Result<()> {
        MODES.fetch_or(SYNC, Ordering::SeqCst);
        execute!(self.terminal.backend_mut(), BeginSynchronizedUpdate)?;
        let drawn = (|| {
            if let Some(inline) = &mut self.inline {
                let held = inline.draw(&mut self.terminal, state)?;
                HELD_ROWS.store(held, Ordering::SeqCst);
                return Ok(());
            }
            if stale {
                let size = self.terminal.size()?;
                self.terminal.resize(Rect::from(size))?;
            }
            self.terminal.draw(|frame| render(state, frame))?;
            Ok(())
        })();
        let ended = execute!(self.terminal.backend_mut(), EndSynchronizedUpdate);
        if ended.is_ok() {
            MODES.fetch_and(!SYNC, Ordering::SeqCst);
        }
        drawn.and(ended)
    }

    fn write(&mut self, sequence: &str) -> io::Result<()> {
        execute!(self.terminal.backend_mut(), Print(sequence))
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = restore();
        release(&mut self.terminal);
    }
}

/// Drops `terminal` once the modes are restored. Ratatui's drop shows a
/// cursor it hid, such as while an approval takes the keyboard, and prints
/// a failure to standard error, which panics on a terminal that has gone
/// away. [`restore`] has already shown the cursor, so a terminal that
/// cannot show it again is left undropped. This retains its screen buffers
/// and standard-output handle until process exit after terminal loss; no
/// worker or runtime task is owned by this terminal value.
fn release(terminal: &mut ManuallyDrop<Terminal<CrosstermBackend<Stdout>>>) {
    if terminal.show_cursor().is_ok() {
        // SAFETY: `terminal` is not used again: this runs once, from the
        // session's drop.
        unsafe { ManuallyDrop::drop(terminal) };
    }
}

/// Undoes the recorded modes in reverse order, once. Every step runs even if
/// an earlier one fails; the first error is returned.
fn restore() -> io::Result<()> {
    let modes = MODES.swap(0, Ordering::SeqCst);
    if modes == 0 {
        return Ok(());
    }
    let mut out = io::stdout();
    let mut result = Ok(());
    let mut keep = |step: io::Result<()>| {
        if result.is_ok() {
            result = step;
        }
    };
    if modes & SYNC != 0 {
        keep(execute!(out, EndSynchronizedUpdate));
    }
    // DECRC may restore saved rendition and autowrap as well as position;
    // release the inline save slot before restoring those terminal modes.
    if modes & INLINE != 0 {
        let held = HELD_ROWS.swap(0, Ordering::SeqCst);
        if held > 0 {
            keep(execute!(
                out,
                RestorePosition,
                Clear(ClearType::FromCursorDown)
            ));
        }
    }
    if modes & NO_WRAP != 0 {
        keep(execute!(out, EnableLineWrap));
    }
    if modes & MOUSE != 0 {
        keep(execute!(out, Print(MOUSE_OFF)));
    }
    if modes & PASTE != 0 {
        keep(execute!(out, DisableBracketedPaste));
    }
    if modes & ALTERNATE != 0 {
        keep(execute!(out, LeaveAlternateScreen));
    }
    keep(execute!(out, Show));
    if modes & RAW != 0 {
        #[cfg(windows)]
        keep(crate::windows_input::disable());
        keep(disable_raw_mode());
    }
    result
}

fn install_panic_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = restore();
            previous(info);
        }));
    });
}

/// Runs the fullscreen preview until Ctrl-C or a handled signal. The caller
/// must have checked that stdin and stdout are terminals.
///
/// The input thread is joined and the terminal restored before this returns.
/// A loop error takes priority over a restoration error; otherwise a
/// restoration error is returned.
pub fn run_preview() -> io::Result<PreviewExit> {
    run_preview_mode(ScreenMode::Fullscreen)
}

/// Runs the existing sample preview in the selected terminal screen.
pub fn run_preview_mode(mode: ScreenMode) -> io::Result<PreviewExit> {
    run(mode, Runtime::Fixture)
}

/// Runs a live agent session in the selected terminal screen: the same
/// loop, composer, and transcript as the preview, answered by the session
/// instead of the fixture. The caller must have checked that stdin and
/// stdout are terminals. The session is shut down, awaiting its run and
/// writes, before the terminal is restored and this returns.
pub fn run_live(mode: ScreenMode, live: LiveSession) -> io::Result<PreviewExit> {
    run(mode, Runtime::Live(Box::new(live)))
}

/// What answers the composer.
enum Runtime {
    Fixture,
    Live(Box<LiveSession>),
}

/// The running runtime port.
enum Port {
    Fixture(FixturePort),
    Live(LivePort),
}

impl Port {
    fn request(&self, request: Request) {
        match self {
            Self::Fixture(port) => port.request(request),
            Self::Live(port) => port.request(request),
        }
    }

    fn stop(self) -> io::Result<()> {
        match self {
            Self::Fixture(port) => port.stop(),
            Self::Live(port) => port.stop(),
        }
    }
}

/// A port before the terminal is set up: a live one that owns its session,
/// or the fixture's update sender.
enum Unstarted<F> {
    Live(LivePort),
    Fixture(F),
}

fn run(mode: ScreenMode, runtime: Runtime) -> io::Result<PreviewExit> {
    let env = |name: &str| std::env::var(name).ok();
    let (frame_style, tones) = (frame::resolve(env, cfg!(windows)), Tones::resolve(env));
    // The session's rows are read before the terminal changes mode.
    let mut state = match &runtime {
        Runtime::Fixture => State::new(frame_style, tones),
        Runtime::Live(live) => State::live(frame_style, tones, live.initial_rows()),
    };
    let (sender, inputs) = mpsc::channel();
    // One clock for the loop's ticks, the reader's timestamps, and the
    // fixture's timeline.
    let clock = Instant::now();
    let updates = sender.clone();
    let send = move |update| updates.send(Source::Msg(Msg::Runtime(update))).is_ok();
    // A live port owns the session from here on, but runs nothing until the
    // terminal is set up: if setup fails, dropping it shuts the session
    // down.
    let unstarted = match runtime {
        Runtime::Fixture => Unstarted::Fixture(send),
        Runtime::Live(live) => Unstarted::Live(LivePort::start(*live, send)),
    };
    // Declared before the terminal session so it is dropped after it:
    // signals stay handled until the terminal is restored.
    let signals = signals::Forwarder::start(sender.clone())?;
    let mut session = TerminalSession::acquire_mode(mode)?;
    state.status.cwd = working_directory(env);
    state.wheel = WheelSteps::new(wheel::reports(env, cfg!(target_os = "macos")));
    state.status.branch = std::env::current_dir()
        .ok()
        .and_then(|cwd| git::branch(&cwd));
    // Started only once raw mode is on, so it never reads cooked input.
    let found = sender.clone();
    let watched = sender.clone();
    let shown = Arc::new(AtomicU64::new(0));
    let outcome = Reader::start(sender, clock, Arc::clone(&shown)).and_then(|reader| {
        let watcher = match hangup::Watcher::start(watched) {
            Ok(watcher) => watcher,
            Err(err) => {
                let _ = reader.stop();
                return Err(err);
            }
        };
        let clipboard = Clipboard::new(Platform::current(), env);
        let home = env(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
        // Declared after the terminal session, so even a panic shuts a live
        // session down before the terminal is restored.
        let port = match unstarted {
            Unstarted::Live(mut live) => {
                live.begin();
                Port::Live(live)
            }
            Unstarted::Fixture(send) => Port::Fixture(FixturePort::start(clock, send)?),
        };
        let root = std::env::current_dir().unwrap_or_else(|_| ".".into());
        let finder = Finder::start(root, move |query, found_paths| {
            let msg = Msg::FilesFound {
                query,
                found: found_paths,
            };
            found.send(Source::Msg(msg)).is_ok()
        })?;
        let outcome = run_loop(
            &mut session,
            &mut state,
            &inputs,
            clock,
            Owners {
                clipboard,
                home,
                runtime: &|request| port.request(request),
                files: &|ask| finder.ask(ask),
                shown: &shown,
            },
        );
        // Joined before the terminal leaves raw mode, so it reads nothing
        // meant for the shell, and no runtime update or found path outlives
        // the loop. After a hangup there is no shell, and the reader may be
        // stuck, so it is waited for only briefly.
        let ported = port.stop();
        let searched = finder.stop();
        drop(watcher);
        let stopped = if matches!(outcome, Ok(PreviewExit::Hangup)) {
            reader.abandon(HANGUP_JOIN);
            Ok(())
        } else {
            reader.stop()
        };
        outcome.and_then(|exit| ported.and(searched).and(stopped).map(|()| exit))
    });
    let closed = session.close();
    drop(signals);
    let exit = outcome?;
    if exit != PreviewExit::Hangup {
        closed?;
    }
    Ok(exit)
}

/// Whether an I/O error means the terminal is gone: EIO from a hung-up
/// terminal, or EPIPE (on Windows, a closed console's pipe) from a closed
/// reader.
fn hung_up(err: &io::Error) -> bool {
    #[cfg(unix)]
    if err.raw_os_error() == Some(libc::EIO) {
        return true;
    }
    err.kind() == io::ErrorKind::BrokenPipe
}

/// What the loop hands effects to: the clipboard and image jobs, the
/// runtime port, and path discovery; and the token of the approval
/// request's latest showing, which the input thread arms.
struct Owners<'a> {
    clipboard: Clipboard,
    home: Option<String>,
    runtime: &'a dyn Fn(Request),
    files: &'a dyn Fn(Ask),
    shown: &'a AtomicU64,
}

/// Draws, waits for the next message or the view's next timed change, applies
/// the current time and then every waiting message, and draws again. A burst
/// of keys or resizes therefore draws once, and a batch of runtime updates
/// alone waits until [`RUNTIME_FRAME`] after the last frame. Returns when
/// the view asks to quit, a signal arrives, or input fails.
fn run_loop(
    screen: &mut impl Screen,
    state: &mut State,
    inputs: &Receiver<Source>,
    clock: Instant,
    owners: Owners,
) -> io::Result<PreviewExit> {
    match drive(screen, state, inputs, clock, owners) {
        Err(err) if hung_up(&err) => Ok(PreviewExit::Hangup),
        outcome => outcome,
    }
}

/// The loop itself; [`run_loop`] turns a lost terminal's errors into a
/// hangup.
fn drive(
    screen: &mut impl Screen,
    state: &mut State,
    inputs: &Receiver<Source>,
    clock: Instant,
    owners: Owners,
) -> io::Result<PreviewExit> {
    let mut jobs = Jobs::new(owners.clipboard, owners.home);
    let mut stale = false;
    // When the last frame was drawn, and whether a runtime-only batch is
    // waiting for its turn to be drawn.
    let mut drawn = clock.elapsed();
    let mut deferred = false;
    let mut first_frame = true;
    loop {
        let now = clock.elapsed();
        update(state, Msg::Tick(now));
        jobs.settle(screen, state)?;
        if first_frame || !deferred || now >= drawn + RUNTIME_FRAME {
            screen.draw(state, stale)?;
            // Published only once the frame is out: the input thread arms
            // the showing after it has read every key waiting by then.
            if let Some(token) = state.approval_drawn() {
                owners.shown.store(token, Ordering::SeqCst);
            }
            (stale, deferred, first_frame, drawn) = (false, false, false, now);
        }
        // While a copy is pending, the loop also wakes to report it, and
        // while a frame is deferred, to draw it.
        let mut wait = state.next_change();
        if jobs.pending > 0 {
            wait = Some(wait.map_or(COPY_POLL, |wait| wait.min(COPY_POLL)));
        }
        if deferred {
            let due = (drawn + RUNTIME_FRAME).saturating_sub(clock.elapsed());
            wait = Some(wait.map_or(due, |wait| wait.min(due)));
        }
        let first = match wait {
            Some(wait) => match inputs.recv_timeout(wait) {
                Ok(source) => source,
                // Something timed is due: draw it whatever is deferred.
                Err(RecvTimeoutError::Timeout) => {
                    deferred = false;
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return Err(input_stopped()),
            },
            None => inputs.recv().map_err(|_| input_stopped())?,
        };
        update(state, Msg::Tick(clock.elapsed()));
        let mut source = first;
        let mut applied = 0;
        let mut input = false;
        loop {
            match source {
                Source::Signal(signal) => return Ok(PreviewExit::Signal(signal)),
                Source::Hangup => return Ok(PreviewExit::Hangup),
                Source::Failed(err) => return Err(err),
                Source::Msg(msg) => {
                    input |= !matches!(msg, Msg::Runtime(_));
                    // A finished turn may have changed the tree, so later
                    // bare queries see it once the traversal is rebuilt.
                    if matches!(msg, Msg::Runtime(RuntimeUpdate::TurnEnded(_))) {
                        (owners.files)(Ask::Invalidate);
                    }
                    stale |= matches!(msg, Msg::Resize { .. });
                    for effect in update(state, msg) {
                        match effect {
                            Effect::Quit => return Ok(PreviewExit::Quit),
                            Effect::Copy(text) => jobs.copy(screen, state, text)?,
                            Effect::ReadImage(source) => jobs.read_image(source),
                            Effect::Submit(submission) => {
                                (owners.runtime)(Request::Submit(submission));
                            }
                            Effect::Cancel => (owners.runtime)(Request::Cancel),
                            Effect::SendPending => (owners.runtime)(Request::SendPending),
                            Effect::FindFiles(query) => (owners.files)(Ask::Find(query)),
                            Effect::Answer { generation, answer } => {
                                (owners.runtime)(Request::Answer { generation, answer });
                            }
                            Effect::Dialog { id, answer } => {
                                (owners.runtime)(Request::Dialog { id, answer });
                            }
                            Effect::StopJob(id) => (owners.runtime)(Request::StopJob(id)),
                            Effect::StopAgent(id) => (owners.runtime)(Request::StopAgent(id)),
                        }
                    }
                }
            }
            applied += 1;
            // Past the cap, what is still waiting stays queued for the next batch.
            if applied == MAX_BATCH {
                break;
            }
            match inputs.try_recv() {
                Ok(waiting) => source = waiting,
                Err(_) => break,
            }
        }
        // A batch with input is drawn at once; runtime updates alone wait
        // for their frame, unless one is already waiting.
        deferred = !input;
    }
}

/// The working directory for the status line, under home as `~`; empty when
/// it cannot be read, so the status line leaves it out.
fn working_directory(env: impl Fn(&str) -> Option<String>) -> String {
    let Ok(cwd) = std::env::current_dir() else {
        return String::new();
    };
    let home = env(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    status::home_relative(&cwd.to_string_lossy(), home.as_deref())
}

fn input_stopped() -> io::Error {
    io::Error::other("terminal input stopped")
}

/// The input thread: decodes Crossterm events and sends them to the loop.
struct Reader {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Reader {
    fn start(sender: Sender<Source>, clock: Instant, shown: Arc<AtomicU64>) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("bake-tui-input".into())
            .spawn(move || {
                #[cfg(windows)]
                if crate::windows_input::active() {
                    match crate::windows_input::VtConsole::open() {
                        Ok(mut console) => {
                            return read_input(Stopped(sender), &flag, clock, &shown, &mut console);
                        }
                        Err(err) => {
                            let _ = sender.send(Source::Failed(err));
                            return;
                        }
                    }
                }
                read_input(Stopped(sender), &flag, clock, &shown, &mut Crossterm);
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    /// Stops the thread and waits for it; reports a panic in it.
    fn stop(mut self) -> io::Result<()> {
        self.join()
    }

    /// Stops the thread and waits at most `limit` for it; a thread still
    /// running then is left to end with the process.
    fn abandon(mut self, limit: Duration) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            join_within(thread, limit);
        }
    }

    fn join(&mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::SeqCst);
        match self.thread.take() {
            Some(thread) => thread
                .join()
                .map_err(|_| io::Error::other("the terminal input thread panicked")),
            None => Ok(()),
        }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

/// Joins `thread` if it finishes within `limit`, and otherwise detaches it.
/// Returns whether it finished.
fn join_within(thread: JoinHandle<()>, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while !thread.is_finished() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let _ = thread.join();
    true
}

/// Tells the loop that input has ended however the input thread exits,
/// including by a panic, so the loop never waits on a reader that is gone.
struct Stopped(Sender<Source>);

impl Drop for Stopped {
    fn drop(&mut self) {
        let _ = self.0.send(Source::Failed(input_stopped()));
    }
}

/// Where the input thread reads events: the terminal through Crossterm,
/// or a test's script.
trait Events {
    /// Whether an event is ready within `wait`.
    fn poll(&mut self, wait: Duration) -> io::Result<bool>;
    /// The next event, waiting for one.
    fn read(&mut self) -> io::Result<event::Event>;
}

/// The terminal's events.
struct Crossterm;

impl Events for Crossterm {
    fn poll(&mut self, wait: Duration) -> io::Result<bool> {
        event::poll(wait)
    }

    fn read(&mut self) -> io::Result<event::Event> {
        event::read()
    }
}

/// The console's VT input on Windows ([`crate::windows_input`]).
#[cfg(windows)]
impl Events for crate::windows_input::VtConsole {
    fn poll(&mut self, wait: Duration) -> io::Result<bool> {
        crate::windows_input::VtConsole::poll(self, wait)
    }

    fn read(&mut self) -> io::Result<event::Event> {
        crate::windows_input::VtConsole::read(self)
    }
}

/// Events the input thread reads in one sweep before it sends them.
const SWEEP: usize = 1024;
/// How long a sweep polls for the next waiting event. Crossterm 0.29's
/// `/dev/tty` source answers a zero timeout without looking, not even at
/// an event it has already parsed, so the sweep asks with the shortest
/// wait it honours. Events already waiting, parsed or in the terminal's
/// buffer, are found at once; the wait only lets a key that arrives during
/// it join the sweep, which never lets a key count as later than it was.
const SWEEP_POLL: Duration = Duration::from_millis(1);

/// Reads events until stopped, in sweeps: every event already waiting is
/// read before any is sent, so a sweep with more than one key marks its
/// keys as a burst ([`stamp`]).
///
/// Before each wait for input, the thread checks `shown`, the token of
/// the approval request's latest showing. A showing not armed yet is armed
/// here: every waiting event is swept and sent first, until the input
/// observably runs dry, and [`Msg::ApprovalArmed`] follows. Input found
/// while a request is waiting to be armed is left for that sweep, so it
/// too goes ahead of the arming.
fn read_input(
    out: Stopped,
    stop: &AtomicBool,
    clock: Instant,
    shown: &AtomicU64,
    events: &mut impl Events,
) {
    let fail = |err| {
        let _ = out.0.send(Source::Failed(err));
    };
    let send = |msgs: Vec<Msg>| {
        msgs.into_iter()
            .all(|msg| out.0.send(Source::Msg(msg)).is_ok())
    };
    let mut armed = 0;
    let mut decoder = input::Decoder::new();
    while !stop.load(Ordering::SeqCst) {
        let drawn = shown.load(Ordering::SeqCst);
        if drawn != armed {
            loop {
                // A stream that never runs dry must not hold up a stop.
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                let (sweep, dry) = match sweep(events, &mut decoder, clock) {
                    Ok(swept) => swept,
                    Err(err) => return fail(err),
                };
                if !send(stamp(sweep)) {
                    return;
                }
                if dry {
                    break;
                }
            }
            if !send(vec![Msg::ApprovalArmed(drawn)]) {
                return;
            }
            armed = drawn;
            continue;
        }
        match events.poll(READER_WAKE) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(err) => return fail(err),
        }
        if shown.load(Ordering::SeqCst) != armed {
            continue;
        }
        match sweep(events, &mut decoder, clock) {
            Ok((swept, _)) => {
                if !send(stamp(swept)) {
                    return;
                }
            }
            Err(err) => return fail(err),
        }
    }
}

/// Reads every waiting event, up to [`SWEEP`] events read, whether or not
/// they decode to anything; `true` with them when the input then ran dry,
/// `false` when the sweep stopped at its size.
fn sweep(
    events: &mut impl Events,
    decoder: &mut input::Decoder,
    clock: Instant,
) -> io::Result<(Vec<Msg>, bool)> {
    let mut swept = Vec::new();
    let mut read = 0;
    while events.poll(SWEEP_POLL)? {
        swept.extend(decoder.decode(events.read()?, clock.elapsed()));
        read += 1;
        if read >= SWEEP {
            return Ok((swept, false));
        }
    }
    Ok((swept, true))
}

/// Marks the keys of one sweep as a burst when it holds more than one key.
fn stamp(sweep: Vec<Msg>) -> Vec<Msg> {
    let keys = sweep
        .iter()
        .filter(|msg| matches!(msg, Msg::Key(_) | Msg::KeyRepeat(_)))
        .count();
    sweep
        .into_iter()
        .map(|msg| match msg {
            Msg::Key(key) => Msg::KeyRead {
                key,
                burst: keys > 1,
            },
            other => other,
        })
        .collect()
}

#[cfg(unix)]
mod signals {
    use std::io;
    use std::sync::mpsc::Sender;
    use std::thread::{self, JoinHandle};

    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::{Handle, Signals};

    use super::Source;

    fn source(signal: i32) -> Source {
        if signal == SIGHUP {
            Source::Hangup
        } else {
            Source::Signal(signal)
        }
    }

    /// Forwards SIGINT, SIGTERM, and SIGHUP to the loop instead of
    /// terminating, so the loop can restore the terminal. Raw mode turns the
    /// Ctrl-C key into input, so SIGINT arrives only from another process.
    ///
    /// Dropping it closes and joins its thread, which removes only these
    /// registrations. signal-hook keeps its process handler installed and does
    /// not restore the previous disposition, so the same signals are ignored
    /// from then until exit.
    pub struct Forwarder {
        handle: Handle,
        thread: Option<JoinHandle<()>>,
    }

    impl Forwarder {
        pub fn start(sender: Sender<Source>) -> io::Result<Self> {
            let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])?;
            let handle = signals.handle();
            let thread = thread::Builder::new()
                .name("bake-tui-signals".into())
                .spawn(move || {
                    for signal in signals.forever() {
                        if sender.send(source(signal)).is_err() {
                            break;
                        }
                    }
                })?;
            Ok(Self {
                handle,
                thread: Some(thread),
            })
        }
    }

    impl Drop for Forwarder {
        fn drop(&mut self) {
            self.handle.close();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

#[cfg(unix)]
mod hangup {
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::Sender;
    use std::thread::{self, JoinHandle};

    use super::{READER_WAKE, Source};

    /// Sends [`Source::Hangup`] once standard input or output reports a
    /// hangup or an error. The kernel hangs a terminal up when its emulator
    /// closes, and SIGHUP follows only for a process that the terminal
    /// controls; meanwhile Crossterm's reader may keep polling the closed
    /// terminal without returning, so the loop would wait forever.
    pub struct Watcher {
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Watcher {
        pub fn start(sender: Sender<Source>) -> io::Result<Self> {
            let stop = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&stop);
            let thread = thread::Builder::new()
                .name("bake-tui-hangup".into())
                .spawn(move || watch(&[libc::STDIN_FILENO, libc::STDOUT_FILENO], &sender, &flag))?;
            Ok(Self {
                stop,
                thread: Some(thread),
            })
        }
    }

    impl Drop for Watcher {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// Polls `fds` for input, waking every [`READER_WAKE`] to check `stop`.
    /// Input is asked for because macOS builds `poll` on kqueue and reports a
    /// hangup only on a descriptor that asked for an event; the input itself
    /// is left to the reader, so while it waits the watcher sleeps a beat
    /// instead of polling again at once. A descriptor poll cannot watch,
    /// which macOS reports for some devices, ends the watch instead of
    /// counting as a hangup.
    pub(super) fn watch(fds: &[libc::c_int], sender: &Sender<Source>, stop: &AtomicBool) {
        let wake = libc::c_int::try_from(READER_WAKE.as_millis()).unwrap_or(libc::c_int::MAX);
        let mut polled: Vec<libc::pollfd> = fds
            .iter()
            .map(|&fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let count = libc::nfds_t::try_from(polled.len()).unwrap_or(0);
        while !stop.load(Ordering::SeqCst) {
            // SAFETY: `polled` holds `count` initialized records and outlives
            // the call.
            let ready = unsafe { libc::poll(polled.as_mut_ptr(), count, wake) };
            if ready < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
            let events = polled.iter().fold(0, |all, fd| all | fd.revents);
            if events & libc::POLLNVAL != 0 {
                return;
            }
            if events & (libc::POLLHUP | libc::POLLERR) != 0 {
                let _ = sender.send(Source::Hangup);
                return;
            }
            if ready > 0 {
                thread::sleep(READER_WAKE);
            }
        }
    }
}

#[cfg(not(unix))]
mod hangup {
    use std::io;
    use std::sync::mpsc::Sender;

    use super::Source;

    /// Watches nothing: a closed console fails the next write with a broken
    /// pipe, which ends the loop as a hangup. It holds its sender for as long
    /// as the Unix watcher's thread would, as the signal forwarder does.
    pub struct Watcher {
        _sender: Sender<Source>,
    }

    impl Watcher {
        pub fn start(sender: Sender<Source>) -> io::Result<Self> {
            Ok(Self { _sender: sender })
        }
    }
}

#[cfg(not(unix))]
mod signals {
    use std::io;
    use std::sync::mpsc::Sender;

    use super::Source;

    /// Forwards nothing: the console's Ctrl+C reaches the preview as a key
    /// in raw mode. It holds its sender for as long as the Unix forwarder's
    /// thread would, so the channel closes at the same point on every OS.
    pub struct Forwarder {
        _sender: Sender<Source>,
    }

    impl Forwarder {
        pub fn start(sender: Sender<Source>) -> io::Result<Self> {
            Ok(Self { _sender: sender })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bake_tui_view::keys::{Key, KeyInput, Mods};
    use bake_tui_view::transcript::Row;

    fn key(c: char) -> Source {
        Source::Msg(Msg::Key(KeyInput::plain(Key::Char(c))))
    }

    fn ctrl(c: char) -> Source {
        Source::Msg(Msg::Key(KeyInput::new(Key::Char(c), Mods::CTRL)))
    }

    /// Records each frame and, after the frame at index `n`, sends the `n`th
    /// batch of the script, as if it had arrived while that frame was shown.
    /// Once the script runs out it ends input, unless `wait_for_tick` holds
    /// input open until a frame is drawn without any message.
    struct Scripted {
        sender: Option<Sender<Source>>,
        script: Vec<Vec<Source>>,
        frames: Vec<(String, bool, Duration)>,
    }

    impl Scripted {
        fn new(script: Vec<Vec<Source>>) -> (Self, Receiver<Source>) {
            let (sender, inputs) = mpsc::channel();
            let screen = Self {
                sender: Some(sender),
                script: script.into_iter().rev().collect(),
                frames: Vec::new(),
            };
            (screen, inputs)
        }
    }

    impl Screen for Scripted {
        fn write(&mut self, _sequence: &str) -> io::Result<()> {
            Ok(())
        }

        fn draw(&mut self, state: &mut State, stale: bool) -> io::Result<()> {
            self.frames
                .push((state.draft.text().to_owned(), stale, state.now));
            match self.script.pop() {
                Some(batch) => {
                    for source in batch {
                        self.sender.as_ref().unwrap().send(source).unwrap();
                    }
                }
                None => self.sender = None,
            }
            Ok(())
        }
    }

    /// Records the sequences written outside frames.
    #[derive(Default)]
    struct Recorder(Vec<String>);

    impl Screen for Recorder {
        fn draw(&mut self, _state: &mut State, _stale: bool) -> io::Result<()> {
            Ok(())
        }

        fn write(&mut self, sequence: &str) -> io::Result<()> {
            self.0.push(sequence.to_owned());
            Ok(())
        }
    }

    #[test]
    fn over_ssh_a_copy_goes_to_the_terminal_at_once() {
        let mut jobs = Jobs::new(no_clipboard(), None);
        let (mut screen, mut state) = (Recorder::default(), State::default());
        jobs.copy(&mut screen, &mut state, "hi".into()).unwrap();
        assert_eq!(screen.0, ["\x1b]52;c;aGk=\x07"]);
        assert_eq!(state.copied.map(|(ok, _)| ok), Some(true));
        assert_eq!(jobs.pending, 0);
    }

    #[test]
    fn a_copy_no_tool_took_falls_back_to_the_terminal() {
        let mut jobs = Jobs::new(no_clipboard(), None);
        let (mut screen, mut state) = (Recorder::default(), State::default());
        // As a worker reports: one copy a tool took, one none did.
        jobs.pending = 2;
        jobs.done.send(Done::Copy("taken".into(), true)).unwrap();
        jobs.done.send(Done::Copy("hi".into(), false)).unwrap();
        jobs.settle(&mut screen, &mut state).unwrap();
        assert_eq!(screen.0, ["\x1b]52;c;aGk=\x07"]);
        assert_eq!(jobs.pending, 0);
        assert_eq!(state.copied.map(|(ok, _)| ok), Some(true));
    }

    #[test]
    fn a_pasted_path_is_read_off_the_loop_and_staged_when_it_settles() {
        let dir = std::env::temp_dir().join(format!("bake-jobs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("shot.gif"), b"GIF89a\x04\x00\x03\x00rest").unwrap();
        let mut jobs = Jobs::new(no_clipboard(), None);
        let (mut screen, mut state) = (Recorder::default(), State::default());
        let path = dir.join("shot.gif").to_string_lossy().into_owned();
        jobs.read_image(ImageSource::File {
            path: path.clone(),
            pasted: path,
        });
        assert_eq!(jobs.pending, 1);
        while jobs.pending > 0 {
            jobs.settle(&mut screen, &mut state).unwrap();
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(state.draft.text(), "[Image #1]");
        assert_eq!(
            state.attachments[0].1.summary(),
            "shot.gif · image/gif · 14 B · 4×3"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A clipboard with no tools: copies go to the terminal itself.
    fn no_clipboard() -> Clipboard {
        Clipboard::new(Platform::Other, |name| {
            (name == "SSH_TTY").then(|| "/dev/pts/0".to_owned())
        })
    }

    fn owners(runtime: &dyn Fn(Request)) -> Owners<'_> {
        Owners {
            clipboard: no_clipboard(),
            home: None,
            runtime,
            files: &|_| {},
            // Each loop publishes to its own.
            shown: Box::leak(Box::new(AtomicU64::new(0))),
        }
    }

    fn run(screen: &mut Scripted, inputs: &Receiver<Source>) -> io::Result<PreviewExit> {
        run_loop(
            screen,
            &mut State::default(),
            inputs,
            Instant::now(),
            owners(&|_| {}),
        )
    }

    fn runtime(update: RuntimeUpdate) -> Source {
        Source::Msg(Msg::Runtime(update))
    }

    #[test]
    fn submit_and_cancel_reach_the_runtime() {
        let (mut screen, inputs) = Scripted::new(vec![
            vec![
                key('h'),
                Source::Msg(Msg::Key(KeyInput::plain(Key::Enter))),
                runtime(RuntimeUpdate::TurnStarted { seed: "t".into() }),
            ],
            vec![Source::Msg(Msg::Key(KeyInput::plain(Key::Esc)))],
            vec![ctrl('c'), ctrl('c')],
        ]);
        let requests = std::cell::RefCell::new(Vec::new());
        let exit = run_loop(
            &mut screen,
            &mut State::default(),
            &inputs,
            Instant::now(),
            owners(&|request| {
                requests.borrow_mut().push(match request {
                    Request::Submit(prompt) => format!("Submit({:?})", prompt.submission),
                    other => format!("{other:?}"),
                });
            }),
        );
        assert_eq!(exit.unwrap(), PreviewExit::Quit);
        assert_eq!(requests.into_inner(), ["Submit(FollowUp(\"h\"))", "Cancel"]);
    }

    #[test]
    fn runtime_updates_alone_wait_for_their_frame_and_input_does_not() {
        let live = |text: &str| runtime(RuntimeUpdate::Live(vec![Row::Answer(text.into())]));
        let (mut screen, inputs) = Scripted::new(vec![
            // Arrives as soon as the first frame is shown: deferred.
            vec![live("a")],
            // Arrives once that frame is drawn: deferred again.
            vec![live("ab")],
            // Input with an update draws at once.
            vec![live("abc"), key('x')],
            vec![ctrl('c'), ctrl('c')],
        ]);
        assert_eq!(run(&mut screen, &inputs).unwrap(), PreviewExit::Quit);
        let times: Vec<Duration> = screen.frames.iter().map(|(.., now)| *now).collect();
        assert_eq!(times.len(), 4, "{times:?}");
        assert!(times[1] - times[0] >= RUNTIME_FRAME, "{times:?}");
        assert!(times[2] - times[1] >= RUNTIME_FRAME, "{times:?}");
        assert!(times[3] - times[2] < RUNTIME_FRAME, "{times:?}");
        assert_eq!(screen.frames[3].0, "x");
    }

    #[test]
    fn a_waiting_batch_is_applied_together_and_drawn_once() {
        let (mut screen, inputs) = Scripted::new(vec![
            vec![
                key('a'),
                Source::Msg(Msg::Resize { cols: 40, rows: 12 }),
                Source::Msg(Msg::Paste("bc".into())),
            ],
            // The second Ctrl+C quits; the key after it is never applied.
            vec![key('d'), ctrl('c'), ctrl('c'), key('e')],
        ]);
        assert_eq!(run(&mut screen, &inputs).unwrap(), PreviewExit::Quit);
        let frames: Vec<_> = screen
            .frames
            .iter()
            .map(|(text, stale, _)| (text.as_str(), *stale))
            .collect();
        // The resize repaints every cell once; the quit is not drawn.
        assert_eq!(frames, [("", false), ("abc", true)]);
    }

    #[test]
    fn a_signal_ends_the_loop_before_the_rest_of_its_batch() {
        let (mut screen, inputs) =
            Scripted::new(vec![vec![key('x'), Source::Signal(15), key('y')]]);
        assert_eq!(run(&mut screen, &inputs).unwrap(), PreviewExit::Signal(15));
        assert_eq!(screen.frames.len(), 1);
    }

    #[test]
    fn a_hangup_ends_the_loop_before_the_rest_of_its_batch() {
        let (mut screen, inputs) = Scripted::new(vec![vec![key('x'), Source::Hangup, key('y')]]);
        assert_eq!(run(&mut screen, &inputs).unwrap(), PreviewExit::Hangup);
        assert_eq!(screen.frames.len(), 1);
    }

    /// Fails every frame with one error.
    struct Lost(fn() -> io::Error);

    impl Screen for Lost {
        fn draw(&mut self, _state: &mut State, _stale: bool) -> io::Result<()> {
            Err((self.0)())
        }

        fn write(&mut self, _sequence: &str) -> io::Result<()> {
            Err((self.0)())
        }
    }

    fn draw_fails(error: fn() -> io::Error) -> io::Result<PreviewExit> {
        let (_sender, inputs) = mpsc::channel();
        let mut state = State::default();
        run_loop(
            &mut Lost(error),
            &mut state,
            &inputs,
            Instant::now(),
            owners(&|_| {}),
        )
    }

    #[test]
    fn a_write_to_a_lost_terminal_is_a_hangup_and_other_failures_are_errors() {
        let broken = || io::Error::from(io::ErrorKind::BrokenPipe);
        assert_eq!(draw_fails(broken).unwrap(), PreviewExit::Hangup);
        #[cfg(unix)]
        assert_eq!(
            draw_fails(|| io::Error::from_raw_os_error(libc::EIO)).unwrap(),
            PreviewExit::Hangup
        );
        let err = draw_fails(|| io::Error::other("disk full")).unwrap_err();
        assert_eq!(err.to_string(), "disk full");
    }

    #[test]
    fn a_thread_that_does_not_stop_in_time_is_left_behind() {
        let (release, wait) = mpsc::channel::<()>();
        let stuck = thread::spawn(move || {
            let _ = wait.recv();
        });
        assert!(!join_within(stuck, Duration::from_millis(20)));
        drop(release);
        assert!(join_within(thread::spawn(|| {}), Duration::from_secs(5)));
    }

    #[cfg(unix)]
    fn pipe() -> [libc::c_int; 2] {
        let mut ends = [0; 2];
        // SAFETY: `ends` has room for the two descriptors `pipe` writes.
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        ends
    }

    #[cfg(unix)]
    fn close(fd: libc::c_int) {
        // SAFETY: each test closes only descriptors its own `pipe` opened.
        unsafe { libc::close(fd) };
    }

    /// Watches `fd` on a thread until it reports or `limit` passes, then
    /// stops it, so a watcher that misses a hangup fails instead of hanging.
    #[cfg(unix)]
    fn watched(fd: libc::c_int, limit: Duration) -> bool {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let (sender, found) = mpsc::channel();
        let watcher = thread::spawn(move || hangup::watch(&[fd], &sender, &flag));
        let reported = matches!(found.recv_timeout(limit), Ok(Source::Hangup));
        stop.store(true, Ordering::SeqCst);
        watcher.join().unwrap();
        reported
    }

    #[cfg(unix)]
    #[test]
    fn the_watcher_reports_a_hung_up_descriptor_but_not_waiting_input() {
        let [read, write] = pipe();
        close(write);
        assert!(watched(read, Duration::from_secs(5)));
        close(read);

        // Input the reader has not taken yet is not a hangup.
        let [read, write] = pipe();
        // SAFETY: the buffer is one live byte.
        assert_eq!(unsafe { libc::write(write, b"x".as_ptr().cast(), 1) }, 1);
        assert!(!watched(read, READER_WAKE * 3));
        close(read);
        close(write);
    }

    #[test]
    fn failed_or_ended_input_is_an_error() {
        let (mut screen, inputs) =
            Scripted::new(vec![vec![Source::Failed(io::Error::other("read failed"))]]);
        let err = run(&mut screen, &inputs).unwrap_err();
        assert_eq!(err.to_string(), "read failed");

        let (mut screen, inputs) = Scripted::new(vec![vec![key('a')]]);
        let err = run(&mut screen, &inputs).unwrap_err();
        assert_eq!(err.to_string(), "terminal input stopped");
        assert_eq!(screen.frames.last().unwrap().0, "a");
    }

    #[test]
    fn a_long_burst_draws_after_each_full_batch() {
        let burst = (0..MAX_BATCH + 4).map(|_| key('z')).collect();
        let (mut screen, inputs) = Scripted::new(vec![burst, vec![], vec![ctrl('c'), ctrl('c')]]);
        assert_eq!(run(&mut screen, &inputs).unwrap(), PreviewExit::Quit);
        let lengths: Vec<_> = screen.frames.iter().map(|(text, ..)| text.len()).collect();
        assert_eq!(lengths, [0, MAX_BATCH, MAX_BATCH + 4]);
    }

    /// Starts a sample activity, then ends the loop once a frame has been
    /// drawn with no message behind it.
    struct UntilTick {
        sender: Sender<Source>,
        frames: Vec<(Duration, Option<Duration>)>,
    }

    impl Screen for UntilTick {
        fn write(&mut self, _sequence: &str) -> io::Result<()> {
            Ok(())
        }

        fn draw(&mut self, state: &mut State, _stale: bool) -> io::Result<()> {
            self.frames
                .push((state.now, state.activity.map(|sample| sample.started)));
            if self.frames.len() == 1 {
                self.sender.send(ctrl('t')).unwrap();
            } else if self.frames.len() > 2 {
                self.sender.send(ctrl('c')).unwrap();
                self.sender.send(ctrl('c')).unwrap();
            }
            Ok(())
        }
    }

    #[test]
    fn a_running_activity_redraws_at_its_next_beat_without_input() {
        let (sender, inputs) = mpsc::channel();
        let mut screen = UntilTick {
            sender,
            frames: Vec::new(),
        };
        let exit = run_loop(
            &mut screen,
            &mut State::default(),
            &inputs,
            Instant::now(),
            owners(&|_| {}),
        );
        assert_eq!(exit.unwrap(), PreviewExit::Quit);
        // Frame 1 shows the started sample; frame 2 came from its timer alone,
        // no sooner than its first beat.
        assert_eq!(screen.frames.len(), 3);
        let (now, started) = screen.frames[2];
        assert_eq!(started, screen.frames[1].1);
        assert!(now >= started.unwrap() + bake_tui_view::activity::BEAT);
    }

    // ---- Approvals ----

    /// Renders every frame with the real view into a test backend, records
    /// the request the loop published after it, then sends the next batch
    /// of the script, as [`Scripted`] does.
    struct Rendered {
        terminal: Terminal<ratatui::backend::TestBackend>,
        sender: Option<Sender<Source>>,
        script: Vec<Vec<Source>>,
        screens: Vec<String>,
        /// The terminal's size from a frame on, by frame index.
        sizes: Vec<(usize, u16, u16)>,
    }

    impl Rendered {
        fn new(script: Vec<Vec<Source>>) -> (Self, Receiver<Source>) {
            let (sender, inputs) = mpsc::channel();
            let screen = Self {
                terminal: Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap(),
                sender: Some(sender),
                script: script.into_iter().rev().collect(),
                screens: Vec::new(),
                sizes: Vec::new(),
            };
            (screen, inputs)
        }
    }

    impl Screen for Rendered {
        fn write(&mut self, _sequence: &str) -> io::Result<()> {
            Ok(())
        }

        fn draw(&mut self, state: &mut State, _stale: bool) -> io::Result<()> {
            let frame = self.screens.len();
            if let Some(&(_, cols, rows)) = self.sizes.iter().find(|(at, ..)| *at == frame) {
                self.terminal.backend_mut().resize(cols, rows);
            }
            self.terminal.draw(|frame| render(state, frame)).unwrap();
            let buffer = self.terminal.backend().buffer();
            let text: String = buffer
                .content()
                .chunks(usize::from(buffer.area.width))
                .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>() + "\n")
                .collect();
            self.screens.push(text);
            match self.script.pop() {
                Some(batch) => {
                    for source in batch {
                        self.sender.as_ref().unwrap().send(source).unwrap();
                    }
                }
                None => self.sender = None,
            }
            Ok(())
        }
    }

    fn approval(generation: u64, call_id: &str) -> bake_tui_view::runtime::Approval {
        bake_tui_view::runtime::Approval {
            generation,
            tool: "write".into(),
            call_id: call_id.into(),
            parent_call_id: None,
            reason: "escalate sandbox to workspace-write: create notes.txt".into(),
            arguments: "path: notes.txt\ncontent: hello".into(),
        }
    }

    fn approvals(list: Vec<bake_tui_view::runtime::Approval>) -> Source {
        runtime(RuntimeUpdate::Approvals(list))
    }

    fn read(key: Key) -> Source {
        Source::Msg(Msg::KeyRead {
            key: KeyInput::plain(key),
            burst: false,
        })
    }

    fn armed(generation: u64) -> Source {
        Source::Msg(Msg::ApprovalArmed(generation))
    }

    /// Runs the loop to its quit; returns the requests and what the loop
    /// published for the input thread to arm.
    fn answers(
        screen: &mut Rendered,
        inputs: &Receiver<Source>,
        state: &mut State,
    ) -> (Vec<String>, u64) {
        let requests = std::cell::RefCell::new(Vec::new());
        let shown = AtomicU64::new(0);
        let exit = run_loop(
            screen,
            state,
            inputs,
            Instant::now(),
            Owners {
                shown: &shown,
                ..owners(&|request| requests.borrow_mut().push(format!("{request:?}")))
            },
        );
        assert_eq!(exit.unwrap(), PreviewExit::Quit);
        (requests.into_inner(), shown.into_inner())
    }

    fn live_state() -> State {
        State::live(Default::default(), Tones::None, Vec::new())
    }

    fn field(id: u64, secret: bool) -> Source {
        use bake_tui_view::dialog::{DialogKind, DialogRequest, FieldDialog};
        runtime(RuntimeUpdate::Dialog(Some(DialogRequest {
            id,
            kind: DialogKind::Field(FieldDialog {
                title: "Sign in · CLIProxyAPI".into(),
                prompt: if secret { "API key" } else { "Base URL" }.into(),
                secret,
                initial: String::new(),
                error: None,
            }),
        })))
    }

    fn typed_keys(text: &str) -> Vec<Source> {
        text.chars().map(|c| read(Key::Char(c))).collect()
    }

    /// The sources of `parts`, in order.
    fn batch(parts: Vec<Vec<Source>>) -> Vec<Source> {
        parts.into_iter().flatten().collect()
    }

    // Bake: keys queued behind each dialog change reach the loop ahead of
    // that change's arming and go nowhere: Enter typed ahead of the key
    // field does not answer it, the end of an unbracketed paste after the
    // key does not reach the closed dialog's composer, and nothing of the
    // key is ever drawn.
    #[test]
    fn keys_queued_across_dialog_changes_reach_neither_the_next_field_nor_the_draft() {
        let mut batches: Vec<Vec<Source>> = vec![
            // URL field opens; typeahead behind it.
            batch(vec![
                vec![field(1, false)],
                typed_keys("early"),
                vec![read(Key::Enter)],
            ]),
            // Frame drew it: swept keys, then the arming, then the URL.
            batch(vec![
                vec![armed(1)],
                typed_keys("http://x"),
                vec![read(Key::Enter)],
            ]),
            // Key field replaces it, with the URL step's held Enter queued.
            vec![field(2, true), read(Key::Enter), read(Key::Enter)],
            batch(vec![
                vec![armed(2)],
                typed_keys("sk-fake-queued"),
                vec![read(Key::Enter)],
            ]),
            // Closed, and the rest of an unbracketed paste queued behind it.
            batch(vec![
                vec![runtime(RuntimeUpdate::Dialog(None))],
                typed_keys("tail"),
                vec![read(Key::Enter)],
            ]),
            // The composer's arming; only now does typing reach the draft.
            batch(vec![vec![armed(3)], typed_keys("ok")]),
            vec![ctrl('c'), ctrl('c')],
        ];
        let (mut screen, inputs) = Rendered::new(std::mem::take(&mut batches));
        let mut state = live_state();
        let (requests, shown) = answers(&mut screen, &inputs, &mut state);
        assert_eq!(shown, 3);
        assert_eq!(requests.len(), 2, "{requests:?}");
        assert!(
            requests[0].contains("Entered(SecretText(<8 chars>))"),
            "{requests:?}"
        );
        assert!(
            requests[1].contains("Entered(SecretText(<14 chars>))"),
            "{requests:?}"
        );
        assert!(requests.iter().all(|r| !r.contains("sk-fake")));
        assert_eq!(state.draft.text(), "ok");
        assert!(screen.screens.iter().all(|s| !s.contains("sk-fake")));
        assert!(screen.screens.iter().any(|s| s.contains("Preparing")));
    }

    #[test]
    fn keys_queued_before_the_arming_answer_nothing_and_the_draft_stays() {
        let (mut screen, inputs) = Rendered::new(vec![
            // Typing for the draft, then the request arrives with typed-ahead
            // keys queued behind it in the same batch.
            vec![
                read(Key::Char('k')),
                approvals(vec![approval(7, "call-7")]),
                read(Key::Char('y')),
                read(Key::Enter),
                Source::Msg(Msg::Paste("y".into())),
            ],
            // Frame 2 drew the request; keys the input thread swept before
            // arming it reach the loop only now, ahead of the arming.
            vec![read(Key::Char('y')), read(Key::Enter), armed(1)],
            vec![read(Key::Char('y'))],
            vec![read(Key::Enter), read(Key::Enter)],
            vec![ctrl('c'), ctrl('c')],
        ]);
        let mut state = live_state();
        let (requests, shown) = answers(&mut screen, &inputs, &mut state);
        assert_eq!(
            requests,
            ["Answer { generation: 7, answer: AllowOnce }"],
            "only y then Enter after the arming answered it, once"
        );
        assert_eq!(shown, 1, "the loop published the request's showing");
        let drawn = &screen.screens[1];
        assert!(
            drawn.contains("Approval required: write  call-7"),
            "{drawn}"
        );
        assert!(drawn.contains("escalate sandbox to workspace-write: create notes.txt"));
        assert!(drawn.contains("path: notes.txt") && drawn.contains("content: hello"));
        assert!(
            drawn.contains("Preparing approval… · Esc cancels"),
            "{drawn}"
        );
        let armed = &screen.screens[2];
        assert!(
            armed.contains("Y then Enter allows once · N rejects · Esc cancels"),
            "{armed}"
        );
        assert!(screen.screens[3].contains("Allow this call once?"));
        // The draft and its caret stayed as typed before the prompt.
        assert_eq!((state.draft.text(), state.draft.caret()), ("k", 1));
    }

    #[test]
    fn the_answer_to_one_prompt_cannot_answer_the_next() {
        let (mut screen, inputs) = Rendered::new(vec![
            vec![approvals(vec![approval(1, "a"), approval(2, "b")])],
            vec![armed(1), read(Key::Char('y'))],
            vec![read(Key::Enter)],
            // The runtime settles 1; the same keys again, before 2's arming,
            // and its arming for the old request, decide nothing.
            vec![
                approvals(vec![approval(2, "b")]),
                read(Key::Char('y')),
                read(Key::Enter),
                armed(1),
                read(Key::Char('y')),
                read(Key::Enter),
            ],
            vec![armed(2), read(Key::Char('n')), read(Key::Esc)],
            vec![approvals(Vec::new())],
            vec![ctrl('c'), ctrl('c')],
        ]);
        let (requests, shown) = answers(&mut screen, &inputs, &mut live_state());
        assert_eq!(
            requests,
            [
                "Answer { generation: 1, answer: AllowOnce }",
                "Answer { generation: 2, answer: Reject }",
            ]
        );
        assert_eq!(shown, 2);
        assert!(screen.screens[1].contains("1/2"), "{}", screen.screens[1]);
        assert!(
            screen.screens.last().unwrap().contains("Type a draft"),
            "the composer came back"
        );
    }

    #[test]
    fn a_shrink_disarms_a_chosen_prompt_and_only_a_fresh_arming_after_it_grows_allows() {
        let resize = |cols, rows| Source::Msg(Msg::Resize { cols, rows });
        let (mut screen, inputs) = Rendered::new(vec![
            vec![approvals(vec![approval(7, "call-7")])],
            // Frame 2 showed it (showing 1); armed, `y` chooses.
            vec![armed(1), read(Key::Char('y'))],
            // The terminal shrinks; frame 4 cannot show the request.
            vec![resize(80, 3)],
            // Enter, a stale arming, and `y` then Enter allow nothing.
            vec![
                read(Key::Enter),
                armed(1),
                read(Key::Char('y')),
                read(Key::Enter),
            ],
            // It grows back; frame 6 shows the request again (showing 2).
            vec![resize(80, 24)],
            vec![armed(1), read(Key::Char('y')), read(Key::Enter)],
            vec![armed(2), read(Key::Char('y'))],
            vec![read(Key::Enter)],
            vec![ctrl('c'), ctrl('c')],
        ]);
        screen.sizes = vec![(3, 80, 3), (5, 80, 24)];
        let (requests, shown) = answers(&mut screen, &inputs, &mut live_state());
        assert_eq!(requests, ["Answer { generation: 7, answer: AllowOnce }"]);
        assert_eq!(shown, 2, "growing back published a new showing");
        assert!(
            screen.screens[2].contains(CONFIRM_LINE),
            "{}",
            screen.screens[2]
        );
        // Shrunk: the title alone, without its detail or keys.
        assert!(
            !screen.screens[3].contains("Y then Enter"),
            "{}",
            screen.screens[3]
        );
        assert!(
            !screen.screens[5].contains(CONFIRM_LINE),
            "{}",
            screen.screens[5]
        );
    }

    const CONFIRM_LINE: &str = "Allow this call once?";

    #[test]
    fn esc_withdraws_a_prompt_even_before_it_is_armed() {
        let (mut screen, inputs) = Rendered::new(vec![
            vec![approvals(vec![approval(3, "c")]), read(Key::Esc)],
            vec![ctrl('c'), ctrl('c')],
        ]);
        let (requests, _) = answers(&mut screen, &inputs, &mut live_state());
        assert_eq!(requests, ["Answer { generation: 3, answer: Cancel }"]);
    }

    #[test]
    fn a_prompt_too_short_to_show_its_detail_is_never_published() {
        let (mut screen, inputs) = Rendered::new(vec![
            vec![approvals(vec![approval(4, "d")])],
            vec![ctrl('c'), ctrl('c')],
        ]);
        screen.terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 3)).unwrap();
        let (requests, shown) = answers(&mut screen, &inputs, &mut live_state());
        assert!(requests.is_empty(), "{requests:?}");
        assert_eq!(shown, 0, "a title and keys alone are not armed");
    }

    /// Terminal input as the input thread meets it: events already waiting
    /// in the terminal, and a hook that runs at the thread's first read, as
    /// the loop may draw at any point while it reads.
    struct Typed {
        waiting: std::collections::VecDeque<event::Event>,
        on_read: Option<Box<dyn FnMut()>>,
        stop: Arc<AtomicBool>,
        dry: u8,
    }

    impl Events for Typed {
        /// Nothing more is typed: the second time input is found dry, the
        /// thread is told to stop once it gets back to its loop.
        fn poll(&mut self, _wait: Duration) -> io::Result<bool> {
            if self.waiting.is_empty() {
                self.dry += 1;
                if self.dry == 2 {
                    self.stop.store(true, Ordering::SeqCst);
                }
            }
            Ok(!self.waiting.is_empty())
        }

        fn read(&mut self) -> io::Result<event::Event> {
            if let Some(mut hook) = self.on_read.take() {
                hook();
            }
            Ok(self.waiting.pop_front().expect("polled"))
        }
    }

    fn typed(c: char) -> event::Event {
        event::Event::Key(event::KeyEvent::new(
            event::KeyCode::Char(c),
            event::KeyModifiers::NONE,
        ))
    }

    /// What the input thread sent: keys as `c` or `c*` for a burst, an
    /// arming as `armed N`.
    fn sent(inputs: &Receiver<Source>) -> Vec<String> {
        inputs
            .try_iter()
            .filter_map(|source| match source {
                Source::Msg(Msg::KeyRead { key, burst }) => {
                    let Key::Char(c) = key.key else { return None };
                    Some(format!("{c}{}", if burst { "*" } else { "" }))
                }
                Source::Msg(Msg::ApprovalArmed(generation)) => Some(format!("armed {generation}")),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_key_waiting_in_the_terminal_when_the_prompt_is_drawn_goes_ahead_of_its_arming() {
        // The reader is late: `y` was typed before the prompt appeared and
        // is still in the terminal when the loop publishes the request.
        let stop = Arc::new(AtomicBool::new(false));
        let shown = AtomicU64::new(5);
        let (sender, inputs) = mpsc::channel();
        let mut events = Typed {
            waiting: [typed('y')].into(),
            on_read: None,
            stop: Arc::clone(&stop),
            dry: 0,
        };
        read_input(Stopped(sender), &stop, Instant::now(), &shown, &mut events);
        assert_eq!(sent(&inputs), ["y", "armed 5"]);
    }

    #[test]
    fn a_prompt_drawn_while_the_reader_sweeps_is_armed_only_after_that_sweep() {
        // The reader found `y` waiting and is reading it when the loop draws
        // the prompt and publishes it; `n` arrives after the arming's sweep.
        let stop = Arc::new(AtomicBool::new(false));
        let shown = Arc::new(AtomicU64::new(0));
        let published = Arc::clone(&shown);
        let (sender, inputs) = mpsc::channel();
        let mut events = Typed {
            waiting: [typed('y'), typed('e')].into(),
            on_read: Some(Box::new(move || published.store(6, Ordering::SeqCst))),
            stop: Arc::clone(&stop),
            dry: 0,
        };
        read_input(Stopped(sender), &stop, Instant::now(), &shown, &mut events);
        // The sweep in progress is sent before the arming, as a burst.
        assert_eq!(sent(&inputs), ["y*", "e*", "armed 6"]);
    }

    /// Input that never runs dry: focus events, which decode to nothing,
    /// without end. The thread is told to stop after `stop_after` reads.
    struct Endless {
        reads: usize,
        stop_after: usize,
        stop: Arc<AtomicBool>,
    }

    impl Events for Endless {
        fn poll(&mut self, _wait: Duration) -> io::Result<bool> {
            Ok(true)
        }

        fn read(&mut self) -> io::Result<event::Event> {
            self.reads += 1;
            if self.reads == self.stop_after {
                self.stop.store(true, Ordering::SeqCst);
            }
            Ok(event::Event::FocusGained)
        }
    }

    #[test]
    fn a_sweep_counts_events_read_even_when_they_decode_to_nothing() {
        let stop = Arc::new(AtomicBool::new(false));
        let mut events = Endless {
            reads: 0,
            stop_after: usize::MAX,
            stop,
        };
        let (swept, dry) = sweep(&mut events, &mut input::Decoder::new(), Instant::now()).unwrap();
        assert!(swept.is_empty());
        assert!(!dry);
        assert_eq!(events.reads, SWEEP);
    }

    #[test]
    fn input_that_never_runs_dry_does_not_hold_up_a_stop_while_arming() {
        let stop = Arc::new(AtomicBool::new(false));
        let shown = AtomicU64::new(1);
        let (sender, inputs) = mpsc::channel();
        let mut events = Endless {
            reads: 0,
            stop_after: SWEEP + 3,
            stop: Arc::clone(&stop),
        };
        read_input(Stopped(sender), &stop, Instant::now(), &shown, &mut events);
        assert!(events.reads < 3 * SWEEP, "{}", events.reads);
        assert!(sent(&inputs).is_empty(), "never armed");
    }

    #[test]
    fn a_sweep_with_more_than_one_key_is_a_burst() {
        let key = |k| Msg::Key(KeyInput::plain(k));
        let burst = |msgs: Vec<Msg>| -> Vec<bool> {
            msgs.into_iter()
                .filter_map(|msg| match msg {
                    Msg::KeyRead { burst, .. } => Some(burst),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(burst(stamp(vec![key(Key::Char('y'))])), [false]);
        let paste = Msg::Paste("yes".into());
        assert_eq!(burst(stamp(vec![paste, key(Key::Enter)])), [false]);
        assert_eq!(
            burst(stamp(vec![key(Key::Char('y')), key(Key::Enter)])),
            [true, true]
        );
    }
}
