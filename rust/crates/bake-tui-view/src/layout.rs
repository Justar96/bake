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
}

/// Rows granted to each region, top to bottom as drawn: body, gap, notice,
/// header, status, the box's top edge, the draft, its bottom edge, and the
/// standing row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rows {
    pub body: u16,
    pub gap: u16,
    pub notice: u16,
    pub header: u16,
    pub status: u16,
    pub top_edge: u16,
    pub composer: u16,
    pub bottom_edge: u16,
    pub standing: u16,
}

impl Rows {
    pub fn total(&self) -> u16 {
        self.body
            + self.gap
            + self.notice
            + self.header
            + self.status
            + self.top_edge
            + self.composer
            + self.bottom_edge
            + self.standing
    }
}

/// Grants rows in claim order: the draft's first row, the header, status,
/// the box's top edge and then its bottom edge, the standing row, the gap,
/// further draft rows up to [`composer::window_rows`] while the body keeps
/// [`MIN_BODY_ROWS`], panels such as the notice, and the body last.
///
/// This reproduces the TypeScript collapse: the gap, the bottom edge, the top
/// edge, status, and then the header give way before the input. Claiming the
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
    let header = take(1);
    let status = take(1);
    let top_edge = take(1);
    let bottom_edge = take(1);
    let standing = take(u16::from(needs.standing));
    let gap = take(1);
    let extra = needs.draft_rows.clamp(1, composer::window_rows(height)) - 1;
    let more = take(extra.min(left_after_body(
        height,
        composer + header + status + top_edge + bottom_edge + standing + gap,
    )));
    let notice = take(needs.notice);
    let body = take(u16::MAX);
    Rows {
        body,
        gap,
        notice,
        header,
        status,
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
        // Each row of height adds the next claimant: composer, header, status,
        // top edge, bottom edge, standing row, gap, then the notice.
        let shown = |height| {
            let r = plan(height, needs(1));
            [
                r.composer,
                r.header,
                r.status,
                r.top_edge,
                r.bottom_edge,
                r.standing,
                r.gap,
                r.notice,
                r.body,
            ]
        };
        assert_eq!(shown(1), [1, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(shown(2), [1, 1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(shown(3), [1, 1, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(shown(4), [1, 1, 1, 1, 0, 0, 0, 0, 0]);
        assert_eq!(shown(5), [1, 1, 1, 1, 1, 0, 0, 0, 0]);
        assert_eq!(shown(7), [1, 1, 1, 1, 1, 1, 1, 0, 0]);
        assert_eq!(shown(8), [1, 1, 1, 1, 1, 1, 1, 1, 0]);
        assert_eq!(shown(9), [1, 1, 1, 1, 1, 1, 1, 1, 1]);
    }

    #[test]
    fn the_draft_grows_before_the_notice_and_keeps_the_body_minimum() {
        let rows = plan(12, needs(10));
        // 7 chrome rows, then 2 draft rows leave the body its 3 rows; the
        // notice, claimed after the draft, may take one of them.
        assert_eq!((rows.composer, rows.notice, rows.body), (3, 1, 2));
        let rows = plan(24, needs(10));
        assert_eq!((rows.composer, rows.notice), (5, 1));
        assert!(rows.body >= MIN_BODY_ROWS);
    }

    #[test]
    fn without_a_standing_row_its_row_goes_to_the_others() {
        let rows = plan(
            7,
            Needs {
                draft_rows: 1,
                standing: false,
                notice: 0,
            },
        );
        assert_eq!((rows.standing, rows.gap, rows.body), (0, 1, 1));
    }
}
