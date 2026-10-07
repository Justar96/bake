//! The layout planner: one pure function from the terminal's height and the
//! presentation state to the rows each region gets.
//!
//! Regions claim rows in the design's order, and on a short terminal the last
//! claimant yields first. The draft's first row is always granted. Every
//! chrome row is one row at any width, so a width change never changes a row
//! count by itself.

use crate::composer;

/// Rows the transcript keeps before the draft may grow into them.
pub const MIN_BODY_ROWS: u16 = 3;

/// What the planner needs to know besides the height.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Needs {
    /// Visual rows the draft wraps to; at least one is drawn.
    pub draft_rows: u16,
    /// Whether a standing row (the sample agents) is present.
    pub standing: bool,
    /// Rows a notice wants; zero without one.
    pub notice: u16,
    /// Rows the pending-input panel wants; zero while nothing waits.
    pub pending: u16,
    /// Rows the attachments panel wants; zero without staged images.
    pub panel: u16,
    /// Rows the slash menu or a command's usage line wants; zero without.
    pub menu: u16,
}

/// Rows granted to each region, top to bottom as drawn: body, gap, the
/// pending-input panel, the attachments panel, notice, the slash menu,
/// the bar, which holds the activity and the status line, the box's top
/// edge, the draft, its bottom edge, and the standing row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rows {
    pub body: u16,
    pub gap: u16,
    pub pending: u16,
    pub panel: u16,
    pub notice: u16,
    pub menu: u16,
    pub bar: u16,
    pub top_edge: u16,
    pub composer: u16,
    pub bottom_edge: u16,
    pub standing: u16,
}

impl Rows {
    pub fn total(&self) -> u16 {
        self.body
            + self.gap
            + self.pending
            + self.panel
            + self.notice
            + self.menu
            + self.bar
            + self.top_edge
            + self.composer
            + self.bottom_edge
            + self.standing
    }
}

/// Grants rows in claim order: the draft's first row, the bar, the box's top
/// edge and then its bottom edge, the standing row, the gap,
/// further draft rows up to [`composer::window_rows`] while the body keeps
/// [`MIN_BODY_ROWS`], panels such as the notice, and the body last.
///
/// This follows the TypeScript collapse: the gap, the bottom edge, the top
/// edge, and then the bar give way before the input; the bar holds both the
/// TypeScript header and status line, so they go together. Claiming the
/// top edge first means a short terminal loses the open edge, and the box
/// closes only when both edges have rows.
pub fn plan(height: u16, needs: Needs) -> Rows {
    let mut left = height;
    let mut take = |want: u16| {
        let got = want.min(left);
        left -= got;
        got
    };
    let composer = take(1);
    let bar = take(1);
    let top_edge = take(1);
    let bottom_edge = take(1);
    let standing = take(u16::from(needs.standing));
    let gap = take(1);
    let extra = needs.draft_rows.clamp(1, composer::window_rows(height)) - 1;
    let more = take(extra.min(left_after_body(
        height,
        composer + bar + top_edge + bottom_edge + standing + gap,
    )));
    let notice = take(needs.notice);
    let menu = take(needs.menu);
    // Pending input claims before attachments, as the TypeScript panels do.
    let pending = take(needs.pending);
    let panel = take(needs.panel);
    let body = take(u16::MAX);
    Rows {
        body,
        gap,
        pending,
        panel,
        notice,
        menu,
        bar,
        top_edge,
        composer: composer + more,
        bottom_edge,
        standing,
    }
}

/// Rows left for further draft rows once the body keeps its minimum.
fn left_after_body(height: u16, claimed: u16) -> u16 {
    height.saturating_sub(claimed).saturating_sub(MIN_BODY_ROWS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn needs(draft_rows: u16) -> Needs {
        Needs {
            draft_rows,
            standing: true,
            notice: 1,
            pending: 0,
            panel: 0,
            menu: 0,
        }
    }

    #[test]
    fn the_plan_fills_the_height_exactly_and_bounds_the_draft() {
        for height in 0..80 {
            for draft in 1..20 {
                let rows = plan(height, needs(draft));
                assert_eq!(rows.total(), height, "{height} rows, {draft} draft rows");
                assert!(rows.composer <= composer::window_rows(height).max(1));
            }
        }
    }

    #[test]
    fn short_terminals_give_up_chrome_in_the_typescript_order() {
        // Each row of height adds the next claimant: composer, bar, top edge,
        // bottom edge, standing row, gap, then the notice.
        let shown = |height| {
            let r = plan(height, needs(1));
            [
                r.composer,
                r.bar,
                r.top_edge,
                r.bottom_edge,
                r.standing,
                r.gap,
                r.notice,
                r.body,
            ]
        };
        assert_eq!(shown(1), [1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(shown(2), [1, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(shown(3), [1, 1, 1, 0, 0, 0, 0, 0]);
        assert_eq!(shown(4), [1, 1, 1, 1, 0, 0, 0, 0]);
        assert_eq!(shown(6), [1, 1, 1, 1, 1, 1, 0, 0]);
        assert_eq!(shown(7), [1, 1, 1, 1, 1, 1, 1, 0]);
        assert_eq!(shown(8), [1, 1, 1, 1, 1, 1, 1, 1]);
    }

    #[test]
    fn the_draft_grows_before_the_notice_and_keeps_the_body_minimum() {
        let rows = plan(12, needs(10));
        // 6 chrome rows, then 3 draft rows leave the body its 3 rows; the
        // notice, claimed after the draft, may take one of them.
        assert_eq!((rows.composer, rows.notice, rows.body), (4, 1, 2));
        let rows = plan(24, needs(10));
        assert_eq!((rows.composer, rows.notice), (5, 1));
        assert!(rows.body >= MIN_BODY_ROWS);
    }

    #[test]
    fn without_a_standing_row_its_row_goes_to_the_others() {
        let rows = plan(
            6,
            Needs {
                draft_rows: 1,
                standing: false,
                notice: 0,
                pending: 0,
                panel: 0,
                menu: 0,
            },
        );
        assert_eq!((rows.standing, rows.gap, rows.body), (0, 1, 1));
    }
}
