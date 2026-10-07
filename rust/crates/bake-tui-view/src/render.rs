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
use crate::state::{Focus, Notice, Outcome, SAMPLE_AGENTS, SampleAgent, SampleKind, State, agent};
use crate::status::{self, Tone};
use crate::transcript::{self, Look};

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
            notice: u16::from(app.notice.is_some()),
        },
    );

    let mut y = area.y;
    let mut next = |height: u16| {
        let rect = Rect::new(area.x, y, area.width, height);
        y += height;
        rect
    };
    let body = next(rows.body);
    next(rows.gap);
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
    render_body(app, body, buf);
    if let Some(kind) = app.notice {
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
        Focus::Inspect(_) => (
            vec![Span::styled(
                copy::INSPECTING,
                Style::new().add_modifier(Modifier::BOLD),
            )],
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
        Line::styled("Tab", dim()),
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

/// Rows the body needs before it gives one to the transcript's hint row.
const HINT_ROW_MIN: u16 = 4;

/// The transcript viewport, with a hint row at its foot when there is room:
/// right-aligned keys while following output, and the way back to the
/// newest line, leading the row, while reading history.
fn render_transcript(app: &mut State, area: Rect, buf: &mut Buffer) {
    let hint = area.height >= HINT_ROW_MIN;
    let rows = area.height - u16::from(hint);
    let view = &mut app.transcript;
    view.resize(
        usize::from(area.width),
        usize::from(rows),
        Look {
            tones: app.tones,
            classic: app.frame == FrameStyle::Classic,
            lit: transcript::lit(app.now),
        },
    );
    for (i, content) in view.visible().into_iter().enumerate() {
        line(
            buf,
            Rect::new(area.x, area.y + i as u16, area.width, 1),
            content,
        );
    }
    if !hint {
        return;
    }
    let row = Rect::new(area.x, area.y + rows, area.width, 1);
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
        let width = hint.width() as u16;
        if width <= area.width {
            line(
                buf,
                Rect::new(area.x + area.width - width, row.y, width, 1),
                hint,
            );
        }
    } else {
        line(buf, row, Line::styled(copy::HINT_LATEST, accent()));
    }
}

fn render_body(app: &mut State, area: Rect, buf: &mut Buffer) {
    if area.is_empty() {
        return;
    }
    let width = usize::from(area.width);
    let classic = app.frame == FrameStyle::Classic;
    let lines = match app.focus {
        Focus::Composer => return render_transcript(app, area, buf),
        Focus::AgentList => agent_list(app.selected, width, classic),
        Focus::Inspect(id) => inspection(agent(id).unwrap_or(&SAMPLE_AGENTS[0]), width),
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

/// A name on the left and an id right-aligned, when both fit.
fn titled(lead: Vec<Span<'static>>, id: &str, width: usize) -> Line<'static> {
    let mut line = Line::from(lead);
    let used = line.width();
    if used + 2 + id.width() <= width {
        line.spans
            .push(Span::raw(" ".repeat(width - used - id.width())));
        line.spans.push(Span::styled(id.to_owned(), dim()));
    }
    line
}

/// The agents as cards: a marker on the selected one, its name and id, and
/// the first line of what it does under it.
fn agent_list(selected: &str, width: usize, classic: bool) -> Vec<Line<'static>> {
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
        lines.push(titled(vec![Span::raw("  "), mark, name], agent.id, width));
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

/// One agent, read only: its name and id, the read-only line, what it does,
/// and that the draft is kept.
fn inspection(agent: &SampleAgent, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![titled(
        vec![
            Span::raw("  "),
            Span::styled(agent.name, accent().add_modifier(Modifier::BOLD)),
        ],
        agent.id,
        width,
    )];
    lines.extend(indented(
        copy::INSPECT_READ_ONLY,
        width,
        2,
        Style::new().fg(Color::Yellow),
    ));
    lines.push(Line::default());
    for detail in agent.detail {
        lines.extend(indented(detail, width, 2, Style::new()));
    }
    lines.push(Line::default());
    lines.extend(indented(copy::INSPECT_PARENT, width, 2, dim()));
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

    use crate::state::{Msg, SampleActivity, update};

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
                    .any(|r| r.trim_end().ends_with("rejects it before splitting."))
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
        assert!(rows[prompt + 2].ends_with("Tab  "));
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
        key(&mut app, Key::Tab);
        key(&mut app, Key::Enter);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(&mut app, f)).unwrap();
        assert!(!terminal.backend().cursor_visible());
        let (rows, _) = draw(&mut app, 80, 24);
        assert!(
            rows.iter()
                .any(|r| r.starts_with("  Inspecting  Tab agents · Esc draft"))
        );
        let name = row_index(&rows, "Sample explorer");
        assert!(rows[name].trim_end().ends_with("sample-explorer"));
        assert!(rows[name + 1].starts_with("  Read only · typing never reaches"));
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
        key(&mut app, Key::Tab);
        key(&mut app, Key::Down);
        let (rows, _) = draw(&mut app, 60, 24);
        let at = |needle| row_index(&rows, needle);
        assert_eq!(
            rows[at("Sample reviewer")].trim_end(),
            format!("  ▸ Sample reviewer{}sample-reviewer", " ".repeat(26))
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
        assert!(prompt.starts_with("│ ❯ Type a draft · Alt+Enter newline · Ctrl+Z undo"));
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
        key(&mut app, Key::Tab);
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
        assert!(rows[row_index(&rows, "Agents  2 samples")].ends_with("Tab  "));
        let (rows, _) = draw(&mut app, 59, 24);
        assert!(!rows[row_index(&rows, "Agents  2 samples")].contains("Tab"));
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
        assert!(rows[hint].ends_with("PgUp scroll · Ctrl+↑ prompts"));
        assert_eq!(rows[hint - 1].trim_end(), "  rejects it before splitting.");
        key(&mut app, Key::PageUp);
        let (rows, _) = draw(&mut app, 80, 16);
        assert!(rows.iter().any(|r| r.starts_with("↓ Latest · Ctrl+End")));
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
}
