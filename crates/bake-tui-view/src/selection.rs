//! Transcript selection: points in transcript coordinates, word and row
//! ranges, and the cells a selection covers on each drawn line.
//!
//! The preview reports the mouse, so the terminal's own selection is gone and
//! the view draws and copies one instead. A point names a line by the
//! transcript row and line that draw it, not by its place on screen, so a
//! selection stays on its text while the view scrolls or output arrives
//! below.

use std::cmp::Ordering;
use std::time::Duration;

use unicode_segmentation::UnicodeSegmentation;

use crate::editor::cell_width;
use crate::transcript::Anchor;

/// A cell of the transcript: the line that draws it, and its column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub at: Anchor,
    pub column: usize,
    /// The column is the cell after the selection rather than its last
    /// cell: a word or row range ends on a boundary, while a dragged point
    /// covers its cell.
    pub boundary: bool,
}

impl Point {
    pub fn new(at: Anchor, column: usize) -> Self {
        Self {
            at,
            column,
            boundary: false,
        }
    }

    fn at_boundary(at: Anchor, column: usize) -> Self {
        Self {
            at,
            column,
            boundary: true,
        }
    }
}

/// Two points, the first not after the second.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub start: Point,
    pub end: Point,
}

/// How a press extends: a cell at a time, a word, or a whole line, for one,
/// two, or three clicks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Granularity {
    Character,
    Word,
    Line,
}

/// Presses closer together than this, on the same word, count toward a
/// double or triple click.
pub const CLICK: Duration = Duration::from_millis(500);

/// Characters that join words into one, as terminals select a path or a
/// kebab-case name whole on a double click.
const JOINERS: [&str; 2] = ["/", "-"];

/// Orders two lines by the row and line that draw them.
pub fn compare_lines(a: Anchor, b: Anchor) -> Ordering {
    (a.row, a.line).cmp(&(b.row, b.line))
}

fn compare_points(a: Point, b: Point) -> Ordering {
    compare_lines(a.at, b.at).then(a.column.cmp(&b.column))
}

/// The selection between an anchor and a focus, or `None` while they are
/// one cell.
pub fn ordered(anchor: Point, focus: Point) -> Option<Range> {
    if anchor == focus {
        return None;
    }
    Some(if compare_points(anchor, focus).is_le() {
        Range {
            start: anchor,
            end: focus,
        }
    } else {
        Range {
            start: focus,
            end: anchor,
        }
    })
}

/// Each grapheme of a line with the half-open cells it spans.
fn cells(text: &str) -> Vec<(&str, usize, usize)> {
    let mut column = 0;
    text.graphemes(true)
        .map(|grapheme| {
            let from = column;
            column += cell_width(grapheme);
            (grapheme, from, column)
        })
        .collect()
}

fn text_width(text: &str) -> usize {
    cells(text).last().map_or(0, |&(_, _, to)| to)
}

/// The half-open cells of line `at` that `range` covers, given the line as
/// drawn, which widens a cut through a wide character; `None` when the
/// selection misses the line.
pub fn line_columns(range: Range, at: Anchor, text: &str) -> Option<(usize, usize)> {
    if compare_lines(at, range.start.at).is_lt() || compare_lines(at, range.end.at).is_gt() {
        return None;
    }
    let width = text_width(text);
    let cells = cells(text);
    let cell_at = |column: usize| {
        cells
            .iter()
            .find(|&&(_, from, to)| column >= from && column < to)
    };
    let from = if compare_lines(at, range.start.at).is_eq() {
        cell_at(range.start.column).map_or(range.start.column.min(width), |c| c.1)
    } else {
        0
    };
    let to = if !compare_lines(at, range.end.at).is_eq() {
        width
    } else if range.end.boundary {
        range.end.column.min(width)
    } else {
        cell_at(range.end.column).map_or((range.end.column + 1).min(width), |c| c.2)
    };
    (to > from).then_some((from, to))
}

/// The graphemes of `text` that start inside cells `from..to`.
///
/// When `to` reaches the end of `text`, trailing zero-width graphemes (such as
/// an intentional trailing U+FEFF) at column `to` are included as well.
pub fn slice_cells(text: &str, from: usize, to: usize) -> String {
    let grapheme_cells = cells(text);
    let width = grapheme_cells.last().map_or(0, |&(_, _, end)| end);
    grapheme_cells
        .into_iter()
        .filter(|&(_, start, end)| {
            start >= from && (start < to || (to == width && start == to && end == to))
        })
        .map(|(grapheme, ..)| grapheme)
        .collect()
}

/// One source unit (a grapheme cluster or `\t`) inside a [`SourceLine`], with
/// its byte range in [`SourceLine::text`] and its half-open visual cell columns
/// `[from, to)` on the rendered line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CellSpan {
    pub(crate) byte_start: usize,
    pub(crate) byte_end: usize,
    pub(crate) from: usize,
    pub(crate) to: usize,
}

/// Source-backed mapping for a single rendered transcript line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceLine {
    /// Leading layout gutter cells to skip when copying (e.g. `> `, the
    /// two-space rail, fenced-code `  │ `, or soft-wrap continuation indent).
    pub(crate) gutter_cells: usize,
    /// Visual cell column where `text` starts on this line. Any cells in
    /// `gutter_cells..content_col` (such as a formatted list bullet `• ` on
    /// the first line of a list item) are sliced from [`CopyLine::visual`].
    pub(crate) content_col: usize,
    /// Exact source text for this visual row (with original `\t` and trailing
    /// whitespace intact before display tab expansion or visual trimming).
    pub(crate) text: String,
    /// Visual cell spans for each source unit in `text`, in ascending order.
    pub(crate) spans: Vec<CellSpan>,
    /// Whether this visual row is a soft-wrap continuation of the preceding
    /// visual row (no `\n` inserted before it when both are selected).
    pub(crate) wrapped_from_prev: bool,
    /// Whether this visual row soft-wraps into the following visual row.
    pub(crate) wrapped_to_next: bool,
}

/// One rendered transcript line paired with optional [`SourceLine`] metadata
/// for faithful copy extraction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CopyLine {
    /// The rendered visual line text, matching the terminal cell grid.
    pub(crate) visual: String,
    /// Structured source mapping when the line is backed by wrapped source
    /// text; `None` for visual-only regions (tables, diagrams, banners, rules,
    /// tool cards).
    pub(crate) source: Option<SourceLine>,
}

impl CopyLine {
    /// A line without source metadata, copied from its rendered visual cells.
    pub(crate) fn visual(visual: String) -> Self {
        Self {
            visual,
            source: None,
        }
    }
}

/// Appends printable text plus `\t` and `\n` from `chunk`, dropping any raw
/// control characters (such as concealed ESC / C0 / C1 / `\r`) so untrusted
/// output cannot smuggle terminal control sequences into the clipboard.
fn push_safe_text(out: &mut String, chunk: &str) {
    for ch in chunk.chars() {
        if ch == '\t' || ch == '\n' || !ch.is_control() {
            out.push(ch);
        }
    }
}

fn is_wrap_blank(unit: &str) -> bool {
    unit == " " || unit == "\t"
}

fn slice_source_line(
    src: &SourceLine,
    visual: &str,
    range: Range,
    is_start: bool,
    is_end: bool,
    out: &mut String,
) {
    let raw_from = if is_start { range.start.column } else { 0 };
    let from = raw_from.max(src.gutter_cells);
    let to = if !is_end {
        usize::MAX
    } else if range.end.boundary {
        range.end.column
    } else {
        range.end.column.saturating_add(1)
    };
    if to <= from {
        return;
    }

    // Formatted visual prefix between `gutter_cells` and `content_col`
    // (for example a list bullet `• ` or blockquote bar `│ ` on the first
    // line of an item, or unclamped leading indentation).
    if from < src.content_col {
        let vis_from = from;
        let vis_to = to.min(src.content_col);
        if vis_to > vis_from {
            for (grapheme, g_from, g_to) in cells(visual) {
                if g_from >= src.content_col {
                    break;
                }
                if g_to > vis_from && g_from < vis_to && g_from >= src.gutter_cells {
                    push_safe_text(out, grapheme);
                }
            }
        }
    }

    let content_from = from.max(src.content_col);
    let content_end = src.spans.last().map_or(src.content_col, |span| span.to);
    // When a line soft-wraps into the next line and the selection ends on
    // this line (without continuing onto the next), cap at the trimmed visual
    // end so a trailing wrap-break space is not copied unintentionally.
    let effective_to = if is_end && src.wrapped_to_next {
        to.min(text_width(visual))
    } else {
        to
    };
    let reaches_end = !is_end || effective_to >= content_end;

    for &span in &src.spans {
        let unit = &src.text[span.byte_start..span.byte_end];
        let included = if span.from == span.to {
            // Zero-width grapheme (e.g. intentional U+FEFF BOM, U+200B) or
            // hanging space/tab past the wrap limit.
            if !is_end {
                span.from >= content_from
            } else if src.wrapped_to_next {
                span.from >= content_from
                    && (span.from < effective_to
                        || (reaches_end && span.from == effective_to && !is_wrap_blank(unit)))
            } else {
                span.from >= content_from
                    && (span.from < effective_to || (reaches_end && span.from == effective_to))
            }
        } else {
            span.to > content_from && span.from < effective_to
        };
        if included {
            push_safe_text(out, unit);
        }
    }
}

/// Extracts the text covered by `range` from the presented `lines`.
///
/// Lines with [`SourceLine`] metadata are sliced from their source spans with
/// layout gutters removed, soft-wrapped continuations rejoined without
/// spurious newlines, and original tabs, trailing whitespace, and intentional
/// Unicode (including U+FEFF) preserved. Lines without source metadata fall
/// back to slicing rendered visual cells. Control characters other than `\t`
/// and `\n` are stripped.
///
/// Returns `None` when nothing is covered. Whitespace-only selections that
/// cover actual characters return `Some(...)`.
pub(crate) fn extract(range: Range, lines: &[(Anchor, CopyLine)]) -> Option<String> {
    let mut out = String::new();
    let mut has_prev = false;
    let mut prev_wrapped_to_next = false;

    for &(at, ref line) in lines {
        if compare_lines(at, range.start.at).is_lt() || compare_lines(at, range.end.at).is_gt() {
            continue;
        }
        let is_start = compare_lines(at, range.start.at).is_eq();
        let is_end = compare_lines(at, range.end.at).is_eq();

        let wrapped_from_prev = line
            .source
            .as_ref()
            .is_some_and(|src| src.wrapped_from_prev);

        if has_prev && !(wrapped_from_prev && prev_wrapped_to_next) {
            out.push('\n');
        }
        has_prev = true;

        match &line.source {
            Some(src) => {
                slice_source_line(src, &line.visual, range, is_start, is_end, &mut out);
                prev_wrapped_to_next = src.wrapped_to_next;
            }
            None => {
                if let Some((from, to)) = line_columns(range, at, &line.visual) {
                    let sliced = slice_cells(&line.visual, from, to);
                    push_safe_text(&mut out, sliced.trim_end_matches(' '));
                }
                prev_wrapped_to_next = false;
            }
        }
    }

    (!out.is_empty()).then_some(out)
}

/// The half-open cells of the word under `column`, joined across `/` and
/// `-` so a path or a hyphenated name is one word, as a terminal's double
/// click takes it; `None` between words.
pub fn word_at(text: &str, column: usize) -> Option<(usize, usize)> {
    struct Segment {
        from: usize,
        to: usize,
        selectable: bool,
        joiner: bool,
    }
    let mut at = 0;
    let segments: Vec<Segment> = text
        .split_word_bounds()
        .map(|segment| {
            let from = at;
            at += segment.graphemes(true).map(cell_width).sum::<usize>();
            let joiner = JOINERS.contains(&segment);
            Segment {
                from,
                to: at,
                selectable: joiner || segment.chars().any(char::is_alphanumeric),
                joiner,
            }
        })
        .collect();
    let clicked = segments
        .iter()
        .position(|s| column >= s.from && column < s.to)?;
    if !segments[clicked].selectable {
        return None;
    }
    let joins = |left: &Segment, right: &Segment| {
        left.selectable && right.selectable && (left.joiner || right.joiner)
    };
    let mut first = clicked;
    let mut last = clicked;
    while first > 0 && joins(&segments[first - 1], &segments[first]) {
        first -= 1;
    }
    while last + 1 < segments.len() && joins(&segments[last], &segments[last + 1]) {
        last += 1;
    }
    Some((segments[first].from, segments[last].to))
}

/// The word or whole line around `point`, as a double or triple click takes
/// it; a line runs to `width`, the transcript's.
pub fn range_at(point: Point, granularity: Granularity, text: &str, width: usize) -> Option<Range> {
    match granularity {
        Granularity::Character => None,
        Granularity::Line => Some(Range {
            start: Point::new(point.at, 0),
            end: Point::at_boundary(point.at, width),
        }),
        Granularity::Word => word_at(text, point.column).map(|(from, to)| Range {
            start: Point::new(point.at, from),
            end: Point::at_boundary(point.at, to),
        }),
    }
}

/// The point under a pointer `row` lines below the first of `lines`, the
/// lines on screen: above them is their start, below them their end, and a
/// column is held inside the transcript's `width`.
pub fn point_at(column: usize, row: isize, lines: &[Anchor], width: usize) -> Option<Point> {
    let last = *lines.last()?;
    if row < 0 {
        return Some(Point::new(lines[0], 0));
    }
    match lines.get(row.unsigned_abs()) {
        Some(&at) => Some(Point::new(at, column.min(width.saturating_sub(1)))),
        None => Some(Point::at_boundary(last, width)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(row: usize, line: usize) -> Anchor {
        Anchor { row, line }
    }

    #[test]
    fn a_range_orders_its_ends_and_is_empty_on_one_cell() {
        let a = Point::new(at(2, 0), 4);
        let b = Point::new(at(1, 3), 9);
        assert_eq!(ordered(a, b), Some(Range { start: b, end: a }));
        assert_eq!(ordered(a, a), None);
    }

    #[test]
    fn a_range_covers_whole_middle_lines_and_its_ends_cells() {
        let range = Range {
            start: Point::new(at(0, 1), 2),
            end: Point::new(at(1, 0), 3),
        };
        assert_eq!(line_columns(range, at(0, 0), "abcdef"), None);
        assert_eq!(line_columns(range, at(0, 1), "abcdef"), Some((2, 6)));
        assert_eq!(line_columns(range, at(0, 2), "abc"), Some((0, 3)));
        // A dragged end covers its own cell.
        assert_eq!(line_columns(range, at(1, 0), "abcdef"), Some((0, 4)));
        // A cut through a wide character takes the whole character.
        let wide = Range {
            start: Point::new(at(0, 0), 1),
            end: Point::new(at(0, 0), 2),
        };
        assert_eq!(line_columns(wide, at(0, 0), "你好"), Some((0, 4)));
        assert_eq!(slice_cells("你好x", 0, 4), "你好");
    }

    #[test]
    fn a_word_joins_across_slashes_and_hyphens() {
        let text = "  read src/app-shell.ts now";
        assert_eq!(word_at(text, 3), Some((2, 6)));
        assert_eq!(word_at(text, 10), Some((7, 23)));
        assert_eq!(word_at(text, 6), None);
        assert_eq!(word_at(text, 99), None);
    }

    #[test]
    fn a_pointer_off_the_lines_reaches_their_ends() {
        let lines = [at(0, 0), at(0, 1)];
        assert_eq!(point_at(5, -1, &lines, 40), Some(Point::new(at(0, 0), 0)));
        assert_eq!(point_at(99, 1, &lines, 40), Some(Point::new(at(0, 1), 39)));
        assert_eq!(
            point_at(5, 7, &lines, 40),
            Some(Point::at_boundary(at(0, 1), 40))
        );
        assert_eq!(point_at(5, 0, &[], 40), None);
    }
}
