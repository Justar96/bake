//! Presentation state and its update function. Every input arrives as a
//! [`Msg`]; [`update`] applies it and returns the [`Effect`]s the terminal
//! owner must perform. Nothing here touches the terminal or reads a clock.

use std::time::Duration;

use crate::activity::{self, Tones};
use crate::composer::ComposerWindow;
use crate::editor::Draft;
use crate::frame::FrameStyle;
use crate::keys::{self, Action, KeyInput, Scope};
use crate::mode::Mode;

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
}

/// Requests the terminal owner performs on the view's behalf.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Quit,
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
    /// The working directory as the status line reads it, already shortened
    /// against home by the terminal owner; empty when unknown.
    pub cwd: String,
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
            cwd: String::new(),
            frame,
            tones,
            now: Duration::ZERO,
            window: ComposerWindow::default(),
            samples: 0,
        }
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
        self.activity.map(|sample| {
            activity::next_change(self.now.saturating_sub(sample.started), self.tones.moves())
        })
    }
}

/// Applies one message and returns the effects it requests.
pub fn update(state: &mut State, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Key(input) => return key(state, input),
        Msg::Paste(text) => match state.focus {
            Focus::Composer => {
                let complete = state.draft.paste(&text);
                state.notice = (!complete).then_some(Notice::DraftLimit);
            }
            Focus::AgentList => state.notice = Some(Notice::ListKeys),
            Focus::Inspect(_) => state.notice = Some(Notice::ReadOnly),
        },
        Msg::Resize { .. } => {}
        Msg::Tick(now) => state.now = now,
    }
    Vec::new()
}

fn key(state: &mut State, input: KeyInput) -> Vec<Effect> {
    let bound = keys::action(state.focus.scope(), input);
    if bound == Some(Action::Quit) {
        return vec![Effect::Quit];
    }
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
        Some(Action::OpenAgentList) => {
            state.focus = Focus::AgentList;
            state.notice = None;
            return;
        }
        // Esc stops a sample the way it interrupts a turn or cancels compaction.
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
        Some(Action::LineStart) => {
            draft.home();
            true
        }
        Some(Action::LineEnd) => {
            draft.end();
            true
        }
        _ => match input.types() {
            Some(c) => draft.type_text(c.encode_utf8(&mut [0; 4])),
            None => return,
        },
    };
    state.notice = (!complete).then_some(Notice::DraftLimit);
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

        press(&mut state, Key::Tab);
        press(&mut state, Key::Down);
        press(&mut state, Key::Enter);
        assert_eq!(state.focus, Focus::Inspect("sample-reviewer"));
        type_str(&mut state, "zz");
        press(&mut state, Key::Backspace);
        chord(&mut state, Key::Char('z'), Mods::CTRL);
        update(&mut state, Msg::Paste("pasted".into()));
        press(&mut state, Key::Enter);
        assert_eq!(state.notice, Some(Notice::ReadOnly));

        press(&mut state, Key::Esc);
        assert_eq!(state.focus, Focus::Composer);
        assert_eq!((state.draft.text().to_owned(), state.draft.caret()), before);
        chord(&mut state, Key::Char('z'), Mods::CTRL);
        assert_eq!(state.draft.text(), "abc");
    }

    #[test]
    fn list_selection_follows_identity_and_stops_at_the_ends() {
        let mut state = State::default();
        press(&mut state, Key::Tab);
        press(&mut state, Key::Up);
        assert_eq!(state.selected, "sample-explorer");
        for _ in 0..5 {
            press(&mut state, Key::Down);
        }
        assert_eq!(state.selected, SAMPLE_AGENTS.last().unwrap().id);
        type_str(&mut state, "q");
        assert_eq!(state.notice, Some(Notice::ListKeys));
        assert!(state.draft.is_empty());
        press(&mut state, Key::Tab);
        assert_eq!(state.focus, Focus::Composer);
        assert_eq!(state.selected, SAMPLE_AGENTS.last().unwrap().id);
    }

    #[test]
    fn ctrl_c_requests_quit_from_every_focus_and_changes_nothing() {
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
    fn inspection_is_its_own_mode_whatever_the_session_does() {
        let mut state = State::default();
        chord(&mut state, Key::Char('t'), Mods::CTRL);
        press(&mut state, Key::Tab);
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
