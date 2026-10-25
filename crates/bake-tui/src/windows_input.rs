//! Windows console input read as VT sequences.
//!
//! Crossterm 0.29 reads a Windows console's key records and implements no
//! bracketed paste there, so a paste reaches it as typed keys: a pasted
//! line break is Enter, which submits, and a pasted path is typed, not
//! staged. With `ENABLE_VIRTUAL_TERMINAL_INPUT` the console passes its input
//! through as VT sequences, a paste's `ESC[200~`/`ESC[201~` markers
//! included, carried one UTF-16 unit per key-down record, or, for text no
//! key produces, on an Alt code's release. [`VtConsole`]
//! reads those records and decodes them with crossterm's own Unix parser
//! ([`crate::vt_parse`]), so input decodes on Windows as it does on Unix.
//!
//! [`enable`] sets the mode after raw mode is on and [`disable`] clears it
//! before raw mode is left, so the console's input mode is restored
//! exactly; a console that refuses the mode keeps crossterm's key records.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::Event;
use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    ENABLE_VIRTUAL_TERMINAL_INPUT, GetConsoleMode, GetNumberOfConsoleInputEvents, INPUT_RECORD,
    KEY_EVENT, ReadConsoleInputW, SetConsoleMode, WINDOW_BUFFER_SIZE_EVENT,
};
use windows_sys::Win32::System::Threading::WaitForSingleObject;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_MENU;

use crate::vt_parse::Parser;

/// Whether [`enable`] set the mode, so [`disable`] clears only what it set.
static SET: AtomicBool = AtomicBool::new(false);

/// Records read at most per read.
const BATCH: u32 = 512;

/// The console input buffer, `CONIN$`, as crossterm opens it; closed on drop.
struct Conin(HANDLE);

impl Conin {
    fn open() -> io::Result<Self> {
        let name: Vec<u16> = "CONIN$\0".encode_utf16().collect();
        // SAFETY: `name` is NUL-terminated and outlives the call; the other
        // arguments are plain values.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(handle))
    }

    fn mode(&self) -> io::Result<u32> {
        let mut mode = 0;
        // SAFETY: the handle is open and `mode` is a valid out pointer.
        if unsafe { GetConsoleMode(self.0, &mut mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(mode)
    }

    fn set_mode(&self, mode: u32) -> io::Result<()> {
        // SAFETY: the handle is open.
        if unsafe { SetConsoleMode(self.0, mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn waiting(&self) -> io::Result<u32> {
        let mut count = 0;
        // SAFETY: the handle is open and `count` is a valid out pointer.
        if unsafe { GetNumberOfConsoleInputEvents(self.0, &mut count) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(count)
    }
}

impl Drop for Conin {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by `open` and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// Turns VT input on, after raw mode. Returns whether input is now VT; a
/// console that was already in the mode is left as it was.
pub(crate) fn enable() -> bool {
    let Ok(conin) = Conin::open() else {
        return false;
    };
    let Ok(mode) = conin.mode() else {
        return false;
    };
    if mode & ENABLE_VIRTUAL_TERMINAL_INPUT != 0 {
        return true;
    }
    if conin
        .set_mode(mode | ENABLE_VIRTUAL_TERMINAL_INPUT)
        .is_err()
    {
        return false;
    }
    SET.store(true, Ordering::SeqCst);
    true
}

/// Turns VT input off again if [`enable`] turned it on, before raw mode
/// is left.
pub(crate) fn disable() -> io::Result<()> {
    if !SET.swap(false, Ordering::SeqCst) {
        return Ok(());
    }
    let conin = Conin::open()?;
    let mode = conin.mode()?;
    conin.set_mode(mode & !ENABLE_VIRTUAL_TERMINAL_INPUT)
}

/// Whether [`enable`] left the console's input as VT sequences.
pub(crate) fn active() -> bool {
    let Ok(conin) = Conin::open() else {
        return false;
    };
    conin
        .mode()
        .is_ok_and(|mode| mode & ENABLE_VIRTUAL_TERMINAL_INPUT != 0)
}

/// The console's VT input, decoded into crossterm events.
pub(crate) struct VtConsole {
    conin: Conin,
    parser: Parser,
    /// UTF-16 units read and not yet parsed: a high surrogate whose low
    /// half has not arrived stays here.
    units: Vec<u16>,
    queue: VecDeque<Event>,
}

impl VtConsole {
    pub(crate) fn open() -> io::Result<Self> {
        Ok(Self {
            conin: Conin::open()?,
            parser: Parser::default(),
            units: Vec::new(),
            queue: VecDeque::new(),
        })
    }

    /// Whether an event is ready within `wait`.
    pub(crate) fn poll(&mut self, wait: Duration) -> io::Result<bool> {
        let deadline = Instant::now() + wait;
        loop {
            if !self.queue.is_empty() {
                return Ok(true);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if !self.wait(left)? {
                return Ok(false);
            }
            self.fill()?;
            // Records that decode to nothing, such as key releases, end a
            // wait that has run out.
            if self.queue.is_empty() && Instant::now() >= deadline {
                return Ok(false);
            }
        }
    }

    /// The next event, waiting for one.
    pub(crate) fn read(&mut self) -> io::Result<Event> {
        loop {
            if let Some(event) = self.queue.pop_front() {
                return Ok(event);
            }
            self.wait(Duration::MAX)?;
            self.fill()?;
        }
    }

    /// Waits up to `wait` for the input buffer to hold a record.
    fn wait(&self, wait: Duration) -> io::Result<bool> {
        let millis = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: the handle is open.
        match unsafe { WaitForSingleObject(self.conin.0, millis) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(io::Error::last_os_error()),
        }
    }

    /// Reads the waiting records and decodes what they carry.
    fn fill(&mut self) -> io::Result<()> {
        let waiting = self.conin.waiting()?.min(BATCH);
        if waiting == 0 {
            return Ok(());
        }
        // SAFETY: `INPUT_RECORD` is plain data, valid when zeroed.
        let mut records: Vec<INPUT_RECORD> = vec![unsafe { std::mem::zeroed() }; waiting as usize];
        let mut read = 0;
        // SAFETY: the buffer holds `waiting` records and `read` is a valid
        // out pointer.
        if unsafe { ReadConsoleInputW(self.conin.0, records.as_mut_ptr(), waiting, &mut read) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        for record in records.iter().take(read as usize) {
            match u32::from(record.EventType) {
                KEY_EVENT => {
                    // SAFETY: the record is a key event.
                    let key = unsafe { record.Event.KeyEvent };
                    // SAFETY: both union fields are the same `u16`.
                    let unit = unsafe { key.uChar.UnicodeChar };
                    // Text no key produces, such as an emoji's surrogate
                    // halves or a combining mark, arrives even in VT mode
                    // as an Alt code: its unit rides on the Alt release.
                    let alt_code = key.bKeyDown == 0 && key.wVirtualKeyCode == VK_MENU;
                    if (key.bKeyDown != 0 || alt_code) && unit != 0 {
                        for _ in 0..key.wRepeatCount.max(1) {
                            self.units.push(unit);
                        }
                    }
                }
                WINDOW_BUFFER_SIZE_EVENT => {
                    self.flush(false);
                    let (cols, rows) = crossterm::terminal::size()?;
                    self.queue.push_back(Event::Resize(cols, rows));
                }
                _ => {}
            }
        }
        let more = self.conin.waiting()? > 0;
        self.flush(more);
        Ok(())
    }

    /// Parses the units read so far; `more` when further input is waiting.
    fn flush(&mut self, more: bool) {
        // A high surrogate at the end waits for its low half.
        let kept = self.units.pop_if(|unit| is_high(*unit));
        let text: String = char::decode_utf16(self.units.drain(..))
            .map(|decoded| decoded.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
        self.units.extend(kept);
        if text.is_empty() {
            return;
        }
        self.parser.advance(text.as_bytes(), more);
        while let Some(event) = self.parser.next_event() {
            self.queue.push_back(event);
        }
    }
}

fn is_high(unit: u16) -> bool {
    (0xD800..=0xDBFF).contains(&unit)
}
