//! Pure composer draft: grapheme-safe editing, bounded undo, and wrapping.
//!
//! Wrapping, tab expansion, and caret placement share one cell measure, which
//! delegates to the measure Ratatui's buffer uses, so drawn text and the
//! cursor agree.

use std::borrow::Cow;
use std::collections::VecDeque;

use ratatui_core::buffer::CellWidth;
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

/// Largest draft kept in memory; longer input is cut at a grapheme boundary.
pub const MAX_DRAFT_BYTES: usize = 256 * 1024;
/// Undo steps retained; the oldest step is dropped first.
pub const UNDO_DEPTH: usize = 64;
/// Tab stops fall every `TAB_WIDTH` cells from the start of a row.
pub const TAB_WIDTH: usize = 4;
/// Killed texts the yank ring keeps; the oldest is dropped first.
pub const RING_LIMIT: usize = 30;

/// The last edit, which decides what the next one joins: typing is one undo
/// step, consecutive deletions are one, consecutive kills are one ring
/// entry, and only a yank can be replaced by an older kill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditKind {
    Type,
    Delete,
    Block,
    Kill,
    Yank,
}

/// ASCII punctuation a word step stops at inside a word, such as the dots
/// and slashes of a path: the TypeScript `PUNCTUATION` class.
const PUNCTUATION: &str = "(){}[]<>.,;:'\"!?+-=*/\\|&%^$#@~`";

/// Pasted text longer than this many bytes collapses into a placeholder; the
/// TypeScript `PASTE_COLLAPSE_CHARS`, which counts UTF-16 units.
pub const PASTE_COLLAPSE_BYTES: usize = 800;
/// Pasted text with at least this many lines collapses into a placeholder.
pub const PASTE_COLLAPSE_LINES: usize = 3;

/// What a registered placeholder stands for: pasted text, which a submission
/// expands back, or a staged image, by its attachment number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Atom {
    Text(String),
    Image(u32),
}

/// Whether a paste is long enough to collapse into a placeholder.
pub fn collapses(text: &str) -> bool {
    text.len() > PASTE_COLLAPSE_BYTES || text.split('\n').count() >= PASTE_COLLAPSE_LINES
}

/// The placeholder a collapsed paste shows in the draft: `[Pasted text #1
/// +42 lines]`, or `[Pasted text #1 900 chars]` for one long line. The
/// format is fixed English, as the oracle's is.
pub fn pasted_text_token(id: u32, text: &str) -> String {
    let breaks = text.matches('\n').count();
    if breaks > 0 {
        format!("[Pasted text #{id} +{breaks} lines]")
    } else {
        format!("[Pasted text #{id} {} chars]", text.chars().count())
    }
}

/// The placeholder a staged image shows in the draft: `[Image #2]`.
pub fn image_token(id: u32) -> String {
    format!("[Image #{id}]")
}

/// Draft text with a caret that always sits on a grapheme boundary.
#[derive(Clone, Debug, Default)]
pub struct Draft {
    text: String,
    caret: usize,
    undo: VecDeque<(String, usize)>,
    last_edit: Option<EditKind>,
    /// Killed text, newest last.
    ring: VecDeque<String>,
    /// Where the last yank put the ring's newest entry.
    yanked: Option<(usize, usize)>,
    /// The cell column a run of Up and Down presses keeps to; any other
    /// change ends the run.
    goal: Option<usize>,
    /// A browse through input history, from Up on the first row until it
    /// returns to the draft it started from.
    visit: Option<Visit>,
    /// Placeholders this draft inserted. One stays registered after it is
    /// erased, so undo and yank can bring it back as itself.
    atoms: Vec<(String, Atom)>,
    /// Placeholders numbered so far; the next takes the next number.
    pastes: u32,
    /// Image placeholders whose image was unstaged when they left the draft;
    /// undo strips them from what it restores, so none comes back without
    /// its image.
    unstaged: Vec<String>,
}

/// An open history browse: the draft it started from, each entry shown so
/// far as the user left it, and the one shown now.
#[derive(Clone, Debug)]
struct Visit {
    scratch: (String, usize),
    entries: Vec<(String, usize)>,
    index: Option<usize>,
}

impl Draft {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Caret as a byte offset into [`Draft::text`].
    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Inserts typed text; consecutive typing undoes as one step.
    /// Returns `false` when the draft limit cut the input.
    pub fn type_text(&mut self, input: &str) -> bool {
        self.insert(input, EditKind::Type)
    }

    /// Inserts pasted text as one undo step. Returns `false` when cut.
    pub fn paste(&mut self, input: &str) -> bool {
        self.insert(input, EditKind::Block)
    }

    pub fn newline(&mut self) -> bool {
        self.insert("\n", EditKind::Block)
    }

    // Removing a grapheme can join its neighbors, such as a letter and a
    // combining mark that a newline separated, or two regional indicators. The
    // caret then leaves the joined grapheme on the side the deletion moves
    // toward, so repeating the key deletes the joined grapheme next.

    pub fn backspace(&mut self) {
        if let Some((start, end)) = self.atom_at(self.caret, true) {
            return self.erase(start, end);
        }
        let start = self.prev_boundary(self.caret);
        if start < self.caret {
            self.checkpoint(EditKind::Delete);
            self.text.replace_range(start..self.caret, "");
            self.caret = if self.is_boundary(start) {
                start
            } else {
                self.next_boundary(start)
            };
        }
    }

    pub fn delete(&mut self) {
        if let Some((start, end)) = self.atom_at(self.caret, false) {
            return self.erase(start, end);
        }
        let end = self.next_boundary(self.caret);
        if end > self.caret {
            self.checkpoint(EditKind::Delete);
            self.text.replace_range(self.caret..end, "");
            if !self.is_boundary(self.caret) {
                self.caret = self.prev_boundary(self.caret);
            }
        }
    }

    pub fn left(&mut self) {
        match self.atom_at(self.caret, true) {
            Some((start, _)) => self.move_to(start),
            None => self.move_to(self.prev_boundary(self.caret)),
        }
    }

    pub fn right(&mut self) {
        match self.atom_at(self.caret, false) {
            Some((_, end)) => self.move_to(end),
            None => self.move_to(self.next_boundary(self.caret)),
        }
    }

    /// Inserts a terminal paste: as text, or, when it [`collapses`], as one
    /// placeholder that stands for it. Returns `false` when the draft limit
    /// cut the pasted text.
    pub fn paste_block(&mut self, input: &str) -> bool {
        let clean = sanitize(input);
        if !collapses(&clean) {
            return self.paste(&clean);
        }
        let complete = clean.len() <= MAX_DRAFT_BYTES;
        let kept = cut_at_boundary(&clean, MAX_DRAFT_BYTES).to_owned();
        self.pastes += 1;
        let token = pasted_text_token(self.pastes, &kept);
        self.atoms.push((token.clone(), Atom::Text(kept)));
        self.paste(&token) && complete
    }

    /// Inserts the placeholder of staged image `id` at the caret, as one
    /// undo step, and returns it.
    pub fn attach(&mut self, id: u32) -> String {
        self.pastes += 1;
        let token = image_token(self.pastes);
        self.atoms.push((token.clone(), Atom::Image(id)));
        self.paste(&token);
        token
    }

    /// Unstages every image whose placeholder has left the draft, and
    /// returns their ids. Nothing is unstaged while a history browse shows
    /// another entry, since returning restores the draft and its
    /// placeholders.
    pub fn sweep(&mut self) -> Vec<u32> {
        if self.visit.is_some() {
            return Vec::new();
        }
        let mut gone = Vec::new();
        let text = &self.text;
        self.atoms.retain(|(token, atom)| match atom {
            Atom::Image(id) if !text.contains(token.as_str()) => {
                gone.push((token.clone(), *id));
                false
            }
            _ => true,
        });
        gone.into_iter()
            .map(|(token, id)| {
                self.unstaged.push(token);
                id
            })
            .collect()
    }

    /// The text a submission sends: every pasted-text placeholder replaced by
    /// the text it stands for. Image placeholders stay, beside their
    /// attachments.
    pub fn expanded(&self) -> String {
        let mut text = self.text.clone();
        for (token, atom) in &self.atoms {
            if let Atom::Text(pasted) = atom {
                text = text.replace(token.as_str(), pasted);
            }
        }
        text
    }

    /// The placeholder the caret would cross going back (`backward`) or
    /// forward from byte `at`: one that ends at or contains it going back,
    /// and one that starts at or contains it going forward.
    fn atom_at(&self, at: usize, backward: bool) -> Option<(usize, usize)> {
        atom_at(&self.text, at, &self.atoms, backward)
    }

    /// `at`, moved to the start of any placeholder it falls strictly inside.
    fn outside_atoms(&self, at: usize) -> usize {
        match self.atom_at(at, false) {
            Some((start, _)) if start < at => start,
            _ => at,
        }
    }

    /// Removes `start..end` as a deletion step.
    fn erase(&mut self, start: usize, end: usize) {
        self.checkpoint(EditKind::Delete);
        self.text.replace_range(start..end, "");
        self.caret = start;
    }

    /// Moves to the start of the caret's logical line.
    pub fn home(&mut self) {
        let start = self.text[..self.caret].rfind('\n').map_or(0, |i| i + 1);
        self.move_to(start);
    }

    /// Moves to the end of the caret's logical line.
    pub fn end(&mut self) {
        let rest = &self.text[self.caret..];
        let end = rest.find('\n').map_or(self.text.len(), |i| self.caret + i);
        self.move_to(end);
    }

    /// Moves to the start of the drawn row at `width`, or from there to the
    /// start of the logical line. A width of zero, before the first frame,
    /// reaches the logical line's start at once.
    pub fn row_home(&mut self, width: usize) {
        match self.row_edge(width, false) {
            Some(at) => self.move_to(at),
            None => self.home(),
        }
    }

    /// Moves to the end of the drawn row at `width`, or from there to the end
    /// of the logical line.
    pub fn row_end(&mut self, width: usize) {
        match self.row_edge(width, true) {
            Some(at) => self.move_to(at),
            None => self.end(),
        }
    }

    /// The caret's place at an edge of the row it is drawn on, when that is
    /// not where it already is. A wrapped row's last place is before its last
    /// grapheme, because a caret after it is drawn on the next row.
    fn row_edge(&self, width: usize, end: bool) -> Option<usize> {
        if width == 0 {
            return None;
        }
        let drawn = layout(&self.text, self.caret, width);
        let index = drawn.caret.0;
        let row = drawn.rows[index];
        let target = if !end {
            row.start
        } else if layout(&self.text, row.end, width).caret.0 == index {
            row.end
        } else {
            self.prev_boundary(row.end).max(row.start)
        };
        let target = self.outside_atoms(target);
        (target != self.caret).then_some(target)
    }

    /// Up or Down: moves the caret to the drawn row above or below at
    /// `width`, to the place nearest the column a run of presses keeps.
    /// Returns `false` on the first row going up or the last going down,
    /// where history takes over, and before the first frame.
    pub fn vertical(&mut self, width: usize, up: bool) -> bool {
        if width == 0 {
            return false;
        }
        let rows = row_stops(&self.text, width);
        let Some(from) = rows
            .iter()
            .position(|row| row.iter().any(|&(at, _)| at == self.caret))
        else {
            return false;
        };
        let Some(to) = (if up {
            from.checked_sub(1)
        } else {
            Some(from + 1)
        })
        .filter(|&to| to < rows.len()) else {
            return false;
        };
        let column = self.goal.unwrap_or_else(|| {
            rows[from]
                .iter()
                .find(|&&(at, _)| at == self.caret)
                .map_or(0, |&(_, column)| column)
        });
        // The last place at or before the column, else the row's first.
        let landing = rows[to]
            .iter()
            .take_while(|&&(_, at)| at <= column)
            .last()
            .unwrap_or(&rows[to][0]);
        self.caret = self.outside_atoms(landing.0);
        self.last_edit = None;
        self.goal = Some(column);
        true
    }

    /// Puts the caret where a click landed: on drawn row `row` at `width`, the
    /// place at or before cell `column` of its text, the oracle's `offsetAt`.
    /// A column past the text reaches the row's last place. Returns `false`
    /// past the last row.
    pub fn place(&mut self, row: usize, column: usize, width: usize) -> bool {
        let rows = row_stops(&self.text, width.max(1));
        let Some(stops) = rows.get(row) else {
            return false;
        };
        let landing = stops
            .iter()
            .take_while(|&&(_, at)| at <= column)
            .last()
            .unwrap_or(&stops[0]);
        self.move_to(self.outside_atoms(landing.0));
        true
    }

    /// Steps through `history`, newest first: older for Up, newer for Down.
    /// The first step keeps the draft as one undo step and as the place a
    /// browse returns to; stepping newer than the newest entry restores it,
    /// caret and all. An entry left with edits shows them again within the
    /// same browse. An entry of several rows at `width` opens with the caret
    /// at its start going older and its end going newer, so the next press
    /// keeps walking; a one-row entry keeps the caret where it was. Returns
    /// `false` when there was nothing further to show.
    pub fn recall(&mut self, history: &[String], older: bool, width: usize) -> bool {
        if self.visit.is_none() {
            if !older {
                return false;
            }
            self.checkpoint(EditKind::Block);
            self.visit = Some(Visit {
                scratch: (self.text.clone(), self.caret),
                entries: Vec::new(),
                index: None,
            });
        }
        let current = (self.text.clone(), self.caret);
        let Some(visit) = self.visit.as_mut() else {
            return false;
        };
        if let Some(index) = visit.index {
            visit.entries[index] = current;
        }
        let next = match (visit.index, older) {
            (None, true) => 0,
            (Some(index), true) => index + 1,
            (Some(index), false) if index > 0 => index - 1,
            _ => {
                let (text, caret) = visit.scratch.clone();
                self.visit = None;
                self.show(text, caret);
                return true;
            }
        };
        if next == visit.entries.len() {
            let Some(entry) = history.get(next) else {
                if visit.entries.is_empty() {
                    self.visit = None;
                }
                return false;
            };
            let entry = sanitize(entry);
            let end = entry.len();
            visit.entries.push((entry, end));
        }
        visit.index = Some(next);
        let (text, caret) = visit.entries[next].clone();
        let rows = if width == 0 {
            1
        } else {
            layout(&text, 0, width).rows.len()
        };
        let caret = match rows {
            0 | 1 => caret,
            _ if older => 0,
            _ => text.len(),
        };
        self.show(text, caret);
        true
    }

    /// Replaces the text and caret without an undo step, as recall does.
    fn show(&mut self, text: String, caret: usize) {
        self.text = text;
        self.caret = caret.min(self.text.len());
        self.last_edit = None;
        self.goal = None;
    }

    /// Moves one word toward the start: see [`word_stop`].
    pub fn word_left(&mut self) {
        self.move_to(word_stop(&self.text, self.caret, false, &self.atoms));
    }

    /// Moves one word toward the end: see [`word_stop`].
    pub fn word_right(&mut self) {
        self.move_to(word_stop(&self.text, self.caret, true, &self.atoms));
    }

    /// Ctrl+W and Alt+Backspace: kills back to the previous word stop.
    pub fn kill_word_left(&mut self) {
        let stop = word_stop(&self.text, self.caret, false, &self.atoms);
        self.kill(stop, self.caret, true);
    }

    /// Alt+D, Alt+Delete, and Ctrl+Delete: kills on to the next word stop.
    pub fn kill_word_right(&mut self) {
        let stop = word_stop(&self.text, self.caret, true, &self.atoms);
        self.kill(self.caret, stop, false);
    }

    /// Ctrl+U: kills to the logical line's start, or, at its start, the line
    /// break before it.
    pub fn kill_line_left(&mut self) {
        let start = self.text[..self.caret].rfind('\n').map_or(0, |i| i + 1);
        let from = if start == self.caret {
            self.caret.saturating_sub(1)
        } else {
            start
        };
        self.kill(from, self.caret, true);
    }

    /// Ctrl+K: kills to the logical line's end, or, at its end, the line
    /// break after it.
    pub fn kill_line_right(&mut self) {
        let end = self.text[self.caret..]
            .find('\n')
            .map_or(self.text.len(), |i| self.caret + i);
        let to = if end == self.caret {
            (self.caret + 1).min(self.text.len())
        } else {
            end
        };
        self.kill(self.caret, to, false);
    }

    /// Cuts `from..to` into the yank ring as its own undo step. Consecutive
    /// kills join one entry, in the order the text stood.
    fn kill(&mut self, from: usize, to: usize, backward: bool) {
        // A kill takes any placeholder it touches whole.
        let from = match self.atom_at(from, false) {
            Some((start, _)) if start < from => start,
            _ => from,
        };
        let to = match self.atom_at(to, true) {
            Some((_, end)) if end > to => end,
            _ => to,
        };
        if from >= to {
            return;
        }
        let joining = self.last_edit == Some(EditKind::Kill);
        self.checkpoint(EditKind::Kill);
        let mut removed: String = self.text.drain(from..to).collect();
        // An image's placeholder goes with its image; a yank brings back
        // only text.
        for (token, atom) in &self.atoms {
            if matches!(atom, Atom::Image(_)) {
                removed = removed.replace(token.as_str(), "");
            }
        }
        let joined = if joining { self.ring.pop_back() } else { None };
        let entry = match joined {
            Some(joined) if backward => removed + &joined,
            Some(joined) => joined + &removed,
            None => removed,
        };
        self.ring.push_back(entry);
        if self.ring.len() > RING_LIMIT {
            self.ring.pop_front();
        }
        self.caret = from;
    }

    /// Ctrl+Y: inserts the newest kill at the caret.
    pub fn yank(&mut self) -> bool {
        let Some(entry) = self.ring.back().filter(|e| !e.is_empty()).cloned() else {
            return true;
        };
        let start = self.caret;
        let complete = self.insert(&entry, EditKind::Yank);
        self.yanked = Some((start, self.caret));
        complete
    }

    /// Alt+Y right after a yank: replaces what it put in with the next older
    /// kill, cycling through the ring.
    pub fn yank_pop(&mut self) {
        let Some((start, end)) = self.yanked else {
            return;
        };
        if self.last_edit != Some(EditKind::Yank) || self.ring.len() < 2 {
            return;
        }
        let mut ring = self.ring.clone();
        if let Some(newest) = ring.pop_back() {
            ring.push_front(newest);
        }
        let Some(older) = ring.back().cloned() else {
            return;
        };
        if self.text.len() - (end - start) + older.len() > MAX_DRAFT_BYTES {
            return;
        }
        self.ring = ring;
        self.checkpoint(EditKind::Yank);
        self.text.replace_range(start..end, &older);
        self.caret = start + older.len();
        self.yanked = Some((start, self.caret));
    }

    /// Restores the text and caret before the latest step. Returns `false` when
    /// there is nothing to undo.
    pub fn undo(&mut self) -> bool {
        self.last_edit = None;
        self.goal = None;
        // Undo returns from a whole browse to the draft it started from.
        self.visit = None;
        match self.undo.pop_back() {
            Some((mut text, mut caret)) => {
                for token in &self.unstaged {
                    while let Some(at) = text.rfind(token.as_str()) {
                        text.replace_range(at..at + token.len(), "");
                        if caret > at {
                            caret = caret.saturating_sub(token.len()).max(at);
                        }
                    }
                }
                self.text = text;
                self.caret = caret;
                true
            }
            None => false,
        }
    }

    fn insert(&mut self, input: &str, kind: EditKind) -> bool {
        let clean = sanitize(input);
        let room = MAX_DRAFT_BYTES.saturating_sub(self.text.len());
        let mut take = 0;
        for grapheme in clean.graphemes(true) {
            if take + grapheme.len() > room {
                break;
            }
            take += grapheme.len();
        }
        if take > 0 {
            self.checkpoint(kind);
            self.text.insert_str(self.caret, &clean[..take]);
            // Inserted text can join the grapheme after it, such as a base
            // letter before a combining mark; keep the caret outside it.
            let after = self.caret + take;
            self.caret = if self.is_boundary(after) {
                after
            } else {
                self.next_boundary(after)
            };
        }
        take == clean.len()
    }

    fn checkpoint(&mut self, kind: EditKind) {
        // A kill or a yank is always a step of its own.
        self.goal = None;
        let own = matches!(kind, EditKind::Block | EditKind::Kill | EditKind::Yank);
        if own || self.last_edit != Some(kind) {
            if self.undo.len() == UNDO_DEPTH {
                self.undo.pop_front();
            }
            self.undo.push_back((self.text.clone(), self.caret));
        }
        self.last_edit = Some(kind);
    }

    fn move_to(&mut self, caret: usize) {
        self.caret = caret;
        self.last_edit = None;
        self.goal = None;
    }

    fn is_boundary(&self, at: usize) -> bool {
        GraphemeCursor::new(at, self.text.len(), true)
            .is_boundary(&self.text, 0)
            .unwrap_or(true)
    }

    fn prev_boundary(&self, at: usize) -> usize {
        GraphemeCursor::new(at, self.text.len(), true)
            .prev_boundary(&self.text, 0)
            .ok()
            .flatten()
            .unwrap_or(0)
    }

    fn next_boundary(&self, at: usize) -> usize {
        GraphemeCursor::new(at, self.text.len(), true)
            .next_boundary(&self.text, 0)
            .ok()
            .flatten()
            .unwrap_or(self.text.len())
    }
}

/// Where a word step from byte `caret` lands, toward the end when `forward`:
/// a port of the TypeScript `wordStop`.
///
/// A step skips whitespace, then one word or one run of punctuation. Inside
/// a word, ASCII punctuation such as the dots of a path is a stop of its
/// own. The step stays inside its logical line: at a line's edge it crosses
/// the line break alone. Words are Unicode word segments without a
/// dictionary, so CJK text steps one ideograph at a time where the oracle's
/// ICU segmenter steps by dictionary word.
pub fn word_stop(text: &str, caret: usize, forward: bool, atoms: &[(String, Atom)]) -> usize {
    let line_start = text[..caret].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[caret..].find('\n').map_or(text.len(), |i| caret + i);
    let wordlike = |segment: &str| segment.chars().any(char::is_alphanumeric);
    let blank = |segment: &str| segment.chars().any(char::is_whitespace);
    if !forward {
        if caret == line_start {
            return caret.saturating_sub(1);
        }
        let at = text[line_start..caret].trim_end().len() + line_start;
        if at == line_start {
            return at;
        }
        // A placeholder is crossed whole.
        if let Some((start, _)) = atom_at(text, at, atoms, true) {
            return start;
        }
        let segments: Vec<(usize, &str)> =
            text[line_start..at].split_word_bound_indices().collect();
        let Some(&(index, last)) = segments.last() else {
            return line_start;
        };
        if wordlike(last) {
            // After the last punctuation that leaves part of the word to cross.
            let inner = last
                .char_indices()
                .filter(|&(_, c)| PUNCTUATION.contains(c))
                .map(|(i, c)| i + c.len_utf8())
                .rfind(|&end| end < last.len())
                .unwrap_or(0);
            return line_start + index + inner;
        }
        let kept = segments
            .iter()
            .rposition(|&(_, segment)| wordlike(segment) || blank(segment));
        return line_start + kept.map_or(0, |i| segments[i].0 + segments[i].1.len());
    }
    if caret == line_end {
        return (caret + 1).min(text.len());
    }
    let at = line_end - text[caret..line_end].trim_start().len();
    if at == line_end {
        return at;
    }
    if let Some((_, end)) = atom_at(text, at, atoms, false) {
        return end;
    }
    let mut offset = at;
    for (_, segment) in text[at..line_end].split_word_bound_indices() {
        if offset == at && wordlike(segment) {
            let stop = segment
                .char_indices()
                .find(|&(_, c)| PUNCTUATION.contains(c))
                .map_or(
                    segment.len(),
                    |(i, _)| if i == 0 { segment.len() } else { i },
                );
            return at + stop;
        }
        if wordlike(segment) || blank(segment) {
            break;
        }
        offset += segment.len();
    }
    offset
}

/// The range of a placeholder in `text` that the caret at `at` would cross
/// going back or forward: the TypeScript `atomAt`.
fn atom_at(
    text: &str,
    at: usize,
    atoms: &[(String, Atom)],
    backward: bool,
) -> Option<(usize, usize)> {
    atoms.iter().find_map(|(token, _)| {
        text.match_indices(token.as_str())
            .map(|(start, _)| (start, start + token.len()))
            .find(|&(start, end)| {
                if backward {
                    start < at && at <= end
                } else {
                    start <= at && at < end
                }
            })
    })
}

/// The longest prefix of `text` within `limit` bytes that ends on a grapheme
/// boundary.
fn cut_at_boundary(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut end = 0;
    for (at, grapheme) in text.grapheme_indices(true) {
        if at + grapheme.len() > limit {
            break;
        }
        end = at + grapheme.len();
    }
    &text[..end]
}

/// Normalizes CRLF and CR to LF and drops control characters other than LF and
/// tab, so no input can become a terminal escape sequence.
pub fn sanitize(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Terminal cells one grapheme other than a tab occupies.
pub fn cell_width(grapheme: &str) -> usize {
    usize::from(grapheme.cell_width())
}

/// Spaces and tabs separate words and may hang past a row's end. Other
/// whitespace, such as a no-break space, belongs to its word.
fn is_blank(grapheme: &str) -> bool {
    grapheme == " " || grapheme == "\t"
}

/// Cells a grapheme advances a row's column from `col`, where the row's text
/// ends at `limit`. A tab reaches the next stop. A blank at or past `limit`
/// hangs: it stays on the row and occupies nothing.
fn advance(grapheme: &str, col: usize, limit: usize) -> usize {
    match grapheme {
        " " => usize::from(col < limit),
        "\t" => (TAB_WIDTH - col % TAB_WIDTH).min(limit.saturating_sub(col)),
        _ => cell_width(grapheme),
    }
}

/// Row text as drawn: each tab becomes the spaces to its stop. `limit` is the
/// text width [`layout`] wrapped the row at.
pub fn display(row: &str, limit: usize) -> Cow<'_, str> {
    if !row.contains('\t') {
        return Cow::Borrowed(row);
    }
    let mut out = String::with_capacity(row.len() + TAB_WIDTH);
    let mut col = 0;
    for grapheme in row.graphemes(true) {
        let cells = advance(grapheme, col, limit);
        if grapheme == "\t" {
            out.extend(std::iter::repeat_n(' ', cells));
        } else {
            out.push_str(grapheme);
        }
        col += cells;
    }
    Cow::Owned(out)
}

/// One visual row: a byte range of the draft without its line break.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisualRow {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftLayout {
    pub rows: Vec<VisualRow>,
    /// Caret as (visual row, cell column). The column is below `width` unless
    /// a single grapheme is wider than the row.
    pub caret: (usize, usize),
}

/// Wraps `text` the way an editor does, into rows of `width` cells that
/// include the caret's column, and places the caret.
///
/// Ports `layoutDraft` from the TypeScript composer. Rows break after spaces
/// and tabs, and blanks at a break hang past the row, so a wrapped row never
/// starts with a space the user did not type. A wide grapheme is a word of
/// its own, because CJK text has no spaces to break on. A word that does not
/// fit starts a new row, and splits between graphemes only when it is longer
/// than a row. Thai, Lao, Khmer, and Myanmar get no dictionary word breaks
/// here, so a word in those scripts splits between graphemes.
///
/// Text takes at most `width - 1` cells of a row, and the rows never depend
/// on the caret: a caret after a full row's text takes the remaining column,
/// so moving the caret never moves a word or changes the row count.
pub fn layout(text: &str, caret: usize, width: usize) -> DraftLayout {
    let limit = width.saturating_sub(1).max(1);
    let mut rows = Vec::new();
    let mut line_start = 0;
    for line in text.split('\n') {
        wrap_line(line, line_start, limit, &mut rows);
        line_start += line.len() + 1;
    }
    let caret = place_caret(text, &rows, caret, limit);
    DraftLayout { rows, caret }
}

/// Appends the rows of one logical line that starts at byte `base`.
fn wrap_line(line: &str, base: usize, limit: usize, rows: &mut Vec<VisualRow>) {
    let graphemes: Vec<(usize, &str)> = line
        .grapheme_indices(true)
        .map(|(at, grapheme)| (base + at, grapheme))
        .collect();
    let mut start = base;
    let mut col = 0;
    let mut i = 0;
    while i < graphemes.len() {
        if is_blank(graphemes[i].1) {
            col += advance(graphemes[i].1, col, limit);
            i += 1;
            continue;
        }
        let first = i;
        let mut size = 0;
        while i < graphemes.len() && !is_blank(graphemes[i].1) {
            let cells = cell_width(graphemes[i].1);
            if i > first && (cells > 1 || cell_width(graphemes[i - 1].1) > 1) {
                break;
            }
            size += cells;
            i += 1;
        }
        // A word that does not fit opens a row even when it must then split,
        // so a pasted path or URL starts at the left edge.
        if col > 0 && col + size > limit {
            open_row(rows, &mut start, &mut col, graphemes[first].0);
        }
        for &(at, grapheme) in &graphemes[first..i] {
            let cells = cell_width(grapheme);
            if col > 0 && col + cells > limit {
                open_row(rows, &mut start, &mut col, at);
            }
            col += cells;
        }
    }
    rows.push(VisualRow {
        start,
        end: base + line.len(),
    });
}

fn open_row(rows: &mut Vec<VisualRow>, start: &mut usize, col: &mut usize, at: usize) {
    rows.push(VisualRow {
        start: *start,
        end: at,
    });
    *start = at;
    *col = 0;
}

/// The caret's places on each row [`layout`] draws at `width`, with their
/// cell columns: before each grapheme, and after the text of the row a
/// logical line ends on. A wrapped row's end is the next row's start, so it
/// is not a place on the row.
fn row_stops(text: &str, width: usize) -> Vec<Vec<(usize, usize)>> {
    let limit = width.saturating_sub(1).max(1);
    let line_ends_at = |at: usize| text.as_bytes().get(at).is_none_or(|&byte| byte == b'\n');
    layout(text, 0, width)
        .rows
        .iter()
        .map(|row| {
            let mut column = 0;
            let mut stops: Vec<(usize, usize)> = text[row.start..row.end]
                .grapheme_indices(true)
                .map(|(at, grapheme)| {
                    let stop = (row.start + at, column);
                    column += advance(grapheme, column, limit);
                    stop
                })
                .collect();
            if line_ends_at(row.end) {
                stops.push((row.end, column));
            }
            stops
        })
        .collect()
}

/// The row holding the grapheme at `caret`, or the row a logical line ends on
/// for a caret after its last grapheme, and the caret's column in it.
fn place_caret(text: &str, rows: &[VisualRow], caret: usize, limit: usize) -> (usize, usize) {
    let line_ends_at = |at: usize| text.as_bytes().get(at).is_none_or(|&byte| byte == b'\n');
    let index = rows
        .iter()
        .position(|row| {
            row.start <= caret && (caret < row.end || (caret == row.end && line_ends_at(row.end)))
        })
        .unwrap_or(rows.len() - 1);
    let row = rows[index];
    let mut col = 0;
    if (row.start..=row.end).contains(&caret) {
        for grapheme in text[row.start..caret].graphemes(true) {
            col += advance(grapheme, col, limit);
        }
    }
    (index, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAMILY: &str = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";

    fn typed(text: &str) -> Draft {
        let mut draft = Draft::default();
        draft.type_text(text);
        draft
    }

    /// Every stop a run of word steps makes from one end of `text`, the
    /// caret drawn as `|`: the TypeScript `wordStop` walk.
    fn walk(text: &str, forward: bool) -> Vec<String> {
        let mut caret = if forward { 0 } else { text.len() };
        let mut stops = Vec::new();
        loop {
            let next = word_stop(text, caret, forward, &[]);
            if next == caret {
                return stops;
            }
            caret = next;
            stops.push(format!("{}|{}", &text[..caret], &text[caret..]));
        }
    }

    #[test]
    fn a_word_step_skips_whitespace_then_one_word() {
        assert_eq!(
            walk("hello  world", false),
            ["hello  |world", "|hello  world"]
        );
        assert_eq!(
            walk("hello  world", true),
            ["hello|  world", "hello  world|"]
        );
    }

    #[test]
    fn a_word_step_stops_inside_a_path_and_crosses_punctuation_whole() {
        assert_eq!(
            walk("src/main.ts --fix", false),
            [
                "src/main.ts --|fix",
                "src/main.ts |--fix",
                "src/main.|ts --fix",
                "src/main|.ts --fix",
                "src/|main.ts --fix",
                "src|/main.ts --fix",
                "|src/main.ts --fix",
            ]
        );
        assert_eq!(
            walk("x  ...  y", true),
            ["x|  ...  y", "x  ...|  y", "x  ...  y|"]
        );
    }

    #[test]
    fn a_word_step_crosses_a_line_break_alone() {
        assert_eq!(
            walk("one\ntwo", false),
            ["one\n|two", "one|\ntwo", "|one\ntwo"]
        );
        assert_eq!(
            walk("one\ntwo", true),
            ["one|\ntwo", "one\n|two", "one\ntwo|"]
        );
    }

    #[test]
    fn text_without_spaces_steps_by_unicode_word_segment() {
        // Without the oracle's dictionary, each ideograph is a segment.
        assert_eq!(walk("你好 hi", false), ["你好 |hi", "你|好 hi", "|你好 hi"]);
    }

    #[test]
    fn word_moves_follow_the_oracle_around_a_path() {
        let mut draft = typed("run src/app.ts now");
        draft.word_left();
        assert_eq!(&draft.text()[draft.caret()..], "now");
        draft.word_left();
        assert_eq!(&draft.text()[draft.caret()..], "ts now");
        draft.word_left();
        assert_eq!(&draft.text()[draft.caret()..], ".ts now");
        draft.word_right();
        assert_eq!(&draft.text()[draft.caret()..], "ts now");
    }

    #[test]
    fn kills_join_one_ring_entry_that_yanks_back_and_undoes_by_step() {
        // The TypeScript shell test: Ctrl+W then Alt+Backspace join one entry.
        let mut draft = typed("alpha beta gamma");
        draft.kill_word_left();
        assert_eq!(draft.text(), "alpha beta ");
        draft.kill_word_left();
        assert_eq!(draft.text(), "alpha ");
        draft.yank();
        assert_eq!(draft.text(), "alpha beta gamma");
        assert_eq!(draft.caret(), draft.text().len());
        // Alt+D, then Ctrl+K, from the start: a second entry.
        draft.home();
        draft.kill_word_right();
        assert_eq!(draft.text(), " beta gamma");
        draft.kill_line_right();
        assert_eq!(draft.text(), "");
        draft.yank();
        assert_eq!(draft.text(), "alpha beta gamma");
        // Alt+Y right after a yank swaps in the older kill.
        draft.yank_pop();
        assert_eq!(draft.text(), "beta gamma");
        // Undo takes back the swap, then the yank.
        draft.undo();
        assert_eq!(draft.text(), "alpha beta gamma");
        draft.undo();
        assert_eq!(draft.text(), "");
        // Alt+Y without a yank before it does nothing.
        draft.type_text("x");
        draft.yank_pop();
        assert_eq!(draft.text(), "x");
    }

    #[test]
    fn line_kills_take_the_line_break_at_an_edge() {
        let mut draft = typed("one");
        draft.newline();
        draft.type_text("two");
        draft.kill_line_left();
        assert_eq!(draft.text(), "one\n");
        draft.kill_line_left();
        assert_eq!((draft.text(), draft.caret()), ("one", 3));
        draft.home();
        draft.kill_line_right();
        assert_eq!(draft.text(), "");
        // "two" and the break before it joined; Home ended the run, so
        // "one" is an entry of its own.
        draft.yank();
        assert_eq!(draft.text(), "one");
        draft.yank_pop();
        assert_eq!(draft.text(), "\ntwo");
    }

    #[test]
    fn the_ring_keeps_its_newest_kills() {
        let mut draft = Draft::default();
        for i in 0..RING_LIMIT + 5 {
            draft.type_text(&format!("w{i}"));
            draft.kill_line_left();
            // A move between kills keeps them apart.
            draft.left();
        }
        assert_eq!(draft.ring.len(), RING_LIMIT);
        assert_eq!(draft.ring.front().map(String::as_str), Some("w5"));
    }

    /// The caret's row at `width`, and that row with the caret drawn as `|`.
    fn shown(draft: &Draft, width: usize) -> String {
        let drawn = layout(draft.text(), draft.caret(), width);
        let row = drawn.rows[drawn.caret.0];
        let text = draft.text();
        let caret = draft.caret().clamp(row.start, row.end);
        format!(
            "{}:{}|{}",
            drawn.caret.0,
            &text[row.start..caret],
            &text[caret..row.end]
        )
    }

    fn at(text: &str, caret: usize) -> Draft {
        let mut draft = typed(text);
        draft.caret = caret;
        draft
    }

    #[test]
    fn up_and_down_move_between_lines_and_keep_the_column() {
        let mut draft = at("first line\nsecond line", 3);
        assert!(draft.vertical(40, false));
        assert_eq!(shown(&draft, 40), "1:sec|ond line");
        assert!(draft.vertical(40, true));
        assert_eq!(shown(&draft, 40), "0:fir|st line");
    }

    #[test]
    fn up_and_down_move_between_the_rows_of_a_wrapped_line() {
        // "alpha beta " then "gamma" at 11 columns, as the oracle draws it.
        let text = "alpha beta gamma";
        let mut draft = at(text, 2);
        assert!(draft.vertical(11, false));
        assert_eq!(shown(&draft, 11), "1:ga|mma");
        // Past the end of the last row, the caret goes after its text.
        let mut draft = at(text, 9);
        assert!(draft.vertical(11, false));
        assert_eq!(shown(&draft, 11), "1:gamma|");
        let mut draft = at(text, text.len());
        assert!(draft.vertical(11, true));
        assert_eq!(shown(&draft, 11), "0:alpha| beta ");
        // A wrapped row's last place is before the space hanging past it,
        // and a run of presses keeps the column it started from.
        let mut draft = at(text, text.len());
        draft.goal = Some(30);
        assert!(draft.vertical(11, true));
        assert_eq!(shown(&draft, 11), "0:alpha beta| ");
        assert!(draft.vertical(11, false));
        assert_eq!(shown(&draft, 11), "1:gamma|");
    }

    #[test]
    fn the_first_and_last_rows_leave_up_and_down_to_history() {
        assert!(!at("one\ntwo", 2).vertical(40, true));
        assert!(!at("one\ntwo", 6).vertical(40, false));
        assert!(!at("", 0).vertical(40, true));
        // Before the first frame there are no drawn rows.
        assert!(!at("one\ntwo", 6).vertical(0, true));
    }

    #[test]
    fn recall_browses_history_and_returns_to_the_unsent_draft() {
        let history = [
            "Latest".to_owned(),
            "First line\nsecond line".into(),
            "Oldest".into(),
        ];
        let mut draft = typed("unsent");
        draft.left();
        // Newer than the draft there is nothing.
        assert!(!draft.recall(&history, false, 40));
        assert!(draft.recall(&history, true, 40));
        assert_eq!((draft.text(), draft.caret()), ("Latest", 6));
        // An entry of several rows opens at its start going older ...
        assert!(draft.recall(&history, true, 40));
        assert_eq!(
            (draft.text(), draft.caret()),
            ("First line\nsecond line", 0)
        );
        assert!(draft.recall(&history, true, 40));
        assert_eq!(draft.text(), "Oldest");
        assert!(!draft.recall(&history, true, 40));
        // ... and at its end going newer.
        assert!(draft.recall(&history, false, 40));
        assert_eq!(draft.caret(), draft.text().len());
        // An entry edited in the browse shows its edit again.
        draft.type_text("!");
        assert!(draft.recall(&history, false, 40));
        assert!(draft.recall(&history, true, 40));
        assert_eq!(draft.text(), "First line\nsecond line!");
        // Back past the newest, the unsent draft returns with its caret.
        assert!(draft.recall(&history, false, 40));
        assert!(draft.recall(&history, false, 40));
        assert_eq!((draft.text(), draft.caret()), ("unsent", 5));
        assert!(!draft.recall(&history, false, 40));
    }

    #[test]
    fn one_undo_returns_from_a_whole_browse() {
        let history = ["one".to_owned(), "two".into()];
        let mut draft = typed("mine");
        draft.recall(&history, true, 40);
        draft.recall(&history, true, 40);
        assert_eq!(draft.text(), "two");
        draft.undo();
        assert_eq!(draft.text(), "mine");
        // The browse ended with it; Up starts a new one from the newest.
        draft.recall(&history, true, 40);
        assert_eq!(draft.text(), "one");
        // Without history, nothing changes and no browse stays open.
        let mut draft = typed("mine");
        assert!(!draft.recall(&[], true, 40));
        assert!(draft.visit.is_none());
    }

    #[test]
    fn a_click_lands_before_the_grapheme_at_its_cell() {
        // "alpha beta " / "gamma " / "delta" at 11 columns, as the oracle's
        // `offsetAt` cases draw it.
        let text = "alpha beta gamma delta";
        let place = |text: &str, row, column, width| {
            let mut draft = typed(text);
            draft.place(row, column, width).then(|| draft.caret())
        };
        assert_eq!(place(text, 1, 2, 11), Some(13));
        assert_eq!(place(text, 0, 50, 11), Some(10));
        assert_eq!(place(text, 2, 50, 11), Some(text.len()));
        assert_eq!(place(text, 3, 0, 11), None);
        // Column 1 is the second cell of 你, which a click lands before.
        assert_eq!(place("你好", 0, 1, 40), Some(0));
        assert_eq!(place("你好", 0, 2, 40), Some("你".len()));
    }

    #[test]
    fn row_edges_follow_the_oracle_cases() {
        let text = "alpha beta gamma delta";
        let mut draft = at(text, 13);
        draft.row_home(11);
        assert_eq!(draft.caret(), 11);
        let mut draft = at(text, 13);
        draft.row_end(11);
        assert_eq!(draft.caret(), 16);
        let mut draft = at(text, 11);
        draft.row_home(11);
        assert_eq!(draft.caret(), 0);
        let mut draft = at(text, 16);
        draft.row_end(11);
        assert_eq!(draft.caret(), text.len());
        let mut draft = at("one\ntwo", 5);
        draft.row_home(40);
        assert_eq!(draft.caret(), 4);
        draft.row_home(40);
        assert_eq!(draft.caret(), 4);
    }

    #[test]
    fn home_and_end_reach_the_drawn_row_then_the_logical_line() {
        // At width 8 the text takes seven cells a row: "alpha " / "beta " / "gamma".
        let mut draft = typed("alpha beta gamma");
        assert_eq!(layout(draft.text(), draft.caret(), 8).rows.len(), 3);
        draft.row_home(8);
        assert_eq!(&draft.text()[draft.caret()..], "gamma");
        draft.row_home(8);
        assert_eq!(draft.caret(), 0);
        draft.row_end(8);
        // A wrapped row's last place is before its last grapheme.
        assert_eq!(&draft.text()[draft.caret()..], " beta gamma");
        draft.row_end(8);
        assert_eq!(draft.caret(), draft.text().len());
        // Before the first frame, there is no drawn row.
        draft.row_home(0);
        assert_eq!(draft.caret(), 0);
    }

    #[test]
    fn a_long_paste_collapses_into_a_placeholder_that_edits_as_one() {
        assert!(collapses("a\nb\nc") && !collapses("a\nb"));
        assert!(collapses(&"x".repeat(801)) && !collapses(&"x".repeat(800)));
        assert_eq!(pasted_text_token(1, "a\nb\nc"), "[Pasted text #1 +2 lines]");
        assert_eq!(
            pasted_text_token(2, &"x".repeat(900)),
            "[Pasted text #2 900 chars]"
        );
        let pasted = "line 0\nline 1\nline 2\nline 3";
        let mut draft = typed("see ");
        assert!(draft.paste_block(pasted));
        assert_eq!(draft.text(), "see [Pasted text #1 +3 lines]");
        assert_eq!(draft.expanded(), format!("see {pasted}"));
        // A short paste stays text.
        draft.paste_block(" ok");
        assert_eq!(draft.text(), "see [Pasted text #1 +3 lines] ok");
        // The caret, word steps, and Backspace cross the placeholder whole.
        draft.word_left();
        draft.left();
        draft.left();
        assert_eq!(draft.caret(), 4);
        draft.right();
        assert_eq!(&draft.text()[draft.caret()..], " ok");
        draft.word_left();
        assert_eq!(draft.caret(), 4);
        draft.word_right();
        assert_eq!(&draft.text()[draft.caret()..], " ok");
        draft.backspace();
        assert_eq!(draft.text(), "see  ok");
        // Undo brings it back, still standing for its text.
        draft.undo();
        assert_eq!(draft.expanded(), format!("see {pasted} ok"));
    }

    #[test]
    fn a_placeholder_is_killed_and_yanked_whole_and_never_split() {
        let pasted = "a\nb\nc";
        let mut draft = Draft::default();
        draft.paste_block(pasted);
        // Ctrl+U takes it; Ctrl+Y puts it back as the paste.
        draft.kill_line_left();
        assert_eq!(draft.text(), "");
        draft.type_text("first ");
        draft.yank();
        assert_eq!(draft.expanded(), format!("first {pasted}"));
        // A kill that starts inside it widens to take it all.
        let start = "first [Pasted".len();
        draft.caret = start;
        draft.kill(start, draft.text().len(), false);
        assert_eq!(draft.text(), "first ");
        // A click or a row edge inside it lands before it.
        draft.yank();
        assert!(draft.place(0, 10, 80));
        assert_eq!(draft.caret(), "first ".len());
    }

    #[test]
    fn a_word_step_crosses_a_placeholder_whole() {
        let atoms = [("[Image #1]".to_owned(), Atom::Image(1))];
        let walk = |forward: bool| {
            let text = "a [Image #1] b";
            let mut caret = if forward { 0 } else { text.len() };
            let mut stops = Vec::new();
            loop {
                let next = word_stop(text, caret, forward, &atoms);
                if next == caret {
                    return stops;
                }
                caret = next;
                stops.push(format!("{}|{}", &text[..caret], &text[caret..]));
            }
        };
        assert_eq!(
            walk(false),
            ["a [Image #1] |b", "a |[Image #1] b", "|a [Image #1] b"]
        );
        assert_eq!(
            walk(true),
            ["a| [Image #1] b", "a [Image #1]| b", "a [Image #1] b|"]
        );
    }

    #[test]
    fn zwj_emoji_moves_and_deletes_as_one_grapheme() {
        let mut draft = typed("a");
        draft.paste(FAMILY);
        draft.type_text("b");
        draft.left();
        draft.left();
        assert_eq!(draft.caret(), 1);
        draft.right();
        assert_eq!(draft.caret(), 1 + FAMILY.len());
        draft.backspace();
        assert_eq!(draft.text(), "ab");
        assert_eq!(draft.caret(), 1);
    }

    #[test]
    fn zwj_typed_between_emoji_keeps_caret_after_the_joined_sequence() {
        let mut draft = typed("\u{1F468}\u{1F469}");
        draft.left();
        draft.type_text("\u{200D}");
        assert_eq!(draft.text(), "\u{1F468}\u{200D}\u{1F469}");
        assert_eq!(draft.caret(), draft.text().len());
    }

    #[test]
    fn combining_marks_stay_with_their_base() {
        let mut draft = typed("e\u{301}x");
        draft.left();
        draft.left();
        assert_eq!(draft.caret(), 0);
        draft.delete();
        assert_eq!(draft.text(), "x");

        // A base inserted before a lone mark joins it; the caret skips the pair.
        let mut draft = typed("\u{301}");
        draft.home();
        draft.type_text("a");
        assert_eq!(draft.caret(), draft.text().len());
    }

    #[test]
    fn backspace_that_joins_neighbors_leaves_the_caret_after_the_joined_grapheme() {
        let mut draft = typed("e");
        draft.newline();
        draft.type_text("\u{301}");
        draft.left();
        assert_eq!(draft.caret(), 2);
        draft.backspace();
        assert_eq!((draft.text(), draft.caret()), ("e\u{301}", 3));
        draft.left();
        assert_eq!(draft.caret(), 0);
        draft.right();
        assert_eq!(draft.caret(), 3);
        draft.backspace();
        assert_eq!(draft.text(), "");
        // Consecutive deletions undo together.
        assert!(draft.undo());
        assert_eq!((draft.text(), draft.caret()), ("e\u{301}", 3));
        assert!(draft.undo());
        assert_eq!((draft.text(), draft.caret()), ("e\n\u{301}", 2));
    }

    #[test]
    fn delete_that_joins_neighbors_leaves_the_caret_before_the_joined_grapheme() {
        let mut draft = typed("e");
        draft.newline();
        draft.type_text("\u{301}");
        draft.home();
        draft.left();
        assert_eq!(draft.caret(), 1);
        draft.delete();
        assert_eq!((draft.text(), draft.caret()), ("e\u{301}", 0));
        draft.right();
        assert_eq!(draft.caret(), 3);
        draft.left();
        draft.delete();
        assert_eq!(draft.text(), "");
        // Movement between the deletions makes them separate undo steps.
        assert!(draft.undo());
        assert_eq!((draft.text(), draft.caret()), ("e\u{301}", 0));
        assert!(draft.undo());
        assert_eq!((draft.text(), draft.caret()), ("e\n\u{301}", 1));
    }

    #[test]
    fn deleting_between_regional_indicators_re_pairs_them_without_splitting() {
        const US: &str = "\u{1F1FA}";
        const S: &str = "\u{1F1F8}";
        const A: &str = "\u{1F1E6}";
        // US, newline, S+A: removing the newline pairs US+S and leaves A alone.
        let mut draft = typed(US);
        draft.newline();
        draft.type_text(&format!("{S}{A}"));
        draft.home();
        draft.backspace();
        let joined = format!("{US}{S}{A}");
        assert_eq!(draft.text(), joined);
        assert_eq!(draft.caret(), US.len() + S.len());
        draft.right();
        assert_eq!(draft.caret(), joined.len());

        let mut draft = typed(US);
        draft.newline();
        draft.type_text(&format!("{S}{A}"));
        draft.home();
        draft.left();
        draft.delete();
        assert_eq!((draft.text(), draft.caret()), (joined.as_str(), 0));
        draft.right();
        assert_eq!(draft.caret(), US.len() + S.len());
    }

    #[test]
    fn thai_clusters_move_and_delete_whole() {
        let mut draft = typed("\u{0E01}\u{0E33}\u{0E25}\u{0E31}\u{0E07}");
        draft.backspace();
        assert_eq!(draft.text(), "\u{0E01}\u{0E33}\u{0E25}\u{0E31}");
        draft.backspace();
        assert_eq!(draft.text(), "\u{0E01}\u{0E33}");
        draft.left();
        assert_eq!(draft.caret(), 0);
    }

    #[test]
    fn sanitize_normalizes_line_breaks_and_drops_escape_controls() {
        assert_eq!(
            sanitize("a\r\nb\rc\x1b[31m\u{9b}d\tz\x07"),
            "a\nb\nc[31md\tz"
        );
    }

    #[test]
    fn paste_is_one_undo_step_and_typing_runs_coalesce() {
        let mut draft = typed("hi");
        draft.paste("one\r\ntwo");
        assert_eq!(draft.text(), "hione\ntwo");
        assert!(draft.undo());
        assert_eq!((draft.text(), draft.caret()), ("hi", 2));
        assert!(draft.undo());
        assert_eq!(draft.text(), "");
        assert!(!draft.undo());
    }

    #[test]
    fn movement_splits_typing_into_separate_undo_steps() {
        let mut draft = typed("ab");
        draft.home();
        draft.type_text("x");
        draft.undo();
        assert_eq!((draft.text(), draft.caret()), ("ab", 0));
    }

    #[test]
    fn undo_history_is_bounded() {
        let mut draft = Draft::default();
        for _ in 0..UNDO_DEPTH + 10 {
            draft.paste("x");
        }
        assert_eq!(draft.undo_len(), UNDO_DEPTH);
    }

    #[test]
    fn draft_limit_cuts_at_a_grapheme_boundary() {
        let mut draft = Draft::default();
        assert!(draft.paste(&"a".repeat(MAX_DRAFT_BYTES - 1)));
        assert!(!draft.paste(FAMILY));
        assert_eq!(draft.text().len(), MAX_DRAFT_BYTES - 1);
        assert!(!draft.paste("bc"));
        assert_eq!(draft.text().len(), MAX_DRAFT_BYTES);
    }

    #[test]
    fn home_and_end_use_the_logical_line() {
        let mut draft = typed("one");
        draft.newline();
        draft.type_text("two");
        draft.home();
        assert_eq!(draft.caret(), 4);
        draft.left();
        draft.home();
        assert_eq!(draft.caret(), 0);
        draft.end();
        assert_eq!(draft.caret(), 3);
    }

    #[test]
    fn layout_wraps_wide_graphemes_and_places_caret_with_the_same_widths() {
        let text = "\u{4F60}\u{597D}\u{4E16}\u{754C}";
        let rows = layout(text, text.len(), 5);
        assert_eq!(rows.rows.len(), 2);
        assert_eq!(
            &text[rows.rows[0].start..rows.rows[0].end],
            "\u{4F60}\u{597D}"
        );
        assert_eq!(rows.caret, (1, 4));
    }

    fn rows_of(text: &str, width: usize) -> Vec<&str> {
        layout(text, 0, width)
            .rows
            .iter()
            .map(|row| &text[row.start..row.end])
            .collect()
    }

    #[test]
    fn rows_break_after_blanks_and_blanks_at_a_break_hang() {
        assert_eq!(rows_of("hello big world", 10), ["hello big ", "world"]);
        assert_eq!(rows_of("hello   world", 8), ["hello   ", "world"]);
        assert_eq!(rows_of("  indented", 40), ["  indented"]);
    }

    #[test]
    fn a_long_word_starts_its_own_row_and_splits_between_graphemes() {
        assert_eq!(
            rows_of("go https://example.com/x", 10),
            ["go ", "https://e", "xample.co", "m/x"]
        );
    }

    #[test]
    fn wide_graphemes_are_break_opportunities() {
        assert_eq!(
            rows_of("ab\u{4F60}\u{597D}cd", 6),
            ["ab\u{4F60}", "\u{597D}cd"]
        );
    }

    #[test]
    fn rows_never_depend_on_the_caret_and_fit_their_width() {
        let text = format!(
            "one two\tthree {FAMILY} \u{4F60}\u{597D} e\u{301}e\u{301} https://example.com/a/b\n\n   last"
        );
        let boundaries: Vec<usize> = text
            .grapheme_indices(true)
            .map(|(at, _)| at)
            .chain([text.len()])
            .collect();
        for width in 1..30 {
            let rows = layout(&text, 0, width).rows;
            for &at in &boundaries {
                let placed = layout(&text, at, width);
                assert_eq!(placed.rows, rows, "width {width}, caret {at}");
                if width >= 3 {
                    assert!(placed.caret.1 < width, "width {width}, caret {at}");
                }
            }
            if width < 3 {
                continue;
            }
            let limit = width - 1;
            for (index, row) in rows.iter().enumerate() {
                let mut col = 0;
                for grapheme in text[row.start..row.end].graphemes(true) {
                    col += advance(grapheme, col, limit);
                }
                assert!(col <= limit, "width {width}, row {index} is {col} cells");
                // Only a logical line's first row may open with a blank.
                let soft = row.start > 0 && text.as_bytes()[row.start - 1] != b'\n';
                assert!(
                    !(soft && text[row.start..].starts_with([' ', '\t'])),
                    "width {width}, row {index} opens with a blank"
                );
            }
        }
    }

    #[test]
    fn caret_after_a_full_row_takes_the_remaining_column() {
        let placed = layout("abcd", 4, 5);
        assert_eq!((placed.rows.len(), placed.caret), (1, (0, 4)));
        // A blank typed after a full row hangs; the caret stays on that row.
        let placed = layout("abcd ", 5, 5);
        assert_eq!((placed.rows.len(), placed.caret), (1, (0, 4)));
        // The next word opens a row of its own.
        assert_eq!(rows_of("abcd x", 5), ["abcd ", "x"]);
        assert_eq!(layout("ab\ncd", 2, 4).caret, (0, 2));
    }

    #[test]
    fn zero_width_graphemes_stay_on_a_full_row() {
        for mark in ["\u{200B}", "\u{2060}", "\u{FEFF}"] {
            let text = format!("abcd{mark}x");
            let first = format!("abcd{mark}");
            assert_eq!(rows_of(&text, 5), [first.as_str(), "x"]);
            assert_eq!(layout(&text, 4, 5).caret, (0, 4), "caret before {mark:?}");
            assert_eq!(layout(&text, text.len(), 5).caret, (1, 1));
        }
        // A wide grapheme overfills a one-cell row; what follows still wraps.
        let text = "\u{4F60}\u{200B}";
        let placed = layout(text, 3, 1);
        assert_eq!(placed.caret, (1, 0));
        assert_eq!(&text[placed.rows[1].start..placed.rows[1].end], "\u{200B}");
    }

    #[test]
    fn tabs_reach_the_next_stop() {
        assert_eq!(layout("a\tb", 3, 80).caret, (0, TAB_WIDTH + 1));
        assert_eq!(display("a\tb", 79), "a   b");
        assert_eq!(display("abcd\t", 79), "abcd    ");
        // A tab that reaches the row's end hangs like a space.
        assert_eq!(rows_of("abc\tdef", 5), ["abc\t", "def"]);
    }
}
