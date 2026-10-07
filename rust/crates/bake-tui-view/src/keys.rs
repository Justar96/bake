//! Bake's own key type and the one table that binds keys to actions.
//!
//! The terminal owner decodes its backend's events into [`KeyInput`], so no
//! widget or backend decides what a key does. [`action`] reads [`BINDINGS`]
//! top to bottom and returns the first row that matches the focus and key.

use std::ops::BitOr;

/// A key, without its modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    BackTab,
    Esc,
    Backspace,
    Delete,
    Insert,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
    /// A key Bake binds nothing to, such as a media or lock key.
    Other,
}

/// Held modifier keys. Platform keys beyond these three are not reported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods(u8);

impl Mods {
    pub const NONE: Self = Self(0);
    pub const SHIFT: Self = Self(1);
    pub const CTRL: Self = Self(1 << 1);
    pub const ALT: Self = Self(1 << 2);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

impl BitOr for Mods {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// One key press or repeat; releases are never delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyInput {
    pub key: Key,
    pub mods: Mods,
}

impl KeyInput {
    pub const fn new(key: Key, mods: Mods) -> Self {
        Self { key, mods }
    }

    pub const fn plain(key: Key) -> Self {
        Self::new(key, Mods::NONE)
    }

    /// Whether the key types its character. Ctrl and Alt together count as
    /// typing, because Windows reports AltGr that way.
    pub fn types(self) -> Option<char> {
        match self.key {
            Key::Char(c) if self.mods.contains(Mods::CTRL) == self.mods.contains(Mods::ALT) => {
                Some(c)
            }
            _ => None,
        }
    }
}

/// Where a binding applies: the focused region, or anywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Anywhere,
    Composer,
    AgentList,
    Inspect,
}

/// What a bound key asks the view to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Ctrl+C: the first press arms a quit, a second within
    /// [`crate::state::QUIT_WINDOW`] quits. It never interrupts a turn.
    Quit,
    /// Ctrl+G: opens the agent list from the composer and closes it again.
    ToggleAgentList,
    /// Esc in the composer: stops the activity and dismisses the notice.
    Interrupt,
    ToggleSampleActivity,
    Newline,
    Submit,
    Undo,
    Backspace,
    /// By word: Ctrl+←/→, Alt+←/→, and Alt+B/F.
    WordLeft,
    WordRight,
    /// Ctrl+W and Alt+Backspace kill back to a word stop; Alt+D, Alt+Delete,
    /// and Ctrl+Delete kill on to one.
    KillWordLeft,
    KillWordRight,
    /// Ctrl+U and Ctrl+K: to the logical line's start and end.
    KillLineLeft,
    KillLineRight,
    /// Ctrl+Y puts back the last kill; Alt+Y right after cycles older ones.
    Yank,
    YankPop,
    /// Up and Down: the drawn row above or below, then input history from
    /// the first or the last row.
    CaretUp,
    CaretDown,
    /// Ctrl+P and Ctrl+N: input history at once, from any row.
    RecallOlder,
    RecallNewer,
    /// Ctrl+V: stage the clipboard's image.
    PasteImage,
    /// Ctrl+A and Ctrl+E: the logical line's start and end, past any row.
    LogicalStart,
    LogicalEnd,
    Delete,
    Left,
    Right,
    LineStart,
    LineEnd,
    SelectPrevious,
    SelectNext,
    InspectSelected,
    ReturnToComposer,
    ReturnToAgentList,
    /// Transcript navigation, from the composer.
    PageUp,
    PageDown,
    PreviousPrompt,
    NextPrompt,
    ToStart,
    ToLatest,
}

/// One row of [`BINDINGS`]: `key` in `scope`, with every modifier in
/// `require` held and none in `forbid`. Modifiers in neither are ignored.
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    pub scope: Scope,
    pub key: Key,
    pub require: Mods,
    pub forbid: Mods,
    pub action: Action,
}

const fn bind(scope: Scope, key: Key, require: Mods, forbid: Mods, action: Action) -> Binding {
    Binding {
        scope,
        key,
        require,
        forbid,
        action,
    }
}

use Action as A;
use Key as K;
use Scope as S;
const ANY: Mods = Mods::NONE;

/// Every key binding, in match order. Keys a row does not bind fall through
/// to the focus's default: typing in the composer, and a hint elsewhere.
pub const BINDINGS: &[Binding] = &[
    bind(S::Anywhere, K::Char('c'), Mods::CTRL, ANY, A::Quit),
    bind(
        S::Composer,
        K::Char('g'),
        Mods::CTRL,
        Mods::ALT,
        A::ToggleAgentList,
    ),
    bind(S::Composer, K::Esc, ANY, ANY, A::Interrupt),
    bind(
        S::Composer,
        K::Char('t'),
        Mods::CTRL,
        Mods::ALT,
        A::ToggleSampleActivity,
    ),
    bind(S::Composer, K::Enter, Mods::ALT, ANY, A::Newline),
    bind(S::Composer, K::Char('j'), Mods::CTRL, ANY, A::Newline),
    bind(S::Composer, K::Enter, ANY, ANY, A::Submit),
    // Ctrl+-, Ctrl+_, and Ctrl+/ send the unit separator, which legacy
    // decoding reads as Ctrl+7; a terminal reporting keys in full names them.
    bind(S::Composer, K::Char('-'), Mods::CTRL, Mods::ALT, A::Undo),
    bind(S::Composer, K::Char('_'), Mods::CTRL, Mods::ALT, A::Undo),
    bind(S::Composer, K::Char('/'), Mods::CTRL, Mods::ALT, A::Undo),
    bind(S::Composer, K::Char('7'), Mods::CTRL, Mods::ALT, A::Undo),
    // Readline editing, as the TypeScript composer's `editKey` binds it.
    bind(S::Composer, K::Backspace, Mods::ALT, ANY, A::KillWordLeft),
    bind(S::Composer, K::Delete, Mods::ALT, ANY, A::KillWordRight),
    bind(S::Composer, K::Delete, Mods::CTRL, ANY, A::KillWordRight),
    bind(S::Composer, K::Left, Mods::CTRL, ANY, A::WordLeft),
    bind(S::Composer, K::Left, Mods::ALT, ANY, A::WordLeft),
    bind(S::Composer, K::Right, Mods::CTRL, ANY, A::WordRight),
    bind(S::Composer, K::Right, Mods::ALT, ANY, A::WordRight),
    bind(
        S::Composer,
        K::Char('b'),
        Mods::ALT,
        Mods::CTRL,
        A::WordLeft,
    ),
    bind(
        S::Composer,
        K::Char('f'),
        Mods::ALT,
        Mods::CTRL,
        A::WordRight,
    ),
    bind(
        S::Composer,
        K::Char('d'),
        Mods::ALT,
        Mods::CTRL,
        A::KillWordRight,
    ),
    bind(S::Composer, K::Char('y'), Mods::ALT, Mods::CTRL, A::YankPop),
    bind(
        S::Composer,
        K::Char('w'),
        Mods::CTRL,
        Mods::ALT,
        A::KillWordLeft,
    ),
    bind(
        S::Composer,
        K::Char('u'),
        Mods::CTRL,
        Mods::ALT,
        A::KillLineLeft,
    ),
    bind(
        S::Composer,
        K::Char('k'),
        Mods::CTRL,
        Mods::ALT,
        A::KillLineRight,
    ),
    bind(S::Composer, K::Char('y'), Mods::CTRL, Mods::ALT, A::Yank),
    bind(S::Composer, K::Char('b'), Mods::CTRL, Mods::ALT, A::Left),
    bind(S::Composer, K::Char('f'), Mods::CTRL, Mods::ALT, A::Right),
    bind(S::Composer, K::Char('d'), Mods::CTRL, Mods::ALT, A::Delete),
    bind(
        S::Composer,
        K::Char('v'),
        Mods::CTRL,
        Mods::ALT,
        A::PasteImage,
    ),
    bind(
        S::Composer,
        K::Char('a'),
        Mods::CTRL,
        Mods::ALT,
        A::LogicalStart,
    ),
    bind(
        S::Composer,
        K::Char('e'),
        Mods::CTRL,
        Mods::ALT,
        A::LogicalEnd,
    ),
    bind(S::Composer, K::Backspace, ANY, ANY, A::Backspace),
    bind(S::Composer, K::Delete, ANY, ANY, A::Delete),
    bind(S::Composer, K::Left, ANY, ANY, A::Left),
    bind(S::Composer, K::Right, ANY, ANY, A::Right),
    bind(S::Composer, K::PageUp, ANY, ANY, A::PageUp),
    bind(S::Composer, K::PageDown, ANY, ANY, A::PageDown),
    bind(S::Composer, K::Up, Mods::CTRL, ANY, A::PreviousPrompt),
    bind(S::Composer, K::Down, Mods::CTRL, ANY, A::NextPrompt),
    bind(S::Composer, K::Home, Mods::CTRL, ANY, A::ToStart),
    bind(S::Composer, K::End, Mods::CTRL, ANY, A::ToLatest),
    // After Ctrl+↑ and Ctrl+↓, which move the transcript. Alt+↑ is the
    // oracle's steering key, so neither arrow moves with Alt.
    bind(S::Composer, K::Up, ANY, Mods::ALT, A::CaretUp),
    bind(S::Composer, K::Down, ANY, Mods::ALT, A::CaretDown),
    bind(
        S::Composer,
        K::Char('p'),
        Mods::CTRL,
        Mods::ALT,
        A::RecallOlder,
    ),
    bind(
        S::Composer,
        K::Char('n'),
        Mods::CTRL,
        Mods::ALT,
        A::RecallNewer,
    ),
    bind(S::Composer, K::Home, ANY, ANY, A::LineStart),
    bind(S::Composer, K::End, ANY, ANY, A::LineEnd),
    bind(S::AgentList, K::Up, ANY, ANY, A::SelectPrevious),
    bind(S::AgentList, K::Down, ANY, ANY, A::SelectNext),
    bind(S::AgentList, K::Enter, ANY, ANY, A::InspectSelected),
    bind(S::AgentList, K::Esc, ANY, ANY, A::ReturnToComposer),
    bind(
        S::AgentList,
        K::Char('g'),
        Mods::CTRL,
        Mods::ALT,
        A::ReturnToComposer,
    ),
    bind(S::Inspect, K::Esc, ANY, ANY, A::ReturnToComposer),
    bind(S::Inspect, K::Tab, ANY, ANY, A::ReturnToAgentList),
];

/// The first binding for `input` in `scope`, or one that applies anywhere.
pub fn action(scope: Scope, input: KeyInput) -> Option<Action> {
    BINDINGS
        .iter()
        .find(|b| {
            (b.scope == Scope::Anywhere || b.scope == scope)
                && b.key == input.key
                && input.mods.contains(b.require)
                && !input.mods.intersects(b.forbid)
        })
        .map(|b| b.action)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL_ALT: Mods = Mods(Mods::CTRL.0 | Mods::ALT.0);

    #[test]
    fn modifiers_must_include_the_required_and_exclude_the_forbidden() {
        let t = |mods| action(Scope::Composer, KeyInput::new(Key::Char('t'), mods));
        assert_eq!(t(Mods::CTRL), Some(Action::ToggleSampleActivity));
        assert_eq!(
            t(Mods::CTRL | Mods::SHIFT),
            Some(Action::ToggleSampleActivity)
        );
        assert_eq!(t(CTRL_ALT), None);
        assert_eq!(t(Mods::NONE), None);
    }

    #[test]
    fn enter_with_alt_is_a_newline_before_it_is_a_submission() {
        let enter = |mods| action(Scope::Composer, KeyInput::new(Key::Enter, mods));
        assert_eq!(enter(Mods::ALT), Some(Action::Newline));
        assert_eq!(enter(Mods::ALT | Mods::SHIFT), Some(Action::Newline));
        assert_eq!(enter(Mods::NONE), Some(Action::Submit));
        assert_eq!(enter(Mods::SHIFT), Some(Action::Submit));
    }

    #[test]
    fn ctrl_c_quits_in_every_scope() {
        let input = KeyInput::new(Key::Char('c'), Mods::CTRL);
        for scope in [Scope::Composer, Scope::AgentList, Scope::Inspect] {
            assert_eq!(action(scope, input), Some(Action::Quit));
        }
    }

    #[test]
    fn a_key_binds_only_in_its_own_scope() {
        let tab = KeyInput::plain(Key::Tab);
        assert_eq!(action(Scope::Composer, tab), None);
        assert_eq!(action(Scope::Inspect, tab), Some(Action::ReturnToAgentList));
        // Ctrl+G opens the agent list and closes it again.
        let ctrl_g = KeyInput::new(Key::Char('g'), Mods::CTRL);
        assert_eq!(
            action(Scope::Composer, ctrl_g),
            Some(Action::ToggleAgentList)
        );
        assert_eq!(
            action(Scope::AgentList, ctrl_g),
            Some(Action::ReturnToComposer)
        );
        // Undo is the unit separator however the terminal names it; Ctrl+Z
        // is not bound.
        for c in ['-', '_', '/', '7'] {
            assert_eq!(
                action(Scope::Composer, KeyInput::new(Key::Char(c), Mods::CTRL)),
                Some(Action::Undo)
            );
        }
        assert_eq!(
            action(Scope::Composer, KeyInput::new(Key::Char('z'), Mods::CTRL)),
            None
        );
        assert_eq!(
            action(Scope::Composer, KeyInput::plain(Key::Up)),
            Some(Action::CaretUp)
        );
        assert_eq!(
            action(Scope::Composer, KeyInput::new(Key::Up, Mods::ALT)),
            None
        );
        // Ctrl+Home reaches the transcript's start; Home alone, the line's.
        let home = |mods| action(Scope::Composer, KeyInput::new(Key::Home, mods));
        assert_eq!(home(Mods::CTRL), Some(Action::ToStart));
        assert_eq!(home(Mods::NONE), Some(Action::LineStart));
        assert_eq!(
            action(
                Scope::Composer,
                KeyInput::new(Key::Up, Mods::CTRL | Mods::SHIFT)
            ),
            Some(Action::PreviousPrompt)
        );
    }

    #[test]
    fn altgr_types_while_ctrl_or_alt_alone_does_not() {
        let typed = |mods| KeyInput::new(Key::Char('@'), mods).types();
        assert_eq!(typed(Mods::NONE), Some('@'));
        assert_eq!(typed(Mods::SHIFT), Some('@'));
        assert_eq!(typed(CTRL_ALT), Some('@'));
        assert_eq!(typed(Mods::CTRL), None);
        assert_eq!(typed(Mods::ALT), None);
        assert_eq!(KeyInput::plain(Key::Enter).types(), None);
    }

    #[test]
    fn every_scoped_key_binds_once_for_each_modifier_set() {
        // A later row for the same scope and key must differ in modifiers, or
        // it could never match.
        for (i, later) in BINDINGS.iter().enumerate() {
            for earlier in &BINDINGS[..i] {
                let same = earlier.scope == later.scope && earlier.key == later.key;
                assert!(
                    !(same
                        && later.require.contains(earlier.require)
                        && earlier.forbid == Mods::NONE),
                    "{later:?} is shadowed by {earlier:?}"
                );
            }
        }
    }
}
