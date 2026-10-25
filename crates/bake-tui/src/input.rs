//! Decodes Crossterm events into the view's messages. Key releases, focus
//! changes, motion with no button held, and buttons other than the primary
//! one produce nothing, except a character Windows delivers as an Alt code
//! ([`Decoder`]). The decoder keeps the latest focus change for the
//! terminal's indicators instead ([`Decoder::take_focus`]).

use bake_tui_view::keys::{Key, KeyInput, Mods};
use std::time::Duration;

use bake_tui_view::state::{Mouse, MouseKind, Msg};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

/// Decodes the events of one input stream, in order.
///
/// A Windows console delivers text no key produces, such as a combining
/// mark a pseudoconsole forwards, as an Alt code: the Alt press carries no
/// character, which Crossterm 0.29 drops, and the character rides on the
/// Alt release, which it reports as a release of that character (the
/// halves of a surrogate pair it joins into a press instead). Crossterm
/// reports every key's release on Windows, so the decoder tracks the
/// characters held: a released character with none held had no press and
/// is typed; any other release lets go of one held key, the released
/// character if it is held, since Shift or AltGr may change the character
/// between a key's press and its release. A character past U+FFFF, which
/// Crossterm joins from an Alt code's surrogate pair, is never held.
#[derive(Debug, Default)]
pub struct Decoder {
    windows: bool,
    held: Vec<char>,
    /// The latest focus change not yet taken: `true` when gained.
    focus: Option<bool>,
}

impl Decoder {
    /// A decoder for this platform's console.
    pub fn new() -> Self {
        Self::on(cfg!(windows))
    }

    /// A decoder for a Windows console when `windows`.
    fn on(windows: bool) -> Self {
        Self {
            windows,
            held: Vec::new(),
            focus: None,
        }
    }

    /// The latest focus change decoded since the last call, if any.
    pub fn take_focus(&mut self) -> Option<bool> {
        self.focus.take()
    }

    /// Decodes `event`, read at `at` on the loop's clock.
    pub fn decode(&mut self, event: Event, at: Duration) -> Option<Msg> {
        match event {
            Event::FocusGained => self.focus = Some(true),
            Event::FocusLost => self.focus = Some(false),
            _ => {}
        }
        if self.windows
            && let Event::Key(key) = &event
            && let KeyCode::Char(c) = key.code
        {
            match key.kind {
                KeyEventKind::Release if self.held.is_empty() => {
                    return Some(Msg::Key(key_input(KeyEvent {
                        kind: KeyEventKind::Press,
                        ..*key
                    })));
                }
                KeyEventKind::Release => {
                    let index = self.held.iter().position(|held| *held == c).unwrap_or(0);
                    self.held.remove(index);
                }
                // Crossterm makes a character past U+FFFF only from an Alt
                // code's surrogate pair, a press with no release: no key
                // is held.
                _ if u32::from(c) > 0xFFFF => {}
                // A held key repeats as presses; it is held once.
                _ if !self.held.contains(&c) => self.held.push(c),
                _ => {}
            }
        }
        decode(event, at)
    }
}

/// Decodes `event`, read at `at` on the loop's clock, with no state.
fn decode(event: Event, at: Duration) -> Option<Msg> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Repeat => Some(Msg::KeyRepeat(key_input(key))),
        Event::Key(key) if key.kind != KeyEventKind::Release => Some(Msg::Key(key_input(key))),
        Event::Paste(text) => Some(Msg::Paste(text)),
        Event::Resize(cols, rows) => Some(Msg::Resize { cols, rows }),
        Event::Mouse(mouse) => mouse_input(mouse, at).map(Msg::Mouse),
        _ => None,
    }
}

fn mouse_input(event: MouseEvent, at: Duration) -> Option<Mouse> {
    let kind = match event.kind {
        MouseEventKind::ScrollUp => MouseKind::WheelUp,
        MouseEventKind::ScrollDown => MouseKind::WheelDown,
        MouseEventKind::Down(MouseButton::Left) => MouseKind::Down,
        MouseEventKind::Drag(MouseButton::Left) => MouseKind::Drag,
        MouseEventKind::Up(MouseButton::Left) => MouseKind::Up,
        _ => return None,
    };
    Some(Mouse {
        kind,
        column: event.column,
        row: event.row,
        alt: event.modifiers.contains(KeyModifiers::ALT),
        at,
    })
}

fn key_input(event: KeyEvent) -> KeyInput {
    let key = match event.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Esc => Key::Esc,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::F(n) => Key::F(n),
        _ => Key::Other,
    };
    let held = event.modifiers;
    let mut mods = Mods::NONE;
    for (flag, bit) in [
        (KeyModifiers::SHIFT, Mods::SHIFT),
        (KeyModifiers::CONTROL, Mods::CTRL),
        (KeyModifiers::ALT, Mods::ALT),
    ] {
        if held.contains(flag) {
            mods = mods | bit;
        }
    }
    KeyInput::new(key, mods)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decodes an event read at the loop's start.
    fn at_start(event: Event) -> Option<Msg> {
        Decoder::on(false).decode(event, Duration::ZERO)
    }
    use crossterm::event::{KeyEventState, MouseEvent, MouseEventKind};

    fn key(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind,
            state: KeyEventState::NONE,
        })
    }

    #[test]
    fn presses_and_repeats_decode_with_their_modifiers() {
        for kind in [KeyEventKind::Press, KeyEventKind::Repeat] {
            assert_eq!(
                at_start(key(
                    KeyCode::Char('j'),
                    KeyModifiers::CONTROL | KeyModifiers::SUPER,
                    kind
                )),
                Some(if kind == KeyEventKind::Repeat {
                    Msg::KeyRepeat(KeyInput::new(Key::Char('j'), Mods::CTRL))
                } else {
                    Msg::Key(KeyInput::new(Key::Char('j'), Mods::CTRL))
                })
            );
        }
        assert_eq!(
            at_start(key(
                KeyCode::Enter,
                KeyModifiers::ALT | KeyModifiers::SHIFT,
                KeyEventKind::Press
            )),
            Some(Msg::Key(KeyInput::new(Key::Enter, Mods::ALT | Mods::SHIFT)))
        );
        assert_eq!(
            at_start(key(
                KeyCode::CapsLock,
                KeyModifiers::NONE,
                KeyEventKind::Press
            )),
            Some(Msg::Key(KeyInput::plain(Key::Other)))
        );
    }

    #[test]
    fn releases_focus_and_mouse_reports_decode_to_nothing() {
        assert_eq!(
            at_start(key(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
                KeyEventKind::Release
            )),
            None
        );
        assert_eq!(at_start(Event::FocusGained), None);
        assert_eq!(at_start(Event::FocusLost), None);
        assert_eq!(
            at_start(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            })),
            None
        );
    }

    fn char_key(c: char, kind: KeyEventKind) -> Event {
        key(KeyCode::Char(c), KeyModifiers::NONE, kind)
    }

    fn typed(decoder: &mut Decoder, events: Vec<Event>) -> String {
        events
            .into_iter()
            .filter_map(|event| decoder.decode(event, Duration::ZERO))
            .map(|msg| match msg {
                Msg::Key(KeyInput {
                    key: Key::Char(c), ..
                }) => c,
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    /// Crossterm's events for `漢字🙂é` typed through a Windows
    /// pseudoconsole, as conhost's records showed: each character pressed
    /// and released, the emoji joined into a press, and U+0301 only as the
    /// Alt code's release.
    #[test]
    fn a_windows_alt_code_character_is_typed_once_and_keys_are_not_doubled() {
        use KeyEventKind::{Press, Release};
        let events = vec![
            char_key('漢', Press),
            char_key('漢', Release),
            char_key('字', Press),
            char_key('字', Release),
            char_key('🙂', Press),
            char_key('e', Press),
            char_key('e', Release),
            char_key('\u{301}', Release),
        ];
        assert_eq!(
            typed(&mut Decoder::on(true), events.clone()),
            "漢字🙂e\u{301}"
        );
        // Elsewhere a release is never text.
        assert_eq!(typed(&mut Decoder::on(false), events), "漢字🙂e");
    }

    /// Releases while keys are held are never text: overlapping keys, a
    /// repeat, and a release whose character Shift changed.
    #[test]
    fn windows_releases_of_held_keys_type_nothing() {
        use KeyEventKind::{Press, Release};
        let mut decoder = Decoder::on(true);
        let events = vec![
            char_key('a', Press),
            char_key('b', Press),
            char_key('a', Release),
            char_key('b', Release),
            char_key('x', Press),
            char_key('x', Press),
            char_key('x', Release),
            char_key('A', Press),
            char_key('a', Release),
        ];
        assert_eq!(typed(&mut decoder, events), "abxxA");
        // Nothing is left held, so the next Alt code is typed.
        assert_eq!(
            typed(&mut decoder, vec![char_key('\u{301}', Release)]),
            "\u{301}"
        );
        // A released key that is not text stays nothing.
        assert_eq!(
            decoder.decode(
                key(KeyCode::Enter, KeyModifiers::NONE, Release),
                Duration::ZERO
            ),
            None
        );
    }

    #[test]
    fn the_latest_focus_change_is_kept_for_the_indicators() {
        let mut decoder = Decoder::on(false);
        assert_eq!(decoder.take_focus(), None);
        for event in [Event::FocusLost, Event::FocusGained, Event::FocusLost] {
            assert_eq!(decoder.decode(event, Duration::ZERO), None);
        }
        assert_eq!(decoder.take_focus(), Some(false));
        assert_eq!(decoder.take_focus(), None);
    }

    #[test]
    fn paste_and_resize_carry_their_data() {
        assert_eq!(
            at_start(Event::Paste("a\r\nb".into())),
            Some(Msg::Paste("a\r\nb".into()))
        );
        assert_eq!(
            at_start(Event::Resize(100, 30)),
            Some(Msg::Resize {
                cols: 100,
                rows: 30
            })
        );
    }

    #[test]
    fn the_wheel_and_the_primary_button_decode_with_their_place_and_time() {
        let event = |kind, modifiers| {
            Event::Mouse(MouseEvent {
                kind,
                column: 7,
                row: 3,
                modifiers,
            })
        };
        let at = Duration::from_millis(250);
        let wheel = decode(event(MouseEventKind::ScrollUp, KeyModifiers::ALT), at);
        assert_eq!(
            wheel,
            Some(Msg::Mouse(Mouse {
                kind: MouseKind::WheelUp,
                column: 7,
                row: 3,
                alt: true,
                at,
            }))
        );
        for (kind, expected) in [
            (MouseEventKind::ScrollDown, MouseKind::WheelDown),
            (MouseEventKind::Down(MouseButton::Left), MouseKind::Down),
            (MouseEventKind::Drag(MouseButton::Left), MouseKind::Drag),
            (MouseEventKind::Up(MouseButton::Left), MouseKind::Up),
        ] {
            let Some(Msg::Mouse(mouse)) = decode(event(kind, KeyModifiers::NONE), at) else {
                panic!("{kind:?}");
            };
            assert_eq!((mouse.kind, mouse.alt), (expected, false));
        }
        for ignored in [
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Drag(MouseButton::Middle),
            MouseEventKind::ScrollLeft,
        ] {
            assert_eq!(decode(event(ignored, KeyModifiers::NONE), at), None);
        }
    }
}
