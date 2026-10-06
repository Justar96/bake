//! The terminal owner and the preview's event loop.
//!
//! [`TerminalSession`] is the only code that changes terminal modes. Each mode
//! is recorded before it is requested, so a partial setup failure, an error,
//! a panic, or a handled SIGINT/SIGTERM/SIGHUP restores exactly what was
//! changed.

use std::io::{self, Stdout};
use std::sync::Once;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use crossterm::cursor::Show;
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event};
use crossterm::execute;
use crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;

use crate::app::App;
use crate::render::render;

const OWNED: u8 = 1;
const RAW: u8 = 1 << 1;
const ALTERNATE: u8 = 1 << 2;
const PASTE: u8 = 1 << 3;

/// Modes the live session changed. Process-wide so the panic hook can restore
/// them before the panic message is printed.
static MODES: AtomicU8 = AtomicU8::new(0);

/// Bounds how long a pending signal waits for the loop to notice it.
const POLL: Duration = Duration::from_millis(250);

/// How the preview ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewExit {
    Quit,
    /// A handled termination signal, by number.
    Signal(i32),
}

/// Owns raw mode, the alternate screen, bracketed paste, and cursor
/// visibility. [`TerminalSession::close`] restores them and reports failures;
/// dropping it restores them silently if `close` did not run.
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

    /// Clears the screen and forgets the last frame, so the next draw repaints
    /// every cell. A shrink and grow between draws can end at the size of the
    /// last frame while the terminal has reflowed what it showed; Ratatui's own
    /// size check would then skip the clear and leave stale rows.
    fn invalidate(&mut self) -> io::Result<()> {
        let size = self.terminal.size()?;
        self.terminal.resize(Rect::from(size))
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
/// The terminal is restored before this returns. A loop error takes priority
/// over a restoration error; otherwise a restoration error is returned.
pub fn run_preview() -> io::Result<PreviewExit> {
    // Declared first so it is dropped last: signals stay handled until the
    // terminal is restored.
    let signals = signals::Watch::register()?;
    let mut session = TerminalSession::acquire()?;
    let outcome = run_loop(&mut session, &signals);
    let closed = session.close();
    let exit = outcome?;
    closed?;
    Ok(exit)
}

fn run_loop(session: &mut TerminalSession, signals: &signals::Watch) -> io::Result<PreviewExit> {
    let mut app = App::default();
    let mut dirty = true;
    loop {
        if let Some(signal) = signals.received() {
            return Ok(PreviewExit::Signal(signal));
        }
        if dirty {
            session.terminal.draw(|frame| render(frame, &app))?;
            dirty = false;
        }
        if event::poll(POLL)? {
            let event = event::read()?;
            if matches!(event, Event::Resize(..)) {
                session.invalidate()?;
            }
            dirty = app.handle_event(event);
            if app.should_quit() {
                return Ok(PreviewExit::Quit);
            }
        }
    }
}

#[cfg(unix)]
mod signals {
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use signal_hook::SigId;
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};

    /// Records SIGINT, SIGTERM, and SIGHUP instead of terminating, so the loop
    /// can restore the terminal. Raw mode turns the Ctrl-C key into input, so
    /// SIGINT arrives only from another process.
    ///
    /// Dropping it removes only these registrations. signal-hook keeps its
    /// process handler installed and does not restore the previous
    /// disposition, so the same signals are ignored from then until exit.
    pub struct Watch {
        received: Arc<AtomicUsize>,
        ids: Vec<SigId>,
    }

    impl Watch {
        pub fn register() -> io::Result<Self> {
            let mut watch = Self {
                received: Arc::new(AtomicUsize::new(0)),
                ids: Vec::new(),
            };
            for signal in [SIGINT, SIGTERM, SIGHUP] {
                let id = signal_hook::flag::register_usize(
                    signal,
                    Arc::clone(&watch.received),
                    signal as usize,
                )?;
                watch.ids.push(id);
            }
            Ok(watch)
        }

        pub fn received(&self) -> Option<i32> {
            match self.received.load(Ordering::SeqCst) {
                0 => None,
                signal => i32::try_from(signal).ok(),
            }
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            for id in self.ids.drain(..) {
                signal_hook::low_level::unregister(id);
            }
        }
    }
}

#[cfg(not(unix))]
mod signals {
    use std::io;

    pub struct Watch;

    impl Watch {
        pub fn register() -> io::Result<Self> {
            Ok(Self)
        }

        pub fn received(&self) -> Option<i32> {
            None
        }
    }
}
