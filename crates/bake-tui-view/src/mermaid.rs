//! A ```` ```mermaid ```` fence in an answer drawn as a diagram, as Pi
//! v1.5's Mermaid Markdown transformer draws it
//! (`packages/coding-agent/src/modes/interactive/components/mermaid.ts`),
//! with [`bake_mermaid`] in place of `lovely-mermaid`.
//!
//! [`Mermaid`] is Pi's `markdown.mermaid` setting. A closed fence draws as
//! a diagram, or, when it cannot, as its source with one dim line saying
//! why. With [`Mermaid::Streaming`], a fence still being written draws the
//! longest run of its complete lines that renders, so a statement half
//! written never turns the diagram back into source; until some run
//! renders, it draws as source.
//!
//! The diagram's boxes take the code block's dim border, its edges the
//! accent, and its labels the text's own colour. The classic frame draws
//! the diagram's glyphs in ASCII, cell for cell, so its layout holds.
//!
//! A `visualize` call's diagram ([`crate::visual`]) is drawn by the same
//! path, through [`drawing`] and [`crate::markdown::mermaid_fence`], with
//! the card's gutter in place of the rail. It ignores [`Mermaid`]: the
//! model asked to show the diagram, so it always draws, and draws while
//! its source streams.

use bake_mermaid::{RenderError, Role, render_spans};
use ratatui_core::style::Style;
use ratatui_core::text::{Line, Span};

use crate::transcript::{self, Look, RAIL};

/// How answers draw ```` ```mermaid ```` fences: Pi's `markdown.mermaid`,
/// with its values and default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Mermaid {
    /// Every fence draws as code.
    Off,
    /// A fence draws as a diagram once it is closed. Pi waits for the
    /// whole message; Bake draws each fence when it closes.
    Final,
    /// As [`Mermaid::Final`], and a fence still being written also draws
    /// as far as it renders.
    #[default]
    Streaming,
}

impl Mermaid {
    /// The setting's value as Pi writes it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Final => "final",
            Self::Streaming => "streaming",
        }
    }

    /// Pi's `getMermaidRenderingMode`: `off` and `final` as written,
    /// anything else the default.
    pub fn parse(value: &str) -> Self {
        match value {
            "off" => Self::Off,
            "final" => Self::Final,
            _ => Self::Streaming,
        }
    }
}

/// Bytes of source a fence still being written may try, in all, while it
/// looks for the longest run of lines that renders. Real diagrams are a few
/// kilobytes at most, so the search reaches every run of them.
pub(crate) const SEARCH_BYTES: usize = 128 * 1024;

/// The same budget for a `visualize` diagram, whose source the tool holds
/// to [`bake_mermaid`]'s 16 KiB. It covers every run a diagram within the
/// renderer's limits can try: up to about 72 lines of a sequence block
/// still open, each a prefix of at most 16 KiB, so a diagram that rendered
/// never draws as source again while it streams.
pub(crate) const TOOL_SEARCH_BYTES: usize = 2 * 1024 * 1024;

/// What a fence draws.
pub(crate) enum Drawing {
    /// The diagram's lines, each opening with the rail.
    Diagram(Vec<Line<'static>>),
    /// The fence's source as code, with why it is not a diagram when that
    /// is final.
    Source(Option<String>),
}

/// Whether a fence's info string names Mermaid, as Pi's `isMermaid` reads
/// it: its first word, in any case.
pub(crate) fn is_mermaid(info: &str) -> bool {
    info.split_whitespace()
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case("mermaid"))
}

/// The fence holding `body` drawn in `width` cells; `closed` when its
/// closing fence has arrived.
pub(crate) fn draw(body: &[String], closed: bool, width: usize, look: Look) -> Drawing {
    match look.mermaid {
        Mermaid::Off => Drawing::Source(None),
        Mermaid::Final if !closed => Drawing::Source(None),
        _ => drawing(body, closed, width, RAIL, SEARCH_BYTES, look),
    }
}

/// `body` drawn in `width` cells, each line opening with `rail` cells:
/// when `closed`, the whole of it or why not; else the longest run of its
/// complete lines that renders, trying at most `budget` bytes of source.
pub(crate) fn drawing(
    body: &[String],
    closed: bool,
    width: usize,
    rail: usize,
    budget: usize,
    look: Look,
) -> Drawing {
    let room = width.saturating_sub(rail);
    if closed {
        return match render_spans(&body.join("\n"), room) {
            Ok(rows) => Drawing::Diagram(lines(rows, rail, look)),
            Err(error) => Drawing::Source(Some(reason(error, room))),
        };
    }
    longest(body, room, budget).map_or(Drawing::Source(None), |rows| {
        Drawing::Diagram(lines(rows, rail, look))
    })
}

/// The longest run of `body`'s complete lines that renders in `room`
/// cells. The last line is the one being written, so it is left out.
fn longest(body: &[String], room: usize, budget: usize) -> Option<Vec<Vec<bake_mermaid::Span>>> {
    let complete = &body[..body.len().saturating_sub(1)];
    let mut budget = budget;
    for end in (1..=complete.len()).rev() {
        // A run that ends in a blank or comment line draws as the shorter
        // run before it does.
        let last = complete[end - 1].trim();
        if last.is_empty() || last.starts_with("%%") {
            continue;
        }
        let source = complete[..end].join("\n");
        budget = budget.checked_sub(source.len())?;
        if let Ok(rows) = render_spans(&source, room) {
            return Some(rows);
        }
    }
    None
}

/// Why a closed fence draws as source.
fn reason(error: RenderError, room: usize) -> String {
    match error {
        RenderError::TooWide => format!("diagram too wide for {room} columns; showing source"),
        RenderError::Limit => "diagram too large to draw; showing source".to_owned(),
        RenderError::Unsupported => "Mermaid syntax not supported here; showing source".to_owned(),
    }
}

/// The diagram's rows as lines after `rail` cells, styled by role.
fn lines(rows: Vec<Vec<bake_mermaid::Span>>, rail: usize, look: Look) -> Vec<Line<'static>> {
    let node = transcript::dim();
    let edge = transcript::accent(look.tones);
    rows.into_iter()
        .map(|row| {
            let mut spans = vec![transcript::pad(rail)];
            for span in row {
                let style = match span.role {
                    Role::Node => node,
                    Role::Edge => edge,
                    Role::Text => Style::new(),
                };
                let text = if look.classic {
                    span.text.chars().map(ascii).collect()
                } else {
                    span.text
                };
                spans.push(Span::styled(text, style));
            }
            Line::from(spans)
        })
        .collect()
}

/// The ASCII cell the classic frame draws for one of the renderer's
/// glyphs. Each is one cell wide, as the glyph is, so the layout holds;
/// other characters, labels' own among them, are kept.
fn ascii(glyph: char) -> char {
    match glyph {
        '─' | '—' => '-',
        '│' => '|',
        '┄' => '.',
        '┆' => ':',
        // `╪`, a route crossing a lane without joining it, crosses as
        // ASCII lines do.
        '┌' | '┐' | '└' | '┘' | '╭' | '╮' | '╰' | '╯' | '├' | '┤' | '┬' | '┴' | '┼' | '╪' => {
            '+'
        }
        '◄' | '◀' | '◁' => '<',
        '▶' => '>',
        '▲' | '△' => '^',
        '◆' | '●' => '*',
        '◇' => 'o',
        '◎' => '@',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_values_read_as_pi_reads_them() {
        for mode in [Mermaid::Off, Mermaid::Final, Mermaid::Streaming] {
            assert_eq!(Mermaid::parse(mode.name()), mode);
        }
        assert_eq!(Mermaid::parse("on"), Mermaid::Streaming);
        assert_eq!(Mermaid::parse("FINAL"), Mermaid::Streaming);
        assert_eq!(Mermaid::default(), Mermaid::Streaming);
    }

    #[test]
    fn the_info_string_names_mermaid_by_its_first_word() {
        assert!(is_mermaid("mermaid"));
        assert!(is_mermaid("Mermaid title=x"));
        assert!(!is_mermaid("mermaidjs"));
        assert!(!is_mermaid(""));
    }

    #[test]
    fn every_glyph_the_renderer_draws_has_a_one_cell_ascii_form() {
        use unicode_width::UnicodeWidthChar;
        for glyph in "─—│┄┆┌┐└┘╭╮╰╯├┤┬┴┼╪◄◀◁▶▲△◆●◇◎".chars()
        {
            let cell = ascii(glyph);
            assert!(cell.is_ascii_graphic(), "{glyph}");
            assert_eq!(glyph.width(), Some(1), "{glyph}");
        }
    }
}
