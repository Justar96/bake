//! The composer box: horizontal geometry by width, the caret-following
//! window, and the box's edges.
//!
//! Geometry depends only on the terminal width, so a change in height never
//! rewraps the draft. The window keeps its position between frames and moves
//! only when the caret would leave it.

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::frame::Glyphs;

/// Narrowest terminal that draws the box's sides; narrower ones keep only its
/// top and bottom edges as plain rules.
pub const BOX_MIN_WIDTH: u16 = 12;
/// Draft rows the window shows on an ordinary terminal.
pub const MIN_WINDOW_ROWS: u16 = 5;
/// Draft rows the window shows at most, on a tall terminal.
pub const MAX_WINDOW_ROWS: u16 = 12;

/// Draft rows the window may show: five below 30 rows, growing with the
/// terminal's height to [`MAX_WINDOW_ROWS`].
pub fn window_rows(height: u16) -> u16 {
    (height / 5).clamp(MIN_WINDOW_ROWS, MAX_WINDOW_ROWS)
}

/// Where the prompt and draft sit inside the composer's rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    /// The box's sides and padding fit at this width.
    pub boxed: bool,
    /// Column of the prompt marker, when there is room for one.
    pub prompt_x: Option<u16>,
    /// Column where draft text starts.
    pub text_x: u16,
    /// Cells for each draft row, the caret's column included, as
    /// [`crate::editor::layout`] takes them.
    pub wrap_width: usize,
}

/// Places the draft for a terminal `width` cells wide. Boxed, the row reads
/// `│ ❯ text… │`: the caret's extra column is the padding cell before the
/// right side, so a full row of text never wraps for the caret.
pub fn geometry(width: u16) -> Geometry {
    if width >= BOX_MIN_WIDTH {
        return Geometry {
            boxed: true,
            prompt_x: Some(2),
            text_x: 4,
            wrap_width: usize::from(width - 5),
        };
    }
    let prompt = width > 3;
    let text_x = if prompt { 2 } else { 0 };
    Geometry {
        boxed: false,
        prompt_x: prompt.then_some(0),
        text_x,
        wrap_width: usize::from(width.saturating_sub(text_x)).max(1),
    }
}

/// The window's first draft row, kept between frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ComposerWindow {
    top: usize,
    /// The caret's row within the window on the last frame.
    slot: usize,
    /// Wrap width of the last frame; zero before the first.
    width: usize,
}

impl ComposerWindow {
    /// Returns the first visible row for a caret on `caret_row` of `total`
    /// rows, `visible` at a time. The first frame puts the caret on the
    /// window's bottom row; after that the window moves only when the caret
    /// would leave it. After a width change, which renumbers the rows, the
    /// caret keeps the window row it had, as far as the new layout allows.
    pub fn follow(
        &mut self,
        caret_row: usize,
        visible: usize,
        total: usize,
        width: usize,
    ) -> usize {
        let visible = visible.max(1);
        if self.width == 0 {
            self.top = (caret_row + 1).saturating_sub(visible);
        } else if width != self.width {
            self.top = caret_row.saturating_sub(self.slot.min(visible - 1));
        }
        self.width = width;
        if caret_row < self.top {
            self.top = caret_row;
        } else if caret_row >= self.top + visible {
            self.top = caret_row + 1 - visible;
        }
        // Never leave blank rows under the draft while rows above are hidden.
        self.top = self.top.min(total.saturating_sub(visible));
        self.slot = caret_row - self.top;
        self.top
    }
}

/// Which horizontal edge of the box to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
}

/// Draws one horizontal edge: corners when the box is `closed`, otherwise a
/// plain rule. A `label`, such as a hidden-row count, sits near the right end
/// with at least one rule cell on each side; one that does not fit is left
/// out whole.
pub fn edge(
    buf: &mut Buffer,
    area: Rect,
    glyphs: &Glyphs,
    edge: Edge,
    closed: bool,
    label: Option<&str>,
) {
    if area.is_empty() {
        return;
    }
    let style = Style::new().add_modifier(Modifier::DIM);
    let rule = glyphs.horizontal;
    let (left, right) = match (closed, edge) {
        (false, _) => (rule, rule),
        (true, Edge::Top) => (glyphs.top_left, glyphs.top_right),
        (true, Edge::Bottom) => (glyphs.bottom_left, glyphs.bottom_right),
    };
    let width = usize::from(area.width);
    let inner = width.saturating_sub(2);
    let mut spans = vec![Span::styled(left, style)];
    match label
        .map(|text| format!(" {text} "))
        .filter(|text| text.width() + 2 <= inner)
    {
        Some(text) => {
            spans.push(Span::styled(rule.repeat(inner - text.width() - 1), style));
            spans.push(Span::styled(text, style));
            spans.push(Span::styled(rule, style));
        }
        None => spans.push(Span::styled(rule.repeat(inner), style)),
    }
    if width > 1 {
        spans.push(Span::styled(right, style));
    }
    buf.set_line(area.x, area.y, &Line::from(spans), area.width);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::FrameStyle;

    fn drawn(width: u16, closed: bool, label: Option<&str>) -> String {
        let area = Rect::new(0, 0, width, 1);
        let mut buf = Buffer::empty(area);
        edge(
            &mut buf,
            area,
            FrameStyle::Round.glyphs(),
            Edge::Top,
            closed,
            label,
        );
        (0..width).map(|x| buf[(x, 0)].symbol()).collect()
    }

    #[test]
    fn geometry_depends_on_width_alone_and_fits_the_caret() {
        for width in 1..=200 {
            let g = geometry(width);
            assert_eq!(g.boxed, width >= BOX_MIN_WIDTH);
            // The caret's column, the last of `wrap_width`, stays on screen
            // and, boxed, inside the right side.
            let caret = usize::from(g.text_x) + g.wrap_width - 1;
            let last = usize::from(width) - if g.boxed { 2 } else { 1 };
            assert!(caret <= last, "{width}: {g:?}");
        }
        assert_eq!(geometry(80).wrap_width, 75);
    }

    #[test]
    fn window_rows_match_the_oracle_below_thirty_rows() {
        assert_eq!(window_rows(8), 5);
        assert_eq!(window_rows(29), 5);
        assert_eq!(window_rows(40), 8);
        assert_eq!(window_rows(200), MAX_WINDOW_ROWS);
    }

    #[test]
    fn window_moves_only_when_the_caret_leaves_it() {
        let mut window = ComposerWindow::default();
        assert_eq!(window.follow(9, 5, 10, 40), 5);
        // Up inside the window moves the caret, not the text.
        assert_eq!(window.follow(6, 5, 10, 40), 5);
        assert_eq!(window.follow(5, 5, 10, 40), 5);
        assert_eq!(window.follow(4, 5, 10, 40), 4);
        assert_eq!(window.follow(9, 5, 10, 40), 5);
        // A shrinking draft never leaves rows blank below it.
        assert_eq!(window.follow(2, 5, 6, 40), 1);
        assert_eq!(window.follow(0, 5, 3, 40), 0);
    }

    #[test]
    fn a_width_change_keeps_the_caret_on_its_window_row() {
        let mut window = ComposerWindow::default();
        window.follow(9, 5, 20, 40);
        assert_eq!(window.follow(7, 5, 20, 40), 5);
        // Rewrapped narrower, the caret is now on row 12 of 30; it stays two
        // rows down the window.
        assert_eq!(window.follow(12, 5, 30, 20), 10);
        // A shorter window clamps the slot.
        assert_eq!(window.follow(12, 1, 30, 30), 12);
    }

    #[test]
    fn edges_close_the_box_and_carry_labels_that_fit() {
        assert_eq!(drawn(12, true, None), "╭──────────╮");
        assert_eq!(drawn(12, false, None), "────────────");
        assert_eq!(drawn(20, true, Some("+2 above")), "╭─────── +2 above ─╮");
        // Too narrow for the label: the rule is drawn whole.
        assert_eq!(drawn(12, true, Some("+2 above")), "╭──────────╮");
        assert_eq!(drawn(1, true, Some("x")), "╭");
    }
}
