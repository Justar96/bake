//! Dialogs a live runtime opens over the composer: a list to choose from,
//! for `/model`, `/thinking`, `/settings`, `/resume`, `/tree`, and `/fork`; a one-line field,
//! for the CLIProxyAPI sign-in's address and key and `/name`; and a
//! read-only information sheet, for `/session`.
//!
//! The list follows Pi v1.2.0's selectors
//! (`packages/coding-agent/src/modes/interactive/components/model-selector.ts`,
//! `thinking-selector.ts`, `settings-selector.ts`) and is drawn as the
//! TypeScript terminal's picker (`apps/tui/packages/ui/src/picker.tsx`)
//! ([`crate::render`]). The current item is marked `Current` and stays
//! marked while another is browsed. Typing filters a filterable list by
//! every word, in a row's label, detail, group, description, or value, and moves the
//! selection to its first row (Pi regression 7209). ↑↓ (or Ctrl+P and
//! Ctrl+N) move one row and wrap; PgUp, PgDn, Home, and End move a page or
//! to an end. Enter chooses for this session. Ctrl+S chooses and sets the
//! default where the list offers it (`app.models.save`/`app.thinking.save`).
//! Esc cancels.
//!
//! `/resume`'s list instead searches with the runtime's [`SessionSearch`],
//! Pi's session search (`session-selector-search.ts`): words matched
//! fuzzily, `"quoted phrases"`, and `re:` patterns over each session's id,
//! name, messages, and directory. Ctrl+S switches its order between recent
//! and relevance and Ctrl+N shows only named sessions (Pi's
//! `app.session.toggleSort` and `app.session.toggleNamedFilter`), so Ctrl+N
//! does not move the selection there; ↓ does. A toggle keeps the selected
//! session selected while it is still listed. The search runs once per
//! change of the query or a toggle and its rows are kept, never once per
//! frame; a query that cannot match, such as an invalid pattern, lists
//! nothing and says why, and Enter then chooses nothing.
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
use std::sync::Arc;

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

/// The order of a session search.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchOrder {
    /// The listing's order, newest first.
    #[default]
    Recent,
    /// Best match first, then newest.
    Relevance,
}

/// Why a session search's query cannot match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchError {
    /// `re:` with no pattern.
    EmptyRegex,
    /// A `re:` pattern refused, and the reason, on one line.
    InvalidRegex(String),
}

impl SearchError {
    /// What the sheet says.
    pub fn message(&self) -> String {
        match self {
            Self::EmptyRegex => crate::copy::SESSION_SEARCH_EMPTY_REGEX.to_owned(),
            Self::InvalidRegex(reason) => {
                crate::copy::SESSION_SEARCH_INVALID_REGEX.replace("{reason}", reason)
            }
        }
    }
}

/// What a session search found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchResult {
    /// Indexes into the list's items, best first.
    pub rows: Vec<usize>,
    /// Why the query matches nothing, when it cannot match.
    pub error: Option<SearchError>,
}

/// A search over a session list's items, supplied by the runtime. It must
/// be pure: the same query, order, and filter always find the same rows.
pub trait SessionFilter: Send + Sync {
    /// The items `query` finds under `order`, only named sessions when
    /// `named_only`.
    fn search(&self, query: &str, order: SearchOrder, named_only: bool) -> SearchResult;
}

/// A list's [`SessionFilter`]. Two are equal only when they are the same
/// search; `Debug` shows nothing of what it searches.
#[derive(Clone)]
pub struct SessionSearch(Arc<dyn SessionFilter>);

impl SessionSearch {
    pub fn new(filter: impl SessionFilter + 'static) -> Self {
        Self(Arc::new(filter))
    }
}

impl fmt::Debug for SessionSearch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionSearch(..)")
    }
}

impl PartialEq for SessionSearch {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for SessionSearch {}

/// One row of a list dialog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListItem {
    /// What an answer names. Typing also matches it, so a model's exact
    /// `provider/id` finds it whatever its label.
    pub value: String,
    /// What the row shows.
    pub label: String,
    /// Dim text after the label's column, such as a model's route or a
    /// session's age and id.
    pub detail: Option<String>,
    /// The run of rows this one belongs to, such as a model's provider. A
    /// heading with the run's count is drawn above each run.
    pub group: Option<String>,
    /// Short facts in aligned columns between the label and the detail,
    /// such as a context window; an empty fact leaves its column blank.
    pub facts: Vec<String>,
    /// What the row is, drawn under the list while it is selected.
    pub about: Option<String>,
}

/// A list to choose from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListDialog {
    pub title: String,
    pub items: Vec<ListItem>,
    /// The item that is in effect now, marked `Current` and selected first.
    pub current: Option<usize>,
    /// The item selected first when it is not the current one, as `/fork`
    /// starts at the latest message.
    pub selected: Option<usize>,
    /// Whether typing filters the rows.
    pub filterable: bool,
    /// Whether Ctrl+S chooses and sets the default.
    pub saves_default: bool,
    /// A dim line under the title, such as why a list is short.
    pub note: Option<String>,
    /// Searches the list as `/resume` does instead of filtering by words;
    /// see the module's documentation.
    pub search: Option<SessionSearch>,
}

/// One line of an information sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InfoLine {
    /// A section's heading, drawn bold.
    Heading(String),
    /// A dim label and its value, the labels of a section in one column.
    Field { label: String, value: String },
    /// A blank line between sections.
    Blank,
}

/// A read-only sheet, such as `/session`'s: it offers nothing to choose,
/// only scrolls, and any of Enter, Esc, or `q` closes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InfoDialog {
    pub title: String,
    pub lines: Vec<InfoLine>,
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
    Info(InfoDialog),
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
    /// Esc, or an information sheet closed.
    Cancel,
}

/// One line of a list's body as drawn: a run's heading, or the row at
/// `row` among the filtered rows, which is item `index`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListLine {
    Heading { group: usize, count: usize },
    Row { row: usize, index: usize },
}

/// A session search's state in an open list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchView {
    pub order: SearchOrder,
    pub named_only: bool,
    /// Why the query matches nothing, when it cannot match.
    pub error: Option<SearchError>,
    /// What the search found for the dialog's filter and these toggles.
    rows: Vec<usize>,
}

/// An open dialog and what the person has done in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dialog {
    pub request: DialogRequest,
    /// The list's filter. Changed only through [`Dialog::key`],
    /// [`Dialog::paste`], and [`Dialog::set_filter`], which keep a session
    /// search's rows and error in step with it.
    filter: String,
    /// The selected row among the filtered ones.
    pub selected: usize,
    /// The field's text.
    pub text: SecretText,
    /// Answered; waiting for the runtime's next dialog or close.
    pub answered: bool,
    /// Esc was sent; nothing more is.
    pub cancelled: bool,
    /// The first body line drawn: a list's first line, or an information
    /// sheet's. Rendering keeps the selection in view and records it here.
    pub top: usize,
    /// Body lines the last frame showed, for PgUp and PgDn.
    pub page: usize,
    /// Body lines in all as the last frame laid them out, which bound an
    /// information sheet's scroll.
    pub total: usize,
    /// A session search's order, filter, and last result.
    pub search: Option<SearchView>,
}

impl Dialog {
    /// Opens `request`, its current item selected and its field holding
    /// its initial text.
    pub fn open(request: DialogRequest) -> Self {
        let (selected, text) = match &request.kind {
            DialogKind::List(list) => (
                list.selected
                    .or(list.current)
                    .unwrap_or(0)
                    .min(list.items.len().saturating_sub(1)),
                String::new(),
            ),
            DialogKind::Field(field) => (0, field.initial.clone()),
            DialogKind::Info(_) => (0, String::new()),
        };
        let mut dialog = Self {
            request,
            filter: String::new(),
            selected,
            text: SecretText(text),
            answered: false,
            cancelled: false,
            top: 0,
            page: 1,
            total: 0,
            search: None,
        };
        if let DialogKind::List(list) = &dialog.request.kind
            && list.search.is_some()
        {
            let first = list.selected.or(list.current);
            dialog.search = Some(SearchView::default());
            dialog.refresh_search();
            dialog.selected = first
                .and_then(|item| dialog.rows().iter().position(|&row| row == item))
                .unwrap_or(0);
        }
        dialog
    }

    /// The list's filter.
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Replaces the list's filter, as typing would: the first row is
    /// selected and a session search runs again.
    pub fn set_filter(&mut self, filter: impl Into<String>) {
        self.filter = filter.into();
        self.selected = 0;
        self.refresh_search();
    }

    /// The session search's result for the current query and toggles, its
    /// rows kept in range and each item at most once.
    fn run_search(&self) -> Option<SearchResult> {
        let (DialogKind::List(list), Some(view)) = (&self.request.kind, &self.search) else {
            return None;
        };
        let search = list.search.as_ref()?;
        let mut found = search.0.search(&self.filter, view.order, view.named_only);
        let mut seen = vec![false; list.items.len()];
        found.rows.retain(|&row| {
            let fresh = seen.get(row).is_some_and(|seen| !seen);
            if fresh {
                seen[row] = true;
            }
            fresh
        });
        if found.error.is_some() {
            found.rows.clear();
        }
        Some(found)
    }

    /// Searches again after the query or a toggle changed, replacing the
    /// rows and the error together.
    fn refresh_search(&mut self) {
        let Some(found) = self.run_search() else {
            return;
        };
        if let Some(view) = self.search.as_mut() {
            view.rows = found.rows;
            view.error = found.error;
        }
    }

    /// Changes a session search's order or filter, keeping the selected
    /// item selected while it is still listed.
    fn toggle_search(&mut self, change: impl FnOnce(&mut SearchView)) {
        let chosen = self.rows().get(self.selected).copied();
        if let Some(view) = self.search.as_mut() {
            change(view);
        }
        self.refresh_search();
        self.selected = chosen
            .and_then(|item| self.rows().iter().position(|&row| row == item))
            .unwrap_or(0);
    }

    /// The list's rows that match the filter, as indexes into its items.
    /// Every word of the filter must appear, in any case, in the row's
    /// label, detail, group, description ([`ListItem::about`]), or value.
    /// A session search's rows are those its last run found, which every
    /// change of the filter or a toggle replaces; drawing never searches.
    pub fn rows(&self) -> Vec<usize> {
        let DialogKind::List(list) = &self.request.kind else {
            return Vec::new();
        };
        if let Some(view) = &self.search {
            return view.rows.clone();
        }
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
                    "{} {} {} {} {}",
                    item.label,
                    item.detail.as_deref().unwrap_or(""),
                    item.group.as_deref().unwrap_or(""),
                    item.about.as_deref().unwrap_or(""),
                    item.value,
                )
                .to_lowercase();
                words.iter().all(|word| haystack.contains(word.as_str()))
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// The list's body lines for the filtered rows: each run of rows that
    /// share a group under that group's heading, which counts them.
    pub fn lines(&self) -> Vec<ListLine> {
        list_lines(self.list_items(), &self.rows())
    }

    /// The body lines of the unfiltered list, which size the sheet, so
    /// typing a filter never changes its height.
    pub fn all_lines(&self) -> Vec<ListLine> {
        let items = self.list_items();
        let all: Vec<usize> = (0..items.len()).collect();
        list_lines(items, &all)
    }

    fn list_items(&self) -> &[ListItem] {
        match &self.request.kind {
            DialogKind::List(list) => &list.items,
            _ => &[],
        }
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
        let plain = input.mods == Mods::NONE;
        let ctrl = |c: char| input.key == Key::Char(c) && input.mods == Mods::CTRL;
        let page = self.page.max(1);
        match self.request.kind.clone() {
            DialogKind::List(list) => {
                if list.search.is_some() {
                    if ctrl('s') {
                        self.toggle_search(|view| {
                            view.order = match view.order {
                                SearchOrder::Recent => SearchOrder::Relevance,
                                SearchOrder::Relevance => SearchOrder::Recent,
                            }
                        });
                        return None;
                    }
                    if ctrl('n') {
                        self.toggle_search(|view| view.named_only = !view.named_only);
                        return None;
                    }
                }
                let rows = self.rows();
                let count = rows.len();
                if count > 0 {
                    let last = count - 1;
                    let moved = match input.key {
                        Key::Up if plain => Some((self.selected + last) % count),
                        Key::Down if plain => Some((self.selected + 1) % count),
                        Key::PageUp if plain => Some(self.selected.saturating_sub(page)),
                        Key::PageDown if plain => Some((self.selected + page).min(last)),
                        Key::Home if plain => Some(0),
                        Key::End if plain => Some(last),
                        _ if ctrl('p') => Some((self.selected + last) % count),
                        _ if ctrl('n') => Some((self.selected + 1) % count),
                        _ => None,
                    };
                    if let Some(moved) = moved {
                        self.selected = moved;
                        return None;
                    }
                }
                match input.key {
                    Key::Enter if fresh && plain => {
                        return self.choose(&rows, &list, false);
                    }
                    Key::Char('s') if fresh && input.mods == Mods::CTRL && list.saves_default => {
                        return self.choose(&rows, &list, true);
                    }
                    Key::Backspace if list.filterable && plain => {
                        let mut filter = self.filter.clone();
                        if filter.pop().is_some() {
                            self.set_filter(filter);
                        }
                    }
                    Key::Char('u') if list.filterable && input.mods == Mods::CTRL => {
                        if !self.filter.is_empty() {
                            self.set_filter(String::new());
                        }
                    }
                    _ => {
                        if let Some(c) = typed(input).filter(|_| list.filterable) {
                            let mut filter = self.filter.clone();
                            filter.push(c);
                            self.set_filter(filter);
                        }
                    }
                }
                None
            }
            DialogKind::Field(_) => {
                match input.key {
                    Key::Enter if fresh && plain => {
                        self.answered = true;
                        return Some(DialogAnswer::Entered(std::mem::take(&mut self.text)));
                    }
                    Key::Backspace if plain => {
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
            DialogKind::Info(info) => {
                let last = self.total.max(info.lines.len()).saturating_sub(page);
                match input.key {
                    Key::Up if plain => self.top = self.top.saturating_sub(1),
                    Key::Down if plain => self.top = (self.top + 1).min(last),
                    Key::PageUp if plain => self.top = self.top.saturating_sub(page),
                    Key::PageDown if plain => self.top = (self.top + page).min(last),
                    Key::Home if plain => self.top = 0,
                    Key::End if plain => self.top = last,
                    Key::Enter | Key::Char('q') if fresh && plain => {
                        self.answered = true;
                        self.cancelled = true;
                        return Some(DialogAnswer::Cancel);
                    }
                    _ => {}
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
                let filter: String = self.filter.chars().chain(clean).collect();
                self.set_filter(filter);
            }
            DialogKind::List(_) | DialogKind::Info(_) => {}
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

/// [`Dialog::lines`] over `rows`, indexes into `items`. A heading names
/// its group by the index of the run's first item.
fn list_lines(items: &[ListItem], rows: &[usize]) -> Vec<ListLine> {
    let mut lines = Vec::with_capacity(rows.len());
    let group = |row: usize| items.get(rows[row]).and_then(|item| item.group.as_deref());
    for (row, &index) in rows.iter().enumerate() {
        if let Some(name) = group(row)
            && (row == 0 || group(row - 1) != Some(name))
        {
            let count = (row..rows.len())
                .take_while(|&next| group(next) == Some(name))
                .count();
            lines.push(ListLine::Heading {
                group: index,
                count,
            });
        }
        lines.push(ListLine::Row { row, index });
    }
    lines
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
                        ..ListItem::default()
                    })
                    .collect(),
                current,
                selected: None,
                filterable,
                saves_default: true,
                note: None,
                search: None,
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

    // Pi's user-message selector starts at the latest message, which is
    // not marked current; an out-of-range start is clamped.
    #[test]
    fn a_first_selection_other_than_current_is_honoured_and_clamped() {
        let request = list(&["old", "latest"], None, false).request;
        let starting = |at: usize| {
            let mut request = request.clone();
            if let DialogKind::List(list) = &mut request.kind {
                list.selected = Some(at);
            }
            Dialog::open(request).selected
        };
        assert_eq!(starting(1), 1);
        assert_eq!(starting(9), 1);
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

    // TS picker.tsx: PgUp and PgDn move a page and stop at the ends, Home
    // and End jump, and Ctrl+P and Ctrl+N move one row and wrap.
    #[test]
    fn paging_keys_move_by_the_drawn_page() {
        let names: Vec<String> = (0..20).map(|n| format!("m{n}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut dialog = list(&names, Some(0), true);
        dialog.page = 5;
        dialog.key(key(Key::PageDown), true);
        assert_eq!(dialog.selected, 5);
        dialog.key(key(Key::End), true);
        assert_eq!(dialog.selected, 19);
        dialog.key(key(Key::PageDown), true);
        assert_eq!(dialog.selected, 19);
        dialog.key(
            KeyInput {
                key: Key::Char('n'),
                mods: Mods::CTRL,
            },
            true,
        );
        assert_eq!(dialog.selected, 0);
        dialog.key(
            KeyInput {
                key: Key::Char('p'),
                mods: Mods::CTRL,
            },
            true,
        );
        assert_eq!(dialog.selected, 19);
        dialog.key(key(Key::Home), true);
        assert_eq!(dialog.selected, 0);
        dialog.key(key(Key::PageUp), true);
        assert_eq!(dialog.selected, 0);
    }

    // TS choices.ts `filterChoices`: every word, anywhere in a row,
    // including its description and its value, so a named session is
    // found by its first message and a model by its exact route.
    #[test]
    fn a_filter_matches_descriptions_and_values() {
        let mut dialog = Dialog::open(DialogRequest {
            id: 1,
            kind: DialogKind::List(ListDialog {
                title: "Resume".into(),
                items: vec![
                    ListItem {
                        value: "01a13e27-aaaa".into(),
                        label: "Release notes".into(),
                        about: Some("draft the changelog".into()),
                        ..ListItem::default()
                    },
                    ListItem {
                        value: "01b00000-bbbb".into(),
                        label: "Other".into(),
                        ..ListItem::default()
                    },
                ],
                current: None,
                selected: None,
                filterable: true,
                saves_default: false,
                note: None,
                search: None,
            }),
        });
        dialog.paste("CHANGELOG draft");
        assert_eq!(dialog.rows(), [0]);
        dialog.set_filter("01b0");
        assert_eq!(dialog.rows(), [1]);
    }

    // TS sheet.tsx: runs of rows that share a group sit under one heading
    // that counts them, also after a filter.
    #[test]
    fn groups_head_their_runs_with_counts() {
        let mut dialog = list(&["a", "b", "c"], None, true);
        if let DialogKind::List(list) = &mut dialog.request.kind {
            list.items[0].group = Some("one".into());
            list.items[1].group = Some("one".into());
            list.items[2].group = Some("two".into());
        }
        assert_eq!(
            dialog.lines(),
            [
                ListLine::Heading { group: 0, count: 2 },
                ListLine::Row { row: 0, index: 0 },
                ListLine::Row { row: 1, index: 1 },
                ListLine::Heading { group: 2, count: 1 },
                ListLine::Row { row: 2, index: 2 },
            ]
        );
        dialog.set_filter("b");
        assert_eq!(
            dialog.lines(),
            [
                ListLine::Heading { group: 1, count: 1 },
                ListLine::Row { row: 0, index: 1 },
            ]
        );
    }

    // Bake: an information sheet offers nothing to choose; it scrolls,
    // and Enter, q, or Esc closes it once.
    #[test]
    fn an_info_sheet_only_scrolls_and_closes() {
        let mut dialog = Dialog::open(DialogRequest {
            id: 3,
            kind: DialogKind::Info(InfoDialog {
                title: "Session info".into(),
                lines: (0..10)
                    .map(|n| InfoLine::Field {
                        label: "Row".into(),
                        value: n.to_string(),
                    })
                    .collect(),
            }),
        });
        dialog.page = 4;
        assert_eq!(dialog.key(key(Key::Down), true), None);
        assert_eq!(dialog.top, 1);
        dialog.key(key(Key::End), true);
        assert_eq!(dialog.top, 6);
        assert_eq!(dialog.key(key(Key::Char('x')), true), None);
        assert_eq!(dialog.key(key(Key::Enter), false), None);
        assert_eq!(
            dialog.key(key(Key::Char('q')), true),
            Some(DialogAnswer::Cancel)
        );
        assert_eq!(dialog.key(key(Key::Esc), true), None);
    }

    /// A pure search over fixed texts, standing in for the runtime's index:
    /// words must appear in a text; `re:` alone, `re:(`, and a `dup` query
    /// answer as the runtime's would or worse, so the dialog's guards show.
    struct Texts {
        texts: Vec<(&'static str, bool)>,
        runs: std::sync::atomic::AtomicUsize,
    }

    impl SessionFilter for Texts {
        fn search(&self, query: &str, order: SearchOrder, named_only: bool) -> SearchResult {
            self.runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let query = query.trim();
            if query == "re:" {
                return SearchResult {
                    rows: vec![0],
                    error: Some(SearchError::EmptyRegex),
                };
            }
            if query == "re:(" {
                return SearchResult {
                    rows: Vec::new(),
                    error: Some(SearchError::InvalidRegex("unclosed group".into())),
                };
            }
            if query == "dup" {
                return SearchResult {
                    rows: vec![2, 99, 2, 0],
                    error: None,
                };
            }
            let mut rows: Vec<usize> = (0..self.texts.len())
                .filter(|&i| !named_only || self.texts[i].1)
                .filter(|&i| self.texts[i].0.contains(query))
                .collect();
            if order == SearchOrder::Relevance {
                rows.reverse();
            }
            SearchResult { rows, error: None }
        }
    }

    fn sessions(current: Option<usize>) -> (Dialog, Arc<Texts>) {
        let texts = Arc::new(Texts {
            texts: vec![
                ("alpha one", true),
                ("beta two", false),
                ("alpha three", false),
            ],
            runs: std::sync::atomic::AtomicUsize::new(0),
        });
        struct Shared(Arc<Texts>);
        impl SessionFilter for Shared {
            fn search(&self, query: &str, order: SearchOrder, named: bool) -> SearchResult {
                self.0.search(query, order, named)
            }
        }
        let dialog = Dialog::open(DialogRequest {
            id: 9,
            kind: DialogKind::List(ListDialog {
                title: "Resume".into(),
                items: ["s0", "s1", "s2"]
                    .iter()
                    .map(|id| ListItem {
                        value: (*id).into(),
                        label: (*id).into(),
                        ..ListItem::default()
                    })
                    .collect(),
                current,
                selected: None,
                filterable: true,
                saves_default: false,
                note: None,
                search: Some(SessionSearch::new(Shared(Arc::clone(&texts)))),
            }),
        });
        (dialog, texts)
    }

    fn ctrl(c: char) -> KeyInput {
        KeyInput {
            key: Key::Char(c),
            mods: Mods::CTRL,
        }
    }

    fn typing(dialog: &mut Dialog, text: &str) {
        for c in text.chars() {
            dialog.key(key(Key::Char(c)), true);
        }
    }

    // Pi session-selector.ts: Ctrl+S switches the order and Ctrl+N the
    // named filter; Bake keeps the selected session selected while it is
    // listed, and the current one stays marked wherever the selection is.
    #[test]
    fn session_search_toggles_keep_the_selection() {
        let (mut dialog, _) = sessions(Some(2));
        assert_eq!(dialog.rows(), [0, 1, 2]);
        assert_eq!(dialog.selected, 2);
        dialog.key(ctrl('s'), true);
        let view = dialog.search.clone().expect("search");
        assert_eq!(view.order, SearchOrder::Relevance);
        assert_eq!(dialog.rows(), [2, 1, 0]);
        assert_eq!(dialog.selected, 0, "s2 is still selected, now first");
        // Ctrl+N filters here; it does not move the selection.
        dialog.key(key(Key::Down), true);
        assert_eq!(dialog.rows()[dialog.selected], 1);
        dialog.key(ctrl('n'), true);
        assert!(dialog.search.as_ref().expect("search").named_only);
        assert_eq!(dialog.rows(), [0]);
        assert_eq!(dialog.selected, 0, "s1 is not named; the first row is");
        dialog.key(ctrl('n'), true);
        dialog.key(ctrl('s'), true);
        assert_eq!(dialog.rows(), [0, 1, 2]);
        assert_eq!(dialog.selected, 0, "s0 stays selected");
        let DialogKind::List(list) = &dialog.request.kind else {
            unreachable!()
        };
        assert_eq!(list.current, Some(2));
    }

    // Bake: an invalid or empty pattern lists nothing and says why; Enter
    // then answers nothing, and fixing the query brings the rows back.
    #[test]
    fn an_invalid_query_lists_nothing_and_enter_chooses_nothing() {
        let (mut dialog, _) = sessions(Some(0));
        dialog.key(key(Key::Down), true);
        typing(&mut dialog, "re:(");
        assert!(dialog.rows().is_empty());
        assert_eq!(
            dialog.search.as_ref().and_then(|view| view.error.clone()),
            Some(SearchError::InvalidRegex("unclosed group".into()))
        );
        assert_eq!(
            SearchError::InvalidRegex("unclosed group".into()).message(),
            "Invalid regex: unclosed group"
        );
        assert_eq!(dialog.key(key(Key::Enter), true), None);
        assert!(!dialog.answered);
        // An error's rows are dropped even when the search returned some.
        dialog.key(key(Key::Backspace), true);
        assert!(dialog.rows().is_empty());
        assert_eq!(
            dialog.search.as_ref().and_then(|view| view.error.clone()),
            Some(SearchError::EmptyRegex)
        );
        assert_eq!(dialog.key(key(Key::Enter), true), None);
        dialog.key(ctrl('u'), true);
        assert_eq!(dialog.rows(), [0, 1, 2]);
        assert_eq!(
            dialog.search.as_ref().and_then(|view| view.error.clone()),
            None
        );
        typing(&mut dialog, "alpha");
        assert_eq!(dialog.rows(), [0, 2]);
        dialog.key(key(Key::Down), true);
        assert_eq!(
            dialog.key(key(Key::Enter), true),
            Some(DialogAnswer::Chose {
                value: "s2".into(),
                save_default: false,
            })
        );
    }

    // Bake: the search runs once per change of the query or a toggle, never
    // to draw or to move; rows out of range or repeated are dropped.
    #[test]
    fn the_search_runs_per_change_and_its_rows_are_checked() {
        let (mut dialog, texts) = sessions(None);
        let runs = || texts.runs.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(runs(), 1);
        for _ in 0..5 {
            let _ = dialog.rows();
            let _ = dialog.lines();
            dialog.key(key(Key::Down), true);
        }
        assert_eq!(runs(), 1);
        typing(&mut dialog, "dup");
        assert_eq!(runs(), 4);
        assert_eq!(dialog.rows(), [2, 0]);
        dialog.paste(" x");
        assert_eq!(runs(), 5);
        dialog.key(ctrl('s'), true);
        assert_eq!(runs(), 6);
        let shown = format!("{dialog:?}");
        assert!(shown.contains("SessionSearch(..)"), "{shown}");
        assert!(!shown.contains("alpha"), "{shown}");
    }

    // Bake: lists without a session search keep Ctrl+N as down and Ctrl+S
    // as nothing unless they save a default.
    #[test]
    fn other_lists_keep_their_keys() {
        let mut dialog = list(&["a", "b"], Some(0), true);
        if let DialogKind::List(list) = &mut dialog.request.kind {
            list.saves_default = false;
        }
        dialog.key(ctrl('n'), true);
        assert_eq!(dialog.selected, 1);
        assert_eq!(dialog.key(ctrl('s'), true), None);
        assert!(dialog.search.is_none());
    }
}
