//! Decodes Crossterm events into the view's messages. Key releases, focus
//! changes, motion with no button held, and buttons other than the primary
//! one produce nothing.

use bake_tui_view::keys::{Key, KeyInput, Mods};
use std::time::Duration;

use bake_tui_view::state::{Mouse, MouseKind, Msg};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

/// Decodes `event`, read at `at` on the loop's clock.
pub fn decode(event: Event, at: Duration) -> Option<Msg> {
    match event {
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
        decode(event, Duration::ZERO)
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
                Some(Msg::Key(KeyInput::new(Key::Char('j'), Mods::CTRL)))
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
