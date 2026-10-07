//! Draws [`State`] into a Ratatui frame. Rows from the top: body, gap, notice,
//! the bar (activity and status), the composer box (top edge, draft rows,
//! bottom edge), and the sample-agents row, in the heights
//! [`crate::layout::plan`] grants them.

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Position, Rect};
use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::terminal::Frame;
use ratatui_core::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::activity::{self, Hue, Tones};
use crate::composer::{self, Edge};
use crate::copy;
use crate::editor::{self, display};
use crate::frame::FrameStyle;
use crate::layout::{self, Needs};
use crate::mode::{self, HINT_MIN_COLUMNS};
use crate::state::{
    DraftSpot, Focus, Notice, Outcome, SAMPLE_AGENTS, SampleAgent, SampleKind, ScrollTrack, Spot,
    State, agent,
};
use crate::status::{self, Tone};
use crate::transcript::{self, Look, TagTone};

/// Cells between the terminal's edges and rows drawn outside the box, so
/// they align with the box's contents.
const INSET: u16 = 2;

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

fn accent() -> Style {
    Style::new().fg(Color::Cyan)
}

/// Draws one frame. The only state it changes is the composer window, which
/// follows the caret within the rows this frame grants it.
pub fn render(app: &mut State, frame: &mut Frame) {
    let area = frame.area();
    if area.is_empty() {
        return;
    }
    let geometry = composer::geometry(area.width);
    let draft = editor::layout(app.draft.text(), app.draft.caret(), geometry.wrap_width);
    let rows = layout::plan(
        area.height,
        Needs {
            draft_rows: u16::try_from(draft.rows.len()).unwrap_or(u16::MAX),
            standing: true,
            notice: u16::from(app.notice.is_some() || app.quitting()),
        },
    );

    let mut y = area.y;
    let mut next = |height: u16| {
        let rect = Rect::new(area.x, y, area.width, height);
        y += height;
        rect
    };
    let body = next(rows.body);
    let gap = next(rows.gap);
    let notice = next(rows.notice);
    let bar = next(rows.bar);
    let rule = next(rows.top_edge);
    let band = next(rows.composer);
    let base_rule = next(rows.bottom_edge);
    let agents = next(rows.standing);
    // Rows outside the box start and end where its contents do.
    let inset = |r: Rect| {
        if geometry.boxed {
            Rect::new(r.x + INSET, r.y, r.width - 2 * INSET, r.height)
        } else {
            r
        }
    };

    let buf = frame.buffer_mut();
    app.latest = None;
    render_body(app, body, gap, buf);
    // The prompt to press Ctrl+C again takes the notice's row while it is
    // armed; the notice returns when it lapses.
    if app.quitting() {
        line(
            buf,
            inset(notice),
            Line::styled(copy::QUIT, tone_style(Tone::Waiting, app.tones)),
        );
    } else if let Some(kind) = app.notice {
        let text = match kind {
            Notice::NoModel => copy::NO_MODEL,
            Notice::ReadOnly => copy::READ_ONLY,
            Notice::ListKeys => copy::LIST_KEYS,
            Notice::DraftLimit => copy::DRAFT_LIMIT,
        };
        line(
            buf,
            inset(notice),
            Line::styled(text, Style::new().fg(Color::Yellow)),
        );
    }
    let columns = area.width;
    render_bar(app, inset(bar), buf);
    render_agents_row(columns, inset(agents), buf);

    let visible = usize::from(band.height);
    let total = draft.rows.len();
    let top = app
        .window
        .follow(draft.caret.0, visible, total, geometry.wrap_width);
    let above = top;
    let below = total.saturating_sub(top + visible);
    app.draft_spot = (band.height > 0).then_some(DraftSpot {
        top_row: band.y,
        rows: band.height,
        text_x: band.x + geometry.text_x,
        first: top,
        width: geometry.wrap_width,
    });

    // The box closes only when both edges have rows; otherwise the edges that
    // fit are plain rules. Hidden-row counts ride the edges, so they never
    // take a column from the draft.
    let glyphs = app.frame.glyphs();
    let closed = geometry.boxed && rule.height > 0 && base_rule.height > 0;
    let above_label = (above > 0).then(|| format!("+{above} {}", copy::ABOVE));
    // The sample-agent list holds focus outside the composer, so the
    // composer names no key while it is open.
    let listing = app.focus == Focus::AgentList;
    let mode = mode::view(app.mode(), !app.draft.is_empty(), area.width);
    let below_label = if below > 0 {
        Some(format!("+{below} {}", copy::BELOW))
    } else {
        mode.hint.filter(|_| !listing).map(str::to_owned)
    };
    composer::edge(buf, rule, glyphs, Edge::Top, closed, above_label.as_deref());
    composer::edge(
        buf,
        base_rule,
        glyphs,
        Edge::Bottom,
        closed,
        below_label.as_deref(),
    );

    let editing = app.focus == Focus::Composer;
    let quiet = mode.dim || listing;
    let text_style = if quiet { dim() } else { Style::new() };
    let prompt_style = if quiet { dim() } else { accent() };
    let limit = geometry.wrap_width.saturating_sub(1).max(1);
    let text_x = band.x + geometry.text_x;
    let text_width = u16::try_from(limit)
        .unwrap_or(u16::MAX)
        .min(band.width.saturating_sub(geometry.text_x));
    for i in 0..visible {
        let y = band.y + i as u16;
        if closed {
            buf.set_string(band.x, y, glyphs.vertical, dim());
            buf.set_string(band.right() - 1, y, glyphs.vertical, dim());
        }
        // The prompt keeps the draft's first row; `^` and `v` mark rows the
        // window hides above and below.
        if let Some(px) = geometry.prompt_x {
            let marker = if top + i == 0 {
                Some((glyphs.prompt, prompt_style))
            } else if i == 0 && above > 0 {
                Some(("^", dim()))
            } else if i + 1 == visible && below > 0 {
                Some(("v", dim()))
            } else {
                None
            };
            if let Some((marker, style)) = marker {
                buf.set_string(band.x + px, y, marker, style);
            }
        }
        if let Some(row) = draft.rows.get(top + i) {
            let text = display(&app.draft.text()[row.start..row.end], limit);
            line(
                buf,
                Rect::new(text_x, y, text_width, 1),
                Line::styled(text, text_style),
            );
        }
    }
    if visible > 0 && app.draft.is_empty() {
        line(
            buf,
            Rect::new(text_x, band.y, text_width, 1),
            Line::styled(
                mode::placeholder(mode.placeholder, area.width, usize::from(text_width)),
                dim(),
            ),
        );
    }
    if editing && visible > 0 {
        let (row, col) = draft.caret;
        let x = text_x
            .saturating_add(u16::try_from(col).unwrap_or(u16::MAX))
            .min(area.right() - 1);
        frame.set_cursor_position(Position::new(x, band.y + (row - top) as u16));
    }
}

fn line(buf: &mut Buffer, area: Rect, content: Line) {
    if !area.is_empty() {
        buf.set_line(area.x, area.y, &content, area.width);
    }
}

/// Whether a row's key hint still draws at its right edge: never on a
/// terminal narrower than [`HINT_MIN_COLUMNS`], as the composer's hint, and
/// only whole, two cells clear of the row's own text. The key it names still
/// works unnamed. Ports the TypeScript `tailFits`.
pub fn tail_fits(columns: u16, row: usize, fixed: usize, tail: &str) -> bool {
    columns >= HINT_MIN_COLUMNS && fixed + 2 + tail.width() <= row
}

/// Left text, then a key hint at the right edge where [`tail_fits`] allows.
fn split_row(buf: &mut Buffer, columns: u16, area: Rect, left: Line, right: Line) {
    if area.is_empty() {
        return;
    }
    let width = usize::from(area.width);
    let (lw, rw) = (left.width(), right.width());
    line(buf, area, left);
    if tail_fits(columns, width, lw, &right.to_string()) {
        let x = area.x + (width - rw) as u16;
        line(buf, Rect::new(x, area.y, rw as u16, 1), right);
    }
}

/// The bar above the composer: what the session is doing on the left, and
/// the status fields right-aligned beside it.
///
/// The left side is the sample's word with its phase and time, a finished
/// turn's outcome with its time, or what the list or inspection shows. Its
/// head is never cut; its tail gives way first, then status fields by rank,
/// then the status line whole. Right-aligned, the status does not move when
/// the left side appears or changes.
fn render_bar(app: &State, area: Rect, buf: &mut Buffer) {
    if area.is_empty() {
        return;
    }
    let (head, tail) = bar_left(app);
    let fields = status::fields(&app.status);
    let width = usize::from(area.width);
    let head_width = Line::from(head.clone()).width();
    let tail_width = Line::from(tail.clone()).width();
    let mut levels = vec![(head_width + tail_width, true)];
    if tail_width > 0 {
        levels.push((head_width, false));
    }
    for (left_width, with_tail) in levels {
        let room = width.saturating_sub(left_width + if left_width > 0 { 2 } else { 0 });
        let fitted = status::fit(&fields, room);
        let status_width = status::fitted_width(&fitted);
        if status_width > room || left_width > width {
            continue;
        }
        let mut spans = head.clone();
        if with_tail {
            spans.extend(tail);
        }
        line(buf, area, Line::from(spans));
        let mut right = Vec::new();
        for (i, field) in fitted.iter().enumerate() {
            if i > 0 {
                right.push(Span::raw(" ".repeat(status::FIELD_GAP)));
            }
            for part in status::drawn(field) {
                right.push(Span::styled(part.text, tone_style(part.tone, app.tones)));
            }
        }
        let x = area.x + (width - status_width) as u16;
        line(
            buf,
            Rect::new(x, area.y, status_width as u16, 1),
            Line::from(right),
        );
        return;
    }
    // Not even the head beside the narrowest status: the head alone.
    line(buf, area, Line::from(head));
}

/// The bar's left side as a head that stays and a tail that gives way.
fn bar_left(app: &State) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    match app.focus {
        Focus::Composer => match (app.activity, app.summary) {
            (Some(sample), _) => {
                let elapsed = app.now.saturating_sub(sample.started);
                let (hue, phases) = match sample.kind {
                    SampleKind::Turn => (Hue::Running, copy::SAMPLE_PHASES),
                    SampleKind::Compaction => (Hue::Compacting, copy::COMPACTING_PHASES),
                };
                let step = elapsed.as_secs() / copy::PHASE_SECONDS;
                let phase = phases[step as usize % phases.len()];
                let word = format!("{}…", sample.word);
                let head =
                    activity::shimmer_spans(&word, elapsed, app.tones, hue, app.tones.moves());
                let details = format!("  {phase} · {}", activity::format_elapsed(elapsed));
                (head, vec![Span::styled(details, dim())])
            }
            (None, Some(summary)) => {
                let (head, color) = match summary.outcome {
                    Outcome::Completed => (format!("✓ {}", copy::COMPLETED), Color::Green),
                    Outcome::Interrupted => (format!("■ {}", copy::INTERRUPTED), Color::Yellow),
                };
                let details = format!("  {}", activity::format_elapsed(summary.elapsed));
                (
                    vec![Span::styled(head, Style::new().fg(color).bold())],
                    vec![Span::styled(details, dim())],
                )
            }
            (None, None) => (Vec::new(), Vec::new()),
        },
        Focus::AgentList => (
            vec![Span::styled(
                copy::AGENTS,
                Style::new().add_modifier(Modifier::BOLD),
            )],
            vec![Span::styled(format!("  {}", copy::HEADER_LIST_HINT), dim())],
        ),
        // A breadcrumb, as a catalogue's: where it came from, then where it is.
        Focus::Inspect(id) => (
            vec![
                Span::styled(copy::AGENTS, accent().add_modifier(Modifier::UNDERLINED)),
                Span::styled(" / ", dim()),
                Span::styled(
                    agent(id).map_or(id, |a| a.name),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
            ],
            vec![Span::styled(format!("  {}", copy::INSPECT_KEYS), dim())],
        ),
    }
}

/// The agents' standing row: `Agents  2 samples · none running`, with the
/// key that opens them at the right.
fn render_agents_row(columns: u16, area: Rect, buf: &mut Buffer) {
    split_row(
        buf,
        columns,
        area,
        Line::from(vec![
            Span::styled(copy::AGENTS, Style::new().add_modifier(Modifier::BOLD)),
            Span::styled(
                format!("  {} {}", SAMPLE_AGENTS.len(), copy::AGENTS_SUMMARY),
                dim(),
            ),
        ]),
        Line::styled(copy::AGENTS_KEY, dim()),
    );
}

/// A status tone in the palette's colour on a truecolor terminal, the nearest
/// ANSI colour otherwise, and without colour under `NO_COLOR`.
fn tone_style(tone: Tone, tones: Tones) -> Style {
    let (rgb, ansi) = match tone {
        Tone::Plain => return Style::new(),
        Tone::Dim => return dim(),
        Tone::Waiting => ((0xea, 0xb3, 0x08), Color::Yellow),
        Tone::Asking => ((0x0e, 0xa5, 0xe9), Color::LightBlue),
        Tone::Hot | Tone::Ramp(2) => ((0xfb, 0x92, 0x3c), Color::LightRed),
        Tone::Max => ((0xf4, 0x72, 0xb6), Color::LightMagenta),
        Tone::Ramp(0) => ((0xfd, 0xe6, 0x8a), Color::LightYellow),
        Tone::Ramp(1) => ((0xfa, 0xcc, 0x15), Color::Yellow),
        Tone::Ramp(_) => ((0xf8, 0x71, 0x71), Color::Red),
    };
    match tones {
        Tones::TrueColor => Style::new().fg(Color::Rgb(rgb.0, rgb.1, rgb.2)),
        Tones::Ansi => Style::new().fg(ansi),
        Tones::None => Style::new(),
    }
}

/// The transcript viewport, with a hint row at its foot when there is room:
/// right-aligned keys while following output, and the way back to the
/// newest line, leading the row, while reading history.
/// The transcript viewport and its scroll indicator. The indicator takes
/// the gap row just above the controls, so it reads with the bar and the
/// box rather than over the text. The planner grants the gap before any
/// viewport row, so a viewport always has one under it.
fn render_transcript(app: &mut State, area: Rect, gap: Rect, buf: &mut Buffer) {
    let rows = area.height;
    let hint = (gap.height > 0).then_some(gap);
    // The last column is the scrollbar's, whether or not it is drawn, so
    // the text never reflows when the history outgrows the viewport.
    let bar = area.width > SCROLLBAR_MIN_WIDTH;
    let width = area.width - u16::from(bar);
    let classic = app.frame == FrameStyle::Classic;
    let view = &mut app.transcript;
    view.resize(
        usize::from(width),
        usize::from(rows),
        Look {
            tones: app.tones,
            classic,
            lit: transcript::lit(app.now),
        },
    );
    for (i, content) in view.visible().into_iter().enumerate() {
        line(buf, Rect::new(area.x, area.y + i as u16, width, 1), content);
    }
    let thumb = bar.then(|| view.thumb(usize::from(rows))).flatten();
    app.track = thumb.map(|_| ScrollTrack {
        column: area.x + width,
        top: area.y,
        rows,
    });
    if let Some(thumb) = thumb {
        let (glyphs, styles) = scrollbar_look(app.tones, classic, app.dragging);
        for i in 0..usize::from(rows) {
            let on = (thumb.start..thumb.start + thumb.len).contains(&i);
            let (glyph, style) = if on {
                (glyphs.0, styles.0)
            } else {
                (glyphs.1, styles.1)
            };
            buf[(area.x + width, area.y + i as u16)]
                .set_symbol(glyph)
                .set_style(style);
        }
    }
    if let Some(row) = hint {
        render_scroll_hint(app, Rect::new(row.x, row.y, width, 1), buf);
    }
}

/// The scroll indicator. While following output, the keys that scroll,
/// right-aligned and quiet. While reading, a pill centred under the text:
/// `↓ 12 lines below · Ctrl+End`, or `↓ New output · …` in yellow once
/// output arrives below. The pill gives up its key, then its count, before
/// it would be cut. Pressing it follows output again.
fn render_scroll_hint(app: &mut State, row: Rect, buf: &mut Buffer) {
    let view = &app.transcript;
    if view.following() {
        if !view.more_above() {
            return;
        }
        let mut spans = Vec::new();
        for (i, (key, does)) in copy::HINT_FOLLOWING.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", dim()));
            }
            spans.push(Span::raw(*key));
            spans.push(Span::styled(format!(" {does}"), dim()));
        }
        let hint = Line::from(spans);
        let cells = hint.width() as u16;
        if cells <= row.width {
            line(
                buf,
                Rect::new(row.x + row.width - cells, row.y, cells, 1),
                hint,
            );
        }
        return;
    }
    let below = view.total().saturating_sub(view.offset() + view.height);
    let fresh = view.new_below();
    let Some(pill) = scroll_pill(below, fresh, app.tones, usize::from(row.width)) else {
        return;
    };
    let cells = pill.width() as u16;
    let x = row.x + (row.width - cells) / 2;
    line(buf, Rect::new(x, row.y, cells, 1), pill);
    app.latest = Some(Spot {
        column: x,
        row: row.y,
        width: cells,
    });
}

/// The reading pill at its widest that fits `width`, padded a cell each side.
fn scroll_pill(below: usize, fresh: bool, tones: Tones, width: usize) -> Option<Line<'static>> {
    let (text, key, back) = match tones {
        Tones::TrueColor if fresh => (
            Style::new().fg(Color::Rgb(0xfd, 0xe6, 0x8a)),
            Style::new().fg(Color::Rgb(0xca, 0xb8, 0x6a)),
            Some(Color::Rgb(0x3a, 0x34, 0x16)),
        ),
        Tones::TrueColor => (
            Style::new().fg(Color::Rgb(0x7d, 0xd3, 0xfc)),
            Style::new().fg(Color::Rgb(0x9c, 0xa3, 0xaf)),
            Some(Color::Rgb(0x25, 0x28, 0x2f)),
        ),
        Tones::Ansi => (
            Style::new().fg(if fresh { Color::Yellow } else { Color::Cyan }),
            dim(),
            None,
        ),
        Tones::None => (Style::new().add_modifier(Modifier::BOLD), dim(), None),
    };
    let count = format!("{below} {}", copy::LINES_BELOW);
    let key_tail = Some(format!(" · {}", copy::LATEST_KEY));
    // Widest first: the key goes, then the count.
    let candidates: Vec<(String, Option<String>)> = if fresh {
        let new = format!("↓ {}", copy::NEW_OUTPUT);
        vec![
            (format!("{new} · {count}"), key_tail.clone()),
            (format!("{new} · {count}"), None),
            (new.clone(), key_tail),
            (new, None),
        ]
    } else {
        vec![
            (format!("↓ {count}"), key_tail),
            (format!("↓ {count}"), None),
            (format!("↓ {below}"), None),
        ]
    };
    for (head, tail) in candidates {
        let cells = 2 + head.width() + tail.as_ref().map_or(0, |t| t.width());
        if cells > width {
            continue;
        }
        let paint = |style: Style| back.map_or(style, |bg| style.bg(bg));
        let mut spans = vec![Span::styled(format!(" {head}"), paint(text))];
        if let Some(tail) = tail {
            spans.push(Span::styled(tail, paint(key)));
        }
        spans.push(Span::styled(" ", paint(text)));
        return Some(Line::from(spans));
    }
    None
}

/// Columns the transcript needs before it gives one to the scrollbar.
const SCROLLBAR_MIN_WIDTH: u16 = 20;

/// The scrollbar's glyphs and styles, thumb then track. The thumb is a heavy
/// rule on a thin one, grey on dark grey, and accented while dragged; under
/// `NO_COLOR` the thumb is bold and the track dim; the classic frame draws
/// ASCII.
fn scrollbar_look(
    tones: Tones,
    classic: bool,
    dragging: bool,
) -> ((&'static str, &'static str), (Style, Style)) {
    let glyphs = if classic { ("#", "|") } else { ("┃", "│") };
    let styles = match tones {
        Tones::TrueColor => (
            Style::new().fg(if dragging {
                Color::Rgb(0x0e, 0xa5, 0xe9)
            } else {
                Color::Rgb(0x9c, 0xa3, 0xaf)
            }),
            Style::new().fg(Color::Rgb(0x37, 0x41, 0x51)),
        ),
        Tones::Ansi => (
            Style::new().fg(if dragging { Color::Cyan } else { Color::Gray }),
            Style::new().fg(Color::DarkGray),
        ),
        Tones::None => (Style::new().add_modifier(Modifier::BOLD), dim()),
    };
    (glyphs, styles)
}

fn render_body(app: &mut State, area: Rect, gap: Rect, buf: &mut Buffer) {
    if area.is_empty() {
        return;
    }
    let width = usize::from(area.width);
    let classic = app.frame == FrameStyle::Classic;
    let lines = match app.focus {
        Focus::Composer => return render_transcript(app, area, gap, buf),
        Focus::AgentList => agent_list(app.selected, width, classic, app.tones),
        Focus::Inspect(id) => inspection(
            agent(id).unwrap_or(&SAMPLE_AGENTS[0]),
            width,
            Look {
                tones: app.tones,
                classic,
                lit: true,
            },
        ),
    };
    // Anchored to the body's foot, beside the controls, as the transcript is.
    let skip = lines.len().saturating_sub(usize::from(area.height));
    let top = area.y + area.height - (lines.len() - skip) as u16;
    for (i, content) in lines.into_iter().skip(skip).enumerate() {
        line(
            buf,
            Rect::new(area.x, top + i as u16, area.width, 1),
            content,
        );
    }
}

/// `text` wrapped at `width` less `indent`, every row indented.
fn indented(text: &str, width: usize, indent: usize, style: Style) -> Vec<Line<'static>> {
    transcript::wrap(text, width.saturating_sub(indent))
        .into_iter()
        .map(|row| {
            Line::from(vec![
                Span::raw(" ".repeat(indent)),
                Span::styled(row, style),
            ])
        })
        .collect()
}

/// `lead`, then an id as a blue tag two cells after it, when it fits.
fn titled(lead: Vec<Span<'static>>, id: &str, width: usize, tones: Tones) -> Line<'static> {
    let mut line = Line::from(lead);
    let tag = transcript::tag(id, TagTone::Blue, tones);
    if line.width() + 2 + tag.width() <= width {
        line.spans.push(Span::raw("  "));
        line.spans.push(tag);
    }
    line
}

/// The agents as cards: a marker on the selected one, its name and id tag,
/// and the first line of what it does under it.
fn agent_list(selected: &str, width: usize, classic: bool, tones: Tones) -> Vec<Line<'static>> {
    let marker = if classic { ">" } else { "▸" };
    let mut lines = indented(copy::LIST_SUBTITLE, width, INSET.into(), dim());
    for agent in SAMPLE_AGENTS {
        let chosen = agent.id == selected;
        let (mark, name) = if chosen {
            (
                Span::styled(format!("{marker} "), accent()),
                Span::styled(agent.name, accent().add_modifier(Modifier::BOLD)),
            )
        } else {
            (Span::raw("  "), Span::styled(agent.name, Style::new()))
        };
        lines.push(Line::default());
        lines.push(titled(
            vec![Span::raw("  "), mark, name],
            agent.id,
            width,
            tones,
        ));
        let about = agent.detail.first().copied().unwrap_or_default();
        lines.extend(indented(
            about,
            width,
            4,
            if chosen { Style::new() } else { dim() },
        ));
    }
    lines
}

/// Cells an inspection ledger's label column takes, its gap included.
const LEDGER_LABEL: usize = 8;

/// One agent, read only, as a ledger: its name, a dotted rule, then a label
/// and a value per row, the value wrapped under itself.
fn inspection(agent: &SampleAgent, width: usize, look: Look) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
            Span::raw("  "),
            Span::styled(agent.name, accent().add_modifier(Modifier::BOLD)),
        ]),
        transcript::rule(width, look),
    ];
    let indent = usize::from(INSET) + LEDGER_LABEL;
    let labelled = |label: &str, cells: usize, value: Vec<Span<'static>>| {
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(format!("{label:<cells$}"), dim()),
        ];
        spans.extend(value);
        Line::from(spans)
    };
    let row = |label: &str, value| labelled(label, LEDGER_LABEL, value);
    // A tag's padding takes the cell before its column, so its text lines
    // up with the other values.
    lines.push(labelled(
        copy::LEDGER_ID,
        LEDGER_LABEL - 1,
        vec![transcript::tag(agent.id, TagTone::Blue, look.tones)],
    ));
    let text = |label: &str, value: &str, style: Style| -> Vec<Line<'static>> {
        transcript::wrap(value, width.saturating_sub(indent))
            .into_iter()
            .enumerate()
            .map(|(i, part)| {
                let value = vec![Span::styled(part, style)];
                if i == 0 {
                    row(label, value)
                } else {
                    let mut spans = vec![Span::raw(" ".repeat(indent))];
                    spans.extend(value);
                    Line::from(spans)
                }
            })
            .collect()
    };
    let about = agent.detail.first().copied().unwrap_or_default();
    lines.extend(text(copy::LEDGER_ROLE, about, Style::new()));
    lines.extend(text(
        copy::LEDGER_STATE,
        copy::LEDGER_STATE_VALUE,
        Style::new(),
    ));
    lines.extend(text(
        copy::LEDGER_INPUT,
        copy::INSPECT_READ_ONLY,
        Style::new().fg(Color::Yellow),
    ));
    lines.extend(text(copy::LEDGER_DRAFT, copy::INSPECT_PARENT, dim()));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui_core::backend::TestBackend;
    use ratatui_core::terminal::Terminal;

    use crate::activity::Tones;
    use crate::frame::FrameStyle;
    use crate::keys::{Key, KeyInput, Mods};
    use std::time::Duration;

    use crate::state::{Mouse, MouseKind, Msg, QUIT_WINDOW, SampleActivity, update};

    /// Columns where boxed draft text starts.
    const TEXT_X: u16 = 4;

    /// Draws `app` on a fresh screen. The composer window persists in `app`
    /// from one draw to the next, as it does between frames.
    fn draw(app: &mut State, width: u16, height: u16) -> (Vec<String>, Position) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(app, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        // A wide symbol's trailing cells are blank fillers; skip them so each
        // row string has the screen's width.
        let rows = (0..height)
            .map(|y| {
                let mut row = String::new();
                let mut x = 0;
                while x < width {
                    let symbol = buffer[(x, y)].symbol();
                    row.push_str(symbol);
                    x += editor::cell_width(symbol).max(1) as u16;
                }
                row
            })
            .collect();
        (rows, terminal.get_cursor_position().unwrap())
    }

    fn key(app: &mut State, key: Key) {
        update(app, Msg::Key(KeyInput::plain(key)));
    }

    fn row_index(rows: &[String], needle: &str) -> usize {
        rows.iter()
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} not found in {rows:#?}"))
    }

    fn lines(count: usize) -> State {
        let mut app = State::default();
        for i in 0..count {
            if i > 0 {
                app.draft.newline();
            }
            app.draft.type_text(&format!("line{i}"));
        }
        app
    }

    #[test]
    fn first_screen_names_the_preview_and_sample_agents() {
        for (w, h) in [(80, 24), (120, 36)] {
            let (rows, cursor) = draw(&mut State::default(), w, h);
            // The transcript follows the sample session's newest line.
            assert!(
                rows.iter()
                    .any(|r| r.contains("rejects it before splitting."))
            );
            assert!(rows.iter().any(|r| r.contains("Agents  2 samples")));
            let prompt = row_index(&rows, "│ ❯ Type a draft");
            assert_eq!(cursor, Position::new(TEXT_X, prompt as u16));
            // Idle, the bar above the box holds the status alone, right-aligned.
            assert!(rows[prompt - 2].ends_with("no model  "));
            assert!(rows[prompt - 2].starts_with("    "));
            assert!(rows[usize::from(h) - 1].contains("Agents  2 samples"));
        }
    }

    #[test]
    fn the_composer_is_a_closed_box_aligned_with_the_rows_around_it() {
        let mut app = State::default();
        app.draft.type_text("hello");
        let (rows, _) = draw(&mut app, 40, 12);
        let prompt = row_index(&rows, "❯ hello");
        assert_eq!(rows[prompt - 1], format!("╭{}╮", "─".repeat(38)));
        assert_eq!(rows[prompt], format!("│ ❯ hello{}│", " ".repeat(30)));
        assert_eq!(rows[prompt + 1], format!("╰{}╯", "─".repeat(38)));
        // The bar and the agents row align with the box's contents.
        assert_eq!(rows[prompt - 2], format!("{}no model  ", " ".repeat(30)));
        assert!(rows[prompt + 2].starts_with("  Agents  2 samples · none running"));
        // Right-hand text ends where the box's contents do.
        let (rows, _) = draw(&mut app, 80, 24);
        let prompt = row_index(&rows, "❯ hello");
        assert!(rows[prompt + 2].ends_with("Ctrl+G  "));
    }

    #[test]
    fn classic_frames_draw_ascii_edges_and_prompt() {
        let mut app = State::default();
        app.draft.type_text("hi");
        app.frame = FrameStyle::Classic;
        let (rows, _) = draw(&mut app, 20, 12);
        let prompt = row_index(&rows, "| > hi");
        assert_eq!(rows[prompt - 1], format!("+{}+", "-".repeat(18)));
        assert!(rows[prompt].ends_with(" |"));
        assert_eq!(rows[prompt + 1], format!("+{}+", "-".repeat(18)));
    }

    #[test]
    fn refused_submission_shows_the_notice_above_the_bar() {
        let mut app = State::default();
        app.draft.type_text("keep me");
        key(&mut app, Key::Enter);
        let (rows, cursor) = draw(&mut app, 80, 24);
        let notice = row_index(&rows, "Model connection is not available");
        let bar = row_index(&rows, "no model");
        let prompt = row_index(&rows, "❯ keep me");
        assert!(notice < bar && bar + 2 == prompt);
        assert_eq!(cursor, Position::new(TEXT_X + 7, prompt as u16));
    }

    #[test]
    fn long_narrow_draft_keeps_the_caret_on_its_last_visible_row() {
        let mut app = State::default();
        app.draft.paste(&"界e\u{301}".repeat(60));
        app.draft.newline();
        app.draft.type_text("end");
        let (rows, cursor) = draw(&mut app, 40, 12);
        let base_rule = rows.len() - 2;
        assert_eq!(usize::from(cursor.y), base_rule - 1);
        assert!(rows[base_rule - 1].contains("end"));
        assert_eq!(cursor.x, TEXT_X + 3);
        // The top edge counts the rows above the window.
        assert!(rows[row_index(&rows, "above")].starts_with('╭'));
    }

    #[test]
    fn the_window_holds_still_while_the_caret_moves_inside_it() {
        let mut app = lines(10);
        let (rows, _) = draw(&mut app, 80, 24);
        assert!(rows.iter().any(|r| r.contains("line5")));
        assert!(!rows.iter().any(|r| r.contains("line4")));
        assert!(rows[row_index(&rows, "line5")].contains("│ ^ line5"));
        assert!(rows.iter().any(|r| r.contains("+5 above")));
        // Up to line8, then line5: the text stays put and the caret moves.
        for _ in 0..2 {
            key(&mut app, Key::Home);
            key(&mut app, Key::Left);
        }
        let (rows, cursor) = draw(&mut app, 80, 24);
        assert!(!rows.iter().any(|r| r.contains("line4")));
        assert_eq!(usize::from(cursor.y), row_index(&rows, "line7"));
        for _ in 0..3 {
            key(&mut app, Key::Home);
            key(&mut app, Key::Left);
        }
        let (rows, cursor) = draw(&mut app, 80, 24);
        assert_eq!(usize::from(cursor.y), row_index(&rows, "line4"));
        assert!(rows.iter().any(|r| r.contains("+1 below")));
        assert!(rows[row_index(&rows, "line8")].contains("v line8"));
    }

    #[test]
    fn the_caret_never_changes_the_composer_height() {
        // Exactly one full row of text at 40 columns: 34 cells.
        let mut app = State::default();
        app.draft.type_text(&"x".repeat(34));
        let (at_end, end_cursor) = draw(&mut app, 40, 12);
        key(&mut app, Key::Home);
        let (at_start, start_cursor) = draw(&mut app, 40, 12);
        assert_eq!(at_end, at_start);
        assert_eq!(end_cursor.y, start_cursor.y);
        // The caret after a full row sits on the padding cell, inside the box.
        assert_eq!(end_cursor.x, 38);
    }

    #[test]
    fn resizing_rewraps_the_draft_and_keeps_the_caret_in_the_box() {
        let mut app = State::default();
        app.draft.paste(&"word ".repeat(80));
        for (w, h) in [(80, 24), (30, 10), (12, 8), (11, 8), (120, 40), (40, 12)] {
            let (rows, cursor) = draw(&mut app, w, h);
            assert!(cursor.x < w && cursor.y < h, "{w}x{h}");
            assert!(rows.iter().all(|r| r.width() <= usize::from(w)), "{w}x{h}");
            let caret_row = &rows[usize::from(cursor.y)];
            assert!(caret_row.contains("word"), "{w}x{h}: {rows:#?}");
            if w >= composer::BOX_MIN_WIDTH && h >= 8 {
                assert!(
                    caret_row.starts_with('│') && caret_row.ends_with('│'),
                    "{w}x{h}"
                );
            }
        }
    }

    #[test]
    fn tall_terminals_show_more_draft_rows() {
        let mut app = lines(20);
        for (height, shown) in [(24, 5), (40, 8), (60, 12)] {
            let (rows, _) = draw(&mut app, 80, height);
            let count = rows
                .iter()
                .filter(|r| r.starts_with('│') && r.contains("line"))
                .count();
            assert_eq!(count, shown, "{height} rows");
        }
    }

    #[test]
    fn inspector_says_read_only_and_hides_the_caret_from_the_draft() {
        let mut app = State::default();
        app.draft.type_text("draft");
        update(
            &mut app,
            Msg::Key(KeyInput::new(Key::Char('g'), Mods::CTRL)),
        );
        key(&mut app, Key::Enter);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(&mut app, f)).unwrap();
        assert!(!terminal.backend().cursor_visible());
        let (rows, _) = draw(&mut app, 80, 24);
        // The bar is a breadcrumb back to the list.
        assert!(
            rows.iter()
                .any(|r| r.starts_with("  Agents / Sample explorer  Tab agents · Esc draft"))
        );
        // The body is a ledger under the agent's name and a dotted rule.
        let name = rows
            .iter()
            .position(|r| r.trim_end() == "  Sample explorer")
            .unwrap();
        assert!(rows[name + 1].starts_with("  ┄┄┄"));
        let ledger: Vec<&str> = rows[name + 2..name + 7]
            .iter()
            .map(|r| r.trim_end())
            .collect();
        assert_eq!(
            ledger,
            [
                "  Id      sample-explorer",
                "  Role    Example of a child that would read files for its parent.",
                "  State   Sample · not running",
                "  Input   Read only · typing never reaches an agent or your draft",
                "  Draft   Kept unchanged while you inspect",
            ]
        );
        assert!(rows.iter().any(|r| r.contains("❯ draft")));
        assert!(
            rows.iter()
                .any(|r| r.starts_with('╰') && r.contains("draft kept · Esc returns"))
        );
    }

    fn sampling(word: &'static str) -> State {
        let mut app = State::default();
        app.activity = Some(SampleActivity {
            kind: SampleKind::Turn,
            word,
            started: Duration::from_secs(1),
        });
        app
    }

    /// The header's cell styles at `now`.
    fn header_styles(app: &mut State, tones: Tones, now: Duration) -> Vec<Style> {
        app.tones = tones;
        app.now = now;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(app, f)).unwrap();
        let (rows, _) = draw(app, 80, 24);
        let y = row_index(&rows, "Kneading…") as u16;
        let buffer = terminal.backend().buffer();
        (2..11).map(|x| buffer[(x, y)].style()).collect()
    }

    #[test]
    fn sample_activity_is_text_with_its_phase_and_time() {
        let mut app = sampling("Kneading");
        app.now = Duration::from_millis(6_500);
        let (rows, _) = draw(&mut app, 80, 24);
        let header = row_index(&rows, "Kneading…");
        // The activity and the status share the bar above the box.
        assert!(rows[header].starts_with("  Kneading…  writing · 5s"));
        assert!(rows[header].ends_with("  no model  "));
        assert_eq!(row_index(&rows, "❯") - 2, header);
        assert!(
            rows.iter()
                .any(|r| r.starts_with('╰') && r.contains(" Esc interrupts "))
        );
        // No glyph stands for the activity: the words carry it.
        let braille = |c: char| ('\u{2800}'..='\u{28FF}').contains(&c);
        assert!(!rows.iter().any(|r| r.chars().any(braille)));
        // Narrow, the phase and time give way before the status, which goes
        // before the word.
        let (rows, _) = draw(&mut app, 30, 12);
        assert_eq!(
            rows[row_index(&rows, "Kneading…")].trim_end(),
            "  Kneading…         no model"
        );
        let (rows, _) = draw(&mut app, 20, 12);
        assert_eq!(
            rows[row_index(&rows, "Kneading…")].trim_end(),
            "  Kneading…"
        );
    }

    #[test]
    fn the_shimmer_moves_across_the_word_unless_color_is_off() {
        let mut app = sampling("Kneading");
        let early = header_styles(
            &mut app,
            Tones::TrueColor,
            Duration::from_secs(1) + activity::BEAT * 3,
        );
        let later = header_styles(
            &mut app,
            Tones::TrueColor,
            Duration::from_secs(1) + activity::BEAT * 7,
        );
        assert_ne!(early, later);
        let brightest = |styles: &[Style]| {
            styles
                .iter()
                .position(|s| s.fg == Tones::TrueColor.style(Hue::Running, 1.0).fg)
        };
        assert!(brightest(&early) < brightest(&later));
        let still = header_styles(
            &mut app,
            Tones::None,
            Duration::from_secs(1) + activity::BEAT * 3,
        );
        let still_later = header_styles(
            &mut app,
            Tones::None,
            Duration::from_secs(1) + activity::BEAT * 7,
        );
        assert_eq!(still, still_later);
    }

    #[test]
    fn agent_list_marks_the_selected_row() {
        let mut app = State::default();
        update(
            &mut app,
            Msg::Key(KeyInput::new(Key::Char('g'), Mods::CTRL)),
        );
        key(&mut app, Key::Down);
        let (rows, _) = draw(&mut app, 60, 24);
        let at = |needle| row_index(&rows, needle);
        assert_eq!(
            rows[at("Sample reviewer")].trim_end(),
            "  ▸ Sample reviewer   sample-reviewer"
        );
        assert!(rows[at("Sample explorer")].starts_with("    Sample explorer"));
        // Each card says what the agent does; a blank separates the cards.
        assert!(
            rows[at("Sample reviewer") + 1].starts_with("    Example of a child that would review")
        );
        assert_eq!(rows[at("Sample reviewer") - 1].trim(), "");
        assert!(rows[at("Fixed examples")].starts_with("  Fixed examples · nothing is running"));
        assert!(rows.iter().any(|r| r.starts_with("  Agents  ↑↓ select")));
    }

    /// The row holding the prompt and the box's bottom edge.
    fn composer_rows(app: &mut State, width: u16) -> (String, String) {
        let (rows, _) = draw(app, width, 24);
        let bottom = rows.iter().rposition(|r| r.starts_with('╰')).unwrap();
        let prompt = row_index(&rows, "❯");
        (rows[prompt].clone(), rows[bottom].clone())
    }

    fn ctrl_t(app: &mut State) {
        update(app, Msg::Key(KeyInput::new(Key::Char('t'), Mods::CTRL)));
    }

    #[test]
    fn each_mode_names_what_enter_does_and_its_key() {
        let mut app = State::default();
        let (prompt, bottom) = composer_rows(&mut app, 80);
        assert!(prompt.starts_with("│ ❯ Type a draft · Alt+Enter newline · Ctrl+- undo"));
        assert_eq!(bottom, format!("╰{}╯", "─".repeat(78)));
        key(&mut app, Key::Char('x'));
        assert!(composer_rows(&mut app, 80).1.ends_with("─ Enter sends ─╯"));
        // Narrow, the hint and the placeholder's later parts give way.
        assert!(!composer_rows(&mut app, 59).1.contains("Enter"));
        key(&mut app, Key::Backspace);
        assert!(
            composer_rows(&mut app, 59)
                .0
                .starts_with("│ ❯ Type a draft    ")
        );

        ctrl_t(&mut app);
        let (prompt, bottom) = composer_rows(&mut app, 80);
        assert!(prompt.starts_with("│ ❯ Enter steers the next step · Alt+↑ sends now"));
        assert!(bottom.ends_with("─ Esc interrupts ─╯"));

        ctrl_t(&mut app);
        let (prompt, bottom) = composer_rows(&mut app, 80);
        assert!(prompt.starts_with("│ ❯ Compacting… Enter queues · Esc cancels"));
        assert_eq!(bottom, format!("╰{}╯", "─".repeat(78)));

        ctrl_t(&mut app);
        update(
            &mut app,
            Msg::Key(KeyInput::new(Key::Char('g'), Mods::CTRL)),
        );
        key(&mut app, Key::Enter);
        let (prompt, bottom) = composer_rows(&mut app, 80);
        assert!(prompt.starts_with("│ ❯ Read-only · Esc returns to parent"));
        assert!(bottom.ends_with("─ draft kept · Esc returns ─╯"));
    }

    #[test]
    fn a_mode_change_never_rewraps_the_draft_or_moves_the_caret() {
        let mut app = State::default();
        app.draft.paste(&"steady words ".repeat(30));
        let interior = |rows: &[String]| -> Vec<String> {
            let top = rows.iter().position(|r| r.starts_with('╭')).unwrap();
            let bottom = rows.iter().rposition(|r| r.starts_with('╰')).unwrap();
            rows[top + 1..bottom].to_vec()
        };
        let (rows, cursor) = draw(&mut app, 80, 24);
        let expected = (interior(&rows), cursor);
        for _ in 0..3 {
            ctrl_t(&mut app);
            let (rows, cursor) = draw(&mut app, 80, 24);
            assert_eq!((interior(&rows), cursor), expected, "{:?}", app.mode());
        }
    }

    #[test]
    fn compaction_names_itself_in_its_own_hue() {
        let mut app = State::default();
        app.tones = Tones::TrueColor;
        ctrl_t(&mut app);
        ctrl_t(&mut app);
        app.now = Duration::from_secs(5);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(&mut app, f)).unwrap();
        let (rows, _) = draw(&mut app, 80, 24);
        let y = row_index(&rows, "Compacting history…");
        assert!(rows[y].starts_with("  Compacting history…  summarizing · 5s"));
        let rest = Tones::TrueColor.style(Hue::Compacting, 0.0);
        // Far from the band, the first letter rests in the compacting blue.
        app.now = Duration::from_secs(5) + activity::BEAT * 20;
        terminal.draw(|f| render(&mut app, f)).unwrap();
        let cell = &terminal.backend().buffer()[(2, y as u16)];
        assert_eq!((cell.symbol(), cell.style().fg), ("C", rest.fg));
    }

    #[test]
    fn small_terminals_keep_the_input_row_without_panicking() {
        let mut app = State::default();
        app.draft
            .paste("emoji \u{1F468}\u{200D}\u{1F469} 你好 กำลัง\nsecond line");
        key(&mut app, Key::Enter);
        for width in 0..=12 {
            for height in 0..=8 {
                let (rows, cursor) = draw(&mut app, width, height);
                if width > 0 && height > 0 {
                    assert!(cursor.x < width && cursor.y < height, "{width}x{height}");
                    assert!(rows.iter().all(|r| r.width() <= usize::from(width)));
                }
            }
        }
        let (rows, cursor) = draw(&mut app, 40, 1);
        assert_eq!(cursor.y, 0);
        assert!(rows[0].contains("second"));
    }

    #[test]
    fn the_header_holds_how_the_last_turn_ended() {
        let mut app = State::default();
        update(&mut app, Msg::Tick(Duration::from_secs(1)));
        ctrl_t(&mut app);
        update(&mut app, Msg::Tick(Duration::from_secs(4)));
        key(&mut app, Key::Esc);
        let (rows, _) = draw(&mut app, 80, 24);
        let header = row_index(&rows, "Interrupted");
        assert!(rows[header].starts_with("  ■ Interrupted  3s"));
        assert!(rows[header].ends_with("  no model  "));
        // The next turn replaces it; a completed one reads as such.
        ctrl_t(&mut app);
        update(&mut app, Msg::Tick(Duration::from_secs(9)));
        ctrl_t(&mut app);
        ctrl_t(&mut app);
        let (rows, _) = draw(&mut app, 80, 24);
        assert!(rows[row_index(&rows, "Completed")].starts_with("  ✓ Completed  5s"));
        assert!(!rows.iter().any(|r| r.contains("Interrupted")));
        // Narrow, the time and then the status give way before the outcome.
        let (rows, _) = draw(&mut app, 18, 12);
        assert_eq!(
            rows[row_index(&rows, "Completed")].trim_end(),
            "  ✓ Completed"
        );
    }

    #[test]
    fn row_keys_give_way_below_sixty_columns() {
        let mut app = State::default();
        let (rows, _) = draw(&mut app, 60, 24);
        assert!(rows[row_index(&rows, "Agents  2 samples")].ends_with("Ctrl+G  "));
        let (rows, _) = draw(&mut app, 59, 24);
        assert!(!rows[row_index(&rows, "Agents  2 samples")].contains("Ctrl+G"));
    }

    #[test]
    fn the_bar_shares_the_activity_and_the_consolidated_status() {
        let mut app = State::default();
        app.status = status::StatusInput {
            model: Some("deepseek-official/deepseek-v4-flash".into()),
            thinking: Some("high".into()),
            context: Some(status::ContextUsage {
                used: 15_200,
                window: 128_000,
            }),
            branch: Some(status::Branch {
                name: "main".into(),
                detached: false,
            }),
            cwd: "~/projects/bake".into(),
            ascii: false,
        };
        let bar = |rows: &[String]| rows[row_index(rows, "❯") - 2].trim_end().to_owned();
        let (rows, _) = draw(&mut app, 80, 24);
        assert_eq!(
            bar(&rows),
            format!(
                "{}deepseek-v4-flash high  ctx ~11% (15.2k/128k)  ~/projects/bake ⎇ main",
                " ".repeat(9)
            )
        );
        // A running turn takes the left; the status keeps its right edge and
        // gives way by rank to make room.
        app.now = Duration::from_secs(2);
        ctrl_t(&mut app);
        let (rows, _) = draw(&mut app, 80, 24);
        let row = bar(&rows);
        assert!(row.starts_with("  "), "{row}");
        assert!(
            row.ends_with("deepseek-v4-flash high  ctx ~11%  …cts/bake ⎇ main"),
            "{row}"
        );
        assert!(row.contains("…  thinking · 0s  "), "{row}");
        // The level takes its tone; the context's label is dim.
        app.tones = Tones::TrueColor;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(&mut app, f)).unwrap();
        let y = (row_index(&rows, "❯") - 2) as u16;
        let x_of = |needle: &str| row.find(needle).unwrap() as u16;
        let buffer = terminal.backend().buffer();
        assert_eq!(
            buffer[(x_of(" high") + 1, y)].style().fg,
            Some(Color::Rgb(0x0e, 0xa5, 0xe9))
        );
        assert!(
            buffer[(x_of("ctx"), y)]
                .style()
                .add_modifier
                .contains(Modifier::DIM)
        );
    }

    #[test]
    fn the_transcript_scrolls_from_the_composer_and_names_the_way_back() {
        let mut app = State::default();
        app.draft.type_text("draft");
        let (rows, _) = draw(&mut app, 80, 16);
        let hint = row_index(&rows, "PgUp scroll");
        assert!(
            rows[hint]
                .trim_end()
                .ends_with("Wheel/PgUp scroll · Ctrl+↑ prompts")
        );
        assert!(rows[hint - 1].starts_with("  rejects it before splitting.     "));
        key(&mut app, Key::PageUp);
        let (rows, _) = draw(&mut app, 80, 16);
        assert!(rows.iter().any(|r| r.contains(" lines below · Ctrl+End")));
        assert!(
            !rows
                .iter()
                .any(|r| r.contains("reject it before splitting"))
        );
        update(&mut app, Msg::Key(KeyInput::new(Key::End, Mods::CTRL)));
        let (rows, _) = draw(&mut app, 80, 16);
        assert!(rows.iter().any(|r| r.contains("PgUp scroll")));
        // Navigation never touches the draft.
        assert_eq!((app.draft.text(), app.draft.caret()), ("draft", 5));
    }

    fn mouse(kind: MouseKind, column: u16, row: u16, at_ms: u64) -> Msg {
        Msg::Mouse(Mouse {
            kind,
            column,
            row,
            alt: false,
            at: Duration::from_millis(at_ms),
        })
    }

    #[test]
    fn a_press_on_the_draft_places_the_caret_at_its_cell() {
        let mut app = State::default();
        for c in "hello world".chars() {
            key(&mut app, Key::Char(c));
        }
        let (rows, _) = draw(&mut app, 40, 12);
        let row = row_index(&rows, "❯ hello world");
        let at = |rows: &[String], needle: &str| {
            let line = &rows[row];
            let byte = line.find(needle).unwrap();
            u16::try_from(line[..byte].chars().count()).unwrap()
        };
        let world = at(&rows, "world");
        let y = u16::try_from(row).unwrap();
        update(&mut app, mouse(MouseKind::Down, world, y, 0));
        assert_eq!(&app.draft.text()[app.draft.caret()..], "world");
        // Past the text, the row's last place; on the prompt, its start.
        update(&mut app, mouse(MouseKind::Down, 38, y, 0));
        assert_eq!(app.draft.caret(), app.draft.text().len());
        update(&mut app, mouse(MouseKind::Down, at(&rows, "❯"), y, 0));
        assert_eq!(app.draft.caret(), 0);
        // With the agent list open the draft is not editable.
        update(
            &mut app,
            Msg::Key(KeyInput::new(Key::Char('g'), Mods::CTRL)),
        );
        draw(&mut app, 40, 12);
        update(&mut app, mouse(MouseKind::Down, world, y, 0));
        assert_eq!(app.draft.caret(), 0);
    }

    /// The scrollbar column of a drawn screen, top to bottom.
    fn bar(rows: &[String], track: ScrollTrack) -> String {
        rows[usize::from(track.top)..usize::from(track.top + track.rows)]
            .iter()
            .map(|r| r.chars().nth(usize::from(track.column)).unwrap())
            .collect()
    }

    #[test]
    fn the_scrollbar_shows_where_the_viewport_is() {
        let mut app = State::default();
        let (rows, _) = draw(&mut app, 80, 24);
        let track = app.track.expect("the history outgrows the viewport");
        assert_eq!(track.column, 79);
        // Following: the thumb rests at the foot of the track.
        let column = bar(&rows, track);
        assert!(column.ends_with('┃') && column.starts_with('│'), "{column}");
        // At the start, at its head; reading, the hint counts what is below.
        update(&mut app, Msg::Key(KeyInput::new(Key::Home, Mods::CTRL)));
        let (rows, _) = draw(&mut app, 80, 24);
        let column = bar(&rows, track);
        assert!(column.starts_with('┃') && column.ends_with('│'), "{column}");
        let below = app.transcript.total() - app.transcript.height;
        let pill = format!("↓ {below} lines below · Ctrl+End");
        assert!(rows.iter().any(|r| r.contains(&pill)), "{pill}");
        // The classic frame draws it in ASCII.
        let mut classic = State::new(FrameStyle::Classic, Tones::None);
        let (rows, _) = draw(&mut classic, 80, 24);
        let column = bar(&rows, classic.track.unwrap());
        assert!(column.chars().all(|c| c == '#' || c == '|'), "{column}");
    }

    #[test]
    fn the_wheel_scrolls_the_transcript_and_steps_the_list() {
        let mut app = State::default();
        draw(&mut app, 80, 24);
        update(&mut app, mouse(MouseKind::WheelUp, 10, 5, 1_000));
        assert!(!app.transcript.following());
        let one = app.transcript.offset();
        update(&mut app, mouse(MouseKind::WheelUp, 10, 5, 2_000));
        assert_eq!(
            app.transcript.offset(),
            one - 1,
            "a lone notch moves one row"
        );
        // Alt moves five times as far.
        update(
            &mut app,
            Msg::Mouse(Mouse {
                alt: true,
                ..match mouse(MouseKind::WheelUp, 10, 5, 3_000) {
                    Msg::Mouse(m) => m,
                    _ => unreachable!(),
                }
            }),
        );
        assert_eq!(app.transcript.offset(), one - 6);
        for at in 0..20 {
            update(
                &mut app,
                mouse(MouseKind::WheelDown, 10, 5, 4_000 + at * 1_000),
            );
        }
        assert!(
            app.transcript.following(),
            "down to the bottom follows again"
        );
        // In the agent list, the wheel steps the selection.
        update(
            &mut app,
            Msg::Key(KeyInput::new(Key::Char('g'), Mods::CTRL)),
        );
        update(&mut app, mouse(MouseKind::WheelDown, 10, 5, 40_000));
        assert_eq!(app.selected, "sample-reviewer");
        update(&mut app, mouse(MouseKind::WheelUp, 10, 5, 41_000));
        assert_eq!(app.selected, "sample-explorer");
    }

    #[test]
    fn pressing_and_dragging_the_scrollbar_moves_the_transcript() {
        let mut app = State::default();
        app.draft.type_text("keep");
        draw(&mut app, 80, 24);
        let track = app.track.unwrap();
        // A press at the head of the track goes to the start.
        update(&mut app, mouse(MouseKind::Down, track.column, track.top, 0));
        assert_eq!(app.transcript.offset(), 0);
        // Dragging follows the pointer, even off the bar, and the thumb is
        // accented while it is held.
        update(
            &mut app,
            mouse(MouseKind::Drag, 3, track.top + track.rows / 2, 10),
        );
        let middle = app.transcript.offset();
        assert!(middle > 0 && !app.transcript.following());
        app.tones = Tones::TrueColor;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(&mut app, f)).unwrap();
        let thumb = app.transcript.thumb(usize::from(track.rows)).unwrap();
        let y = track.top + thumb.start as u16;
        assert_eq!(
            terminal.backend().buffer()[(track.column, y)].fg,
            Color::Rgb(0x0e, 0xa5, 0xe9)
        );
        update(
            &mut app,
            mouse(MouseKind::Drag, 3, track.top + track.rows + 10, 20),
        );
        assert!(app.transcript.following(), "dragged past the foot");
        // Released, a drag does nothing; a press off the bar does nothing.
        update(&mut app, mouse(MouseKind::Up, 3, 0, 30));
        update(&mut app, mouse(MouseKind::Drag, 3, track.top, 40));
        assert!(app.transcript.following());
        update(&mut app, mouse(MouseKind::Down, 3, track.top, 50));
        assert!(app.transcript.following());
        assert_eq!(app.draft.text(), "keep");
    }

    fn screen(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_owned())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn reading_puts_a_centred_pill_just_above_the_bar() {
        let mut app = State::default();
        let (rows, _) = draw(&mut app, 80, 24);
        let bar = row_index(&rows, "no model");
        // Following: the scroll keys sit right-aligned on the row above the bar.
        assert!(
            rows[bar - 1]
                .trim_end()
                .ends_with("Wheel/PgUp scroll · Ctrl+↑ prompts")
        );
        key(&mut app, Key::PageUp);
        let (rows, _) = draw(&mut app, 80, 24);
        let pill = &rows[bar - 1];
        let below = app.transcript.total() - app.transcript.offset() - app.transcript.height;
        let text = format!(" ↓ {below} lines below · Ctrl+End ");
        let start = pill.find(&text).expect(pill);
        // Centred under the text, the scrollbar's column excluded.
        let left = pill[..start].chars().count();
        let right = 79 - left - text.chars().count();
        assert!(left.abs_diff(right) <= 1, "{left} {right}");
        // Its spot is recorded, and a press there follows output again.
        let spot = app.latest.unwrap();
        assert_eq!(
            (usize::from(spot.row), usize::from(spot.column)),
            (bar - 1, left)
        );
        update(
            &mut app,
            mouse(MouseKind::Down, spot.column + 2, spot.row, 0),
        );
        assert!(app.transcript.following());
        let (rows, _) = draw(&mut app, 80, 24);
        assert_eq!(app.latest, None);
        assert!(rows[bar - 1].contains("Wheel/PgUp scroll"));
    }

    #[test]
    fn output_arriving_while_reading_turns_the_pill_yellow() {
        let mut app = State::default();
        app.tones = Tones::TrueColor;
        draw(&mut app, 80, 24);
        key(&mut app, Key::PageUp);
        draw(&mut app, 80, 24);
        // A sample turn appends its call below what is being read.
        ctrl_t(&mut app);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(&mut app, f)).unwrap();
        let rows = screen(&terminal);
        let y = rows
            .iter()
            .position(|r| r.contains("↓ New output · "))
            .unwrap();
        let x = rows[y][..rows[y].find('↓').unwrap()].chars().count() as u16;
        let cell = &terminal.backend().buffer()[(x, y as u16)];
        assert_eq!(cell.fg, Color::Rgb(0xfd, 0xe6, 0x8a));
        assert_eq!(cell.bg, Color::Rgb(0x3a, 0x34, 0x16));
        // Following again clears it.
        update(&mut app, Msg::Key(KeyInput::new(Key::End, Mods::CTRL)));
        draw(&mut app, 80, 24);
        assert!(!app.transcript.new_below());
    }

    #[test]
    fn the_pill_gives_up_its_key_then_its_count_before_it_is_cut() {
        let pill = |below, fresh, width| {
            scroll_pill(below, fresh, Tones::None, width).map(|l| l.to_string())
        };
        assert_eq!(
            pill(12, false, 80).unwrap(),
            " ↓ 12 lines below · Ctrl+End "
        );
        assert_eq!(pill(12, false, 20).unwrap(), " ↓ 12 lines below ");
        assert_eq!(pill(12, false, 8).unwrap(), " ↓ 12 ");
        assert_eq!(
            pill(12, true, 80).unwrap(),
            " ↓ New output · 12 lines below · Ctrl+End "
        );
        assert_eq!(pill(12, true, 30).unwrap(), " ↓ New output · Ctrl+End ");
        assert_eq!(pill(12, true, 15).unwrap(), " ↓ New output ");
        assert_eq!(pill(12, false, 4), None);
    }

    #[test]
    fn home_reaches_the_drawn_row_before_the_line() {
        let mut app = State::default();
        for c in "alpha beta gamma delta".chars() {
            key(&mut app, Key::Char(c));
        }
        // At 20 columns the draft wraps at 15: "alpha beta " / "gamma delta".
        draw(&mut app, 20, 12);
        key(&mut app, Key::Home);
        assert_eq!(&app.draft.text()[app.draft.caret()..], "gamma delta");
        key(&mut app, Key::Home);
        assert_eq!(app.draft.caret(), 0);
    }

    #[test]
    fn an_armed_quit_asks_for_a_second_press_above_the_bar() {
        let mut app = State::default();
        update(&mut app, Msg::Key(KeyInput::plain(Key::Enter)));
        let ctrl_c = Msg::Key(KeyInput::new(Key::Char('c'), Mods::CTRL));
        update(&mut app, ctrl_c.clone());
        let (rows, _) = draw(&mut app, 80, 24);
        let prompt = row_index(&rows, "Press Ctrl-C again to quit");
        assert_eq!(prompt + 1, row_index(&rows, "no model"));
        assert!(!rows.iter().any(|r| r.contains("not available")));
        // Once it lapses, the notice it covered is back.
        update(&mut app, Msg::Tick(QUIT_WINDOW));
        let (rows, _) = draw(&mut app, 80, 24);
        assert!(!rows.iter().any(|r| r.contains("again to quit")));
        assert!(rows.iter().any(|r| r.contains("not available")));
    }

    #[test]
    fn a_notice_sits_between_the_pill_and_the_bar() {
        let mut app = State::default();
        app.draft.type_text("x");
        draw(&mut app, 80, 24);
        key(&mut app, Key::PageUp);
        key(&mut app, Key::Enter);
        let (rows, _) = draw(&mut app, 80, 24);
        let bar = row_index(&rows, "no model");
        assert!(rows[bar - 1].contains("Model connection is not available"));
        assert!(rows[bar - 2].contains("lines below · Ctrl+End"));
    }
}
