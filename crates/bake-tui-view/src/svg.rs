//! A `visualize` call's SVG image as cells, drawn by [`bake_svg`].
//!
//! Each cell holds two pixels, one above the other, in an upper half block
//! (`▀`) with the top pixel as its foreground and the bottom as its
//! background, so pixels are about square. Truecolor terminals get the
//! pixels' colours; [`Tones::Ansi`] terminals the nearest of the 256-colour
//! palette. A pixel less than half covered is the terminal's own
//! background. The classic frame draws one pixel per cell, as a coloured
//! space, so it stays ASCII; `NO_COLOR` draws a shade per cell by how far
//! it is from the image's corner colour, in `░▒▓█`, or ASCII under the
//! classic frame.
//!
//! A source still arriving ([`Spec::partial`]) draws the first of
//! [`bake_svg::repairs`] that parses: the image grows tag by tag as it is
//! written. Drawings are kept in a small cache keyed by what they are drawn
//! from, so a frame that redraws the transcript does not rasterize again.
//!
//! Text in the image needs fonts: [`set_fonts`] gives them, once the
//! terminal has read the system's in the background. Until then, and in
//! tests, text draws nothing. Where the terminal shows images, the terminal
//! crate covers the drawing's cells with the same image at full resolution
//! ([`Drawing::image`]); the cells are what scrollback, selection, and
//! other terminals keep.

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use ratatui_core::style::{Color, Style};
use ratatui_core::text::{Line, Span};

use crate::activity::Tones;
use crate::transcript::{Look, dim};
use crate::visual::Spec;

/// Most cells wide an image draws, and most rows: [`bake_svg::CELLS`].
pub const COLUMNS: usize = bake_svg::CELLS.0 as usize;
pub const ROWS: usize = bake_svg::CELLS.1 as usize;
/// The same for an image the call asks to be large
/// ([`Spec::large`]): [`bake_svg::LARGE_CELLS`].
pub const LARGE_COLUMNS: usize = bake_svg::LARGE_CELLS.0 as usize;
pub const LARGE_ROWS: usize = bake_svg::LARGE_CELLS.1 as usize;
/// Drawings the cache keeps.
const CACHED: usize = 24;
/// A pixel at least this covered (of 255) is drawn.
const COVERED: u8 = 128;

static FONTS: OnceLock<bake_svg::Fonts> = OnceLock::new();

/// Gives text its fonts, once; later calls are ignored.
pub fn set_fonts(fonts: bake_svg::Fonts) {
    let _ = FONTS.set(fonts);
}

fn fonts() -> bake_svg::Fonts {
    FONTS.get().cloned().unwrap_or_else(bake_svg::Fonts::none)
}

/// An image drawn at a size, for the terminal to show in place of its
/// cells.
#[derive(Clone, Debug)]
pub struct Image {
    /// Identifies the source and the size; equal keys draw the same image.
    pub key: u64,
    /// The cells the image covers.
    pub columns: u16,
    pub rows: u16,
    /// The parsed drawing, to render at the terminal's pixel size.
    pub svg: Arc<bake_svg::Svg>,
}

/// An SVG image as rows of cells.
#[derive(Clone, Debug, Default)]
pub struct Drawing {
    pub lines: Vec<Line<'static>>,
    /// The image the cells show, when one parsed.
    pub image: Option<Image>,
    /// Why the complete source does not draw.
    pub error: Option<String>,
}

thread_local! {
    static CACHE: RefCell<Vec<(u64, Rc<Drawing>)>> = const { RefCell::new(Vec::new()) };
}

/// The drawing of `spec`'s SVG source at most `width` cells wide.
pub fn draw(spec: &Spec, width: usize, look: Look) -> Rc<Drawing> {
    let fonts = fonts();
    let mut hasher = DefaultHasher::new();
    spec.source.hash(&mut hasher);
    spec.partial.hash(&mut hasher);
    spec.large.hash(&mut hasher);
    width.hash(&mut hasher);
    look.classic.hash(&mut hasher);
    (look.tones as u8).hash(&mut hasher);
    fonts.faces().hash(&mut hasher);
    let key = hasher.finish();
    if let Some(found) = CACHE.with(|cache| {
        cache
            .borrow()
            .iter()
            .find(|(cached, _)| *cached == key)
            .map(|(_, drawing)| Rc::clone(drawing))
    }) {
        return found;
    }
    let drawing = Rc::new(fresh(spec, width, look, &fonts, key));
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() == CACHED {
            cache.remove(0);
        }
        cache.push((key, Rc::clone(&drawing)));
    });
    drawing
}

fn fresh(spec: &Spec, width: usize, look: Look, fonts: &bake_svg::Fonts, key: u64) -> Drawing {
    if spec.source.trim().is_empty() {
        return Drawing::default();
    }
    let candidates = if spec.partial {
        bake_svg::repairs(&spec.source)
    } else {
        vec![spec.source.clone()]
    };
    let mut error = None;
    let svg = candidates
        .iter()
        .find_map(|source| match bake_svg::parse(source, fonts) {
            Ok(svg) => Some(svg),
            Err(failed) => {
                error.get_or_insert(failed);
                None
            }
        });
    let Some(svg) = svg else {
        return Drawing {
            lines: Vec::new(),
            image: None,
            error: (!spec.partial)
                .then(|| error.map(|error| error.to_string()))
                .flatten(),
        };
    };
    let outline = svg.outline();
    let (most_columns, most_rows) = bake_svg::cells(spec.large);
    let columns = (width.max(1) as u32).min(most_columns);
    let half_blocks = !look.classic && look.tones != Tones::None;
    // Pixels per cell, down then across.
    let (down, aspect) = if half_blocks { (2, 1.0) } else { (1, 2.0) };
    let (w, h) = bake_svg::fit(
        outline.width,
        outline.height,
        columns,
        most_rows * down,
        aspect,
    );
    let Ok(raster) = svg.render(w, h) else {
        return Drawing {
            error: Some(bake_svg::Error::Failed.to_string()),
            ..Drawing::default()
        };
    };
    let lines = if half_blocks {
        halves(&raster, look.tones)
    } else if look.tones == Tones::None {
        shades(&raster, look.classic)
    } else {
        spaces(&raster, look.tones)
    };
    let rows = lines.len() as u16;
    Drawing {
        image: Some(Image {
            key,
            columns: w as u16,
            rows,
            svg: Arc::new(svg),
        }),
        lines,
        error: None,
    }
}

fn colour([r, g, b, _]: [u8; 4], tones: Tones) -> Color {
    match tones {
        Tones::TrueColor => Color::Rgb(r, g, b),
        _ => Color::Indexed(indexed(r, g, b)),
    }
}

/// The nearest colour of the 256-colour palette's cube and grey ramp.
pub fn indexed(r: u8, g: u8, b: u8) -> u8 {
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let step = |value: u8| {
        STEPS
            .iter()
            .enumerate()
            .min_by_key(|(_, step)| value.abs_diff(**step))
            .map_or(0, |(index, _)| index as u8)
    };
    let (ri, gi, bi) = (step(r), step(g), step(b));
    let cube = [STEPS[ri as usize], STEPS[gi as usize], STEPS[bi as usize]];
    let mean = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
    let grey_index = ((mean.saturating_sub(8)) / 10).min(23) as u8;
    let grey = 8 + 10 * grey_index;
    let distance = |[cr, cg, cb]: [u8; 3]| {
        let d = |a: u8, b: u8| u32::from(a.abs_diff(b)).pow(2);
        d(cr, r) + d(cg, g) + d(cb, b)
    };
    if distance([grey, grey, grey]) < distance(cube) {
        232 + grey_index
    } else {
        16 + 36 * ri + 6 * gi + bi
    }
}

/// Appends `text` in `style`, joining it to the last span when the style
/// is the same.
fn push(spans: &mut Vec<Span<'static>>, text: &str, style: Style) {
    if let Some(last) = spans.last_mut()
        && last.style == style
    {
        last.content.to_mut().push_str(text);
        return;
    }
    spans.push(Span::styled(text.to_owned(), style));
}

fn shown(pixel: [u8; 4]) -> bool {
    pixel[3] >= COVERED
}

/// Two pixels a cell.
fn halves(raster: &bake_svg::Raster, tones: Tones) -> Vec<Line<'static>> {
    (0..raster.height.div_ceil(2))
        .map(|row| {
            let mut spans = Vec::new();
            for x in 0..raster.width {
                let top = raster.pixel(x, row * 2);
                let bottom = raster.pixel(x, row * 2 + 1);
                let (glyph, style) = match (shown(top), shown(bottom)) {
                    (false, false) => (" ", Style::new()),
                    (true, false) => ("▀", Style::new().fg(colour(top, tones))),
                    (false, true) => ("▄", Style::new().fg(colour(bottom, tones))),
                    (true, true) => (
                        "▀",
                        Style::new()
                            .fg(colour(top, tones))
                            .bg(colour(bottom, tones)),
                    ),
                };
                push(&mut spans, glyph, style);
            }
            Line::from(spans)
        })
        .collect()
}

/// One pixel a cell, as a coloured space.
fn spaces(raster: &bake_svg::Raster, tones: Tones) -> Vec<Line<'static>> {
    (0..raster.height)
        .map(|y| {
            let mut spans = Vec::new();
            for x in 0..raster.width {
                let pixel = raster.pixel(x, y);
                let style = if shown(pixel) {
                    Style::new().bg(colour(pixel, tones))
                } else {
                    Style::new()
                };
                push(&mut spans, " ", style);
            }
            Line::from(spans)
        })
        .collect()
}

/// One pixel a cell, as a shade of how far it is from the corner's colour.
fn shades(raster: &bake_svg::Raster, classic: bool) -> Vec<Line<'static>> {
    let ramp: &[&str] = if classic {
        &[" ", ".", ":", "+", "#"]
    } else {
        &[" ", "░", "▒", "▓", "█"]
    };
    let corner = raster.pixel(0, 0);
    let ink = |pixel: [u8; 4]| -> f32 {
        let alpha = f32::from(pixel[3]) / 255.0;
        if !shown(corner) {
            return alpha;
        }
        let d = |i: usize| (f32::from(pixel[i]) - f32::from(corner[i])).abs() / 255.0;
        alpha * ((d(0) + d(1) + d(2)) / 3.0 * 2.0).min(1.0)
    };
    (0..raster.height)
        .map(|y| {
            let text: String = (0..raster.width)
                .map(|x| {
                    let level = (ink(raster.pixel(x, y)) * (ramp.len() - 1) as f32).round();
                    ramp[(level as usize).min(ramp.len() - 1)]
                })
                .collect();
            Line::from(Span::styled(text, dim()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOOK: Look = Look {
        tones: Tones::TrueColor,
        classic: false,
        lit: true,
        mermaid: crate::mermaid::Mermaid::Streaming,
    };

    fn spec(source: &str, partial: bool) -> Spec {
        Spec {
            kind: Some(crate::visual::Kind::Svg),
            source: source.into(),
            partial,
            ..Spec::default()
        }
    }

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    // A 4:2 image, red over blue, as two rows of half blocks: the red top
    // row of pixels is each cell's foreground, the blue its background.
    const FLAG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 2"><rect width="4" height="1" fill="#ff0000"/><rect y="1" width="4" height="1" fill="#0000ff"/></svg>"##;

    #[test]
    fn half_blocks_carry_two_pixels_a_cell() {
        let drawing = draw(&spec(FLAG, false), 8, LOOK);
        // 8 cells wide; a 2:1 image of square pixels is 4 pixel rows, 2 cells.
        assert_eq!(text(&drawing.lines), ["▀▀▀▀▀▀▀▀", "▀▀▀▀▀▀▀▀"]);
        assert_eq!(
            drawing.lines[0].spans[0].style,
            Style::new()
                .fg(Color::Rgb(255, 0, 0))
                .bg(Color::Rgb(255, 0, 0))
        );
        assert_eq!(
            drawing.lines[1].spans[0].style,
            Style::new()
                .fg(Color::Rgb(0, 0, 255))
                .bg(Color::Rgb(0, 0, 255))
        );
        let image = drawing.image.as_ref().expect("an image");
        assert_eq!((image.columns, image.rows), (8, 2));
        // Drawn again, the cache answers.
        assert!(Rc::ptr_eq(&drawing, &draw(&spec(FLAG, false), 8, LOOK)));
    }

    #[test]
    fn transparent_pixels_leave_the_terminal_s_background() {
        let source = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 2 2"><rect width="1" height="1" fill="#00ff00"/></svg>"##;
        let drawing = draw(&spec(source, false), 2, LOOK);
        assert_eq!(text(&drawing.lines), ["▀ "]);
        assert_eq!(
            drawing.lines[0].spans[0].style,
            Style::new().fg(Color::Rgb(0, 255, 0))
        );
        assert_eq!(drawing.lines[0].spans[1].style, Style::new());
    }

    #[test]
    fn other_looks_draw_without_half_blocks() {
        let classic = Look {
            classic: true,
            ..LOOK
        };
        let drawing = draw(&spec(FLAG, false), 4, classic);
        // One pixel a cell, cells twice as tall as wide: 4 by 1.
        assert_eq!(text(&drawing.lines), ["    "]);
        let none = Look {
            tones: Tones::None,
            ..LOOK
        };
        let lines = draw(&spec(FLAG, false), 8, none).lines.clone();
        assert_eq!(text(&lines), ["        ", "████████"]);
        let ansi = Look {
            tones: Tones::Ansi,
            ..LOOK
        };
        let lines = draw(&spec(FLAG, false), 8, ansi).lines.clone();
        assert_eq!(
            lines[0].spans[0].style.fg,
            Some(Color::Indexed(196)),
            "{lines:?}"
        );
    }

    #[test]
    fn a_streaming_source_draws_its_complete_tags() {
        let partial = &FLAG[..FLAG.find("<rect y").unwrap() + 10];
        let drawing = draw(&spec(partial, true), 8, LOOK);
        // The red rect is complete; the blue one is not yet drawn.
        assert_eq!(text(&drawing.lines), ["▀▀▀▀▀▀▀▀", "        "]);
        assert!(drawing.error.is_none());
        // Before the root's tag is complete, nothing.
        let early = draw(&spec("<svg view", true), 8, LOOK);
        assert!(early.lines.is_empty() && early.error.is_none());
        // A complete source that does not parse says why.
        let broken = draw(&spec("<svg viewBox=\"0 0 1 1\"><g></svg>", false), 8, LOOK);
        assert!(broken.lines.is_empty());
        assert!(broken.error.as_deref().unwrap().starts_with("line 1"));
    }

    // A call that asks for a large image gets more cells for it.
    #[test]
    fn a_large_image_takes_more_cells() {
        let square = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" fill="#00ff00"/></svg>"##;
        let usual = draw(&spec(square, false), 200, LOOK);
        // Two pixels a row: 40 pixels a side.
        assert_eq!(usual.lines.len(), ROWS);
        assert_eq!(text(&usual.lines)[0].chars().count(), ROWS * 2);
        let large = draw(
            &Spec {
                large: true,
                ..spec(square, false)
            },
            200,
            LOOK,
        );
        assert_eq!(large.lines.len(), LARGE_ROWS);
        assert_eq!(text(&large.lines)[0].chars().count(), LARGE_ROWS * 2);
        // A wide one is bound by its columns, and by the width it is given.
        let wide = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 10"><rect width="100" height="10" fill="#00ff00"/></svg>"##;
        let large_wide = |width| {
            let drawing = draw(
                &Spec {
                    large: true,
                    ..spec(wide, false)
                },
                width,
                LOOK,
            );
            text(&drawing.lines)[0].chars().count()
        };
        assert_eq!(large_wide(200), LARGE_COLUMNS);
        assert_eq!(large_wide(60), 60);
        assert_eq!(
            text(&draw(&spec(wide, false), 200, LOOK).lines)[0]
                .chars()
                .count(),
            COLUMNS
        );
    }

    #[test]
    fn the_palette_index_is_the_nearest_colour() {
        assert_eq!(indexed(255, 0, 0), 196);
        assert_eq!(indexed(0, 0, 0), 16);
        assert_eq!(indexed(255, 255, 255), 231);
        assert_eq!(indexed(128, 128, 128), 244);
    }
}
