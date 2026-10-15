//! Dialogs a live runtime opens over the composer: a list to choose from,
//! for `/model`, `/thinking`, and `/settings`, and a one-line field, for
//! the CLIProxyAPI sign-in's address and key.
//!
//! The list follows Pi v1.2.0's selectors
//! (`packages/coding-agent/src/modes/interactive/components/model-selector.ts`,
//! `thinking-selector.ts`, `settings-selector.ts`). The current item is
//! marked `✓` and stays marked while another is browsed. Typing filters a
//! filterable list and moves the selection to its first row (Pi regression
//! 7209). Enter chooses for this session. Ctrl+S chooses and sets the
//! default where the list offers it (`app.models.save`/`app.thinking.save`).
//! Esc cancels.
//!
//! The runtime owns every dialog. It opens one with
//! [`crate::runtime::RuntimeUpdate::Dialog`], and the view answers it once
//! by its `id` with [`crate::state::Effect::Dialog`]. The runtime ignores
//! an answer for a dialog it no longer shows. After answering, the view
//! keeps the dialog on screen, waiting, and takes no other answer until
//! the runtime opens the next dialog or closes this one.
//!
//! Each open, change, or close hands the keyboard over the way an approval
//! request is armed ([`crate::state::State::approval_drawn`]): once a frame
//! shows the new state, the terminal owner's input thread reads every key
//! already waiting and sends it ahead of [`crate::state::Msg::ApprovalArmed`],
//! and the view drops keys and pastes until that arming. A held or repeated
//! Enter, typeahead, or the rest of an unbracketed paste therefore never
//! reaches a dialog the person has not seen, nor the draft after one
//! closes. Within a dialog, only a fresh Enter answers: not a reported
//! repeat, and not one read in a burst with other keys.
//!
//! A secret field's text lives only in [`SecretText`]. It never enters the
//! draft, its undo or kill ring, the prompt history, the transcript, or a
//! `Debug` rendering. It is drawn masked, and leaves the view only in its
//! answer.

use std::fmt;

use crate::keys::{Key, KeyInput, Mods};

/// Text typed into a secret field. `Debug` shows only its length.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretText(String);

impl SecretText {
    /// The text, for the one place that sends it on.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Characters typed.
    pub fn chars(&self) -> usize {
        self.0.chars().count()
    }
}

impl From<String> for SecretText {
    fn from(text: String) -> Self {
        Self(text)
    }
}

impl fmt::Debug for SecretText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretText(<{} chars>)", self.chars())
    }
}

/// One row of a list dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListItem {
    /// What an answer names.
    pub value: String,
    /// What the row shows.
    pub label: String,
    /// Dim text after the label, such as a model's provider.
    pub detail: Option<String>,
}

/// A list to choose from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListDialog {
    pub title: String,
    pub items: Vec<ListItem>,
    /// The item that is in effect now, marked `✓` and selected first.
    pub current: Option<usize>,
    /// Whether typing filters the rows.
    pub filterable: bool,
    /// Whether Ctrl+S chooses and sets the default.
    pub saves_default: bool,
    /// A dim line under the rows, such as why a list is short.
    pub note: Option<String>,
}

/// A one-line field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDialog {
    pub title: String,
    /// The line above the field, such as what to enter.
    pub prompt: String,
    /// Drawn masked, and kept in [`SecretText`].
    pub secret: bool,
    /// The text the field starts with. A runtime never sends a secret here.
    pub initial: String,
    /// Why the last answer was not taken, shown under the field. A runtime
    /// never puts a secret here.
    pub error: Option<String>,
}

/// What a dialog asks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogKind {
    List(ListDialog),
    Field(FieldDialog),
}

/// A dialog the runtime opens. A new `id` is a new dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogRequest {
    pub id: u64,
    pub kind: DialogKind,
}

/// The person's answer to a dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogAnswer {
    /// A list item's value. `save_default` when it was chosen with Ctrl+S.
    Chose { value: String, save_default: bool },
    /// A field's text.
    Entered(SecretText),
    /// Esc.
    Cancel,
}

/// An open dialog and what the person has done in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dialog {
    pub request: DialogRequest,
    /// The list's filter.
    pub filter: String,
    /// The selected row among the filtered ones.
    pub selected: usize,
    /// The field's text.
    pub text: SecretText,
    /// Answered; waiting for the runtime's next dialog or close.
    pub answered: bool,
    /// Esc was sent; nothing more is.
    pub cancelled: bool,
}

impl Dialog {
    /// Opens `request`, its current item selected and its field holding
    /// its initial text.
    pub fn open(request: DialogRequest) -> Self {
        let (selected, text) = match &request.kind {
            DialogKind::List(list) => (list.current.unwrap_or(0), String::new()),
            DialogKind::Field(field) => (0, field.initial.clone()),
        };
        Self {
            request,
            filter: String::new(),
            selected,
            text: SecretText(text),
            answered: false,
            cancelled: false,
        }
    }

    /// The list's rows that match the filter, as indexes into its items.
    pub fn rows(&self) -> Vec<usize> {
        let DialogKind::List(list) = &self.request.kind else {
            return Vec::new();
        };
        let words: Vec<String> = self
            .filter
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        list.items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                let haystack = format!(
                    "{} {}",
                    item.label.to_lowercase(),
                    item.detail.as_deref().unwrap_or("").to_lowercase()
                );
                words.iter().all(|word| haystack.contains(word.as_str()))
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// Applies a key. `fresh` is false for a reported repeat and for a key
    /// read in a burst with others, neither of which answers. Returns the
    /// answer when the key gives one.
    pub fn key(&mut self, input: KeyInput, fresh: bool) -> Option<DialogAnswer> {
        // Esc cancels once, also while an answer is being worked on, such
        // as a held check of the proxy.
        if input.key == Key::Esc && input.mods == Mods::NONE {
            if self.cancelled {
                return None;
            }
            self.answered = true;
            self.cancelled = true;
            return Some(DialogAnswer::Cancel);
        }
        if self.answered {
            return None;
        }
        let typed = |input: KeyInput| match input.key {
            Key::Char(c) if input.mods == Mods::NONE || input.mods == Mods::SHIFT => Some(c),
            _ => None,
        };
        match self.request.kind.clone() {
            DialogKind::List(list) => {
                let rows = self.rows();
                match input.key {
                    Key::Up if input.mods == Mods::NONE && !rows.is_empty() => {
                        self.selected = (self.selected + rows.len() - 1) % rows.len();
                    }
                    Key::Down if input.mods == Mods::NONE && !rows.is_empty() => {
                        self.selected = (self.selected + 1) % rows.len();
                    }
                    Key::Enter if fresh && input.mods == Mods::NONE => {
                        return self.choose(&rows, &list, false);
                    }
                    Key::Char('s') if fresh && input.mods == Mods::CTRL && list.saves_default => {
                        return self.choose(&rows, &list, true);
                    }
                    Key::Backspace if list.filterable && input.mods == Mods::NONE => {
                        if self.filter.pop().is_some() {
                            self.selected = 0;
                        }
                    }
                    _ => {
                        if let Some(c) = typed(input).filter(|_| list.filterable) {
                            self.filter.push(c);
                            self.selected = 0;
                        }
                    }
                }
                None
            }
            DialogKind::Field(_) => {
                match input.key {
                    Key::Enter if fresh && input.mods == Mods::NONE => {
                        self.answered = true;
                        return Some(DialogAnswer::Entered(std::mem::take(&mut self.text)));
                    }
                    Key::Backspace if input.mods == Mods::NONE => {
                        self.text.0.pop();
                    }
                    Key::Char('u') if input.mods == Mods::CTRL => self.text.0.clear(),
                    _ => {
                        if let Some(c) = typed(input).filter(|c| !c.is_control()) {
                            self.text.0.push(c);
                        }
                    }
                }
                None
            }
        }
    }

    /// A bracketed paste: into the field, line breaks and other control
    /// characters left out, so it never answers; into a filterable list's
    /// filter. Ignored once answered.
    pub fn paste(&mut self, text: &str) {
        if self.answered {
            return;
        }
        let clean = text.chars().filter(|c| !c.is_control());
        match &self.request.kind {
            DialogKind::Field(_) => self.text.0.extend(clean),
            DialogKind::List(list) if list.filterable => {
                self.filter.extend(clean);
                self.selected = 0;
            }
            DialogKind::List(_) => {}
        }
    }

    fn choose(&mut self, rows: &[usize], list: &ListDialog, save: bool) -> Option<DialogAnswer> {
        let item = list.items.get(*rows.get(self.selected)?)?;
        self.answered = true;
        Some(DialogAnswer::Chose {
            value: item.value.clone(),
            save_default: save,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Key) -> KeyInput {
        KeyInput {
            key,
            mods: Mods::NONE,
        }
    }

    fn list(items: &[&str], current: Option<usize>, filterable: bool) -> Dialog {
        Dialog::open(DialogRequest {
            id: 1,
            kind: DialogKind::List(ListDialog {
                title: "Model".into(),
                items: items
                    .iter()
                    .map(|id| ListItem {
                        value: (*id).into(),
                        label: (*id).into(),
                        detail: Some("faux".into()),
                    })
                    .collect(),
                current,
                filterable,
                saves_default: true,
                note: None,
            }),
        })
    }

    fn field(secret: bool) -> Dialog {
        Dialog::open(DialogRequest {
            id: 2,
            kind: DialogKind::Field(FieldDialog {
                title: "CLIProxyAPI".into(),
                prompt: "API key".into(),
                secret,
                initial: String::new(),
                error: None,
            }),
        })
    }

    // Pi model-selector.test.ts "keeps the current model marked while
    // browsing" and thinking-selector.test.ts: the current item is selected
    // first; browsing moves the selection, not the mark.
    #[test]
    fn the_current_item_is_selected_first() {
        let mut dialog = list(&["current", "browsed"], Some(0), false);
        assert_eq!(dialog.selected, 0);
        dialog.key(key(Key::Down), true);
        assert_eq!(dialog.selected, 1);
        assert_eq!(
            dialog.key(key(Key::Enter), true),
            Some(DialogAnswer::Chose {
                value: "browsed".into(),
                save_default: false,
            })
        );
    }

    // Pi 7209-model-selector-filter-resets-selection.test.ts: typing a
    // query moves the selection to the first matching row.
    #[test]
    fn typing_a_filter_selects_its_first_row() {
        let mut dialog = list(&["alpha-1", "beta-1", "alpha-2", "alpha-3"], Some(3), true);
        for c in "alpha".chars() {
            dialog.key(key(Key::Char(c)), true);
        }
        assert_eq!(dialog.rows(), [0, 2, 3]);
        assert_eq!(dialog.selected, 0);
        dialog.key(key(Key::Down), true);
        dialog.key(key(Key::Char('-')), true);
        assert_eq!(dialog.selected, 0);
        dialog.key(key(Key::Backspace), true);
        assert_eq!(dialog.rows(), [0, 2, 3]);
    }

    // Pi model-selector.test.ts and thinking-selector.test.ts "uses the
    // configured save binding": the save key chooses and sets the default;
    // Bake binds Pi's default, Ctrl+S.
    #[test]
    fn ctrl_s_chooses_and_saves() {
        let mut dialog = list(&["a", "b"], Some(1), false);
        assert_eq!(
            dialog.key(
                KeyInput {
                    key: Key::Char('s'),
                    mods: Mods::CTRL,
                },
                true
            ),
            Some(DialogAnswer::Chose {
                value: "b".into(),
                save_default: true,
            })
        );
    }

    // Bake: an answered dialog takes no second answer until the runtime
    // opens the next one, and neither a repeated nor a burst Enter answers.
    #[test]
    fn only_a_fresh_enter_answers_and_only_once() {
        let mut dialog = field(true);
        for c in "sk-fake".chars() {
            dialog.key(key(Key::Char(c)), false);
        }
        assert_eq!(dialog.key(key(Key::Enter), false), None);
        assert!(!dialog.answered);
        let answer = dialog.key(key(Key::Enter), true);
        assert_eq!(
            answer,
            Some(DialogAnswer::Entered("sk-fake".to_owned().into()))
        );
        assert_eq!(dialog.text.chars(), 0);
        assert_eq!(dialog.key(key(Key::Enter), true), None);
        assert_eq!(dialog.key(key(Key::Char('x')), true), None);
        assert_eq!(dialog.text.chars(), 0);
        // Esc still cancels the work on that answer, once.
        assert_eq!(dialog.key(key(Key::Esc), true), Some(DialogAnswer::Cancel));
        assert_eq!(dialog.key(key(Key::Esc), true), None);
    }

    // Bake: a paste stays in the field, line breaks dropped, and never
    // answers it.
    #[test]
    fn a_paste_stays_in_the_field() {
        let mut dialog = field(true);
        dialog.paste("sk-fake-\r\nrest\n");
        assert!(!dialog.answered);
        assert_eq!(dialog.text.expose(), "sk-fake-rest");
        let shown = format!("{dialog:?}");
        assert!(!shown.contains("sk-fake"), "{shown}");
    }
}
