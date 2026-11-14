//! The charts that are neither plain bars nor lines: stacked bars, a
//! scatter, a pie, and a heatmap.
//!
//! - **Stacked bars** take one row per category: its series end to end
//!   from 0, each in its series' colour, and the row's total after them.
//!   Where colour tells the series apart, a cell two series share is an
//!   eighth block in the left one's colour on the right one's; elsewhere
//!   each cell takes the glyph of the series at its middle. Values below 0
//!   are left out, as parts of a whole have none.
//! - **Scatters** plot each row's series against the row's first value,
//!   its x: two by two dots in braille, or a marker. Both axes take their
//!   bounds from the data; the x axis is labelled at its ends and middle
//!   and named after the first column.
//! - **Pies** show one series as shares of its total: a disc of half
//!   blocks beside a legend of each slice's share and value. More than
//!   [`SLICES`] rows keep the largest and gather the rest into one slice.
//!   Without colour, or under the classic frame, slices cannot be told
//!   apart on a disc, so the shares are drawn as bars.
//! - **Heatmaps** draw a cell per row and column on a scale from the
//!   least value to the greatest: the cell's background where there is
//!   colour, with its value in it when the cell has room, and a shade of
//!   `░▒▓█` (or `.:+#`) without.

use ratatui_core::style::Color;
use unicode_segmentation::UnicodeSegmentation;

use super::*;

/// Most slices a pie draws.
pub const SLICES: usize = 7;
/// Rows a pie's disc takes, and when the call asks for a large one.
const PIE_ROWS: usize = 8;
const LARGE_PIE_ROWS: usize = 14;
/// Most cells a heatmap's cell takes.
const HEAT_CELL: usize = 7;
/// The heatmap's scale, from its least value to its greatest.
const COLD: (u8, u8, u8) = (0x14, 0x2b, 0x3a);
const HOT: (u8, u8, u8) = (0x3d, 0xe8, 0xcf);
/// Steps of the scale a heatmap's legend shows.
const SCALE_STEPS: usize = 5;

/// A series' colour, when the look has colours.
fn series_colour(series: usize, look: Look) -> Option<Color> {
    series_style(series, look).fg
}

/// A label column as a bar chart sizes it: the labels' width stepped up to
/// a multiple of [`LABEL_STEP`], at most [`LABEL_COLUMN`] and a third of
/// `width`.
fn label_column(labels: &[String], width: usize) -> usize {
    let widest = labels.iter().map(|label| label.width()).max().unwrap_or(0);
    widest
        .div_ceil(LABEL_STEP)
        .saturating_mul(LABEL_STEP)
        .min(LABEL_COLUMN)
        .min(width / 3)
}

/// Stacked bars: a legend for several series, then each row's label, its
/// series end to end, and its total.
pub(super) fn stacked(spec: &Spec, width: usize, look: Look) -> Vec<Line<'static>> {
    let series = series_of(spec).max(1);
    let glyphs = glyphs(look, series);
    let unit = fit(&clean(&spec.unit), UNIT_CELLS);
    let labels: Vec<String> = spec.rows.iter().map(|row| label_of(row.first())).collect();
    // Parts of a whole: a value below 0 is no part.
    let parts: Vec<Vec<f64>> = values(spec, series)
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|value| value.filter(|v| *v > 0.0).unwrap_or(0.0))
                .collect()
        })
        .collect();
    let totals: Vec<f64> = parts
        .iter()
        .map(|row| row.iter().fold(0.0, |sum, v| (sum + v).min(f64::MAX)))
        .collect();
    let most = totals.iter().copied().fold(0.0, f64::max);
    let (_, hi) = bounds_of(None, spec.max, Some((0.0, most)), true);
    let text = |total: f64| format!("{}{unit}", short(total));
    let mut value_cells = totals.iter().map(|&v| text(v).width()).max().unwrap_or(0);
    let mut label_cells = label_column(&labels, width);
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
    let whole = !glyphs.eighths;
    let key = |s: usize| {
        if whole {
            glyphs.bars[s % MAX_SERIES]
        } else {
            "█"
        }
    };
    let mut lines = legend(spec, series, &key, width, look);
    let eighths = area.saturating_mul(8);
    for ((row, label), total) in parts.iter().zip(&labels).zip(&totals) {
        let mut spans = Vec::new();
        if label_cells > 0 {
            spans.push(Span::raw(padded(label, label_cells)));
            spans.push(pad(gap));
        }
        // Where each series ends, in eighths of a cell from the start.
        let mut sum = 0.0f64;
        let ends: Vec<usize> = row
            .iter()
            .map(|value| {
                sum = (sum + value).min(f64::MAX);
                (ratio(sum, 0.0, hi) * eighths as f64).round() as usize
            })
            .collect();
        let length = ends.last().copied().unwrap_or(0);
        // The series drawn at an eighth, if the bar reaches it.
        let at = |eighth: usize| ends.iter().position(|end| eighth < *end);
        for cell in 0..length.div_ceil(8) {
            let start = cell * 8;
            let (glyph, style) = if whole {
                match at(start + 4) {
                    Some(s) => (glyphs.bars[s % MAX_SERIES], series_style(s, look)),
                    None => break,
                }
            } else {
                let Some(left) = at(start) else {
                    break;
                };
                let right = at(start + 7);
                // The eighths of the cell the left series fills.
                let filled = (ends[left] - start).min(8);
                let style = series_style(left, look);
                match right {
                    _ if filled == 8 => ("█", style),
                    Some(right) => (
                        EIGHTHS[filled - 1],
                        series_colour(right, look).map_or(style, |colour| style.bg(colour)),
                    ),
                    None => (EIGHTHS[filled - 1], style),
                }
            };
            push_span(&mut spans, glyph, style);
        }
        if value_cells > 0 {
            spans.push(pad(1));
            spans.push(Span::styled(text(*total), dim()));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// Appends `text` in `style`, joining it to the last span when the style
/// is the same.
fn push_span(spans: &mut Vec<Span<'static>>, text: &str, style: Style) {
    if let Some(last) = spans.last_mut()
        && last.style == style
        && !last.content.is_empty()
    {
        last.content.to_mut().push_str(text);
        return;
    }
    spans.push(Span::styled(text.to_owned(), style));
}

/// A scatter: a legend for several series, the points behind a y axis,
/// the x axis with its ends and middle labelled, and the x column's name.
pub(super) fn scatter(spec: &Spec, width: usize, look: Look) -> Vec<Line<'static>> {
    let series = series_of(spec).max(1);
    let glyphs = glyphs(look, series);
    let unit = fit(&clean(&spec.unit), UNIT_CELLS);
    // A row without an x has no point.
    let points: Vec<(f64, Vec<Option<f64>>)> = spec
        .rows
        .iter()
        .zip(values(spec, series))
        .filter_map(|(row, ys)| Some((value_of(row.first())?, ys)))
        .collect();
    let ys: Vec<Vec<Option<f64>>> = points.iter().map(|(_, ys)| ys.clone()).collect();
    let (lo, hi) = bounds(spec, extent(&ys), false);
    let xs: Vec<Vec<Option<f64>>> = points.iter().map(|(x, _)| vec![Some(*x)]).collect();
    let (x_lo, x_hi) = bounds_of(None, None, extent(&xs), false);
    let (y_labels, y_cells) = y_axis(lo, hi, &unit, width);
    let columns = width.saturating_sub(y_cells + 2).max(1);
    let key = |s: usize| {
        if glyphs.braille {
            "●"
        } else {
            glyphs.markers[s % MAX_SERIES]
        }
    };
    let mut lines = legend(spec, series, &key, width, look);
    let mut plot = Plot::new(columns, plot_rows(spec), glyphs.braille);
    let (dots_x, dots_y) = plot.dots();
    // A braille point is two dots a side, so it keeps one from each end;
    // across, it fills a cell's two columns, so points read as one shape.
    let side = if glyphs.braille { 2 } else { 1 };
    let dot = |t: f64, dots: usize| (t * dots.saturating_sub(side) as f64).round() as i64;
    for (x, ys) in &points {
        let px = dot(ratio(*x, x_lo, x_hi), dots_x) / side as i64 * side as i64;
        for (s, y) in ys.iter().enumerate() {
            let Some(y) = y else {
                continue;
            };
            let py = dot(1.0 - ratio(*y, lo, hi), dots_y);
            for dx in 0..side as i64 {
                for dy in 0..side as i64 {
                    plot.set(px + dx, py + dy, s, &glyphs);
                }
            }
        }
    }
    lines.extend(plot.lines(&y_labels, y_cells, &glyphs, look));
    let indent = y_cells + 2;
    let x_mid = x_lo / 2.0 + x_hi / 2.0;
    lines.push(axis_labels(
        [short(x_lo), short(x_mid), short(x_hi)],
        columns,
        indent,
    ));
    let name = spec
        .columns
        .first()
        .map(|name| fit(&clean(name), columns.min(LABEL_CELLS)))
        .unwrap_or_default();
    if !name.is_empty() {
        let before = indent + columns.saturating_sub(name.width()) / 2;
        lines.push(Line::from(vec![pad(before), Span::styled(name, dim())]));
    }
    lines
}

/// An axis's labels under a plot `plot` cells wide that starts `indent`
/// cells in: its ends at the plot's edges, and its middle when it fits
/// between them with room to spare.
fn axis_labels(labels: [String; 3], plot: usize, indent: usize) -> Line<'static> {
    let room = (plot / 3).max(1);
    let [lo, mid, hi] = labels.map(|label| fit(&label, room));
    let mut spans = vec![pad(indent), Span::styled(lo.clone(), dim())];
    let mut at = lo.width();
    let hi_start = plot.saturating_sub(hi.width());
    let mid_start = (plot / 2).saturating_sub(mid.width() / 2);
    if mid_start >= at + 2 && mid_start + mid.width() + 2 <= hi_start {
        spans.push(pad(mid_start - at));
        at = mid_start + mid.width();
        spans.push(Span::styled(mid, dim()));
    }
    if hi_start >= at + 2 {
        spans.push(pad(hi_start - at));
        spans.push(Span::styled(hi, dim()));
    }
    Line::from(spans)
}

/// A pie's slices: each row's label and value where the value is above
/// 0, in the rows' order. More than [`SLICES`] keep the largest and
/// gather the rest into a last slice.
pub(super) fn slices(spec: &Spec) -> Vec<(String, f64)> {
    let mut slices: Vec<(String, f64)> = spec
        .rows
        .iter()
        .filter_map(|row| {
            let value = value_of(row.get(1)).filter(|v| *v > 0.0)?;
            Some((label_of(row.first()), value))
        })
        .collect();
    if slices.len() <= SLICES {
        return slices;
    }
    // The least value that keeps its own slice; ties go to the earlier row.
    let mut order: Vec<usize> = (0..slices.len()).collect();
    order.sort_by(|&a, &b| slices[b].1.total_cmp(&slices[a].1).then(a.cmp(&b)));
    let kept: Vec<usize> = order[..SLICES - 1].to_vec();
    let rest = order[SLICES - 1..]
        .iter()
        .fold(0.0, |sum: f64, &i| (sum + slices[i].1).min(f64::MAX));
    let mut at = 0;
    slices.retain(|_| {
        at += 1;
        kept.contains(&(at - 1))
    });
    slices.push((copy::PIE_OTHER.to_owned(), rest));
    slices
}

/// A share as a legend shows it: `42%`, or `<1%` for one that rounds to
/// nothing.
pub(super) fn percent(share: f64) -> String {
    let rounded = (share * 100.0).round();
    if rounded < 1.0 && share > 0.0 {
        "<1%".to_owned()
    } else {
        format!("{rounded:.0}%")
    }
}

/// A pie: the disc beside its legend, or under it when the width is short;
/// without colour, or under the classic frame, each share as a bar.
pub(super) fn pie(spec: &Spec, width: usize, look: Look) -> Vec<Line<'static>> {
    let slices = slices(spec);
    let total = slices
        .iter()
        .fold(0.0, |sum: f64, (_, v)| (sum + v).min(f64::MAX));
    if slices.is_empty() || total <= 0.0 {
        return Vec::new();
    }
    let unit = fit(&clean(&spec.unit), UNIT_CELLS);
    let shares: Vec<f64> = slices.iter().map(|(_, v)| v / total).collect();
    let values: Vec<String> = slices
        .iter()
        .map(|(_, v)| format!("{}{unit}", short(*v)))
        .collect();
    let labels: Vec<String> = slices.iter().map(|(label, _)| label.clone()).collect();
    let value_cells = values.iter().map(|v| v.width()).max().unwrap_or(0);
    if look.classic || look.tones == Tones::None {
        return share_bars(&labels, &shares, &values, width, look);
    }
    let rows = if spec.large { LARGE_PIE_ROWS } else { PIE_ROWS };
    // Half blocks are square pixels: twice as many across as rows.
    let rows = rows.min(width / 2).max(1);
    let size = rows * 2;
    let disc = disc(&shares, size, look);
    // The legend: a key, the label, the share, and the value.
    let label_cells = labels.iter().map(|l| l.width()).max().unwrap_or(0);
    let legend_row = |i: usize, room: usize| -> Vec<Span<'static>> {
        let share = percent(shares[i]);
        // The key and the share always; the value and the label as fit.
        let fixed = 2 + 1 + 4;
        let with_value = fixed + 2 + value_cells;
        let (label_room, value) = if room >= with_value + 4 {
            (room - with_value, true)
        } else {
            (room.saturating_sub(fixed), false)
        };
        let cells = label_cells.min(label_room);
        let mut spans = vec![Span::styled("█", series_style(i, look)), pad(1)];
        spans.push(Span::raw(padded(&labels[i], cells)));
        spans.push(pad(1));
        spans.push(Span::raw(format!("{share:>4}")));
        if value {
            spans.push(pad(2));
            spans.push(Span::styled(format!("{:>value_cells$}", values[i]), dim()));
        }
        spans
    };
    let beside = width.saturating_sub(size + 2);
    if beside >= 12 {
        let count = slices.len();
        let height = rows.max(count);
        let first = (height - count) / 2;
        let top = (height - rows) / 2;
        return (0..height)
            .map(|r| {
                let row = r.checked_sub(top).and_then(|r| disc.get(r));
                let mut spans = row.map(|row| row.spans.clone()).unwrap_or_default();
                if let Some(i) = r.checked_sub(first).filter(|i| *i < count) {
                    let drawn = row.map_or(0, Line::width);
                    spans.push(pad(size.saturating_sub(drawn) + 2));
                    spans.extend(legend_row(i, beside));
                }
                Line::from(spans)
            })
            .collect();
    }
    let mut lines = disc;
    lines.extend((0..slices.len()).map(|i| Line::from(legend_row(i, width))));
    lines
}

/// A disc of `size` by `size` pixels in half blocks, its slices clockwise
/// from the top in the series' colours.
fn disc(shares: &[f64], size: usize, look: Look) -> Vec<Line<'static>> {
    let radius = size as f64 / 2.0;
    let slice_at = |x: usize, y: usize| -> Option<usize> {
        let (dx, dy) = (x as f64 + 0.5 - radius, y as f64 + 0.5 - radius);
        if dx * dx + dy * dy > radius * radius {
            return None;
        }
        // The turn from the top, clockwise, from 0 to 1.
        let turn = (dx.atan2(-dy) / std::f64::consts::TAU).rem_euclid(1.0);
        let mut end = 0.0;
        for (i, share) in shares.iter().enumerate() {
            end += share;
            if turn < end {
                return Some(i);
            }
        }
        shares.len().checked_sub(1)
    };
    let colour = |slice: usize| series_colour(slice, look);
    (0..size.div_ceil(2))
        .map(|row| {
            let mut spans = Vec::new();
            for x in 0..size {
                let top = slice_at(x, row * 2).and_then(colour);
                let bottom = slice_at(x, row * 2 + 1).and_then(colour);
                let (glyph, style) = match (top, bottom) {
                    (None, None) => (" ", Style::new()),
                    (Some(top), None) => ("▀", Style::new().fg(top)),
                    (None, Some(bottom)) => ("▄", Style::new().fg(bottom)),
                    (Some(top), Some(bottom)) if top == bottom => ("█", Style::new().fg(top)),
                    (Some(top), Some(bottom)) => ("▀", Style::new().fg(top).bg(bottom)),
                };
                push_span(&mut spans, glyph, style);
            }
            // Trailing blanks are not drawn.
            while spans
                .last()
                .is_some_and(|span| span.style == Style::new() && span.content.trim().is_empty())
            {
                spans.pop();
            }
            Line::from(spans)
        })
        .collect()
}

/// Shares as bars, where a disc's slices could not be told apart: each
/// slice's label, a bar of its share of the whole width, the share, and
/// the value.
fn share_bars(
    labels: &[String],
    shares: &[f64],
    values: &[String],
    width: usize,
    look: Look,
) -> Vec<Line<'static>> {
    let mut label_cells = label_column(labels, width);
    let mut value_cells = values.iter().map(|v| v.width()).max().unwrap_or(0);
    let gap = usize::from(label_cells > 0);
    // The share, such as ` 100%`.
    let share_cells = 5;
    let mut area = width.saturating_sub(label_cells + gap + share_cells + 2 + value_cells);
    if area < 4 {
        value_cells = 0;
        area = width.saturating_sub(label_cells + gap + share_cells);
    }
    if area < 2 {
        label_cells = 0;
        area = width.saturating_sub(share_cells);
    }
    let gap = usize::from(label_cells > 0);
    let full = if look.classic { "#" } else { "█" };
    labels
        .iter()
        .zip(shares)
        .zip(values)
        .map(|((label, share), value)| {
            let mut spans = Vec::new();
            if label_cells > 0 {
                spans.push(Span::raw(padded(label, label_cells)));
                spans.push(pad(gap));
            }
            let eighths = (share * (area * 8) as f64).round() as usize;
            let (cells, rest) = if look.classic {
                ((eighths + 4) / 8, 0)
            } else {
                (eighths / 8, eighths % 8)
            };
            let mut bar = full.repeat(cells);
            if rest > 0 {
                bar.push_str(EIGHTHS[rest - 1]);
            }
            let drawn = bar.width();
            spans.push(Span::styled(bar, series_style(0, look)));
            spans.push(pad(area.saturating_sub(drawn)));
            spans.push(Span::raw(format!("{:>share_cells$}", percent(*share))));
            if value_cells > 0 {
                spans.push(pad(2));
                spans.push(Span::styled(format!("{value:>value_cells$}"), dim()));
            }
            Line::from(spans)
        })
        .collect()
}

/// The first graphemes of `text` that fit `cells`, with no ellipsis: a
/// narrow column's name reads better cut than as `…`.
fn head_of(text: &str, cells: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let w = grapheme.width();
        if used + w > cells {
            break;
        }
        used += w;
        out.push_str(grapheme);
    }
    out
}

/// The scale's colour at `t`, from 0 to 1, as the look can show it, and
/// the text colour that reads on it.
fn heat(t: f64, tones: Tones) -> (Color, Color) {
    let mix = |a: u8, b: u8| (f64::from(a) + (f64::from(b) - f64::from(a)) * t).round() as u8;
    let (r, g, b) = (mix(COLD.0, HOT.0), mix(COLD.1, HOT.1), mix(COLD.2, HOT.2));
    // Rec. 601 luma: black reads on the bright half.
    let luma = 0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b);
    let bright = luma > 128.0;
    match tones {
        Tones::TrueColor => (
            Color::Rgb(r, g, b),
            if bright {
                Color::Rgb(0, 0, 0)
            } else {
                Color::Rgb(255, 255, 255)
            },
        ),
        _ => (
            Color::Indexed(crate::svg::indexed(r, g, b)),
            if bright {
                Color::Indexed(16)
            } else {
                Color::Indexed(231)
            },
        ),
    }
}

/// A heatmap: the column names, each row's label and cells, and the scale.
pub(super) fn heatmap(spec: &Spec, width: usize, look: Look) -> Vec<Line<'static>> {
    let columns = series_of(spec);
    if columns == 0 {
        return Vec::new();
    }
    let unit = fit(&clean(&spec.unit), UNIT_CELLS);
    let labels: Vec<String> = spec.rows.iter().map(|row| label_of(row.first())).collect();
    let values = values(spec, columns);
    let (lo, hi) = bounds(spec, extent(&values), false);
    let label_cells = label_column(&labels, width);
    let gap = usize::from(label_cells > 0);
    let room = width.saturating_sub(label_cells + gap);
    let cell = (room / columns).clamp(1, HEAT_CELL);
    // Columns past the width are not drawn.
    let columns = columns.min(room / cell).max(1);
    let plain = look.tones == Tones::None;
    let shades: [&str; 4] = if look.classic {
        [".", ":", "+", "#"]
    } else {
        ["░", "▒", "▓", "█"]
    };
    let shade = |t: f64| shades[((t * 3.0).round() as usize).min(3)];
    let texts: Vec<Vec<Option<String>>> = values
        .iter()
        .map(|row| row.iter().map(|v| v.map(short)).collect())
        .collect();
    let widest = texts
        .iter()
        .flatten()
        .flatten()
        .map(|text| text.width())
        .max()
        .unwrap_or(0);
    // A value in its cell needs a blank beside it to read.
    let labelled = !plain && widest < cell;
    let mut lines = Vec::new();
    let mut names = vec![pad(label_cells + gap)];
    for c in 0..columns {
        let name = spec
            .columns
            .get(c + 1)
            .map(|name| head_of(&clean(name), cell.saturating_sub(usize::from(cell > 2))))
            .unwrap_or_default();
        let before = cell.saturating_sub(name.width()) / 2;
        names.push(pad(before));
        names.push(Span::styled(
            padded(&name, cell.saturating_sub(before)),
            dim(),
        ));
    }
    // The last name's padding is not drawn.
    if let Some(last) = names.last_mut() {
        last.content = last.content.trim_end().to_owned().into();
    }
    lines.push(Line::from(names));
    for ((row, texts), label) in values.iter().zip(&texts).zip(&labels) {
        let mut spans = Vec::new();
        if label_cells > 0 {
            spans.push(Span::raw(padded(label, label_cells)));
            spans.push(pad(gap));
        }
        for c in 0..columns {
            let Some(value) = row.get(c).copied().flatten() else {
                spans.push(pad(cell));
                continue;
            };
            let t = ratio(value, lo, hi);
            if plain {
                spans.push(Span::raw(shade(t).repeat(cell)));
                continue;
            }
            let (back, ink) = heat(t, look.tones);
            let style = Style::new().bg(back).fg(ink);
            let text = texts
                .get(c)
                .and_then(Option::as_deref)
                .filter(|_| labelled)
                .unwrap_or_default();
            let before = cell.saturating_sub(text.width()) / 2;
            let after = cell.saturating_sub(text.width() + before);
            spans.push(Span::styled(
                format!("{}{text}{}", " ".repeat(before), " ".repeat(after)),
                style,
            ));
        }
        lines.push(Line::from(spans));
    }
    // The scale: its least value, its steps, and its greatest.
    let mut scale = vec![
        pad(label_cells + gap),
        Span::styled(format!("{}{unit}", short(lo)), dim()),
        pad(1),
    ];
    if plain {
        scale.extend(shades.map(|shade| Span::raw(shade.repeat(2))));
    } else {
        for step in 0..SCALE_STEPS {
            let t = step as f64 / (SCALE_STEPS - 1) as f64;
            scale.push(Span::styled("  ", Style::new().bg(heat(t, look.tones).0)));
        }
    }
    scale.push(pad(1));
    scale.push(Span::styled(format!("{}{unit}", short(hi)), dim()));
    lines.push(Line::from(scale));
    lines
}
