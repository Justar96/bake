//! Preview state and key handling. Nothing here touches the terminal.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::editor::Draft;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Composer,
    AgentList,
    /// Read-only inspection of the sample agent with this id.
    Inspect(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    NoModel,
    ReadOnly,
    ListKeys,
    DraftLimit,
}

/// Presentation state: the parent draft, keyboard focus, and one notice.
/// Navigation never replaces the draft, so its caret and undo survive it.
#[derive(Debug)]
pub struct App {
    pub draft: Draft,
    pub focus: Focus,
    pub selected: &'static str,
    pub notice: Option<Notice>,
    quit: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            draft: Draft::default(),
            focus: Focus::Composer,
            selected: SAMPLE_AGENTS[0].id,
            notice: None,
            quit: false,
        }
    }
}

impl App {
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    pub fn selected_agent(&self) -> &'static SampleAgent {
        agent(self.selected).unwrap_or(&SAMPLE_AGENTS[0])
    }

    /// Applies one terminal event. Returns whether the screen needs a redraw.
    pub fn handle_event(&mut self, event: Event) -> bool {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                self.handle_key(key);
                true
            }
            Event::Paste(text) => {
                match self.focus {
                    Focus::Composer => {
                        let complete = self.draft.paste(&text);
                        self.notice = (!complete).then_some(Notice::DraftLimit);
                    }
                    Focus::AgentList => self.notice = Some(Notice::ListKeys),
                    Focus::Inspect(_) => self.notice = Some(Notice::ReadOnly),
                }
                true
            }
            Event::Resize(..) => true,
            _ => false,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match self.focus {
            Focus::Composer => self.composer_key(key),
            Focus::AgentList => self.list_key(key),
            Focus::Inspect(_) => self.inspect_key(key),
        }
    }

    fn composer_key(&mut self, key: KeyEvent) {
        let mods = key.modifiers;
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        let alt = mods.contains(KeyModifiers::ALT);
        let mut complete = true;
        match key.code {
            KeyCode::Tab => {
                self.focus = Focus::AgentList;
                self.notice = None;
                return;
            }
            KeyCode::Esc => {
                self.notice = None;
                return;
            }
            KeyCode::Enter if alt => complete = self.draft.newline(),
            KeyCode::Char('j') if ctrl => complete = self.draft.newline(),
            KeyCode::Enter => {
                self.notice = Some(Notice::NoModel);
                return;
            }
            KeyCode::Char('z') if ctrl => {
                self.draft.undo();
            }
            KeyCode::Backspace => self.draft.backspace(),
            KeyCode::Delete => self.draft.delete(),
            KeyCode::Left => self.draft.left(),
            KeyCode::Right => self.draft.right(),
            KeyCode::Home => self.draft.home(),
            KeyCode::End => self.draft.end(),
            // AltGr arrives as Ctrl+Alt on Windows; it still types a character.
            KeyCode::Char(c) if ctrl == alt => {
                let mut buf = [0; 4];
                complete = self.draft.type_text(c.encode_utf8(&mut buf));
            }
            _ => return,
        }
        self.notice = (!complete).then_some(Notice::DraftLimit);
    }

    fn list_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.step_selection(-1),
            KeyCode::Down => self.step_selection(1),
            KeyCode::Enter => {
                self.focus = Focus::Inspect(self.selected);
                self.notice = None;
            }
            KeyCode::Esc | KeyCode::Tab => {
                self.focus = Focus::Composer;
                self.notice = None;
            }
            _ => self.notice = Some(Notice::ListKeys),
        }
    }

    fn inspect_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.focus = Focus::Composer;
                self.notice = None;
            }
            KeyCode::Tab => {
                self.focus = Focus::AgentList;
                self.notice = None;
            }
            _ => self.notice = Some(Notice::ReadOnly),
        }
    }

    /// Moves the selection by identity, so a reordered list keeps the same agent.
    fn step_selection(&mut self, delta: isize) {
        let index = SAMPLE_AGENTS
            .iter()
            .position(|a| a.id == self.selected)
            .unwrap_or(0);
        let next = index
            .saturating_add_signed(delta)
            .min(SAMPLE_AGENTS.len() - 1);
        self.selected = SAMPLE_AGENTS[next].id;
        self.notice = None;
    }
}

pub fn agent(id: &str) -> Option<&'static SampleAgent> {
    SAMPLE_AGENTS.iter().find(|a| a.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(app: &mut App, code: KeyCode) {
        app.handle_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn chord(app: &mut App, code: KeyCode, mods: KeyModifiers) {
        app.handle_event(Event::Key(KeyEvent::new(code, mods)));
    }

    fn type_str(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    #[test]
    fn enter_is_refused_and_keeps_the_draft() {
        let mut app = App::default();
        type_str(&mut app, "hello");
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.notice, Some(Notice::NoModel));
        assert_eq!((app.draft.text(), app.draft.caret()), ("hello", 4));
    }

    #[test]
    fn newline_keys_and_paste_never_submit() {
        let mut app = App::default();
        type_str(&mut app, "a");
        chord(&mut app, KeyCode::Enter, KeyModifiers::ALT);
        chord(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
        app.handle_event(Event::Paste("b\r\nc".into()));
        assert_eq!(app.draft.text(), "a\n\nb\nc");
        assert_eq!(app.notice, None);
    }

    #[test]
    fn inspection_blocks_edits_and_returning_restores_the_draft() {
        let mut app = App::default();
        type_str(&mut app, "abc");
        app.handle_event(Event::Paste("XY".into()));
        press(&mut app, KeyCode::Left);
        let before = (app.draft.text().to_owned(), app.draft.caret());

        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus, Focus::Inspect("sample-reviewer"));
        type_str(&mut app, "zz");
        press(&mut app, KeyCode::Backspace);
        chord(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        app.handle_event(Event::Paste("pasted".into()));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.notice, Some(Notice::ReadOnly));

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Composer);
        assert_eq!((app.draft.text().to_owned(), app.draft.caret()), before);
        chord(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(app.draft.text(), "abc");
    }

    #[test]
    fn list_selection_follows_identity_and_stops_at_the_ends() {
        let mut app = App::default();
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.selected, "sample-explorer");
        for _ in 0..5 {
            press(&mut app, KeyCode::Down);
        }
        assert_eq!(app.selected, SAMPLE_AGENTS.last().unwrap().id);
        type_str(&mut app, "q");
        assert_eq!(app.notice, Some(Notice::ListKeys));
        assert!(app.draft.is_empty());
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Composer);
        assert_eq!(app.selected, SAMPLE_AGENTS.last().unwrap().id);
    }

    #[test]
    fn ctrl_c_quits_from_every_focus() {
        for focus in [
            Focus::Composer,
            Focus::AgentList,
            Focus::Inspect("sample-explorer"),
        ] {
            let mut app = App {
                focus,
                ..App::default()
            };
            chord(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
            assert!(app.should_quit());
        }
    }

    #[test]
    fn key_release_events_are_ignored() {
        let mut app = App::default();
        let mut key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        key.kind = KeyEventKind::Release;
        assert!(!app.handle_event(Event::Key(key)));
        assert!(app.draft.is_empty());
    }
}
