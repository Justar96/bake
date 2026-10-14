//! The header's activity line: a word for the turn, its phase, and elapsed
//! time, drawn as text alone. A band of light sweeps across the word in place
//! of a spinner glyph, so the row says the session is working without a
//! symbol whose width a terminal could measure differently.
//!
//! Everything here is pure: time arrives as a [`Duration`], and the colour
//! level is resolved from the environment by the caller.

use std::time::Duration;

use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::text::Span;
use unicode_segmentation::UnicodeSegmentation;

/// Verbs for a turn in progress; one is chosen per turn. The TypeScript
/// list, extended. Each names active work: none says the turn is resting or
/// done, and "Laminating" stays with compaction.
pub const WORDS: &[&str] = &[
    "Baking",
    "Kneading",
    "Proofing",
    "Whisking",
    "Simmering",
    "Folding",
    "Glazing",
    "Rising",
    "Preheating",
    "Sifting",
    "Toasting",
    "Caramelizing",
    "Creaming",
    "Tempering",
    "Blooming",
    "Braiding",
    "Scoring",
    "Basting",
    "Zesting",
    "Dusting",
    "Piping",
    "Rolling",
    "Crimping",
    "Stirring",
    "Reducing",
    "Browning",
    "Infusing",
    "Marbling",
    "Frosting",
    "Drizzling",
    "Steaming",
    "Measuring",
];

/// How long the shimmer takes to move one grapheme.
pub const BEAT: Duration = Duration::from_millis(70);
/// Graphemes on each side of the brightest one that the band lightens.
const FALLOFF: usize = 2;
/// Beats the word rests unlit between sweeps.
const REST: usize = 12;

type Rgb = (u8, u8, u8);

/// What the activity line reports, which decides its colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hue {
    /// A turn: the TypeScript palette's `running` orange, brightening toward
    /// its progress `glint`.
    #[default]
    Running,
    /// History compaction: the palette's `compacting` blue, brightening
    /// toward a lighter blue.
    Compacting,
}

impl Hue {
    /// The resting colour and the glint the band brightens it toward.
    const fn blend(self) -> (Rgb, Rgb) {
        match self {
            Self::Running => ((0xf9, 0x73, 0x16), (0xff, 0xf7, 0xed)),
            Self::Compacting => ((0x3b, 0x82, 0xf6), (0xdb, 0xea, 0xfe)),
        }
    }

    /// The resting and middle ANSI colours; the brightest step is white.
    const fn ansi(self) -> (Color, Color) {
        match self {
            Self::Running => (Color::Yellow, Color::LightYellow),
            Self::Compacting => (Color::Blue, Color::LightBlue),
        }
    }
}

/// Picks the turn's word from `words`. The choice is deterministic in `seed`,
/// so one turn keeps one word; it hashes as `activityWord` does.
pub fn pick(words: &[&'static str], seed: &str) -> &'static str {
    let hash = seed.chars().fold(0u32, |hash, c| {
        hash.wrapping_mul(31).wrapping_add(u32::from(c))
    });
    words[hash as usize % words.len()]
}

/// Elapsed time as the header shows it: `8s`, `1m 05s`.
pub fn format_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

/// Brightness from 0 to 1 of each of `len` graphemes at `elapsed`. A band
/// enters from the left, crosses the word one grapheme per [`BEAT`], leaves
/// on the right, and the word rests unlit for `REST` beats.
pub fn shimmer(len: usize, elapsed: Duration) -> Vec<f32> {
    let period = len + 2 * FALLOFF + REST;
    let beat = (elapsed.as_millis() / BEAT.as_millis()) as usize % period;
    let center = beat as isize - FALLOFF as isize;
    (0..len)
        .map(|i| {
            let distance = (i as isize - center).unsigned_abs();
            1.0 - (distance as f32 / (FALLOFF + 1) as f32).min(1.0)
        })
        .collect()
}

/// Time from `elapsed` until the line next changes: the next beat while the
/// shimmer moves, otherwise the next whole second of elapsed time.
pub fn next_change(elapsed: Duration, motion: bool) -> Duration {
    let step = if motion { BEAT } else { Duration::from_secs(1) };
    let step_ms = step.as_millis();
    let into = elapsed.as_millis() % step_ms;
    Duration::from_millis(u64::try_from(step_ms - into).unwrap_or(1))
}

/// The colours a terminal can show the shimmer in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tones {
    /// 24-bit colour: a smooth blend from orange to glint.
    #[default]
    TrueColor,
    /// The 16 ANSI colours: yellow, light yellow, then bold bright white.
    Ansi,
    /// `NO_COLOR`: bold text in the terminal's foreground, and no motion.
    None,
}

impl Tones {
    /// Resolves the level from environment values that `env` returns.
    /// `NO_COLOR` wins; `COLORTERM` of `truecolor` or `24bit` selects
    /// [`Tones::TrueColor`].
    pub fn resolve(env: impl Fn(&str) -> Option<String>) -> Self {
        if env("NO_COLOR").is_some_and(|value| !value.is_empty()) {
            return Self::None;
        }
        match env("COLORTERM").as_deref() {
            Some("truecolor" | "24bit") => Self::TrueColor,
            _ => Self::Ansi,
        }
    }

    /// Whether the shimmer moves. It holds still without colour, because a
    /// moving weight change alone reads as flicker.
    pub fn moves(self) -> bool {
        self != Self::None
    }

    /// The style of a grapheme in `hue` at `brightness`, from 0 (the
    /// resting colour) to 1 (the glint).
    pub fn style(self, hue: Hue, brightness: f32) -> Style {
        match self {
            Self::None => Style::new().add_modifier(Modifier::BOLD),
            Self::TrueColor => {
                let (rest, glint) = hue.blend();
                let mix = |from: u8, to: u8| {
                    let value = f32::from(from) + (f32::from(to) - f32::from(from)) * brightness;
                    value.round().clamp(0.0, 255.0) as u8
                };
                Style::new().fg(Color::Rgb(
                    mix(rest.0, glint.0),
                    mix(rest.1, glint.1),
                    mix(rest.2, glint.2),
                ))
            }
            Self::Ansi if brightness > 0.6 => {
                Style::new().fg(Color::White).add_modifier(Modifier::BOLD)
            }
            Self::Ansi if brightness > 0.3 => Style::new().fg(hue.ansi().1),
            Self::Ansi => Style::new().fg(hue.ansi().0),
        }
    }
}

/// `text` as spans, each grapheme styled in `hue` by the shimmer at
/// `elapsed`. Without `motion` every grapheme takes the resting style.
pub fn shimmer_spans(
    text: &str,
    elapsed: Duration,
    tones: Tones,
    hue: Hue,
    motion: bool,
) -> Vec<Span<'static>> {
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    let light = if motion && tones.moves() {
        shimmer(graphemes.len(), elapsed)
    } else {
        vec![0.0; graphemes.len()]
    };
    graphemes
        .iter()
        .zip(light)
        .map(|(grapheme, brightness)| {
            Span::styled((*grapheme).to_owned(), tones.style(hue, brightness))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brightest(len: usize, elapsed: Duration) -> Option<usize> {
        let light = shimmer(len, elapsed);
        let max = light.iter().copied().fold(0.0f32, f32::max);
        (max >= 1.0).then(|| light.iter().position(|&b| b == max).unwrap())
    }

    #[test]
    fn elapsed_time_reads_like_the_typescript_header() {
        assert_eq!(format_elapsed(Duration::from_millis(8_999)), "8s");
        assert_eq!(format_elapsed(Duration::from_secs(65)), "1m 05s");
        assert_eq!(format_elapsed(Duration::from_secs(600)), "10m 00s");
    }

    #[test]
    fn the_word_is_stable_for_a_seed_and_varies_between_seeds() {
        assert_eq!(pick(WORDS, "sample-1"), pick(WORDS, "sample-1"));
        let distinct: std::collections::HashSet<_> = (0..8)
            .map(|i| pick(WORDS, &format!("sample-{i}")))
            .collect();
        assert!(distinct.len() > 4);
        // The hash matches the TypeScript one: 31 * 'a' + 'b' = 3105.
        assert_eq!(pick(&["x", "y", "z"], "ab"), ["x", "y", "z"][3105 % 3]);
    }

    #[test]
    fn the_band_crosses_left_to_right_then_rests() {
        let len = 9;
        let positions: Vec<Option<usize>> = (0..(len + 2 * FALLOFF + REST))
            .map(|beat| brightest(len, BEAT * beat as u32))
            .collect();
        let lit: Vec<usize> = positions.iter().flatten().copied().collect();
        assert_eq!(lit, (0..len).collect::<Vec<_>>());
        // Past the word, nothing is lit until the next sweep enters.
        let resting = shimmer(len, BEAT * (len + 2 * FALLOFF) as u32);
        assert!(resting.iter().all(|&b| b == 0.0));
        assert_eq!(
            brightest(len, BEAT * (len + 2 * FALLOFF + REST + FALLOFF) as u32),
            Some(0)
        );
        for beat in 0..40 {
            assert!(
                shimmer(len, BEAT * beat)
                    .iter()
                    .all(|b| (0.0..=1.0).contains(b))
            );
        }
    }

    #[test]
    fn redraws_wait_for_the_next_beat_or_second() {
        assert_eq!(
            next_change(Duration::from_millis(100), true),
            Duration::from_millis(40)
        );
        assert_eq!(
            next_change(Duration::from_millis(1_250), false),
            Duration::from_millis(750)
        );
        assert_eq!(next_change(Duration::ZERO, false), Duration::from_secs(1));
    }

    #[test]
    fn tones_follow_no_color_and_colorterm() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(
            Tones::resolve(env(&[("NO_COLOR", "1"), ("COLORTERM", "truecolor")])),
            Tones::None
        );
        assert_eq!(
            Tones::resolve(env(&[("NO_COLOR", ""), ("COLORTERM", "24bit")])),
            Tones::TrueColor
        );
        assert_eq!(
            Tones::resolve(env(&[("TERM", "xterm-256color")])),
            Tones::Ansi
        );
        assert!(!Tones::None.moves());
    }

    #[test]
    fn the_blend_runs_from_each_hue_to_its_glint() {
        let fg = |tones: Tones, hue, brightness| tones.style(hue, brightness).fg;
        assert_eq!(
            fg(Tones::TrueColor, Hue::Running, 0.0),
            Some(Color::Rgb(0xf9, 0x73, 0x16))
        );
        assert_eq!(
            fg(Tones::TrueColor, Hue::Running, 1.0),
            Some(Color::Rgb(0xff, 0xf7, 0xed))
        );
        assert_eq!(
            fg(Tones::TrueColor, Hue::Compacting, 0.0),
            Some(Color::Rgb(0x3b, 0x82, 0xf6))
        );
        assert_eq!(
            fg(Tones::TrueColor, Hue::Compacting, 1.0),
            Some(Color::Rgb(0xdb, 0xea, 0xfe))
        );
        assert_eq!(fg(Tones::Ansi, Hue::Running, 1.0), Some(Color::White));
        assert_eq!(fg(Tones::Ansi, Hue::Running, 0.5), Some(Color::LightYellow));
        assert_eq!(fg(Tones::Ansi, Hue::Running, 0.0), Some(Color::Yellow));
        assert_eq!(fg(Tones::Ansi, Hue::Compacting, 1.0), Some(Color::White));
        assert_eq!(
            fg(Tones::Ansi, Hue::Compacting, 0.5),
            Some(Color::LightBlue)
        );
        assert_eq!(fg(Tones::Ansi, Hue::Compacting, 0.0), Some(Color::Blue));
        assert_eq!(fg(Tones::None, Hue::Compacting, 1.0), None);
    }

    #[test]
    fn without_motion_every_grapheme_rests() {
        let rest = Tones::TrueColor.style(Hue::Running, 0.0);
        let spans = shimmer_spans("Kneading…", BEAT * 5, Tones::TrueColor, Hue::Running, false);
        assert_eq!(spans.len(), 9);
        assert!(spans.iter().all(|s| s.style == rest));
        let moving = shimmer_spans("Kneading…", BEAT * 5, Tones::TrueColor, Hue::Running, true);
        assert!(moving.iter().any(|s| s.style != rest));
    }
}
