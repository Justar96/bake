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

/// Rows an open tray keeps even when the transcript is down to its minimum,
/// so the list that holds the keyboard stays on screen.
pub const MIN_TRAY_ROWS: u16 = 3;

/// What the planner needs to know besides the height.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Needs {
    /// Visual rows the draft wraps to; at least one is drawn.
    pub draft_rows: u16,
    /// Whether the bar above the box has something to say: the tasks
    /// list's or one task's title and keys. Bake: with nothing to say, its
    /// row goes back to the transcript.
    pub bar: bool,
    /// Whether the status line is drawn under everything else.
    pub status: bool,
    /// Whether the tasks' standing row is present: while any subagent or
    /// background shell is reported, or the tray is open.
    pub standing: bool,
    /// Rows a notice wants; zero without one.
    pub notice: u16,
    /// Rows the pending-input panel wants; zero while nothing waits.
    pub pending: u16,
    /// Rows the attachments panel wants; zero without staged images.
    pub panel: u16,
    /// Rows the slash menu or a command's usage line wants; zero without.
    pub menu: u16,
    /// Rows the crew panel wants ([`crate::crew`]); zero while the session
    /// has spawned no agent.
    pub crew: u16,
    /// Rows the approval panel wants; zero while no request waits.
    pub approval: u16,
    /// Rows the tray under the controls wants: the tasks list, or one agent
    /// or shell, while it holds the keyboard.
    pub tray: u16,
}

/// Rows granted to each region, top to bottom as drawn: body, gap, the
/// pending-input panel, the attachments panel, notice, the slash menu,
/// the crew panel, the approval panel, the bar, the box's top edge, the draft, its bottom
/// edge, the standing row, the tray, and the status line, always last, so
/// it holds still while the tasks appear and the tray opens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rows {
    pub body: u16,
    pub gap: u16,
    pub pending: u16,
    pub panel: u16,
    pub notice: u16,
    pub menu: u16,
    pub crew: u16,
    pub approval: u16,
    pub bar: u16,
    pub top_edge: u16,
    pub composer: u16,
    pub bottom_edge: u16,
    pub standing: u16,
    pub tray: u16,
    pub status: u16,
}

impl Rows {
    pub fn total(&self) -> u16 {
        self.body
            + self.gap
            + self.pending
            + self.panel
            + self.notice
            + self.menu
            + self.crew
            + self.approval
            + self.bar
            + self.top_edge
            + self.composer
            + self.bottom_edge
            + self.standing
            + self.tray
            + self.status
    }
}

/// Grants rows in claim order: the draft's first row, the approval panel,
/// which a waiting request needs on screen before it can be answered, the
/// bar while it has something to say, the box's top edge and then its
/// bottom edge, the standing row, the tray, which may take what the body
/// keeps above [`MIN_BODY_ROWS`] once the status line has its row and
/// always gets up to [`MIN_TRAY_ROWS`], the status line, the gap, further
/// draft rows up to [`composer::window_rows`] while the body keeps
/// [`MIN_BODY_ROWS`], panels such as the notice, and the body last.
///
/// This follows the TypeScript collapse: the gap, the bottom edge, the top
/// edge, and then the bar give way before the input. Claiming the top edge
/// first means a short terminal loses the open edge, and the box closes
/// only when both edges have rows. Bake: the status line sits under the
/// tray and gives up its row before the draft, the standing row, and the
/// tray's minimum do.
pub fn plan(height: u16, needs: Needs) -> Rows {
    let mut left = height;
    let mut take = |want: u16| {
        let got = want.min(left);
        left -= got;
        got
    };
    let composer = take(1);
    let approval = take(needs.approval);
    let bar = take(u16::from(needs.bar));
    let top_edge = take(1);
    let bottom_edge = take(1);
    let standing = take(u16::from(needs.standing));
    let chrome = composer + approval + bar + top_edge + bottom_edge + standing;
    let tray = take(
        needs
            .tray
            .min(left_after_body(height, chrome + u16::from(needs.status)).max(MIN_TRAY_ROWS)),
    );
    let status = take(u16::from(needs.status));
    let gap = take(1);
    let extra = needs.draft_rows.clamp(1, composer::window_rows(height)) - 1;
    let more = take(extra.min(left_after_body(height, chrome + tray + status + gap)));
    let notice = take(needs.notice);
    let menu = take(needs.menu);
    // The crew panel gives way before the body's minimum, like the draft.
    let crew = take(needs.crew.min(left_after_body(
        height,
        chrome + tray + status + gap + more + notice + menu,
    )));
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
        crew,
        approval,
        bar,
        top_edge,
        composer: composer + more,
        bottom_edge,
        standing,
        tray,
        status,
    }
}

/// Rows left for further draft rows once the body keeps its minimum.
fn left_after_body(height: u16, claimed: u16) -> u16 {
    height.saturating_sub(claimed).saturating_sub(MIN_BODY_ROWS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The composer's scope: no bar, the status line, and a standing row.
    fn needs(draft_rows: u16) -> Needs {
        Needs {
            draft_rows,
            bar: false,
            status: true,
            standing: true,
            notice: 1,
            pending: 0,
            panel: 0,
            menu: 0,
            crew: 0,
            approval: 0,
            tray: 0,
        }
    }

    #[test]
    fn the_plan_fills_the_height_exactly_and_bounds_the_draft() {
        for height in 0..80 {
            for draft in 1..20 {
                for bar in [false, true] {
                    let rows = plan(
                        height,
                        Needs {
                            bar,
                            ..needs(draft)
                        },
                    );
                    assert_eq!(rows.total(), height, "{height} rows, {draft} draft rows");
                    assert!(rows.composer <= composer::window_rows(height).max(1));
                }
            }
        }
    }

    #[test]
    fn short_terminals_give_up_chrome_in_the_typescript_order() {
        // Each row of height adds the next claimant: composer, top edge,
        // bottom edge, standing row, status line, gap, then the notice.
        let shown = |height, bar| {
            let r = plan(height, Needs { bar, ..needs(1) });
            [
                r.composer,
                r.bar,
                r.top_edge,
                r.bottom_edge,
                r.standing,
                r.status,
                r.gap,
                r.notice,
                r.body,
            ]
        };
        assert_eq!(shown(1, false), [1, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(shown(2, false), [1, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(shown(3, false), [1, 0, 1, 1, 0, 0, 0, 0, 0]);
        assert_eq!(shown(4, false), [1, 0, 1, 1, 1, 0, 0, 0, 0]);
        assert_eq!(shown(5, false), [1, 0, 1, 1, 1, 1, 0, 0, 0]);
        assert_eq!(shown(6, false), [1, 0, 1, 1, 1, 1, 1, 0, 0]);
        assert_eq!(shown(7, false), [1, 0, 1, 1, 1, 1, 1, 1, 0]);
        assert_eq!(shown(8, false), [1, 0, 1, 1, 1, 1, 1, 1, 1]);
        // A bar with something to say claims right after the draft, as the
        // TypeScript header does.
        assert_eq!(shown(2, true), [1, 1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(shown(6, true), [1, 1, 1, 1, 1, 1, 0, 0, 0]);
    }

    // Bake: the status line is the last row whenever it is drawn, and on a
    // short terminal it gives up its row before the draft, the box, the
    // standing row, and the tray's minimum do.
    #[test]
    fn the_status_line_yields_before_the_draft_the_standing_row_and_the_tray_minimum() {
        for height in 0..60 {
            for tray in [0, 1, 2, 3, 4, 10, 40] {
                for bar in [false, true] {
                    for standing in [false, true] {
                        let rows = plan(
                            height,
                            Needs {
                                tray,
                                bar,
                                standing,
                                ..needs(3)
                            },
                        );
                        let case = format!("{height} rows, tray {tray}, bar {bar}, {standing}");
                        assert_eq!(rows.total(), height, "{case}");
                        assert!(rows.status <= 1, "{case}");
                        if rows.status > 0 {
                            assert_eq!(
                                (rows.composer.min(1), rows.top_edge, rows.bottom_edge),
                                (1, 1, 1),
                                "{case}"
                            );
                            assert_eq!(rows.standing, u16::from(standing), "{case}");
                            assert!(rows.tray >= tray.min(MIN_TRAY_ROWS), "{case}");
                        }
                        let before =
                            1 + u16::from(bar) + 2 + u16::from(standing) + tray.min(MIN_TRAY_ROWS);
                        assert_eq!(rows.status, u16::from(height > before), "{case}");
                    }
                }
            }
        }
        let rows = plan(
            5,
            Needs {
                status: false,
                ..needs(1)
            },
        );
        assert_eq!((rows.status, rows.gap), (0, 1));
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
    fn an_approval_panel_claims_its_rows_before_the_rest_of_the_chrome() {
        let rows = plan(
            6,
            Needs {
                approval: 3,
                ..needs(4)
            },
        );
        assert_eq!(
            (
                rows.composer,
                rows.approval,
                rows.top_edge,
                rows.bottom_edge
            ),
            (1, 3, 1, 1)
        );
        assert_eq!(rows.total(), 6);
        assert_eq!((rows.status, rows.body), (0, 0));
        let rows = plan(
            30,
            Needs {
                approval: 4,
                ..needs(1)
            },
        );
        assert_eq!(rows.approval, 4);
        assert!(rows.body >= MIN_BODY_ROWS);
    }

    #[test]
    fn the_crew_panel_gives_way_before_the_body() {
        let rows = plan(
            40,
            Needs {
                crew: 5,
                ..needs(1)
            },
        );
        assert_eq!(rows.crew, 5);
        assert_eq!(rows.total(), 40);
        for height in 0..20 {
            let rows = plan(
                height,
                Needs {
                    crew: 5,
                    ..needs(1)
                },
            );
            assert_eq!(rows.total(), height);
            assert!(
                rows.crew == 0 || rows.body >= MIN_BODY_ROWS,
                "{height}: {rows:?}"
            );
        }
    }

    #[test]
    fn without_a_standing_row_its_row_goes_to_the_others() {
        let rows = plan(
            6,
            Needs {
                draft_rows: 1,
                bar: false,
                status: true,
                standing: false,
                notice: 0,
                pending: 0,
                panel: 0,
                menu: 0,
                crew: 0,
                approval: 0,
                tray: 0,
            },
        );
        assert_eq!(
            (rows.standing, rows.status, rows.gap, rows.body),
            (0, 1, 1, 1)
        );
    }

    // Bake: an open tray sits under the controls and leaves the transcript
    // its rows above, so opening it blanks nothing; on a short terminal it
    // still keeps a few rows, as the list it shows holds the keyboard.
    #[test]
    fn a_tray_takes_rows_under_the_controls_and_keeps_the_transcript() {
        for height in 0..80 {
            for tray in 0..40 {
                // The tray's scopes name themselves on the bar.
                let rows = plan(
                    height,
                    Needs {
                        tray,
                        bar: true,
                        ..needs(1)
                    },
                );
                assert_eq!(rows.total(), height, "{height} rows, tray {tray}");
                assert!(rows.tray <= tray);
                // The draft's first row, the bar, both edges, and the
                // standing row claim before the tray; the status line, the
                // gap, and the notice after it.
                let (before, after) = (5, 3);
                if height >= before + MIN_TRAY_ROWS {
                    assert!(
                        rows.tray >= tray.min(MIN_TRAY_ROWS),
                        "{height} rows, tray {tray}"
                    );
                }
                if height >= before + tray + after + MIN_BODY_ROWS {
                    assert_eq!(rows.tray, tray);
                    assert!(rows.body >= MIN_BODY_ROWS, "{height} rows, tray {tray}");
                }
            }
        }
        let rows = plan(
            40,
            Needs {
                tray: 10,
                bar: true,
                ..needs(1)
            },
        );
        assert_eq!((rows.tray, rows.status, rows.body), (10, 1, 22));
    }
}
