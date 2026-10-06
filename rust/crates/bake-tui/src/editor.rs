//! Pure composer draft: grapheme-safe editing, bounded undo, and wrapping.
//!
//! Wrapping and caret placement share [`cell_width`], which delegates to the
//! measure Ratatui's buffer uses, so drawn text and the cursor agree.

use std::borrow::Cow;
use std::collections::VecDeque;

use ratatui::buffer::CellWidth;
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

/// Largest draft kept in memory; longer input is cut at a grapheme boundary.
pub const MAX_DRAFT_BYTES: usize = 256 * 1024;
/// Undo steps retained; the oldest step is dropped first.
pub const UNDO_DEPTH: usize = 64;
/// Cells a tab occupies wherever it appears in a row.
pub const TAB_WIDTH: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditKind {
    Type,
    Delete,
    Block,
}

/// Draft text with a caret that always sits on a grapheme boundary.
#[derive(Clone, Debug, Default)]
pub struct Draft {
    text: String,
    caret: usize,
    undo: VecDeque<(String, usize)>,
    last_edit: Option<EditKind>,
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
        self.move_to(self.prev_boundary(self.caret));
    }

    pub fn right(&mut self) {
        self.move_to(self.next_boundary(self.caret));
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

    /// Restores the text and caret before the latest step. Returns `false` when
    /// there is nothing to undo.
    pub fn undo(&mut self) -> bool {
        self.last_edit = None;
        match self.undo.pop_back() {
            Some((text, caret)) => {
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
        if kind == EditKind::Block || self.last_edit != Some(kind) {
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

/// Terminal cells one grapheme occupies in the composer.
pub fn cell_width(grapheme: &str) -> usize {
    if grapheme == "\t" {
        TAB_WIDTH
    } else {
        usize::from(grapheme.cell_width())
    }
}

/// Row text as drawn: tabs become [`TAB_WIDTH`] spaces.
pub fn display(row: &str) -> Cow<'_, str> {
    if row.contains('\t') {
        Cow::Owned(row.replace('\t', &" ".repeat(TAB_WIDTH)))
    } else {
        Cow::Borrowed(row)
    }
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
    /// Caret as (visual row, cell column); the column is always below `width`.
    pub caret: (usize, usize),
}

/// Wraps `text` at grapheme boundaries into rows of at most `width` cells. A
/// grapheme wider than `width` gets a row of its own. A caret at the end of a
/// full row is placed at the start of an extra empty row.
pub fn layout(text: &str, caret: usize, width: usize) -> DraftLayout {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut caret_at = None;
    let mut line_start = 0;
    for line in text.split('\n') {
        let line_end = line_start + line.len();
        let mut row_start = line_start;
        let mut col = 0;
        for (offset, grapheme) in line.grapheme_indices(true) {
            let at = line_start + offset;
            let w = cell_width(grapheme);
            // `col >= width` also wraps a zero-width grapheme after a full or
            // overfilled row, so the caret never sits past the last column.
            if col > 0 && (col >= width || col + w > width) {
                rows.push(VisualRow {
                    start: row_start,
                    end: at,
                });
                row_start = at;
                col = 0;
            }
            if at == caret {
                caret_at = Some((rows.len(), col));
            }
            col += w;
        }
        rows.push(VisualRow {
            start: row_start,
            end: line_end,
        });
        if caret == line_end {
            caret_at = Some(if col >= width {
                rows.push(VisualRow {
                    start: line_end,
                    end: line_end,
                });
                (rows.len() - 1, 0)
            } else {
                (rows.len() - 1, col)
            });
        }
        line_start = line_end + 1;
    }
    DraftLayout {
        caret: caret_at.unwrap_or((rows.len() - 1, 0)),
        rows,
    }
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

    #[test]
    fn caret_after_a_full_row_starts_a_new_row() {
        let rows = layout("abcd", 4, 4);
        assert_eq!(rows.rows.len(), 2);
        assert_eq!(rows.caret, (1, 0));
        let rows = layout("ab\ncd", 2, 4);
        assert_eq!(rows.caret, (0, 2));
    }

    #[test]
    fn zero_width_grapheme_after_a_full_row_starts_a_new_row() {
        for mark in ["\u{200B}", "\u{2060}", "\u{FEFF}"] {
            let text = format!("abcd{mark}x");
            let rows = layout(&text, 4, 4);
            assert_eq!(rows.caret, (1, 0), "caret before {mark:?}");
            assert_eq!(&text[rows.rows[0].start..rows.rows[0].end], "abcd");
            assert_eq!(
                &text[rows.rows[1].start..rows.rows[1].end],
                format!("{mark}x")
            );
            assert_eq!(layout(&text, text.len(), 4).caret, (1, 1));
        }
        // A wide grapheme overfills a one-cell row; what follows still wraps.
        let text = "\u{4F60}\u{200B}";
        let rows = layout(text, 3, 1);
        assert_eq!(rows.caret, (1, 0));
        assert_eq!(&text[rows.rows[1].start..rows.rows[1].end], "\u{200B}");
    }

    #[test]
    fn tabs_count_as_fixed_cells() {
        let rows = layout("a\tb", 3, 80);
        assert_eq!(rows.caret, (0, 1 + TAB_WIDTH + 1));
        assert_eq!(display("a\tb"), "a    b");
    }
}
