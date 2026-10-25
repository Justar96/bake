//! Transcript selection: points in transcript coordinates, word and row
//! ranges, and the cells a selection covers on each drawn line.
//!
//! The preview reports the mouse, so the terminal's own selection is gone and
//! the view draws and copies one instead. A point names a line by the
//! transcript row and line that draw it, not by its place on screen, so a
//! selection stays on its text while the view scrolls or output arrives
//! below. Ports the TypeScript `selection.ts`.

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
pub fn slice_cells(text: &str, from: usize, to: usize) -> String {
    cells(text)
        .into_iter()
        .filter(|&(_, start, _)| start >= from && start < to)
        .map(|(grapheme, ..)| grapheme)
        .collect()
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
