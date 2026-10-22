//! How far one mouse-wheel report scrolls. A port of the TypeScript
//! `WheelSteps` (adapted there from the `WheelScrollAccelerator` of pi, MIT,
//! Mario Zechner).
//!
//! Terminals report the wheel in one of two ways. A local macOS terminal
//! sends one report per line, already accelerated by the system, so each
//! report moves one row. Elsewhere, and over SSH, where the client is
//! unknown, a terminal sends one report per notch: a single notch moves one
//! row, and a fast spin moves more rows per report, up to six.

use std::time::Duration;

/// One report per line, or one per notch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WheelReports {
    Lines,
    #[default]
    Notches,
}

/// Reports closer together than this are one notch the terminal split, or a
/// high-resolution device; each moves one row and does not accelerate.
const BURST: Duration = Duration::from_millis(5);
/// A longer pause, or a change of direction, ends a spin.
const SPIN: Duration = Duration::from_millis(200);
/// The average gap, in milliseconds, at which a report moves one row.
const REFERENCE_MS: f64 = 100.0;
/// The most rows one accelerated report moves.
const MAX_ROWS: f64 = 6.0;
/// How many times as far a report moves while Alt is held.
pub const ALT_FACTOR: usize = 5;

/// How this terminal reports the wheel: by line on a local macOS terminal,
/// by notch otherwise. Only the SSH variables are read from `env`.
pub fn reports(env: impl Fn(&str) -> Option<String>, macos: bool) -> WheelReports {
    let remote = ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        .iter()
        .any(|name| env(name).is_some());
    if macos && !remote {
        WheelReports::Lines
    } else {
        WheelReports::Notches
    }
}

/// Rows per wheel report, following the speed of a spin. Notches 100 ms
/// apart move one row each, 50 ms apart two, and 20 ms apart five.
/// Fractions carry into the next report, so a steady spin moves an even
/// distance.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WheelSteps {
    pub reports: WheelReports,
    last: Option<Duration>,
    direction: i8,
    gap: Option<f64>,
    carry: f64,
}

impl WheelSteps {
    pub fn new(reports: WheelReports) -> Self {
        Self {
            reports,
            ..Self::default()
        }
    }

    /// The rows a report toward older output (`-1`) or newer (`1`), made at
    /// `at` on the terminal owner's clock, moves; at least one.
    pub fn rows(&mut self, direction: i8, at: Duration) -> usize {
        if self.reports == WheelReports::Lines {
            return 1;
        }
        let gap = self.last.map(|last| at.saturating_sub(last));
        let spinning = direction == self.direction && gap.is_some_and(|gap| gap <= SPIN);
        self.last = Some(at);
        self.direction = direction;
        let Some(gap) = gap.filter(|_| spinning) else {
            self.gap = None;
            self.carry = 0.0;
            return 1;
        };
        if gap < BURST {
            return 1;
        }
        let gap = gap.as_secs_f64() * 1000.0;
        let average = self.gap.map_or(gap, |previous| (previous + gap) / 2.0);
        self.gap = Some(average);
        let rows = (REFERENCE_MS / average).clamp(1.0, MAX_ROWS) + self.carry;
        let whole = rows.floor();
        self.carry = rows - whole;
        whole as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_slow_notch_moves_one_row_and_a_fast_spin_more() {
        let mut steps = WheelSteps::new(WheelReports::Notches);
        assert_eq!(steps.rows(-1, ms(1_000)), 1);
        assert_eq!(steps.rows(-1, ms(1_100)), 1);
        assert_eq!(steps.rows(-1, ms(1_150)), 1, "averaging 75 ms moves 1.33");
        // A steady 20 ms spin reaches five rows a notch.
        let mut steps = WheelSteps::new(WheelReports::Notches);
        steps.rows(1, ms(0));
        let spun: Vec<usize> = (1..=6).map(|i| steps.rows(1, ms(20 * i))).collect();
        assert_eq!(spun, [5, 5, 5, 5, 5, 5]);
        // Faster still is capped at six.
        let mut steps = WheelSteps::new(WheelReports::Notches);
        steps.rows(1, ms(0));
        steps.rows(1, ms(8));
        assert_eq!(steps.rows(1, ms(16)), 6);
    }

    #[test]
    fn a_pause_a_reversal_or_a_split_notch_starts_over() {
        let mut steps = WheelSteps::new(WheelReports::Notches);
        steps.rows(1, ms(0));
        assert_eq!(steps.rows(1, ms(20)), 5);
        assert_eq!(steps.rows(-1, ms(40)), 1, "reversed");
        assert_eq!(steps.rows(-1, ms(400)), 1, "paused");
        assert_eq!(steps.rows(-1, ms(402)), 1, "a split notch");
    }

    #[test]
    fn fractions_carry_so_a_steady_spin_moves_evenly() {
        let mut steps = WheelSteps::new(WheelReports::Notches);
        steps.rows(1, ms(0));
        // 40 ms apart is 2.5 rows a notch: 2, 3, 2, 3.
        let spun: Vec<usize> = (1..=4).map(|i| steps.rows(1, ms(40 * i))).collect();
        assert_eq!(spun, [2, 3, 2, 3]);
    }

    #[test]
    fn a_local_macos_terminal_reports_lines() {
        let none = |_: &str| None;
        let ssh = |name: &str| (name == "SSH_TTY").then(|| "/dev/pts/1".to_owned());
        assert_eq!(reports(none, true), WheelReports::Lines);
        assert_eq!(reports(ssh, true), WheelReports::Notches);
        assert_eq!(reports(none, false), WheelReports::Notches);
        let mut lines = WheelSteps::new(WheelReports::Lines);
        lines.rows(1, ms(0));
        assert_eq!(lines.rows(1, ms(10)), 1);
    }
}
