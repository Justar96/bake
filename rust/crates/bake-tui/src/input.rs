//! Decodes Crossterm events into the view's messages. Key releases, focus
//! changes, and mouse reports produce nothing; the preview requests neither
//! focus nor mouse reporting.

use bake_tui_view::keys::{Key, KeyInput, Mods};
use bake_tui_view::state::Msg;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

pub fn decode(event: Event) -> Option<Msg> {
    match event {
        Event::Key(key) if key.kind != KeyEventKind::Release => Some(Msg::Key(key_input(key))),
        Event::Paste(text) => Some(Msg::Paste(text)),
        Event::Resize(cols, rows) => Some(Msg::Resize { cols, rows }),
        _ => None,
    }
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
                decode(key(
                    KeyCode::Char('j'),
                    KeyModifiers::CONTROL | KeyModifiers::SUPER,
                    kind
                )),
                Some(Msg::Key(KeyInput::new(Key::Char('j'), Mods::CTRL)))
            );
        }
        assert_eq!(
            decode(key(
                KeyCode::Enter,
                KeyModifiers::ALT | KeyModifiers::SHIFT,
                KeyEventKind::Press
            )),
            Some(Msg::Key(KeyInput::new(Key::Enter, Mods::ALT | Mods::SHIFT)))
        );
        assert_eq!(
            decode(key(
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
            decode(key(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
                KeyEventKind::Release
            )),
            None
        );
        assert_eq!(decode(Event::FocusGained), None);
        assert_eq!(
            decode(Event::Mouse(MouseEvent {
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
            decode(Event::Paste("a\r\nb".into())),
            Some(Msg::Paste("a\r\nb".into()))
        );
        assert_eq!(
            decode(Event::Resize(100, 30)),
            Some(Msg::Resize {
                cols: 100,
                rows: 30
            })
        );
    }
}
