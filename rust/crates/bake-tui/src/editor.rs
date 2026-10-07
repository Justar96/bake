//! Pure composer draft: grapheme-safe editing, bounded undo, and wrapping.
//!
//! Wrapping, tab expansion, and caret placement share one cell measure, which
//! delegates to the measure Ratatui's buffer uses, so drawn text and the
//! cursor agree.

use std::borrow::Cow;
use std::collections::VecDeque;

use ratatui::buffer::CellWidth;
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

/// Largest draft kept in memory; longer input is cut at a grapheme boundary.
pub const MAX_DRAFT_BYTES: usize = 256 * 1024;
/// Undo steps retained; the oldest step is dropped first.
pub const UNDO_DEPTH: usize = 64;
/// Tab stops fall every `TAB_WIDTH` cells from the start of a row.
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
