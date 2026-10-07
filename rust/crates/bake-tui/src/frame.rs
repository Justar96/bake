//! Which glyphs draw the composer box, decided once from the environment.
//!
//! Ports `resolveFrame` from the TypeScript terminal: a terminal that is not
//! encoding UTF-8, names no capable `TERM`, or may draw East Asian Ambiguous
//! characters two cells wide gets the ASCII frame. A full-width run of
//! box-drawing characters drawn wider than measured wraps and breaks every
//! row below it, so the frame must match what the terminal can draw.

/// Glyph set for the composer's edges and prompt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameStyle {
    /// Rounded box-drawing corners and lines.
    #[default]
    Round,
    /// ASCII `+`, `-`, `|`, and `>`, for terminals that cannot draw the round frame.
    Classic,
}

/// The characters one [`FrameStyle`] draws. Each is one cell wide.
#[derive(Debug, PartialEq, Eq)]
pub struct Glyphs {
    pub top_left: &'static str,
    pub top_right: &'static str,
    pub bottom_left: &'static str,
    pub bottom_right: &'static str,
    pub horizontal: &'static str,
    pub vertical: &'static str,
    pub prompt: &'static str,
}

const ROUND: Glyphs = Glyphs {
    top_left: "╭",
    top_right: "╮",
    bottom_left: "╰",
    bottom_right: "╯",
    horizontal: "─",
    vertical: "│",
    prompt: "❯",
};

const CLASSIC: Glyphs = Glyphs {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    horizontal: "-",
    vertical: "|",
    prompt: ">",
};

/// Locale variables in the order POSIX resolves them for character handling.
const CTYPE_VARIABLES: [&str; 3] = ["LC_ALL", "LC_CTYPE", "LANG"];

impl FrameStyle {
    pub fn glyphs(self) -> &'static Glyphs {
        match self {
            Self::Round => &ROUND,
            Self::Classic => &CLASSIC,
        }
    }

    /// Resolves the style from this process's environment.
    pub fn from_env() -> Self {
        resolve(|name| std::env::var(name).ok(), cfg!(windows))
    }
}

/// Chooses the frame from environment values that `env` returns.
///
/// Windows Terminal sets neither a locale nor `TERM` but draws UTF-8, so its
/// `WT_SESSION` stands in for both. Other Windows consoles draw the frame
/// unless `TERM` is `dumb`; unlike the TypeScript resolver, this one does not
/// read the Windows system locale.
pub fn resolve(env: impl Fn(&str) -> Option<String>, windows: bool) -> FrameStyle {
    let set = |name: &str| env(name).filter(|value| !value.is_empty());
    let ctype = CTYPE_VARIABLES
        .iter()
        .find_map(|name| set(name))
        .map(|value| value.to_lowercase());
    if ctype.as_deref().is_some_and(ambiguous_wide) {
        return FrameStyle::Classic;
    }
    if set("WT_SESSION").is_none() {
        let term = set("TERM");
        let dumb = term.as_deref() == Some("dumb");
        // No locale variable means the C locale, which is not UTF-8.
        let utf8 = ctype
            .as_deref()
            .is_some_and(|value| value.contains("utf-8") || value.contains("utf8"));
        if dumb || (!windows && (!utf8 || term.is_none())) {
            return FrameStyle::Classic;
        }
    }
    FrameStyle::Round
}

/// Chinese, Japanese, and Korean terminals are commonly configured to draw
/// Ambiguous characters wide; the locale stands in for that preference.
fn ambiguous_wide(locale: &str) -> bool {
    ["zh", "ja", "ko"]
        .iter()
        .any(|language| locale.starts_with(language))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(pairs: &[(&str, &str)], windows: bool) -> FrameStyle {
        resolve(
            |name| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            },
            windows,
        )
    }

    #[test]
    fn a_utf8_locale_and_a_named_terminal_draw_the_round_frame() {
        let env = [("LANG", "en_US.UTF-8"), ("TERM", "xterm-256color")];
        assert_eq!(with(&env, false), FrameStyle::Round);
        assert_eq!(
            with(&[("LC_ALL", "C.utf8"), ("TERM", "xterm")], false),
            FrameStyle::Round
        );
    }

    #[test]
    fn missing_or_non_utf8_locales_and_dumb_terminals_draw_ascii() {
        assert_eq!(with(&[("TERM", "xterm")], false), FrameStyle::Classic);
        assert_eq!(
            with(&[("LANG", "en_US.ISO-8859-1"), ("TERM", "xterm")], false),
            FrameStyle::Classic
        );
        assert_eq!(with(&[("LANG", "en_US.UTF-8")], false), FrameStyle::Classic);
        assert_eq!(
            with(&[("LANG", "en_US.UTF-8"), ("TERM", "dumb")], false),
            FrameStyle::Classic
        );
        assert_eq!(with(&[("TERM", "dumb")], true), FrameStyle::Classic);
    }

    #[test]
    fn the_first_set_locale_variable_wins_and_cjk_locales_draw_ascii() {
        let env = [
            ("LC_ALL", ""),
            ("LC_CTYPE", "ja_JP.UTF-8"),
            ("LANG", "en_US.UTF-8"),
            ("TERM", "xterm"),
        ];
        assert_eq!(with(&env, false), FrameStyle::Classic);
        assert_eq!(
            with(&[("LANG", "zh_CN.UTF-8"), ("WT_SESSION", "1")], true),
            FrameStyle::Classic
        );
    }

    #[test]
    fn windows_terminal_and_windows_consoles_draw_the_round_frame() {
        assert_eq!(with(&[("WT_SESSION", "abc")], false), FrameStyle::Round);
        assert_eq!(with(&[], true), FrameStyle::Round);
    }

    #[test]
    fn every_glyph_is_one_cell() {
        use unicode_width::UnicodeWidthStr;
        for style in [FrameStyle::Round, FrameStyle::Classic] {
            let g = style.glyphs();
            for glyph in [
                g.top_left,
                g.top_right,
                g.bottom_left,
                g.bottom_right,
                g.horizontal,
                g.vertical,
                g.prompt,
            ] {
                assert_eq!(glyph.width(), 1, "{glyph:?}");
            }
        }
    }
}
