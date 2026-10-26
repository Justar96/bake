//! Presentation state and its update function. Every input arrives as a
//! [`Msg`]; [`update`] applies it and returns the [`Effect`]s the terminal
//! owner must perform. Nothing here touches the terminal or reads a clock.

use std::time::Duration;

use crate::activity::{self, Tones};
use crate::agents::{self, Agent, AgentState};
use crate::completion::{self, Command};
use crate::composer::ComposerWindow;
use crate::dialog::{Dialog, DialogAnswer};
use crate::editor::Draft;
use crate::frame::FrameStyle;
use crate::jobs::{self, BackgroundJob};
use crate::keys::{self, Action, Key, KeyInput, Mods, Scope};
use crate::live;
use crate::mention::{self, Candidate, Choice};
use crate::mode::Mode;
use crate::paste::{self, Image};
use crate::runtime::{Approval, ApprovalAnswer, Pending, Prompt, RuntimeUpdate, Submission};
use crate::selection::{self, Granularity, Point, Range};
use crate::status::StatusInput;
use crate::tips::{self, Tips};
use crate::transcript::{self, Transcript};
use crate::wheel::{self, WheelSteps};

/// Everything the frontend applies, in arrival order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Msg {
    /// A key, read after every frame drawn so far.
    Key(KeyInput),
    /// An explicitly reported held-key repeat. The composer accepts it;
    /// an approval requires fresh presses for every decision.
    KeyRepeat(KeyInput),
    /// A key the terminal owner's input thread read; `burst` when it came
    /// in one sweep with other keys, as a paste without bracketed paste or
    /// text typed faster than it was read does. A key in a burst never
    /// answers an approval request.
    KeyRead {
        key: KeyInput,
        burst: bool,
    },
    /// The input thread has read, and sent ahead of this, every key that
    /// was waiting once the displayed approval request came on screen for
    /// the showing `token` names ([`State::approval_drawn`]). Keys after
    /// this one may answer the request while that showing lasts; none
    /// before it can.
    ApprovalArmed(u64),
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
    /// The image an [`Effect::ReadImage`] read, or `None` when there was none
    /// or it could not be read.
    ImageRead {
        source: ImageSource,
        image: Option<Image>,
    },
    /// A change the agent runtime reports, in the order it sent them.
    Runtime(RuntimeUpdate),
    /// The paths an [`Effect::FindFiles`] found for `query`, or why it could
    /// not look. A result for a query no longer wanted is dropped.
    FilesFound {
        query: String,
        found: Result<Vec<Candidate>, String>,
    },
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

/// The slash menu as drawn: the matching commands and the selected one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Menu {
    pub entries: Vec<Command>,
    pub selected: usize,
}

/// The file menu as drawn: the mentions found for the `@` token at the
/// caret, the selected one, and whether discovery is still looking or
/// failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMenu {
    pub entries: Vec<Choice>,
    pub selected: usize,
    pub loading: bool,
    pub error: Option<String>,
}

/// What a menu remembers about the draft and caret it was last changed at:
/// the selected row's name and whether Esc closed it there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct MenuMemory {
    at: Option<(String, usize)>,
    selected: Option<String>,
    dismissed: bool,
}

/// The query the view last asked discovery for, and what came back for it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Files {
    pub query: Option<String>,
    /// `None` while discovery is looking.
    pub found: Option<Result<Vec<Candidate>, String>>,
}

/// How long the scroll indicator says whether a selection was copied.
pub const COPIED: Duration = Duration::from_millis(1500);

/// Requests the terminal owner performs on the view's behalf.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Quit,
    /// Put this text on the clipboard, then report with [`Msg::Copied`].
    Copy(String),
    /// Read an image, then report with [`Msg::ImageRead`].
    ReadImage(ImageSource),
    /// Send a prompt, with any staged images, to the runtime. The draft is
    /// cleared; the prompt appears once the runtime commits it.
    Submit(Prompt),
    /// Stop the running turn; the runtime reports when it has ended.
    Cancel,
    /// Send every waiting prompt now; the runtime reports the turn it starts.
    SendPending,
    /// List the paths that match this `@` query, then report with
    /// [`Msg::FilesFound`]. A newer request supersedes it.
    FindFiles(String),
    /// Answer the approval request `generation` names. The runtime settles
    /// it only while it is the displayed request.
    Answer {
        generation: u64,
        answer: ApprovalAnswer,
    },
    /// Answer the dialog `id` names ([`crate::dialog`]). The runtime takes
    /// it only while that dialog is open.
    Dialog {
        id: u64,
        answer: DialogAnswer,
    },
    /// Stop the background shell with this id; the runtime reports it
    /// stopping and stopped ([`RuntimeUpdate::Jobs`]).
    StopJob(String),
    /// Stop the run of the subagent with this id; the runtime reports how
    /// it ended ([`RuntimeUpdate::Agents`]).
    StopAgent(String),
    /// Move the session to its model's next thinking level; the runtime
    /// reports it in the status line ([`RuntimeUpdate::Status`]).
    CycleThinking,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Focus {
    #[default]
    Composer,
    AgentList,
    /// Read-only inspection of the selected agent ([`State::selected`]).
    Inspect,
    /// The background shells' list ([`crate::jobs`]).
    Jobs,
    /// The selected background shell's output.
    Shell,
}

impl Focus {
    fn scope(self) -> Scope {
        match self {
            Self::Composer => Scope::Composer,
            Self::AgentList => Scope::AgentList,
            Self::Inspect => Scope::Inspect,
            Self::Jobs => Scope::Jobs,
            Self::Shell => Scope::Shell,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    /// Enter during the Ctrl+T sample activity, which takes no prompt.
    SampleOnly,
    /// Enter with staged images, which the preview cannot send.
    NoImages,
    ReadOnly,
    DraftLimit,
    /// Ctrl+V, or an empty paste, found no image on the clipboard.
    NoClipboardImage,
    /// A slash command was submitted; the preview runs none.
    NoCommands,
    /// Alt+↑ sent the waiting prompts.
    PendingSent,
    /// Alt+↑ with nothing waiting.
    NoPending,
    /// Enter with staged images while a live turn runs: a steer takes text
    /// alone, so the draft waits for the turn to end.
    ImagesWhileRunning,
    /// The preview's sample activity key in a live session.
    PreviewOnly,
    /// Ctrl+G in a live session that has started no subagents.
    NoAgents,
    /// The runtime gave prompts back to the draft.
    Returned,
    /// `/tree` or `/fork` put the chosen message into the draft.
    Restored,
    /// The runtime refused a prompt, for the reason in [`State::refusal`];
    /// its draft is back.
    Refused,
    /// While an approval waits, a key or paste that does not answer it: a
    /// key other than the panel's, or one typed before the request was on
    /// screen or in a burst of text.
    ApprovalKeys,
    /// While an approval waits, Enter before `y` chose to allow it.
    ApprovalChooseFirst,
}

/// Where a pasted image is read from: the clipboard, or a file whose path
/// was pasted, kept with the paste to insert as text should the file not
/// read as an image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageSource {
    Clipboard,
    File { path: String, pasted: String },
}

/// What a sample activity stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityKind {
    /// A running turn.
    Turn,
    /// History being compacted.
    Compaction,
}

/// What the header's activity line shows and the composer mode it puts the
/// session in: a turn the runtime is running, or a Ctrl+T sample, which
/// nothing runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Activity {
    pub kind: ActivityKind,
    /// The turn's word, kept until it ends; compaction names itself.
    pub word: &'static str,
    /// When it started, on the clock [`Msg::Tick`] carries.
    pub started: Duration,
    /// Whether the runtime owns it. Only the runtime ends its turns; a
    /// sample is stepped and stopped by keys.
    pub runtime: bool,
}

/// How a sample turn ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// It ran to its end; Ctrl+T moved on to a compaction.
    Completed,
    /// Esc stopped it.
    Interrupted,
    /// A live turn ended with an error, which the transcript shows.
    Failed,
}

/// The header's line about the last sample turn, held until the next starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnSummary {
    pub outcome: Outcome,
    pub elapsed: Duration,
    /// What ended: a turn, or a compaction that was the work itself.
    pub kind: ActivityKind,
}

/// Presentation state: the parent draft, keyboard focus, one notice, the
/// sample activity, the composer window, and the terminal's capabilities.
/// Navigation never replaces the draft, so its caret and undo survive it.
#[derive(Debug)]
pub struct State {
    pub draft: Draft,
    pub focus: Focus,
    /// The agents the row under the composer counts and Ctrl+G lists: the
    /// preview's samples, or the subagents a live session reported
    /// ([`RuntimeUpdate::Agents`]), in start order.
    pub agents: Vec<Agent>,
    /// The agent the list selects, by id; one no longer listed selects the
    /// first.
    pub selected: String,
    pub notice: Option<Notice>,
    pub activity: Option<Activity>,
    /// The runtime's turn a compaction interrupted, shown again when the
    /// compaction ends ([`RuntimeUpdate::CompactionEnded`]).
    pub compacting_over: Option<Activity>,
    /// What the runtime's turn is doing, as it last reported.
    pub phase: Option<String>,
    /// Prompts the runtime holds but has not admitted, as it last reported.
    pub pending: Vec<Pending>,
    /// How the last turn ended.
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
    /// Settled rows excluded from the inline live region while following.
    pub(crate) transcript_published: usize,
    /// How many times the runtime replaced the transcript
    /// ([`RuntimeUpdate::Replaced`]). A normal-screen writer that counts
    /// published rows starts its count over when this changes.
    pub transcript_epoch: u64,
    /// The transcript selection.
    pub selecting: Selecting,
    /// The terminal's columns at the last resize; a change drops the
    /// selection, whose lines wrap anew.
    columns: u16,
    /// Whether the last selection was copied, said until the time given.
    pub copied: Option<(bool, Duration)>,
    /// The banner's tips while they rotate ([`State::start_tips`]).
    tips: Option<Tips>,
    /// Images staged with the draft, by id, in the order they were pasted.
    /// The preview has no model to send them to; erasing a placeholder
    /// unstages its image.
    pub attachments: Vec<(u32, Image)>,
    /// Images staged so far; the next takes the next id.
    images: u32,
    /// The menu's selection and whether Esc closed it, for the draft and
    /// caret they were made at. Any edit or move starts the menu over.
    menu: MenuMemory,
    /// Path discovery for the file menu.
    pub files: Files,
    /// Whether a press on the scrollbar is being dragged.
    pub(crate) dragging: bool,
    /// While a first Ctrl+C is armed, when it lapses; a second before then
    /// quits. The prompt to press again shows until then.
    pub quit_until: Option<Duration>,
    /// Whether a live agent session answers the composer instead of the
    /// preview's fixture: the transcript starts with the session's rows,
    /// staged images are sent, slash prompts reach the runtime, the
    /// sample activity is refused, and the agent list holds the session's
    /// subagents.
    pub live: bool,
    /// Prompts sent to a live runtime that it has not admitted yet, each
    /// with the draft and staged images it came from.
    unadmitted: Vec<Unadmitted>,
    /// Prompts submitted so far; the next takes the next id.
    submitted: u64,
    /// Why the runtime last refused a prompt, for [`Notice::Refused`].
    pub refusal: Option<String>,
    /// Approval requests the runtime waits on, oldest first, as it last
    /// reported. While any waits, the first takes the keyboard: the draft,
    /// its caret, and its staged images stay as they were.
    pub approvals: Vec<Approval>,
    /// The dialog the runtime opened over the composer, if any.
    pub dialog: Option<Dialog>,
    /// A dialog opened, replaced, or closed since the last frame, so the
    /// keyboard changes hands at the next one ([`Self::approval_drawn`]).
    handoff_due: bool,
    /// The token of a keyboard handoff not armed yet. Until it is
    /// ([`Msg::ApprovalArmed`]), keys and pastes go to neither the dialog
    /// nor the draft, so input typed for what had the keyboard before
    /// never reaches what has it now.
    handoff: Option<u64>,
    /// The dialog the last frame drew whole, set by rendering.
    pub(crate) dialog_on_screen: Option<u64>,
    /// The dialog whose current showing holds, or is being handed, the
    /// keyboard. Cleared when a frame hides it, an approval covers it, or
    /// the terminal is resized, so it is handed over afresh.
    dialog_showing: Option<u64>,
    /// The displayed request's current showing: its generation and the
    /// token the showing was published under, from the first frame that
    /// drew it whole until one does not, a resize, or another request.
    approval_showing: Option<(u64, u64)>,
    /// Showings so far; the next takes the next token, so no token repeats.
    approval_showings: u64,
    /// The showing the input thread has armed ([`Msg::ApprovalArmed`]).
    approval_armed: Option<u64>,
    /// The showing in which `y` chose to allow; Enter confirms.
    approval_chosen: Option<u64>,
    /// The request the last rendered frame drew whole: its title, at
    /// least one row of its detail, and its keys.
    pub(crate) approval_on_screen: Option<u64>,
    /// The request this view has already answered, so a second key for it
    /// sends nothing before the runtime's next list arrives.
    approval_answered: Option<u64>,
    /// The first row of the displayed request's detail in view, and how
    /// many rows the last frame showed, for paging.
    pub(crate) approval_scroll: usize,
    pub(crate) approval_page: usize,
    /// The background shells as the runtime last reported them, in start
    /// order, and when that report arrived, which running clocks go on
    /// from.
    pub jobs: Vec<BackgroundJob>,
    pub jobs_at: Duration,
    /// The shell the list selects, by id; `None` selects the newest.
    pub job_selected: Option<String>,
    /// Rows the shell view is scrolled up from its output's end, and the
    /// most it could be at the last frame, set by rendering.
    pub shell_scroll: usize,
    pub(crate) shell_most: usize,
}

/// A submitted prompt's draft, kept until the runtime admits the prompt.
#[derive(Debug)]
struct Unadmitted {
    id: u64,
    draft: Draft,
    attachments: Vec<(u32, Image)>,
}

impl Default for State {
    fn default() -> Self {
        Self::new(FrameStyle::default(), Tones::default())
    }
}

impl State {
    /// Rows that will not change again, including a sample script only after
    /// its turn settles. Terminal scrollback may publish only these rows.
    pub fn settled_rows(&self) -> usize {
        self.live_call
            .unwrap_or(self.transcript.committed())
            .min(self.transcript.committed())
    }
    pub fn new(frame: FrameStyle, tones: Tones) -> Self {
        Self {
            draft: Draft::default(),
            focus: Focus::Composer,
            agents: agents::samples(),
            selected: String::new(),
            notice: None,
            activity: None,
            compacting_over: None,
            phase: None,
            pending: Vec::new(),
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
            transcript_published: 0,
            transcript_epoch: 0,
            selecting: Selecting::default(),
            columns: 0,
            copied: None,
            tips: None,
            attachments: Vec::new(),
            images: 0,
            menu: MenuMemory::default(),
            files: Files::default(),
            dragging: false,
            quit_until: None,
            live: false,
            unadmitted: Vec::new(),
            submitted: 0,
            refusal: None,
            approvals: Vec::new(),
            dialog: None,
            handoff_due: false,
            handoff: None,
            dialog_on_screen: None,
            dialog_showing: None,
            approval_showing: None,
            approval_showings: 0,
            approval_armed: None,
            approval_chosen: None,
            approval_on_screen: None,
            approval_answered: None,
            approval_scroll: 0,
            approval_page: 0,
            jobs: Vec::new(),
            jobs_at: Duration::ZERO,
            job_selected: None,
            shell_scroll: 0,
            shell_most: 0,
        }
    }

    /// The state of a live session whose transcript starts with `rows`:
    /// what a resumed session replays, or what startup reported.
    pub fn live(frame: FrameStyle, tones: Tones, rows: Vec<transcript::Row>) -> Self {
        Self {
            transcript: Transcript::new(rows),
            live: true,
            agents: Vec::new(),
            ..Self::new(frame, tones)
        }
    }

    /// Starts the live banner's tips at [`State::now`], rotating from a
    /// place `seed` picks. The terminal owner calls it in fullscreen only:
    /// inline, the banner is already in scrollback when a tip would change.
    pub fn start_tips(&mut self, seed: u64) {
        self.tips = Some(Tips::new(self.now, seed));
        self.show_tip();
    }

    /// Brings the banner's tip up to [`State::now`], and forgets the tips
    /// once the last has gone.
    fn show_tip(&mut self) {
        let tip = self.tips.and_then(|tips| tips.at(self.now));
        tips::show(&mut self.transcript, tip);
        if self.tips.is_some_and(|tips| tips.done(self.now)) {
            self.tips = None;
        }
    }

    /// Stops the tips and takes the one shown down.
    fn end_tips(&mut self) {
        self.tips = None;
        tips::show(&mut self.transcript, None);
    }

    /// The draft and caret while a menu may open there: in the composer,
    /// unless Esc closed one at this very draft and caret.
    fn menu_here(&self) -> Option<(&str, usize)> {
        if self.focus != Focus::Composer {
            return None;
        }
        let (draft, caret) = (self.draft.text(), self.draft.caret());
        let here = self
            .menu
            .at
            .as_ref()
            .is_some_and(|(text, at)| text == draft && *at == caret);
        (!(here && self.menu.dismissed)).then_some((draft, caret))
    }

    /// The row a menu keeps selected at this draft and caret, by name.
    fn remembered(&self, names: impl Iterator<Item = impl AsRef<str>>) -> usize {
        let here = self
            .menu
            .at
            .as_ref()
            .is_some_and(|(text, at)| text == self.draft.text() && *at == self.draft.caret());
        let Some(name) = self.menu.selected.as_deref().filter(|_| here) else {
            return 0;
        };
        names
            .into_iter()
            .position(|n| n.as_ref() == name)
            .unwrap_or(0)
    }

    /// The slash menu, while the draft up to the caret is a leading slash
    /// token and Esc has not closed it there.
    pub fn menu(&self) -> Option<Menu> {
        // A live session lists the commands it runs; the preview, its
        // sample catalog.
        let (draft, caret) = self.menu_here()?;
        let entries = completion::matches(draft, caret, self.catalog())?;
        let selected = self.remembered(entries.iter().map(|c| c.name));
        Some(Menu { entries, selected })
    }

    /// The commands the slash menu lists.
    pub fn catalog(&self) -> &'static [Command] {
        if self.live {
            completion::LIVE_CATALOG
        } else {
            completion::CATALOG
        }
    }

    /// The `@` token the file menu completes, with where it ends, while the
    /// caret is inside one, no slash menu is open, and Esc has not closed
    /// the menu there.
    fn file_token(&self) -> Option<(mention::AtToken, usize)> {
        let (draft, caret) = self.menu_here()?;
        if completion::matches(draft, caret, self.catalog()).is_some() {
            return None;
        }
        let token = mention::active_at_token(draft, caret)?;
        let end = mention::token_end(draft, caret, &token)?;
        Some((token, end))
    }

    /// The file menu for the `@` token at the caret. Until discovery answers
    /// that token's query, it is loading and lists nothing.
    pub fn file_menu(&self) -> Option<FileMenu> {
        let (token, end) = self.file_token()?;
        let answered = self.files.query.as_deref() == Some(token.query.as_str());
        let (entries, loading, error) = match self.files.found.as_ref().filter(|_| answered) {
            Some(Ok(found)) => (
                mention::choices(self.draft.text(), &token, end, found),
                false,
                None,
            ),
            Some(Err(error)) => (Vec::new(), false, Some(error.clone())),
            None => (Vec::new(), true, None),
        };
        let selected = self.remembered(entries.iter().map(|c| c.name.as_str()));
        Some(FileMenu {
            entries,
            selected,
            loading,
            error,
        })
    }

    /// Records whether the frame just drawn showed the displayed approval
    /// request whole. The first such frame starts a showing and returns
    /// its token, for the terminal owner to have the input thread arm
    /// ([`Msg::ApprovalArmed`]); later frames of the same showing return
    /// `None`. A frame that does not show it whole ends the showing: the
    /// request is disarmed and its choice dropped, and the next showing
    /// needs a new arming, and starts without the last one's key notice.
    /// Call once after each frame.
    ///
    /// With no approval waiting, a frame drawn after a dialog opened,
    /// changed, or closed starts a keyboard handoff the same way: its token
    /// is returned for arming, and until then keys and pastes are dropped
    /// ([`Self::handing_off`]).
    pub fn approval_drawn(&mut self) -> Option<u64> {
        let on_screen = self.approval_on_screen.take();
        let dialog_on_screen = self.dialog_on_screen.take();
        let front = self.approval().map(|a| a.generation);
        if front.is_none() {
            self.end_showing();
            return self.dialog_drawn(dialog_on_screen);
        }
        // The request covers any dialog, which is handed over afresh once
        // it shows again.
        if self.dialog.is_some() {
            self.dialog_showing = None;
            self.handoff = None;
        }
        if on_screen != front {
            self.end_showing();
            return None;
        }
        let generation = front?;
        if self
            .approval_showing
            .is_some_and(|(shown, _)| shown == generation)
        {
            return None;
        }
        self.approval_showings += 1;
        self.approval_showing = Some((generation, self.approval_showings));
        // What the last showing's keys said no longer applies.
        if matches!(
            self.notice,
            Some(Notice::ApprovalKeys | Notice::ApprovalChooseFirst)
        ) {
            self.notice = None;
        }
        Some(self.approval_showings)
    }

    /// [`Self::approval_drawn`] with no approval waiting: a dialog drawn
    /// whole that does not hold the keyboard yet, or the composer after a
    /// dialog closed, starts a handoff. A dialog the frame did not draw
    /// whole loses the keyboard.
    fn dialog_drawn(&mut self, on_screen: Option<u64>) -> Option<u64> {
        match self.dialog.as_ref().map(|dialog| dialog.request.id) {
            Some(id) => {
                if on_screen != Some(id) {
                    self.dialog_showing = None;
                    self.handoff = None;
                    return None;
                }
                if self.dialog_showing == Some(id) && !self.handoff_due {
                    return None;
                }
                self.dialog_showing = Some(id);
            }
            None => {
                if !self.handoff_due {
                    return None;
                }
                self.dialog_showing = None;
            }
        }
        self.handoff_due = false;
        self.approval_showings += 1;
        self.handoff = Some(self.approval_showings);
        self.handoff
    }

    /// Whether the composer is waiting for the keyboard after a dialog
    /// closed.
    pub fn handing_off(&self) -> bool {
        self.dialog.is_none() && (self.handoff_due || self.handoff.is_some())
    }

    /// Whether the open dialog holds the keyboard: drawn whole, handed
    /// over, and armed, with no approval waiting.
    pub fn dialog_ready(&self) -> bool {
        self.approvals.is_empty()
            && !self.handoff_due
            && self.handoff.is_none()
            && self
                .dialog
                .as_ref()
                .is_some_and(|dialog| self.dialog_showing == Some(dialog.request.id))
    }

    /// Ends the displayed request's showing: disarmed, with no choice.
    fn end_showing(&mut self) {
        self.approval_showing = None;
        self.approval_armed = None;
        self.approval_chosen = None;
    }

    /// The token of the displayed request's current showing.
    fn showing(&self) -> Option<u64> {
        let front = self.approval()?.generation;
        self.approval_showing
            .filter(|(shown, _)| *shown == front)
            .map(|(_, token)| token)
    }

    /// Whether keys can answer the displayed request: its current showing
    /// is armed ([`Msg::ApprovalArmed`]).
    pub fn approval_armed(&self) -> bool {
        self.showing()
            .is_some_and(|token| self.approval_armed == Some(token))
    }

    /// Whether `y` has chosen to allow the displayed request in its current
    /// showing, so Enter confirms it.
    pub fn approval_chosen(&self) -> bool {
        self.showing()
            .is_some_and(|token| self.approval_chosen == Some(token))
    }

    /// The displayed approval request, if any waits.
    pub fn approval(&self) -> Option<&Approval> {
        self.approvals.first()
    }

    /// Whether a second Ctrl+C would quit now.
    pub fn quitting(&self) -> bool {
        self.quit_until.is_some_and(|until| self.now < until)
    }

    /// The composer's mode: inspection, else what the sample activity
    /// stands for, else idle.
    pub fn mode(&self) -> Mode {
        match (self.focus, self.activity) {
            (Focus::Inspect, _) => Mode::Inspecting,
            (_, Some(sample)) if sample.kind == ActivityKind::Compaction => Mode::Compacting,
            (_, Some(_)) => Mode::Running,
            (_, None) => Mode::Idle,
        }
    }

    /// Whether the row under the composer shows background shells: in a
    /// live session, while any runs.
    pub fn jobs_row(&self) -> bool {
        self.live && self.jobs.iter().any(|job| job.state.is_live())
    }

    /// The background shell the list selects.
    pub fn selected_job(&self) -> Option<&BackgroundJob> {
        jobs::selected(&self.jobs, self.job_selected.as_deref())
    }

    /// The agent the list selects, if any is listed.
    pub fn selected_agent(&self) -> Option<&Agent> {
        agents::selected(&self.agents, Some(&self.selected))
    }

    /// Whether the row under the composer shows agents: the preview's
    /// samples, or a live session's subagents once it has any.
    pub fn agents_row(&self) -> bool {
        !self.agents.is_empty()
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
        let tip = self.tips.and_then(|tips| tips.next_change(self.now));
        // A running shell's clock, where it shows, turns each second.
        let clock = (self.jobs_row() || matches!(self.focus, Focus::Jobs | Focus::Shell))
            .then(|| {
                self.jobs
                    .iter()
                    .filter(|job| job.state.is_live())
                    .map(|job| {
                        let shown = jobs::elapsed(job, self.jobs_at, self.now);
                        Duration::from_secs(1)
                            - Duration::from_nanos(u64::from(shown.subsec_nanos()))
                    })
                    .min()
            })
            .flatten();
        activity
            .into_iter()
            .chain(blink)
            .chain(script)
            .chain(quit)
            .chain(edge)
            .chain(copied)
            .chain(tip)
            .chain(clock)
            .min()
    }

    /// Time since the sample turn started, while its script runs.
    fn live_elapsed(&self) -> Option<Duration> {
        let sample = self.activity.filter(|s| s.kind == ActivityKind::Turn)?;
        self.live_call?;
        Some(self.now.saturating_sub(sample.started))
    }
}

/// Applies one message and returns the effects it requests.
pub fn update(state: &mut State, msg: Msg) -> Vec<Effect> {
    let tick = matches!(msg, Msg::Tick(_));
    let mut effects = apply(state, msg);
    // Only a message can move the caret into or out of an `@` token.
    if !tick && let Some(find) = discover(state) {
        effects.push(find);
    }
    effects
}

/// Asks for the paths of the `@` token at the caret once its query differs
/// from the last asked; leaving the token forgets what was found.
fn discover(state: &mut State) -> Option<Effect> {
    let wanted = state.file_token().map(|(token, _)| token.query);
    if wanted == state.files.query {
        return None;
    }
    state.files = Files {
        query: wanted.clone(),
        found: None,
    };
    wanted.map(Effect::FindFiles)
}

fn apply(state: &mut State, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Key(input) if !state.approvals.is_empty() => return approval_key(state, input, false),
        Msg::KeyRead { key, burst } if !state.approvals.is_empty() => {
            return approval_key(state, key, burst);
        }
        Msg::KeyRepeat(_) if !state.approvals.is_empty() => {}
        // Typed for what had the keyboard before the handoff: dropped, but
        // Ctrl+C keeps its quit gesture.
        Msg::Key(input) | Msg::KeyRepeat(input) | Msg::KeyRead { key: input, .. }
            if state.handing_off() =>
        {
            let bound = keys::action(Scope::Composer, input);
            if let Some(effects) = quit_key(state, bound) {
                return effects;
            }
        }
        Msg::Paste(_) if state.approvals.is_empty() && state.handing_off() => {}
        Msg::Key(input) if state.dialog.is_some() => return dialog_key(state, input, true),
        Msg::KeyRepeat(input) if state.dialog.is_some() => {
            return dialog_key(state, input, false);
        }
        Msg::KeyRead { key, burst } if state.dialog.is_some() => {
            return dialog_key(state, key, !burst);
        }
        Msg::Key(input) | Msg::KeyRepeat(input) | Msg::KeyRead { key: input, .. } => {
            return key(state, input);
        }
        Msg::ApprovalArmed(token) => {
            if state.showing() == Some(token) {
                state.approval_armed = Some(token);
            }
            if state.handoff == Some(token) {
                state.handoff = None;
            }
        }
        // A paste is taken and dropped while an approval waits: pasted text
        // that happens to read `y` is not a decision.
        Msg::Paste(_) if !state.approvals.is_empty() => {
            state.approval_chosen = None;
            state.notice = Some(Notice::ApprovalKeys);
        }
        Msg::Paste(text) if state.dialog.is_some() => {
            if state.dialog_ready()
                && let Some(dialog) = state.dialog.as_mut()
            {
                dialog.paste(&text);
            }
        }
        Msg::Paste(text) => match state.focus {
            // A terminal pastes nothing for a clipboard that holds only an
            // image, and a dropped file arrives as its path; either is read
            // as an image. Other text goes in, a long paste collapsed into a
            // placeholder, as the oracle's does.
            Focus::Composer if text.is_empty() => {
                return vec![Effect::ReadImage(ImageSource::Clipboard)];
            }
            Focus::Composer => {
                if let Some(path) = paste::image_path(&text) {
                    return vec![Effect::ReadImage(ImageSource::File { path, pasted: text })];
                }
                paste_text(state, &text);
            }
            // The list's keys are on the bar; a paste there is ignored, as
            // the oracle ignores one while a sheet is open.
            Focus::AgentList | Focus::Jobs | Focus::Shell => {}
            Focus::Inspect => state.notice = Some(Notice::ReadOnly),
        },
        Msg::Resize { cols, .. } => {
            // What was on screen no longer is: the request must be drawn
            // whole and armed again before a key answers it.
            if !state.approvals.is_empty() {
                state.end_showing();
            }
            if state.dialog.is_some() {
                state.dialog_showing = None;
                state.handoff = None;
            }
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
            if state.tips.is_some() {
                state.show_tip();
            }
        }
        // While an approval waits, the wheel still scrolls the transcript,
        // but no press moves the caret or starts a selection.
        Msg::Mouse(mouse)
            if !state.approvals.is_empty()
                && !matches!(mouse.kind, MouseKind::WheelUp | MouseKind::WheelDown) => {}
        Msg::Mouse(mouse) => return pointer(state, mouse),
        Msg::Copied(ok) => state.copied = Some((ok, state.now + COPIED)),
        Msg::ImageRead { source, image } => staged(state, source, image),
        Msg::Runtime(update) => runtime(state, update),
        Msg::FilesFound { query, found } => {
            if state.files.query.as_deref() == Some(query.as_str()) {
                state.files.found = Some(found);
            }
        }
    }
    Vec::new()
}

/// Applies what the runtime reports. The transcript and the turn change only
/// here, so the screen shows what the runtime holds.
fn runtime(state: &mut State, update: RuntimeUpdate) {
    match update {
        RuntimeUpdate::TurnStarted { seed } => {
            // The tips are for the idle start; the first turn ends them.
            state.end_tips();
            // A sample still running gives way; the runtime's turn is real.
            end_turn(state, Outcome::Completed);
            state.activity = Some(Activity {
                kind: ActivityKind::Turn,
                word: activity::pick(activity::WORDS, &seed),
                started: state.now,
                runtime: true,
            });
            state.compacting_over = None;
            state.phase = None;
            state.summary = None;
        }
        RuntimeUpdate::Phase(phase) => state.phase = Some(phase),
        RuntimeUpdate::Pending(pending) => state.pending = pending,
        RuntimeUpdate::Agents(list) => {
            state.agents = list;
            // An inspected agent no longer listed, as after a new session,
            // returns to the list; an empty list gives the keyboard back to
            // the composer.
            if state.focus == Focus::Inspect
                && !state.agents.iter().any(|agent| agent.id == state.selected)
            {
                state.focus = Focus::AgentList;
            }
            if state.agents.is_empty() && matches!(state.focus, Focus::AgentList | Focus::Inspect) {
                state.focus = Focus::Composer;
            }
        }
        RuntimeUpdate::Jobs(list) => {
            state.jobs = list;
            state.jobs_at = state.now;
            // A shell no longer kept, as after a new session, closes its
            // view; an empty list gives the keyboard back to the composer.
            if state
                .job_selected
                .as_ref()
                .is_some_and(|id| !state.jobs.iter().any(|job| &job.id == id))
            {
                state.job_selected = None;
                if state.focus == Focus::Shell {
                    state.focus = Focus::Jobs;
                }
            }
            if state.jobs.is_empty() && matches!(state.focus, Focus::Jobs | Focus::Shell) {
                state.focus = Focus::Composer;
            }
        }
        RuntimeUpdate::Live(rows) => state.transcript.set_live(rows),
        RuntimeUpdate::Commit(rows) => state.transcript.commit(rows),
        RuntimeUpdate::Replaced(rows) => {
            // What the old session's view held goes with it: its rows, the
            // selection over them, its turn, and its pending prompts. The
            // draft, its staged images, and any dialog stay.
            state.tips = None;
            state.transcript = Transcript::new(rows);
            state.transcript_epoch = state.transcript_epoch.wrapping_add(1);
            state.transcript_published = 0;
            state.selecting.clear();
            state.dragging = false;
            state.track = None;
            state.latest = None;
            state.transcript_area = None;
            state.live_call = None;
            // `/compact` rebuilds the transcript before it ends: the
            // compaction that is the work itself keeps the bar until its
            // [`RuntimeUpdate::TurnEnded`].
            let compacting = state.activity.filter(|work| {
                work.runtime
                    && work.kind == ActivityKind::Compaction
                    && state.compacting_over.is_none()
            });
            if compacting.is_none() {
                state.phase = None;
            }
            state.activity = compacting;
            state.compacting_over = None;
            state.summary = None;
            state.pending.clear();
        }
        RuntimeUpdate::Admitted(id) => state.unadmitted.retain(|kept| kept.id != id),
        RuntimeUpdate::Refused { id, reason } => {
            if let Some(index) = state.unadmitted.iter().position(|kept| kept.id == id) {
                let kept = state.unadmitted.remove(index);
                state.draft.restore(&kept.draft);
                state.attachments.extend(kept.attachments);
                state.attachments.sort_by_key(|(id, _)| *id);
            }
            state.refusal = Some(reason);
            state.notice = Some(Notice::Refused);
        }
        RuntimeUpdate::Returned(texts) => {
            // Pi's `restoreQueuedMessagesToEditor`: the returned prompts,
            // then the draft, each non-blank part a paragraph.
            if prepend_to_draft(state, &texts) {
                state.notice = Some(Notice::Returned);
            }
        }
        RuntimeUpdate::Declined { texts, reason } => {
            prepend_to_draft(state, &texts);
            state.refusal = Some(reason);
            state.notice = Some(Notice::Refused);
        }
        RuntimeUpdate::Restored(text) => {
            if prepend_to_draft(state, std::slice::from_ref(&text)) {
                state.notice = Some(Notice::Restored);
            }
        }
        RuntimeUpdate::Status(status) => {
            state.status.model = status.model;
            state.status.thinking = status.thinking;
            state.status.context = status.context;
            state.status.tokens = status.tokens;
            state.status.cache = status.cache;
        }
        RuntimeUpdate::Approvals(approvals) => {
            let front = approvals.first().map(|a| a.generation);
            // A newly displayed request starts unarmed, unchosen, and at
            // the top of its detail.
            if state.approval().map(|a| a.generation) != front {
                state.end_showing();
                state.approval_scroll = 0;
            }
            if state
                .approval_answered
                .is_some_and(|answered| !approvals.iter().any(|a| a.generation == answered))
            {
                state.approval_answered = None;
            }
            if front.is_some() {
                state.selecting.clear();
                state.dragging = false;
            } else if matches!(
                state.notice,
                Some(Notice::ApprovalKeys | Notice::ApprovalChooseFirst)
            ) {
                state.notice = None;
            }
            state.approvals = approvals;
        }
        RuntimeUpdate::Dialog(request) => {
            state.handoff_due = true;
            state.handoff = None;
            state.dialog_showing = None;
            state.dialog = request.map(Dialog::open);
            if state.dialog.is_some() {
                state.selecting.clear();
                state.dragging = false;
            }
        }
        RuntimeUpdate::TurnEnded(outcome) => {
            // A compaction that interrupted the turn ends with it, and the
            // turn is what ended; a compaction that was the work, as
            // `/compact` is, ends as a compaction.
            let ended = state.compacting_over.take().or(state.activity);
            if let Some(work) = ended.filter(|a| a.runtime) {
                state.summary = Some(TurnSummary {
                    outcome,
                    elapsed: state.now.saturating_sub(work.started),
                    kind: work.kind,
                });
                state.activity = None;
            }
            state.phase = None;
        }
        RuntimeUpdate::CompactionStarted { interrupts } => {
            state.end_tips();
            let current = state.activity.filter(|a| a.runtime);
            // A repeated start, or a sample, leaves the work as it is.
            if current.is_some_and(|a| a.kind == ActivityKind::Compaction) {
                return;
            }
            state.compacting_over = current.filter(|_| interrupts);
            state.activity = Some(Activity {
                kind: ActivityKind::Compaction,
                word: crate::copy::COMPACTING,
                started: if interrupts {
                    state.now
                } else {
                    current.map_or(state.now, |a| a.started)
                },
                runtime: true,
            });
            state.phase = None;
            state.summary = None;
        }
        RuntimeUpdate::CompactionEnded => {
            if state
                .activity
                .is_some_and(|a| a.runtime && a.kind == ActivityKind::Compaction)
                && let Some(turn) = state.compacting_over.take()
            {
                state.activity = Some(turn);
                state.phase = None;
            }
        }
    }
}

/// Puts `texts`, then the draft, into the draft, each non-blank part a
/// paragraph. Whether the draft changed.
fn prepend_to_draft(state: &mut State, texts: &[String]) -> bool {
    let current = state.draft.text().to_owned();
    let combined = texts
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(current.as_str()))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if combined == current {
        return false;
    }
    state.draft.replace(&combined, combined.len());
    true
}

/// Whether the runtime is running a turn.
fn turn_running(state: &State) -> bool {
    state.activity.is_some_and(|a| a.runtime)
}

/// What Enter does with the draft: sends it to the runtime, as a follow-up
/// while idle and a steer while a turn runs, and clears it. A blank draft is
/// left alone, and a slash command, the Ctrl+T sample, or staged images keep
/// the draft and say why.
fn submit(state: &mut State) -> Vec<Effect> {
    let text = state.draft.expanded();
    if text.trim().is_empty() {
        return Vec::new();
    }
    let running = turn_running(state);
    let refused = if text.starts_with('/') && !state.live {
        Some(Notice::NoCommands)
    } else if state.activity.is_some_and(|a| !a.runtime) {
        Some(Notice::SampleOnly)
    } else if !state.attachments.is_empty() && !state.live {
        Some(Notice::NoImages)
    } else if !state.attachments.is_empty() && running {
        Some(Notice::ImagesWhileRunning)
    } else {
        None
    };
    if let Some(notice) = refused {
        state.notice = Some(notice);
        return Vec::new();
    }
    state.notice = None;
    state.submitted += 1;
    let id = state.submitted;
    // A live runtime may refuse the prompt; until it admits it, the draft
    // and its images are kept to put back.
    if state.live {
        state.unadmitted.push(Unadmitted {
            id,
            draft: state.draft.clone(),
            attachments: state.attachments.clone(),
        });
    }
    // The images go in the order their placeholders appear in the draft.
    let placed = state.draft.attached();
    let mut images: Vec<_> = std::mem::take(&mut state.attachments);
    images.sort_by_key(|(id, _)| placed.iter().position(|p| p == id).unwrap_or(usize::MAX));
    state.draft.replace("", 0);
    let submission = if running {
        Submission::Steer(text)
    } else {
        Submission::FollowUp(text)
    };
    vec![Effect::Submit(Prompt {
        id,
        submission,
        images: images.into_iter().map(|(_, image)| image).collect(),
    })]
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
        // The transcript stays on screen above an open tray, and the wheel
        // over it scrolls it there too.
        let over_transcript = state
            .transcript_area
            .is_some_and(|area| area.holds(mouse.column, mouse.row));
        match state.focus {
            _ if over_transcript && direction < 0 => state.transcript.scroll_up(rows),
            _ if over_transcript => state.transcript.scroll_down(rows),
            Focus::Composer if direction < 0 => state.transcript.scroll_up(rows),
            Focus::Composer => state.transcript.scroll_down(rows),
            Focus::AgentList => step_selection(state, isize::from(direction)),
            Focus::Inspect => {}
            Focus::Jobs => step_job(state, isize::from(direction)),
            Focus::Shell => scroll_shell(state, -isize::from(direction) * rows as isize),
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
fn on_screen(transcript: &Transcript, published: usize) -> Vec<(transcript::Anchor, String)> {
    transcript
        .screen()
        .into_iter()
        .filter(|(at, _)| !transcript.following() || at.row >= published)
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
    let lines = on_screen(&state.transcript, state.transcript_published);
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
    let anchors: Vec<_> = on_screen(&state.transcript, state.transcript_published)
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

/// A first Ctrl+C arms the quit and a second within the window quits, so a
/// stray press never ends the session. Returns `None` for any other key,
/// which disarms it.
/// A key while a dialog is open and no approval waits: Ctrl+C keeps its
/// quit gesture, and every other key goes to the dialog, never to the
/// draft. `fresh` as [`Dialog::key`] takes it.
fn dialog_key(state: &mut State, input: KeyInput, fresh: bool) -> Vec<Effect> {
    let bound = keys::action(Scope::Composer, input);
    if let Some(effects) = quit_key(state, bound) {
        return effects;
    }
    // Until the dialog holds the keyboard, only Esc reaches it, to cancel.
    let cancel = input.key == Key::Esc && input.mods == Mods::NONE;
    if !state.dialog_ready() && !cancel {
        return Vec::new();
    }
    let Some(dialog) = state.dialog.as_mut() else {
        return Vec::new();
    };
    let id = dialog.request.id;
    dialog
        .key(input, fresh)
        .map(|answer| vec![Effect::Dialog { id, answer }])
        .unwrap_or_default()
}

fn quit_key(state: &mut State, bound: Option<Action>) -> Option<Vec<Effect>> {
    if bound == Some(Action::Quit) {
        if state.quitting() {
            return Some(vec![Effect::Quit]);
        }
        state.quit_until = Some(state.now + QUIT_WINDOW);
        return Some(Vec::new());
    }
    state.quit_until = None;
    None
}

/// A key while an approval request waits; `burst` when it was read in one
/// sweep with other keys.
///
/// As the TypeScript `ApprovalView`, `n` rejects the call, and Esc
/// withdraws the request, as the TypeScript controller's cancel does. To
/// allow, `y` chooses and Enter confirms: a terminal that repeats a held
/// key as fresh presses can then repeat `y` into no grant, and Enter alone
/// never allows. Only keys read alone after the input thread armed the
/// request ([`Msg::ApprovalArmed`]) choose, confirm, or reject, so a key
/// typed before the prompt appeared, still waiting, or one inside a burst
/// of typed or pasted text decides nothing; it says how to answer instead.
/// The arrow and page keys scroll the request's detail. Nothing touches
/// the draft, its caret, or its staged images, and Ctrl+C keeps its quit
/// gesture.
fn approval_key(state: &mut State, input: KeyInput, burst: bool) -> Vec<Effect> {
    let bound = keys::action(state.focus.scope(), input);
    if let Some(effects) = quit_key(state, bound) {
        return effects;
    }
    let Some(generation) = state.approval().map(|a| a.generation) else {
        return Vec::new();
    };
    if state.approval_answered == Some(generation) {
        return Vec::new();
    }
    let page = state.approval_page.max(1);
    let scroll = match input.key {
        Key::Up => Some(-1),
        Key::Down => Some(1),
        Key::PageUp => Some(-(page as isize)),
        Key::PageDown => Some(page as isize),
        _ => None,
    };
    if let Some(delta) = scroll.filter(|_| input.mods == Mods::NONE) {
        // Render clamps the far end to the detail's length.
        state.approval_scroll = state.approval_scroll.saturating_add_signed(delta);
        return Vec::new();
    }
    let plain = input.mods == Mods::NONE || input.mods == Mods::SHIFT;
    let showing = state.showing();
    let ready = plain && !burst && showing.is_some() && state.approval_armed == showing;
    let chosen = showing.is_some() && state.approval_chosen == showing;
    let answer = match input.key {
        Key::Esc if input.mods == Mods::NONE => ApprovalAnswer::Cancel,
        Key::Char('n' | 'N') if ready => ApprovalAnswer::Reject,
        Key::Enter if ready && chosen && input.mods == Mods::NONE => ApprovalAnswer::AllowOnce,
        Key::Char('y' | 'Y') if ready => {
            state.approval_chosen = showing;
            state.notice = None;
            return Vec::new();
        }
        Key::Enter if ready && input.mods == Mods::NONE => {
            state.notice = Some(Notice::ApprovalChooseFirst);
            return Vec::new();
        }
        _ => {
            state.approval_chosen = None;
            state.notice = Some(Notice::ApprovalKeys);
            return Vec::new();
        }
    };
    state.notice = None;
    state.approval_chosen = None;
    state.approval_answered = Some(generation);
    vec![Effect::Answer { generation, answer }]
}

fn key(state: &mut State, input: KeyInput) -> Vec<Effect> {
    let bound = keys::action(state.focus.scope(), input);
    if let Some(effects) = quit_key(state, bound) {
        return effects;
    }
    match state.focus {
        Focus::Composer if bound == Some(Action::PasteImage) => {
            return vec![Effect::ReadImage(ImageSource::Clipboard)];
        }
        Focus::Composer => {
            let effects = composer_key(state, bound, input);
            unstage(state);
            return effects;
        }
        Focus::AgentList => return list_key(state, bound),
        Focus::Inspect => inspect_key(state, bound),
        Focus::Jobs | Focus::Shell => return jobs_key(state, bound),
    }
    Vec::new()
}

/// Up, Down, Tab, and Enter while the slash menu is open. Returns `false`
/// when the key is the composer's: without a menu, or Enter on a bare `/`
/// with nothing to choose, which the composer submits.
fn menu_key(state: &mut State, bound: Option<Action>) -> bool {
    let Some(menu) = state.menu() else {
        return false;
    };
    let choice = menu.entries.get(menu.selected).copied();
    match bound {
        Some(Action::CaretUp | Action::CaretDown) => {
            let count = menu.entries.len();
            if count > 0 {
                let step = if bound == Some(Action::CaretUp) {
                    count - 1
                } else {
                    1
                };
                let next = menu.entries[(menu.selected + step) % count].name;
                state.menu = MenuMemory {
                    at: Some((state.draft.text().to_owned(), state.draft.caret())),
                    selected: Some(next.to_owned()),
                    dismissed: false,
                };
            }
            true
        }
        Some(Action::Complete) => {
            if let Some(choice) = choice {
                let (text, caret) = completion::complete(state.draft.text(), &choice);
                state.draft.replace(&text, caret);
            }
            true
        }
        // Enter takes the selected command: one that needs an argument is
        // filled in to type it, and any other is submitted as its name.
        Some(Action::Submit) => {
            let Some(choice) = choice else {
                return state.draft.text() == "/";
            };
            let (text, caret) = completion::complete(state.draft.text(), &choice);
            if completion::requires_input(&choice) {
                state.draft.replace(&text, caret);
                return true;
            }
            let name = text.trim_end();
            if state.draft.text() != name {
                state.draft.replace(name, name.len());
            }
            // A live session runs it: the composer submits the name.
            if state.live {
                return false;
            }
            state.notice = Some(Notice::NoCommands);
            true
        }
        _ => false,
    }
}

/// Up, Down, and Tab while the file menu is open, as the oracle's composer
/// takes them: the arrows cycle the selection and Tab puts in the selected
/// mention; with nothing listed yet they do nothing. Enter is not the
/// menu's, so it sends the draft as typed. Returns `false` without a menu.
fn file_key(state: &mut State, bound: Option<Action>) -> bool {
    let Some(menu) = state.file_menu() else {
        return false;
    };
    let count = menu.entries.len();
    if count == 0 {
        return true;
    }
    if bound == Some(Action::Complete) {
        let choice = &menu.entries[menu.selected];
        state.draft.replace(&choice.draft, choice.caret);
        return true;
    }
    let step = if bound == Some(Action::CaretUp) {
        count - 1
    } else {
        1
    };
    state.menu = MenuMemory {
        at: Some((state.draft.text().to_owned(), state.draft.caret())),
        selected: Some(menu.entries[(menu.selected + step) % count].name.clone()),
        dismissed: false,
    };
    true
}

/// Inserts pasted text at the caret, a long paste as a placeholder.
fn paste_text(state: &mut State, text: &str) {
    let complete = state.draft.paste_block(text);
    state.notice = (!complete).then_some(Notice::DraftLimit);
}

/// Stages an image read for the draft, its placeholder at the caret. A file
/// that did not read as an image goes in as the text that named it; an
/// empty clipboard says so.
fn staged(state: &mut State, source: ImageSource, image: Option<Image>) {
    if state.focus != Focus::Composer {
        return;
    }
    match (image, source) {
        (Some(image), _) => {
            state.images += 1;
            state.draft.attach(state.images);
            state.attachments.push((state.images, image));
            state.notice = None;
        }
        (None, ImageSource::File { pasted, .. }) => paste_text(state, &pasted),
        (None, ImageSource::Clipboard) => state.notice = Some(Notice::NoClipboardImage),
    }
}

/// Unstages the images whose placeholders an edit removed.
fn unstage(state: &mut State) {
    let gone = state.draft.sweep();
    state.attachments.retain(|(id, _)| !gone.contains(id));
}

fn composer_key(state: &mut State, bound: Option<Action>, input: KeyInput) -> Vec<Effect> {
    // Esc drops a selection, then closes the menu until the draft or the
    // caret changes, before it stops anything.
    if bound == Some(Action::Interrupt) {
        if state.selecting.clear() {
            return Vec::new();
        }
        if state.menu().is_some() || state.file_menu().is_some() {
            state.menu = MenuMemory {
                at: Some((state.draft.text().to_owned(), state.draft.caret())),
                selected: None,
                dismissed: true,
            };
            return Vec::new();
        }
    }
    if matches!(
        bound,
        Some(Action::CaretUp | Action::CaretDown | Action::Complete | Action::Submit)
    ) && menu_key(state, bound)
    {
        return Vec::new();
    }
    if matches!(
        bound,
        Some(Action::CaretUp | Action::CaretDown | Action::Complete)
    ) && file_key(state, bound)
    {
        return Vec::new();
    }
    let running = turn_running(state);
    let shells = state.jobs_row();
    let draft = &mut state.draft;
    let complete = match bound {
        Some(Action::ToggleAgentList) if state.agents.is_empty() => {
            state.notice = Some(Notice::NoAgents);
            return Vec::new();
        }
        Some(Action::ToggleAgentList) => {
            state.focus = Focus::AgentList;
            state.notice = None;
            return Vec::new();
        }
        // Without a menu, Tab types a tab, as the oracle's composer does.
        Some(Action::Complete) => draft.type_text("\t"),
        // Esc asks the runtime to stop its turn, which ends when it says so.
        Some(Action::Interrupt) if running => {
            state.notice = None;
            return vec![Effect::Cancel];
        }
        // Esc stops a sample the way it interrupts a turn or cancels compaction.
        Some(Action::Interrupt) => {
            end_turn(state, Outcome::Interrupted);
            state.activity = None;
            state.notice = None;
            return Vec::new();
        }
        // The sample stands in for a turn, so it waits while a real one runs.
        Some(Action::ToggleSampleActivity) if state.live => {
            state.notice = Some(Notice::PreviewOnly);
            return Vec::new();
        }
        Some(Action::ToggleSampleActivity) => {
            if !running {
                toggle_activity(state);
            }
            return Vec::new();
        }
        Some(Action::Submit) => return submit(state),
        // Shift+Tab: a live session's next thinking level. The status line
        // shows it once the runtime has applied it.
        Some(Action::CycleThinking) => {
            return if state.live {
                vec![Effect::CycleThinking]
            } else {
                Vec::new()
            };
        }
        // Alt+↑ hands every waiting prompt to the runtime at once; the
        // panel empties when the runtime reports it.
        Some(Action::SendPending) => {
            if state.pending.is_empty() {
                state.notice = Some(Notice::NoPending);
                return Vec::new();
            }
            state.notice = Some(Notice::PendingSent);
            return vec![Effect::SendPending];
        }
        Some(Action::PageUp) => {
            state.transcript.scroll_up(state.transcript.page());
            return Vec::new();
        }
        Some(Action::PageDown) => {
            state.transcript.scroll_down(state.transcript.page());
            return Vec::new();
        }
        Some(Action::PreviousPrompt) => {
            state.transcript.previous_prompt();
            return Vec::new();
        }
        Some(Action::NextPrompt) => {
            state.transcript.next_prompt();
            return Vec::new();
        }
        Some(Action::ToStart) => {
            state.transcript.to_start();
            return Vec::new();
        }
        Some(Action::ToLatest) => {
            state.transcript.follow();
            return Vec::new();
        }
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
        // Down with nothing newer to show moves to the background shells'
        // row while any runs.
        Some(Action::CaretUp | Action::CaretDown) => {
            let up = bound == Some(Action::CaretUp);
            let width = state.window.width();
            if !draft.vertical(width, up)
                && !draft.recall(&input_history(&state.transcript), up, width)
                && !up
                && shells
            {
                state.focus = Focus::Jobs;
                state.job_selected = None;
                state.notice = None;
                return Vec::new();
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
            None => return Vec::new(),
        },
    };
    state.notice = (!complete).then_some(Notice::DraftLimit);
    Vec::new()
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

fn list_key(state: &mut State, bound: Option<Action>) -> Vec<Effect> {
    match bound {
        Some(Action::SelectPrevious) => step_selection(state, -1),
        Some(Action::SelectNext) => step_selection(state, 1),
        Some(Action::InspectSelected) => {
            if let Some(id) = state.selected_agent().map(|agent| agent.id.clone()) {
                state.selected = id;
                state.focus = Focus::Inspect;
            }
            state.notice = None;
        }
        Some(Action::ReturnToComposer) => {
            state.focus = Focus::Composer;
            state.notice = None;
        }
        // Only a working subagent's run can be stopped; the runtime says
        // how it ended.
        Some(Action::StopAgent) => {
            if let Some(agent) = state
                .selected_agent()
                .filter(|agent| agent.state == AgentState::Working)
            {
                return vec![Effect::StopAgent(agent.id.clone())];
            }
        }
        // Other keys do nothing: the bar already names the list's keys.
        _ => {}
    }
    Vec::new()
}

/// Keys in the background shells' list and in one shell.
fn jobs_key(state: &mut State, bound: Option<Action>) -> Vec<Effect> {
    let shell = state.focus == Focus::Shell;
    match bound {
        Some(Action::SelectPrevious) if shell => scroll_shell(state, 1),
        Some(Action::SelectNext) if shell => scroll_shell(state, -1),
        Some(Action::PageUp) => scroll_shell(state, shell_page(state)),
        Some(Action::PageDown) => scroll_shell(state, -shell_page(state)),
        Some(Action::ToLatest) => state.shell_scroll = 0,
        Some(Action::SelectPrevious) => step_job(state, -1),
        Some(Action::SelectNext) => step_job(state, 1),
        Some(Action::OpenJob) if state.selected_job().is_some() => {
            state.job_selected = state.selected_job().map(|job| job.id.clone());
            state.shell_scroll = 0;
            state.focus = Focus::Shell;
        }
        Some(Action::StopJob) => {
            if let Some(job) = state
                .selected_job()
                .filter(|job| job.state == jobs::JobState::Running)
            {
                return vec![Effect::StopJob(job.id.clone())];
            }
        }
        Some(Action::ReturnToJobs) => state.focus = Focus::Jobs,
        Some(Action::ReturnToComposer) => state.focus = Focus::Composer,
        // Other keys do nothing: the bar names the keys that work here.
        _ => {}
    }
    Vec::new()
}

/// Rows a page of the shell view moves: its output rows less one.
fn shell_page(state: &State) -> isize {
    (state.transcript.page().saturating_sub(4)).max(1) as isize
}

/// Scrolls the shell view `by` rows toward its output's start, within
/// what the last frame could show.
fn scroll_shell(state: &mut State, by: isize) {
    state.shell_scroll = state
        .shell_scroll
        .saturating_add_signed(by)
        .min(state.shell_most);
}

/// Moves the shells' selection `by` rows in list order, newest first,
/// stopping at the ends.
fn step_job(state: &mut State, by: isize) {
    let order: Vec<&str> = jobs::shown(&state.jobs)
        .map(|job| job.id.as_str())
        .collect();
    let Some(current) = state
        .selected_job()
        .and_then(|job| order.iter().position(|id| *id == job.id))
    else {
        return;
    };
    let next = current.saturating_add_signed(by).min(order.len() - 1);
    state.job_selected = Some(order[next].to_owned());
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
            state.transcript.commit(vec![live::at(Duration::ZERO)]);
            Some(Activity {
                kind: ActivityKind::Turn,
                word: activity::pick(activity::WORDS, &format!("sample-{}", state.samples)),
                started: state.now,
                runtime: false,
            })
        }
        Some(ActivityKind::Turn) => Some(Activity {
            kind: ActivityKind::Compaction,
            word: crate::copy::COMPACTING,
            started: state.now,
            runtime: false,
        }),
        Some(ActivityKind::Compaction) => None,
    };
}

/// Records how a running sample turn ended; anything else is left alone.
fn end_turn(state: &mut State, outcome: Outcome) {
    if let Some(sample) = state.activity
        && sample.kind == ActivityKind::Turn
    {
        state.summary = Some(TurnSummary {
            outcome,
            elapsed: state.now.saturating_sub(sample.started),
            kind: ActivityKind::Turn,
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
        Outcome::Interrupted | Outcome::Failed => live::Settle::Interrupted,
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

/// Moves the selection by identity, so a reordered list keeps the same
/// agent, stopping at the ends.
fn step_selection(state: &mut State, delta: isize) {
    let Some(last) = state.agents.len().checked_sub(1) else {
        return;
    };
    let index = state
        .agents
        .iter()
        .position(|a| a.id == state.selected)
        .unwrap_or(0);
    let next = index.saturating_add_signed(delta).min(last);
    state.selected = state.agents[next].id.clone();
    state.notice = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{Key, Mods};
    use crate::paste::Image;
    use crate::runtime::{RuntimeUpdate, Submission};
    use crate::transcript::{CallState, Row};

    /// The submissions among `effects`.
    fn submitted(effects: Vec<Effect>) -> Vec<Submission> {
        effects
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::Submit(prompt) => Some(prompt.submission),
                _ => None,
            })
            .collect()
    }

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

    fn runtime(state: &mut State, update_: RuntimeUpdate) {
        assert_eq!(update(state, Msg::Runtime(update_)), []);
    }

    #[test]
    fn enter_sends_the_draft_as_a_follow_up_and_clears_it() {
        let mut state = State::default();
        let rows = state.transcript.rows.len();
        type_str(&mut state, "hello");
        press(&mut state, Key::Left);
        assert_eq!(
            submitted(press(&mut state, Key::Enter)),
            [Submission::FollowUp("hello".into())]
        );
        assert_eq!((state.draft.text(), state.notice), ("", None));
        // The prompt shows only once the runtime commits it.
        assert_eq!(state.transcript.rows.len(), rows);
        // Undo brings a sent draft back, as the oracle's does.
        chord(&mut state, Key::Char('-'), Mods::CTRL);
        assert_eq!(state.draft.text(), "hello");
    }

    fn live() -> State {
        State::live(FrameStyle::default(), Tones::default(), Vec::new())
    }

    fn shell_job(id: &str, state: crate::jobs::JobState) -> BackgroundJob {
        BackgroundJob {
            id: id.into(),
            command: format!("run {id}"),
            state,
            elapsed: Duration::from_secs(1),
            output: "out\n".into(),
            omitted_bytes: 0,
        }
    }

    // Down with nothing newer to recall reaches the background shells'
    // list while one runs; Enter opens one, `x` asks to stop a running one,
    // and Esc steps back out. Up keeps its history meaning.
    #[test]
    fn down_opens_the_background_shells_and_their_keys_work() {
        use crate::jobs::JobState;
        let mut state = live();
        press(&mut state, Key::Down);
        assert_eq!(state.focus, Focus::Composer, "no shells, no list");
        runtime(
            &mut state,
            RuntimeUpdate::Jobs(vec![
                shell_job("bash-1", JobState::Completed(Some("exit code: 0".into()))),
                shell_job("bash-2", JobState::Running),
            ]),
        );
        assert!(state.jobs_row());
        type_str(&mut state, "draft");
        press(&mut state, Key::Down);
        assert_eq!(state.focus, Focus::Jobs);
        assert_eq!(state.draft.text(), "draft", "the draft stays");
        assert_eq!(
            state.selected_job().map(|job| job.id.as_str()),
            Some("bash-2")
        );
        assert_eq!(
            press(&mut state, Key::Char('x')),
            [Effect::StopJob("bash-2".into())]
        );
        press(&mut state, Key::Down);
        assert_eq!(
            state.selected_job().map(|job| job.id.as_str()),
            Some("bash-1")
        );
        press(&mut state, Key::Down);
        assert_eq!(
            state.selected_job().map(|job| job.id.as_str()),
            Some("bash-1"),
            "stops at the end"
        );
        assert_eq!(
            press(&mut state, Key::Char('x')),
            [],
            "a finished shell has nothing to stop"
        );
        press(&mut state, Key::Enter);
        assert_eq!(state.focus, Focus::Shell);
        assert_eq!(
            press(&mut state, Key::Char('q')),
            [],
            "typing reaches nothing"
        );
        press(&mut state, Key::Esc);
        assert_eq!(state.focus, Focus::Jobs);
        press(&mut state, Key::Esc);
        assert_eq!(state.focus, Focus::Composer);
        assert_eq!(state.draft.text(), "draft");
    }

    // A shell that is gone closes its view, and no shells at all, as after
    // a new session, hand the keyboard back; finished shells hide the row.
    #[test]
    fn reported_shells_steer_the_focus() {
        use crate::jobs::JobState;
        let mut state = live();
        runtime(
            &mut state,
            RuntimeUpdate::Jobs(vec![shell_job("bash-1", JobState::Running)]),
        );
        press(&mut state, Key::Down);
        press(&mut state, Key::Enter);
        assert_eq!(state.focus, Focus::Shell);
        runtime(
            &mut state,
            RuntimeUpdate::Jobs(vec![shell_job("bash-2", JobState::Running)]),
        );
        assert_eq!(state.focus, Focus::Jobs);
        runtime(
            &mut state,
            RuntimeUpdate::Jobs(vec![shell_job("bash-2", JobState::Killed(None))]),
        );
        assert!(!state.jobs_row());
        assert_eq!(
            state.focus,
            Focus::Jobs,
            "the open list keeps finished shells"
        );
        runtime(&mut state, RuntimeUpdate::Jobs(Vec::new()));
        assert_eq!(state.focus, Focus::Composer);
        // The preview has no shells row.
        let mut preview = State::default();
        runtime(
            &mut preview,
            RuntimeUpdate::Jobs(vec![shell_job("bash-1", JobState::Running)]),
        );
        assert!(!preview.jobs_row());
    }

    // A running shell's clock turns the screen each second; none, no wake.
    #[test]
    fn a_running_shell_wakes_the_screen_each_second() {
        use crate::jobs::JobState;
        let mut state = live();
        assert_eq!(state.next_change(), None);
        update(&mut state, Msg::Tick(Duration::from_millis(400)));
        runtime(
            &mut state,
            RuntimeUpdate::Jobs(vec![shell_job("bash-1", JobState::Running)]),
        );
        update(&mut state, Msg::Tick(Duration::from_millis(700)));
        assert_eq!(state.next_change(), Some(Duration::from_millis(700)));
        runtime(
            &mut state,
            RuntimeUpdate::Jobs(vec![shell_job("bash-1", JobState::Completed(None))]),
        );
        assert_eq!(state.next_change(), None);
    }

    fn live_image(state: &mut State, name: &str) {
        let image = Image {
            name: name.into(),
            media_type: "image/png",
            bytes: 3,
            size: None,
            data: crate::paste::ImageData::new(name.as_bytes().to_vec()),
        };
        update(
            state,
            Msg::ImageRead {
                source: ImageSource::Clipboard,
                image: Some(image),
            },
        );
    }

    #[test]
    fn a_live_session_opens_on_its_own_rows_without_the_samples() {
        let state = State::live(
            FrameStyle::default(),
            Tones::default(),
            vec![Row::Info("Session s".into())],
        );
        assert!(state.live);
        assert_eq!(state.transcript.rows, [Row::Info("Session s".into())]);
    }

    #[test]
    fn a_live_prompt_sends_its_images_in_placeholder_order() {
        let mut state = live();
        type_str(&mut state, "a ");
        live_image(&mut state, "first.png");
        type_str(&mut state, " b ");
        live_image(&mut state, "second.png");
        let effects = press(&mut state, Key::Enter);
        let [Effect::Submit(prompt)] = &effects[..] else {
            panic!("{effects:?}");
        };
        assert_eq!(
            prompt.submission,
            Submission::FollowUp("a [Image #1] b [Image #2]".into())
        );
        let names: Vec<_> = prompt.images.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["first.png", "second.png"]);
        assert!(state.attachments.is_empty());
        assert_eq!(state.draft.text(), "");
    }

    #[test]
    fn a_live_turn_refuses_images_and_keeps_the_draft_and_steers_text() {
        let mut state = live();
        runtime(&mut state, RuntimeUpdate::TurnStarted { seed: "s".into() });
        type_str(&mut state, "look ");
        live_image(&mut state, "shot.png");
        assert_eq!(press(&mut state, Key::Enter), []);
        assert_eq!(state.notice, Some(Notice::ImagesWhileRunning));
        assert_eq!(state.draft.text(), "look [Image #1]");
        assert_eq!(state.attachments.len(), 1);
        // Without the image, Enter steers.
        press(&mut state, Key::Backspace);
        assert!(state.attachments.is_empty());
        assert_eq!(
            submitted(press(&mut state, Key::Enter)),
            [Submission::Steer("look ".into())]
        );
    }

    #[test]
    fn live_slash_prompts_reach_the_runtime_and_preview_keys_are_refused() {
        let mut state = live();
        type_str(&mut state, "/skill:review now");
        assert_eq!(state.menu(), None);
        assert_eq!(
            submitted(press(&mut state, Key::Enter)),
            [Submission::FollowUp("/skill:review now".into())]
        );
        assert_eq!(chord(&mut state, Key::Char('t'), Mods::CTRL), []);
        assert_eq!(
            (state.activity, state.notice),
            (None, Some(Notice::PreviewOnly))
        );
        assert_eq!(chord(&mut state, Key::Char('g'), Mods::CTRL), []);
        assert_eq!(
            (state.focus, state.notice),
            (Focus::Composer, Some(Notice::NoAgents))
        );
    }

    fn subagent(id: &str, state: AgentState) -> Agent {
        Agent {
            id: id.into(),
            name: format!("child {id}"),
            state,
            route: Some("faux/faux-1".into()),
            detail: None,
            file: None,
        }
    }

    // A live session lists the subagents its runtime reports: `x` asks to
    // stop the selected one only while it works, and a list that drops the
    // inspected agent, or empties, gives the keyboard back.
    #[test]
    fn live_subagents_open_from_ctrl_g_and_stop_only_while_working() {
        let mut state = live();
        assert!(!state.agents_row());
        runtime(
            &mut state,
            RuntimeUpdate::Agents(vec![
                subagent("a", AgentState::Working),
                subagent("b", AgentState::Idle(Some(agents::Ended::Completed))),
                subagent("c", AgentState::Starting),
            ]),
        );
        assert!(state.agents_row());
        chord(&mut state, Key::Char('g'), Mods::CTRL);
        assert_eq!(state.focus, Focus::AgentList);
        assert_eq!(
            press(&mut state, Key::Char('x')),
            [Effect::StopAgent("a".into())]
        );
        press(&mut state, Key::Down);
        assert_eq!(state.selected, "b");
        assert_eq!(press(&mut state, Key::Char('x')), []);
        // A child still starting has no run to stop yet.
        press(&mut state, Key::Down);
        assert_eq!(state.selected, "c");
        assert_eq!(press(&mut state, Key::Char('x')), []);
        press(&mut state, Key::Up);
        press(&mut state, Key::Enter);
        assert_eq!(state.focus, Focus::Inspect);
        assert_eq!(
            state.selected_agent().map(|a| a.name.as_str()),
            Some("child b")
        );
        // Inspection is read only: `x` is a key like any other there.
        assert_eq!(press(&mut state, Key::Char('x')), []);
        assert_eq!(state.notice, Some(Notice::ReadOnly));
        runtime(
            &mut state,
            RuntimeUpdate::Agents(vec![subagent("a", AgentState::Working)]),
        );
        assert_eq!(state.focus, Focus::AgentList);
        assert_eq!(state.selected_agent().map(|a| a.id.as_str()), Some("a"));
        runtime(&mut state, RuntimeUpdate::Agents(Vec::new()));
        assert_eq!(state.focus, Focus::Composer);
        assert!(!state.agents_row());
    }

    /// Draws nothing, but takes the keyboard handoff a frame would start,
    /// and arms it, as the terminal owner's input thread does.
    fn hand_off(state: &mut State) {
        state.dialog_on_screen = state.dialog.as_ref().map(|dialog| dialog.request.id);
        let token = state.approval_drawn().expect("a handoff");
        update(state, Msg::ApprovalArmed(token));
    }

    fn secret_field(id: u64) -> RuntimeUpdate {
        RuntimeUpdate::Dialog(Some(crate::dialog::DialogRequest {
            id,
            kind: crate::dialog::DialogKind::Field(Box::new(crate::dialog::FieldDialog {
                title: "Sign in to CLIProxyAPI".into(),
                prompt: "API key".into(),
                secret: true,
                initial: String::new(),
                error: None,
                ..Default::default()
            })),
        }))
    }

    // Bake: a secret typed or pasted into a dialog field never reaches the
    // draft, its undo, the prompt history, or the transcript, and leaves
    // only in the dialog's answer; the draft is untouched throughout.
    #[test]
    fn a_secret_field_keeps_its_text_out_of_the_draft_and_history() {
        let mut state = live();
        type_str(&mut state, "draft");
        let undo = state.draft.undo_len();
        let rows = state.transcript.rows.clone();
        runtime(&mut state, secret_field(7));
        hand_off(&mut state);
        type_str(&mut state, "sk-fake-typed");
        update(&mut state, Msg::Paste("-pasted\nrest".into()));
        // A paste's line break did not answer the field.
        assert!(!state.dialog.as_ref().unwrap().answered);
        let effects = press(&mut state, Key::Enter);
        let [Effect::Dialog { id: 7, answer }] = &effects[..] else {
            panic!("{effects:?}");
        };
        assert_eq!(
            answer,
            &DialogAnswer::Entered("sk-fake-typed-pastedrest".to_owned().into())
        );
        let shown = format!("{effects:?} {state:?}");
        assert!(!shown.contains("sk-fake"), "{shown}");
        assert_eq!(state.draft.text(), "draft");
        assert_eq!(state.draft.undo_len(), undo);
        assert_eq!(state.transcript.rows, rows);
        // The runtime closes it; keys reach the composer again.
        runtime(&mut state, RuntimeUpdate::Dialog(None));
        hand_off(&mut state);
        type_str(&mut state, "!");
        assert_eq!(state.draft.text(), "draft!");
    }

    // Pi `showTreeSelector`'s branch-summary prompt: "Summarize branch?"
    // lists no summary, a summary, and a summary with a custom prompt, the
    // first selected and none marked current; ↓ and Enter answer with the
    // choice's value, and Esc goes back. The custom-instructions field
    // answers with what was typed, which never reaches the draft.
    #[test]
    fn the_branch_summary_prompt_answers_with_each_choice() {
        use crate::dialog::{
            BranchSummaryChoice, DialogKind, DialogRequest, branch_summary_instructions,
            branch_summary_prompt,
        };
        let prompt = branch_summary_prompt();
        assert_eq!(prompt.title, "Summarize branch?");
        let labels: Vec<&str> = prompt
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(
            labels,
            ["No summary", "Summarize", "Summarize with custom prompt"]
        );
        assert_eq!((prompt.selected, prompt.current), (Some(0), None));
        assert!(!prompt.filterable && !prompt.saves_default);

        let mut state = live();
        type_str(&mut state, "draft");
        for (id, downs, choice) in [
            (1, 0, BranchSummaryChoice::None),
            (2, 1, BranchSummaryChoice::Summarize),
            (3, 2, BranchSummaryChoice::Custom),
        ] {
            runtime(
                &mut state,
                RuntimeUpdate::Dialog(Some(DialogRequest {
                    id,
                    kind: DialogKind::List(branch_summary_prompt()),
                })),
            );
            hand_off(&mut state);
            for _ in 0..downs {
                press(&mut state, Key::Down);
            }
            let effects = press(&mut state, Key::Enter);
            let [
                Effect::Dialog {
                    id: answered,
                    answer,
                },
            ] = &effects[..]
            else {
                panic!("{effects:?}");
            };
            assert_eq!(*answered, id);
            let DialogAnswer::Chose {
                value,
                save_default,
            } = answer
            else {
                panic!("{answer:?}");
            };
            assert_eq!(BranchSummaryChoice::from_value(value), Some(choice));
            assert!(!save_default);
        }
        assert_eq!(BranchSummaryChoice::from_value("other"), None);

        runtime(
            &mut state,
            RuntimeUpdate::Dialog(Some(DialogRequest {
                id: 4,
                kind: DialogKind::Field(Box::new(branch_summary_instructions())),
            })),
        );
        hand_off(&mut state);
        type_str(&mut state, "keep the numbers");
        assert_eq!(
            press(&mut state, Key::Enter),
            [Effect::Dialog {
                id: 4,
                answer: DialogAnswer::Entered("keep the numbers".to_owned().into()),
            }]
        );
        runtime(
            &mut state,
            RuntimeUpdate::Dialog(Some(DialogRequest {
                id: 5,
                kind: DialogKind::List(branch_summary_prompt()),
            })),
        );
        hand_off(&mut state);
        assert_eq!(
            press(&mut state, Key::Esc),
            [Effect::Dialog {
                id: 5,
                answer: DialogAnswer::Cancel,
            }]
        );
        runtime(&mut state, RuntimeUpdate::Dialog(None));
        hand_off(&mut state);
        assert_eq!(state.draft.text(), "draft");
    }

    // Pi's `BranchSummaryStatusIndicator`: while `/tree` summarizes, the
    // runtime's turn shows the phase and Esc reaches the runtime as a
    // cancel; the summary replays as Pi's collapsed branch row.
    #[test]
    fn a_branch_summary_runs_as_cancellable_work() {
        let mut state = live();
        runtime(&mut state, RuntimeUpdate::TurnStarted { seed: "s".into() });
        runtime(&mut state, RuntimeUpdate::summarizing_branch());
        assert_eq!(state.phase.as_deref(), Some("summarizing branch"));
        assert_eq!(state.mode(), Mode::Running);
        assert_eq!(press(&mut state, Key::Esc), [Effect::Cancel]);
        assert_eq!(
            Row::branch_summary(),
            Row::Info("[branch] Branch summary".into())
        );
    }

    // Pi `AgentSessionRuntime` replacement and its interactive mode's
    // `renderInitialMessages`: the transcript starts over with the new
    // session's rows, its epoch moves so a normal-screen writer publishes
    // them from the first, and the draft and its images stay.
    #[test]
    fn a_replaced_session_starts_the_transcript_over_and_keeps_the_draft() {
        let mut state = live();
        runtime(
            &mut state,
            RuntimeUpdate::Commit(vec![Row::User("old".into())]),
        );
        runtime(
            &mut state,
            RuntimeUpdate::Pending(vec![crate::runtime::Pending {
                text: "queued".into(),
                target: crate::runtime::Target::NextTurn,
            }]),
        );
        type_str(&mut state, "draft");
        let epoch = state.transcript_epoch;
        runtime(
            &mut state,
            RuntimeUpdate::Replaced(vec![
                Row::Info("Resumed session x".into()),
                Row::User("new".into()),
            ]),
        );
        assert_eq!(state.transcript_epoch, epoch + 1);
        assert_eq!(
            state.transcript.rows,
            [
                Row::Info("Resumed session x".into()),
                Row::User("new".into())
            ]
        );
        assert_eq!(state.settled_rows(), 2);
        assert!(state.pending.is_empty());
        assert_eq!(state.draft.text(), "draft");
    }

    // Bake: a repeated or burst Enter never answers, an answered field
    // takes nothing more until the runtime's next dialog, and keys typed
    // across that transition do not spill into the draft.
    #[test]
    fn enter_bursts_and_repeats_do_not_carry_across_dialogs() {
        let mut state = live();
        runtime(&mut state, secret_field(1));
        // Keys queued before the handoff is armed go nowhere.
        type_str(&mut state, "early");
        assert_eq!(press(&mut state, Key::Enter), []);
        hand_off(&mut state);
        assert_eq!(state.dialog.as_ref().unwrap().text.chars(), 0);
        update(
            &mut state,
            Msg::KeyRead {
                key: KeyInput::plain(Key::Char('a')),
                burst: true,
            },
        );
        assert_eq!(
            update(
                &mut state,
                Msg::KeyRead {
                    key: KeyInput::plain(Key::Enter),
                    burst: true,
                },
            ),
            []
        );
        assert_eq!(
            update(&mut state, Msg::KeyRepeat(KeyInput::plain(Key::Enter))),
            []
        );
        assert_eq!(press(&mut state, Key::Enter).len(), 1);
        // Waiting: a held Enter and typing go nowhere.
        assert_eq!(
            update(&mut state, Msg::KeyRepeat(KeyInput::plain(Key::Enter))),
            []
        );
        type_str(&mut state, "late");
        assert_eq!(state.draft.text(), "");
        runtime(&mut state, secret_field(2));
        type_str(&mut state, "typeahead");
        assert_eq!(press(&mut state, Key::Enter), []);
        hand_off(&mut state);
        assert_eq!(
            update(&mut state, Msg::KeyRepeat(KeyInput::plain(Key::Enter))),
            []
        );
        assert_eq!(state.dialog.as_ref().unwrap().text.chars(), 0);
        // Esc cancels the new one, once, even before it holds the keyboard.
        runtime(&mut state, secret_field(2));
        assert!(!state.dialog_ready());
        assert_eq!(
            press(&mut state, Key::Esc),
            [Effect::Dialog {
                id: 2,
                answer: DialogAnswer::Cancel,
            }]
        );
        assert_eq!(press(&mut state, Key::Esc), []);
        // Closed: typeahead before the handoff does not reach the draft.
        runtime(&mut state, RuntimeUpdate::Dialog(None));
        type_str(&mut state, "spill");
        hand_off(&mut state);
        assert_eq!(state.draft.text(), "");
        type_str(&mut state, "ok");
        assert_eq!(state.draft.text(), "ok");
    }

    // Bake: an approval request takes the keyboard first; the dialog keeps
    // what was typed and takes keys again once the request is settled.
    #[test]
    fn an_approval_takes_precedence_over_a_dialog() {
        let mut state = live();
        runtime(&mut state, secret_field(3));
        hand_off(&mut state);
        type_str(&mut state, "ab");
        runtime(
            &mut state,
            RuntimeUpdate::Approvals(vec![Approval {
                generation: 9,
                tool: "bash".into(),
                call_id: "c".into(),
                parent_call_id: None,
                reason: "r".into(),
                arguments: String::new(),
            }]),
        );
        assert_eq!(
            press(&mut state, Key::Esc),
            [Effect::Answer {
                generation: 9,
                answer: ApprovalAnswer::Cancel,
            }]
        );
        assert_eq!(state.dialog.as_ref().unwrap().text.chars(), 2);
        // The approval covered the dialog: once it clears, keys queued
        // before the dialog is handed back go nowhere.
        state.approval_on_screen = Some(9);
        assert!(state.approval_drawn().is_some());
        runtime(&mut state, RuntimeUpdate::Approvals(Vec::new()));
        type_str(&mut state, "queued");
        assert_eq!(state.dialog.as_ref().unwrap().text.expose(), "ab");
        hand_off(&mut state);
        type_str(&mut state, "c");
        assert_eq!(state.dialog.as_ref().unwrap().text.expose(), "abc");
        // A resize keeps its text but takes the keyboard back until the
        // next frame shows it whole and is armed.
        update(&mut state, Msg::Resize { cols: 40, rows: 10 });
        assert_eq!(state.dialog.as_ref().unwrap().text.chars(), 3);
        type_str(&mut state, "x");
        assert_eq!(state.dialog.as_ref().unwrap().text.chars(), 3);
        // A frame that does not show it whole hands nothing over.
        state.dialog_on_screen = None;
        assert_eq!(state.approval_drawn(), None);
        hand_off(&mut state);
        type_str(&mut state, "d");
        assert_eq!(state.dialog.as_ref().unwrap().text.expose(), "abcd");
    }

    // Bake: a live session's slash menu lists the commands it runs, and
    // Enter on one submits its name.
    #[test]
    fn a_live_slash_menu_submits_the_chosen_command() {
        let mut state = live();
        type_str(&mut state, "/thi");
        let menu = state.menu().expect("menu");
        assert_eq!(menu.entries[0].name, "thinking");
        assert_eq!(
            submitted(press(&mut state, Key::Enter)),
            [Submission::FollowUp("/thinking".into())]
        );
    }

    #[test]
    fn a_refused_prompt_restores_its_editing_history() {
        let mut state = live();
        type_str(&mut state, "first");
        state.draft.paste(" second");
        let undo_len = state.draft.undo_len();
        let [Effect::Submit(sent)] = &press(&mut state, Key::Enter)[..] else {
            panic!("no submission");
        };
        runtime(
            &mut state,
            RuntimeUpdate::Refused {
                id: sent.id,
                reason: "no key".into(),
            },
        );
        assert_eq!(state.draft.undo_len(), undo_len);
        chord(&mut state, Key::Char('-'), Mods::CTRL);
        assert_eq!(state.draft.text(), "first");
        chord(&mut state, Key::Char('-'), Mods::CTRL);
        assert_eq!(state.draft.text(), "");
    }

    #[test]
    fn a_refused_prompt_puts_its_draft_images_and_caret_back() {
        let mut state = live();
        type_str(&mut state, "see ");
        live_image(&mut state, "shot.png");
        press(&mut state, Key::Left);
        let caret = state.draft.caret();
        let [Effect::Submit(sent)] = &press(&mut state, Key::Enter)[..] else {
            panic!("no submission");
        };
        let sent = sent.clone();
        assert_eq!((state.draft.text(), state.attachments.len()), ("", 0));
        // Refused with nothing typed since: the draft returns as it was.
        runtime(
            &mut state,
            RuntimeUpdate::Refused {
                id: sent.id,
                reason: "no key".into(),
            },
        );
        assert_eq!(state.draft.text(), "see [Image #1]");
        assert_eq!(state.draft.caret(), caret);
        assert_eq!(state.attachments.len(), 1);
        assert_eq!(
            (state.notice, state.refusal.as_deref()),
            (Some(Notice::Refused), Some("no key"))
        );
        // Sent again, it carries the same image bytes.
        let [Effect::Submit(again)] = &press(&mut state, Key::Enter)[..] else {
            panic!("no submission");
        };
        assert_eq!(again.images, sent.images);
        assert_eq!(again.images[0].data.bytes(), b"shot.png");
        // Typing while it waits survives a refusal, after the prompt.
        type_str(&mut state, "more");
        runtime(
            &mut state,
            RuntimeUpdate::Refused {
                id: again.id,
                reason: "still no key".into(),
            },
        );
        assert_eq!(state.draft.text(), "see [Image #1]\n\nmore");
        assert_eq!(state.attachments.len(), 1);
        // An admitted prompt's draft is forgotten: a late refusal changes nothing.
        state.draft.replace("", 0);
        unstage(&mut state);
        let effects = press(&mut state, Key::Enter);
        assert_eq!(effects, []);
        type_str(&mut state, "ok");
        let [Effect::Submit(admitted)] = &press(&mut state, Key::Enter)[..] else {
            panic!("no submission");
        };
        let id = admitted.id;
        runtime(&mut state, RuntimeUpdate::Admitted(id));
        runtime(
            &mut state,
            RuntimeUpdate::Refused {
                id,
                reason: "late".into(),
            },
        );
        assert_eq!(state.draft.text(), "");
    }

    #[test]
    fn returned_prompts_go_back_ahead_of_the_draft() {
        let mut state = live();
        type_str(&mut state, "typed");
        runtime(
            &mut state,
            RuntimeUpdate::Returned(vec!["one".into(), "two".into()]),
        );
        assert_eq!(state.draft.text(), "one\n\ntwo\n\ntyped");
        assert_eq!(state.notice, Some(Notice::Returned));
    }

    // Declined command-line prompts go ahead of the draft, in order, and
    // say why as a refusal does.
    #[test]
    fn declined_prompts_go_ahead_of_the_draft_with_their_reason() {
        let mut state = live();
        type_str(&mut state, "typed");
        runtime(
            &mut state,
            RuntimeUpdate::Declined {
                texts: vec!["/goal ship".into(), "/help".into()],
                reason: "deferred".into(),
            },
        );
        assert_eq!(state.draft.text(), "/goal ship\n\n/help\n\ntyped");
        assert_eq!(
            (state.notice, state.refusal.as_deref()),
            (Some(Notice::Refused), Some("deferred"))
        );
    }

    // `/tree` and `/fork` give the chosen message back ahead of anything
    // typed since; an empty text changes nothing.
    #[test]
    fn a_restored_message_goes_ahead_of_the_draft() {
        let mut state = live();
        runtime(&mut state, RuntimeUpdate::Restored("chosen".into()));
        assert_eq!(state.draft.text(), "chosen");
        assert_eq!(state.notice, Some(Notice::Restored));
        type_str(&mut state, " more");
        runtime(&mut state, RuntimeUpdate::Restored("again".into()));
        assert_eq!(state.draft.text(), "again\n\nchosen more");
        state.notice = None;
        runtime(&mut state, RuntimeUpdate::Restored(String::new()));
        assert_eq!(state.notice, None);
        // Nothing returned changes nothing.
        let mut state = live();
        runtime(&mut state, RuntimeUpdate::Returned(Vec::new()));
        assert_eq!((state.draft.text(), state.notice), ("", None));
    }

    #[test]
    fn the_runtime_sets_the_status_and_a_failed_turn_says_so() {
        let mut state = live();
        runtime(
            &mut state,
            RuntimeUpdate::Status(crate::runtime::SessionStatus {
                model: Some("p/m".into()),
                thinking: Some("high".into()),
                context: None,
                tokens: Some(crate::status::TokenTotals {
                    input: 12_000,
                    output: 3_400,
                }),
                cache: None,
                name: None,
            }),
        );
        assert_eq!(state.status.model.as_deref(), Some("p/m"));
        assert_eq!(state.status.tokens.map(|t| t.output), Some(3_400));
        assert_eq!(state.status.thinking.as_deref(), Some("high"));
        runtime(&mut state, RuntimeUpdate::TurnStarted { seed: "s".into() });
        runtime(&mut state, RuntimeUpdate::TurnEnded(Outcome::Failed));
        assert_eq!(state.summary.map(|s| s.outcome), Some(Outcome::Failed));
        assert_eq!(state.activity, None);
    }

    // Bake: a compaction between a turn's steps names itself with its own
    // clock and hands the bar back to the turn; one the turn's end cuts
    // short ends with the turn; `/compact`, which is the work itself, keeps
    // its start and ends as a compaction.
    #[test]
    fn a_runtime_compaction_takes_the_bar_and_gives_it_back() {
        let mut state = live();
        runtime(&mut state, RuntimeUpdate::TurnStarted { seed: "s".into() });
        let turn = state.activity;
        state.now = Duration::from_secs(10);
        runtime(
            &mut state,
            RuntimeUpdate::CompactionStarted { interrupts: true },
        );
        let compaction = state.activity.expect("compacting");
        assert_eq!(
            (compaction.kind, compaction.word, compaction.started),
            (
                ActivityKind::Compaction,
                crate::copy::COMPACTING,
                Duration::from_secs(10)
            )
        );
        assert_eq!(state.mode(), Mode::Compacting);
        // A repeated start changes nothing.
        runtime(
            &mut state,
            RuntimeUpdate::CompactionStarted { interrupts: true },
        );
        assert_eq!(state.compacting_over, turn);
        runtime(&mut state, RuntimeUpdate::CompactionEnded);
        assert_eq!(state.activity, turn);
        assert_eq!(state.mode(), Mode::Running);
        runtime(
            &mut state,
            RuntimeUpdate::CompactionStarted { interrupts: true },
        );
        state.now = Duration::from_secs(20);
        runtime(&mut state, RuntimeUpdate::TurnEnded(Outcome::Completed));
        assert_eq!(
            state.summary,
            Some(TurnSummary {
                outcome: Outcome::Completed,
                elapsed: Duration::from_secs(20),
                kind: ActivityKind::Turn,
            })
        );
        assert_eq!((state.activity, state.compacting_over), (None, None));
        // `/compact`.
        runtime(&mut state, RuntimeUpdate::TurnStarted { seed: "c".into() });
        state.now = Duration::from_secs(23);
        runtime(
            &mut state,
            RuntimeUpdate::CompactionStarted { interrupts: false },
        );
        assert_eq!(
            state.activity.map(|a| (a.kind, a.started)),
            Some((ActivityKind::Compaction, Duration::from_secs(20)))
        );
        // Its end comes as the turn's, not a compaction's end, and the
        // transcript it rebuilds first leaves it running.
        runtime(&mut state, RuntimeUpdate::CompactionEnded);
        assert_eq!(state.mode(), Mode::Compacting);
        runtime(&mut state, RuntimeUpdate::Replaced(Vec::new()));
        assert_eq!(state.mode(), Mode::Compacting);
        state.now = Duration::from_secs(32);
        runtime(&mut state, RuntimeUpdate::TurnEnded(Outcome::Completed));
        assert_eq!(
            state.summary,
            Some(TurnSummary {
                outcome: Outcome::Completed,
                elapsed: Duration::from_secs(12),
                kind: ActivityKind::Compaction,
            })
        );
    }

    #[test]
    fn alt_up_sends_what_the_runtime_holds_or_says_nothing_waits() {
        let mut state = State::default();
        assert_eq!(chord(&mut state, Key::Up, Mods::ALT), []);
        assert_eq!(state.notice, Some(Notice::NoPending));
        let waiting = vec![Pending {
            text: "also".into(),
            target: crate::runtime::Target::NextStep,
        }];
        runtime(&mut state, RuntimeUpdate::Pending(waiting.clone()));
        assert_eq!(state.pending, waiting);
        assert_eq!(chord(&mut state, Key::Up, Mods::ALT), [Effect::SendPending]);
        assert_eq!(state.notice, Some(Notice::PendingSent));
        // The panel empties only when the runtime says so.
        assert_eq!(state.pending, waiting);
        runtime(&mut state, RuntimeUpdate::Pending(Vec::new()));
        assert!(state.pending.is_empty());
    }

    #[test]
    fn a_blank_draft_sends_nothing() {
        let mut state = State::default();
        type_str(&mut state, "  ");
        assert_eq!(press(&mut state, Key::Enter), []);
        assert_eq!((state.draft.text(), state.notice), ("  ", None));
    }

    #[test]
    fn enter_during_the_sample_or_with_images_keeps_the_draft() {
        let mut state = State::default();
        chord(&mut state, Key::Char('t'), Mods::CTRL);
        type_str(&mut state, "hi");
        assert_eq!(press(&mut state, Key::Enter), []);
        assert_eq!(
            (state.draft.text(), state.notice),
            ("hi", Some(Notice::SampleOnly))
        );
        press(&mut state, Key::Esc);
        assert_eq!(state.activity, None);

        let image = Image {
            name: "a.png".into(),
            media_type: "image/png",
            bytes: 1,
            size: None,
            data: crate::paste::ImageData::default(),
        };
        update(
            &mut state,
            Msg::ImageRead {
                source: ImageSource::Clipboard,
                image: Some(image),
            },
        );
        assert_eq!(press(&mut state, Key::Enter), []);
        assert_eq!(state.notice, Some(Notice::NoImages));
        assert_eq!(state.draft.text(), "hi[Image #1]");
    }

    #[test]
    fn animated_sample_is_published_only_after_settlement() {
        let mut state = State::default();
        let before = state.settled_rows();
        toggle_activity(&mut state);
        assert_eq!(state.transcript.committed(), before + 1);
        assert_eq!(state.settled_rows(), before);
        update(&mut state, Msg::Tick(Duration::from_secs(2)));
        assert_eq!(state.settled_rows(), before);
        end_turn(&mut state, Outcome::Interrupted);
        assert_eq!(state.settled_rows(), before + 1);
    }

    #[test]
    fn a_runtime_turn_streams_live_rows_then_commits_them_once() {
        let mut state = State::default();
        let before = state.transcript.rows.len();
        update(&mut state, Msg::Tick(Duration::from_secs(3)));
        runtime(
            &mut state,
            RuntimeUpdate::Commit(vec![Row::User("hi".into())]),
        );
        runtime(
            &mut state,
            RuntimeUpdate::TurnStarted {
                seed: "turn-1".into(),
            },
        );
        assert_eq!(state.mode(), Mode::Running);
        let turn = state.activity.unwrap();
        assert!(turn.runtime);
        assert_eq!(turn.started, Duration::from_secs(3));
        runtime(&mut state, RuntimeUpdate::Phase("writing".into()));
        assert_eq!(state.phase.as_deref(), Some("writing"));
        runtime(
            &mut state,
            RuntimeUpdate::Live(vec![Row::Answer("Hel".into())]),
        );
        runtime(
            &mut state,
            RuntimeUpdate::Live(vec![Row::Answer("Hello".into())]),
        );
        assert_eq!(state.transcript.rows.len(), before + 2);
        runtime(
            &mut state,
            RuntimeUpdate::Commit(vec![Row::Answer("Hello.".into())]),
        );
        assert_eq!(
            state.transcript.rows[before..],
            [Row::User("hi".into()), Row::Answer("Hello.".into())]
        );
        assert_eq!(state.transcript.committed(), before + 2);
        // While it runs, Enter steers, Esc asks to stop, and Ctrl+T waits.
        type_str(&mut state, "also");
        assert_eq!(
            submitted(press(&mut state, Key::Enter)),
            [Submission::Steer("also".into())]
        );
        chord(&mut state, Key::Char('t'), Mods::CTRL);
        assert!(state.activity.unwrap().runtime);
        assert_eq!(press(&mut state, Key::Esc), [Effect::Cancel]);
        assert_eq!(state.mode(), Mode::Running, "the runtime ends the turn");
        update(&mut state, Msg::Tick(Duration::from_secs(5)));
        runtime(&mut state, RuntimeUpdate::TurnEnded(Outcome::Interrupted));
        assert_eq!((state.activity, state.phase.as_deref()), (None, None));
        assert_eq!(
            state.summary,
            Some(TurnSummary {
                outcome: Outcome::Interrupted,
                elapsed: Duration::from_secs(2),
                kind: ActivityKind::Turn,
            })
        );
        // The committed prompt is recalled like any other.
        press(&mut state, Key::Up);
        assert_eq!(state.draft.text(), "hi");
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
        assert_eq!(
            (state.focus, state.selected.as_str()),
            (Focus::Inspect, "sample-reviewer")
        );
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
        assert_eq!(state.selected, state.agents.last().unwrap().id);
        // Typing and pasting are ignored, with no notice repeating the bar.
        type_str(&mut state, "q");
        update(&mut state, Msg::Paste("pasted".into()));
        assert_eq!(state.notice, None);
        assert!(state.draft.is_empty());
        chord(&mut state, Key::Char('g'), Mods::CTRL);
        assert_eq!(state.focus, Focus::Composer);
        assert_eq!(state.selected, state.agents.last().unwrap().id);
    }

    #[test]
    fn a_second_ctrl_c_quits_from_every_focus_and_changes_nothing() {
        for focus in [Focus::Composer, Focus::AgentList, Focus::Inspect] {
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

    fn shot(name: &str) -> Image {
        Image {
            name: name.into(),
            media_type: "image/png",
            bytes: 10,
            size: Some((2, 1)),
            data: crate::paste::ImageData::default(),
        }
    }

    #[test]
    fn images_stage_with_a_placeholder_and_unstage_when_it_is_erased() {
        let mut state = State::default();
        // Ctrl+V and an empty paste read the clipboard; a lone image path its file.
        assert_eq!(
            chord(&mut state, Key::Char('v'), Mods::CTRL),
            [Effect::ReadImage(ImageSource::Clipboard)]
        );
        assert_eq!(
            update(&mut state, Msg::Paste(String::new())),
            [Effect::ReadImage(ImageSource::Clipboard)]
        );
        let path = "'/tmp/my shot.png'";
        let source = ImageSource::File {
            path: "/tmp/my shot.png".into(),
            pasted: path.into(),
        };
        assert_eq!(
            update(&mut state, Msg::Paste(path.into())),
            [Effect::ReadImage(source.clone())]
        );
        // A path that did not read as an image goes in as text.
        update(
            &mut state,
            Msg::ImageRead {
                source,
                image: None,
            },
        );
        assert_eq!(state.draft.text(), path);
        // No image on the clipboard says so.
        update(
            &mut state,
            Msg::ImageRead {
                source: ImageSource::Clipboard,
                image: None,
            },
        );
        assert_eq!(state.notice, Some(Notice::NoClipboardImage));
        chord(&mut state, Key::Char('u'), Mods::CTRL);
        update(
            &mut state,
            Msg::ImageRead {
                source: ImageSource::Clipboard,
                image: Some(shot("a.png")),
            },
        );
        type_str(&mut state, " and ");
        update(
            &mut state,
            Msg::ImageRead {
                source: ImageSource::Clipboard,
                image: Some(shot("b.png")),
            },
        );
        assert_eq!(state.draft.text(), "[Image #1] and [Image #2]");
        assert_eq!(state.notice, None);
        assert_eq!(state.attachments.len(), 2);
        // Backspace removes the placeholder whole and unstages its image;
        // undo does not bring it back without its image.
        press(&mut state, Key::Backspace);
        assert_eq!(state.draft.text(), "[Image #1] and ");
        assert_eq!(
            state
                .attachments
                .iter()
                .map(|(_, i)| i.name.as_str())
                .collect::<Vec<_>>(),
            ["a.png"]
        );
        chord(&mut state, Key::Char('-'), Mods::CTRL);
        assert!(!state.draft.text().contains("[Image #2]"));
        // A kill unstages too, and a yank brings back only the text.
        chord(&mut state, Key::Char('u'), Mods::CTRL);
        assert!(state.attachments.is_empty());
        chord(&mut state, Key::Char('y'), Mods::CTRL);
        assert!(!state.draft.text().contains("[Image"));
    }

    fn menu_names(state: &State) -> Option<(Vec<&'static str>, &'static str)> {
        state.menu().map(|menu| {
            let names: Vec<_> = menu.entries.iter().map(|c| c.name).collect();
            let selected = names[menu.selected];
            (names, selected)
        })
    }

    #[test]
    fn the_slash_menu_selects_completes_and_closes_as_the_oracle_does() {
        let mut state = State::default();
        type_str(&mut state, "/lo");
        assert_eq!(menu_names(&state), Some((vec!["login", "logout"], "login")));
        // Down and Up cycle the selection instead of moving the caret.
        press(&mut state, Key::Down);
        assert_eq!(menu_names(&state).unwrap().1, "logout");
        press(&mut state, Key::Down);
        assert_eq!(menu_names(&state).unwrap().1, "login");
        press(&mut state, Key::Up);
        assert_eq!(menu_names(&state).unwrap().1, "logout");
        // Tab fills it in with a space, and the menu gives way to its usage.
        press(&mut state, Key::Tab);
        assert_eq!((state.draft.text(), state.draft.caret()), ("/logout ", 8));
        assert!(state.menu().is_none());
        assert_eq!(
            completion::usage(state.draft.text(), state.catalog()).map(|c| c.name),
            Some("logout")
        );
        // One undo takes the completion back; at the same draft and caret
        // the menu keeps the selection it had there.
        chord(&mut state, Key::Char('-'), Mods::CTRL);
        assert_eq!(menu_names(&state).unwrap().1, "logout");
        // Esc closes it until the draft changes; the next key reopens it.
        press(&mut state, Key::Esc);
        assert!(state.menu().is_none());
        type_str(&mut state, "g");
        assert_eq!(menu_names(&state), Some((vec!["login", "logout"], "login")));
    }

    #[test]
    fn enter_on_the_menu_runs_a_command_whose_argument_is_optional() {
        let mut state = State::default();
        type_str(&mut state, "/mo");
        press(&mut state, Key::Enter);
        assert_eq!(state.draft.text(), "/model");
        assert_eq!(state.notice, Some(Notice::NoCommands));
        // An optional argument does not hold the command back for input.
        chord(&mut state, Key::Char('u'), Mods::CTRL);
        type_str(&mut state, "/thi");
        state.notice = None;
        press(&mut state, Key::Enter);
        assert_eq!(state.draft.text(), "/thinking");
        assert_eq!(state.notice, Some(Notice::NoCommands));
        // With nothing to choose, Enter submits what was typed; a bare `/`
        // does nothing.
        chord(&mut state, Key::Char('u'), Mods::CTRL);
        type_str(&mut state, "/zz");
        state.notice = None;
        press(&mut state, Key::Enter);
        assert_eq!(state.notice, Some(Notice::NoCommands));
        chord(&mut state, Key::Char('u'), Mods::CTRL);
        type_str(&mut state, "/");
        state.notice = None;
        press(&mut state, Key::Enter);
        assert_eq!(
            (state.draft.text(), state.notice),
            ("/model", Some(Notice::NoCommands))
        );
        // Without a menu Tab types a tab, and plain text is sent.
        chord(&mut state, Key::Char('u'), Mods::CTRL);
        type_str(&mut state, "a");
        press(&mut state, Key::Tab);
        assert_eq!(state.draft.text(), "a\t");
        assert_eq!(
            submitted(press(&mut state, Key::Enter)),
            [Submission::FollowUp("a\t".into())]
        );
    }

    fn found(state: &mut State, query: &str, paths: &[&str]) -> Vec<Effect> {
        let found = paths
            .iter()
            .map(|path| match path.strip_suffix('/') {
                Some(dir) => Candidate {
                    path: dir.into(),
                    kind: mention::PathKind::Directory,
                },
                None => Candidate {
                    path: (*path).into(),
                    kind: mention::PathKind::File,
                },
            })
            .collect();
        update(
            state,
            Msg::FilesFound {
                query: query.into(),
                found: Ok(found),
            },
        )
    }

    fn file_names(state: &State) -> Option<(Vec<String>, usize, bool)> {
        state.file_menu().map(|menu| {
            let names = menu.entries.iter().map(|c| c.name.clone()).collect();
            (names, menu.selected, menu.loading)
        })
    }

    #[test]
    fn an_at_token_asks_for_its_query_once_and_lists_only_its_answer() {
        let mut state = State::default();
        type_str(&mut state, "read ");
        assert!(state.file_menu().is_none());
        assert_eq!(
            press(&mut state, Key::Char('@')),
            [Effect::FindFiles(String::new())]
        );
        assert_eq!(file_names(&state), Some((vec![], 0, true)));
        assert_eq!(
            press(&mut state, Key::Char('s')),
            [Effect::FindFiles("s".into())]
        );
        // An answer for an older query is dropped; the current one lists.
        found(&mut state, "", &["README.md"]);
        assert_eq!(file_names(&state), Some((vec![], 0, true)));
        assert_eq!(found(&mut state, "s", &["src/", "src/main.rs"]), []);
        assert_eq!(
            file_names(&state),
            Some((vec!["@src/".into(), "@src/main.rs".into()], 0, false))
        );
        // A key that leaves the query alone asks again for nothing.
        assert_eq!(press(&mut state, Key::Down), []);
        // Leaving the token forgets the query; a failure says why.
        press(&mut state, Key::Char(' '));
        assert_eq!(state.files, Files::default());
        press(&mut state, Key::Char('@'));
        update(
            &mut state,
            Msg::FilesFound {
                query: String::new(),
                found: Err("denied".into()),
            },
        );
        let menu = state.file_menu().unwrap();
        assert_eq!(
            (menu.loading, menu.error.as_deref()),
            (false, Some("denied"))
        );
        // A slash menu takes precedence and asks for no paths.
        chord(&mut state, Key::Char('u'), Mods::CTRL);
        type_str(&mut state, "/@");
        assert!(state.file_menu().is_none() && state.files.query.is_none());
    }

    #[test]
    fn the_file_menu_cycles_completes_closes_and_leaves_enter_to_send() {
        let mut state = State::default();
        type_str(&mut state, "see @s");
        found(&mut state, "s", &["src/", "setup.md"]);
        press(&mut state, Key::Down);
        assert_eq!(file_names(&state).unwrap().1, 1);
        press(&mut state, Key::Up);
        press(&mut state, Key::Up);
        assert_eq!(file_names(&state).unwrap().1, 1);
        // Tab puts in the file and a space, which ends the token.
        press(&mut state, Key::Tab);
        assert_eq!(state.draft.text(), "see @setup.md ");
        assert!(state.file_menu().is_none());
        // A directory keeps the token open and asks for its contents.
        chord(&mut state, Key::Char('u'), Mods::CTRL);
        type_str(&mut state, "@s");
        found(&mut state, "s", &["src/", "setup.md"]);
        let effects = press(&mut state, Key::Tab);
        assert_eq!(state.draft.text(), "@src/");
        assert_eq!(effects, [Effect::FindFiles("src/".into())]);
        // While nothing is listed the menu's keys do nothing.
        press(&mut state, Key::Tab);
        press(&mut state, Key::Down);
        assert_eq!(state.draft.text(), "@src/");
        // Esc closes the menu until the draft or caret changes.
        press(&mut state, Key::Esc);
        assert!(state.file_menu().is_none() && state.files.query.is_none());
        assert_eq!(
            press(&mut state, Key::Char('m')),
            [Effect::FindFiles("src/m".into())]
        );
        // Enter sends the draft as typed, menu or not.
        assert_eq!(
            submitted(press(&mut state, Key::Enter)),
            [Submission::FollowUp("@src/m".into())]
        );
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
            (ActivityKind::Turn, Duration::from_secs(3))
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
                ActivityKind::Compaction,
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
            kind: ActivityKind::Turn,
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
                kind: ActivityKind::Turn,
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
            Row::Codemode(script) => (script.state, script.summary.clone(), script.calls.len()),
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
        // Calls arrive on the clock: the `find` settles, and the reads start.
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
            Row::Codemode(script) => {
                assert_eq!(
                    (script.state, script.summary.as_deref()),
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

    // Pi `app.thinking.cycle`: Shift+Tab, as BackTab or as Tab with Shift,
    // asks a live runtime for the next thinking level and leaves the draft
    // alone; the preview has no model, and Tab still completes.
    #[test]
    fn shift_tab_asks_a_live_runtime_for_the_next_thinking_level() {
        let mut live = State::live(FrameStyle::default(), Tones::None, Vec::new());
        live.draft.type_text("kept");
        assert_eq!(press(&mut live, Key::BackTab), [Effect::CycleThinking]);
        assert_eq!(
            chord(&mut live, Key::Tab, Mods::SHIFT),
            [Effect::CycleThinking]
        );
        assert_eq!(live.draft.text(), "kept");
        let mut preview = State::default();
        assert!(press(&mut preview, Key::BackTab).is_empty());
        press(&mut preview, Key::Tab);
        assert_eq!(preview.draft.text(), "\t");
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
        type_str(&mut state, "/x");
        press(&mut state, Key::Enter);
        update(&mut state, Msg::Resize { cols: 20, rows: 5 });
        update(&mut state, Msg::Tick(Duration::from_secs(9)));
        assert_eq!(state.draft.text(), "/x");
        assert_eq!(state.notice, Some(Notice::NoCommands));
        assert_eq!(state.now, Duration::from_secs(9));
    }

    // ---- Approvals ----
    //
    // The keys follow the TypeScript `ApprovalView`
    // (`apps/tui/packages/ui/src/interaction.tsx`): `n` rejects, a paste
    // is dropped, Ctrl and Alt chords decide nothing; `y` then Enter
    // allows, and only once the input thread has armed the request.

    fn waiting(generation: u64) -> crate::runtime::Approval {
        crate::runtime::Approval {
            generation,
            tool: "bash".into(),
            call_id: format!("call-{generation}"),
            parent_call_id: None,
            reason: "escalate sandbox to danger-full-access: needs the network".into(),
            arguments: "command: curl -fsS https://example.com".into(),
        }
    }

    /// A live state with `text` typed, the caret one left of its end, and
    /// request `generation` displayed, drawn, and armed.
    fn asking(text: &str, generation: u64) -> State {
        let mut state = State::live(FrameStyle::default(), Tones::None, Vec::new());
        type_str(&mut state, text);
        press(&mut state, Key::Left);
        runtime(
            &mut state,
            RuntimeUpdate::Approvals(vec![waiting(generation)]),
        );
        state.approval_on_screen = Some(generation);
        let token = state.approval_drawn().expect("a new showing");
        assert_eq!(update(&mut state, Msg::ApprovalArmed(token)), []);
        state
    }

    fn answer(generation: u64, answer: ApprovalAnswer) -> Vec<Effect> {
        vec![Effect::Answer { generation, answer }]
    }

    fn read(state: &mut State, key: Key, burst: bool) -> Vec<Effect> {
        let key = KeyInput::plain(key);
        update(state, Msg::KeyRead { key, burst })
    }

    #[test]
    fn y_then_enter_allows_and_nothing_else_touches_the_draft() {
        let mut state = asking("draft", 5);
        state.attachments.push((1, shot("a.png")));
        // Enter alone never allows.
        assert_eq!(press(&mut state, Key::Enter), []);
        assert_eq!(state.notice, Some(Notice::ApprovalChooseFirst));
        for key in [
            Key::Char('x'),
            Key::Backspace,
            Key::Tab,
            Key::Left,
            Key::Home,
        ] {
            assert_eq!(press(&mut state, key), [], "{key:?}");
        }
        assert_eq!(state.notice, Some(Notice::ApprovalKeys));
        assert_eq!(chord(&mut state, Key::Char('y'), Mods::CTRL), []);
        assert_eq!(chord(&mut state, Key::Char('y'), Mods::ALT), []);
        assert_eq!(update(&mut state, Msg::Paste("y\n".into())), []);
        assert_eq!(
            update(&mut state, Msg::Paste(String::new())),
            [],
            "no image read"
        );
        let click = Mouse {
            kind: MouseKind::Down,
            column: 4,
            row: 20,
            alt: false,
            at: Duration::ZERO,
        };
        assert_eq!(update(&mut state, Msg::Mouse(click)), []);
        // `y` chooses; another key withdraws the choice; `y` again, then
        // Enter, allows. A held `y` repeats only the choice.
        assert_eq!(chord(&mut state, Key::Char('Y'), Mods::SHIFT), []);
        assert!(state.approval_chosen());
        assert_eq!(press(&mut state, Key::Char('z')), []);
        assert!(!state.approval_chosen());
        for _ in 0..3 {
            assert_eq!(press(&mut state, Key::Char('y')), []);
        }
        assert_eq!(state.notice, None);
        assert_eq!(
            press(&mut state, Key::Enter),
            answer(5, ApprovalAnswer::AllowOnce)
        );
        assert_eq!((state.draft.text(), state.draft.caret()), ("draft", 4));
        assert_eq!(state.attachments.len(), 1);
        // Answered: nothing more is sent for it.
        assert_eq!(press(&mut state, Key::Char('n')), []);
        assert_eq!(press(&mut state, Key::Esc), []);
        // The runtime settles it; the composer has its draft back.
        runtime(&mut state, RuntimeUpdate::Approvals(Vec::new()));
        press(&mut state, Key::Char('!'));
        assert_eq!(state.draft.text(), "draf!t");
    }

    #[test]
    fn reported_key_repeats_edit_the_composer_but_never_answer_an_approval() {
        let mut state = asking("draft", 5);
        let repeat = |key| Msg::KeyRepeat(KeyInput::plain(key));
        assert_eq!(update(&mut state, repeat(Key::Char('y'))), []);
        assert!(!state.approval_chosen());
        press(&mut state, Key::Char('y'));
        assert!(state.approval_chosen());
        for key in [Key::Enter, Key::Char('n'), Key::Esc] {
            assert_eq!(update(&mut state, repeat(key)), []);
        }
        assert_eq!(
            press(&mut state, Key::Enter),
            answer(5, ApprovalAnswer::AllowOnce)
        );
        runtime(&mut state, RuntimeUpdate::Approvals(Vec::new()));
        let before = state.draft.text().len();
        update(&mut state, repeat(Key::Char('x')));
        assert_eq!(state.draft.text().len(), before + 1);
    }

    #[test]
    fn keys_before_the_arming_or_in_a_burst_answer_nothing() {
        let mut state = State::live(FrameStyle::default(), Tones::None, Vec::new());
        runtime(&mut state, RuntimeUpdate::Approvals(vec![waiting(9)]));
        // Neither drawn nor armed.
        assert_eq!(read(&mut state, Key::Char('n'), false), []);
        // Drawn, so the input thread is arming it, but not armed yet: a key
        // read before the arming was waiting when the prompt appeared.
        state.approval_on_screen = Some(9);
        assert_eq!(state.approval_drawn(), Some(1));
        state.approval_on_screen = Some(9);
        assert_eq!(state.approval_drawn(), None, "the same showing");
        assert_eq!(read(&mut state, Key::Char('n'), false), []);
        // An arming for another showing arms nothing.
        update(&mut state, Msg::ApprovalArmed(2));
        assert_eq!(read(&mut state, Key::Char('n'), false), []);
        update(&mut state, Msg::ApprovalArmed(1));
        assert_eq!(read(&mut state, Key::Char('n'), true), [], "a burst");
        assert_eq!(read(&mut state, Key::Char('y'), true), []);
        assert!(!state.approval_chosen());
        assert_eq!(state.notice, Some(Notice::ApprovalKeys));
        assert_eq!(read(&mut state, Key::Char('y'), false), []);
        assert_eq!(read(&mut state, Key::Enter, true), [], "Enter in a burst");
        assert!(!state.approval_chosen(), "a burst withdraws the choice");
        assert_eq!(read(&mut state, Key::Char('y'), false), []);
        assert_eq!(update(&mut state, Msg::Paste("x".into())), []);
        assert!(!state.approval_chosen(), "a paste withdraws the choice");
        assert_eq!(
            read(&mut state, Key::Char('n'), false),
            answer(9, ApprovalAnswer::Reject)
        );
        // The next request is unarmed and unchosen until its own showing is
        // armed, so the last answer's keys cannot reach it.
        runtime(&mut state, RuntimeUpdate::Approvals(vec![waiting(10)]));
        assert!(!state.approval_chosen());
        update(&mut state, Msg::ApprovalArmed(1));
        assert_eq!(read(&mut state, Key::Enter, false), []);
        assert_eq!(read(&mut state, Key::Char('n'), false), []);
        // Esc withdraws it even before it is armed.
        assert_eq!(
            press(&mut state, Key::Esc),
            answer(10, ApprovalAnswer::Cancel)
        );
    }

    #[test]
    fn a_resize_or_a_frame_that_hides_the_request_disarms_it() {
        let mut state = asking("", 4);
        press(&mut state, Key::Char('y'));
        assert!(state.approval_chosen());
        // A frame that cannot show it whole ends the showing and its choice.
        state.approval_on_screen = None;
        assert_eq!(state.approval_drawn(), None);
        assert!(!state.approval_chosen());
        assert_eq!(press(&mut state, Key::Enter), []);
        // Shown again: a new showing, armed afresh; the old arming is stale.
        state.approval_on_screen = Some(4);
        assert_eq!(state.approval_drawn(), Some(2));
        update(&mut state, Msg::ApprovalArmed(1));
        assert_eq!(press(&mut state, Key::Char('y')), []);
        assert_eq!(press(&mut state, Key::Enter), []);
        update(&mut state, Msg::ApprovalArmed(2));
        press(&mut state, Key::Char('y'));
        // A resize ends the showing at once, before any frame.
        update(&mut state, Msg::Resize { cols: 80, rows: 24 });
        assert!(!state.approval_chosen());
        assert_eq!(press(&mut state, Key::Enter), []);
        update(&mut state, Msg::ApprovalArmed(2));
        assert_eq!(press(&mut state, Key::Char('y')), []);
        assert_eq!(press(&mut state, Key::Enter), []);
        state.approval_on_screen = Some(4);
        assert_eq!(state.approval_drawn(), Some(3));
        update(&mut state, Msg::ApprovalArmed(3));
        press(&mut state, Key::Char('y'));
        assert_eq!(
            press(&mut state, Key::Enter),
            answer(4, ApprovalAnswer::AllowOnce)
        );
    }

    #[test]
    fn the_arrow_and_page_keys_scroll_the_detail_and_keep_the_choice() {
        let mut state = asking("", 3);
        state.approval_page = 4;
        press(&mut state, Key::Char('y'));
        press(&mut state, Key::PageDown);
        press(&mut state, Key::Down);
        assert_eq!(state.approval_scroll, 5);
        press(&mut state, Key::Up);
        press(&mut state, Key::PageUp);
        press(&mut state, Key::PageUp);
        assert_eq!(state.approval_scroll, 0);
        assert!(state.approval_chosen());
    }

    #[test]
    fn ctrl_c_keeps_its_quit_gesture_while_an_approval_waits() {
        let mut state = asking("", 1);
        assert_eq!(chord(&mut state, Key::Char('c'), Mods::CTRL), []);
        assert!(state.quitting());
        assert_eq!(
            chord(&mut state, Key::Char('c'), Mods::CTRL),
            [Effect::Quit]
        );
    }

    // Bake: the banner's tips appear after a moment, turn on the clock,
    // wake the loop for each change, and go for good with the first turn.
    #[test]
    fn the_banner_tips_rotate_and_the_first_turn_ends_them() {
        use crate::transcript::{Banner, Row};
        let banner = Row::Banner(Banner {
            version: "0.4.2".into(),
            directory: "~".into(),
            session: "new".into(),
            access: Vec::new(),
            tip: None,
        });
        let tip = |state: &State| match &state.transcript.rows[0] {
            Row::Banner(banner) => banner.tip.clone(),
            other => panic!("{other:?}"),
        };
        let mut state = State::live(FrameStyle::default(), Tones::None, vec![banner]);
        assert_eq!(state.next_change(), None);
        state.start_tips(0);
        assert_eq!(tip(&state), None);
        assert_eq!(state.next_change(), Some(tips::DELAY));
        update(&mut state, Msg::Tick(tips::DELAY));
        assert_eq!(tip(&state).as_deref(), Some(crate::copy::TIPS[0]));
        assert_eq!(state.next_change(), Some(tips::EVERY));
        update(&mut state, Msg::Tick(tips::DELAY + tips::EVERY));
        let second = tip(&state);
        assert!(second.is_some() && second.as_deref() != Some(crate::copy::TIPS[0]));
        runtime(&mut state, RuntimeUpdate::TurnStarted { seed: "s".into() });
        assert_eq!(tip(&state), None);
        update(&mut state, Msg::Tick(tips::DELAY + tips::EVERY * 2));
        assert_eq!(tip(&state), None);
        // Left alone, the last tip goes and nothing more is scheduled.
        let mut idle = State::live(
            FrameStyle::default(),
            Tones::None,
            vec![Row::Banner(Banner {
                version: "0.4.2".into(),
                directory: "~".into(),
                session: "new".into(),
                access: Vec::new(),
                tip: None,
            })],
        );
        idle.start_tips(3);
        let end = tips::DELAY + tips::EVERY * u32::try_from(tips::SHOWN).unwrap();
        update(&mut idle, Msg::Tick(end - Duration::from_millis(1)));
        assert!(tip(&idle).is_some());
        update(&mut idle, Msg::Tick(end));
        assert_eq!(tip(&idle), None);
        assert_eq!(idle.next_change(), None);
    }
}
