//! An SVG image's checks, warnings, and preview.
//!
//! The model writes the image without seeing it, so the warnings say what
//! looking at it would: text outside the canvas or passing its edge,
//! labels that overlap, text too small to read where the terminal shows
//! images, shapes wholly outside the canvas, a transparent background, and
//! elements that do not draw. Each names up to [`MOST_NAMED`] of what it
//! found, by the text's own words, and what to change.
//!
//! Text is laid out with the fonts given, which are the terminal's too:
//! both read the system's. Without fonts no text is measured.

use std::sync::OnceLock;

use bake_ai::{ImageContent, UserContentBlock};
use bake_svg::{CELL_PIXELS, Fonts, Report, Svg, TextBox};
use serde_json::{Map, Value};

use super::{Checked, Preview, Sight, Size, VisualKind, count, given, quoted, units};
use crate::tools::image::{ProcessedImage, process_image};

/// Texts or pairs a warning names before it counts the rest.
const MOST_NAMED: usize = 3;
/// Pixels tall under which text does not read, where the terminal shows
/// images.
const MIN_TEXT_PIXELS: f32 = 9.0;
/// The share of the smaller of two texts that the other covers, past
/// which they overlap.
const OVERLAP: f32 = 0.2;
/// The share of an image that is transparent, past which it is said.
const CLEAR: f32 = 0.02;
/// Pixels a side of the raster the transparent share is counted on.
const CLEAR_PIXELS: u32 = 64;

static FONTS: OnceLock<Fonts> = OnceLock::new();

/// The system's fonts, read once. Blocking: it reads every font directory.
pub(super) fn system_fonts() -> Fonts {
    FONTS.get_or_init(Fonts::system).clone()
}

/// Whether `args` are an SVG image's with text in it, which needs fonts.
pub(super) fn has_text(args: &Value) -> bool {
    args.get("kind").and_then(Value::as_str) == Some("svg")
        && args
            .get("source")
            .and_then(Value::as_str)
            .is_some_and(|source| source.contains("<text"))
}

/// An SVG image: its source, which [`bake_svg`] parses as the terminal
/// will, and nothing else.
pub(super) fn check(
    map: &Map<String, Value>,
    title: String,
    large: bool,
    preview: bool,
    fonts: &Fonts,
) -> Result<Checked, String> {
    for field in ["columns", "y", "rows"] {
        if given(map, field) {
            return Err(format!(
                "{field} does not apply to an SVG image; give its SVG text in source"
            ));
        }
    }
    let source = match map.get("source") {
        None | Some(Value::Null) => {
            return Err(
                "source is required for an SVG image: an <svg> document with a viewBox".into(),
            );
        }
        Some(Value::String(source)) => source,
        Some(other) => return Err(format!("source must be a string, not {}", quoted(other))),
    };
    let parsed = bake_svg::parse(source, fonts)
        .map_err(|error| format!("source is not an SVG the terminal can draw: {error}"))?;
    Ok(Checked {
        kind: VisualKind::Svg,
        title,
        size: Size::Svg(parsed.outline()),
        large,
        preview,
        warnings: warnings(source, &parsed, large, fonts),
    })
}

/// What will not show, or not well, in order of how much it hides.
fn warnings(source: &str, svg: &Svg, large: bool, fonts: &Fonts) -> Vec<String> {
    let outline = svg.outline();
    let report = svg.report();
    let (width, height) = (outline.width, outline.height);
    let mut warnings = Vec::new();
    let has = |tag: &str| source.contains(tag);
    let unread = has("<text") && fonts.faces() == 0;
    if outline.nodes == 0 && !unread {
        warnings.push("The SVG has no shapes or text to draw.".to_owned());
    }
    if unread {
        warnings.push(
            "The image's text was not measured: no fonts were read here, and without fonts text is not drawn."
                .to_owned(),
        );
    }
    if report.hidden_shapes > 0 {
        warnings.push(format!(
            "{} of {} wholly outside the canvas (0 0 {} {}) and {} not show; check {} coordinates.",
            report.hidden_shapes,
            count(report.shapes, "shape is", "shapes are"),
            units(width),
            units(height),
            if report.hidden_shapes == 1 {
                "does"
            } else {
                "do"
            },
            if report.hidden_shapes == 1 {
                "its"
            } else {
                "their"
            },
        ));
    }
    warnings.extend(text_warnings(&report, width, height, large));
    if let Ok(raster) = {
        let (w, h) = bake_svg::fit(width, height, CLEAR_PIXELS, CLEAR_PIXELS, 1.0);
        svg.render(w, h)
    } && outline.nodes > 0
    {
        let clear = raster.clear_share();
        if clear > CLEAR {
            warnings.push(format!(
                "About {:.0}% of the image is transparent and shows the terminal's background, which may be dark or light; add a full-size background rect so its colours read as intended.",
                (clear * 100.0).clamp(1.0, 100.0)
            ));
        }
    }
    if has("<image") {
        warnings.push("Linked images are not loaded; they show as nothing.".to_owned());
    }
    if has("<script") || has("<animate") || has("<set ") {
        warnings
            .push("Scripts and animation do not run; the image is drawn as written.".to_owned());
    }
    if has("<foreignObject") {
        warnings.push("foreignObject content is not drawn.".to_owned());
    }
    warnings
}

/// A text as a warning names it.
fn named(text: &TextBox) -> String {
    if text.label.is_empty() {
        "a text".to_owned()
    } else {
        format!("\"{}\"", text.label)
    }
}

/// `items`, the first [`MOST_NAMED`] of them, then how many more.
fn listed(items: &[String], more: &str) -> String {
    let mut shown: Vec<String> = items.iter().take(MOST_NAMED).cloned().collect();
    if items.len() > MOST_NAMED {
        shown.push(format!("and {} more{more}", items.len() - MOST_NAMED));
    }
    shown.join(", ")
}

/// What is wrong with the texts as laid out on a canvas of `width` by
/// `height`: outside it, passing an edge, overlapping, or too small.
fn text_warnings(report: &Report, width: f32, height: f32, large: bool) -> Vec<String> {
    let mut warnings = Vec::new();
    // Half a hundredth of the canvas: rounding and a glyph's overshoot.
    let slack = width.max(height) * 0.005;
    let outside: Vec<String> = report
        .texts
        .iter()
        .filter(|text| text.outside(width, height))
        .map(named)
        .collect();
    if !outside.is_empty() {
        warnings.push(format!(
            "{} outside the canvas (0 0 {} {}) and {} not show: {}.",
            count(outside.len(), "text is", "texts are"),
            units(width),
            units(height),
            if outside.len() == 1 { "does" } else { "do" },
            listed(&outside, "")
        ));
    }
    let shown: Vec<&TextBox> = report
        .texts
        .iter()
        .filter(|text| !text.outside(width, height))
        .collect();
    let cut: Vec<String> = shown
        .iter()
        .filter_map(|text| {
            let past = text.past(width, height);
            let (edge, by) = ["left", "top", "right", "bottom"]
                .into_iter()
                .zip(past)
                .max_by(|a, b| a.1.total_cmp(&b.1))?;
            (by > slack).then(|| format!("{} (the {edge} edge, by {})", named(text), units(by)))
        })
        .collect();
    if !cut.is_empty() {
        warnings.push(format!(
            "{} the canvas's edge and {} cut: {}. Move or shorten {}, or set text-anchor.",
            count(cut.len(), "text passes", "texts pass"),
            if cut.len() == 1 { "is" } else { "are" },
            listed(&cut, ""),
            if cut.len() == 1 { "it" } else { "them" },
        ));
    }
    let mut pairs = Vec::new();
    for (i, first) in shown.iter().enumerate() {
        for second in &shown[i + 1..] {
            let overlap = first.overlap(second);
            // The same words twice in one place are a shadow or an outline.
            let doubled = first.label == second.label && overlap > 0.9;
            if overlap > OVERLAP && !doubled {
                pairs.push(format!("{} and {}", named(first), named(second)));
            }
        }
    }
    if !pairs.is_empty() {
        warnings.push(format!(
            "{}: {}. Move them apart.",
            if pairs.len() == 1 {
                "Two texts overlap"
            } else {
                "Texts overlap"
            },
            listed(&pairs, " pairs")
        ));
    }
    // Pixels a unit takes where the terminal shows images.
    let (columns, rows) = bake_svg::cells(large);
    let scale = (columns as f32 * CELL_PIXELS.0 as f32 / width)
        .min(rows as f32 * CELL_PIXELS.1 as f32 / height);
    let small: Vec<&&TextBox> = shown
        .iter()
        .filter(|text| text.size > 0.0 && text.size * scale < MIN_TEXT_PIXELS)
        .collect();
    if let Some(smallest) = small.iter().min_by(|a, b| a.size.total_cmp(&b.size))
        && scale.is_finite()
        && scale > 0.0
    {
        warnings.push(format!(
            "{} under {MIN_TEXT_PIXELS:.0} pixels tall even where the terminal shows images (the smallest, {}, has font-size {}, about {:.0} pixels); use a font-size of at least {}{}.",
            count(small.len(), "text is", "texts are"),
            named(smallest),
            units(smallest.size),
            (smallest.size * scale).floor(),
            units((MIN_TEXT_PIXELS / scale).ceil()),
            if large { "" } else { ", or size large" },
        ));
    }
    warnings
}

/// The image as terminals that show images draw it, for a model that
/// reads images.
pub(super) fn preview(source: &str, large: bool, fonts: &Fonts, sight: Sight) -> Preview {
    if !sight.images {
        return Preview::Text("No preview: the current model does not read images.".to_owned());
    }
    let failed = || Preview::Text("No preview: the image did not draw.".to_owned());
    let Ok(svg) = bake_svg::parse(source, fonts) else {
        return failed();
    };
    let outline = svg.outline();
    let (columns, rows) = bake_svg::cells(large);
    let (width, height) = bake_svg::fit(
        outline.width,
        outline.height,
        columns * CELL_PIXELS.0,
        rows * CELL_PIXELS.1,
        1.0,
    );
    let Ok(png) = svg.render_png(width, height) else {
        return failed();
    };
    // Two pixels a cell, one above the other.
    let (across, down) = bake_svg::fit(outline.width, outline.height, columns, rows * 2, 1.0);
    match process_image(&png, "image/png", true, sight.resize) {
        ProcessedImage::Ok {
            data,
            mime_type,
            hints,
        } => {
            let mut note = format!(
                "Preview attached: the image at {width}×{height} pixels, as terminals that show images draw it. Other terminals draw it in about {across}×{down} coloured blocks, where fine detail and small text do not show."
            );
            for hint in hints {
                note.push(' ');
                note.push_str(&hint);
            }
            Preview::Image(
                note,
                UserContentBlock::Image(ImageContent { data, mime_type }),
            )
        }
        ProcessedImage::Failed(why) => Preview::Text(format!("No preview: {why}")),
    }
}
