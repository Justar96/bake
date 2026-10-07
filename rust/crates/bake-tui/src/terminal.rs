//! The terminal owner and the preview's event loop.
//!
//! [`TerminalSession`] is the only code that changes terminal modes. Each mode
//! is recorded before it is requested, so a partial setup failure, an error,
//! a panic, or a handled SIGINT/SIGTERM/SIGHUP restores exactly what was
//! changed.
//!
//! The loop owns the view's state and the terminal. An input thread, the
//! runtime port's thread, and, on Unix, a signal thread only send it
//! messages over one channel; the loop waits for the next message or the
//! view's next timed change, applies every waiting message, and draws once.

use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
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
use crossterm::cursor::Show;
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
use crate::input;
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

/// How long the input thread waits for input before it checks whether to
/// stop. It bounds how long shutdown waits to join the thread; input itself
/// is read as soon as it arrives.
const READER_WAKE: Duration = Duration::from_millis(50);
/// Messages applied before a frame is drawn, so a long burst still draws.
const MAX_BATCH: usize = 256;
/// The shortest gap between frames that only runtime updates prompted. A
/// batch with input is drawn at once.
const RUNTIME_FRAME: Duration = Duration::from_millis(16);

/// How the preview ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewExit {
    Quit,
    /// A handled termination signal, by number.
    Signal(i32),
}

/// What the loop receives from the threads that feed it.
enum Source {
    Msg(Msg),
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

/// Owns raw mode, the alternate screen, bracketed paste, autowrap, cursor
/// visibility, and synchronized output. [`TerminalSession::close`] restores
/// them and reports failures; dropping it restores them silently if `close`
/// did not run.
pub struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalSession {
    pub fn acquire() -> io::Result<Self> {
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
            let mut out = io::stdout();
            MODES.fetch_or(ALTERNATE, Ordering::SeqCst);
            // Cleared directly: `Terminal::clear` queries the cursor position,
            // which waits on a reply that some terminals never send.
            execute!(out, EnterAlternateScreen, Clear(ClearType::All))?;
            MODES.fetch_or(PASTE, Ordering::SeqCst);
            execute!(out, EnableBracketedPaste)?;
            MODES.fetch_or(MOUSE, Ordering::SeqCst);
            execute!(out, Print(MOUSE_ON))?;
            // A row the terminal draws wider than measured is clipped at the
            // right edge instead of wrapping onto the row below.
            MODES.fetch_or(NO_WRAP, Ordering::SeqCst);
            execute!(out, DisableLineWrap)?;
            Terminal::new(CrosstermBackend::new(out))
        };
        match setup() {
            Ok(terminal) => Ok(Self { terminal }),
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
    let (sender, inputs) = mpsc::channel();
    // Declared first so it is dropped last: signals stay handled until the
    // terminal is restored.
    let signals = signals::Forwarder::start(sender.clone())?;
    let mut session = TerminalSession::acquire()?;
    let env = |name: &str| std::env::var(name).ok();
    let mut state = State::new(frame::resolve(env, cfg!(windows)), Tones::resolve(env));
    state.status.cwd = working_directory(env);
    state.wheel = WheelSteps::new(wheel::reports(env, cfg!(target_os = "macos")));
    state.status.branch = std::env::current_dir()
        .ok()
        .and_then(|cwd| git::branch(&cwd));
    // One clock for the loop's ticks and the reader's timestamps.
    let clock = Instant::now();
    // Started only once raw mode is on, so it never reads cooked input.
    let updates = sender.clone();
    let outcome = Reader::start(sender, clock).and_then(|reader| {
        let clipboard = Clipboard::new(Platform::current(), env);
        let home = env(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
        let found = updates.clone();
        let port = FixturePort::start(clock, move |update| {
            updates.send(Source::Msg(Msg::Runtime(update))).is_ok()
        })?;
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
            },
        );
        // Joined before the terminal leaves raw mode, so it reads nothing
        // meant for the shell, and no runtime update or found path outlives
        // the loop.
        let ported = port.stop();
        let searched = finder.stop();
        let stopped = reader.stop();
        outcome.and_then(|exit| ported.and(searched).and(stopped).map(|()| exit))
    });
    let closed = session.close();
    drop(signals);
    let exit = outcome?;
    closed?;
    Ok(exit)
}

/// What the loop hands effects to: the clipboard and image jobs, the
/// runtime port, and path discovery.
struct Owners<'a> {
    clipboard: Clipboard,
    home: Option<String>,
    runtime: &'a dyn Fn(Request),
    files: &'a dyn Fn(Ask),
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
    fn start(sender: Sender<Source>, clock: Instant) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("bake-tui-input".into())
            .spawn(move || read_input(Stopped(sender), &flag, clock))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    /// Stops the thread and waits for it; reports a panic in it.
    fn stop(mut self) -> io::Result<()> {
        self.join()
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

/// Tells the loop that input has ended however the input thread exits,
/// including by a panic, so the loop never waits on a reader that is gone.
struct Stopped(Sender<Source>);

impl Drop for Stopped {
    fn drop(&mut self) {
        let _ = self.0.send(Source::Failed(input_stopped()));
    }
}

fn read_input(out: Stopped, stop: &AtomicBool, clock: Instant) {
    let fail = |err| {
        let _ = out.0.send(Source::Failed(err));
    };
    while !stop.load(Ordering::SeqCst) {
        match event::poll(READER_WAKE) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(err) => return fail(err),
        }
        match event::read() {
            Ok(event) => {
                if let Some(msg) = input::decode(event, clock.elapsed())
                    && out.0.send(Source::Msg(msg)).is_err()
                {
                    return;
                }
            }
            Err(err) => return fail(err),
        }
    }
}

#[cfg(unix)]
mod signals {
    use std::io;
    use std::sync::mpsc::Sender;
    use std::thread::{self, JoinHandle};

    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::{Handle, Signals};

    use super::Source;

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
                        if sender.send(Source::Signal(signal)).is_err() {
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
            owners(&|request| requests.borrow_mut().push(format!("{request:?}"))),
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
}
