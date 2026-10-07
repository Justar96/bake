//! Presentation state and its update function. Every input arrives as a
//! [`Msg`]; [`update`] applies it and returns the [`Effect`]s the terminal
//! owner must perform. Nothing here touches the terminal or reads a clock.

use std::time::Duration;

use crate::activity::{self, Tones};
use crate::composer::ComposerWindow;
use crate::editor::Draft;
use crate::frame::FrameStyle;
use crate::keys::{self, Action, KeyInput, Scope};
use crate::live;
use crate::mode::Mode;
use crate::selection::{self, Granularity, Point, Range};
use crate::status::StatusInput;
use crate::transcript::{self, Transcript};
use crate::wheel::{self, WheelSteps};

/// Everything the frontend applies, in arrival order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Msg {
    Key(KeyInput),
    /// A bracketed paste, inserted as text without submitting.
    Paste(String),
    /// The terminal's new size. Rendering reads the size from the frame, so
    /// this only marks that a repaint is due.
    Resize {
        cols: u16,
        rows: u16,
    },
    /// The current time on the terminal owner's monotonic clock, measured
    /// from when it started.
    Tick(Duration),
    Mouse(Mouse),
    /// Whether the text of the last [`Effect::Copy`] reached a clipboard.
    Copied(bool),
}

/// A mouse report, at a cell of the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mouse {
    pub kind: MouseKind,
    pub column: u16,
    pub row: u16,
    /// Alt held: the wheel moves [`wheel::ALT_FACTOR`] times as far.
    pub alt: bool,
    /// When the report arrived, on the same clock as [`Msg::Tick`], so the
    /// wheel's acceleration sees the real gaps between reports in a batch.
    pub at: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    /// The wheel toward older output.
    WheelUp,
    /// The wheel toward newer output.
    WheelDown,
    /// The primary button pressed, moved while held, and released.
    Down,
    Drag,
    Up,
}

/// Where the transcript's scrollbar was last drawn: its column and the rows
/// of its track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollTrack {
    pub column: u16,
    pub top: u16,
    pub rows: u16,
}

/// Where the draft's rows were last drawn, so a press there places the
/// caret: the screen rows, the column its text starts at, and the window's
/// first draft row and wrap width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DraftSpot {
    pub top_row: u16,
    pub rows: u16,
    pub text_x: u16,
    pub first: usize,
    pub width: usize,
}

/// Where the transcript's lines were last drawn: the cell of the first
/// line's first column, and the columns and lines the text takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Area {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub rows: u16,
}

impl Area {
    fn holds(self, column: u16, row: u16) -> bool {
        (self.x..self.x + self.width).contains(&column)
            && (self.y..self.y + self.rows).contains(&row)
    }
}

/// How often a drag held at the transcript's edge scrolls a line on.
pub const EDGE_SCROLL: Duration = Duration::from_millis(50);

/// A press in progress: what it extends by, and the range a double or triple
/// click started with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Gesture {
    granularity: Granularity,
    initial: Option<Range>,
    dragged: bool,
}

/// The last press, for counting a double or triple click on the same word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Click {
    at: Duration,
    point: Point,
    word: (usize, usize),
    count: u8,
}

/// A drag held at the transcript's top or bottom: where the pointer is, the
/// way it scrolls, and when it next does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EdgeHold {
    column: usize,
    row: isize,
    direction: i8,
    next: Duration,
}

/// The transcript selection and the press that is making it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selecting {
    /// Where the selection began and where its free end is now.
    pub ends: Option<(Point, Point)>,
    gesture: Option<Gesture>,
    last_click: Option<Click>,
    edge: Option<EdgeHold>,
    /// Set when a press that selected is released, until it is copied.
    finished: bool,
}

impl Selecting {
    /// The selection, or `None` while nothing, or one cell, is selected.
    pub fn range(&self) -> Option<Range> {
        self.ends
            .and_then(|(anchor, focus)| selection::ordered(anchor, focus))
    }

    /// Drops the selection and the press making it. Returns whether there
    /// was one, so Esc does nothing else.
    fn clear(&mut self) -> bool {
        let had = self.ends.is_some() || self.gesture.is_some();
        *self = Self {
            last_click: self.last_click,
            ..Self::default()
        };
        had
    }
}

/// A run of cells on one row that a press acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spot {
    pub column: u16,
    pub row: u16,
    pub width: u16,
}

impl Spot {
    fn holds(self, column: u16, row: u16) -> bool {
        row == self.row && (self.column..self.column + self.width).contains(&column)
    }
}

/// How long a first Ctrl+C waits for a second before the quit is disarmed:
/// the TypeScript `doubleInterruptMs` default.
pub const QUIT_WINDOW: Duration = Duration::from_millis(2000);

/// How long the scroll indicator says whether a selection was copied.
pub const COPIED: Duration = Duration::from_millis(1500);

/// Requests the terminal owner performs on the view's behalf.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Quit,
    /// Put this text on the clipboard, then report with [`Msg::Copied`].
    Copy(String),
}

/// A fixed example row for the agent list; it describes no running work.
#[derive(Debug, PartialEq, Eq)]
pub struct SampleAgent {
    pub id: &'static str,
    pub name: &'static str,
    pub detail: &'static [&'static str],
}

pub const SAMPLE_AGENTS: &[SampleAgent] = &[
    SampleAgent {
        id: "sample-explorer",
        name: "Sample explorer",
        detail: &[
            "Example of a child that would read files for its parent.",
            "It is static text; no agent is running.",
        ],
    },
    SampleAgent {
        id: "sample-reviewer",
        name: "Sample reviewer",
        detail: &[
            "Example of a child that would review a change.",
            "It is static text; no agent is running.",
        ],
    },
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Focus {
    #[default]
    Composer,
    AgentList,
    /// Read-only inspection of the sample agent with this id.
    Inspect(&'static str),
}

impl Focus {
    fn scope(self) -> Scope {
        match self {
            Self::Composer => Scope::Composer,
            Self::AgentList => Scope::AgentList,
            Self::Inspect(_) => Scope::Inspect,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    NoModel,
    ReadOnly,
    ListKeys,
    DraftLimit,
}

/// What a sample activity stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleKind {
    /// A running turn.
    Turn,
    /// History being compacted.
    Compaction,
}

/// A sample of the header's activity line and the composer mode it puts the
/// session in. Nothing runs; it shows how a session at work reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleActivity {
    pub kind: SampleKind,
    /// The turn's word, kept until the sample stops; compaction names itself.
    pub word: &'static str,
    /// When the sample started, on the clock [`Msg::Tick`] carries.
    pub started: Duration,
}

/// How a sample turn ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// It ran to its end; Ctrl+T moved on to a compaction.
    Completed,
    /// Esc stopped it.
    Interrupted,
}

/// The header's line about the last sample turn, held until the next starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnSummary {
    pub outcome: Outcome,
    pub elapsed: Duration,
}

/// Presentation state: the parent draft, keyboard focus, one notice, the
/// sample activity, the composer window, and the terminal's capabilities.
/// Navigation never replaces the draft, so its caret and undo survive it.
#[derive(Debug)]
pub struct State {
    pub draft: Draft,
    pub focus: Focus,
    pub selected: &'static str,
    pub notice: Option<Notice>,
    pub activity: Option<SampleActivity>,
    /// How the last sample turn ended.
    pub summary: Option<TurnSummary>,
    /// What the status line reports. The terminal owner fills in the working
    /// directory and branch; the preview has no model, level, or context.
    pub status: StatusInput,
    /// The fixed sample session and the viewport over it. Rendering records
    /// the viewport's size; navigation keys move it.
    pub transcript: Transcript,
    /// Glyphs for the composer box, chosen once before the first frame.
    pub frame: FrameStyle,
    /// Colours the activity line may use; [`Tones::None`] also stops its shimmer.
    pub tones: Tones,
    /// The time of the last [`Msg::Tick`].
    pub now: Duration,
    /// The composer's first visible row. Only rendering moves it, because it
    /// depends on the frame's size.
    pub(crate) window: ComposerWindow,
    /// Samples started so far; seeds each sample's word.
    samples: u32,
    /// The transcript row of the sample turn's running script.
    live_call: Option<usize>,
    /// Rows per wheel report; the terminal owner sets how its terminal reports.
    pub wheel: WheelSteps,
    /// The scrollbar as last drawn, for clicks and drags; `None` when hidden.
    pub track: Option<ScrollTrack>,
    /// The scroll indicator's pill as last drawn; a press on it follows output.
    pub latest: Option<Spot>,
    /// The draft's rows as last drawn; a press on them places the caret.
    pub draft_spot: Option<DraftSpot>,
    /// The transcript's lines as last drawn; a press there selects.
    pub transcript_area: Option<Area>,
    /// The transcript selection.
    pub selecting: Selecting,
    /// The terminal's columns at the last resize; a change drops the
    /// selection, whose lines wrap anew.
    columns: u16,
    /// Whether the last selection was copied, said until the time given.
    pub copied: Option<(bool, Duration)>,
    /// Whether a press on the scrollbar is being dragged.
    pub(crate) dragging: bool,
    /// While a first Ctrl+C is armed, when it lapses; a second before then
    /// quits. The prompt to press again shows until then.
    pub quit_until: Option<Duration>,
}

impl Default for State {
    fn default() -> Self {
        Self::new(FrameStyle::default(), Tones::default())
    }
}

impl State {
    pub fn new(frame: FrameStyle, tones: Tones) -> Self {
        Self {
            draft: Draft::default(),
            focus: Focus::Composer,
            selected: SAMPLE_AGENTS[0].id,
            notice: None,
            activity: None,
            summary: None,
            status: StatusInput {
                ascii: frame == FrameStyle::Classic,
                ..StatusInput::default()
            },
            transcript: Transcript::new(transcript::sample_session()),
            frame,
            tones,
            now: Duration::ZERO,
            window: ComposerWindow::default(),
            samples: 0,
            live_call: None,
            wheel: WheelSteps::default(),
            track: None,
            latest: None,
            draft_spot: None,
            transcript_area: None,
            selecting: Selecting::default(),
            columns: 0,
            copied: None,
            dragging: false,
            quit_until: None,
        }
    }

    /// Whether a second Ctrl+C would quit now.
    pub fn quitting(&self) -> bool {
        self.quit_until.is_some_and(|until| self.now < until)
    }

    /// The composer's mode: inspection, else what the sample activity
    /// stands for, else idle.
    pub fn mode(&self) -> Mode {
        match (self.focus, self.activity) {
            (Focus::Inspect(_), _) => Mode::Inspecting,
            (_, Some(sample)) if sample.kind == SampleKind::Compaction => Mode::Compacting,
            (_, Some(_)) => Mode::Running,
            (_, None) => Mode::Idle,
        }
    }

    pub fn selected_agent(&self) -> &'static SampleAgent {
        agent(self.selected).unwrap_or(&SAMPLE_AGENTS[0])
    }

    /// How long after [`State::now`] the screen next changes without input:
    /// the sample's next shimmer beat, or its next second without motion.
    /// `None` while nothing moves.
    pub fn next_change(&self) -> Option<Duration> {
        let activity = self.activity.map(|sample| {
            activity::next_change(self.now.saturating_sub(sample.started), self.tones.moves())
        });
        let blink = self
            .transcript
            .running()
            .then(|| transcript::next_pulse(self.now));
        // The turn's script changes when its next call starts or settles.
        let script = self.live_elapsed().and_then(live::next);
        let quit = self.quit_until.map(|until| until.saturating_sub(self.now));
        let edge = self
            .selecting
            .edge
            .map(|edge| edge.next.saturating_sub(self.now));
        let copied = self.copied.map(|(_, until)| until.saturating_sub(self.now));
        activity
            .into_iter()
            .chain(blink)
            .chain(script)
            .chain(quit)
            .chain(edge)
            .chain(copied)
            .min()
    }

    /// Time since the sample turn started, while its script runs.
    fn live_elapsed(&self) -> Option<Duration> {
        let sample = self.activity.filter(|s| s.kind == SampleKind::Turn)?;
        self.live_call?;
        Some(self.now.saturating_sub(sample.started))
    }
}

/// Applies one message and returns the effects it requests.
pub fn update(state: &mut State, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Key(input) => return key(state, input),
        Msg::Paste(text) => match state.focus {
            // A long paste collapses into a placeholder, as the oracle's does.
            Focus::Composer => {
                let complete = state.draft.paste_block(&text);
                state.notice = (!complete).then_some(Notice::DraftLimit);
            }
            Focus::AgentList => state.notice = Some(Notice::ListKeys),
            Focus::Inspect(_) => state.notice = Some(Notice::ReadOnly),
        },
        Msg::Resize { cols, .. } => {
            // The first resize only learns the width.
            if state.columns != 0 && cols != state.columns {
                state.selecting.clear();
            }
            state.columns = cols;
        }
        Msg::Tick(now) => {
            state.now = now;
            if !state.quitting() {
                state.quit_until = None;
            }
            advance_script(state);
            edge_scroll(state);
            if state.copied.is_some_and(|(_, until)| now >= until) {
                state.copied = None;
            }
        }
        Msg::Mouse(mouse) => return pointer(state, mouse),
        Msg::Copied(ok) => state.copied = Some((ok, state.now + COPIED)),
    }
    Vec::new()
}

/// Applies a mouse report. The wheel scrolls what the arrows would: the
/// transcript from the composer, the selection in the agent list. A press on
/// the scrollbar's track moves the transcript there, and dragging follows
/// the pointer until release, wherever it goes.
fn pointer(state: &mut State, mouse: Mouse) -> Vec<Effect> {
    pointer_at(state, mouse);
    match mouse.kind {
        MouseKind::Up => copy_selection(state)
            .map(Effect::Copy)
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

/// The text a finished selection copies: each line as drawn, cut to the
/// cells it covers and trimmed at its end; `None` when nothing but spaces is
/// selected, or the press is still going.
fn copy_selection(state: &mut State) -> Option<String> {
    if !std::mem::take(&mut state.selecting.finished) {
        return None;
    }
    let range = state.selecting.range()?;
    let text = state
        .transcript
        .between(range.start.at, range.end.at)
        .into_iter()
        .map(|(at, line)| {
            selection::line_columns(range, at, &line).map_or(String::new(), |(from, to)| {
                selection::slice_cells(&line, from, to)
                    .trim_end()
                    .to_owned()
            })
        })
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

fn pointer_at(state: &mut State, mouse: Mouse) {
    let wheel = match mouse.kind {
        MouseKind::WheelUp => Some(-1),
        MouseKind::WheelDown => Some(1),
        _ => None,
    };
    if let Some(direction) = wheel {
        let factor = if mouse.alt { wheel::ALT_FACTOR } else { 1 };
        let rows = state.wheel.rows(direction, mouse.at) * factor;
        match state.focus {
            Focus::Composer if direction < 0 => state.transcript.scroll_up(rows),
            Focus::Composer => state.transcript.scroll_down(rows),
            Focus::AgentList => step_selection(state, isize::from(direction)),
            Focus::Inspect(_) => {}
        }
        return;
    }
    if mouse.kind == MouseKind::Down
        && state.focus == Focus::Composer
        && state
            .latest
            .is_some_and(|spot| spot.holds(mouse.column, mouse.row))
    {
        state.transcript.follow();
        return;
    }
    // A press on the draft's rows puts the caret at the cell it hit; one in
    // the rail before the text reaches the row's start.
    if mouse.kind == MouseKind::Down
        && state.focus == Focus::Composer
        && let Some(spot) = state
            .draft_spot
            .filter(|spot| (spot.top_row..spot.top_row + spot.rows).contains(&mouse.row))
    {
        let row = spot.first + usize::from(mouse.row - spot.top_row);
        let column = usize::from(mouse.column.saturating_sub(spot.text_x));
        if state.draft.place(row, column, spot.width) {
            return;
        }
    }
    if state.focus != Focus::Composer {
        state.dragging = false;
        return;
    }
    let on_track = state.track.is_some_and(|track| {
        mouse.column == track.column && (track.top..track.top + track.rows).contains(&mouse.row)
    });
    match mouse.kind {
        MouseKind::Down if on_track => state.dragging = true,
        MouseKind::Down => return press(state, mouse),
        MouseKind::Drag if state.dragging => {}
        MouseKind::Drag => return drag(state, mouse),
        MouseKind::Up if state.dragging => {
            state.dragging = false;
            return;
        }
        MouseKind::Up => return release(state),
        _ => return,
    }
    let Some(track) = state.track else {
        return;
    };
    let row = usize::from(mouse.row.clamp(track.top, track.top + track.rows - 1) - track.top);
    let offset = state.transcript.offset_at(row, usize::from(track.rows));
    state.transcript.jump(offset);
}

/// The lines on screen, each with the row and line that draw it.
fn on_screen(transcript: &Transcript) -> Vec<(transcript::Anchor, String)> {
    transcript
        .screen()
        .into_iter()
        .map(|(at, line)| (at, line.to_string()))
        .collect()
}

/// A press in the transcript starts a selection: a cell, then a word on a
/// double click and a line on a triple, counted on the same word within
/// [`selection::CLICK`]. A press anywhere else drops the selection.
fn press(state: &mut State, mouse: Mouse) {
    let select = &mut state.selecting;
    select.edge = None;
    let inside = state
        .transcript_area
        .filter(|area| area.holds(mouse.column, mouse.row));
    let Some(area) = inside else {
        select.gesture = None;
        select.ends = None;
        return;
    };
    let lines = on_screen(&state.transcript);
    let anchors: Vec<_> = lines.iter().map(|(at, _)| *at).collect();
    let row = usize::from(mouse.row - area.y);
    let column = usize::from(mouse.column - area.x);
    let width = usize::from(area.width);
    let Some(point) = selection::point_at(column, row as isize, &anchors, width) else {
        select.gesture = None;
        select.ends = None;
        return;
    };
    let text = lines.get(row).map_or("", |(_, text)| text.as_str());
    let word = lines
        .get(row)
        .and_then(|_| selection::word_at(text, point.column));
    let count = match (select.last_click, word) {
        (Some(previous), Some(word))
            if mouse.at.saturating_sub(previous.at) <= selection::CLICK
                && previous.point.at == point.at
                && previous.word == word =>
        {
            previous.count % 3 + 1
        }
        _ => 1,
    };
    select.last_click = word.map(|word| Click {
        at: mouse.at,
        point,
        word,
        count,
    });
    let granularity = match count {
        2 => Granularity::Word,
        3 => Granularity::Line,
        _ => Granularity::Character,
    };
    let initial = selection::range_at(point, granularity, text, width);
    select.gesture = Some(Gesture {
        granularity: if initial.is_some() {
            granularity
        } else {
            Granularity::Character
        },
        initial,
        dragged: false,
    });
    select.ends = Some(initial.map_or((point, point), |range| (range.start, range.end)));
}

/// A drag moves the selection's free end. Held on the transcript's top or
/// bottom line, or past it, the view scrolls the selection on a line every
/// [`EDGE_SCROLL`].
fn drag(state: &mut State, mouse: Mouse) {
    let Some(area) = state.transcript_area else {
        return;
    };
    let Some(gesture) = state.selecting.gesture.as_mut() else {
        return;
    };
    gesture.dragged = true;
    state.selecting.last_click = None;
    let room = area.rows as isize;
    let row = mouse.row as isize - area.y as isize;
    let pinned = row.clamp(0, (room - 1).max(0));
    let column = usize::from(mouse.column.saturating_sub(area.x));
    extend_to(state, column, if row >= room { room } else { pinned });
    let direction = if row <= 0 {
        -1
    } else if row >= room - 1 {
        1
    } else {
        0
    };
    if direction == 0 {
        state.selecting.edge = None;
        return;
    }
    let next = state
        .selecting
        .edge
        .map_or(mouse.at + EDGE_SCROLL, |edge| edge.next);
    state.selecting.edge = Some(EdgeHold {
        column,
        row: pinned,
        direction,
        next,
    });
}

/// Moves the selection's free end to the point under screen line `row`, a
/// word or a line at a time after a double or triple click.
fn extend_to(state: &mut State, column: usize, row: isize) {
    let Some(area) = state.transcript_area else {
        return;
    };
    let width = usize::from(area.width);
    let anchors: Vec<_> = state
        .transcript
        .screen()
        .into_iter()
        .map(|(at, _)| at)
        .collect();
    let Some(point) = selection::point_at(column, row, &anchors, width) else {
        return;
    };
    let select = &mut state.selecting;
    let (Some(gesture), Some((anchor, _))) = (select.gesture, select.ends) else {
        return;
    };
    let range = gesture.initial.and_then(|_| {
        let text = state.transcript.line_text(point.at);
        selection::range_at(point, gesture.granularity, &text, width)
    });
    select.ends = Some(match (gesture.initial, range) {
        (Some(initial), Some(range)) => {
            let before = selection::compare_lines(range.start.at, initial.start.at)
                .then(range.start.column.cmp(&initial.start.column))
                .is_lt();
            if before {
                (initial.end, range.start)
            } else {
                (initial.start, range.end)
            }
        }
        (initial, _) => (initial.map_or(anchor, |initial| initial.start), point),
    });
}

/// The end of a press: a click selects nothing and only clears what was
/// selected; a drag, a word, or a line stays selected.
fn release(state: &mut State) {
    let select = &mut state.selecting;
    select.edge = None;
    let Some(gesture) = select.gesture.take() else {
        return;
    };
    if gesture.granularity == Granularity::Character && !gesture.dragged {
        select.ends = None;
    } else {
        select.finished = true;
    }
}

/// Scrolls a drag held at an edge on by a line for each [`EDGE_SCROLL`]
/// that has passed, until either end of the transcript.
fn edge_scroll(state: &mut State) {
    while let Some(mut edge) = state.selecting.edge {
        if state.now < edge.next {
            return;
        }
        let at_end = if edge.direction < 0 {
            !state.transcript.more_above()
        } else {
            state.transcript.following()
        };
        if at_end {
            state.selecting.edge = None;
            return;
        }
        if edge.direction < 0 {
            state.transcript.scroll_up(1);
        } else {
            state.transcript.scroll_down(1);
        }
        extend_to(state, edge.column, edge.row);
        edge.next += EDGE_SCROLL;
        state.selecting.edge = Some(edge);
    }
}

fn key(state: &mut State, input: KeyInput) -> Vec<Effect> {
    let bound = keys::action(state.focus.scope(), input);
    // A first Ctrl+C arms the quit and a second within the window quits, so
    // a stray press never ends the session. Any other key disarms it and
    // then does what it does.
    if bound == Some(Action::Quit) {
        if state.quitting() {
            return vec![Effect::Quit];
        }
        state.quit_until = Some(state.now + QUIT_WINDOW);
        return Vec::new();
    }
    state.quit_until = None;
    match state.focus {
        Focus::Composer => composer_key(state, bound, input),
        Focus::AgentList => list_key(state, bound),
        Focus::Inspect(_) => inspect_key(state, bound),
    }
    Vec::new()
}

fn composer_key(state: &mut State, bound: Option<Action>, input: KeyInput) {
    let draft = &mut state.draft;
    let complete = match bound {
        Some(Action::ToggleAgentList) => {
            state.focus = Focus::AgentList;
            state.notice = None;
            return;
        }
        // Esc stops a sample the way it interrupts a turn or cancels compaction.
        // Esc drops a selection before it stops anything.
        Some(Action::Interrupt) if state.selecting.clear() => return,
        Some(Action::Interrupt) => {
            end_turn(state, Outcome::Interrupted);
            state.activity = None;
            state.notice = None;
            return;
        }
        Some(Action::ToggleSampleActivity) => {
            toggle_activity(state);
            return;
        }
        Some(Action::Submit) => {
            state.notice = Some(Notice::NoModel);
            return;
        }
        Some(Action::PageUp) => return state.transcript.scroll_up(state.transcript.page()),
        Some(Action::PageDown) => return state.transcript.scroll_down(state.transcript.page()),
        Some(Action::PreviousPrompt) => return state.transcript.previous_prompt(),
        Some(Action::NextPrompt) => return state.transcript.next_prompt(),
        Some(Action::ToStart) => return state.transcript.to_start(),
        Some(Action::ToLatest) => return state.transcript.follow(),
        Some(Action::Newline) => draft.newline(),
        Some(Action::Undo) => {
            draft.undo();
            true
        }
        Some(Action::Backspace) => {
            draft.backspace();
            true
        }
        Some(Action::Delete) => {
            draft.delete();
            true
        }
        Some(Action::Left) => {
            draft.left();
            true
        }
        Some(Action::Right) => {
            draft.right();
            true
        }
        // Home and End reach the drawn row's edge, then the logical line's.
        Some(Action::LineStart) => {
            draft.row_home(state.window.width());
            true
        }
        Some(Action::LineEnd) => {
            draft.row_end(state.window.width());
            true
        }
        // Up and Down move between drawn rows, and from the first or the
        // last row browse the prompts the session holds; Ctrl+P and Ctrl+N
        // browse at once.
        Some(Action::CaretUp | Action::CaretDown) => {
            let up = bound == Some(Action::CaretUp);
            let width = state.window.width();
            if !draft.vertical(width, up) {
                draft.recall(&input_history(&state.transcript), up, width);
            }
            true
        }
        Some(Action::RecallOlder | Action::RecallNewer) => {
            let older = bound == Some(Action::RecallOlder);
            draft.recall(
                &input_history(&state.transcript),
                older,
                state.window.width(),
            );
            true
        }
        Some(Action::LogicalStart) => {
            draft.home();
            true
        }
        Some(Action::LogicalEnd) => {
            draft.end();
            true
        }
        Some(Action::WordLeft) => {
            draft.word_left();
            true
        }
        Some(Action::WordRight) => {
            draft.word_right();
            true
        }
        Some(Action::KillWordLeft) => {
            draft.kill_word_left();
            true
        }
        Some(Action::KillWordRight) => {
            draft.kill_word_right();
            true
        }
        Some(Action::KillLineLeft) => {
            draft.kill_line_left();
            true
        }
        Some(Action::KillLineRight) => {
            draft.kill_line_right();
            true
        }
        Some(Action::Yank) => draft.yank(),
        Some(Action::YankPop) => {
            draft.yank_pop();
            true
        }
        _ => match input.types() {
            Some(c) => draft.type_text(c.encode_utf8(&mut [0; 4])),
            None => return,
        },
    };
    state.notice = (!complete).then_some(Notice::DraftLimit);
}

/// The prompts the session holds, newest first, as Up recalls them: the
/// TypeScript `inputHistory` over the transcript's user rows. The preview
/// has no pending input or commands to recall.
fn input_history(transcript: &Transcript) -> Vec<String> {
    transcript
        .rows
        .iter()
        .rev()
        .filter_map(|row| match row {
            transcript::Row::User(text) if !text.trim().is_empty() => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn list_key(state: &mut State, bound: Option<Action>) {
    match bound {
        Some(Action::SelectPrevious) => step_selection(state, -1),
        Some(Action::SelectNext) => step_selection(state, 1),
        Some(Action::InspectSelected) => {
            state.focus = Focus::Inspect(state.selected);
            state.notice = None;
        }
        Some(Action::ReturnToComposer) => {
            state.focus = Focus::Composer;
            state.notice = None;
        }
        _ => state.notice = Some(Notice::ListKeys),
    }
}

fn inspect_key(state: &mut State, bound: Option<Action>) {
    match bound {
        Some(Action::ReturnToComposer) => {
            state.focus = Focus::Composer;
            state.notice = None;
        }
        Some(Action::ReturnToAgentList) => {
            state.focus = Focus::AgentList;
            state.notice = None;
        }
        _ => state.notice = Some(Notice::ReadOnly),
    }
}

/// Steps the sample from none to a turn, to a compaction, and back to none.
/// Each sample starts its own clock, as a compaction does after a turn.
fn toggle_activity(state: &mut State) {
    state.notice = None;
    end_turn(state, Outcome::Completed);
    state.activity = match state.activity.map(|sample| sample.kind) {
        None => {
            state.samples = state.samples.wrapping_add(1);
            state.summary = None;
            // The turn's work: a code-mode script, whose last call runs
            // until the turn ends.
            state.live_call = Some(state.transcript.rows.len());
            state.transcript.rows.push(live::at(Duration::ZERO));
            Some(SampleActivity {
                kind: SampleKind::Turn,
                word: activity::pick(activity::WORDS, &format!("sample-{}", state.samples)),
                started: state.now,
            })
        }
        Some(SampleKind::Turn) => Some(SampleActivity {
            kind: SampleKind::Compaction,
            word: crate::copy::COMPACTING,
            started: state.now,
        }),
        Some(SampleKind::Compaction) => None,
    };
}

/// Records how a running sample turn ended; anything else is left alone.
fn end_turn(state: &mut State, outcome: Outcome) {
    if let Some(sample) = state.activity
        && sample.kind == SampleKind::Turn
    {
        state.summary = Some(TurnSummary {
            outcome,
            elapsed: state.now.saturating_sub(sample.started),
        });
    }
    // The turn's script settles with it: run to its end when the turn
    // completed, stopped where it was when Esc interrupted it.
    let elapsed = state.live_elapsed().unwrap_or_default();
    let Some(index) = state.live_call.take() else {
        return;
    };
    let how = match outcome {
        Outcome::Completed => live::Settle::Completed,
        Outcome::Interrupted => live::Settle::Interrupted,
    };
    if let Some(row) = state.transcript.rows.get_mut(index) {
        *row = live::settle(elapsed, how);
        state.transcript.touched(index);
    }
}

/// Brings the turn's running script to the clock, re-measuring its row only
/// when a call started or settled.
fn advance_script(state: &mut State) {
    let (Some(elapsed), Some(index)) = (state.live_elapsed(), state.live_call) else {
        return;
    };
    let row = live::at(elapsed);
    if state.transcript.rows.get(index) != Some(&row) {
        state.transcript.rows[index] = row;
        state.transcript.touched(index);
    }
}

/// Moves the selection by identity, so a reordered list keeps the same agent.
fn step_selection(state: &mut State, delta: isize) {
    let index = SAMPLE_AGENTS
        .iter()
        .position(|a| a.id == state.selected)
        .unwrap_or(0);
    let next = index
        .saturating_add_signed(delta)
        .min(SAMPLE_AGENTS.len() - 1);
    state.selected = SAMPLE_AGENTS[next].id;
    state.notice = None;
}

pub fn agent(id: &str) -> Option<&'static SampleAgent> {
    SAMPLE_AGENTS.iter().find(|a| a.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{Key, Mods};
    use crate::transcript::{CallState, Row};

    fn press(state: &mut State, key: Key) -> Vec<Effect> {
        update(state, Msg::Key(KeyInput::plain(key)))
    }

    fn chord(state: &mut State, key: Key, mods: Mods) -> Vec<Effect> {
        update(state, Msg::Key(KeyInput::new(key, mods)))
    }

    fn type_str(state: &mut State, text: &str) {
        for c in text.chars() {
            press(state, Key::Char(c));
        }
    }

    #[test]
    fn enter_is_refused_and_keeps_the_draft() {
        let mut state = State::default();
        type_str(&mut state, "hello");
        press(&mut state, Key::Left);
        assert_eq!(press(&mut state, Key::Enter), []);
        assert_eq!(state.notice, Some(Notice::NoModel));
        assert_eq!((state.draft.text(), state.draft.caret()), ("hello", 4));
    }

    #[test]
    fn newline_keys_and_paste_never_submit() {
        let mut state = State::default();
        type_str(&mut state, "a");
        chord(&mut state, Key::Enter, Mods::ALT);
        chord(&mut state, Key::Char('j'), Mods::CTRL);
        update(&mut state, Msg::Paste("b\r\nc".into()));
        assert_eq!(state.draft.text(), "a\n\nb\nc");
        assert_eq!(state.notice, None);
    }

    #[test]
    fn inspection_blocks_edits_and_returning_restores_the_draft() {
        let mut state = State::default();
        type_str(&mut state, "abc");
        update(&mut state, Msg::Paste("XY".into()));
        press(&mut state, Key::Left);
        let before = (state.draft.text().to_owned(), state.draft.caret());

        chord(&mut state, Key::Char('g'), Mods::CTRL);
        press(&mut state, Key::Down);
        press(&mut state, Key::Enter);
        assert_eq!(state.focus, Focus::Inspect("sample-reviewer"));
        type_str(&mut state, "zz");
        press(&mut state, Key::Backspace);
        chord(&mut state, Key::Char('-'), Mods::CTRL);
        update(&mut state, Msg::Paste("pasted".into()));
        press(&mut state, Key::Enter);
        assert_eq!(state.notice, Some(Notice::ReadOnly));

        press(&mut state, Key::Esc);
        assert_eq!(state.focus, Focus::Composer);
        assert_eq!((state.draft.text().to_owned(), state.draft.caret()), before);
        chord(&mut state, Key::Char('-'), Mods::CTRL);
        assert_eq!(state.draft.text(), "abc");
    }

    #[test]
    fn list_selection_follows_identity_and_stops_at_the_ends() {
        let mut state = State::default();
        chord(&mut state, Key::Char('g'), Mods::CTRL);
        press(&mut state, Key::Up);
        assert_eq!(state.selected, "sample-explorer");
        for _ in 0..5 {
            press(&mut state, Key::Down);
        }
        assert_eq!(state.selected, SAMPLE_AGENTS.last().unwrap().id);
        type_str(&mut state, "q");
        assert_eq!(state.notice, Some(Notice::ListKeys));
        assert!(state.draft.is_empty());
        chord(&mut state, Key::Char('g'), Mods::CTRL);
        assert_eq!(state.focus, Focus::Composer);
        assert_eq!(state.selected, SAMPLE_AGENTS.last().unwrap().id);
    }

    #[test]
    fn a_second_ctrl_c_quits_from_every_focus_and_changes_nothing() {
        for focus in [
            Focus::Composer,
            Focus::AgentList,
            Focus::Inspect("sample-explorer"),
        ] {
            let mut state = State {
                focus,
                ..State::default()
            };
            type_str(&mut state, "x");
            let before = (state.focus, state.notice, state.draft.text().to_owned());
            // The first press only arms the quit.
            assert!(chord(&mut state, Key::Char('c'), Mods::CTRL).is_empty());
            assert!(state.quitting());
            assert_eq!(
                chord(&mut state, Key::Char('c'), Mods::CTRL),
                [Effect::Quit]
            );
            assert_eq!(
                (state.focus, state.notice, state.draft.text().to_owned()),
                before
            );
        }
    }

    #[test]
    fn readline_keys_move_kill_and_yank_as_the_oracle_does() {
        let mut state = State::default();
        let caret_at = |state: &State| {
            let (text, caret) = (state.draft.text(), state.draft.caret());
            format!("{}|{}", &text[..caret], &text[caret..])
        };
        type_str(&mut state, "run src/app.ts now");
        chord(&mut state, Key::Left, Mods::CTRL);
        assert_eq!(caret_at(&state), "run src/app.ts |now");
        chord(&mut state, Key::Left, Mods::ALT);
        assert_eq!(caret_at(&state), "run src/app.|ts now");
        chord(&mut state, Key::Char('b'), Mods::ALT);
        assert_eq!(caret_at(&state), "run src/app|.ts now");
        chord(&mut state, Key::Char('f'), Mods::ALT);
        assert_eq!(caret_at(&state), "run src/app.|ts now");
        chord(&mut state, Key::Right, Mods::CTRL);
        assert_eq!(caret_at(&state), "run src/app.ts| now");
        chord(&mut state, Key::Char('a'), Mods::CTRL);
        assert_eq!(caret_at(&state), "|run src/app.ts now");
        chord(&mut state, Key::Char('e'), Mods::CTRL);
        assert_eq!(caret_at(&state), "run src/app.ts now|");
        // Ctrl+W, then Alt+Backspace, join one kill that Ctrl+Y puts back.
        chord(&mut state, Key::Char('w'), Mods::CTRL);
        chord(&mut state, Key::Backspace, Mods::ALT);
        assert_eq!(caret_at(&state), "run src/app.|");
        chord(&mut state, Key::Char('y'), Mods::CTRL);
        assert_eq!(caret_at(&state), "run src/app.ts now|");
        // Ctrl+U and Ctrl+K; Alt+Y swaps the yank for the older kill.
        chord(&mut state, Key::Char('u'), Mods::CTRL);
        assert_eq!(state.draft.text(), "");
        chord(&mut state, Key::Char('y'), Mods::CTRL);
        chord(&mut state, Key::Char('y'), Mods::ALT);
        assert_eq!(state.draft.text(), "ts now");
        // Ctrl+B, Ctrl+F, Ctrl+D, Alt+D, and Ctrl+Delete.
        chord(&mut state, Key::Char('a'), Mods::CTRL);
        chord(&mut state, Key::Char('f'), Mods::CTRL);
        chord(&mut state, Key::Char('b'), Mods::CTRL);
        chord(&mut state, Key::Char('d'), Mods::CTRL);
        assert_eq!(caret_at(&state), "|s now");
        chord(&mut state, Key::Char('d'), Mods::ALT);
        assert_eq!(caret_at(&state), "| now");
        chord(&mut state, Key::Delete, Mods::CTRL);
        assert_eq!(caret_at(&state), "|");
        // Every kill is its own undo step.
        chord(&mut state, Key::Char('-'), Mods::CTRL);
        assert_eq!(state.draft.text(), " now");
    }

    #[test]
    fn up_and_down_move_between_rows_before_they_recall_prompts() {
        let mut state = State::default();
        // As a frame of 40 columns would have wrapped the draft.
        state.window.follow(0, 5, 1, 40);
        let caret_at = |state: &State| {
            let (text, caret) = (state.draft.text(), state.draft.caret());
            format!("{}|{}", &text[..caret], &text[caret..])
        };
        type_str(&mut state, "one");
        chord(&mut state, Key::Enter, Mods::ALT);
        type_str(&mut state, "second");
        for _ in 0..3 {
            press(&mut state, Key::Left);
        }
        // The column is kept by cells, past the end of `one`.
        press(&mut state, Key::Up);
        assert_eq!(caret_at(&state), "one|\nsecond");
        // From the first row, Up recalls the newest prompt.
        press(&mut state, Key::Up);
        assert_eq!(caret_at(&state), "Run the parser tests|");
        press(&mut state, Key::Up);
        assert_eq!(state.draft.text(), "Find TODO comments in the source files");
        // Down returns, and back past the newest restores the draft and caret.
        press(&mut state, Key::Down);
        press(&mut state, Key::Down);
        assert_eq!(caret_at(&state), "one|\nsecond");
        press(&mut state, Key::Down);
        assert_eq!(caret_at(&state), "one\nsec|ond");
        // The last row has nothing newer.
        press(&mut state, Key::Down);
        assert_eq!(caret_at(&state), "one\nsec|ond");
        // Ctrl+P recalls from any row, and Ctrl+N comes back.
        chord(&mut state, Key::Char('p'), Mods::CTRL);
        assert_eq!(state.draft.text(), "Run the parser tests");
        chord(&mut state, Key::Char('n'), Mods::CTRL);
        assert_eq!(caret_at(&state), "one\nsec|ond");
        // Alt+↑ is not a caret key.
        chord(&mut state, Key::Up, Mods::ALT);
        assert_eq!(caret_at(&state), "one\nsec|ond");
    }

    #[test]
    fn an_armed_quit_lapses_with_its_window_or_at_any_other_key() {
        let mut state = State::default();
        chord(&mut state, Key::Char('c'), Mods::CTRL);
        // The loop wakes when the window ends, and the prompt goes with it.
        assert_eq!(state.next_change(), Some(QUIT_WINDOW));
        update(
            &mut state,
            Msg::Tick(QUIT_WINDOW - Duration::from_millis(1)),
        );
        assert!(state.quitting());
        update(&mut state, Msg::Tick(QUIT_WINDOW));
        assert!(!state.quitting() && state.quit_until.is_none());
        assert_eq!(state.next_change(), None);
        assert!(chord(&mut state, Key::Char('c'), Mods::CTRL).is_empty());
        // Another key disarms it and still does what it does.
        type_str(&mut state, "a");
        assert!(!state.quitting());
        assert_eq!(state.draft.text(), "a");
        assert!(chord(&mut state, Key::Char('c'), Mods::CTRL).is_empty());
        // Ctrl+C never interrupts a turn; Esc does.
        chord(&mut state, Key::Char('t'), Mods::CTRL);
        assert_eq!(state.mode(), Mode::Running);
        chord(&mut state, Key::Char('c'), Mods::CTRL);
        assert_eq!(state.mode(), Mode::Running);
    }

    #[test]
    fn ctrl_t_steps_through_a_turn_and_a_compaction_at_the_last_tick() {
        let mut state = State::default();
        type_str(&mut state, "keep");
        assert_eq!(state.mode(), Mode::Idle);
        update(&mut state, Msg::Tick(Duration::from_secs(3)));
        chord(&mut state, Key::Char('t'), Mods::CTRL);
        let sample = state.activity.expect("sample started");
        assert_eq!(
            (sample.kind, sample.started),
            (SampleKind::Turn, Duration::from_secs(3))
        );
        assert!(activity::WORDS.contains(&sample.word));
        assert_eq!(state.mode(), Mode::Running);
        assert_eq!(state.next_change(), Some(activity::BEAT));

        update(&mut state, Msg::Tick(Duration::from_secs(5)));
        chord(&mut state, Key::Char('t'), Mods::CTRL);
        let sample = state.activity.expect("compaction started");
        assert_eq!(
            (sample.kind, sample.word, sample.started),
            (
                SampleKind::Compaction,
                "Compacting history",
                Duration::from_secs(5)
            )
        );
        assert_eq!(state.mode(), Mode::Compacting);

        chord(&mut state, Key::Char('t'), Mods::CTRL);
        assert_eq!(state.activity, None);
        assert_eq!(state.mode(), Mode::Idle);
        assert_eq!(state.next_change(), None);
        // Esc stops either sample, as it would interrupt a turn or cancel compaction.
        for steps in 1..=2 {
            for _ in 0..steps {
                chord(&mut state, Key::Char('t'), Mods::CTRL);
            }
            press(&mut state, Key::Esc);
            assert_eq!(state.activity, None);
        }
        assert_eq!((state.draft.text(), state.draft.caret()), ("keep", 4));
    }

    #[test]
    fn a_finished_turn_leaves_its_outcome_until_the_next_turn_starts() {
        let mut state = State::default();
        let ctrl_t = |state: &mut State, secs| {
            update(state, Msg::Tick(Duration::from_secs(secs)));
            chord(state, Key::Char('t'), Mods::CTRL);
        };
        ctrl_t(&mut state, 1);
        ctrl_t(&mut state, 9);
        let completed = Some(TurnSummary {
            outcome: Outcome::Completed,
            elapsed: Duration::from_secs(8),
        });
        assert_eq!(state.summary, completed);
        // Ending the compaction, by Ctrl+T or Esc, keeps the turn's outcome.
        ctrl_t(&mut state, 12);
        assert_eq!(state.summary, completed);
        ctrl_t(&mut state, 20);
        assert_eq!(state.summary, None);
        update(&mut state, Msg::Tick(Duration::from_secs(23)));
        press(&mut state, Key::Esc);
        assert_eq!(
            state.summary,
            Some(TurnSummary {
                outcome: Outcome::Interrupted,
                elapsed: Duration::from_secs(3),
            })
        );
        press(&mut state, Key::Esc);
        assert_eq!(state.summary.map(|s| s.outcome), Some(Outcome::Interrupted));
    }

    #[test]
    fn a_sample_turn_runs_a_script_that_settles_when_the_turn_ends() {
        let mut state = State::default();
        let rows = state.transcript.rows.len();
        let script = |state: &State| match &state.transcript.rows[rows] {
            Row::Script {
                state: script,
                summary,
                calls,
                ..
            } => (*script, summary.clone(), calls.len()),
            other => panic!("{other:?}"),
        };
        update(&mut state, Msg::Tick(Duration::from_millis(250)));
        chord(&mut state, Key::Char('t'), Mods::CTRL);
        assert_eq!(script(&state), (CallState::Running, None, 1));
        assert!(state.transcript.running());
        // The loop wakes for the blink as well as the shimmer and the script.
        assert_eq!(
            state.next_change(),
            Some(activity::BEAT.min(Duration::from_millis(350)))
        );
        // Calls arrive on the clock: the glob settles, and the reads start.
        update(&mut state, Msg::Tick(Duration::from_millis(650)));
        assert_eq!(script(&state), (CallState::Running, None, 1 + live::POOL));
        chord(&mut state, Key::Char('t'), Mods::CTRL);
        assert_eq!(script(&state).0, CallState::Done);
        assert!(!state.transcript.running());
        // Esc stops the next turn, and its script fails where it stood.
        chord(&mut state, Key::Char('t'), Mods::CTRL);
        chord(&mut state, Key::Char('t'), Mods::CTRL);
        press(&mut state, Key::Esc);
        assert_eq!(state.transcript.rows.len(), rows + 2);
        match &state.transcript.rows[rows + 1] {
            Row::Script {
                state: script,
                summary,
                ..
            } => {
                assert_eq!(
                    (*script, summary.as_deref()),
                    (CallState::Failed, Some("interrupted"))
                );
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(state.next_change(), None);
    }

    #[test]
    fn inspection_is_its_own_mode_whatever_the_session_does() {
        let mut state = State::default();
        chord(&mut state, Key::Char('t'), Mods::CTRL);
        chord(&mut state, Key::Char('g'), Mods::CTRL);
        assert_eq!(state.mode(), Mode::Running);
        press(&mut state, Key::Enter);
        assert_eq!(state.mode(), Mode::Inspecting);
        press(&mut state, Key::Esc);
        assert_eq!(state.mode(), Mode::Running);
    }

    #[test]
    fn altgr_types_and_unbound_chords_leave_the_draft_alone() {
        let mut state = State::default();
        chord(&mut state, Key::Char('@'), Mods::CTRL | Mods::ALT);
        chord(&mut state, Key::Char('x'), Mods::ALT);
        chord(&mut state, Key::Char('x'), Mods::CTRL);
        press(&mut state, Key::BackTab);
        press(&mut state, Key::F(5));
        assert_eq!(state.draft.text(), "@");
        assert_eq!(state.focus, Focus::Composer);
    }

    #[test]
    fn resize_and_tick_leave_the_draft_and_notice_alone() {
        let mut state = State::default();
        type_str(&mut state, "ab");
        press(&mut state, Key::Enter);
        update(&mut state, Msg::Resize { cols: 20, rows: 5 });
        update(&mut state, Msg::Tick(Duration::from_secs(9)));
        assert_eq!(state.draft.text(), "ab");
        assert_eq!(state.notice, Some(Notice::NoModel));
        assert_eq!(state.now, Duration::from_secs(9));
    }
}
