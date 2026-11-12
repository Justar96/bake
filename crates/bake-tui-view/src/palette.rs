//! The status line's palette: a few named tokens, each a truecolor value and
//! its nearest ANSI colour, so the row has one source of colour.
//!
//! Most of the row is [`Token::Muted`]. The warm scale means attention and
//! nothing else: [`Token::Ok`], [`Token::Notice`], [`Token::Warn`], and
//! [`Token::Danger`], green through red. [`Token::Accent`] is Bake's orange,
//! the running turn's shimmer, for work in progress. The cool scale is
//! information, the thinking levels from [`Token::High`] to [`Token::Max`],
//! so a high level never reads as a warning. The values are mid-tones that
//! read on dark and light backgrounds alike; Bake does not detect which.

use ratatui_core::style::{Color, Modifier, Style};

use crate::activity::{self, Rgb, Tones};

/// A semantic colour of the status line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Token {
    /// Labels and secondary values: the terminal's dim text, not a colour.
    Muted,
    /// Healthy: a context with room, a warm cache.
    Ok,
    /// Wants a look: waiting on the user, a filling context, a detached
    /// branch.
    Notice,
    /// Close to a limit.
    Warn,
    /// At a limit, or failed.
    Danger,
    /// Bake's orange: work in progress.
    Accent,
    /// The `high` thinking level, and a dialog's asking rows.
    High,
    /// The `xhigh` thinking level.
    XHigh,
    /// The `max` thinking level.
    Max,
}

impl Token {
    /// The truecolor value and nearest ANSI colour; `None` for
    /// [`Token::Muted`], which dims instead.
    pub const fn colour(self) -> Option<(Rgb, Color)> {
        Some(match self {
            Self::Muted => return None,
            Self::Ok => ((0x4a, 0xde, 0x80), Color::Green),
            Self::Notice => ((0xfa, 0xcc, 0x15), Color::Yellow),
            Self::Warn => ((0xfb, 0x92, 0x3c), Color::LightRed),
            Self::Danger => ((0xf8, 0x71, 0x71), Color::Red),
            Self::Accent => (activity::RUNNING, activity::RUNNING_ANSI),
            Self::High => ((0x38, 0xbd, 0xf8), Color::LightBlue),
            Self::XHigh => ((0xa7, 0x8b, 0xfa), Color::Magenta),
            Self::Max => ((0xf4, 0x72, 0xb6), Color::LightMagenta),
        })
    }

    /// The token as the terminal can show it: its colour on a truecolor
    /// terminal, the nearest ANSI colour otherwise, and no colour under
    /// `NO_COLOR`. [`Token::Muted`] dims at every level, since dimming is
    /// not a colour.
    pub fn style(self, tones: Tones) -> Style {
        let Some((rgb, ansi)) = self.colour() else {
            return Style::new().add_modifier(Modifier::DIM);
        };
        match tones {
            Tones::TrueColor => Style::new().fg(Color::Rgb(rgb.0, rgb.1, rgb.2)),
            Tones::Ansi => Style::new().fg(ansi),
            Tones::None => Style::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Token; 9] = [
        Token::Muted,
        Token::Ok,
        Token::Notice,
        Token::Warn,
        Token::Danger,
        Token::Accent,
        Token::High,
        Token::XHigh,
        Token::Max,
    ];

    #[test]
    fn every_colour_has_a_distinct_value_and_none_shows_under_no_color() {
        let values: std::collections::HashSet<_> =
            ALL.iter().filter_map(|t| t.colour()).map(|c| c.0).collect();
        assert_eq!(values.len(), ALL.len() - 1);
        for token in ALL {
            assert_eq!(token.style(Tones::None).fg, None, "{token:?}");
        }
        assert_eq!(
            Token::Accent.style(Tones::TrueColor),
            Tones::TrueColor.style(activity::Hue::Running, 0.0),
            "the accent is the shimmer's resting orange"
        );
        for tones in [Tones::TrueColor, Tones::Ansi, Tones::None] {
            let muted = Token::Muted.style(tones);
            assert_eq!((muted.fg, muted.add_modifier), (None, Modifier::DIM));
        }
        assert_eq!(
            Token::Danger.style(Tones::Ansi).fg,
            Some(Color::Red),
            "ANSI takes the nearest colour"
        );
    }
}
