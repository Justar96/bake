//! A `visualize` call as one transcript block: a chart, a table, a Mermaid
//! diagram, or an SVG image, drawn from the call's arguments at the width
//! it is given.
//!
//! The block reads as any call does: a state mark, the kind of visual as
//! its title (`Bar chart: Build time by crate`), and a status that counts
//! its rows and series, then the drawing hung from the card's gutter.
//!
//! The drawing is text:
//!
//! - **Bars** are horizontal, one row per category and series, in eighth
//!   blocks (`▏▎▍▌▋▊▉█`). Labels take a column padded to a multiple of
//!   [`LABEL_STEP`] cells, at most [`LABEL_COLUMN`], so most later labels
//!   do not move the bars; each bar ends with its value.
//! - **Stacked bars** are one row per category, its series end to end
//!   and its total after it ([`charts`]).
//! - **Lines** are a braille plot of [`PLOT_ROWS`] rows, or
//!   [`LARGE_PLOT_ROWS`] when the call asks for a large one, with a y axis
//!   at its bounds and middle, and the first, middle, and last x labels.
//! - **Areas** are lines filled down to 0, a smaller area in front of a
//!   larger.
//! - **Scatters**, **pies**, and **heatmaps** are drawn by [`charts`]:
//!   points on two value axes, shares as a disc of half blocks beside
//!   their legend, and a grid of cells coloured by value.
//! - **Tables** are drawn in box-drawing borders, numbers aligned right.
//! - **Diagrams** are drawn as an answer's ```` ```mermaid ```` fence is
//!   ([`crate::mermaid`]): the same role colours, ASCII under the classic
//!   frame, and, when the diagram is too wide or cannot be drawn, its source
//!   as code with a dim line saying why. A diagram whose source is still
//!   arriving draws the longest run of its complete lines that renders, so
//!   it never turns back into source. `markdown.mermaid` governs answers
//!   only: the model asked to show this diagram, so it always draws.
//!
//! - **SVG images** are drawn in half blocks, two pixels a cell
//!   ([`crate::svg`]), growing tag by tag while the source arrives.
//!
//! Series take their colours from [`palette::SERIES`]. Under `NO_COLOR`
//! they are told apart by glyph instead (`█▓▒░` bars, `•×+o` points), and
//! a chart of several series always has a legend. The classic frame draws
//! in ASCII: `#` and `=` bars, `*o+x` points, and `+-|` axes and borders.
//!
//! Nothing here emits an escape sequence or a control character, and every
//! row fits the width it is given, so a resize redraws the visual at the
//! new width. Bounds the arguments do not give are taken from the data,
//! rounded out to two significant digits, so they only widen as rows
//! arrive. While the model is still writing the arguments, the projector
//! leaves out the last, possibly cut, element of the array being written
//! and marks the visual [`Visual::streaming`]. The preview draws
//! [`PREVIEW_ROWS`] rows of the drawing and counts the rest;
//! [`Detail::Full`] draws up to [`EXPANDED_ROWS`].

use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::activity::Tones;
use crate::cards;
use crate::copy;
use crate::editor;
use crate::markdown;
use crate::mermaid;
use crate::palette;
use crate::shell_output;
use crate::transcript::{
    self, BODY, CallState, Detail, EXPANDED_ROWS, FAILED_LINES, Look, RAIL, dim, pad,
};

mod charts;

/// Rows of the drawing a preview shows before it counts the rest.
pub const PREVIEW_ROWS: usize = 24;
/// Rows a line chart's plot takes, whatever its data.
pub const PLOT_ROWS: usize = 10;
/// Rows a plot takes when the call asks for a large one.
pub const LARGE_PLOT_ROWS: usize = 20;
/// Most columns of values a heatmap draws: the columns after the first.
pub const HEAT_COLUMNS: usize = 24;
/// Most cells a label, a column name, or a table cell shows before `…`.
pub const LABEL_CELLS: usize = 40;
/// Most cells a bar chart's label column takes.
pub const LABEL_COLUMN: usize = 24;
/// The step a bar chart's label column widens by.
pub const LABEL_STEP: usize = 8;
/// Most series a chart draws: the columns after the first.
pub const MAX_SERIES: usize = 7;
/// Most cells a value's unit shows.
const UNIT_CELLS: usize = 12;
/// Characters of a label read before it is cleaned and cut.
const LABEL_INPUT: usize = 512;

/// What a visual draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bar,
    Stacked,
    Line,
    Area,
    Scatter,
    Pie,
    Heatmap,
    Table,
    Diagram,
    Svg,
}

/// A finite number. A value that is not finite reads as missing, so it
/// never reaches a drawing.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Number(f64);

impl Number {
    /// `value`, or `None` when it is not finite; `-0` reads as `0`.
    pub fn new(value: f64) -> Option<Self> {
        value.is_finite().then_some(Self(value + 0.0))
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

// Never NaN, so equality is reflexive.
impl Eq for Number {}

/// One cell of a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cell {
    Text(String),
    Number(Number),
    /// `null`, or a value the visual cannot read.
    Empty,
}

/// A visual's arguments as the view reads them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Spec {
    /// `None` until the kind has arrived, or when it is none of the kinds.
    pub kind: Option<Kind>,
    pub title: String,
    /// The first labels each row (its category or x); the rest name the
    /// series.
    pub columns: Vec<String>,
    /// The value axis's bounds, when given.
    pub min: Option<Number>,
    pub max: Option<Number>,
    /// The values' unit, such as `s`, drawn after each value.
    pub unit: String,
    /// One row per category or x, in column order.
    pub rows: Vec<Vec<Cell>>,
    /// A diagram's Mermaid source, or an image's SVG.
    pub source: String,
    /// The call asks for a large drawing: a taller plot, or an image in
    /// more cells.
    pub large: bool,
    /// The arguments may go on: they are still arriving or were cut off,
    /// so a diagram draws the longest run of its source's complete lines
    /// that renders, its last line left out as the one being written.
    pub partial: bool,
}

/// A `visualize` call and how it stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Visual {
    pub spec: Spec,
    pub state: CallState,
    /// The model is still writing the arguments.
    pub streaming: bool,
    /// The arguments are too large to redraw on every update while they
    /// arrive; the visual is drawn once they are complete.
    pub too_large: bool,
    /// A status after the counts, such as `interrupted`.
    pub summary: Option<String>,
    /// The tool's error, drawn in place of the visual.
    pub error: Vec<String>,
}

impl Visual {
    /// A visual of `spec` that is still running.
    pub fn running(spec: Spec) -> Self {
        Self {
            spec,
            state: CallState::Running,
            streaming: false,
            too_large: false,
            summary: None,
            error: Vec::new(),
        }
    }
}

/// The card's title for a kind.
fn title_of(kind: Option<Kind>) -> &'static str {
    match kind {
        Some(Kind::Bar) => copy::VISUAL_BAR,
        Some(Kind::Stacked) => copy::VISUAL_STACKED,
        Some(Kind::Line) => copy::VISUAL_LINE,
        Some(Kind::Area) => copy::VISUAL_AREA,
        Some(Kind::Scatter) => copy::VISUAL_SCATTER,
        Some(Kind::Pie) => copy::VISUAL_PIE,
        Some(Kind::Heatmap) => copy::VISUAL_HEATMAP,
        Some(Kind::Table) => copy::VISUAL_TABLE,
        Some(Kind::Diagram) => copy::VISUAL_DIAGRAM,
        Some(Kind::Svg) => copy::VISUAL_SVG,
        None => copy::VISUALIZE,
    }
}

/// The block's rows: its head, then the drawing, the tool's error, or the
/// note that the arguments are too large to draw yet, under the gutter.
pub(crate) fn present(
    visual: &Visual,
    width: usize,
    look: Look,
    detail: Detail,
) -> Vec<Line<'static>> {
    let failed = visual.state == CallState::Failed;
    let quiet = if failed { transcript::red(look) } else { dim() };
    let glyph = transcript::gutter(look);
    let gutter = || vec![pad(RAIL), Span::styled(format!("{glyph} "), quiet)];
    let room = width.saturating_sub(BODY).max(1);
    let mut lines = head(visual, width, look);
    let full = detail == Detail::Full;
    if failed && !visual.error.is_empty() {
        let limit = if full { EXPANDED_ROWS } else { FAILED_LINES };
        let mut rows = Vec::new();
        for line in &visual.error {
            rows.extend(transcript::hang(
                &clean(line),
                width,
                BODY,
                gutter(),
                gutter(),
                transcript::red(look),
            ));
        }
        transcript::clip_rows(&mut rows, limit);
        lines.extend(rows);
    } else if visual.too_large {
        lines.extend(transcript::note(
            gutter(),
            copy::TOO_LARGE_TO_PREVIEW,
            width,
        ));
    } else {
        let mut drawn = draw(&visual.spec, room, look);
        // A plot, a pie, and an image have a height of their own, which a
        // preview keeps whole: half of one says nothing.
        let whole = matches!(
            visual.spec.kind,
            Some(Kind::Line | Kind::Area | Kind::Scatter | Kind::Pie | Kind::Svg)
        );
        let limit = if full || whole {
            EXPANDED_ROWS
        } else {
            PREVIEW_ROWS
        };
        let mut more = None;
        if drawn.len() > limit + 1 {
            more = Some(drawn.len() - limit);
            drawn.truncate(limit);
        }
        for row in drawn {
            let mut spans = gutter();
            spans.extend(row.spans);
            lines.push(Line::from(spans));
        }
        if let Some(more) = more {
            let mut spans = gutter();
            spans.push(Span::styled(
                format!(
                    "{} {}",
                    look.marks().more,
                    cards::count(more, copy::MORE_ROW_NOUNS)
                ),
                dim(),
            ));
            lines.push(Line::from(spans));
        }
    }
    lines
        .into_iter()
        .map(|line| transcript::clip_line(line, width))
        .collect()
}

/// Where an SVG image's cells start among the lines [`present`] draws at
/// `width`: the line after the head, at column [`BODY`]; and the image.
/// `None` for any other visual, one still streaming, failed, or too large,
/// and one whose source does not draw.
pub(crate) fn image_spot(
    visual: &Visual,
    width: usize,
    look: Look,
) -> Option<(usize, crate::svg::Image)> {
    if visual.spec.kind != Some(Kind::Svg)
        || visual.streaming
        || visual.too_large
        || visual.state == CallState::Failed
    {
        return None;
    }
    let room = width.saturating_sub(BODY).max(1);
    let drawing = crate::svg::draw(&visual.spec, room, look);
    let image = drawing.image.clone()?;
    if drawing.error.is_some() || drawing.lines.is_empty() {
        return None;
    }
    Some((head(visual, width, look).len(), image))
}

/// `● Bar chart: title`, then the status: the counts, `streaming` while
/// the arguments arrive, and the call's own status, such as `interrupted`.
fn head(visual: &Visual, width: usize, look: Look) -> Vec<Line<'static>> {
    let spec = &visual.spec;
    let (mark, mark_style) = transcript::mark_of(visual.state, look);
    let title = clean(&spec.title);
    let mut label = title_of(spec.kind).to_owned();
    if !title.is_empty() {
        label.push(':');
    }
    let mut first = vec![
        pad(RAIL),
        Span::styled(mark, mark_style),
        pad(1),
        Span::styled(label.clone(), Style::new().add_modifier(Modifier::BOLD)),
    ];
    let indent = RAIL + mark.width() + 1 + label.width() + 1;
    let mut lines = if title.is_empty() {
        vec![Line::from(first)]
    } else {
        first.push(pad(1));
        transcript::hang(
            &title,
            width,
            indent,
            first,
            vec![pad(indent)],
            Style::new(),
        )
    };
    let failed = visual.state == CallState::Failed;
    let quiet = if failed { transcript::red(look) } else { dim() };
    let mut status: Vec<Span<'static>> = Vec::new();
    let mut push = |text: String, style: Style| {
        if !status.is_empty() {
            status.push(Span::styled(copy::STATUS_SEP, dim()));
        }
        status.push(Span::styled(text, style));
    };
    if let Some(counts) = counts(spec).filter(|_| !visual.too_large) {
        push(counts, dim());
    }
    if visual.streaming {
        push(copy::STREAMING.to_owned(), dim());
    }
    if let Some(summary) = &visual.summary {
        if !status.is_empty() {
            status.push(pad(2));
        }
        status.extend(transcript::status_spans(summary, quiet, look));
    }
    let under = vec![pad(RAIL + mark.width() + 1)];
    transcript::ride(&mut lines, status, under, width);
    lines
}

/// `12 rows · 2 series`, a table's `4 rows · 3 columns`, or the family a
/// diagram's first line names, such as `flowchart`; nothing until the kind
/// is known.
fn counts(spec: &Spec) -> Option<String> {
    let kind = spec.kind?;
    let rows = cards::count(spec.rows.len(), copy::ROW_NOUNS);
    let other = match kind {
        Kind::Table => cards::count(columns_of(spec), copy::COLUMN_NOUNS),
        Kind::Heatmap => cards::count(series_of(spec), copy::COLUMN_NOUNS),
        Kind::Pie => return Some(rows),
        Kind::Bar | Kind::Stacked | Kind::Line | Kind::Area | Kind::Scatter => {
            cards::count(series_of(spec), copy::SERIES_NOUNS)
        }
        Kind::Diagram => return family(&spec.source).map(str::to_owned),
        Kind::Svg => return None,
    };
    Some(format!("{rows}{}{other}", copy::STATUS_SEP))
}

/// The family a diagram's first statement names, as the renderer reads
/// it: blank lines and `%%` comments skipped, the statement ending at a
/// `;` or the line's end.
fn family(source: &str) -> Option<&'static str> {
    let line = source
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("%%"))?;
    let header = line.split(';').next().unwrap_or_default();
    match header.split_whitespace().next()? {
        "flowchart" | "graph" => Some(copy::DIAGRAM_FLOWCHART),
        "sequenceDiagram" => Some(copy::DIAGRAM_SEQUENCE),
        "stateDiagram-v2" | "stateDiagram" => Some(copy::DIAGRAM_STATE),
        "classDiagram" => Some(copy::DIAGRAM_CLASS),
        "erDiagram" => Some(copy::DIAGRAM_ENTITY),
        _ => None,
    }
}

/// The drawing of `spec` at `width`, each row at most `width` cells.
pub fn draw(spec: &Spec, width: usize, look: Look) -> Vec<Line<'static>> {
    let lines = match spec.kind {
        Some(Kind::Bar) => bar_chart(spec, width, look),
        Some(Kind::Stacked) => charts::stacked(spec, width, look),
        Some(Kind::Line) => line_chart(spec, width, look, false),
        Some(Kind::Area) => line_chart(spec, width, look, true),
        Some(Kind::Scatter) => charts::scatter(spec, width, look),
        Some(Kind::Pie) => charts::pie(spec, width, look),
        Some(Kind::Heatmap) => charts::heatmap(spec, width, look),
        Some(Kind::Table) => table(spec, width, look),
        Some(Kind::Diagram) => diagram(spec, width, look),
        Some(Kind::Svg) => svg_image(spec, width, look),
        None => Vec::new(),
    };
    lines
        .into_iter()
        .map(|line| transcript::clip_line(line, width))
        .collect()
}

/// `text` on one line, with no escape sequence or control character: a
/// line break or tab reads as a space.
pub fn clean(text: &str) -> String {
    let flat: String = text
        .chars()
        .take(LABEL_INPUT)
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            }
        })
        .collect();
    shell_output::parse_sgr(&flat, Tones::None).0
}

/// `text` cut to `cells`, its last cell `…` when cut.
fn fit(text: &str, cells: usize) -> String {
    if text.width() <= cells {
        return text.to_owned();
    }
    if cells == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let w = grapheme.width();
        if used + w + 1 > cells {
            break;
        }
        used += w;
        out.push_str(grapheme);
    }
    out.push('…');
    out
}

/// `text` cut to `cells` and padded with spaces to them.
fn padded(text: &str, cells: usize) -> String {
    let text = fit(text, cells);
    let w = text.width();
    format!("{text}{}", " ".repeat(cells.saturating_sub(w)))
}

/// Columns a spec's rows and names fill: at most eight, or a heatmap's
/// label column and [`HEAT_COLUMNS`].
fn columns_of(spec: &Spec) -> usize {
    let widest = spec.rows.iter().map(Vec::len).max().unwrap_or(0);
    let most = if spec.kind == Some(Kind::Heatmap) {
        HEAT_COLUMNS
    } else {
        MAX_SERIES
    };
    spec.columns.len().max(widest).min(most + 1)
}

/// Rows a plot of `spec` takes.
fn plot_rows(spec: &Spec) -> usize {
    if spec.large {
        LARGE_PLOT_ROWS
    } else {
        PLOT_ROWS
    }
}

/// Series a chart draws: the columns after the first.
fn series_of(spec: &Spec) -> usize {
    columns_of(spec).saturating_sub(1)
}

/// A series' name: its column's, or its number.
fn series_name(spec: &Spec, series: usize) -> String {
    spec.columns
        .get(series + 1)
        .map(|name| fit(&clean(name), LABEL_CELLS))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("{}", series + 1))
}

/// A cell as a label.
fn label_of(cell: Option<&Cell>) -> String {
    match cell {
        Some(Cell::Text(text)) => fit(&clean(text), LABEL_CELLS),
        Some(Cell::Number(number)) => exact(number.get()),
        Some(Cell::Empty) | None => String::new(),
    }
}

/// A cell as a value: a number, or missing.
fn value_of(cell: Option<&Cell>) -> Option<f64> {
    match cell {
        Some(Cell::Number(number)) => Some(number.get()),
        _ => None,
    }
}

/// The spec's values by row, one per series.
fn values(spec: &Spec, series: usize) -> Vec<Vec<Option<f64>>> {
    spec.rows
        .iter()
        .map(|row| (1..=series).map(|i| value_of(row.get(i))).collect())
        .collect()
}

/// The least and greatest of `values`, if any.
fn extent(values: &[Vec<Option<f64>>]) -> Option<(f64, f64)> {
    values
        .iter()
        .flatten()
        .flatten()
        .fold(None, |range, &v| match range {
            None => Some((v, v)),
            Some((lo, hi)) => Some((v.min(lo), v.max(hi))),
        })
}

/// `x` rounded to two significant digits, up or down: a bound that only
/// moves out as the data it holds grows.
fn round_out(x: f64, up: bool) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    let step = 10f64.powf(x.abs().log10().floor() - 1.0);
    if !(step.is_finite() && step > 0.0) {
        return x;
    }
    // Ten digits, so `0.3 / 0.01` reads as 30, not 29.999….
    let q = ((x / step) * 1e9).round() / 1e9;
    let rounded = if up { q.ceil() } else { q.floor() } * step;
    if rounded.is_finite() { rounded } else { x }
}

/// [`bounds_of`] with the bounds `spec` gives.
fn bounds(spec: &Spec, data: Option<(f64, f64)>, zero: bool) -> (f64, f64) {
    bounds_of(spec.min, spec.max, data, zero)
}

/// Bounds `lo < hi` from the given ones, else from `data` rounded out;
/// with `zero`, they hold 0, as a bar chart's do.
fn bounds_of(
    min: Option<Number>,
    max: Option<Number>,
    data: Option<(f64, f64)>,
    zero: bool,
) -> (f64, f64) {
    let (data_lo, data_hi) = data.unwrap_or((0.0, 0.0));
    let mut lo = round_out(data_lo, false);
    let mut hi = round_out(data_hi, true);
    if zero {
        lo = lo.min(0.0);
        hi = hi.max(0.0);
    }
    if let Some(min) = min {
        lo = min.get();
    }
    if let Some(max) = max {
        hi = max.get();
    }
    if data.is_none() && min.is_none() && max.is_none() {
        return (0.0, 1.0);
    }
    if hi > lo && (hi / 2.0 - lo / 2.0) > 0.0 {
        return (lo, hi);
    }
    // One value, or bounds given the wrong way round: a range around it.
    let centre = if max.is_some() && min.is_none() {
        hi
    } else {
        lo
    };
    let spread = if centre == 0.0 {
        1.0
    } else {
        centre.abs() / 10.0
    };
    let (lo, hi) = (centre - spread, centre + spread);
    if hi > lo && lo.is_finite() && hi.is_finite() {
        (lo, hi)
    } else {
        (0.0, 1.0)
    }
}

/// Where `v` falls between `lo` and `hi`, from 0 to 1, without
/// overflowing on huge ranges.
fn ratio(v: f64, lo: f64, hi: f64) -> f64 {
    let t = (v / 2.0 - lo / 2.0) / (hi / 2.0 - lo / 2.0);
    if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// A value as a chart labels it, in at most about six cells: `12.5`,
/// `1500`, `12.3k`, `4.1M`, `0.0012`, `1.2e15`.
pub fn short(value: f64) -> String {
    if value == 0.0 || !value.is_finite() {
        return "0".to_owned();
    }
    let sign = if value < 0.0 { "-" } else { "" };
    let a = value.abs();
    if !(1e-4..1e15).contains(&a) {
        return format!("{sign}{}", exponent(a));
    }
    let (scaled, suffix) = if a >= 1e12 {
        (a / 1e12, "T")
    } else if a >= 1e9 {
        (a / 1e9, "G")
    } else if a >= 1e6 {
        (a / 1e6, "M")
    } else if a >= 1e4 {
        (a / 1e3, "k")
    } else {
        (a, "")
    };
    let decimals = if scaled >= 100.0 {
        0
    } else if scaled >= 10.0 {
        1
    } else if scaled >= 1.0 {
        2
    } else {
        // Three significant digits.
        usize::try_from((-scaled.log10().floor()) as i64)
            .unwrap_or(0)
            .saturating_add(2)
            .min(6)
    };
    let text = trim_zeros(format!("{scaled:.decimals$}"));
    if text == "0" {
        return "0".to_owned();
    }
    format!("{sign}{text}{suffix}")
}

/// `1.2e15` for `a`, its mantissa to one decimal.
fn exponent(a: f64) -> String {
    let text = format!("{a:.1e}");
    match text.split_once('e') {
        Some((mantissa, power)) => format!("{}e{power}", trim_zeros(mantissa.to_owned())),
        None => text,
    }
}

/// `text` without the zeros, and point, a decimal ends with.
fn trim_zeros(text: String) -> String {
    if !text.contains('.') {
        return text;
    }
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// A value as a table shows it: whole numbers whole, others as given, and
/// very large or small ones with an exponent.
pub fn exact(value: f64) -> String {
    let a = value.abs();
    if a != 0.0 && !(1e-6..1e15).contains(&a) {
        return format!("{value:e}");
    }
    if value.fract() == 0.0 {
        return format!("{}", value as i64);
    }
    format!("{value}")
}

/// How the series of a chart are drawn.
struct Glyphs {
    /// Eighth blocks for every series, told apart by colour.
    eighths: bool,
    /// Each series' bar glyph, when bars are drawn in whole cells.
    bars: [&'static str; MAX_SERIES],
    /// Each series' point, when lines are drawn in markers.
    markers: [&'static str; MAX_SERIES],
    /// The lines' plot is braille, else markers.
    braille: bool,
    /// The glyph a series' line shows in the legend, in braille.
    line_key: &'static str,
    axis: &'static str,
    tick: &'static str,
    corner: &'static str,
    rule: &'static str,
    missing: &'static str,
}

const EIGHTHS: [&str; 7] = ["▏", "▎", "▍", "▌", "▋", "▊", "▉"];

fn glyphs(look: Look, series: usize) -> Glyphs {
    if look.classic {
        return Glyphs {
            eighths: false,
            bars: ["#", "=", "*", "+", "o", "x", "%"],
            markers: ["*", "o", "+", "x", "#", "@", "%"],
            braille: false,
            line_key: "*",
            axis: "|",
            tick: "+",
            corner: "+",
            rule: "-",
            missing: "-",
        };
    }
    let plain = look.tones == Tones::None;
    Glyphs {
        eighths: !plain,
        bars: ["█", "▓", "▒", "░", "▚", "▞", "▘"],
        markers: ["•", "×", "+", "o", "◆", "▲", "■"],
        braille: !(plain && series > 1),
        line_key: "━",
        axis: "│",
        tick: "┤",
        corner: "└",
        rule: "─",
        missing: copy::NO_VALUE,
    }
}

/// A series' colour, none under `NO_COLOR`.
fn series_style(series: usize, look: Look) -> Style {
    palette::SERIES[series % palette::SERIES.len()].style(look.tones)
}

/// The legend of a chart of several series: each series' key and name,
/// two cells apart, flowing onto more rows as `width` needs.
fn legend(
    spec: &Spec,
    series: usize,
    key: &dyn Fn(usize) -> &'static str,
    width: usize,
    look: Look,
) -> Vec<Line<'static>> {
    if series < 2 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for s in 0..series {
        let glyph = key(s);
        let name = fit(
            &series_name(spec, s),
            width.saturating_sub(glyph.width() + 1),
        );
        let cells = glyph.width() + 1 + name.width();
        if used > 0 && used + 2 + cells > width {
            lines.push(Line::from(std::mem::take(&mut spans)));
            used = 0;
        }
        if used > 0 {
            spans.push(pad(2));
            used += 2;
        }
        spans.push(Span::styled(glyph, series_style(s, look)));
        spans.push(Span::styled(format!(" {name}"), dim()));
        used += cells;
    }
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
    lines
}

/// Horizontal bars: a legend for several series, then each row's label
/// and its series' bars, each ending in its value.
fn bar_chart(spec: &Spec, width: usize, look: Look) -> Vec<Line<'static>> {
    let series = series_of(spec).max(1);
    let glyphs = glyphs(look, series);
    let unit = fit(&clean(&spec.unit), UNIT_CELLS);
    let labels: Vec<String> = spec.rows.iter().map(|row| label_of(row.first())).collect();
    let values = values(spec, series);
    let (lo, hi) = bounds(spec, extent(&values), true);
    let text = |v: Option<f64>| match v {
        Some(v) => format!("{}{unit}", short(v)),
        None => glyphs.missing.to_owned(),
    };
    let mut value_cells = values
        .iter()
        .flatten()
        .map(|&v| text(v).width())
        .max()
        .unwrap_or(0);
    let widest = labels.iter().map(|label| label.width()).max().unwrap_or(0);
    let mut label_cells = widest
        .div_ceil(LABEL_STEP)
        .saturating_mul(LABEL_STEP)
        .min(LABEL_COLUMN)
        .min(width / 3);
    let gap = usize::from(label_cells > 0);
    let mut area = width.saturating_sub(label_cells + gap + 1 + value_cells);
    if area < 4 {
        value_cells = 0;
        area = width.saturating_sub(label_cells + gap);
    }
    if area < 2 {
        label_cells = 0;
        area = width;
    }
    let gap = usize::from(label_cells > 0);
    let key = |s: usize| {
        if glyphs.eighths || (s == 0 && !look.classic) {
            "█"
        } else {
            glyphs.bars[s % MAX_SERIES]
        }
    };
    let mut lines = legend(spec, series, &key, width, look);
    let total = area.saturating_mul(8);
    let eighth = |v: f64| (ratio(v, lo, hi) * total as f64).round() as usize;
    // Bars start from the cell edge nearest 0.
    let base = (eighth(0f64.clamp(lo, hi)) + 4) / 8;
    for (row, label) in values.iter().zip(&labels) {
        for (s, value) in row.iter().enumerate() {
            let mut spans = Vec::new();
            if label_cells > 0 {
                let shown = if s == 0 {
                    padded(label, label_cells)
                } else {
                    " ".repeat(label_cells)
                };
                spans.push(Span::raw(shown));
                spans.push(pad(gap));
            }
            spans.extend(bar(*value, base, &eighth, s, &glyphs, look));
            if value_cells > 0 {
                spans.push(pad(1));
                spans.push(Span::styled(text(*value), dim()));
            }
            lines.push(Line::from(spans));
        }
    }
    lines
}

/// One series' bar for `value` from cell `base`, where 0 falls: rightward
/// for a value above it, in eighths unless the series draws whole cells,
/// and leftward, to the half cell, for one below.
fn bar(
    value: Option<f64>,
    base: usize,
    eighth: &dyn Fn(f64) -> usize,
    series: usize,
    glyphs: &Glyphs,
    look: Look,
) -> Vec<Span<'static>> {
    let Some(value) = value else {
        return vec![pad(base)];
    };
    let style = series_style(series, look);
    // The first series keeps eighths under `NO_COLOR`; the others are
    // told apart by their shade, drawn in whole cells.
    let whole = !(glyphs.eighths || (series == 0 && !look.classic));
    let full = if whole {
        glyphs.bars[series % MAX_SERIES]
    } else {
        "█"
    };
    let at = eighth(value);
    if at >= base * 8 {
        let length = at - base * 8;
        let (cells, rest) = if whole {
            ((length + 4) / 8, 0)
        } else {
            (length / 8, length % 8)
        };
        let mut drawn = full.repeat(cells);
        if rest > 0 {
            drawn.push_str(EIGHTHS[rest - 1]);
        }
        return vec![pad(base), Span::styled(drawn, style)];
    }
    let length = base * 8 - at;
    let (cells, part) = match length % 8 {
        _ if whole => ((length + 4) / 8, ""),
        rest if rest >= 6 => (length / 8 + 1, ""),
        rest if rest >= 3 => (length / 8, "▐"),
        rest if rest >= 1 => (length / 8, "▕"),
        _ => (length / 8, ""),
    };
    let start = base.saturating_sub(cells + usize::from(!part.is_empty()));
    let mut drawn = part.to_owned();
    drawn.push_str(&full.repeat(cells));
    vec![pad(start), Span::styled(drawn, style)]
}

/// The braille dot bits of a cell, by the dot's column and row in it.
const DOTS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

/// The points of a segment from `a` to `b`, both ends included.
fn segment(a: (i64, i64), b: (i64, i64)) -> Vec<(i64, i64)> {
    let (mut x, mut y) = a;
    let (dx, dy) = ((b.0 - x).abs(), -(b.1 - y).abs());
    let (sx, sy) = (if x < b.0 { 1 } else { -1 }, if y < b.1 { 1 } else { -1 });
    let mut error = dx + dy;
    let mut points = Vec::new();
    loop {
        points.push((x, y));
        if (x, y) == b {
            return points;
        }
        let twice = 2 * error;
        if twice >= dy {
            error += dy;
            x += sx;
        }
        if twice <= dx {
            error += dx;
            y += sy;
        }
    }
}

/// A plot's cells: each holds braille dots or a marker, and the series
/// drawn in it last.
struct Plot {
    cells: Vec<Vec<(u8, Option<&'static str>, usize)>>,
    braille: bool,
}

impl Plot {
    fn new(columns: usize, rows: usize, braille: bool) -> Self {
        Self {
            cells: vec![vec![(0, None, 0); columns]; rows],
            braille,
        }
    }

    /// The dots across and down: two by four a cell in braille, else one.
    fn dots(&self) -> (usize, usize) {
        let columns = self.cells.first().map_or(0, Vec::len);
        if self.braille {
            (columns * 2, self.cells.len() * 4)
        } else {
            (columns, self.cells.len())
        }
    }

    /// Draws the dot at `x`, `y` for `series`; outside the plot, nothing.
    fn set(&mut self, x: i64, y: i64, series: usize, glyphs: &Glyphs) {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return;
        };
        if self.braille {
            let cell = self.cells.get_mut(y / 4).and_then(|row| row.get_mut(x / 2));
            if let Some(cell) = cell {
                cell.0 |= DOTS[x % 2][y % 4];
                cell.2 = series;
            }
        } else if let Some(cell) = self.cells.get_mut(y).and_then(|row| row.get_mut(x)) {
            cell.1 = Some(glyphs.markers[series % MAX_SERIES]);
            cell.2 = series;
        }
    }

    /// The plot's rows behind a y axis labelled at its top, middle, and
    /// bottom, then the x axis under them.
    fn lines(
        &self,
        y_labels: &[String; 3],
        y_cells: usize,
        glyphs: &Glyphs,
        look: Look,
    ) -> Vec<Line<'static>> {
        let rows = self.cells.len();
        let columns = self.cells.first().map_or(0, Vec::len);
        let mid_row = rows / 2;
        let mut lines = Vec::with_capacity(rows + 1);
        for (r, row) in self.cells.iter().enumerate() {
            let (text, tick) = match r {
                0 => (&y_labels[0], true),
                r if r == mid_row => (&y_labels[1], true),
                r if r + 1 == rows => (&y_labels[2], true),
                _ => (&String::new(), false),
            };
            let mut spans = vec![
                Span::styled(format!("{:>y_cells$}", fit(text, y_cells)), dim()),
                pad(1),
                Span::styled(if tick { glyphs.tick } else { glyphs.axis }, dim()),
            ];
            let mut run = String::new();
            let mut run_style = Style::new();
            for &(bits, marker, s) in row {
                let (glyph, style) = match (bits, marker) {
                    (0, None) => (" ".to_owned(), Style::new()),
                    (bits, None) => (
                        char::from_u32(0x2800 + u32::from(bits))
                            .unwrap_or(' ')
                            .to_string(),
                        series_style(s, look),
                    ),
                    (_, Some(marker)) => (marker.to_owned(), series_style(s, look)),
                };
                if style != run_style && !run.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut run), run_style));
                }
                run_style = style;
                run.push_str(&glyph);
            }
            let kept = run.trim_end();
            if !kept.is_empty() {
                spans.push(Span::styled(kept.to_owned(), run_style));
            }
            lines.push(Line::from(spans));
        }
        lines.push(Line::from(vec![
            pad(y_cells + 1),
            Span::styled(
                format!("{}{}", glyphs.corner, glyphs.rule.repeat(columns)),
                dim(),
            ),
        ]));
        lines
    }
}

/// A value axis's labels at its top, middle, and bottom, and the cells
/// they take, at most a third of `width`.
fn y_axis(lo: f64, hi: f64, unit: &str, width: usize) -> ([String; 3], usize) {
    let mid = lo / 2.0 + hi / 2.0;
    let label = |v: f64| format!("{}{unit}", short(v));
    let labels = [label(hi), label(mid), label(lo)];
    let cells = labels
        .iter()
        .map(|text| text.width())
        .max()
        .unwrap_or(0)
        .min(width / 3);
    (labels, cells)
}

/// A line chart: a legend for several series, the plot with its y axis,
/// the x axis, and the x labels under it. With `fill`, an area chart: each
/// series is filled down to 0, which its bounds then hold, and where
/// areas cover each other the one nearer 0 shows in front.
fn line_chart(spec: &Spec, width: usize, look: Look, fill: bool) -> Vec<Line<'static>> {
    let series = series_of(spec).max(1);
    let glyphs = glyphs(look, series);
    let unit = fit(&clean(&spec.unit), UNIT_CELLS);
    let values = values(spec, series);
    let (lo, hi) = bounds(spec, extent(&values), fill);
    let (y_labels, y_cells) = y_axis(lo, hi, &unit, width);
    let columns = width.saturating_sub(y_cells + 2).max(1);
    let key = |s: usize| {
        if !glyphs.braille {
            glyphs.markers[s % MAX_SERIES]
        } else if fill {
            "█"
        } else {
            glyphs.line_key
        }
    };
    let mut lines = legend(spec, series, &key, width, look);
    let mut plot = Plot::new(columns, plot_rows(spec), glyphs.braille);
    let count = values.len();
    let (dots_x, dots_y) = plot.dots();
    let x_of = |i: usize| -> i64 {
        let x = if count <= 1 {
            dots_x / 2
        } else {
            i * (dots_x - 1) / (count - 1)
        };
        i64::try_from(x).unwrap_or(0)
    };
    let y_of = |v: f64| -> i64 {
        let y = ((1.0 - ratio(v, lo, hi)) * (dots_y - 1) as f64).round();
        y as i64
    };
    let base = y_of(0f64.clamp(lo, hi));
    // An area's dots by series and x: the one farthest from the base.
    let mut tops: Vec<Vec<Option<i64>>> = vec![vec![None; dots_x]; series];
    for (s, tops) in tops.iter_mut().enumerate() {
        let mut previous: Option<(i64, i64)> = None;
        for (i, row) in values.iter().enumerate() {
            let Some(v) = row.get(s).copied().flatten() else {
                previous = None;
                continue;
            };
            let point = (x_of(i), y_of(v));
            for (x, y) in segment(previous.unwrap_or(point), point) {
                if !fill {
                    plot.set(x, y, s, &glyphs);
                    continue;
                }
                let top = usize::try_from(x).ok().and_then(|x| tops.get_mut(x));
                if let Some(top) = top
                    && top.is_none_or(|kept| (y - base).abs() > (kept - base).abs())
                {
                    *top = Some(y);
                }
            }
            previous = Some(point);
        }
    }
    for x in 0..dots_x {
        // The farthest from the base first, so a smaller area is drawn
        // over a larger. A line chart kept no dots here.
        let mut column: Vec<(usize, i64)> = tops
            .iter()
            .enumerate()
            .filter_map(|(s, tops)| Some((s, tops.get(x).copied().flatten()?)))
            .collect();
        column.sort_by_key(|&(s, y)| (std::cmp::Reverse((y - base).abs()), s));
        for (s, top) in column {
            for y in top.min(base)..=top.max(base) {
                plot.set(i64::try_from(x).unwrap_or(0), y, s, &glyphs);
            }
        }
    }
    lines.extend(plot.lines(&y_labels, y_cells, &glyphs, look));
    lines.push(x_labels(spec, columns, y_cells + 2, x_of, glyphs.braille));
    lines
}

/// The x labels under a plot `plot` cells wide that starts `indent` cells
/// in: the first and last rows' labels at their points, and the middle
/// row's when it fits between them with room to spare.
fn x_labels(
    spec: &Spec,
    plot: usize,
    indent: usize,
    x_of: impl Fn(usize) -> i64,
    braille: bool,
) -> Line<'static> {
    let count = spec.rows.len();
    let room = (plot / 3).max(1);
    let cell = |i: usize| {
        let x = usize::try_from(x_of(i)).unwrap_or(0);
        if braille { x / 2 } else { x }
    };
    // Each label as (start, text), kept in order and apart.
    let mut placed: Vec<(usize, String)> = Vec::new();
    let mut place = |i: usize, align: i8| {
        let text = fit(&label_of(spec.rows[i].first()), room);
        if text.is_empty() {
            return;
        }
        let w = text.width();
        let at = cell(i);
        let start = match align {
            -1 => at,
            1 => (at + 1).saturating_sub(w),
            _ => at.saturating_sub(w / 2),
        }
        .min(plot.saturating_sub(w));
        if placed
            .iter()
            .any(|(s, t)| start < s + t.width() + 2 && *s < start + w + 2)
        {
            return;
        }
        placed.push((start, text));
    };
    match count {
        0 => {}
        1 => place(0, 0),
        _ => {
            place(0, -1);
            place(count - 1, 1);
            if count > 2 {
                place(count / 2, 0);
            }
        }
    }
    placed.sort_by_key(|(start, _)| *start);
    let mut spans = vec![pad(indent)];
    let mut at = 0;
    for (start, text) in placed {
        spans.push(pad(start.saturating_sub(at)));
        at = start.max(at) + text.width();
        spans.push(Span::styled(text, dim()));
    }
    Line::from(spans)
}

/// A table in box-drawing borders: the column names bold, numbers aligned
/// right, and columns narrowed, widest first, to fit `width`.
fn table(spec: &Spec, width: usize, look: Look) -> Vec<Line<'static>> {
    let columns = columns_of(spec);
    if columns == 0 {
        return Vec::new();
    }
    let (h, v, cross, top, bottom, left, right, top_left, top_right, bottom_left, bottom_right) =
        if look.classic {
            ("-", "|", "+", "+", "+", "+", "+", "+", "+", "+", "+")
        } else {
            ("─", "│", "┼", "┬", "┴", "├", "┤", "┌", "┐", "└", "┘")
        };
    let names: Vec<String> = (0..columns)
        .map(|c| {
            spec.columns
                .get(c)
                .map(|name| fit(&clean(name), LABEL_CELLS))
                .unwrap_or_default()
        })
        .collect();
    let body: Vec<Vec<(String, bool)>> = spec
        .rows
        .iter()
        .map(|row| {
            (0..columns)
                .map(|c| match row.get(c) {
                    Some(Cell::Number(number)) => (fit(&exact(number.get()), LABEL_CELLS), true),
                    other => (label_of(other), false),
                })
                .collect()
        })
        .collect();
    let mut widths: Vec<usize> = names.iter().map(|name| name.width().max(1)).collect();
    for row in &body {
        for (w, (text, _)) in widths.iter_mut().zip(row) {
            *w = (*w).max(text.width());
        }
    }
    let total = |widths: &[usize]| widths.iter().sum::<usize>() + 3 * widths.len() + 1;
    while total(&widths) > width {
        let Some((at, &widest)) = widths
            .iter()
            .enumerate()
            .max_by_key(|&(i, w)| (*w, std::cmp::Reverse(i)))
        else {
            break;
        };
        if widest <= 1 {
            break;
        }
        widths[at] -= 1;
    }
    let border = dim();
    let rule = |l: &str, m: &str, r: &str| {
        let parts: Vec<String> = widths.iter().map(|w| h.repeat(w + 2)).collect();
        Line::from(Span::styled(format!("{l}{}{r}", parts.join(m)), border))
    };
    let row_line = |cells: Vec<(String, bool)>, style: Style| {
        let mut spans = vec![Span::styled(v, border)];
        for (c, (text, number)) in cells.into_iter().enumerate() {
            let w = widths[c];
            let text = fit(&text, w);
            let room = w.saturating_sub(text.width());
            let (before, after) = if number { (room, 0) } else { (0, room) };
            spans.push(pad(1 + before));
            spans.push(Span::styled(text, style));
            spans.push(pad(after + 1));
            spans.push(Span::styled(v, border));
        }
        Line::from(spans)
    };
    let mut lines = vec![rule(top_left, top, top_right)];
    lines.push(row_line(
        names.into_iter().map(|name| (name, false)).collect(),
        Style::new().add_modifier(Modifier::BOLD),
    ));
    lines.push(rule(left, cross, right));
    for row in body {
        lines.push(row_line(row, Style::new()));
    }
    lines.push(rule(bottom_left, bottom, bottom_right));
    lines
}

/// A diagram, drawn as an answer's Mermaid fence is but with no rail: the
/// card's gutter takes its place. Nothing until the source has a line.
/// An SVG image in cells ([`crate::svg`]), or why its complete source does
/// not draw.
fn svg_image(spec: &Spec, width: usize, look: Look) -> Vec<Line<'static>> {
    let drawing = crate::svg::draw(spec, width, look);
    match &drawing.error {
        Some(error) => vec![Line::from(Span::styled(
            format!("{} {error}", copy::SVG_NOT_DRAWN),
            dim(),
        ))],
        None => drawing.lines.clone(),
    }
}

fn diagram(spec: &Spec, width: usize, look: Look) -> Vec<Line<'static>> {
    if spec.source.trim().is_empty() {
        return Vec::new();
    }
    let clean = editor::sanitize(&spec.source);
    let mut body: Vec<String> = clean.split('\n').map(transcript::expand_tabs).collect();
    // A whole source's trailing blank lines draw nothing, so its code
    // shows none; a partial one's last line is the one being written.
    while !spec.partial && body.last().is_some_and(|line| line.trim().is_empty()) {
        body.pop();
    }
    let drawing = mermaid::drawing(
        &body,
        !spec.partial,
        width,
        0,
        mermaid::TOOL_SEARCH_BYTES,
        look,
    );
    // The projector keeps a partial source to its last newline, so the line
    // being written is empty: the code leaves it out.
    let code = match body.split_last() {
        Some((last, before)) if spec.partial && last.is_empty() => before,
        _ => &body[..],
    };
    markdown::mermaid_fence(drawing, code, "mermaid", width, 0, look)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use ratatui_core::style::Color;

    use super::*;
    use crate::palette::Token;
    use crate::transcript::{Row, present as present_rows, present_detail};

    const COLOUR: Look = Look {
        tones: Tones::TrueColor,
        classic: false,
        lit: true,
        mermaid: crate::mermaid::Mermaid::Streaming,
    };
    const ANSI: Look = Look {
        tones: Tones::Ansi,
        classic: false,
        lit: true,
        mermaid: crate::mermaid::Mermaid::Streaming,
    };
    const PLAIN: Look = Look {
        tones: Tones::None,
        classic: false,
        lit: true,
        mermaid: crate::mermaid::Mermaid::Streaming,
    };
    const CLASSIC: Look = Look {
        tones: Tones::Ansi,
        classic: true,
        lit: true,
        mermaid: crate::mermaid::Mermaid::Streaming,
    };

    fn n(value: f64) -> Cell {
        Cell::Number(Number::new(value).expect("finite"))
    }

    fn t(text: &str) -> Cell {
        Cell::Text(text.into())
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

    fn drawn(spec: &Spec, width: usize, look: Look) -> Vec<String> {
        text(&draw(spec, width, look))
    }

    fn card(visual: Visual, width: usize, look: Look) -> Vec<String> {
        text(&present(&visual, width, look, Detail::Preview))
    }

    /// The foreground of the first span whose text holds `needle`.
    fn fg_of(lines: &[Line<'static>], needle: &str) -> Option<Color> {
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.contains(needle))
            .and_then(|span| span.style.fg)
    }

    fn rgb(token: Token) -> Color {
        let ((r, g, b), _) = token.colour().expect("a colour");
        Color::Rgb(r, g, b)
    }

    fn builds() -> Spec {
        Spec {
            kind: Some(Kind::Bar),
            columns: vec!["crate".into(), "debug".into(), "release".into()],
            unit: "s".into(),
            rows: vec![
                vec![t("bake-ai"), n(12.5), n(30.0)],
                vec![t("bake-tui"), n(8.0), Cell::Empty],
            ],
            ..Spec::default()
        }
    }

    fn trend() -> Spec {
        Spec {
            kind: Some(Kind::Line),
            columns: vec!["day".into(), "a".into(), "b".into()],
            rows: (0..5)
                .map(|i| {
                    vec![
                        n(i as f64 + 1.0),
                        n([3.0, 5.0, 4.0, 8.0, 6.0][i]),
                        n(i as f64),
                    ]
                })
                .collect(),
            ..Spec::default()
        }
    }

    fn files() -> Spec {
        Spec {
            kind: Some(Kind::Table),
            columns: vec!["file".into(), "lines".into(), "名前".into()],
            rows: vec![
                vec![t("a.rs"), n(12.0), t("x")],
                vec![t("long/path/b.rs"), n(1234.5), Cell::Empty],
            ],
            ..Spec::default()
        }
    }

    #[test]
    fn bars_end_in_their_values_and_series_differ_by_colour_or_glyph() {
        let colour = [
            "█ debug  █ release",
            "bake-ai  ██████████▍ 12.5s",
            "         █████████████████████████ 30s",
            "bake-tui ██████▋ 8s",
            "          –",
        ];
        assert_eq!(drawn(&builds(), 40, COLOUR), colour);
        assert_eq!(drawn(&builds(), 40, ANSI), colour);
        assert_eq!(
            drawn(&builds(), 24, COLOUR),
            [
                "█ debug  █ release",
                "bake-ai  ███▊ 12.5s",
                "         █████████ 30s",
                "bake-tui ██▍ 8s",
                "          –",
            ]
        );
        // Without colour the second series is shaded, in whole cells.
        assert_eq!(
            drawn(&builds(), 40, PLAIN),
            [
                "█ debug  ▓ release",
                "bake-ai  ██████████▍ 12.5s",
                "         ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓ 30s",
                "bake-tui ██████▋ 8s",
                "          –",
            ]
        );
        assert_eq!(
            drawn(&builds(), 24, CLASSIC),
            [
                "# debug  = release",
                "bake-ai  #### 12.5s",
                "         ========= 30s",
                "bake-tui ## 8s",
                "          -",
            ]
        );
        let lines = draw(&builds(), 40, COLOUR);
        assert_eq!(fg_of(&lines, "██████████▍"), Some(rgb(Token::Teal)));
        assert_eq!(
            fg_of(&lines, "█████████████████████████"),
            Some(rgb(Token::XHigh))
        );
        let ansi = draw(&builds(), 40, ANSI);
        assert_eq!(fg_of(&ansi, "██████████▍"), Some(Color::Cyan));
        assert_eq!(
            fg_of(&ansi, "█████████████████████████"),
            Some(Color::Magenta)
        );
        let plain = draw(&builds(), 40, PLAIN);
        assert!(
            plain
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.style.fg.is_none())
        );
    }

    #[test]
    fn lines_plot_in_braille_or_in_markers_without_colour_or_unicode() {
        assert_eq!(
            drawn(&trend(), 30, COLOUR),
            [
                "━ a  ━ b",
                "8 ┤                  ⢀⠎⠒⢄",
                "  │                 ⡠⠃   ⠉⠢⢄",
                "  │                ⡰⠁       ⠑⠢",
                "  │      ⢀⡀      ⢀⠜",
                "  │   ⢀⠤⠊⠁⠈⠑⠒⠤⢄⡀⢠⠊",
                "4 ┤⢀⡠⠒⠁        ⠈⠁      ⢀⣀⡠⠤⠒⠒⠉",
                "  │⠁              ⣀⡠⠤⠒⠉⠁",
                "  │          ⣀⡠⠔⠒⠉",
                "  │     ⣀⠤⠔⠊⠉",
                "0 ┤⣀⠤⠒⠊⠉",
                "  └───────────────────────────",
                "   1            3            5",
            ]
        );
        assert_eq!(
            drawn(&trend(), 30, PLAIN),
            [
                "• a  × b",
                "8 ┤                   ••",
                "  │                  •  ••••",
                "  │                ••       ••",
                "  │     •••       •",
                "  │   ••   ••••  •",
                "4 ┤ ••         ••         ××××",
                "  │•               ×××××××",
                "  │          ××××××",
                "  │   ×××××××",
                "0 ┤×××",
                "  └───────────────────────────",
                "   1            3            5",
            ]
        );
        assert_eq!(
            drawn(&trend(), 30, CLASSIC),
            [
                "* a  o b",
                "8 +                   **",
                "  |                  *  ****",
                "  |                **       **",
                "  |     ***       *",
                "  |   **   ****  *",
                "4 + **         **         oooo",
                "  |*               ooooooo",
                "  |          oooooo",
                "  |   ooooooo",
                "0 +ooo",
                "  +---------------------------",
                "   1            3            5",
            ]
        );
        // The plot's height is fixed, whatever the data and the width.
        for rows in [0, 1, 2, 50] {
            let mut spec = trend();
            spec.rows.truncate(rows);
            for width in [12, 30, 80] {
                assert_eq!(draw(&spec, width, COLOUR).len(), 1 + PLOT_ROWS + 2);
            }
        }
    }

    #[test]
    fn tables_align_numbers_right_and_narrow_their_widest_columns() {
        assert_eq!(
            drawn(&files(), 40, COLOUR),
            [
                "┌────────────────┬────────┬──────┐",
                "│ file           │ lines  │ 名前 │",
                "├────────────────┼────────┼──────┤",
                "│ a.rs           │     12 │ x    │",
                "│ long/path/b.rs │ 1234.5 │      │",
                "└────────────────┴────────┴──────┘",
            ]
        );
        assert_eq!(
            drawn(&files(), 20, COLOUR),
            [
                "┌─────┬─────┬──────┐",
                "│ fi… │ li… │ 名前 │",
                "├─────┼─────┼──────┤",
                "│ a.… │  12 │ x    │",
                "│ lo… │ 12… │      │",
                "└─────┴─────┴──────┘",
            ]
        );
        assert_eq!(
            drawn(&files(), 40, CLASSIC),
            [
                "+----------------+--------+------+",
                "| file           | lines  | 名前 |",
                "+----------------+--------+------+",
                "| a.rs           |     12 | x    |",
                "| long/path/b.rs | 1234.5 |      |",
                "+----------------+--------+------+",
            ]
        );
    }

    #[test]
    fn degenerate_data_still_draws() {
        let bar = |rows: Vec<Vec<Cell>>| Spec {
            kind: Some(Kind::Bar),
            columns: vec!["x".into(), "y".into()],
            rows,
            ..Spec::default()
        };
        // Negatives grow left from 0.
        assert_eq!(
            drawn(
                &bar(vec![
                    vec![t("up"), n(10.0)],
                    vec![t("down"), n(-5.0)],
                    vec![t("zero"), n(0.0)],
                ]),
                30,
                COLOUR
            ),
            [
                "up             ████████████ 10",
                "down     ██████ -5",
                "zero            0",
            ]
        );
        // Equal values fill the chart; a range too wide for the smallest
        // values to show leaves them without a bar.
        assert_eq!(
            drawn(
                &bar(vec![vec![t("a"), n(5.0)], vec![t("b"), n(5.0)]]),
                24,
                COLOUR
            ),
            ["a        █████████████ 5", "b        █████████████ 5"]
        );
        assert_eq!(
            drawn(
                &bar(vec![
                    vec![t("tiny"), n(1e-300)],
                    vec![t("huge"), n(1e300)],
                    vec![t("neg"), n(-1.7e308)],
                ]),
                30,
                COLOUR
            ),
            [
                "tiny                  1e-300",
                "huge                  1e300",
                "neg      ████████████ -1.7e308",
            ]
        );
        // One point sits in the middle of a range around it.
        let one = Spec {
            kind: Some(Kind::Line),
            columns: vec!["x".into(), "y".into()],
            rows: vec![vec![t("only"), n(7.0)]],
            ..Spec::default()
        };
        assert_eq!(
            drawn(&one, 24, COLOUR),
            [
                "7.7 ┤",
                "    │",
                "    │",
                "    │",
                "    │",
                "  7 ┤         ⠈",
                "    │",
                "    │",
                "    │",
                "6.3 ┤",
                "    └───────────────────",
                "            only",
            ]
        );
        // Wide labels are cut whole; escapes and controls are dropped.
        assert_eq!(
            drawn(
                &bar(vec![
                    vec![t("日本語のラベル"), n(3.0)],
                    vec![t("🎉 party"), n(2.0)],
                    vec![t("a\u{1b}[31mb\nc\u{7}"), n(1.0)],
                ]),
                40,
                COLOUR
            ),
            [
                "日本語のラベ… ████████████████████████ 3",
                "🎉 party      ████████████████ 2",
                "ab c          ████████ 1",
            ]
        );
        // No kind yet, or no data yet, draws nothing to count.
        assert!(draw(&Spec::default(), 40, COLOUR).is_empty());
        assert!(draw(&bar(Vec::new()), 40, COLOUR).is_empty());
    }

    /// A flowchart 26 columns wide: it fits a 60-column card and not a
    /// 28-column one, whose drawing has 24.
    const FLOW: &str =
        "flowchart TD\n    A[Read] -->|ok| B{Valid?}\n    B -->|yes| C[Save]\n    B -. no .-> A";

    fn flow() -> Spec {
        Spec {
            kind: Some(Kind::Diagram),
            title: "Flow".into(),
            source: FLOW.into(),
            ..Spec::default()
        }
    }

    fn done(spec: Spec) -> Visual {
        Visual {
            state: CallState::Done,
            ..Visual::running(spec)
        }
    }

    #[test]
    fn a_diagram_draws_in_the_card_as_an_answer_draws_its_fence() {
        let round = [
            "┌──────────┐",
            "│ Read     │",
            "│          ├──ok────┐",
            "│          ├◄┄┄┄┄┄┄┄╪┄┄┄┐",
            "└──────────┘        │   ┆",
            "                    │   ┆",
            "                    │   ┆",
            "┌──────────┐        │   ┆",
            "│ ◇ Valid? │        │   ┆",
            "│          ├◄───────┘   ┆",
            "│          ├──yes─────┐ ┆",
            "│          ├─┄no┄┄┄┄┄┄╪┄┘",
            "└──────────┘          │",
            "                      │",
            "                      │",
            "┌──────────┐          │",
            "│ Save     │          │",
            "│          ├◄─────────┘",
            "└──────────┘",
        ];
        let classic = [
            "+----------+",
            "| Read     |",
            "|          +--ok----+",
            "|          +<.......+...+",
            "+----------+        |   :",
            "                    |   :",
            "                    |   :",
            "+----------+        |   :",
            "| o Valid? |        |   :",
            "|          +<-------+   :",
            "|          +--yes-----+ :",
            "|          +-.no......+.+",
            "+----------+          |",
            "                      |",
            "                      |",
            "+----------+          |",
            "| Save     |          |",
            "|          +<---------+",
            "+----------+",
        ];
        // Too wide for 24 columns: its source, and why, as in an answer.
        let narrow_round = [
            "╭─ mermaid",
            "│ flowchart TD",
            "│     A[Read] -->|ok|",
            "│ B{Valid?}",
            "│     B -->|yes| C[Save]",
            "│     B -. no .-> A",
            "╰─",
            "diagram too wide for 24",
            "columns; showing source",
        ];
        let narrow_classic = [
            "+- mermaid",
            "| flowchart TD",
            "|     A[Read] -->|ok|",
            "| B{Valid?}",
            "|     B -->|yes| C[Save]",
            "|     B -. no .-> A",
            "+-",
            "diagram too wide for 24",
            "columns; showing source",
        ];
        for look in [COLOUR, ANSI, PLAIN, CLASSIC] {
            let (wide, narrow): (&[&str], &[&str]) = if look.classic {
                (&classic, &narrow_classic)
            } else {
                (&round, &narrow_round)
            };
            let gutter = format!("  {} ", transcript::gutter(look));
            let mark = if look.classic { "+" } else { "✓" };
            for (width, drawing) in [(60, wide), (28, narrow)] {
                let mut expected = vec![format!("  {mark} Diagram: Flow  flowchart")];
                expected.extend(
                    drawing
                        .iter()
                        .map(|row| format!("{gutter}{row}").trim_end().to_owned()),
                );
                let shown: Vec<String> = card(done(flow()), width, look)
                    .into_iter()
                    .map(|row| row.trim_end().to_owned())
                    .collect();
                assert_eq!(shown, expected, "{look:?} at {width}");
            }
        }
    }

    #[test]
    fn a_diagram_takes_the_answers_role_colours() {
        let answer = crate::markdown::present(&format!("```mermaid\n{FLOW}\n```"), 60, COLOUR);
        let shown = present(&done(flow()), 60, COLOUR, Detail::Preview);
        let style = |lines: &[Line<'static>], needle: &str| {
            let span = lines
                .iter()
                .flat_map(|line| &line.spans)
                .find(|span| span.content.contains(needle))
                .unwrap_or_else(|| panic!("{needle}"));
            (span.style.fg, span.style.bg, span.style.add_modifier)
        };
        for needle in ["┌──────────┐", "◄───────┘", "Read"]
        {
            assert_eq!(style(&shown, needle), style(&answer, needle), "{needle}");
        }
        assert_eq!(
            style(&shown, "◄───────┘").0,
            Some(transcript::accent(COLOUR.tones).fg.expect("an accent"))
        );
        // `NO_COLOR` keeps the boxes dim and the edges uncoloured.
        let plain = present(&done(flow()), 60, PLAIN, Detail::Preview);
        assert!(
            plain
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.style.fg.is_none())
        );
        assert_eq!(style(&plain, "┌──────────┐").2, Modifier::DIM);
    }

    #[test]
    fn a_diagram_draws_whatever_markdown_mermaid_says() {
        let shown = card(done(flow()), 60, COLOUR);
        for mermaid in [crate::mermaid::Mermaid::Off, crate::mermaid::Mermaid::Final] {
            let look = Look { mermaid, ..COLOUR };
            assert_eq!(card(done(flow()), 60, look), shown, "{mermaid:?}");
            let streaming = Visual {
                streaming: true,
                ..Visual::running(Spec {
                    source: format!("{FLOW}\n"),
                    partial: true,
                    ..flow()
                })
            };
            assert_eq!(
                card(streaming, 60, look)[1..],
                shown[1..],
                "{mermaid:?} while streaming"
            );
        }
    }

    #[test]
    fn a_streaming_diagram_never_turns_back_into_source_and_settles_whole() {
        let source = "sequenceDiagram\n    participant U as User\n    U->>Bake: prompt\n    loop each tool\n        Bake->>Tool: call\n        Tool-->>Bake: result\n    end\n    %% done\n    Bake-->>U: answer\n";
        for look in [COLOUR, CLASSIC] {
            for width in [24, 40, 80] {
                let mut drew = false;
                let ends = source.char_indices().map(|(at, _)| at).skip(1);
                for end in ends.chain([source.len()]) {
                    // The projector keeps a streaming source to its last
                    // newline.
                    let cut = &source[..source[..end].rfind('\n').map_or(0, |at| at + 1)];
                    let spec = Spec {
                        kind: Some(Kind::Diagram),
                        source: cut.to_owned(),
                        partial: true,
                        ..Spec::default()
                    };
                    let drawn = drawn(&spec, width, look);
                    let source_shown = drawn.first().is_some_and(|row| {
                        row.starts_with(if look.classic { "+- " } else { "╭─ " })
                    });
                    if drew {
                        assert!(!source_shown, "{width}: turned back at {cut:?}");
                    }
                    drew |= !drawn.is_empty() && !source_shown;
                }
                // Once the call ends the whole source draws, as its
                // replay does: the diagram, or, too wide for 24 columns,
                // its source and why.
                let fits = bake_mermaid::outline(source).expect("reads").width <= width;
                let whole = Spec {
                    kind: Some(Kind::Diagram),
                    source: source.to_owned(),
                    ..Spec::default()
                };
                let streamed = Spec {
                    partial: true,
                    ..whole.clone()
                };
                let settled = drawn(&whole, width, look);
                if fits {
                    assert_eq!(drawn(&streamed, width, look), settled, "{width}");
                } else {
                    assert!(settled.last().is_some_and(|row| row.ends_with("source")));
                }
                assert_eq!(fits, width == 80);
                assert!(drew, "{width}");
            }
        }
        // Until a line renders, the streaming source shows as code; with no
        // line yet, nothing.
        let first = Spec {
            kind: Some(Kind::Diagram),
            source: "sequenceDiagram\n".into(),
            partial: true,
            ..Spec::default()
        };
        assert_eq!(
            drawn(&first, 40, PLAIN),
            ["╭─ mermaid", "│ sequenceDiagram", "╰─"]
        );
        let empty = Spec {
            source: String::new(),
            ..first
        };
        assert!(drawn(&empty, 40, PLAIN).is_empty());
    }

    #[test]
    fn a_tall_diagram_previews_its_first_rows_and_ctrl_o_draws_them_all() {
        let source = format!(
            "flowchart TD\n{}",
            (0..7)
                .map(|i| format!("    N{i} --> N{}\n", i + 1))
                .collect::<String>()
        );
        let tall = done(Spec {
            kind: Some(Kind::Diagram),
            source,
            ..Spec::default()
        });
        let all = draw(&tall.spec, 56, PLAIN).len();
        assert!(all > PREVIEW_ROWS + 1, "{all}");
        let preview = card(tall.clone(), 60, PLAIN);
        assert_eq!(preview.len(), 1 + PREVIEW_ROWS + 1);
        assert_eq!(
            preview[PREVIEW_ROWS + 1],
            format!("  │ ⋯ {} more rows", all - PREVIEW_ROWS)
        );
        let full = text(&present(&tall, 60, PLAIN, Detail::Full));
        assert_eq!(full.len(), 1 + all);
    }

    #[test]
    fn numbers_read_short_on_charts_and_exactly_in_tables() {
        let cases = [
            (0.0, "0"),
            (12.5, "12.5"),
            (1500.0, "1500"),
            (12345.0, "12.3k"),
            (4_100_000.0, "4.1M"),
            (-0.00123, "-0.00123"),
            (2.5e15, "2.5e15"),
            (1e-5, "1e-5"),
        ];
        for (value, expected) in cases {
            assert_eq!(short(value), expected, "{value}");
        }
        assert_eq!(exact(1234.5), "1234.5");
        assert_eq!(exact(12.0), "12");
        assert_eq!(exact(1e20), "1e20");
        assert_eq!(exact(-0.25), "-0.25");
    }

    #[test]
    fn the_card_names_the_kind_counts_the_data_and_says_how_it_stands() {
        let streaming = Visual {
            streaming: true,
            ..Visual::running(builds())
        };
        assert_eq!(
            card(streaming, 40, COLOUR)[..2],
            ["  ● Bar chart", "    2 rows · 2 series · streaming"]
        );
        let titled = Visual::running(Spec {
            title: "Build time".into(),
            ..builds()
        });
        assert_eq!(
            card(titled, 60, PLAIN)[0],
            "  ● Bar chart: Build time  2 rows · 2 series"
        );
        // An interrupted visual keeps its drawing.
        let interrupted = Visual {
            state: CallState::Failed,
            summary: Some("interrupted".into()),
            ..Visual::running(builds())
        };
        assert_eq!(
            card(interrupted, 40, PLAIN),
            [
                "  ✗ Bar chart",
                "    2 rows · 2 series   interrupted ",
                "  │ █ debug  ▓ release",
                "  │ bake-ai  ████████▊ 12.5s",
                "  │          ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓ 30s",
                "  │ bake-tui █████▋ 8s",
                "  │           –",
            ]
        );
        // The tool's error takes the drawing's place.
        let failed = Visual {
            state: CallState::Failed,
            error: vec!["rows[4] has 3 values; columns has 2".into()],
            ..Visual::running(builds())
        };
        assert_eq!(
            card(failed, 40, PLAIN),
            [
                "  ✗ Bar chart  2 rows · 2 series",
                "  │ rows[4] has 3 values; columns has 2",
            ]
        );
        let large = Visual {
            streaming: true,
            too_large: true,
            ..Visual::running(builds())
        };
        assert_eq!(
            card(large, 50, PLAIN),
            [
                "  ● Bar chart  streaming",
                "  │ too large to preview until the call ends",
            ]
        );
        let table = Visual::running(files());
        assert_eq!(card(table, 40, PLAIN)[0], "  ● Table  2 rows · 3 columns");
    }

    #[test]
    fn a_preview_draws_its_first_rows_and_ctrl_o_draws_them_all() {
        let tall = Visual {
            state: CallState::Done,
            ..Visual::running(Spec {
                kind: Some(Kind::Bar),
                title: "Tall".into(),
                columns: vec!["x".into(), "y".into()],
                rows: (0..30)
                    .map(|i| vec![t(&format!("r{i}")), n(f64::from(i))])
                    .collect(),
                ..Spec::default()
            })
        };
        let preview = card(tall.clone(), 30, PLAIN);
        assert_eq!(preview.len(), 2 + PREVIEW_ROWS + 1);
        assert_eq!(preview[2], "  │ r0        0");
        assert_eq!(preview[3], "  │ r1       ▌ 1");
        assert_eq!(preview[25], "  │ r23      ███████████▏ 23");
        assert_eq!(preview[26], "  │ ⋯ 6 more rows");
        let full = text(&present(&tall, 30, PLAIN, Detail::Full));
        assert_eq!(full.len(), 2 + 30);
        assert_eq!(full[31], "  │ r29      ██████████████ 29");
        // In the transcript the count takes Ctrl+O's hint like any card's.
        let rows = [Row::Visual(tall)];
        let shown = text(&present_rows(&rows, 0, 30, PLAIN));
        assert_eq!(shown.len(), preview.len());
        let expanded = text(&present_detail(&rows, 0, 30, PLAIN, Detail::Full));
        assert_eq!(expanded.len(), full.len());
    }

    fn parts() -> Spec {
        Spec {
            kind: Some(Kind::Stacked),
            columns: vec![
                "crate".into(),
                "debug".into(),
                "release".into(),
                "test".into(),
            ],
            unit: "s".into(),
            rows: vec![
                vec![t("bake-ai"), n(12.5), n(30.0), n(4.0)],
                vec![t("bake-tui"), n(8.0), Cell::Empty, n(11.0)],
                vec![t("bake-cli"), n(1.0), n(0.3), n(-2.0)],
            ],
            ..Spec::default()
        }
    }

    fn cloud() -> Spec {
        Spec {
            kind: Some(Kind::Scatter),
            columns: vec!["size".into(), "parse".into(), "render".into()],
            unit: "ms".into(),
            rows: vec![
                vec![n(1.0), n(2.0), n(9.0)],
                vec![n(2.0), n(4.0), Cell::Empty],
                vec![n(4.0), n(3.0), n(6.0)],
                vec![Cell::Empty, n(50.0), n(50.0)],
                vec![n(9.0), n(10.0), n(1.0)],
            ],
            ..Spec::default()
        }
    }

    fn shares() -> Spec {
        Spec {
            kind: Some(Kind::Pie),
            columns: vec!["lang".into(), "lines".into()],
            rows: vec![
                vec![t("Rust"), n(182_000.0)],
                vec![t("Markdown"), n(41_000.0)],
                vec![t("TOML"), n(-5.0)],
                vec![t("JSON"), n(15_000.0)],
                vec![t("Shell"), n(1_200.0)],
            ],
            ..Spec::default()
        }
    }

    fn grid() -> Spec {
        Spec {
            kind: Some(Kind::Heatmap),
            columns: vec![
                "day".into(),
                "00h".into(),
                "06h".into(),
                "12h".into(),
                "18h".into(),
            ],
            unit: "%".into(),
            rows: vec![
                vec![t("Mon"), n(0.0), n(25.0), n(50.0), n(100.0)],
                vec![t("Tue"), n(75.0), Cell::Empty, n(10.0), n(60.0)],
            ],
            ..Spec::default()
        }
    }

    /// The first span of `line` whose text holds `needle`.
    fn span_of<'a>(line: &'a Line<'static>, needle: &str) -> &'a Span<'static> {
        line.spans
            .iter()
            .find(|span| span.content.contains(needle))
            .unwrap_or_else(|| panic!("no span holds {needle:?}: {line:?}"))
    }

    // One bar a row: its series end to end, a cell two series share drawn
    // in both colours, and the total after it. A value below 0 is no part.
    #[test]
    fn stacked_bars_put_a_row_s_series_end_to_end() {
        assert_eq!(
            drawn(&parts(), 48, COLOUR),
            [
                "█ debug  █ release  █ test",
                "bake-ai  ████████▊████████████████████▉██▋ 46.5s",
                "bake-tui █████▋███████▍ 19s",
                "bake-cli ▊ 1.3s",
            ]
        );
        let lines = draw(&parts(), 48, COLOUR);
        let shared = span_of(&lines[1], "▊");
        assert_eq!(
            (shared.style.fg, shared.style.bg),
            (Some(rgb(Token::Teal)), Some(rgb(Token::XHigh)))
        );
        // The bar's last cell has nothing behind it.
        let last = span_of(&lines[1], "▋");
        assert_eq!(
            (last.style.fg, last.style.bg),
            (Some(rgb(Token::High)), None)
        );
        // Without colour each cell takes its series' shade.
        assert_eq!(
            drawn(&parts(), 48, PLAIN),
            [
                "█ debug  ▓ release  ▒ test",
                "bake-ai  █████████▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▒▒▒ 46.5s",
                "bake-tui ██████▒▒▒▒▒▒▒ 19s",
                "bake-cli █ 1.3s",
            ]
        );
        assert_eq!(
            drawn(&parts(), 30, CLASSIC),
            [
                "# debug  = release  * test",
                "bake-ai  ####==========* 46.5s",
                "bake-tui ##**** 19s",
                "bake-cli  1.3s",
            ]
        );
    }

    // Lines filled down to 0, the smaller area in front.
    #[test]
    fn areas_fill_their_lines_down_to_zero() {
        let area = Spec {
            kind: Some(Kind::Area),
            ..trend()
        };
        assert_eq!(
            drawn(&area, 30, COLOUR),
            [
                "█ a  █ b",
                "8 ┤                  ⢀⣾⣶⣄",
                "  │                 ⣠⣿⣿⣿⣿⣿⣦⣄",
                "  │                ⣰⣿⣿⣿⣿⣿⣿⣿⣿⣷⣦",
                "  │      ⢀⡀      ⢀⣼⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿",
                "  │   ⢀⣤⣾⣿⣿⣷⣶⣤⣄⡀⢠⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿",
                "4 ┤⢀⣠⣶⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿",
                "  │⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿",
                "  │⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿",
                "  │⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿",
                "0 ┤⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿",
                "  └───────────────────────────",
                "   1            3            5",
            ]
        );
        // The peak is the first series'; along the axis the second, which
        // stays under it, is in front.
        let lines = draw(&area, 30, COLOUR);
        assert_eq!(span_of(&lines[1], "⣾").style.fg, Some(rgb(Token::Teal)));
        assert_eq!(span_of(&lines[10], "⣿").style.fg, Some(rgb(Token::XHigh)));
        // In markers, the series in front shows by its glyph.
        assert_eq!(
            drawn(&area, 30, CLASSIC),
            [
                "* a  o b",
                "8 +                   **",
                "  |                  *******",
                "  |                ***********",
                "  |     ***       ************",
                "  |   *********  *************",
                "4 + **********************oooo",
                "  |****************ooooooooooo",
                "  |**********ooooooooooooooooo",
                "  |***oooooooooooooooooooooooo",
                "0 +ooooooooooooooooooooooooooo",
                "  +---------------------------",
                "   1            3            5",
            ]
        );
    }

    // Points by their x and y: both axes labelled, the x axis named, and
    // a row without an x left out.
    #[test]
    fn scatters_plot_points_on_two_value_axes() {
        assert_eq!(
            drawn(&cloud(), 36, COLOUR),
            [
                "● parse  ● render",
                " 10ms ┤                            ⠛",
                "      │⠛",
                "      │",
                "      │",
                "      │          ⠶",
                "5.5ms ┤",
                "      │   ⠶",
                "      │          ⣤",
                "      │⣤",
                "  1ms ┤                            ⣤",
                "      └─────────────────────────────",
                "       1             5             9",
                "                   size",
            ]
        );
        let lines = draw(&cloud(), 36, COLOUR);
        assert_eq!(span_of(&lines[1], "⠛").style.fg, Some(rgb(Token::Teal)));
        assert_eq!(span_of(&lines[2], "⠛").style.fg, Some(rgb(Token::XHigh)));
        assert_eq!(
            drawn(&cloud(), 36, CLASSIC),
            [
                "* parse  o render",
                " 10ms +                            *",
                "      |o",
                "      |",
                "      |",
                "      |           o",
                "5.5ms +",
                "      |    *",
                "      |           *",
                "      |*",
                "  1ms +                            o",
                "      +-----------------------------",
                "       1             5             9",
                "                   size",
            ]
        );
    }

    // A disc beside its legend, under it when the width is short, and
    // bars where a disc's slices could not be told apart.
    #[test]
    fn pies_show_shares_as_a_disc_or_as_bars() {
        assert_eq!(
            drawn(&shares(), 48, COLOUR),
            [
                "   ▄▄██████▄▄",
                " ▄████████████▄",
                "▄█████▀████████▄  █ Rust      76%  182k",
                "▀██████▀████████  █ Markdown  17%   41k",
                "████████████████  █ JSON       6%   15k",
                "▀██████████████▀  █ Shell      1%  1200",
                " ▀████████████▀",
                "   ▀▀██████▀▀",
            ]
        );
        // Each legend key is its slice's colour, in the series' order.
        let lines = draw(&shares(), 48, COLOUR);
        for (line, token) in [
            (2, Token::Teal),
            (3, Token::XHigh),
            (4, Token::High),
            (5, Token::Slate),
        ] {
            let key = lines[line]
                .spans
                .iter()
                .rev()
                .find(|span| span.content == "█")
                .expect("a key");
            assert_eq!(key.style.fg, Some(rgb(token)), "line {line}");
        }
        // Clockwise from the top, the first slice comes first.
        assert_eq!(
            lines[0].spans.last().and_then(|span| span.style.fg),
            Some(rgb(Token::Teal))
        );
        let narrow = drawn(&shares(), 24, COLOUR);
        assert_eq!(narrow.len(), 8 + 4);
        assert_eq!(narrow[8], "█ Rust      76%  182k");
        assert_eq!(
            drawn(&shares(), 40, PLAIN),
            [
                "Rust     ███████████████▎      76%  182k",
                "Markdown ███▍                  17%   41k",
                "JSON     █▎                     6%   15k",
                "Shell    ▏                      1%  1200",
            ]
        );
        assert_eq!(
            drawn(&shares(), 40, CLASSIC),
            [
                "Rust     ###############       76%  182k",
                "Markdown ###                   17%   41k",
                "JSON     #                      6%   15k",
                "Shell                           1%  1200",
            ]
        );
        // A large pie takes more rows.
        let large = Spec {
            large: true,
            ..shares()
        };
        assert_eq!(drawn(&large, 60, COLOUR).len(), 14);
        // More rows than slices keep the largest, in the rows' order, and
        // gather the rest; a share that rounds to nothing still shows.
        let many = Spec {
            kind: Some(Kind::Pie),
            rows: (0..10)
                .map(|i| {
                    vec![
                        t(&format!("s{i}")),
                        n(if i == 3 { 0.4 } else { f64::from(i) + 1.0 }),
                    ]
                })
                .collect(),
            ..Spec::default()
        };
        assert_eq!(
            drawn(&many, 30, PLAIN),
            [
                "s4       █▏           10%    5",
                "s5       █▎           12%    6",
                "s6       █▌           14%    7",
                "s7       █▊           16%    8",
                "s8       █▉           18%    9",
                "s9       ██▏          19%   10",
                "other    █▍           12%  6.4",
            ]
        );
        assert_eq!(charts::slices(&many).len(), charts::SLICES);
        assert_eq!(charts::percent(0.004), "<1%");
        assert_eq!(charts::percent(0.0), "0%");
    }

    // A cell per row and column: its colour by value with the value in
    // it, a shade without colour, and the scale under the rows.
    #[test]
    fn heatmaps_colour_each_cell_by_its_value() {
        assert_eq!(
            drawn(&grid(), 40, COLOUR),
            [
                "           00h    06h    12h    18h",
                "Mon         0     25     50     100  ",
                "Tue        75            10     60   ",
                "         0%            100%",
            ]
        );
        let lines = draw(&grid(), 40, COLOUR);
        let hot = span_of(&lines[1], "100");
        assert_eq!(
            (hot.style.bg, hot.style.fg),
            (
                Some(Color::Rgb(0x3d, 0xe8, 0xcf)),
                Some(Color::Rgb(0, 0, 0))
            )
        );
        let cold = span_of(&lines[1], " 0 ");
        assert_eq!(
            (cold.style.bg, cold.style.fg),
            (
                Some(Color::Rgb(0x14, 0x2b, 0x3a)),
                Some(Color::Rgb(255, 255, 255))
            )
        );
        // A missing value has no colour.
        assert!(
            lines[2]
                .spans
                .iter()
                .any(|span| span.content == "       " && span.style == Style::new())
        );
        // On the ANSI palette the scale takes the nearest colours.
        let ansi = draw(&grid(), 40, ANSI);
        assert!(matches!(
            span_of(&ansi[1], "100").style.bg,
            Some(Color::Indexed(_))
        ));
        // Cells too narrow for their values are colour alone.
        assert_eq!(
            drawn(&grid(), 12, COLOUR),
            ["     0011", "Mon      ", "Tue      ", "     0%     "]
        );
        assert_eq!(
            drawn(&grid(), 40, PLAIN),
            [
                "           00h    06h    12h    18h",
                "Mon      ░░░░░░░▒▒▒▒▒▒▒▓▓▓▓▓▓▓███████",
                "Tue      ▓▓▓▓▓▓▓       ░░░░░░░▓▓▓▓▓▓▓",
                "         0% ░░▒▒▓▓██ 100%",
            ]
        );
        let ascii = Look {
            tones: Tones::None,
            ..CLASSIC
        };
        assert_eq!(
            drawn(&grid(), 40, ascii),
            [
                "           00h    06h    12h    18h",
                "Mon      .......:::::::+++++++#######",
                "Tue      +++++++       .......+++++++",
                "         0% ..::++## 100%",
            ]
        );
        // A heatmap takes more columns than a chart has series.
        let wide = Spec {
            kind: Some(Kind::Heatmap),
            columns: (0..30).map(|i| format!("c{i}")).collect(),
            rows: vec![(0..30).map(|i| n(f64::from(i))).collect()],
            ..Spec::default()
        };
        assert_eq!(series_of(&wide), HEAT_COLUMNS);
        // The label column and its gap, then two cells a column.
        assert_eq!(
            drawn(&wide, 80, PLAIN)[1].chars().count(),
            9 + HEAT_COLUMNS * 2
        );
    }

    // A large plot takes twice the rows, and a preview keeps a plot, a
    // pie, and an image whole.
    #[test]
    fn a_large_plot_is_taller_and_previews_whole() {
        let large = Spec {
            large: true,
            ..trend()
        };
        // The legend, the plot, the axis, and the x labels.
        assert_eq!(drawn(&large, 40, COLOUR).len(), 1 + LARGE_PLOT_ROWS + 2);
        assert_eq!(drawn(&trend(), 40, COLOUR).len(), 1 + PLOT_ROWS + 2);
        let shown = card(Visual::running(large.clone()), 40, COLOUR);
        assert_eq!(shown.len(), 1 + 1 + LARGE_PLOT_ROWS + 2);
        assert!(!shown.iter().any(|line| line.contains("more row")));
        assert_eq!(shown[0], "  ● Line chart  5 rows · 2 series");
        for (spec, head) in [
            (parts(), "  ● Stacked bar chart  3 rows · 3 series"),
            (cloud(), "  ● Scatter chart  5 rows · 2 series"),
            (shares(), "  ● Pie chart  5 rows"),
            (grid(), "  ● Heatmap  2 rows · 4 columns"),
        ] {
            assert_eq!(card(Visual::running(spec), 60, PLAIN)[0], head);
        }
    }

    fn any_cell() -> impl Strategy<Value = Cell> {
        prop_oneof![
            any::<f64>().prop_map(|v| Number::new(v).map_or(Cell::Empty, Cell::Number)),
            prop_oneof![
                Just(0.0),
                Just(-1e308),
                Just(1e308),
                Just(1e-310),
                -1e6..1e6f64
            ]
            .prop_map(n),
            "\\PC{0,50}|[\\x00-\\x1f\\x7f\\u{1b}a-z]{0,20}".prop_map(Cell::Text),
            Just(Cell::Empty),
        ]
    }

    fn any_spec() -> impl Strategy<Value = Spec> {
        (
            prop_oneof![
                Just(None),
                Just(Some(Kind::Bar)),
                Just(Some(Kind::Stacked)),
                Just(Some(Kind::Line)),
                Just(Some(Kind::Area)),
                Just(Some(Kind::Scatter)),
                Just(Some(Kind::Pie)),
                Just(Some(Kind::Heatmap)),
                Just(Some(Kind::Table))
            ],
            "\\PC{0,80}|[\\x00-\\x1f\\u{1b}\\[m0-9]{0,20}",
            prop::collection::vec("\\PC{0,50}|[\\x00-\\x1f\\u{1b}a]{0,10}", 0..10),
            prop::option::of(any::<f64>()),
            prop::option::of(any::<f64>()),
            "\\PC{0,20}",
            prop::collection::vec(prop::collection::vec(any_cell(), 0..30), 0..40),
            any::<bool>(),
        )
            .prop_map(|(kind, title, columns, min, max, unit, rows, large)| Spec {
                kind,
                title,
                columns,
                min: min.and_then(Number::new),
                max: max.and_then(Number::new),
                unit,
                rows,
                large,
                ..Spec::default()
            })
    }

    /// Diagram specs: Mermaid sources, valid and not, whole or cut.
    fn any_diagram() -> impl Strategy<Value = Spec> {
        const PIECES: &[&str] = &[
            "flowchart LR\n",
            "flowchart TD\n",
            "sequenceDiagram\n",
            "stateDiagram-v2\n",
            "classDiagram\n",
            "erDiagram\n",
            "A[Read] -->|ok| B{Valid?}\n",
            "B --> C([Done])\n",
            "A->>B: hi\n",
            "B-->>A: 请求\n",
            "loop retry\n",
            "end\n",
            "[*] --> S\n",
            "class C {\n",
            "+int id\n",
            "}\n",
            "USER ||--o{ ORDER : places\n",
            "subgraph s\n",
            "%% note\n",
            "\t",
            "\u{1b}[31m",
            "\r\n",
            "    ",
        ];
        (
            "\\PC{0,40}",
            prop_oneof![
                prop::collection::vec(prop::sample::select(PIECES), 0..30)
                    .prop_map(|pieces| pieces.concat()),
                "\\PC{0,200}|[\\x00-\\x1f\\u{1b}\\n a-z>-]{0,80}",
            ],
            any::<bool>(),
        )
            .prop_map(|(title, source, partial)| Spec {
                kind: Some(Kind::Diagram),
                title,
                source,
                partial,
                ..Spec::default()
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        // Any spec draws inside its width under every look, without a
        // control character, in every state.
        #[test]
        fn any_spec_draws_inside_its_width(
            spec in prop_oneof![any_spec(), any_diagram()],
            width in 1usize..140,
            state in 0usize..3,
            streaming in any::<bool>(),
        ) {
            let state = [CallState::Running, CallState::Done, CallState::Failed][state];
            let visual = Visual {
                spec,
                state,
                streaming,
                too_large: false,
                summary: None,
                error: Vec::new(),
            };
            for look in [COLOUR, ANSI, PLAIN, CLASSIC] {
                for detail in [Detail::Preview, Detail::Full] {
                    for line in present(&visual, width, look, detail) {
                        prop_assert!(line.width() <= width, "{width}: {line:?}");
                        for span in &line.spans {
                            prop_assert!(!span.content.chars().any(char::is_control), "{span:?}");
                        }
                    }
                }
            }
        }
    }
}
