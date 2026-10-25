//! The tips the live banner shows for a while after a fullscreen session
//! starts: a dim `tip` row under the access tags that turns to the next tip
//! every [`EVERY`] and goes after [`SHOWN`] of them, or as soon as the first
//! turn starts. The first is always the scroll keys, which the transcript no
//! longer keeps on screen; the rest start at a place the launch's seed
//! picks, so each launch shows different ones.
//!
//! Inline mode writes the banner into scrollback before any tip could
//! change, so the terminal owner starts tips in fullscreen only.

use std::time::Duration;

use crate::copy;
use crate::transcript::{Row, Transcript};

/// How long after startup the first tip appears, so the banner settles
/// first.
pub const DELAY: Duration = Duration::from_millis(1500);
/// How long each tip stays.
pub const EVERY: Duration = Duration::from_secs(8);
/// How many tips one launch shows.
pub const SHOWN: usize = 3;

/// A launch's tips: when they started and where the rotation begins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tips {
    started: Duration,
    seed: u64,
}

impl Tips {
    /// Tips that start at `now`, rotating from a place `seed` picks.
    pub fn new(now: Duration, seed: u64) -> Self {
        Self { started: now, seed }
    }

    /// The tip at `now`, or `None` before the first and after the last.
    pub fn at(self, now: Duration) -> Option<&'static str> {
        let step = self.step(now)?;
        if step >= SHOWN {
            return None;
        }
        let rest = copy::TIPS.len().saturating_sub(1);
        let index = match step {
            0 => 0,
            _ if rest == 0 => 0,
            _ => {
                let start = usize::try_from(self.seed % rest as u64).unwrap_or(0);
                1 + (start + step - 1) % rest
            }
        };
        copy::TIPS.get(index).copied()
    }

    /// Whether every tip has been shown by `now`.
    pub fn done(self, now: Duration) -> bool {
        self.step(now).is_some_and(|step| step >= SHOWN)
    }

    /// How long after `now` the tip next changes; `None` once done.
    pub fn next_change(self, now: Duration) -> Option<Duration> {
        let first = self.started.saturating_add(DELAY);
        if now < first {
            return Some(first - now);
        }
        let step = self.step(now)?;
        if step >= SHOWN {
            return None;
        }
        let next = first.saturating_add(EVERY.saturating_mul(u32::try_from(step + 1).ok()?));
        Some(next.saturating_sub(now))
    }

    /// Which tip `now` falls in, counting from 0; `None` before the first.
    fn step(self, now: Duration) -> Option<usize> {
        let since = now.checked_sub(self.started.saturating_add(DELAY))?;
        usize::try_from(since.as_millis() / EVERY.as_millis()).ok()
    }
}

/// Sets the banner's tip, when the transcript opens with one, to `tip`;
/// a change is measured again.
pub fn show(transcript: &mut Transcript, tip: Option<&str>) {
    let Some(Row::Banner(banner)) = transcript.rows.first_mut() else {
        return;
    };
    if banner.tip.as_deref() == tip {
        return;
    }
    banner.tip = tip.map(str::to_owned);
    transcript.touched(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::Banner;

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    #[test]
    fn tips_wait_rotate_and_go() {
        let tips = Tips::new(ms(100), 0);
        assert_eq!(tips.at(ms(100)), None);
        assert_eq!(tips.next_change(ms(100)), Some(DELAY));
        let first = ms(100) + DELAY;
        assert_eq!(tips.at(first), Some(copy::TIPS[0]));
        assert_eq!(tips.next_change(first), Some(EVERY));
        assert_eq!(tips.at(first + EVERY), Some(copy::TIPS[1]));
        assert_eq!(tips.at(first + EVERY * 2), Some(copy::TIPS[2]));
        assert_eq!(
            tips.next_change(first + EVERY * 2 + ms(1)),
            Some(EVERY - ms(1))
        );
        let end = first + EVERY * u32::try_from(SHOWN).unwrap();
        assert!(!tips.done(end - ms(1)));
        assert_eq!(tips.at(end), None);
        assert!(tips.done(end));
        assert_eq!(tips.next_change(end), None);
        // A clock behind the start shows nothing and does not panic.
        assert_eq!(tips.at(Duration::ZERO), None);
        assert!(
            Tips::new(Duration::MAX, 7)
                .next_change(Duration::ZERO)
                .is_some()
        );
    }

    #[test]
    fn the_scroll_keys_come_first_and_the_seed_picks_the_rest() {
        let first = DELAY;
        for seed in [0, 1, 5, u64::MAX] {
            let tips = Tips::new(Duration::ZERO, seed);
            assert_eq!(tips.at(first), Some(copy::TIPS[0]));
            let rest: Vec<_> = (1..SHOWN)
                .map(|step| {
                    tips.at(first + EVERY * u32::try_from(step).unwrap())
                        .unwrap()
                })
                .collect();
            assert!(
                rest.iter().all(|tip| *tip != copy::TIPS[0]),
                "{seed}: {rest:?}"
            );
            assert_ne!(rest[0], rest[1]);
        }
        let second = |seed| Tips::new(Duration::ZERO, seed).at(first + EVERY);
        assert_ne!(second(0), second(1));
    }

    #[test]
    fn tips_fit_one_row_of_an_80_column_banner() {
        use unicode_width::UnicodeWidthStr;
        // The banner's text starts after its rail and label column.
        let room = 80 - 1 - 2 - 2 - 13;
        for tip in copy::TIPS {
            assert!(tip.width() <= room, "{tip}");
        }
    }

    #[test]
    fn show_sets_only_an_opening_banners_tip() {
        let banner = Row::Banner(Banner {
            version: "0.4.2".into(),
            directory: "~".into(),
            session: "new".into(),
            access: Vec::new(),
            tip: None,
        });
        let mut transcript = Transcript::new(vec![banner, Row::Info("note".into())]);
        show(&mut transcript, Some("a tip"));
        let Row::Banner(shown) = &transcript.rows[0] else {
            unreachable!()
        };
        assert_eq!(shown.tip.as_deref(), Some("a tip"));
        show(&mut transcript, None);
        let Row::Banner(shown) = &transcript.rows[0] else {
            unreachable!()
        };
        assert_eq!(shown.tip, None);
        // Without a banner first, nothing changes.
        let mut plain = Transcript::new(vec![Row::Info("note".into())]);
        show(&mut plain, Some("a tip"));
        assert_eq!(plain.rows, [Row::Info("note".into())]);
    }
}
